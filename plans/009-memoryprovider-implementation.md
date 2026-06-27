# Plan 009: Implement MemoryProvider store and recall

> **Executor instructions**: Follow this plan step by step. Run every verification command and confirm the expected result before moving to the next step. If anything in the "STOP conditions" section occurs, stop and report — do not improvise. When done, update the status row for this plan in `plans/README.md` — unless a reviewer dispatched you and told you they maintain the index.
>
> **Drift check (run first)**: `git diff --stat ec2cddf..HEAD -- kernel/core/src/memory.rs kernel/memory/`
> If any in-scope file changed since the plan was written, compare the "Current state" excerpts against the live code before proceeding; on a mismatch, treat it as a STOP condition.

## Status

- **Priority**: P1
- **Effort**: M
- **Risk**: MED
- **Depends on**: none
- **Category**: bug
- **Planned at**: commit `ec2cddf`, 2026-06-16

## Why this matters

The `MemoryProvider` trait (`kernel/core/src/memory.rs`) defines the interface for agent memory — storing and recalling semantic memories, listing records, filtering, and deletion. The trait provides safe no-op defaults for most methods, but the two core methods — `store` and `recall` — have no defaults and both panic with `unimplemented!()`. Any code path that calls `memory.store()` or `memory.recall()` (including agent session workflows and idle reflection) will crash at runtime.

The `kernel/memory` crate exists (~721 lines) and contains a `YantrikdbProvider` that presumably implements this trait but has zero tests. This plan completes the implementation: wire up `YantrikdbProvider` to properly implement `store` and `recall`, add tests, or if YantrikDB is not ready, provide a simple SQLite-backed fallback that actually works.

## Current state

- `kernel/core/src/memory.rs:176-184` — the two `unimplemented!()` methods:
```rust
/// Store a memory entry. Returns the provider-assigned record id.
fn store(&self, agent_id: &str, content: &str, tags: Vec<String>) -> String {
    let _ = (agent_id, content, tags);
    unimplemented!("MemoryProvider::store")
}

/// Semantic recall for the given query.
async fn recall(&self, agent_id: &str, query: &str, limit: usize) -> Vec<MemoryRecord> {
    let _ = (agent_id, query, limit);
    unimplemented!("MemoryProvider::recall")
}
```

- `kernel/core/src/memory.rs:188-195` — `list` has a working default (returns empty vec):
```rust
fn list(&self, _agent_id: &str, _filter: Option<&MemoryFilter>) -> Vec<MemoryRecord> {
    vec![]
}
```

- `kernel/memory/src/lib.rs` — `MemoryProviderRegistry` (register/unregister/get/names)
- `kernel/memory/src/yantrikdb.rs` — `YantrikdbProvider` — check if this has real implementations or also stubs.

**First step before implementing**: Read `kernel/memory/src/yantrikdb.rs` to determine its current state. If it already has implementations, the fix may be to wire them into the trait and remove the `unimplemented!()` macros. If it's stubbed too, provide an SQLite fallback.

## Commands you will need

| Purpose | Command | Expected on success |
|---------|---------|---------------------|
| Build | `cargo build -p memory` | exit 0 |
| Test | `cargo test -p memory` | all tests pass |
| Lint | `cargo clippy -p memory -- -D warnings` | exit 0 |
| Build workspace | `cargo build --workspace` | exit 0 (ensure nothing breaks) |

## Scope

**In scope** (the only files you should modify):
- `kernel/memory/src/yantrikdb.rs` — implement `store` and `recall` if stubbed
- `kernel/memory/src/lib.rs` — tests
- OR: `kernel/memory/src/sqlite_memory.rs` — **new file** if YantrikDB is not ready

**Out of scope** (do NOT touch):
- `kernel/core/src/memory.rs` — the trait definition; do not change the trait interface.
- Other `MemoryProvider` implementations outside `kernel/memory/`.
- The idle system or agent harness integration — this plan only provides a working backend.

## Git workflow

- Branch: `advisor/009-memoryprovider-implementation`
- Commit message: `fix(memory): implement MemoryProvider store and recall`
- Do NOT push or open a PR unless instructed.

## Steps

### Step 1: Assess current state of YantrikdbProvider

Read `kernel/memory/src/yantrikdb.rs` thoroughly. Determine:
- Does it implement `MemoryProvider` for `YantrikdbProvider`?
- Are `store` and `recall` already implemented, or are they also `unimplemented!()`?
- What dependencies does it have? Is YantrikDB a real database or an in-memory structure?
- Can a simple test be written against it?

### Step 2: Implement store and recall

