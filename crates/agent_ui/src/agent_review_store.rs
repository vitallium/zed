//! Durable state for reviews of changes produced by an agent.
//!
//! The store deliberately contains only serializable review metadata. Editor
//! anchors are transient projections and must be rebuilt from the target
//! metadata when a diff is opened again.

use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, bail};
use chrono::{DateTime, Utc};
use db::kvp::KeyValueStore;
use editor::ReviewCommentTargetData;
use gpui::{App, AppContext as _, Task};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::thread_metadata_store::ThreadId;

const NAMESPACE: &str = "agent_review_store";
const SCHEMA_VERSION: u32 = 1;

/// A durable scope for the aggregate record. A worktree ID is preferred over
/// its path so a linked worktree can be relocated without losing reviews.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewScope {
    pub worktree_id: Option<String>,
    pub project_id: Option<String>,
    pub worktree_path: PathBuf,
    pub project_path: Option<PathBuf>,
}

impl ReviewScope {
    pub fn key(&self) -> String {
        if let Some(worktree_id) = &self.worktree_id {
            let project = self
                .project_id
                .as_deref()
                .map_or_else(|| "unknown".to_string(), str::to_owned);
            return format!("project:{project}:worktree:{worktree_id}");
        }

        let project = self
            .project_path
            .as_deref()
            .map(normalized_path)
            .unwrap_or_else(|| "unknown".to_string());
        format!(
            "project:{project}:path:{}",
            normalized_path(&self.worktree_path)
        )
    }

    /// Returns whether persisted state can be moved to a newly discovered
    /// location without conflating it with another worktree.
    pub fn can_relocate_to(&self, other: &Self) -> bool {
        match (&self.worktree_id, &other.worktree_id) {
            (Some(left), Some(right)) => left == right,
            (None, None) => {
                normalized_path(&self.worktree_path) == normalized_path(&other.worktree_path)
            }
            _ => false,
        }
    }
}

fn normalized_path(path: &Path) -> String {
    path.canonicalize()
        .unwrap_or_else(|_| path.to_path_buf())
        .to_string_lossy()
        .into_owned()
}

/// Stable identity for a single producing run's review.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewIdentity {
    pub review_id: Uuid,
    pub thread_id: Option<ThreadId>,
    /// Kept as a string to avoid coupling the persisted schema to ACP's
    /// protocol representation.
    pub session_id: Option<String>,
    pub scope: ReviewScope,
    pub diff_generation: String,
}

impl ReviewIdentity {
    pub fn new(scope: ReviewScope, diff_generation: impl Into<String>) -> Self {
        Self {
            review_id: Uuid::new_v4(),
            thread_id: None,
            session_id: None,
            scope,
            diff_generation: diff_generation.into(),
        }
    }

    pub fn stable_key(&self) -> String {
        format!("{}:{}", self.scope.key(), self.review_id)
    }
}

/// The editor owns the canonical serializable target DTO. The review store
/// owns the durable comment envelope and IDs around it.
pub type ReviewTarget = ReviewCommentTargetData;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DeliveryState {
    Pending,
    InFlight,
    Uncertain,
    Retryable,
    Failed,
    Acknowledged,
    Discarded,
}

