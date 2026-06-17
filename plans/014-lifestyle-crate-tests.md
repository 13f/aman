# Plan 014: Add characterization tests for lifestyle crates

> **Executor instructions**: Follow this plan step by step. Run every verification command and confirm the expected result before moving to the next step. If anything in the "STOP conditions" section occurs, stop and report — do not improvise. When done, update the status row for this plan in `plans/README.md` — unless a reviewer dispatched you and told you they maintain the index.
>
> **Drift check (run first)**: `git diff --stat ec2cddf..HEAD -- kernel/lifecycle/ kernel/idle/ kernel/work/ kernel/study/ kernel/daily-life/ kernel/memory/ kernel/notification/ kernel/eval/ kernel/context-manager/`
> If any in-scope file changed since the plan was written, compare the "Current state" excerpts against the live code before proceeding; on a mismatch, treat it as a STOP condition.

## Status

- **Priority**: P3
- **Effort**: L
- **Risk**: HIGH
- **Status**: DONE
- **Depends on**: 008 (TraceStore), 009 (MemoryProvider)
- **Category**: tests
- **Planned at**: commit `ec2cddf`, 2026-06-16

## Why this matters

Nine "agent lifestyle" crates totaling ~16,500 lines have zero tests. These crates implement the agent personality layer — idle behavior (boredom management, incubation, arousal), work processing, daily routines, study/learning, memory, notifications, LLM evaluation, and context management. The ongoing refactoring campaign (ConfigPatch macro, i18n extraction, plugin cleanup, P1-6 test backfill) touches code adjacent to these crates. Without characterization tests, every refactor is a blind landing.

This plan is scoped as a **discovery + minimum coverage** exercise, not exhaustive testing. The goal: at least 3-5 characterization tests per crate covering the most fundamental behavior.

## Current state

Crates with zero tests (verified during audit):

| Crate | Lines | Files | Primary types |
|-------|-------|-------|--------------|
| `kernel/idle` | 3,184 | 12 | `IdleManager`, `BoredomEngine`, `IncubationManager`, `ArousalTracker`, `PersonalityProfile` |
| `kernel/eval` | 3,673 | 16 | `EvalEngine`, `EvalStrategy` (4 variants), `EvalConfig`, hook system |
| `kernel/context-manager` | 2,376 | 6 | `ContextManager`, `TokenBudget`, `Compressor`, `PriorityQueue` |
| `kernel/work` | 2,161 | 6 | `WorkSystem`, task intake, prioritization, execution |
| `kernel/study` | 1,623 | 6 | `StudySystem`, `SpacedRepetition`, knowledge acquisition |
| `kernel/daily-life` | 1,307 | 6 | `DailyLifeSystem`, routines, schedules, habits |
| `kernel/lifecycle` | 1,268 | 5 | `LifecycleStateMachine` (Phase 0→5 startup, 5→0 shutdown) |
| `kernel/memory` | 721 | 3 | `MemoryProviderRegistry`, `YantrikdbProvider` |
| `kernel/notification` | 660 | 4 | `NotificationStore`, delivery channels |

**Important**: Plans 008 (TraceStore) and 009 (MemoryProvider) should land first — the idle and eval crates depend on trace/memory backends for their test data.

## Commands you will need

| Purpose | Command | Expected on success |
|---------|---------|---------------------|
| Build | `cargo build -p <crate>` | exit 0 |
| Test | `cargo test -p <crate>` | all tests pass |
| Lint | `cargo clippy -p <crate> -- -D warnings` | exit 0 |
| Workspace test | `cargo test --workspace` | all tests pass |

## Scope

**In scope** (the only files you should modify):
- `kernel/lifecycle/src/lib.rs` — add tests
- `kernel/idle/src/lib.rs` (or `tests/`) — add tests
- `kernel/work/src/lib.rs` — add tests
- `kernel/study/src/lib.rs` — add tests
- `kernel/daily-life/src/lib.rs` — add tests
- `kernel/memory/src/lib.rs` — add tests
- `kernel/notification/src/lib.rs` — add tests
- `kernel/context-manager/src/lib.rs` — add tests
- `kernel/eval/src/lib.rs` (or `tests/`) — add tests

**Out of scope** (do NOT touch):
- Production code — characterization tests only.
- Full refactoring of these crates to make them testable — if a crate is fundamentally untestable, document it and move on.
- The `kernel/gateway` crate (covered by plan 013).

