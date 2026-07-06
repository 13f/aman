---
name: plan
category: agent
description: >
  Orchestration router for complex tasks. Plan itself does NOT solve
  problems — it routes to the right sub-skills (Brainstorm, Review,
  subagent-driven-development) and guards constraints. Use when the task is
  multi-stage, involves architecture decisions, or is destructive.
version: 2.0.0
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
  - orchestration
metadata:
  hermes:
    tags: [planning, architecture, design, orchestration]
    related_skills: [brainstorm, review, planner, writing-plans, subagent-driven-development, todo]
---

# Plan Mode

## Core Rule

**Plan is an orchestrator, not a solver. Route to sub-skills. Guard constraints.**

Plan's job: decide WHICH sub-skills to invoke, in WHAT order, and WHAT guard
conditions apply. The actual thinking happens inside the sub-skills.

## When to Route to Which Sub-Skill

```
START
  │
  ├─ Situation unclear (vague request, missing goal)
  │   → Ask clarifying questions FIRST, then re-evaluate
  │
  ├─ Knowledge gap (unfamiliar domain, don't know the codebase)
  │   → Context Scout: read files, search code, inspect structure
  │
  ├─ Multiple possible directions (architecture choice, feature design)
  │   → Brainstorm: generate 3-5 distinct options
  │   → Review: evaluate options across Correctness/Safety/Performance/Cost
  │   → User picks a direction (or you recommend if asked)
  │
  ├─ Clear direction, needs multi-step execution
  │   → Write structured plan via planner tool
  │   → Review: validate the plan before execution
  │   → subagent-driven-development: execute task-by-task
  │
  └─ Experience=Apprehensive (EXP.md shows past failure on this)
      → Brainstorm (force alternatives to the failed path)
      → Review (validate new direction avoids the failure mode)
      → Proceed with new plan
```

## Constraint Guards (always active)

Regardless of which sub-skills you invoke:

- **NO** code writing for source files (read-only exploration only)
- **NO** file modification except writing plan markdown to `.aman/plans/`
- **NO** destructive commands (`rm`, `git reset --hard`, `DROP TABLE`)
- **YES** read-only exploration: read files, search code, run `git log`
- **YES** non-destructive queries: `cargo check`, `grep`, `find`

## Structured Plan Output

Use the `planner` tool to persist the plan as structured state:

1. **`planner.create`** — goal, milestones, success criteria (from Brainstorm + Review output)
2. **`planner.set_tasks`** — decomposed task list with dependencies

After these two calls, the **Orchestrator** takes over automatically:
- Picks unblocked tasks and spawns anonymous sub-agents
- Detects stalls and pivots directions
- Escalates to human if all directions are exhausted

Monitor progress with `planner.status`. You do NOT need to manually call
`planner.start`, `planner.complete`, or `planner.increment_stale`.

## When NOT to Use Plan Mode

- "Check the price of X" — one web search (execute directly)
- "Change line 32 of this file" — one edit (execute directly)
- "What does this function do?" — read + explain (execute directly)

## Quick Heuristic

| Signal | Route |
|--------|-------|
| Doesn't know what user wants | Clarify first |
| Don't know the domain well | Context Scout → Brainstorm |
| Architecture / design decision | Brainstorm → Review → Plan |
| Clear multi-step execution | Plan → Review → Execute |
| Past failure on this kind of task | Brainstorm (force alternatives) |

## Cross-Session Recovery

Plans persisted via the `planner` tool survive session restarts. To resume:
1. Call `planner.resume` — returns full plan state + next task
2. The Orchestrator detects the `plan:resumed` event and continues execution

## Core Principle

> Plan is cheap. Guessing is expensive. When unsure which direction to take,
> Brainstorm + Review before committing.
