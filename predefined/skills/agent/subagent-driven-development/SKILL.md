---
name: subagent-driven-development
category: agent
description: Execute implementation plans task-by-task via subagents. Each task gets a clean-context subagent, followed by spec compliance review and code quality review. Clean context per task prevents pollution across tasks.
version: 1.0.0
metadata:
  hermes:
    tags: [execution, subagent, implementation, quality, review]
    related_skills: [plan, writing-plans, todo]
---

# Subagent-Driven Development

## Overview

Execute a plan file task-by-task using isolated subagents. Each task gets a clean context — the subagent knows only the current task, not the accumulated state of previous tasks. This prevents context pollution, the leading cause of agent performance degradation in multi-task sequences.

## Execution Flow

```
For EACH task in plan:
  │
  ├─ 1. Dispatch implementer subagent
  │     Input: current task (from plan) + relevant file contents
  │     Output: code changes committed
  │     ↓
  ├─ 2. Spec Compliance Review (separate subagent)
  │     Question: "Did we build the right thing?"
  │     Check: implementation matches the task spec exactly
  │     ↓  If FAIL → fix → re-review
  ├─ 3. Code Quality Review (separate subagent)
  │     Question: "Did we build it right?"
  │     Check: correctness, safety, reuse, simplification
  │     ↓  If FAIL → fix → re-review
  └─ 4. Mark task complete
        ↓
After ALL tasks:
  └─ Final integration verification
```

## Core Principles

### 1. Per-Task Clean Context
Each subagent is spawned fresh with only the current task's plan section. It does NOT know what previous tasks did. This:
- Prevents context pollution (subagent isn't confused by accumulated state)
- Enables parallel execution of independent tasks
- Makes failures isolated (one broken task doesn't cascade)

### 2. Review Order Matters
Spec review FIRST, then quality review. Never reverse:
- Spec review: "Did we build the right thing?" (requirements match)
- Quality review: "Did we build it right?" (code quality, safety, patterns)

If you review quality first, you might polish code that doesn't even meet spec.

### 3. Reviews Are NOT Optional
Both spec review and quality review must pass before moving to the next task. No skipping "because it's simple." Every task gets both reviews.

### 4. Subagent Self-Reports Are Untrusted
When a subagent says "tests passed" or "committed," verify:
- Check `git log` to confirm the commit exists
- Run the tests yourself to confirm they pass
- Read the diff to confirm it matches the task spec

## Dispatching a Task

For each task, spawn an implementer subagent with this prompt structure:

```
Implement Task N from the plan. You have a clean context — you only need to
complete this one task.

[PASTE FULL TASK FROM PLAN, including: Objective, Files, Steps, Code, Commands]

Rules:
- Follow the steps exactly as written
- Do NOT modify files outside the listed Files section
- Commit after completing the task
- Report: what you changed, what commit you made, and any issues you hit
```

## Spec Compliance Review

After the implementer finishes, spawn a reviewer:

```
Review the following implementation against its task spec. Your ONLY question:
"Does this implementation match what the spec asked for?"

Task Spec:
[PASTE FULL TASK]

Implementation (git diff):
[PASTE DIFF OR COMMIT SHA]

Check:
- Are all listed files modified/created?
- Does the code do what the Objective says?
- Are all steps completed?
- Is anything MISSING that the spec requires?
- Is anything EXTRA that the spec didn't ask for?

Report: PASS (spec matched) or FAIL (with specific mismatches listed).
```

## Code Quality Review

After spec review passes:

```
Review this implementation for code quality.

Commit: [COMMIT SHA]
Files: [LIST]

Check:
- Correctness: any logic errors, off-by-one, missing edge cases?
- Safety: any unsafe code, unwrap() on errors, potential panics?
- Reuse: any duplication with existing code in the codebase?
- Simplification: any over-engineering or unnecessary abstraction?
- Patterns: consistent with the surrounding codebase style?

Report: PASS or FAIL (with specific issues and suggested fixes).
```

## Integration Verification

After ALL tasks complete:

```bash
# Build the full workspace
cargo build --release --workspace

# Run the full test suite
cargo test --workspace

# Lint check
cargo clippy --workspace -- -D warnings

# Manual smoke test
[Follow the plan's Post-flight checklist]
```

## Handling Failures

If a review fails:
1. Feed the review feedback back to the implementer subagent
2. Implementer fixes the issue (same clean context, task re-executed)
3. Re-run BOTH reviews (spec + quality) after the fix
4. Only proceed to next task when both reviews pass

If a task fails 3 times:
- Pause and report to the user with the specific blocker
- Do NOT continue to the next task — the dependency chain is broken
