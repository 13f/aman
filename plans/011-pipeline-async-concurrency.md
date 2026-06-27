# Plan 011: Replace blocking std::sync::Mutex in Pipeline ConcurrencyController with tokio primitives

> **Executor instructions**: Follow this plan step by step. Run every verification command and confirm the expected result before moving to the next step. If anything in the "STOP conditions" section occurs, stop and report — do not improvise. When done, update the status row for this plan in `plans/README.md` — unless a reviewer dispatched you and told you they maintain the index.
>
> **Drift check (run first)**: `git diff --stat ec2cddf..HEAD -- kernel/pipeline/src/lib.rs`
> If this file changed since the plan was written, compare the "Current state" excerpts against the live code before proceeding; on a mismatch, treat it as a STOP condition.

## Status

- **Priority**: P1
- **Effort**: L
- **Risk**: MED
- **Depends on**: none
- **Category**: bug
- **Planned at**: commit `ec2cddf`, 2026-06-16

## Why this matters

`ConcurrencyController::enter()` holds a `std::sync::MutexGuard` across `Condvar::wait()` — a blocking call. When called from an async context (which it is — `PipelineEngine` calls it from `execute_tool_with_retry` and `compensate`, both `async fn`), this blocks the entire tokio worker thread. On a multi-threaded runtime, this degrades throughput by stealing a worker thread. On a single-threaded or heavily-loaded runtime, it can cause deadlocks.

The fix replaces `std::sync::Mutex` + `Condvar` with `tokio::sync::Semaphore` (for concurrency limits) and `tokio::sync::Notify` (for wakeup signaling). The semantic behavior is identical: `Serial` allows 1 concurrent execution, `Limited(n)` allows up to n, and `Parallel` allows unlimited. The difference is that waiting is async (`.await`) instead of blocking.

## Current state

- `kernel/pipeline/src/lib.rs:227-273` — `ConcurrencyController`:
```rust
#[derive(Default)]
pub struct ConcurrencyController {
    states: Mutex<HashMap<String, Arc<PipelineConcurrencyState>>>,
}

impl ConcurrencyController {
    pub fn enter(&self, pipeline_id: &str, model: &ConcurrencyModel) -> ConcurrencyGuard {
        let state = {
            let mut states = self.states.lock().expect("...");
            Arc::clone(states.entry(pipeline_id.to_owned())
                .or_insert_with(|| Arc::new(PipelineConcurrencyState::default())))
        };

        let mut running = state.running.lock().expect("...");
        match model {
            ConcurrencyModel::Serial => {
                while *running > 0 {
                    running = state.wakeup.wait(running).expect("..."); // BLOCKS
                }
                *running = 1;
            }
            ConcurrencyModel::Limited(limit) => {
                let limit = (*limit).max(1);
                while *running >= limit {
                    running = state.wakeup.wait(running).expect("..."); // BLOCKS
                }
                *running += 1;
            }
            ConcurrencyModel::Parallel => {
                *running += 1;
            }
        }
        drop(running);
        ConcurrencyGuard { state }
    }
}
```

- `kernel/pipeline/src/lib.rs:276-300` — `PipelineConcurrencyState` and `ConcurrencyGuard`:
```rust
#[derive(Default)]
struct PipelineConcurrencyState {
    running: Mutex<usize>,
    wakeup: Condvar,
}

pub struct ConcurrencyGuard {
    state: Arc<PipelineConcurrencyState>,
}

impl Drop for ConcurrencyGuard {
    fn drop(&mut self) {
        let mut running = self.state.running.lock().expect("...");
        *running = running.saturating_sub(1);
        if *running == 0 { self.state.wakeup.notify_all(); }
        else { self.state.wakeup.notify_one(); }
    }
}
```

- `kernel/pipeline/src/lib.rs:303-...` — `PipelineEngine` uses the controller via `execute_tool_with_retry` and `compensate`, both async.

- `kernel/pipeline/src/lib.rs:639` — `std::thread::sleep` in `wait_backoff`:
```rust
if delay > 0 {
    std::thread::sleep(std::time::Duration::from_millis(delay.min(5)));
}
```

## Commands you will need

| Purpose | Command | Expected on success |
|---------|---------|---------------------|
| Build | `cargo build -p pipeline` | exit 0 |
| Test | `cargo test -p pipeline` | all tests pass |
| Lint | `cargo clippy -p pipeline -- -D warnings` | exit 0 |
| Bench | `cargo bench -p pipeline` | benchmark runs (compare before/after) |

## Scope

**In scope** (the only files you should modify):
- `kernel/pipeline/src/lib.rs` — ConcurrencyController, PipelineConcurrencyState, ConcurrencyGuard, wait_backoff

**Out of scope** (do NOT touch):
- The `PipelineEngine` execution model — only the concurrency mechanism changes.
- Other crates that call `ConcurrencyController::enter()` — the public API (`enter()`) should remain sync but internally use async-safe primitives, OR become async. Read on.
- The `std::thread::sleep` in tests — those are in test code and fine.

## Git workflow

- Branch: `advisor/011-pipeline-async-concurrency`
- Commit messages:
  - `fix(pipeline): replace std::sync::Mutex+Condvar with tokio::sync::Semaphore`
  - `fix(pipeline): replace std::thread::sleep with tokio::time::sleep in wait_backoff`
- Do NOT push or open a PR unless instructed.

## Steps

### Step 1: Replace PipelineConcurrencyState with Semaphore

Replace the `Mutex<usize>` + `Condvar` pair with a `tokio::sync::Semaphore`:

