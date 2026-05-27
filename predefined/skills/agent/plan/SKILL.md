---
name: plan
category: agent
description: Plan mode — when the task is complex, multi-stage, involves architecture decisions, or is destructive, enter planning mode. Explore the codebase (read-only), write a detailed implementation plan to .aman/plans/, do NOT execute any code changes. Use before any non-trivial implementation task.
version: 1.0.0
metadata:
  hermes:
    tags: [planning, architecture, design, preparation]
    related_skills: [writing-plans, subagent-driven-development, todo]
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

Write ONE file: `.aman/plans/YYYY-MM-DD_HHMMSS-<slug>.md`

The plan must be self-contained and machine-readable — any subagent should be able to execute it with zero additional context.

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