**If YantrikDB is a real backend with working storage**, implement:
```rust
fn store(&self, agent_id: &str, content: &str, tags: Vec<String>) -> String {
    let record = MemoryRecord {
        id: Uuid::new_v4().to_string(),
        agent_id: agent_id.to_string(),
        content: content.to_string(),
        tags,
        created_at_ms: /* current time */,
        updated_at_ms: /* current time */,
    };
    // Insert into YantrikDB
    self.db.insert(&record);
    record.id
}

async fn recall(&self, agent_id: &str, query: &str, limit: usize) -> Vec<MemoryRecord> {
    // Search YantrikDB for semantically similar records
    self.db.search(agent_id, query, limit)
}
```

**If YantrikDB is also stubbed or not ready**, create `kernel/memory/src/sqlite_memory.rs` with a simple SQLite-backed implementation:

```rust
pub struct SqliteMemoryProvider {
    conn: Mutex<Connection>,
}

impl SqliteMemoryProvider {
    pub fn new(db_path: &str) -> AmanResult<Self> {
        // CREATE TABLE IF NOT EXISTS memories (
        //   id TEXT PRIMARY KEY, agent_id TEXT, content TEXT,
        //   tags TEXT, created_at_ms INTEGER, updated_at_ms INTEGER)
        // CREATE INDEX IF NOT EXISTS idx_memories_agent ON memories(agent_id)
    }
}

impl MemoryProvider for SqliteMemoryProvider {
    fn store(&self, agent_id: &str, content: &str, tags: Vec<String>) -> String { ... }
    async fn recall(&self, agent_id: &str, query: &str, limit: usize) -> Vec<MemoryRecord> {
        // Simple: search by tag match and content LIKE '%query%'
        // For semantic search, defer to a future plan
    }
    fn list(&self, agent_id: &str, filter: Option<&MemoryFilter>) -> Vec<MemoryRecord> { ... }
    fn delete(&self, agent_id: &str, id: &str) -> bool { ... }
    fn clear(&self, agent_id: &str) -> usize { ... }
}
```

**Verify**: `cargo build -p memory` → exit 0

### Step 3: Write tests

Add tests to `kernel/memory/src/lib.rs` (or the new file's test module):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_store_and_list() {
        let provider = /* create provider */;
        let id = provider.store("test-agent", "hello world", vec!["greeting".into()]);
        assert!(!id.is_empty());
        let records = provider.list("test-agent", None);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].content, "hello world");
    }

    #[tokio::test]
    async fn test_recall_by_tag() {
        let provider = /* create provider */;
        provider.store("agent1", "alpha", vec!["test".into()]);
        provider.store("agent1", "beta", vec!["other".into()]);
        let results = provider.recall("agent1", "alpha", 10).await;
        assert!(results.iter().any(|r| r.content == "alpha"));
    }

    #[test]
    fn test_delete() {
        let provider = /* create provider */;
        let id = provider.store("agent1", "to delete", vec![]);
        assert!(provider.delete("agent1", &id));
        assert!(provider.list("agent1", None).is_empty());
    }
}
```

**Verify**: `cargo test -p memory` → all tests pass (new + existing)

### Step 4: Run full workspace build to ensure nothing breaks

**Verify**: `cargo build --workspace` → exit 0 (or at minimum `cargo check --workspace`)
**Verify**: `cargo clippy -p memory -- -D warnings` → exit 0

## Test plan

New tests in `kernel/memory/`:
- `test_store_and_list` — store a record, verify it appears in list
- `test_recall_by_content` — basic text search returns matching records
- `test_recall_by_tag` — tag-based filtering works
- `test_delete` — delete removes the record
- `test_clear` — clear removes all records for an agent
- `test_multi_agent_isolation` — agents don't see each other's memories

## Done criteria

- [ ] `cargo build -p memory` exits 0
- [ ] `cargo test -p memory` exits 0; at least 6 new tests pass
- [ ] `cargo clippy -p memory -- -D warnings` exits 0
- [ ] `store()` returns a non-empty record ID and actually persists data
- [ ] `recall()` returns records matching the query (even if basic text search)
- [ ] No `unimplemented!()` calls remain in the backend implementation
- [ ] No files outside `kernel/memory/` are modified (`git status`)

## STOP conditions

Stop and report back (do not improvise) if:

- The `MemoryProvider` trait at `kernel/core/src/memory.rs` has been modified since this plan was written.
- YantrikDB has no documentation and its API is unclear — switch to the SQLite fallback instead of guessing.
- `rusqlite` is not available as a dependency of `memory` (check Cargo.toml; add it if needed).
- Any existing test fails after the change.

## Maintenance notes

- The `recall` implementation uses basic text/tag search. Semantic search (embeddings, vector similarity) is a future enhancement — add a `semantic_search` feature flag or a separate `SemanticMemoryProvider` when ready.
- If YantrikDB is eventually ready, the SQLite fallback can be deprecated. The `MemoryProvider` trait abstraction makes this a clean swap.
- Store timestamps as `i64` milliseconds since epoch (matching the rest of the codebase's convention, e.g., `TraceRecord::started_at_ms`).
