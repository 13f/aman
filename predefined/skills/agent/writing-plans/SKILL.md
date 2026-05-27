---
name: writing-plans
category: agent
description: Write implementation plans with bite-sized tasks, precise file paths, and complete code snippets. Each task = 2-5 minutes of work. Plans are machine-readable protocols for subagent execution — zero context assumptions.
version: 1.0.0
metadata:
  hermes:
    tags: [planning, task-decomposition, implementation, specification]
    related_skills: [plan, subagent-driven-development, todo]
---

# Writing Implementation Plans

Loaded alongside `plan` to enforce quality standards on the plan output.

## Plan Header (Required)

Every plan starts with:

```markdown
# [Feature Name] Implementation Plan

> For aman: Use subagent-driven-development skill to implement this plan task-by-task.

## Goal
[One sentence — what we're building and why]

## Architecture
[2-3 sentences — technical approach, key decisions, data flow]

## Tech Stack
[Key dependencies, crates, external services involved]
```

## Task Granularity (Critical)

**Each task = 2-5 minutes of work.** This is the most important constraint.

```
BAD:  Task 1: Build authentication system     ← too big, 50 lines × 5 files
GOOD: Task 1: Create User model with email    ← just right, 10 lines × 1 file
GOOD: Task 2: Add password hash field         ← just right, 8 lines × 1 file
GOOD: Task 3: Implement login endpoint        ← just right, 15 lines × 1 file
```

If a task feels like it might take more than 5 minutes, split it further.

## Task Template

Every task follows this exact structure:

```markdown
### Task N: [Descriptive Name]

**Objective:** [One sentence — what this task accomplishes]

**Files:**
- `path/to/create.rs` (new)
- `path/to/modify.rs:45-78` (modify existing)

**Steps:**

#### Step 1: Write the implementation
```rust
// Full, compilable code snippet
// Not pseudocode, not "add X here"
// Complete enough to copy-paste and run
```

#### Step 2: Verify
```bash
cargo build -p crate-name
# Expected: compiles without errors
```

#### Step 3: Test
```bash
cargo test -p crate-name test_name -- --nocapture
# Expected: test passes
```

#### Step 4: Commit
```bash
git add path/to/file.rs
git commit -m "category: brief description of change"
```
```

## Zero-Context Principle

The plan must be executable by a subagent that knows NOTHING about the project:

- **File paths are precise** — not "the config file" but `src/config/settings.rs`
- **Code is complete** — not "add validation here" but the actual validation code
- **Commands include expected output** — not "run tests" but the exact command and what to look for
- **No references to "as discussed" or "see above"** — each task is self-contained

## Plan Organization

### Pre-execution Checklist
Before the task list, include a brief checklist:
```markdown
## Pre-flight
- [ ] Branch from main: `git checkout -b feature/xxx`
- [ ] Verify build: `cargo build --workspace`
- [ ] Verify tests pass: `cargo test --workspace`
```

### Task Dependencies
Mark dependencies explicitly:
```markdown
### Task Dependencies
- Task 1 → no dependencies (start here)
- Task 2 → depends on Task 1 (User model must exist)
- Task 3 → depends on Task 1, Task 2
- Task 4 → no dependencies (independent, can run in parallel with 1-3)
```

### Integration Verification
After all tasks, include:
```markdown
## Post-flight
- [ ] Full build: `cargo build --release --workspace`
- [ ] Full test suite: `cargo test --workspace`
- [ ] Lint: `cargo clippy --workspace -- -D warnings`
- [ ] Manual smoke test: [specific steps to verify the feature works]
```

## Anti-patterns

- ❌ Tasks that say "Implement the X system" — too vague
- ❌ Steps that say "Add error handling as needed" — be specific
- ❌ "Refactor as you see fit" — the subagent needs precise instructions
- ❌ Skipping the test step — every task must verify its work
- ❌ Combining unrelated changes in one task — one concern per task
