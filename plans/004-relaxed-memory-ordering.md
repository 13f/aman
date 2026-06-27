# Plan 004: Fix Relaxed memory ordering on plugin bridge shutdown flag

> **Executor instructions**: Follow this plan step by step. Run every verification command and confirm the expected result before moving to the next step. If anything in the "STOP conditions" section occurs, stop and report — do not improvise. When done, update the status row for this plan in `plans/README.md` — unless a reviewer dispatched you and told you they maintain the index.
>
> **Drift check (run first)**: `git diff --stat ec2cddf..HEAD -- kernel/plugin/src/bridge.rs`
> If this file changed since the plan was written, compare the "Current state" excerpts against the live code before proceeding; on a mismatch, treat it as a STOP condition.

## Status

- **Priority**: P1
- **Effort**: S
- **Risk**: LOW
- **Depends on**: none
- **Category**: bug
- **Planned at**: commit `ec2cddf`, 2026-06-16

## Why this matters

`SubprocessPluginBridge` uses an `AtomicBool` named `shutdown` to signal the reader thread to stop. The flag is written from the main async task (`store(true, Ordering::Relaxed)`) and read from a `std::thread::spawn` reader thread (`load(Ordering::Relaxed)`). `Ordering::Relaxed` provides no happens-before relationship between threads — it only guarantees atomicity of the individual load/store, not visibility. While x86 and ARM provide strong ordering in practice (stores eventually become visible), the Rust memory model requires `Release` on writes and `Acquire` on reads for inter-thread signaling. Using `Relaxed` is technically undefined behavior for synchronization.

The practical risk is low on current hardware, but this is a correctness issue that's trivial to fix and eliminates a potential source of nondeterministic bugs on weakly-ordered architectures.

## Current state

- `kernel/plugin/src/bridge.rs:197` — reader thread spawn:
```rust
std::thread::spawn(move || {
    let reader = BufReader::new(stdout);
    for line in reader.lines() {
        if bridge_clone.shutdown.load(Ordering::Relaxed) {  // READ
            break;
        }
```

- `kernel/plugin/src/bridge.rs:211` — reader thread writes shutdown on EOF:
```rust
        Err(_) => break, // pipe closed
    }
}
bridge_clone.shutdown.store(true, Ordering::Relaxed);  // WRITE from reader
```

- `kernel/plugin/src/bridge.rs:238` — another read site:
```rust
if self.shutdown.load(Ordering::Relaxed) {
```

- `kernel/plugin/src/bridge.rs:369` — main thread writes shutdown:
```rust
self.shutdown.store(true, Ordering::Relaxed);  // WRITE from main
```

All four sites use `Ordering::Relaxed` for inter-thread communication.

- **Conventions**: The file uses `Arc<AtomicBool>` for shared state between async tasks and OS threads. Standard Rust concurrency patterns apply.

## Commands you will need

| Purpose | Command | Expected on success |
|---------|---------|---------------------|
| Build | `cargo build -p plugin` | exit 0 |
| Test | `cargo test -p plugin` | all tests pass |
| Lint | `cargo clippy -p plugin -- -D warnings` | exit 0 |

## Scope

**In scope** (the only files you should modify):
- `kernel/plugin/src/bridge.rs`

**Out of scope** (do NOT touch):
- Other `AtomicBool` usage in the codebase — audit each separately.
- The plugin bridge threading model — this is a minimal ordering fix.
- The `Err(_) => break` I/O error handling — that's a separate concern.

## Git workflow

- Branch: `advisor/004-relaxed-memory-ordering`
- Commit message: `fix(plugin): use Acquire/Release ordering on bridge shutdown flag`
- Do NOT push or open a PR unless instructed.

## Steps

### Step 1: Replace Relaxed with Acquire on all reads

Change all three `load(Ordering::Relaxed)` calls to `load(Ordering::Acquire)`:

- Line 200: `bridge_clone.shutdown.load(Ordering::Acquire)`
- Line 238: `self.shutdown.load(Ordering::Acquire)`
- Check for any other `shutdown.load` sites in the file

### Step 2: Replace Relaxed with Release on all writes

Change both `store(true, Ordering::Relaxed)` calls to `store(true, Ordering::Release)`:

- Line 211 (reader thread self-shutdown): `bridge_clone.shutdown.store(true, Ordering::Release)`
- Line 369 (main thread shutdown): `self.shutdown.store(true, Ordering::Release)`

### Step 3: Build, test, lint

**Verify**: `cargo build -p plugin` → exit 0
**Verify**: `cargo test -p plugin` → all tests pass
**Verify**: `cargo clippy -p plugin -- -D warnings` → exit 0

## Test plan

No new tests required. The existing plugin bridge tests verify the shutdown behavior; the ordering change is semantically equivalent but with correct synchronization guarantees. If the existing tests pass, the change is correct.

## Done criteria

- [ ] `cargo build -p plugin` exits 0
- [ ] `cargo test -p plugin` exits 0
- [ ] `cargo clippy -p plugin -- -D warnings` exits 0
- [ ] `grep -n 'Ordering::Relaxed' kernel/plugin/src/bridge.rs` returns no matches (or only matches unrelated to the shutdown flag)
- [ ] No files outside `kernel/plugin/src/bridge.rs` are modified (`git status`)

## STOP conditions

Stop and report back (do not improvise) if:

- The code at the locations in "Current state" doesn't match the excerpts (the codebase has drifted).
- Any existing test fails after the change.
- There are additional `shutdown` flag accesses in `bridge.rs` that were missed (grep for `shutdown` and verify all are updated).

## Maintenance notes

- If new inter-thread shared state is added to the bridge, follow the Acquire/Release pattern: `load(Acquire)` on reads, `store(Release)` on writes.
- This fix applies the same pattern used by `std::sync::Arc` internally for reference counting (Acquire on load, Release on store).
- A broader audit of `Ordering::Relaxed` usage across the workspace would be valuable but is out of scope for this plan.