```rust
use tokio::sync::Semaphore;

#[derive(Debug)]
struct PipelineConcurrencyState {
    semaphore: Semaphore,
    max_permits: usize,
}

impl PipelineConcurrencyState {
    fn new(max_permits: usize) -> Self {
        Self {
            semaphore: Semaphore::new(max_permits),
            max_permits,
        }
    }
}
```

### Step 2: Make `enter()` async

Change `enter()` to be `async` and use `Semaphore::acquire()`:

```rust
impl ConcurrencyController {
    pub async fn enter(&self, pipeline_id: &str, model: &ConcurrencyModel) -> ConcurrencyGuard {
        let permits_needed = match model {
            ConcurrencyModel::Serial => {
                // Serial = 1 permit total. But Semaphore is not
                // easily resized at runtime. Use a single-permit
                // semaphore for Serial.
                self.get_or_create_state(pipeline_id, 1);
                // Acquire the only permit
                let state = self.get_state(pipeline_id);
                let permit = state.semaphore.acquire().await
                    .expect("semaphore closed");
                return ConcurrencyGuard {
                    permit: Some(permit),
                    state,
                    permits_held: 1,
                };
            }
            ConcurrencyModel::Limited(limit) => {
                let limit = (*limit).max(1);
                self.get_or_create_state(pipeline_id, limit);
                let state = self.get_state(pipeline_id);
                let permit = state.semaphore.acquire().await
                    .expect("semaphore closed");
                return ConcurrencyGuard {
                    permit: Some(permit),
                    state,
                    permits_held: 1,
                };
            }
            ConcurrencyModel::Parallel => {
                // No limits — don't acquire any permit
                return ConcurrencyGuard {
                    permit: None,
                    state: Arc::new(PipelineConcurrencyState::new(usize::MAX)),
                    permits_held: 0,
                };
            }
        };
    }
}
```

**Important design decision**: `Semaphore` permits are fixed at creation time. To change the max permits for a pipeline (e.g., config reload), you'd need to recreate the semaphore. For now, create the semaphore on first use with the initial limit and document that runtime limit changes require a restart.

Alternative: Use `tokio::sync::Semaphore` with `add_permits()` / `forget_permits()` for dynamic limits. But this adds complexity. Start simple: fixed-at-creation limits.

### Step 3: Update ConcurrencyGuard to return permits on drop

```rust
pub struct ConcurrencyGuard {
    permit: Option<tokio::sync::OwnedSemaphorePermit>,
    state: Arc<PipelineConcurrencyState>,
    permits_held: usize,
}

impl Drop for ConcurrencyGuard {
    fn drop(&mut self) {
        // Permit is automatically released when OwnedSemaphorePermit drops.
        // No explicit notification needed — the Semaphore handles it.
    }
}
```

### Step 4: Update all callers to use `.await`

`enter()` is now `async`. Update all call sites:
- `execute_tool_with_retry` — already async, add `.await`
- `compensate` — already async, add `.await`
- Any test code that calls `enter()`

### Step 5: Fix `wait_backoff` to use async sleep

Change `kernel/pipeline/src/lib.rs:638-640`:
```rust
if delay > 0 {
    tokio::time::sleep(std::time::Duration::from_millis(delay.min(5))).await;
}
```

### Step 6: Run tests, benchmarks, lint

**Verify**: `cargo test -p pipeline` → all tests pass
**Verify**: `cargo bench -p pipeline` → benchmarks run without errors
**Verify**: `cargo clippy -p pipeline -- -D warnings` → exit 0
**Verify**: `cargo build --workspace` → exit 0 (ensure no other crate is broken by the API change)

## Test plan

- Existing pipeline tests must pass — they exercise the concurrency controller.
- If there are no existing concurrency controller tests, add at minimum:
  - `test_serial_allows_one_at_a_time` — spawn 3 tasks, verify only 1 runs concurrently
  - `test_limited_allows_up_to_n` — spawn 5 tasks with limit 2, verify ≤2 run concurrently
  - `test_parallel_allows_all` — spawn 10 tasks, verify all run concurrently
- The pipeline benchmarks at `kernel/pipeline/benches/latency.rs` serve as regression tests for performance.

## Done criteria

- [ ] `cargo build --workspace` exits 0 (no async/sync mismatch errors in other crates)
- [ ] `cargo test -p pipeline` exits 0; all existing tests pass
- [ ] `cargo clippy -p pipeline -- -D warnings` exits 0
- [ ] `std::sync::Mutex` is no longer used in `ConcurrencyController` or `PipelineConcurrencyState`
- [ ] `std::thread::sleep` is no longer used in `wait_backoff`
- [ ] `std::sync::Condvar` import is removed

## STOP conditions

Stop and report back (do not improvise) if:

- The code at the locations in "Current state" doesn't match the excerpts (the codebase has drifted).
- Making `enter()` async causes a cascade of "function is not async" errors in more than 5 files — reassess; there may be a sync wrapper approach.
- The `Serial` semaphore approach (single permit) causes deadlocks in tests — `Semaphore` with 1 permit should be equivalent to `Mutex`, but verify.
- Pipeline benchmarks show >10% performance regression — the semaphore approach should be at least as fast.

## Maintenance notes

- The `ConcurrencyModel::Serial` case with a 1-permit semaphore means exactly one task can hold the permit at a time. This is semantically equivalent to the old `Mutex<usize>` with `while *running > 0 { wait() }`.
- If runtime limit changes become necessary, use `Semaphore::add_permits()` and `Semaphore::forget_permits()` to adjust the max without recreating state.
- The `Parallel` variant returns a guard with no permit held — this is intentional: no concurrency limit means no permit to acquire.
- The `HashMap<String, Arc<PipelineConcurrencyState>>` outer map still uses `std::sync::Mutex` — this is fine because it's only held briefly during state lookup/creation, never across an await point.
