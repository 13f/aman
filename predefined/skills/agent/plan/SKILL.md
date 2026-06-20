---
name: plan
category: agent
description: >
  Plan mode — when the task is complex, multi-stage, involves architecture
  decisions, or is destructive, enter planning mode. Explore the codebase
  (read-only), write a structured implementation plan via planner.create +
  planner.set_tasks, do NOT execute any code changes. The Orchestrator will
  automatically execute tasks after the plan is created. Use before any
  non-trivial implementation task.
version: 1.1.0
triggers:
  - "plan"
  - "make a plan"
  - "create a plan"
  - "规划"
  - "计划"
  - "方案"
  - "设计"
  - "design"
  - "roadmap"
  - "路线图"
  - "architecture"
  - "架构"
  - "approach"
  - "方案设计"
  - "how would you"
  - "怎么实现"
  - "implement"
  - "refactor"
  - "重构"
  - "migration"
  - "迁移"
  - "audit"
  - "审计"
tags:
  - planning
  - architecture
  - design
  - preparation
metadata:
  hermes:
    tags: [planning, architecture, design, preparation]
    related_skills: [planner, writing-plans, subagent-driven-development, todo]
---

# Plan Mode

## Core Rule

**You are in planning mode. You do NOT write implementation code. You do NOT modify project files (except the plan markdown). You do NOT execute destructive commands.**

Your only job: understand the task, explore the codebase (read-only), and produce a detailed, executable plan.

## Constraints

- NO code writing — no `write`, `edit`, or `patch` tools for source files
- NO file modification — except writing the plan markdown to `.aman/plans/`
- NO destructive commands — no `rm`, `git reset --hard`, `DROP TABLE`, etc.
- YES read-only exploration — read files, search code, inspect structure, run `git log`
- YES running non-destructive queries — `cargo check`, `grep`, `find`, `git diff`

## Output

### Structured plan (primary)

Use the `planner` tool to persist the plan as structured state:

1. **`planner.create`** — initialize the plan with goal, milestones, and success criteria
2. **`planner.set_tasks`** — write the decomposed task list with dependencies

After these two calls, the **Orchestrator** takes over automatically:
- Picks unblocked tasks and spawns anonymous sub-agents
- Detects stalls and pivots directions
- Escalates to human if all directions are exhausted

You do NOT need to manually call `planner.start`, `planner.complete`, or
`planner.increment_stale`. Monitor progress with `planner.status`.

See `planner` skill for the full operation reference and file format details.

### Human-readable plan (supplementary)

Optionally write a markdown plan to `.aman/plans/YYYY-MM-DD_HHMMSS-<slug>.md`
for human review. This is a supplementary artifact — the structured plan from
the `planner` tool is the authoritative state.

## When to Use Plan Mode

Trigger when ANY of these signals are present:

1. **Multi-stage / multi-subsystem** — spans multiple crates, repos, or technology stacks
2. **Architecture decisions** — trade-offs to discuss before coding (data models, API boundaries, abstraction levels)
3. **Exploratory** — you don't know the full picture yet; need to spike/research first
4. **User explicitly asks** — "make a plan", "design this", "how would you approach"
5. **Destructive operations** — large refactors, database migrations, deletions, merges

## When NOT to Use Plan Mode

- "Check the price of X" — one web search
- "Change line 32 of this file" — one edit
- "Run the tests to see if they pass" — terminal command
- "What does this function do?" — read + explain

These are simple tasks; execute directly.

## Quick Heuristic

| Signal | Tendency |
|--------|----------|
| >5 tool calls needed | Plan (or at least todo) |
| Need to read 3+ files to understand | Plan |
| Creating/deleting/renaming multiple files | At least todo |
| "If…then…else…" decision branches | Plan |
| User says "refactor/migrate/implement feature" | Plan |
| User says "check/search/run/look at" | Execute directly |

## Core Principle

When unsure, plan first. A plan costs 30 seconds of discussion. Guessing wrong can waste the entire session.

## Cross-Session Recovery

Plans persisted via the `planner` tool survive session restarts. To resume a plan
from a previous session, call `planner.resume` — it returns the full plan state
and the next task to execute. See the `planner` skill for details.