## Git workflow

- Branch: `advisor/014-lifestyle-crate-tests`
- Commit messages, one per crate:
  - `test(lifecycle): add state machine characterization tests`
  - `test(idle): add IdleManager unit tests`
  - etc.
- Do NOT push or open a PR unless instructed.

## Steps

### Step 1: Start with lifecycle (most self-contained)

`LifecycleStateMachine` is a state machine with well-defined phases and transitions. It's the easiest to test:

```rust
#[cfg(test)]
mod tests {
    #[test]
    fn test_initial_phase_is_zero() { ... }
    #[test]
    fn test_phase_progression_0_to_5() { ... }
    #[test]
    fn test_phase_shutdown_5_to_0() { ... }
    #[test]
    fn test_cannot_skip_phases() { ... }
}
```

### Step 2: Test context-manager

`TokenBudget`, `PriorityQueue`, and `Compressor` are algorithmic components — they take inputs and produce outputs. Pure function tests:

```rust
#[test]
fn test_token_budget_defaults() { ... }
#[test]
fn test_token_budget_deduct_never_goes_negative() { ... }
#[test]
fn test_priority_queue_ordering() { ... }
```

### Step 3: Test eval (strategy-level)

The eval crate has 4 strategies. Test each strategy's configuration and basic scoring:

```rust
#[test]
fn test_llm_judge_strategy_config() { ... }
#[test]
fn test_exact_match_strategy_scores_correctly() { ... }
```

### Step 4: Test work, study, daily-life (smoke tests)

These crates depend on external state and are harder to isolate. Start with:
- Test that `WorkSystem::new()` creates without panicking.
- Test that `StudySystem` defaults are valid.
- Test that `DailyLifeSystem` schedule parsing works.

If a crate can't be tested without a full runtime, document that and move to the next crate.

### Step 5: Test idle (after TraceStore lands)

The idle system depends on `TraceStore` for reflection/meditation. After plan 008 lands, add:
- `IdleManager::new()` doesn't panic.
- `BoredomEngine::evaluate()` returns valid boredom levels.
- `ArousalTracker` state transitions.

### Step 6: Test memory and notification (after MemoryProvider lands)

After plan 009 lands:
- `MemoryProviderRegistry::register()` / `get()` / `names()`.
- `NotificationStore::push()` / `list()` / `mark_read()`.

### Step 7: Run full workspace test

**Verify**: `cargo test --workspace` → all tests pass

## Test plan

Per-crate minimum targets:

| Crate | Min tests | Focus |
|-------|-----------|-------|
| lifecycle | 3 | State machine transitions |
| context-manager | 3 | TokenBudget, PriorityQueue |
| eval | 3 | Strategy config, basic scoring |
| work | 2 | Construction, task parsing |
| study | 2 | Construction, config defaults |
| daily-life | 2 | Construction, schedule parsing |
| idle | 3 | IdleManager, BoredomEngine (after 008) |
| memory | 3 | Registry CRUD (after 009) |
| notification | 2 | Store push/list |

Total: ~23 characterization tests minimum.

## Done criteria

- [ ] Each of the 9 crates has at least the minimum number of tests
- [ ] `cargo test --workspace` exits 0
- [ ] `cargo clippy --workspace -- -D warnings` exits 0
- [ ] No production code is modified (test additions only)
- [ ] Any crate that couldn't be tested has a comment in its `lib.rs` explaining why (e.g., "requires full AgentRuntime to test")

## STOP conditions

Stop and report back (do not improvise) if:

- A crate's types are all private or require unconstructable dependencies — document the barrier and move on.
- Adding tests to a crate causes compilation errors in other crates (dependency issues).
- A test reveals a real bug — document it in the test with `#[should_panic]` and a comment, don't fix it here.
- Plans 008/009 haven't landed yet — skip idle/eval/memory/notification tests and note the dependency.

## Maintenance notes

- These are minimum-viable characterization tests. They establish that the code compiles and doesn't panic on basic usage. As the crates stabilize, tests should deepen.
- If a crate is substantially refactored after this plan, update the tests to match the new behavior.
- The evaluation crate's `EvaluationCompleted` event publishing (TODO at `kernel/eval/src/hook.rs:86`) — when implemented, add integration tests for the event flow.
