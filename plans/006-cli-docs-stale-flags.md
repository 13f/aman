# Plan 006: Remove stale --daemon / --log-level flags from CLI.md

> **Executor instructions**: Follow this plan step by step. Run every verification command and confirm the expected result before moving to the next step. If anything in the "STOP conditions" section occurs, stop and report — do not improvise. When done, update the status row for this plan in `plans/README.md` — unless a reviewer dispatched you and told you they maintain the index.
>
> **Drift check (run first)**: `git diff --stat ec2cddf..HEAD -- kernel/cli/CLI.md kernel/gateway/src/main.rs`
> If any in-scope file changed since the plan was written, compare the "Current state" excerpts against the live code before proceeding; on a mismatch, treat it as a STOP condition.

## Status

- **Priority**: P2
- **Effort**: S
- **Risk**: LOW
- **Depends on**: none
- **Category**: docs
- **Planned at**: commit `ec2cddf`, 2026-06-16

## Why this matters

`CLI.md` documents `aman run` with two flags that don't exist: `--daemon` and `--log-level <level>`. The gateway's actual argument parser (`kernel/gateway/src/main.rs:parse_args()`) only recognizes `--config`, `--bind`, `--token`, `--soul`, and `--no-tui`. Any user who tries `aman run --daemon` or `aman run --log-level debug` gets a usage error. The docs are actively misleading — they promise features the code doesn't deliver.

## Current state

- `CLI.md:47-48` (approximate) — documents nonexistent flags:
```
--daemon           Daemonize (fork to background)
--log-level <level> Set log level (trace/debug/info/warn/error)
```

- `kernel/gateway/src/main.rs:333-374` — `parse_args()` only handles:
  - `--config <path>`
  - `--bind <address>`
  - `--token <api-token>`
  - `--soul <path>`
  - `--no-tui`

**Find the exact lines**: Read `CLI.md` and search for `--daemon` and `--log-level`. The fix is to remove those two flag entries from the `aman run` options table.

## Commands you will need

| Purpose | Command | Expected on success |
|---------|---------|---------------------|
| Verify flags | `cargo run -p gateway -- --help 2>&1` or check `parse_args` | Only lists actual flags |

## Scope

**In scope** (the only files you should modify):
- `kernel/cli/CLI.md` (or wherever CLI.md lives — check with `find . -name CLI.md`)

**Out of scope** (do NOT touch):
- Implementing the `--daemon` or `--log-level` flags — this plan removes stale docs, not adds features.
- `kernel/gateway/src/main.rs` — the parser is correct; the docs are wrong.

## Git workflow

- Branch: `advisor/006-cli-docs-stale-flags`
- Commit message: `docs(cli): remove nonexistent --daemon and --log-level flags from CLI.md`
- Do NOT push or open a PR unless instructed.

## Steps

### Step 1: Locate CLI.md and find the stale flag entries

```bash
find . -name "CLI.md" -type f
```

Read the file and find the `aman run` options section. Locate the `--daemon` and `--log-level` entries.

### Step 2: Remove the two flag entries

Delete the lines documenting `--daemon` and `--log-level <level>`. Do not change anything else.

### Step 3: Verify the remaining flags match the actual parser

Cross-reference the remaining documented flags for `aman run` against the `parse_args()` function at `kernel/gateway/src/main.rs:333-374`. They should be: `--config`, `--bind`, `--token`, `--soul`, `--no-tui`.

## Test plan

No code changes — no tests needed. Manual verification: read the updated CLI.md section and confirm no stale flags remain.

## Done criteria

- [ ] `CLI.md` no longer references `--daemon` or `--log-level`
- [ ] All remaining documented flags for `aman run` match `parse_args()`
- [ ] No other files are modified

## STOP conditions

Stop and report back (do not improvise) if:

- `CLI.md` doesn't exist at the expected path.
- The `--daemon`/`--log-level` entries don't exist (someone already fixed them).
- The documented flags have been restructured and don't match the expected format.

## Maintenance notes

- If `--daemon` or `--log-level` are implemented in the future, re-add them to the docs at that time.
- This plan does not add the missing flags to the gateway — that would be a separate feature plan.
