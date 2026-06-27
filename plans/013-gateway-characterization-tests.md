# Plan 013: Add characterization tests for gateway core paths

> **Executor instructions**: Follow this plan step by step. Run every verification command and confirm the expected result before moving to the next step. If anything in the "STOP conditions" section occurs, stop and report — do not improvise. When done, update the status row for this plan in `plans/README.md` — unless a reviewer dispatched you and told you they maintain the index.
>
> **Drift check (run first)**: `git diff --stat ec2cddf..HEAD -- kernel/gateway/ kernel/test-utils/`
> If any in-scope file changed since the plan was written, compare the "Current state" excerpts against the live code before proceeding; on a mismatch, treat it as a STOP condition.

## Status

- **Priority**: P2
- **Effort**: L
- **Risk**: MED
- **Depends on**: none
- **Category**: tests
- **Planned at**: commit `ec2cddf`, 2026-06-16

## Why this matters

The gateway crate is the highest-churn code in the repo — `agent_runtime.rs` (5,455 lines, 18 commits), `agent_harness.rs` (3,178 lines, 15 commits), `http.rs` (4,092 lines, 10 commits). Yet the existing tests only cover utility functions: `RuntimeLifecycle` state transitions, `process_remember_commands`, `sanitize_api_keys`, `parse_bearer`, and header-parsing helpers. The core execution paths — `AgentRuntime::build()`, `AgentHarness::execute_tools()`, `AgentRegistry` CRUD, and HTTP request routing — have zero coverage.

Characterization tests document current behavior as a safety net for the ongoing refactoring campaign. They don't have to be perfect — they must exist so that a refactor that breaks core behavior fails a test rather than failing silently in production.

## Current state

- **Existing tests** in gateway (good patterns to follow):
  - `kernel/gateway/src/runtime/agent_runtime.rs:4654-...` — `RuntimeLifecycle` tests (802 lines)
  - `kernel/gateway/src/runtime/agent_harness.rs:3073-...` — utility function tests (106 lines)
  - `kernel/gateway/src/runtime/http.rs:3960-...` — header-parsing tests (12 tests)

- **Test utilities available**:
  - `kernel/test-utils/src/fake_event_bus.rs` — `FakeEventBus` (in-memory, controllable)
  - `kernel/test-utils/src/mock_llm.rs` — `MockLLMProvider` (but uses a different trait — see tech debt)
  - `kernel/test-utils/src/clock.rs` — `DeterministicClock`
  - `wiremock` — HTTP mocking (workspace dependency)

- **Key untested functions**:
  - `AgentRuntime::build()` — constructor wiring ~20 subsystems
  - `AgentRuntime::handle()` — main event dispatch
  - `AgentRuntime::register_agent()` / `reload_agent()` / `remove_agent()`
  - `AgentHarness::execute_tools()` — tool dispatch loop
  - `AgentHarness::process_and_broadcast()` — turn processing
  - `AgentHarness::route_message()` — message routing
  - `AgentRegistry::register()` / `get()` / `list()` / `status transitions`

## Commands you will need

| Purpose | Command | Expected on success |
|---------|---------|---------------------|
| Build | `cargo build -p gateway` | exit 0 |
| Test | `cargo test -p gateway` | all tests pass |
| Lint | `cargo clippy -p gateway -- -D warnings` | exit 0 |
| Test (specific) | `cargo test -p gateway -- agent_registry` | new tests pass |

## Scope

**In scope** (the only files you should modify):
- `kernel/gateway/src/runtime/agent_registry.rs` — add test module
- `kernel/gateway/src/runtime/agent_runtime.rs` — add test module (or new test file)
- `kernel/gateway/tests/` — **new directory**: integration tests
- `kernel/gateway/Cargo.toml` — add `test-utils`, `wiremock` as dev-dependencies if needed

**Out of scope** (do NOT touch):
- Production code — characterization tests document existing behavior without changing it.
- `agent_harness.rs` — the harness is too tightly coupled to the runtime; test it via integration tests that exercise the whole stack.
- The desktop crate or messaging plugins.

## Git workflow

- Branch: `advisor/013-gateway-characterization-tests`
- Commit messages, one per test group:
  - `test(gateway): add characterization tests for AgentRegistry`
  - `test(gateway): add smoke test for AgentRuntime::build()`
  - `test(gateway): add integration test for HTTP health endpoint`
- Do NOT push or open a PR unless instructed.

## Steps

### Step 1: Start with AgentRegistry (lowest-hanging fruit)

