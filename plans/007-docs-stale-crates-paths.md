# Plan 007: Fix stale crates/ paths in design docs

> **Executor instructions**: Follow this plan step by step. Run every verification command and confirm the expected result before moving to the next step. If anything in the "STOP conditions" section occurs, stop and report — do not improvise. When done, update the status row for this plan in `plans/README.md` — unless a reviewer dispatched you and told you they maintain the index.
>
> **Drift check (run first)**: `git diff --stat ec2cddf..HEAD -- docs/`
> If any in-scope file changed since the plan was written, compare the "Current state" excerpts against the live code before proceeding; on a mismatch, treat it as a STOP condition.

## Status

- **Priority**: P2
- **Effort**: S
- **Risk**: LOW
- **Depends on**: none
- **Category**: docs
- **Planned at**: commit `ec2cddf`, 2026-06-16

## Why this matters

The workspace was reorganized at some point: source crates moved from `crates/` to `kernel/` and `cognitive/`. Multiple design docs still reference the old `crates/` paths. Anyone trying to follow a code reference from a doc gets a broken path. For AI agents reading these docs (which the project's CLAUDE.md explicitly encourages), stale paths produce false file-not-found errors.

## Current state

Known stale references (from subagent audit):

- `docs/events.md:7` — references `crates/core/src/event.rs:11-37` (actual: `kernel/core/src/event.rs`)
- `docs/daily-life-design.md:5` — references `crates/lifecycle` (actual: `kernel/lifecycle`)
- `docs/events-milestones.md:26` — references `crates/gateway/src/runtime/http.rs` (actual: `kernel/gateway/src/runtime/http.rs`)
- `docs/llm-chat-milestones.md` — references `crates/plugin`, `crates/runtime`, `crates/tauri` (actual: `kernel/plugin`, `kernel/gateway`, `desktop`)

Additional stale paths may exist. The fix is a batch find-and-replace across `docs/`.

## Commands you will need

| Purpose | Command | Expected on success |
|---------|---------|---------------------|
| Find stale paths | `grep -rn 'crates/' docs/` | Lists all occurrences |

## Scope

**In scope** (the only files you should modify):
- Files under `docs/` that contain `crates/` path references

**Out of scope** (do NOT touch):
- Any source code files — this is docs-only.
- Files outside `docs/` — if other files have stale paths, they're separate.
- The `crates/` word when it refers to the Rust concept (e.g., "the crates in this workspace"), not a path.

## Git workflow

- Branch: `advisor/007-docs-stale-paths`
- Commit message: `docs: fix stale crates/ paths → kernel/ after workspace reorg`
- Do NOT push or open a PR unless instructed.

## Steps

### Step 1: Find all stale `crates/` path references

```bash
grep -rn 'crates/' docs/ | grep -v '.git'
```

Review the output. Most `crates/` references in docs are path references that should now be `kernel/` or `cognitive/`.

### Step 2: Determine the correct replacement for each reference

Map old → new:
| Old prefix | New prefix |
|-----------|-----------|
| `crates/core/` | `kernel/core/` |
| `crates/event-bus/` | `kernel/event-bus/` |
| `crates/gateway/` | `kernel/gateway/` |
| `crates/plugin/` | `kernel/plugin/` |
| `crates/skill/` | `kernel/skill/` |
| `crates/workflow/` | `kernel/workflow/` |
| `crates/tool/` | `kernel/tool/` |
| `crates/source/` | `kernel/source/` |
| `crates/pipeline/` | `kernel/pipeline/` |
| `crates/config/` | `kernel/config/` |
| `crates/persistence/` | `kernel/persistence/` |
| `crates/secret/` | `kernel/secret/` |
| `crates/cli/` | `kernel/cli/` |
| `crates/sdk/` | `kernel/sdk/` |
| `crates/sandbox/` | `kernel/sandbox/` |
| `crates/hook/` | `kernel/hook/` |
| `crates/soul/` | `kernel/soul/` |
| `crates/lifecycle/` | `kernel/lifecycle/` |
| `crates/idle/` | `kernel/idle/` |
| `crates/work/` | `kernel/work/` |
| `crates/study/` | `kernel/study/` |
| `crates/memory/` | `kernel/memory/` |
| `crates/notification/` | `kernel/notification/` |
| `crates/plugins/` | `kernel/plugins/` |
| `crates/tauri/` or `crates/desktop/` | `desktop/` |
| `crates/llm/` or `crates/cognitive/llm/` | `cognitive/llm/` |
| `crates/engine/` or `crates/cognitive/engine/` | `cognitive/engine/` |

### Step 3: Replace each stale reference

Use sed for batch replacement where the mapping is one-to-one. For ambiguous references, verify the correct path manually by checking if the file exists at the new path:

```bash
# Example: replace all crates/core/ → kernel/core/
find docs/ -name '*.md' -exec sed -i '' 's|crates/core/|kernel/core/|g' {} +
```

Do this for each prefix mapping. Test after each replacement.

### Step 4: Verify no stale references remain

```bash
grep -rn 'crates/' docs/ | grep -v '.git'
```

Any remaining matches should be either:
- References to the Rust concept of "crates" (not paths)
- References that genuinely still point to `crates/` (none should exist)

**Verify**: The grep output is clean (no false path references).

## Test plan

No code changes — no tests needed. Manual verification: spot-check 2-3 files to confirm paths are corrected.

## Done criteria

- [ ] `grep -rn 'crates/' docs/` returns no path references to the old layout (conceptual uses of the word "crates" are fine)
- [ ] Spot-check: `docs/events.md` references `kernel/core/src/event.rs`, not `crates/core/src/event.rs`
- [ ] No files outside `docs/` are modified

## STOP conditions

Stop and report back (do not improvise) if:

- A `crates/` reference maps to a path that doesn't exist at any expected location (the file may have been deleted, not moved).
- A sed replacement corrupts a file (e.g., replaces `crates/` inside a code block that's showing historical context). Review each file's diff before committing.

## Maintenance notes

- When adding new docs, reference paths from the workspace root (e.g., `kernel/core/src/event.rs`), not relative to any subdirectory.
- If the workspace layout is reorganized again, run `grep -rn 'kernel/' docs/` to find references to update.