impl DeliveryState {
    fn is_terminal(self) -> bool {
        matches!(self, Self::Acknowledged | Self::Discarded)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewCommentRecord {
    pub id: Uuid,
    pub text: String,
    pub target: ReviewTarget,
    pub created_at: DateTime<Utc>,
    pub state: DeliveryState,
}

impl ReviewCommentRecord {
    pub fn new(text: impl Into<String>, target: ReviewTarget) -> Self {
        Self {
            id: Uuid::new_v4(),
            text: text.into(),
            target,
            created_at: Utc::now(),
            state: DeliveryState::Pending,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewRecord {
    pub identity: ReviewIdentity,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub comments: Vec<ReviewCommentRecord>,
}

impl ReviewRecord {
    pub fn new(identity: ReviewIdentity) -> Self {
        let now = Utc::now();
        Self {
            identity,
            created_at: now,
            updated_at: now,
            comments: Vec::new(),
        }
    }

    pub fn pending_count(&self) -> usize {
        self.comments
            .iter()
            .filter(|comment| !comment.state.is_terminal())
            .count()
    }

    pub fn stale_count(&self) -> usize {
        self.comments
            .iter()
            .filter(|comment| {
                matches!(&comment.target, ReviewTarget::Hunk { stale: true, .. })
                    && !comment.state.is_terminal()
            })
            .count()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewRecordSummary {
    pub identity: ReviewIdentity,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub pending_count: usize,
    pub stale_count: usize,
    pub has_uncertain_delivery: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewStoreSnapshot {
    pub schema_version: u32,
    pub revision: u64,
    pub records: Vec<ReviewRecord>,
}

impl Default for ReviewStoreSnapshot {
    fn default() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            revision: 0,
            records: Vec::new(),
        }
    }
}

impl ReviewStoreSnapshot {
    pub fn summaries(&self) -> Vec<ReviewRecordSummary> {
        self.records
            .iter()
            .map(|record| ReviewRecordSummary {
                identity: record.identity.clone(),
                created_at: record.created_at,
                updated_at: record.updated_at,
                pending_count: record.pending_count(),
                stale_count: record.stale_count(),
                has_uncertain_delivery: record
                    .comments
                    .iter()
                    .any(|comment| matches!(comment.state, DeliveryState::Uncertain)),
            })
            .collect()
    }

    pub fn select(&self, review_id: Uuid) -> Option<&ReviewRecord> {
        self.records
            .iter()
            .find(|record| record.identity.review_id == review_id)
    }

    pub fn select_mut(&mut self, review_id: Uuid) -> Option<&mut ReviewRecord> {
        self.records
            .iter_mut()
            .find(|record| record.identity.review_id == review_id)
    }

    pub fn select_current_generation(&self, generation: &str) -> Option<&ReviewRecord> {
        self.records
            .iter()
            .filter(|record| record.identity.diff_generation == generation)
            .max_by_key(|record| record.updated_at)
    }

    pub fn recover_in_flight(&mut self) -> usize {
        let mut recovered = 0;
        for record in &mut self.records {
            let mut record_recovered = false;
            for comment in &mut record.comments {
                if comment.state == DeliveryState::InFlight {
                    comment.state = DeliveryState::Uncertain;
                    recovered += 1;
                    record_recovered = true;
                }
            }
            if record_recovered {
                record.updated_at = Utc::now();
            }
        }
        recovered
    }

    /// Remove acknowledged/discarded comments and empty records. This is
    /// deliberately explicit; pending and uncertain intent is never compacted.
    pub fn compact(&mut self) -> usize {
        let mut removed = 0;
        for record in &mut self.records {
            let before = record.comments.len();
            record
                .comments
                .retain(|comment| !comment.state.is_terminal());
            removed += before.saturating_sub(record.comments.len());
        }
        self.records.retain(|record| !record.comments.is_empty());
        if removed > 0 {
            self.revision = self.revision.saturating_add(1);
        }
        removed
    }

    /// The foreground owner calls this for every mutation. Keeping mutation
    /// behind one method makes it straightforward for the pane to be a single
    /// writer while persistence happens asynchronously.
    pub fn mutate<F>(&mut self, mutation: F) -> Result<()>
    where
        F: FnOnce(&mut Vec<ReviewRecord>) -> Result<()>,
    {
        mutation(&mut self.records)?;
        self.revision = self.revision.saturating_add(1);
        Ok(())
    }
}

pub struct AgentReviewStore {
    pub scope: ReviewScope,
    pub snapshot: ReviewStoreSnapshot,
}

impl AgentReviewStore {
    pub fn load(scope: ReviewScope, cx: &App) -> Result<Self> {
        let key = scope.key();
        let raw = KeyValueStore::global(cx)
            .scoped(NAMESPACE)
            .read(&key)
            .with_context(|| format!("reading review store for {key}"))?;
        let mut snapshot = raw
            .as_deref()
            .map(decode_snapshot)
            .transpose()?
            .unwrap_or_default();
        snapshot.recover_in_flight();
        Ok(Self { scope, snapshot })
    }

    pub fn persist(&self, cx: &App) -> Task<Result<()>> {
        let key = self.scope.key();
        let payload = match serde_json::to_string(&self.snapshot) {
            Ok(payload) => payload,
            Err(error) => return Task::ready(Err(error).context("encoding review store")),
        };
        let kvp = KeyValueStore::global(cx);
        cx.background_spawn(async move {
            kvp.scoped(NAMESPACE)
                .write(key, payload)
                .await
                .context("writing review store")
        })
    }

    pub fn discard_empty(&mut self) -> bool {
        self.snapshot.compact() > 0
    }

    /// Applies a delivery result even when the review pane that initiated the
    /// send has been closed.
    pub fn update_delivery_state(
        scope: ReviewScope,
        comment_ids: Vec<Uuid>,
        state: DeliveryState,
        cx: &App,
    ) -> Task<Result<()>> {
        let mut store = match Self::load(scope, cx) {
            Ok(store) => store,
            Err(error) => return Task::ready(Err(error)),
        };
        let comment_ids: std::collections::HashSet<_> = comment_ids.into_iter().collect();
        for record in &mut store.snapshot.records {
            let mut touched = false;
            for comment in &mut record.comments {
                if comment_ids.contains(&comment.id) {
                    comment.state = state;
                    touched = true;
                }
            }
            if touched {
                record.updated_at = Utc::now();
            }
        }
        store.snapshot.compact();
        store.persist(cx)
    }
}

fn decode_snapshot(raw: &str) -> Result<ReviewStoreSnapshot> {
    let snapshot: ReviewStoreSnapshot =
        serde_json::from_str(raw).context("decoding review store")?;
    if snapshot.schema_version != SCHEMA_VERSION {
        bail!(
            "unsupported review store schema version {}",
            snapshot.schema_version
        );
    }
    Ok(snapshot)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scope(path: &str, id: Option<&str>) -> ReviewScope {
        ReviewScope {
            worktree_id: id.map(str::to_owned),
            project_id: Some("project".into()),
            worktree_path: PathBuf::from(path),
            project_path: None,
        }
    }

    fn target(stale: bool) -> ReviewTarget {
        ReviewTarget::Hunk {
            file_path: "src/lib.rs".into(),
            original_hunk_line: Some(10),
            original_range: Some((10, 10)),
            resolved_hunk_line: Some(10),
            resolved_range: Some((10, 10)),
            content_context: Some("fn example()".into()),
            stale,
        }
    }

    #[test]
    fn snapshot_round_trip_preserves_identity_and_targets() {
        let mut snapshot = ReviewStoreSnapshot::default();
        let identity = ReviewIdentity::new(scope("/tmp/worktree", Some("wt-1")), "run-1");
        let mut record = ReviewRecord::new(identity.clone());
        record
            .comments
            .push(ReviewCommentRecord::new("fix", target(false)));
        snapshot.records.push(record);

        let encoded = serde_json::to_string(&snapshot).unwrap();
        let decoded: ReviewStoreSnapshot = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded, snapshot);
        assert_eq!(
            decoded.select(identity.review_id).unwrap().pending_count(),
            1
        );
    }

    #[test]
    fn summaries_and_selection_are_discoverable() {
        let mut snapshot = ReviewStoreSnapshot::default();
        let identity = ReviewIdentity::new(scope("/tmp/worktree", Some("wt-1")), "run-1");
        let mut record = ReviewRecord::new(identity.clone());
        record
            .comments
            .push(ReviewCommentRecord::new("stale", target(true)));
        snapshot.records.push(record);

        let summary = &snapshot.summaries()[0];
        assert_eq!(summary.pending_count, 1);
        assert_eq!(summary.stale_count, 1);
        assert_eq!(
            snapshot
                .select_current_generation("run-1")
                .unwrap()
                .identity,
            identity
        );
    }

    #[test]
    fn worktree_ids_isolate_and_survive_relocation() {
        let first = scope("/old/worktree", Some("stable-id"));
        let relocated = scope("/new/worktree", Some("stable-id"));
        let different = scope("/new/worktree", Some("other-id"));
        assert_eq!(first.key(), relocated.key());
        assert!(first.can_relocate_to(&relocated));
        assert!(!first.can_relocate_to(&different));
    }

    #[test]
    fn recovery_marks_in_flight_uncertain_and_compaction_is_safe() {
        let identity = ReviewIdentity::new(scope("/tmp/worktree", Some("wt-1")), "run-1");
        let mut record = ReviewRecord::new(identity);
        let mut in_flight = ReviewCommentRecord::new("send", target(false));
        in_flight.state = DeliveryState::InFlight;
        let mut acknowledged = ReviewCommentRecord::new("done", target(false));
        acknowledged.state = DeliveryState::Acknowledged;
        record.comments.extend([in_flight, acknowledged]);
        let mut snapshot = ReviewStoreSnapshot {
            schema_version: SCHEMA_VERSION,
            revision: 0,
            records: vec![record],
        };

        assert_eq!(snapshot.recover_in_flight(), 1);
        assert_eq!(
            snapshot.records[0].comments[0].state,
            DeliveryState::Uncertain
        );
        assert_eq!(snapshot.compact(), 1);
        assert_eq!(snapshot.records[0].comments.len(), 1);

        snapshot.records[0].comments[0].state = DeliveryState::Discarded;
        assert_eq!(snapshot.compact(), 1);
        assert!(snapshot.records.is_empty());
    }

    #[test]
    fn mutation_revision_advances_and_errors_do_not() {
        let mut snapshot = ReviewStoreSnapshot::default();
        assert!(
            snapshot
                .mutate(|records| {
                    records.push(ReviewRecord::new(ReviewIdentity::new(
                        scope("/tmp/worktree", None),
                        "run-1",
                    )));
                    Ok(())
                })
                .is_ok()
        );
        assert_eq!(snapshot.revision, 1);
        assert!(snapshot.mutate(|_| bail!("write failed")).is_err());
        assert_eq!(snapshot.revision, 1);
    }

    #[test]
    fn corrupt_or_unknown_snapshot_is_reported_without_fallback() {
        assert!(decode_snapshot("not json").is_err());
        let unknown = serde_json::json!({
            "schema_version": SCHEMA_VERSION + 1,
            "revision": 0,
            "records": []
        });
        let error = decode_snapshot(&unknown.to_string()).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("unsupported review store schema")
        );
    }
}
