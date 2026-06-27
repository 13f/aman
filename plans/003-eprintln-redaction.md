# Plan 003: Redact sensitive data in startup eprintln! messages

> **Executor instructions**: Follow this plan step by step. Run every verification command and confirm the expected result before moving to the next step. If anything in the "STOP conditions" section occurs, stop and report — do not improvise. When done, update the status row for this plan in `plans/README.md` — unless a reviewer dispatched you and told you they maintain the index.
>
> **Drift check (run first)**: `git diff --stat ec2cddf..HEAD -- kernel/gateway/src/main.rs`
> If this file changed since the plan was written, compare the "Current state" excerpts against the live code before proceeding; on a mismatch, treat it as a STOP condition.

## Status

- **Priority**: P1
- **Effort**: S
- **Risk**: LOW
- **Depends on**: none
- **Category**: security
- **Planned at**: commit `ec2cddf`, 2026-06-16

## Why this matters

The gateway binary uses `eprintln!` for startup errors that occur before the tracing subscriber is initialized. These error messages may contain config file paths, connection strings, or config content that includes API keys, tokens, or other credentials. Normally, all log output goes through `RedactWriter` which strips 7 patterns of sensitive data (OpenAI/Anthropic keys, AWS keys, JWTs, Bearer tokens, key=value secrets, JSON field secrets, env-var tokens). The `eprintln!` calls at `main.rs:106,121,150,155,182,187,394` bypass this redaction entirely.

The fix is minimal: call `redact_sensitive_data()` on the error string before passing it to `eprintln!`.

## Current state

- `kernel/gateway/src/main.rs:95-98` — comment acknowledges the issue:
```rust
// eprintln! is used here for startup errors that occur BEFORE the tracing
// subscriber is initialized. Once tracing is up, all logging goes through
// the RedactWriter-wrapped subscriber.
#[allow(clippy::print_stderr)]
```

- `kernel/gateway/src/main.rs:104-108` — example unredacted error:
```rust
let config = ConfigLoader::load(config_path.as_deref(), None)
    .map_err(|e| {
        eprintln!("Config load error: {e}");
        1
    })?;
```

- `kernel/gateway/src/main.rs:119-123` — HTTP server error:
```rust
.map_err(|e| {
    eprintln!("HTTP server error: {e}");
    1
})?;
```

- `kernel/gateway/src/main.rs:148-156` — runtime start errors (three eprintln! sites):
```rust
Ok(Err(e)) => {
    eprintln!("Runtime start error: {e}");
    return Err(1);
}
// ...
Err(_) => {
    let phase = runtime.phase();
    eprintln!("Runtime start timed out after 30s (phase={phase:?})");
    return Err(1);
}
```

- `kernel/gateway/src/main.rs:182,187,394` — shutdown and signal errors use `eprintln!`

The `redact_sensitive_data` function is in `kernel/core/src/redactor.rs` and is publicly exported from `kernel::redactor`. It takes `&str` and returns `Cow<str>` with all sensitive patterns replaced by `[REDACTED]`.

## Commands you will need

| Purpose | Command | Expected on success |
|---------|---------|---------------------|
| Build | `cargo build -p gateway` | exit 0 |
| Test | `cargo test -p gateway` | all tests pass |
| Lint | `cargo clippy -p gateway -- -D warnings` | exit 0 (note: the `#[allow(clippy::print_stderr)]` remains) |

## Scope

**In scope** (the only files you should modify):
- `kernel/gateway/src/main.rs`

**Out of scope** (do NOT touch):
- The redactor module itself — it works correctly.
- Any `eprintln!` in other crates — this plan only addresses the gateway main.rs startup errors.
- The tracing subscriber initialization — that's a separate concern.

## Git workflow

- Branch: `advisor/003-eprintln-redaction`
- Commit message: `fix(gateway): redact sensitive data in startup eprintln! messages`
- Do NOT push or open a PR unless instructed.

## Steps

### Step 1: Add a redacting eprintln! helper

Add this function near the top of the `run()` function (after line 99, before the `parse_args` call):

```rust
/// eprintln! wrapper that redacts sensitive data before printing.
/// Used for startup errors before the tracing subscriber is active.
macro_rules! safe_eprintln {
    ($($arg:tt)*) => {
        // Import is needed inside the macro expansion scope
        let msg = format!($($arg)*);
        eprintln!("{}", kernel::redactor::redact_sensitive_data(&msg));
    };
}
```

**Verify**: `cargo build -p gateway` → exit 0

### Step 2: Replace all `eprintln!` calls with `safe_eprintln!`

Replace every `eprintln!(...)` call in the `run()` function with `safe_eprintln!(...)`. The sites are at approximately lines 106, 121, 150, 155, 182, 187, and 394. Keep the `#[allow(clippy::print_stderr)]` annotation — it still applies since the macro expands to `eprintln!`.

**Verify**: `cargo build -p gateway` → exit 0
**Verify**: `grep -n 'eprintln!' kernel/gateway/src/main.rs` → all remaining matches should be inside the `safe_eprintln!` macro definition itself (the macro expands to `eprintln!`)

### Step 3: Run tests and lint

**Verify**: `cargo test -p gateway` → all tests pass
**Verify**: `cargo clippy -p gateway -- -D warnings` → exit 0

## Test plan

The redactor module has its own test suite (`kernel/core/tests/redactor_snapshots.rs`) that verifies all 7 pattern categories. No new tests for the gateway main.rs are needed — this is a mechanical substitution.

Optionally, add a simple unit test to verify the macro compiles and runs:

```rust
#[test]
fn safe_eprintln_does_not_panic() {
    safe_eprintln!("test message with fake key: sk-proj-abc123def456");
    // Output goes to stderr; test just verifies no panic
}
```

## Done criteria

- [ ] `cargo build -p gateway` exits 0
- [ ] `cargo test -p gateway` exits 0
- [ ] `cargo clippy -p gateway -- -D warnings` exits 0
- [ ] All `eprintln!` calls in `main.rs` (outside the macro definition) are replaced with `safe_eprintln!`
- [ ] No files outside `kernel/gateway/src/main.rs` are modified (`git status`)

## STOP conditions

Stop and report back (do not improvise) if:

- The code at the locations in "Current state" doesn't match the excerpts (the codebase has drifted).
- `kernel::redactor::redact_sensitive_data` is not accessible from `kernel/gateway` (check Cargo.toml for the `kernel` dependency — gateway depends on `kernel` which re-exports `redactor`).
- Any existing test fails.

## Maintenance notes

- If new `eprintln!` sites are added to `main.rs`, they must use `safe_eprintln!` instead. Add a comment at the top of `run()` reminding maintainers.
- The `#[allow(clippy::print_stderr)]` annotation should remain — the macro still expands to `eprintln!`, which clippy would flag otherwise.
- Consider a future refactor: initialize a minimal tracing subscriber (with RedactWriter) before any config loading, so all output is automatically redacted. This would eliminate the need for the macro entirely.
