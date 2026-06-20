---
name: todo
category: agent
description: Lightweight task tracking for medium-complexity tasks. Create a structured task list, track progress, and adjust iteratively — without the overhead of a full plan. Use when the task needs 3+ distinct steps but doesn't warrant architecture discussion.
version: 1.0.0
metadata:
  hermes:
    tags: [task-tracking, organization, execution, progress]
    related_skills: [planner, plan, writing-plans]
---

# Todo Mode

## When to Use

Todo mode is the middle ground between "just do it" and "write a full plan."

**Use todo when:**
- The task needs 3+ distinct steps to complete
- You'll be modifying 2-5 files
- There's a clear path but it needs tracking
- You want to show the user progress without asking for plan approval first

**Skip todo when:**
- The task is a single straightforward action (just do it)
- The task involves architecture decisions or trade-offs (use plan instead)
- The task spans multiple subsystems or crates (use plan instead)

## How It Works

1. **Create tasks** — break the work into discrete, ordered steps
2. **Mark in_progress** — claim a task before starting work
3. **Execute** — work through the task completely
4. **Mark completed** — when the task is fully done (tests pass, code committed)
5. **Adjust** — if you discover new tasks during execution, add them on the fly

## Task Structure

Each task should be:
- **Specific** — "Fix authentication bug in login flow" not "Fix bugs"
- **Actionable** — starts with a verb: Add, Fix, Update, Remove, Refactor
- **Verifiable** — you know when it's done (tests pass, compiles, etc.)

## Status Flow

```
pending → in_progress → completed
```

- `pending`: not yet started
- `in_progress`: currently working on it (only ONE at a time)
- `completed`: fully done, verified

## Key Rules

1. **One task at a time** — don't mark multiple tasks in_progress simultaneously
2. **Complete before moving on** — don't leave a trail of half-done tasks
3. **Add discovered work** — if you find a new subtask while working, create a new task for it
4. **Tasks are ephemeral** — todo lists live only for this session; don't save them as files
5. **No user confirmation needed** — unlike plan mode, todo mode doesn't require approval; just track and execute

## Difference from Plan Mode

| Aspect | Plan | Todo |
|--------|------|------|
| User confirmation | Required before execution | Not required |
| Output | Markdown file on disk | In-memory task list |
| Granularity | 2-5 min tasks with full code | Descriptive task names |
| When to use | Architecture decisions, multi-system | Multi-step, clear path |
| Overhead | High (write, review, approve) | Low (create, track, adjust) |

## Anti-patterns

- ❌ Creating a todo list for a single straightforward action
- ❌ Using todo when you should plan (architecture decisions involved)
- ❌ Marking a task complete when tests are still failing
- ❌ Having 3+ tasks in_progress at once
- ❌ Tasks that are too vague to verify ("Improve the code")

## When to Use Planner Instead

Todo lists are in-memory and ephemeral — they die with the session. For tasks that
should survive session restarts (long-horizon research, multi-day work, cross-session
recovery), use the `planner` tool instead:

1. `planner.create` to initialize the plan with a goal
2. `planner.set_tasks` to write the task list as structured state
3. `planner.start` / `planner.complete` / `planner.fail` for task lifecycle

See the `planner` skill for the full operation reference.
