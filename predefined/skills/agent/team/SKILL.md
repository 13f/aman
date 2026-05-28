---
name: team
category: agent
description: >
  Use when the user asks to create a work item, track work, submit a ticket,
  or when the conversation reveals an actionable piece of work that should be
  tracked on a kanban board. Teaches the agent how to extract structured work
  items from chat, resolve the target project, and submit via the Team API.
triggers:
  - "create work item"
  - "submit task"
  - "track this"
  - "add to kanban"
  - "put this on the board"
  - "create a ticket"
  - "file this as a task"
  - "记得把这个记下来"
  - "增加任务"
  - "创建任务"
  - "添加任务"
  - "新建任务"
  - "加一个任务"
  - "记录任务"
  - "登记任务"
metadata:
  triggers: "create work item, submit task, track this, add to kanban, create a ticket, file this as a task"
---

# Team — Chat-Driven Work Item Creation

## Overview

The Team system is a kanban scheduler where work items emerge from conversations.
An agent can recognize actionable content in chat and submit it as a tracked
work item to any Team project, with itself as the creator. The agent does NOT
execute the work item — it only creates it. Execution is handled by the kanban
scheduler and the assigned agent.

## When to Use

**Proactively create a work item when:**
- The user explicitly asks ("create a ticket for this", "add to kanban")
- The user describes a concrete bug, feature, or work item during conversation
- The user mentions a problem and it's clear they want it tracked
- The user says "记下来", "track this", "follow up on this"

**Skip when:**
- The user is just thinking out loud or exploring ideas
- No concrete action is implied ("maybe someday we should...")
- The request is trivial and would be done immediately by the current agent
- The user is asking a question or requesting information

## Team API Reference

All endpoints are under the aman gateway. Replace `{gateway}` with the
gateway origin (e.g., `http://127.0.0.1:PORT`).

### List Projects

```
GET /api/v1/team/projects
```

Response:
```json
{
  "projects": [
    {
      "project_key": "aman-core",
      "project_name": "Aman Core Team",
      "description": "Aman agent framework development",
      "stage_count": 5,
      "stages": [
        {"id": "backlog", "name": "待办"},
        {"id": "wip", "name": "处理中"}
      ]
    }
  ]
}
```

### Create a Work Item

```
POST /api/v1/team/projects/{project_key}/works/create
Content-Type: application/json

{
  "title": "Fix event-bus backpressure OOM risk",
  "description": "The backpressure thresholds in src/event-bus/backpressure.rs are too high...",
  "priority": "normal",
  "tags": ["bug", "performance"],
  "creator": "agent-id-here"
}
```

| Field | Type | Required | Notes |
|---|---|---|---|
| `title` | string | **yes** | One-line summary |
| `description` | string | no | Full context, reproduction steps, constraints |
| `priority` | string | no | `low` / `normal` / `high` / `critical` (default: `normal`) |
| `tags` | string[] | no | Labels for categorization (e.g., `["bug", "performance"]`). |
| `creator` | string | no | Your agent ID. If omitted, the work has no creator. |

Response (201):
```json
{
  "id": "work-1716900000000",
  "title": "Fix event-bus backpressure OOM risk",
  "description": "...",
  "current_stage": "backlog",
  "priority": "normal",
  "tags": ["bug", "performance"],
  "source_type": "manual",
  "creator": "agent-id-here"
}
```

### List Works in a Project

```
GET /api/v1/team/projects/{project_key}/works
```

### Get a Single Work Item

```
GET /api/v1/team/projects/{project_key}/works/{work_id}
```

### Read Work Item Context (JSONL history)

```
GET /api/v1/team/projects/{project_key}/works/{work_id}/context
GET /api/v1/team/projects/{project_key}/works/{work_id}/context?raw=1&max_lines=100
```

## Workflow

### Step 1: Identify the Work Item

When the conversation contains an actionable item, extract these fields:

- **title** — One clear sentence in imperative mood. Max ~80 characters.
  "Fix event-bus OOM when queue exceeds 10K items" not "there's a memory problem in the event bus somewhere"
- **description** — All relevant context. Include:
  - What the problem is (or what needs to be built)
  - Where it is (file paths, components)
  - Why it matters (impact, urgency)
  - Any constraints or preferences the user mentioned
  - Links to relevant conversation messages
