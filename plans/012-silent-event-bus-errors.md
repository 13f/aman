# Plan 012: Log silent event bus publish errors at ~30 call sites

> **Executor instructions**: Follow this plan step by step. Run every verification command and confirm the expected result before moving to the next step. If anything in the "STOP conditions" section occurs, stop and report — do not improvise. When done, update the status row for this plan in `plans/README.md` — unless a reviewer dispatched you and told you they maintain the index.
>
> **Drift check (run first)**: `git diff --stat ec2cddf..HEAD -- kernel/`
> If any in-scope file changed since the plan was written, compare the "Current state" excerpts against the live code before proceeding; on a mismatch, treat it as a STOP condition.

## Status

- **Priority**: P2
- **Effort**: L
- **Risk**: MED
- **Depends on**: none
- **Category**: bug
- **Planned at**: commit `ec2cddf`, 2026-06-16

## Why this matters

Event bus publishes across the codebase use `let _ = bus.publish(event).await;` — the `Result` from `publish()` is silently discarded. When the event bus returns an error (backpressure-full, security violation, rate limit exceeded), the event and its entire audit trail disappear with zero diagnostic signal. Affected events include agent lifecycle transitions, tool execution results, emotion evaluations, idle state changes, skill activations, and security audit events.

The `AgentHarness` already has a pattern for this: `try_publish_to_agent_bus()` at `agent_harness.rs:303-311` logs a warning and continues. This plan applies the same pattern uniformly across the codebase.

## Current state

The pattern exists in two forms:

**Form A: `bus.publish()` — global bus**
Sites like `kernel/idle/src/manager.rs:240`, `kernel/gateway/src/main.rs:128`, `kernel/dispatcher/src/lib.rs:430`:
```rust
let _ = bus.publish(event).await;
```

**Form B: `self.publish_to_agent_bus()` — per-agent bus**
Sites inside `AgentHarness` use the existing `try_publish_to_agent_bus` wrapper (already fixed). Sites outside the harness don't have this.

Key locations (approximate, from audit — verify each):
- `kernel/idle/src/manager.rs` — ~6 sites
- `kernel/gateway/src/main.rs` — ~4 sites
- `kernel/gateway/src/runtime/agent_runtime.rs` — ~10 sites
- `kernel/gateway/src/runtime/http.rs` — ~3 sites
- `kernel/gateway/src/runtime/emotion_evaluator.rs` — ~1 site
- `kernel/gateway/src/runtime/exploration.rs` — ~1 site
- `kernel/gateway/src/runtime/incubation_runner.rs` — ~2 sites
- `kernel/gateway/src/runtime/meditation.rs` — ~1 site
- `kernel/tool/src/lib.rs` — ~2 sites
- `kernel/dispatcher/src/lib.rs` — ~2 sites

## Commands you will need

| Purpose | Command | Expected on success |
|---------|---------|---------------------|
| Find all sites | `grep -rn 'let _ = .*\.publish(' kernel/` | List all discard sites |
| Build | `cargo build --workspace` | exit 0 |
| Test | `cargo test --workspace` | all tests pass |
| Lint | `cargo clippy --workspace -- -D warnings` | exit 0 |

## Scope

**In scope** (the only files you should modify):
- Files that contain `let _ = ... .publish(...)` patterns where the bus is a global/shared bus (not the per-agent bus which already uses `try_publish_to_agent_bus`).

**Out of scope** (do NOT touch):
- The `try_publish_to_agent_bus` sites in `agent_harness.rs` — they're already fixed.
- Test code — `let _ = bus.publish(...)` in tests is fine; tests control their own error conditions.
- Sites where `publish` returns `()` (not `Result`) — those don't need fixing.

## Git workflow

- Branch: `advisor/012-silent-event-bus-errors`
- Commit messages: one per crate or logical group:
  - `fix(gateway): log event bus publish failures in main.rs and agent_runtime.rs`
  - `fix(idle): log event bus publish failures in manager.rs`
  - `fix(dispatcher): log event bus publish failures`
  - etc.
