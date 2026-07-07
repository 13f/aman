---
name: plan
category: agent
description: >
  Pure planning — take a goal, break it into subtasks, execute them.
  No brainstorm, no review, no extra cognitive steps. For complex
  multi-step flows (scout → brainstorm → review → plan → execute),
  use the pipeline orchestration (01-complex-plan.yaml) instead.
version: 3.0.0
triggers:
  - "plan"
  - "make a plan"
  - "create a plan"
  - "规划"
  - "计划"
  - "方案"
  - "拆分"
  - "子任务"
  - "break this down"
  - "拆任务"
tags:
  - planning
  - task-decomposition
  - execution
metadata:
  hermes:
    tags: [planning, task-decomposition, execution]
    related_skills: [planner, writing-plans, subagent-driven-development, todo]
---

# Plan Mode

## Core Rule

**Goal → Subtasks → Execute. Nothing else.**

Plan takes a user goal, decomposes it into concrete subtasks, and executes
them. No brainstorming, no multi-lens review, no cognitive diagnosis.

For the full five-step closed loop (scout → brainstorm → review → plan →
execute → extract-exp), use the **pipeline orchestration**
(`01-complex-plan.yaml`) instead.

## When to Use

**Use plan when:**
- The user has a clear goal and wants it broken into steps
- Multi-step execution is needed but the direction is already decided
- As the "plan" step inside a pipeline (between review and execute)
- The user says "plan this", "break this down", "拆成子任务"

**Don't use plan when:**
- The direction is unclear → use brainstorm first (or full pipeline)
- Multi-lens validation is needed → use review first (or full pipeline)
- The task is a single action → just execute it
- The user wants creative exploration → use brainstorm

## Methodology

### 1. Understand the Goal

Read the user's request carefully. If anything is ambiguous, ask ONE
round of clarifying questions. Don't over-ask.

### 2. Decompose into Subtasks

Break the goal into 3-10 concrete subtasks. Each subtask must be:
- **Specific**: "Add auth middleware to /api/users" not "Add auth"
- **Actionable**: starts with a verb
- **Verifiable**: you know when it's done

### 3. Persist via Planner Tool

```markdown
1. `planner.create` — goal, milestones, success criteria
2. `planner.set_tasks` — decomposed task list with dependencies
```

After these calls, the **Orchestrator** takes over automatically:
- Picks unblocked tasks and spawns sub-agents
- Detects stalls and pivots
- Escalates to human if stuck

### 4. Monitor and Report

Use `planner.status` to track progress. Report to user:
- What's done
- What's in progress
- What's blocked (if any)

## Constraints

- **NO** destructive commands without user confirmation
- **YES** read files to understand before planning
- **YES** ask clarifying questions if the goal is ambiguous

## Output Example

```markdown
## Plan: Add user authentication

### Subtasks
1. [ ] Add bcrypt password hashing utility
2. [ ] Create /api/register endpoint
3. [ ] Create /api/login endpoint with JWT
4. [ ] Add auth middleware to protected routes
5. [ ] Write tests for auth flow

### Progress
- Completed: 2/5
- In progress: Create /api/login endpoint
- Blocked: none
```

## Cross-Session Recovery

Plans via `planner` tool survive restarts:
1. `planner.resume` — returns full state + next task
2. Orchestrator picks up where it left off

## Relationship to Other Skills

| Need | Use |
|---|---|
| Creative exploration, multiple directions | brainstorm |
| Multi-lens validation of a plan | review |
| Full five-step closed loop | 01-complex-plan.yaml pipeline |
| **Just plan and execute** | **plan (this skill)** |
