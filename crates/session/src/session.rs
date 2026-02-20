use db::kvp::KeyValueStore;
use gpui::{App, AppContext as _, Context, Subscription, Task, WindowId};
use std::collections::HashSet;
use util::ResultExt;

pub struct Session {
    session_id: String,
    old_session_id: Option<String>,
    old_window_ids: Option<Vec<WindowId>>,
    old_window_tab_groups: Option<Vec<Vec<WindowId>>>,
}

const SESSION_ID_KEY: &str = "session_id";
const SESSION_WINDOW_STACK_KEY: &str = "session_window_stack";
const SESSION_WINDOW_TAB_GROUPS_KEY: &str = "session_window_tab_groups";

impl Session {
    pub async fn new(session_id: String, db: KeyValueStore) -> Self {
        let old_session_id = db.read_kvp(SESSION_ID_KEY).ok().flatten();

        db.write_kvp(SESSION_ID_KEY.to_string(), session_id.clone())
            .await
            .log_err();

        let old_window_ids = db
            .read_kvp(SESSION_WINDOW_STACK_KEY)
            .ok()
            .flatten()
            .and_then(|json| serde_json::from_str::<Vec<u64>>(&json).ok())
            .map(|vec: Vec<u64>| {
                vec.into_iter()
                    .map(WindowId::from)
                    .collect::<Vec<WindowId>>()
            });

        let old_window_tab_groups = db
            .read_kvp(SESSION_WINDOW_TAB_GROUPS_KEY)
            .ok()
            .flatten()
            .and_then(|json| serde_json::from_str::<Vec<Vec<u64>>>(&json).ok())
            .map(|groups| {
                groups
                    .into_iter()
                    .map(|group| group.into_iter().map(WindowId::from).collect())
                    .collect()
            });

        Self {
            session_id,
            old_session_id,
            old_window_ids,
            old_window_tab_groups,
        }
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn test() -> Self {
        Self {
            session_id: uuid::Uuid::new_v4().to_string(),
            old_session_id: None,
            old_window_ids: None,
            old_window_tab_groups: None,
        }
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn test_with_old_session(old_session_id: String) -> Self {
        Self {
            session_id: uuid::Uuid::new_v4().to_string(),
            old_session_id: Some(old_session_id),
            old_window_ids: None,
            old_window_tab_groups: None,
        }
    }

    pub fn id(&self) -> &str {
        &self.session_id
    }
}

pub struct AppSession {
    session: Session,
    _serialization_task: Task<()>,
    _subscriptions: Vec<Subscription>,
}

impl AppSession {
    pub fn new(session: Session, cx: &Context<Self>) -> Self {
        let _subscriptions = vec![cx.on_app_quit(Self::app_will_quit)];

        let _serialization_task = if cfg!(not(any(test, feature = "test-support"))) {
            let db = KeyValueStore::global(cx);
            cx.spawn(async move |_, cx| {
                // Disabled in tests: the infinite loop bypasses "parking forbidden" checks,
                // causing tests to hang instead of panicking.
                {
                    let mut current_window_state = (Vec::new(), Vec::new());
                    loop {
                        if let Some(window_state) = cx.update(session_window_state)
                            && window_state != current_window_state
                        {
                            store_window_stack(db.clone(), &window_state.0).await;
                            store_window_tab_groups(db.clone(), &window_state.1).await;
                            current_window_state = window_state;
                        }

                        cx.background_executor()
                            .timer(std::time::Duration::from_millis(500))
                            .await;
                    }
                }
            })
        } else {
            Task::ready(())
        };

        Self {
            session,
            _subscriptions,
            _serialization_task,
        }
    }

    fn app_will_quit(&mut self, cx: &mut Context<Self>) -> Task<()> {
        if let Some((window_stack, window_tab_groups)) = session_window_state(cx) {
            let db = KeyValueStore::global(cx);
            cx.background_spawn(async move {
                store_window_stack(db.clone(), &window_stack).await;
                store_window_tab_groups(db, &window_tab_groups).await;
            })
        } else {
            Task::ready(())
        }
    }

    pub fn id(&self) -> &str {
        self.session.id()
    }

    pub fn last_session_id(&self) -> Option<&str> {
        self.session.old_session_id.as_deref()
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn replace_session_for_test(&mut self, session: Session) {
        self.session = session;
    }

    pub fn last_session_window_stack(&self) -> Option<Vec<WindowId>> {
        self.session.old_window_ids.clone()
    }

    pub fn last_session_window_tab_groups(&self) -> Option<Vec<Vec<WindowId>>> {
        self.session.old_window_tab_groups.clone()
    }
}

fn session_window_state(cx: &mut App) -> Option<(Vec<u64>, Vec<Vec<u64>>)> {
    let window_stack = cx.window_stack()?;
    let mut seen_tabs = HashSet::new();
    let mut tab_groups = Vec::new();

    for window_handle in &window_stack {
        let Some(tabs) = window_handle
            .update(cx, |_, window, _| window.tabbed_windows())
            .log_err()
            .flatten()
        else {
            continue;
        };
        let participates = window_handle
            .update(cx, |_, window, _| window.system_window_tab_participant())
            .unwrap_or(false);
        if !participates {
            continue;
        }

        let mut ids = tabs
            .into_iter()
            .map(|tab| tab.id.as_u64())
            .collect::<Vec<_>>();
        if ids.len() <= 1 {
            continue;
        }

        let current_window_id = window_handle.window_id().as_u64();
        if let Some(position) = ids.iter().position(|id| *id == current_window_id) {
            ids.rotate_left(position);
        }

        if ids.iter().all(|id| !seen_tabs.contains(id)) {
            tab_groups.push(ids.clone());
        }
        seen_tabs.extend(ids);
    }

    Some((
        window_stack
            .into_iter()
            .map(|window| window.window_id().as_u64())
            .collect(),
        tab_groups,
    ))
}

async fn store_window_stack(db: KeyValueStore, windows: &[u64]) {
    if let Ok(window_ids_json) = serde_json::to_string(windows) {
        db.write_kvp(SESSION_WINDOW_STACK_KEY.to_string(), window_ids_json)
            .await
            .log_err();
    }
}

async fn store_window_tab_groups(db: KeyValueStore, window_tab_groups: &[Vec<u64>]) {
    if let Ok(window_tab_groups_json) = serde_json::to_string(window_tab_groups) {
        db.write_kvp(
            SESSION_WINDOW_TAB_GROUPS_KEY.to_string(),
            window_tab_groups_json,
        )
        .await
        .log_err();
    }
}
