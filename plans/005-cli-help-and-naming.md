# Plan 005: Fix CLI --help flag and binary naming consistency

> **Executor instructions**: Follow this plan step by step. Run every verification command and confirm the expected result before moving to the next step. If anything in the "STOP conditions" section occurs, stop and report — do not improvise. When done, update the status row for this plan in `plans/README.md` — unless a reviewer dispatched you and told you they maintain the index.
>
> **Drift check (run first)**: `git diff --stat ec2cddf..HEAD -- kernel/cli/src/main.rs kernel/cli/Cargo.toml README.md`
> If any in-scope file changed since the plan was written, compare the "Current state" excerpts against the live code before proceeding; on a mismatch, treat it as a STOP condition.

## Status

- **Priority**: P1
- **Effort**: S
- **Risk**: LOW
- **Depends on**: none
- **Category**: dx
- **Planned at**: commit `ec2cddf`, 2026-06-16

## Why this matters

Two related onboarding issues:

1. **Missing `--help` / `-h` flag**: The CLI dispatch macro at `kernel/cli/src/main.rs:52-55` falls through to `print_usage()` + `exit(2)` for unknown arguments. `--help` and `-h` are treated as unknown, so `aman-cli --help` prints usage but exits with code 2 (error) instead of 0 (success). This violates standard CLI conventions and confuses users and scripts.

2. **Binary naming mismatch between docs and reality**: The README and all documentation refer to the CLI binary as `aman` (e.g., `aman config show`, `aman metrics`), but the Cargo.toml names the binary `aman-cli` (`kernel/cli/Cargo.toml:35`). Meanwhile, `kernel/gateway/Cargo.toml` names the gateway daemon binary `aman`. So `aman` is the *server*, not the *client*. A new user following the README gets confused: `aman config show` starts a gateway, doesn't run a CLI command.

The fix: add `--help`/`-h` handling to the CLI, and add a note to README clarifying the binary names.

## Current state

- `kernel/cli/src/main.rs:44-56` — dispatch macro with no `--help` case:
```rust
        $( Some($name) => {
            if let Err(code) = $fn(&$args[1..]).await {
                std::process::exit(code);
            }
        } )+
        Some("--version") | Some("-V") => {
            safe_println!("aman v{} — AmanExistence", env!("CARGO_PKG_VERSION"));
        }
        _ => {
            print_usage();
            std::process::exit(2);
        }
```

- `kernel/cli/Cargo.toml:35` — binary name:
```toml
[[bin]]
name = "aman-cli"
path = "src/main.rs"
```

- `kernel/gateway/Cargo.toml` — gateway binary is named `aman`
- `README.md:45` — docs say `cargo install --path kernel/cli` (installs `aman-cli`)

- **Conventions**: The CLI uses a `dispatch!` macro pattern. Subcommand functions return `Result<(), i32>` where the `i32` is an exit code. `print_usage()` is defined near the bottom of `main.rs`.

## Commands you will need

| Purpose | Command | Expected on success |
|---------|---------|---------------------|
| Build | `cargo build -p cli` | exit 0 |
| Test | `cargo test -p cli` | all tests pass |
| Lint | `cargo clippy -p cli -- -D warnings` | exit 0 |
| Help check | `cargo run -p cli -- --help; echo $?` | prints usage, exits 0 |
| Version check | `cargo run -p cli -- --version; echo $?` | prints version, exits 0 |

## Scope

**In scope** (the only files you should modify):
- `kernel/cli/src/main.rs` — add `--help`/`-h` handling
- `README.md` — add a note about binary naming

**Out of scope** (do NOT touch):
- Renaming the binary from `aman-cli` to `aman` — that would conflict with the gateway binary and is a larger decision.
- The `install-gateway.sh` script.
- Other documentation files (plan 006 handles CLI.md, plan 007 handles stale paths).

## Git workflow

- Branch: `advisor/005-cli-help-and-naming`
- Commit messages:
  - `fix(cli): add --help/-h flag that exits 0` 
  - `docs: clarify CLI binary naming (aman-cli vs aman)`
- Do NOT push or open a PR unless instructed.

## Steps

### Step 1: Add `--help` and `-h` to the dispatch macro

In `kernel/cli/src/main.rs`, add before the `_ =>` fallthrough case (approximately line 52):

```rust
        Some("--help") | Some("-h") => {
            print_usage();
            std::process::exit(0);
        }
```

The complete dispatch macro should look like:
```rust
        $( Some($name) => { ... } )+
        Some("--version") | Some("-V") => { ... }
        Some("--help") | Some("-h") => {
            print_usage();
            std::process::exit(0);
        }
        _ => {
            print_usage();
            std::process::exit(2);
        }
```

**Verify**: `cargo build -p cli` → exit 0
**Verify**: `cargo run -p cli -- --help; echo $?` → prints usage, exits 0
**Verify**: `cargo run -p cli -- -h; echo $?` → prints usage, exits 0

### Step 2: Add a binary naming note to README.md

In `README.md`, after the "Option B: CLI Only" section header (around line 42), add a note:

```markdown
### Option B: CLI Only (no GUI)

> **Note**: The CLI binary is named `aman-cli` (to avoid conflicting with the
> gateway daemon binary `aman`). All examples in documentation use `aman` as a
> shorthand — substitute `aman-cli` if you installed via `cargo install --path
> kernel/cli`.

```bash
# Install the CLI
cargo install --path kernel/cli
```

**Verify**: Read the rendered section. The existing content below the note should be unchanged.

### Step 3: Run tests and lint

**Verify**: `cargo test -p cli` → all tests pass
**Verify**: `cargo clippy -p cli -- -D warnings` → exit 0

## Test plan

- The CLI integration tests at `kernel/cli/tests/` verify subcommand behavior. No new tests required — the `--help` flag is a standard convention.
- Verify manually: `cargo run -p cli -- --help` exits 0 and prints usage; `cargo run -p cli -- --version` exits 0 and prints version; `cargo run -p cli -- --unknown-flag` exits non-zero.

## Done criteria

- [ ] `cargo build -p cli` exits 0
- [ ] `cargo run -p cli -- --help` exits 0 and prints usage
- [ ] `cargo run -p cli -- -h` exits 0 and prints usage
- [ ] `cargo run -p cli -- --version` still exits 0
- [ ] `cargo run -p cli -- --bogus` exits non-zero (unknown args still error)
- [ ] `cargo test -p cli` exits 0
- [ ] `cargo clippy -p cli -- -D warnings` exits 0
- [ ] README.md has the binary naming note
- [ ] No files outside `kernel/cli/src/main.rs` and `README.md` are modified (`git status`)

## STOP conditions

Stop and report back (do not improvise) if:

- The dispatch macro code doesn't match the excerpt in "Current state" (the codebase has drifted).
- The README "Option B" section has been restructured or doesn't match the expected layout.
- Any integration test fails after the change.

## Maintenance notes

- If the CLI is ever renamed from `aman-cli` to `aman`, the binary naming note in README should be removed and `--version` output updated.
- New subcommands added to the dispatch macro should be placed before the `--help`/`--version` catch-all cases.
- Consider a more structured argument parser (clap) in the future — the current dispatch macro works but doesn't support combined flags or subcommand-specific --help.
