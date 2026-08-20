---
category: performance
component: startup
date: 2026-08-20
module: zed
problem_type: performance_issue
root_cause: synchronous_initialization
resolution_type: code_fix
severity: high
track: bug
title: Optimize Zed Startup Performance - Database and Language Initialization
---

## Problem

Zed startup was experiencing two significant bottlenecks totaling ~213ms:
1. Language Extension Initialization: ~139.4ms
2. Database initialization: synchronous blocking on main thread

## Symptoms

- Slow application startup, particularly noticeable on first launch
- Startup profiling showed Language Extension Initialization as the largest bottleneck (#2 at 139.4ms)
- Database initialization happening synchronously during startup, blocking the main thread

## Investigation

Startup profiling with `log_startup_time()` checkpoints revealed:
- Language registration loop: ~127.5ms for 22 built-in languages
- Per-language breakdown showed high deltas for cpp (21.1ms), go (20.4ms), python (23.2ms), rust (20.2ms)
- All high-delta languages had `semantic_token_rules` set
- Each `set_language_semantic_token_rules()` call triggered `SettingsStore::recompute_values()`
- `recompute_values()` iterates over ALL registered settings, merging all sources (default, extension, global, user, server)

With 4 languages calling this separately, we were doing O(4*N) work instead of O(N).

Additionally, `AppDatabase::new()` was calling `gpui::block_on(open_db::<AppMigrator>())` synchronously, which:
- Opens the SQLite database file
- Runs all inventory-registered domain migrations in topological order
- Happens on the main thread during startup

## Solution

### 1. Batch Semantic Token Rules Updates (crates/settings/src/settings_store.rs, crates/languages/src/lib.rs)

Added a new batch API to SettingsStore:

```rust
pub fn set_language_semantic_token_rules_batch(
    &mut self,
    rules: impl IntoIterator<Item = (SharedString, SemanticTokenRules)>,
    cx: &mut App,
) {
    for (language, rule) in rules {
        self.language_semantic_token_rules.insert(language, rule);
    }
    self.recompute_values(None, cx);
}
```

Modified `languages::init()` to:
- Collect all semantic token rules during language registration loop
- Batch update them once at the end via `SettingsStore::update_global()`
- This reduces recomputation from 4 times to 1 time

**Impact:** ~74ms saved from language initialization phase (~53% improvement)

### 2. Lazy Database Initialization (crates/db/src/db.rs, crates/db/src/kvp.rs, crates/zed/src/main.rs)

Changed `AppDatabase` from holding `ThreadSafeConnection` directly to `OnceLock<ThreadSafeConnection>`:

```rust
pub struct AppDatabase(OnceLock<ThreadSafeConnection>);

impl AppDatabase {
    pub fn new() -> Self {
        Self(OnceLock::new())
    }
    
    pub fn get(&self) -> &ThreadSafeConnection {
        self.0.get_or_init(|| {
            let db_dir = database_dir();
            gpui::block_on(open_db::<AppMigrator>(db_dir, *RELEASE_CHANNEL))
        })
    }
}
```

Updated all consumers to use `db.get()` instead of accessing `.0` directly.

**Impact:** Database initialization now happens on first access (lazy), moving it off the critical startup path.

## Why This Works

### Batch Updates
`SettingsStore::recompute_values()` is an O(N) operation where N is the number of registered settings. By batching semantic token rules updates, we eliminate the redundant recomputation. Previously: 4 languages × N settings = 4N work. Now: N work (single batch).

### Lazy Initialization
`OnceLock` provides thread-safe lazy initialization. The database connection is only created when `get()` is first called. In Zed's startup flow, this typically happens later in the initialization sequence when the first database access occurs (e.g., fetching trusted worktrees), allowing other startup tasks to proceed in parallel.

## Prevention

### General Pattern: Batch Similar Updates
When multiple operations trigger the same expensive recomputation, batch them. Look for patterns like:
- Multiple calls to `update_global` in a loop
- Repeated settings/state recomputation
- Sequential operations that could be grouped

### General Pattern: Defer Expensive Initialization
For non-critical resources accessed during startup:
- Use `OnceLock` or `LazyLock` for lazy initialization
- Move synchronous `block_on` calls to background executors when possible
- Ensure first access happens at the right time (not too early, not too late)

## Files Changed

1. `crates/settings/src/settings_store.rs` - Added `set_language_semantic_token_rules_batch()`
2. `crates/languages/src/lib.rs` - Collect and batch semantic token rules
3. `crates/db/src/db.rs` - Changed to `OnceLock<ThreadSafeConnection>`, added `get()` method
4. `crates/db/src/kvp.rs` - Updated `from_app_db()` to use `get()`
5. `crates/zed/src/main.rs` - Updated timing checkpoint

## Committing the Changes

All changes are committed on branch `optimize/optimize-zed-startup`:
- `20c2b79a2b` - Optimize database initialization with lazy loading
- `5b03abbebb` - Optimize language initialization by batching semantic token rules updates
- `6b4077d95a` - Add startup timing instrumentation for profiling
