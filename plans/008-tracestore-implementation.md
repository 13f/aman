# Plan 008: Implement TraceStore for SQLite-backed trace persistence

> **Executor instructions**: Follow this plan step by step. Run every verification command and confirm the expected result before moving to the next step. If anything in the "STOP conditions" section occurs, stop and report — do not improvise. When done, update the status row for this plan in `plans/README.md` — unless a reviewer dispatched you and told you they maintain the index.
>
> **Drift check (run first)**: `git diff --stat ec2cddf..HEAD -- kernel/core/src/trace.rs kernel/persistence/`
> If any in-scope file changed since the plan was written, compare the "Current state" excerpts against the live code before proceeding; on a mismatch, treat it as a STOP condition.

## Status

- **Priority**: P1
- **Effort**: M
- **Risk**: MED
- **Depends on**: none
- **Category**: bug
- **Planned at**: commit `ec2cddf`, 2026-06-16

## Why this matters

The `TraceStore` trait (`kernel/core/src/trace.rs`) defines 16 methods for persisting agent execution traces — the foundation for idle-system reflection, meditation, and error analysis. Every single method body is `unimplemented!("TraceStore::method_name")`. Any code path that calls these methods (e.g., the reflection runner at `kernel/gateway/src/runtime/reflection.rs`, the idle system's trace analysis, or the error-chain detection) will **panic at runtime**. This means the idle system's entire reflection/meditation pipeline is non-functional.

An SQLite-backed implementation using `rusqlite` (already a workspace dependency used by `persistence`, `gateway`, `desktop`) provides a working trace store with minimal new dependencies.

## Current state

- `kernel/core/src/trace.rs:145-292` — `TraceStore` trait with 16 `unimplemented!()` methods:
```rust
async fn save_trace(&self, trace: &TraceRecord) -> AmanResult<()> {
    let _ = trace;
    unimplemented!("TraceStore::save_trace")
}

async fn begin_trace(&self, agent_id: &str, session_id: Option<&str>,
    task_type: &str, description: &str, input: &str) -> AmanResult<String> {
    let _ = (agent_id, session_id, task_type, description, input);
    unimplemented!("TraceStore::begin_trace")
}
// ... 14 more identical patterns
```

- `kernel/core/src/trace.rs:1-50` — `TraceRecord` struct with fields: `trace_id`, `agent_id`, `session_id`, `task_type`, `description`, `input`, `output`, `outcome`, `entities`, `decision_points`, `tool_calls`, `errors`, `started_at_ms`, `ended_at_ms`, `duration_ms`, `parent_trace_id`.

- `kernel/persistence/` — existing crate with WAL, StateStore, DLQ. Uses `rusqlite`. Look at how `kernel/persistence/src/lib.rs` structures its SQLite usage for the pattern to follow.

- `kernel/gateway/src/runtime/agent_runtime.rs` — the `AgentRuntime::build()` method creates subsystems. The `TraceStore` would be instantiated here.

- **Conventions**: Database-backed stores in this repo (see `kernel/persistence/src/` and `kernel/gateway/src/runtime/session_store.rs`) use:
  - `rusqlite::Connection` with `PRAGMA journal_mode=WAL` and `PRAGMA busy_timeout=5000`
  - Methods take `&self` with internal `Mutex<Connection>` for thread safety
  - Error handling maps `rusqlite::Error` to `AmanError::Unrecoverable` or `AmanError::Internal`
  - Table creation via `CREATE TABLE IF NOT EXISTS` in a `new()` or `init()` method

## Commands you will need

| Purpose | Command | Expected on success |
|---------|---------|---------------------|
| Build | `cargo build -p persistence` | exit 0 |
| Test | `cargo test -p persistence` | all tests pass |
| Lint | `cargo clippy -p persistence -- -D warnings` | exit 0 |

## Scope

**In scope** (the only files you should modify):
- `kernel/persistence/src/trace_store.rs` — **new file**: `SqliteTraceStore` implementation
- `kernel/persistence/src/lib.rs` — add `mod trace_store` and re-export
- `kernel/persistence/Cargo.toml` — add dependencies if needed (likely none — `rusqlite` and `serde_json` already present)

**Out of scope** (do NOT touch):
- `kernel/core/src/trace.rs` — the trait definition is correct; don't change it.
- Wiring `TraceStore` into `AgentRuntime` — that's a separate integration step.
- The idle/reflection/meditation systems — this plan only provides the storage backend.
- Migration of existing trace data (there is none — the store was never implemented).

## Git workflow

- Branch: `advisor/008-tracestore-implementation`
- Commit message: `feat(persistence): implement SqliteTraceStore with full TraceStore trait`
- Do NOT push or open a PR unless instructed.

## Steps

### Step 1: Study the persistence patterns

Read these files for the patterns to follow:
- `kernel/persistence/src/lib.rs` — module structure, re-exports
- `kernel/persistence/Cargo.toml` — existing dependencies
- `kernel/gateway/src/runtime/session_store.rs` — an existing SQLite-backed store (good reference for Connection management)

### Step 2: Create `kernel/persistence/src/trace_store.rs`

Implement `SqliteTraceStore`:

```rust
use std::sync::Mutex;
use rusqlite::Connection;
use kernel::trace::{TraceStore, TraceRecord, TraceOutcome, DecisionPoint, TraceError, ToolCallRecord, ChainInfo, TraceStats};
use kernel::{AmanResult, AmanError};

pub struct SqliteTraceStore {
    conn: Mutex<Connection>,
}

impl SqliteTraceStore {
    pub fn new(db_path: &str) -> AmanResult<Self> {
        let conn = Connection::open(db_path).map_err(|e| AmanError::Unrecoverable {
            message: format!("failed to open trace store: {e}"),
        })?;
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA busy_timeout=5000;
             CREATE TABLE IF NOT EXISTS traces (
                 trace_id TEXT PRIMARY KEY,
                 agent_id TEXT NOT NULL,
                 session_id TEXT,
                 task_type TEXT NOT NULL,
                 description TEXT NOT NULL DEFAULT '',
                 input TEXT NOT NULL DEFAULT '',
                 output TEXT,
                 outcome TEXT,
                 entities TEXT NOT NULL DEFAULT '[]',
                 decision_points TEXT NOT NULL DEFAULT '[]',
                 tool_calls TEXT NOT NULL DEFAULT '[]',
                 errors TEXT NOT NULL DEFAULT '[]',
                 started_at_ms INTEGER NOT NULL,
                 ended_at_ms INTEGER,
                 duration_ms INTEGER,
                 parent_trace_id TEXT,
                 created_at TEXT NOT NULL DEFAULT (datetime('now'))
             );
             CREATE INDEX IF NOT EXISTS idx_traces_agent ON traces(agent_id, started_at_ms DESC);
             CREATE INDEX IF NOT EXISTS idx_traces_session ON traces(session_id);
             CREATE INDEX IF NOT EXISTS idx_traces_parent ON traces(parent_trace_id);"
        ).map_err(|e| AmanError::Unrecoverable {
            message: format!("failed to init trace store: {e}"),
        })?;
        Ok(Self { conn: Mutex::new(conn) })
    }
}
```

Implement each `TraceStore` method. Key implementation notes:
- `save_trace` — INSERT OR REPLACE, serialize complex fields to JSON strings
- `begin_trace` — INSERT with `started_at_ms`, generate UUID via `uuid::Uuid::new_v4()`, return `trace_id`
- `end_trace` — UPDATE outcome, output, ended_at_ms, duration_ms
- `load_recent` — SELECT ... ORDER BY started_at_ms DESC LIMIT ?
- `load_by_session` — SELECT ... WHERE session_id = ?
- `load_recent_errors` — SELECT ... WHERE errors != '[]' ORDER BY started_at_ms DESC LIMIT ?
- `append_decision_point` / `append_error` / `append_tool_call` — SELECT JSON field, deserialize, append, serialize, UPDATE
- `find_incomplete` — SELECT ... WHERE outcome IS NULL
- `detect_chains` — SELECT ... WHERE parent_trace_id IN (...)
- `count` / `list_all` / `delete_trace` — straightforward CRUD
- `stats_summary` — SELECT COUNT, AVG duration, GROUP BY task_type
- `prune` — DELETE ... WHERE started_at_ms < ?

**Verify**: `cargo build -p persistence` → exit 0

### Step 3: Add module declaration and re-export

In `kernel/persistence/src/lib.rs`:
```rust
pub mod trace_store;
pub use trace_store::SqliteTraceStore;
```

**Verify**: `cargo build -p persistence` → exit 0

### Step 4: Write tests

Add `#[cfg(test)] mod tests` to `trace_store.rs`. Use a temporary file (`tempfile::TempDir` or in-memory SQLite `:memory:`) for the database. Test:
- `new()` creates tables without error
- `begin_trace()` returns a valid trace_id
- `end_trace()` updates outcome and duration
- `save_trace()` + `load_recent()` roundtrip
- `append_decision_point()` adds to the JSON array
- `find_incomplete()` returns only un-ended traces
- `prune()` deletes old traces
- `stats_summary()` returns correct counts

If `tempfile` isn't in dev-dependencies, use `Connection::open_in_memory()` instead.

**Verify**: `cargo test -p persistence` → all tests pass (existing + new)

### Step 5: Run lint

**Verify**: `cargo clippy -p persistence -- -D warnings` → exit 0

## Test plan

New tests in `kernel/persistence/src/trace_store.rs`:
- `test_create_tables` — verify tables and indexes exist
- `test_begin_and_end_trace` — full lifecycle
- `test_save_and_load` — roundtrip with all fields populated
- `test_load_recent_ordering` — newest first
- `test_append_decision_point` — append to empty and non-empty arrays
- `test_find_incomplete` — only un-ended traces
- `test_prune` — old traces removed, recent kept
- `test_stats_summary` — counts and averages correct

Pattern to follow: `kernel/persistence/src/lib.rs` test module.

## Done criteria

- [ ] `cargo build -p persistence` exits 0
- [ ] `cargo test -p persistence` exits 0; at least 8 new tests pass
- [ ] `cargo clippy -p persistence -- -D warnings` exits 0
- [ ] All 16 `TraceStore` methods have real implementations (no `unimplemented!()`)
- [ ] No files outside `kernel/persistence/` are modified (`git status`)

## STOP conditions

Stop and report back (do not improvise) if:

- The `TraceStore` trait at `kernel/core/src/trace.rs` has been modified since this plan was written.
- `rusqlite` is not available as a dependency of `persistence` (check Cargo.toml).
- Any existing test in the persistence crate fails.
- The approach of storing JSON arrays in TEXT columns is rejected (it's a pragmatic choice; proper normalization can come later).

## Maintenance notes

- The JSON-in-TEXT approach for `decision_points`, `tool_calls`, and `errors` is pragmatic but not normalized. If querying individual decision points becomes necessary, migrate to a `decision_points` table with a foreign key to `traces`.
- The UUID-based `trace_id` generation assumes the `uuid` crate is available (it's a workspace dependency). If not, use a timestamp-based ID or `blake3` hash.
- This implementation stores traces in a dedicated SQLite database. For WAL integration (making traces part of the durability story), a future plan could layer `SqliteTraceStore` on the WAL-backed store.