`AgentRegistry` is `Arc<RwLock<HashMap>>` — straightforward to test. Add a test module to `kernel/gateway/src/runtime/agent_registry.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn test_registry() -> AgentRegistry {
        AgentRegistry::new()
    }

    #[test]
    fn test_register_and_get() {
        let reg = test_registry();
        let agent = AgentEntry { /* minimal fields */ };
        reg.register("test-agent".into(), agent.clone()).unwrap();
        let got = reg.get("test-agent").unwrap();
        assert!(got.is_some());
    }

    #[test]
    fn test_register_duplicate_fails() {
        let reg = test_registry();
        reg.register("dup".into(), AgentEntry::default()).unwrap();
        assert!(reg.register("dup".into(), AgentEntry::default()).is_err());
    }

    #[test]
    fn test_list_all() {
        let reg = test_registry();
        // ... register 2 agents, verify list.len() == 2
    }

    #[test]
    fn test_remove() {
        let reg = test_registry();
        // ... register, remove, verify get returns None
    }

    #[test]
    fn test_get_missing() {
        let reg = test_registry();
        assert!(reg.get("nonexistent").unwrap().is_none());
    }
}
```

**Verify**: `cargo test -p gateway -- agent_registry` → tests pass

### Step 2: Add a build smoke test for AgentRuntime

The `AgentRuntime::build()` requires many dependencies (config, paths, event bus). Use the minimal configuration possible. Study the builder to identify required fields and use `FakeEventBus` + temp directories:

```rust
#[cfg(test)]
mod tests {
    #[tokio::test]
    async fn test_build_minimal_runtime() {
        let tmp = tempfile::tempdir().unwrap();
        let config = AgentConfig::default(); // or minimal config
        let runtime = AgentRuntime::builder()
            .config(config)
            .runtime_dir(tmp.path().to_path_buf())
            .predefined_dir(tmp.path().to_path_buf())
            .build();
        // This may fail if required fields are missing — the test tells us
        // what the minimum viable config is.
        assert!(runtime.is_ok(), "build failed: {:?}", runtime.err());
    }
}
```

The first attempt will likely fail because `AgentConfig::default()` may not exist or may not be valid. This is expected — the goal is to discover and document the minimum viable configuration.

**Verify**: `cargo test -p gateway -- agent_runtime` → discover minimum config

### Step 3: Add HTTP integration tests

Add an integration test file at `kernel/gateway/tests/http_integration.rs` using wiremock:

```rust
#[tokio::test]
async fn test_health_endpoint() {
    // Start a gateway with minimal config on a random port
    // GET /health → 200 OK
}

#[tokio::test]
async fn test_agent_list_empty() {
    // GET /agents → 200 OK, empty list
}
```

Follow the pattern from `kernel/cli/tests/http_mock_integration.rs`.

### Step 4: Add tests for AgentHarness utility functions not yet covered

Audit `agent_harness.rs` to see which utility functions remain untested after the P1-6 campaign. Add tests for any remaining uncovered pure functions.

### Step 5: Run full test suite

**Verify**: `cargo test -p gateway` → all tests pass (existing + new)
**Verify**: `cargo clippy -p gateway -- -D warnings` → exit 0
**Verify**: `cargo test --workspace` → all tests pass (no regressions)

## Test plan

New tests, minimum coverage targets:
- **AgentRegistry**: 5 tests (register, duplicate, get, list, remove)
- **AgentRuntime::build()**: 1 smoke test (validates build doesn't panic)
- **HTTP**: 2 integration tests (health endpoint, agent list)
- **AgentHarness**: cover remaining untested pure functions (see existing test pattern at line 3073)

## Done criteria

- [ ] `cargo test -p gateway` exits 0 with at least 8 new test functions
- [ ] `cargo test --workspace` exits 0 (no regressions)
- [ ] `cargo clippy -p gateway -- -D warnings` exits 0
- [ ] New tests follow the existing patterns in their respective files
- [ ] No production code is modified (only test additions)

## STOP conditions

Stop and report back (do not improvise) if:

- `AgentRuntime::builder()` requires real external services (database, keychain) that can't be faked — report which dependencies are hard requirements.
- `AgentRegistry` tests reveal an existing bug — document it, don't fix it (this is a characterization test plan).
- The test infrastructure (`FakeEventBus`, temp dirs) is insufficient for a meaningful build test — report what's missing.
- Any existing test fails after adding new tests (test interference).

## Maintenance notes

- These are characterization tests: they document how the code currently works. If intentional behavior changes, update the tests.
- The build smoke test is expected to fail on the first attempt — discovering the minimum config is part of the deliverable.
- Integration tests that start a real gateway process are slow. Keep them minimal (2-3 endpoints) and use `#[ignore]` for expensive ones if needed.