- Do NOT push or open a PR unless instructed.

## Steps

### Step 1: Survey all sites

Run the grep to find every `let _ = ...publish` in non-test code:
```bash
grep -rn 'let _ = .*\.publish(' kernel/ | grep -v '/tests/' | grep -v '#[cfg(test)]'
```

Count the sites and group by crate. Confirm the approximate count (~30). Note any sites that use a different pattern (e.g., `bus.publish(...).await.ok();`).

### Step 2: Add a global `try_publish` helper

Where to add this depends on the codebase structure. Most callers have access to an `Arc<dyn EventBus>` or a concrete `InMemoryBus`. The simplest approach: add a free function in `kernel/event-bus/src/lib.rs` or `kernel/core/src/event.rs`:

```rust
/// Publish an event to the bus, logging a warning on failure.
/// Replaces `let _ = bus.publish(event).await;` with
/// `try_publish(&bus, event).await;`.
pub async fn try_publish(bus: &(dyn EventBus + Send + Sync), event: Event) {
    if let Err(e) = bus.publish(event).await {
        tracing::warn!(
            event_type = %event.event_type,
            error = %e,
            "event bus publish failed; event dropped"
        );
    }
}
```

Or, if `EventBus` is a trait, add a default method:
```rust
async fn try_publish(&self, event: Event) {
    if let Err(e) = self.publish(event).await {
        tracing::warn!(error = %e, "event bus publish failed; event dropped");
    }
}
```

Choose the approach that requires the least refactoring at call sites.

### Step 3: Replace `let _ = bus.publish(...)` at each site

For each file identified in Step 1, replace:
```rust
let _ = bus.publish(event).await;
```
with:
```rust
try_publish(&bus, event).await;
```
(or the appropriate syntax for the chosen helper)

Do this crate by crate, building after each to catch errors.

**Verify after each crate**: `cargo build -p <crate>` → exit 0

### Step 4: Handle special cases

Some call sites may have context that should be included in the warning (agent_id, session_id). If `tracing::warn!` has access to a span, the span context will be included automatically. If not, consider passing additional context to the helper.

### Step 5: Run full workspace test and lint

**Verify**: `cargo test --workspace` → all tests pass
**Verify**: `cargo clippy --workspace -- -D warnings` → exit 0
**Verify**: `grep -rn 'let _ = .*\.publish(' kernel/ | grep -v '/tests/' | grep -v '#[cfg(test)]'` → ideally zero matches (or only intentional discards with a comment explaining why)

## Test plan

No new tests required — this is a logging change. Existing tests must continue to pass.

Optionally: add a test in `event-bus` that verifies `try_publish` logs a warning on a bus that returns an error. Use `tracing-test` or capture the log output.

## Done criteria

- [ ] `cargo build --workspace` exits 0
- [ ] `cargo test --workspace` exits 0
- [ ] `cargo clippy --workspace -- -D warnings` exits 0
- [ ] No remaining `let _ = ...publish(...)` in non-test kernel code (or remaining sites have a comment explaining the intentional discard)
- [ ] The new `try_publish` helper is used consistently

## STOP conditions

Stop and report back (do not improvise) if:

- A call site uses a bus type that doesn't implement the expected trait — the helper must be compatible.
- A call site is in a crate that doesn't have `tracing` as a dependency — add it or use an alternative logging approach.
- A site is intentionally discarding the error for a good reason (e.g., publishing a non-critical debug event in a tight loop) — add a comment and skip it.
- The grep survey finds significantly more or fewer sites than ~30 — report the actual count and adjust the effort estimate.

## Maintenance notes

- New code should use `try_publish()` (or the default trait method) instead of `let _ = bus.publish(...)`.
- Consider adding a clippy lint or a workspace-level rule to forbid `let _ = ...publish` in the future.
- The `try_publish_to_agent_bus` method in `AgentHarness` serves the same purpose for per-agent buses and should be kept in sync with the global helper's behavior.