- **priority** — Default to `normal`. Use `high`/`critical` only when the
  user clearly signals urgency. Use `low` for nice-to-haves.

If the conversation doesn't contain enough detail for a meaningful title and
description, ask one clarifying question before creating. Don't guess.

### Step 2: Resolve the Target Project

Before creating, you must know which project to submit to. Try these in order:

1. **Explicit mention** — Did the user say "add to X project" or "track in X"?
2. **Context fit** — Does the work item clearly belong to a specific project?
   (e.g., code changes for `aman-core`, investment research for `investloop`)
3. **List projects** — Call `GET /api/v1/team/projects` and see what's available.
   If there's only one project, use it.
4. **Ask the user** — If you still can't determine the project, present the
   list and ask:

   > I can create a work item for this. Which project should it go to?
   > - `aman-core` — Aman Core Team
   > - `investloop` — Investment Loop

   Wait for the user's answer before proceeding.

### Step 3: Confirm with the User

Before submitting, show the extracted work item for confirmation:

> I'll create this work item in **{project_name}**:
>
> **Title:** {title}
> **Priority:** {priority}
> **Description:** {summary or full description}
>
> Create it?

Only skip confirmation if the user explicitly said to just do it (e.g.,
"create a work item for this" with no ambiguity). If the title or project could
be wrong, confirm first.

### Step 4: Submit

Use the HTTP tool to POST to the create endpoint. Set `creator` to your
own agent ID (the name you use as an agent in the team system).

```
POST /api/v1/team/projects/{project_key}/works/create
```

### Step 5: Report the Result

After creation, report back with the work ID and stage:

> Created [#{work_id}]({board_url}) in **{project_name}** → {stage_name}

The agent does NOT start working on it — the scheduler handles assignment.
If the user wants you to work on it, they'll ask or move the card.

## Title Writing Rules

| Bad | Good |
|---|---|
| "memory issue" | "Fix event-bus backpressure OOM when queue exceeds 10K events" |
| "add the thing" | "Add retry budget metrics to pipeline dashboard" |
| "fix it" | "Fix deadlock in persistence WAL rotate under concurrent write" |
| "maybe refactor?" | "Extract AuthMiddleware from gateway http.rs into separate crate" |

Titles should be specific enough that a different agent could understand
the scope without reading the full description.

## Description Writing Rules

Include enough context for the assigned agent to start working without
re-reading the entire conversation. Structure:

```
## Context
<1-2 sentences about the problem>

## Location
<file paths, components, or systems involved>

## Expected Outcome
<what "done" looks like>

## Constraints (if any)
<things NOT to change, edge cases to preserve>

## Source
<reference to the chat where this came from>
```

## Common Pitfalls

1. **Creating work items for trivial things.** "Rename this variable" doesn't
   need a kanban card — just do it. The team board is for work that benefits
   from tracking, delegation, or multi-stage review.

2. **Guessing the project.** If there are 3 projects and the user didn't
   specify, don't pick one based on vibes. Ask.

3. **Vague titles.** "Improve the thing" is useless an hour later. A different
   agent reading the board should understand the scope from the title alone.

4. **Forgetting to set `creator`.** Without it, the work has no creator and
   loses the connection to the conversation that spawned it.

5. **Including the solution in the title.** "Replace HashMap with BTreeMap
   in pipeline scores" assumes the fix. Use "Pipeline score lookup slower
   than expected under 10K entries" — describe the problem, not the fix.

6. **Not confirming before submitting.** Unless the user explicitly said
   "go ahead" or the intent is unambiguous, confirm the extracted title
   and project before POSTing. A wrong submission is worse than no submission.

7. **Starting work on the work item you just created.** Creating a work item and
   executing it are separate flows. Unless the user asks you to work on it,
   let the scheduler handle it.

## Verification Checklist

- [ ] Title is specific, imperative mood, ≤ 80 characters
- [ ] Description has context, location, expected outcome
- [ ] Priority is set (default normal)
- [ ] Target project is explicitly chosen (not guessed)
- [ ] User confirmed (or explicitly asked you to proceed)
- [ ] `creator` is set to your agent ID
- [ ] API response 201 received with work ID
- [ ] Work ID and stage reported back to user
- [ ] You did NOT start working on it
