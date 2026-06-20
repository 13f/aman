---
name: planner
category: agent
description: >
  API reference for the planner tool — structured plan state management with
  persistent storage under ~/.aman/plans/.  Covers all operations: create,
  set_tasks, start, complete, fail, append_finding, record_direction,
  increment_stale, status, resume.  For guidance on WHEN to create plans,
  see the `plan` skill.
version: 1.2.0
metadata:
  hermes:
    tags: [planner, planning, task-tracking, execution, long-horizon, state-management, cross-session]
    related_skills: [plan, writing-plans, todo, subagent-driven-development]
---

# Planner — API Reference

## Overview

The `planner` tool provides structured, persistent plan state for agent tasks.
Unlike markdown plan files (which are human-readable but not machine-readable),
planner stores plans as JSON files under `~/.aman/plans/`, enabling:

- **Cross-session resume** — close a session, come back later, pick up where you left off
- **Structured task DAG** — tasks with dependencies, milestones, and status tracking
- **Anti-loop primitives** — record tried directions, detect stalls via `stale_count`
- **Intermediate findings** — append-only JSONL for accumulating evidence during research

## File Layout

```
~/.aman/plans/
  {plan_id}.plan            ← task DAG + goal + milestones + directions tried
  {plan_id}.progress        ← execution checkpoint (iteration, stale_count, current_task)
  {plan_id}.findings.jsonl  ← append-only intermediate findings
```

All three files share the same `plan_id` prefix. A plan is identified by its `plan_id`
(typically the creating session_id).

## Core Concepts

| Concept | Where | What |
|---------|-------|------|
| **Goal** | `.plan` | One-sentence description of what the plan aims to achieve |
| **Milestones** | `.plan` | Checkpoints with id, description, and verification criteria |
| **Success criteria** | `.plan` | How we know the plan is done (overall acceptance criteria) |
| **Tasks** | `.plan` | Decomposed work units with id, title, description, depends_on, status |
| **Directions tried** | `.plan` per task | Approaches already explored (anti-loop) |
| **Progress** | `.progress` | Current iteration, stale_count, current_task/milestone/direction |
| **Findings** | `.findings.jsonl` | Accumulated intermediate results (append-only) |

### Task statuses

```
pending → in_progress → completed
pending → in_progress → failed → (pending via retry) → ...
pending → in_progress → failed → blocked (exceeded retries or non-retryable)
```

## Operation Reference

All operations are called via the `planner` tool with a required `operation` field.

### `create` — Initialize a new plan

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `plan_id` | string | no | Plan identifier (defaults to auto-generated UUID-based id) |
| `goal` | string | **yes** | One-sentence description of what the plan aims to achieve |
| `milestones` | array | no | Milestone objects with `{id, description, verification}` |
| `success_criteria` | string | no | Overall acceptance criteria |
| `round_cap` | integer | no | Max rounds per session before forcing a pause (default 15) |

```json
{
  "operation": "create",
  "goal": "Fix backpressure OOM in event-bus L4B",
  "milestones": [
    {"id": "m1", "description": "Identify root cause", "verification": "Can reproduce OOM under load test"},
    {"id": "m2", "description": "Implement fix", "verification": "cargo test passes, 1M events survive"},
    {"id": "m3", "description": "Regression guard", "verification": "OOM test added to CI"}
  ],
  "success_criteria": "event-bus survives 1M events at 10x normal throughput without OOM",
  "round_cap": 15
}
```

Returns: `{ ok, operation, plan_id, plan_path }`

### `set_tasks` — Write the decomposed task list

Replaces the entire task list. Also resets progress (iteration, stale_count) since this
represents a fresh decomposition.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `plan_id` | string | **yes** | Plan identifier |
| `tasks` | array | **yes** | Task objects |

Task object fields:
- `id` (string, required) — Short machine-readable id, e.g. "1", "profile-mem"
- `title` (string, required) — Human-readable title
- `description` (string, required) — What this task should accomplish
- `depends_on` (array of strings, optional) — Task ids that must complete before this one
- `milestone_id` (string, optional) — Milestone this task contributes to

```json
{
  "operation": "set_tasks",
  "plan_id": "sess_abc123",
  "tasks": [
    {
      "id": "1",
      "title": "Profile memory usage in backpressure path",
      "description": "Use heaptrack to identify allocation hotspots in L4B path under load",
      "milestone_id": "m1"
    },
    {
      "id": "2",
      "title": "Add bounded queue with backpressure signal",
      "description": "Cap VecDeque, emit backpressure before overflow at L4A",
      "depends_on": ["1"],
      "milestone_id": "m2"
    },
    {
      "id": "3",
      "title": "Add OOM regression test",
      "description": "Test that 1M events survive without OOM under 10x throughput",
      "depends_on": ["2"],
      "milestone_id": "m3"
    }
  ]
}
```

Returns: `{ ok, operation, plan_id, task_count }`

### `start` — Begin working on a task

Marks the task as `in_progress` and updates the progress pointer.
Optionally sets the current milestone and direction.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `plan_id` | string | **yes** | Plan identifier |
| `task_id` | string | **yes** | Task to start |
| `current_milestone_id` | string | no | Set current milestone in progress |
| `current_direction_id` | string | no | Set current direction in progress |

```json
{
  "operation": "start",
  "plan_id": "sess_abc123",
  "task_id": "1",
  "current_milestone_id": "m1",
  "current_direction_id": "d1"
}
```

Rules:
- Task must be `pending` or `failed` status
- All `depends_on` tasks must be `completed`
- Starting a `completed` or `blocked` task returns an error
- Starting an already `in_progress` task is idempotent (returns success with note)

Returns: `{ ok, operation, plan_id, task_id, status: "in_progress" }`

### `complete` — Mark a task as done

Records the result, marks the task `completed`, resets `stale_count` to 0,
and returns any tasks that became unblocked as a result.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `plan_id` | string | **yes** | Plan identifier |
| `task_id` | string | **yes** | Task to complete |
| `result` | string | **yes** | Summary of what was accomplished |

```json
{
  "operation": "complete",
  "plan_id": "sess_abc123",
  "task_id": "1",
  "result": "Found: VecDeque in L4B grows unbounded when consumers stall. Root cause confirmed via heaptrack."
}
```

Returns: `{ ok, operation, plan_id, task_id, status: "completed", unblocked_tasks: [...] }`

### `fail` — Record a task failure

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `plan_id` | string | **yes** | Plan identifier |
| `task_id` | string | **yes** | Task that failed |
| `reason` | string | **yes** | Why the task failed |
| `retryable` | boolean | no | Can this task be retried? (default true) |

Retry behavior:
- `retryable: true` — Status set to `failed`. Can be restarted with `start`.
  After 3 retries, auto-blocks the task.
- `retryable: false` — Status set to `blocked`. Cannot be restarted.

```json
{
  "operation": "fail",
  "plan_id": "sess_abc123",
  "task_id": "2",
  "reason": "transient network error connecting to metrics endpoint",
  "retryable": true
}
```

Returns: `{ ok, operation, plan_id, task_id, status: "failed"|"blocked" }`

### `append_finding` — Save intermediate result

Appends a finding to the `.findings.jsonl` file. Use this during long-running
research/exploration tasks to accumulate evidence incrementally.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `plan_id` | string | **yes** | Plan identifier |
| `task_id` | string | **yes** | Task this finding belongs to |
| `finding` | string | **yes** | The finding text |
| `confidence` | number | no | Confidence level 0.0–1.0 (default 1.0) |

```json
{
  "operation": "append_finding",
  "plan_id": "sess_abc123",
  "task_id": "1",
  "finding": "L4B consumers stall under concurrent write load above 500 rps",
  "confidence": 0.9
}
```

Returns: `{ ok, operation, plan_id, task_id, seq }`

### `record_direction` — Record a tried approach (anti-loop)

Records a direction/approach explored for a task. If a direction with the same
`id` already exists, it is updated in place (not duplicated).

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `plan_id` | string | **yes** | Plan identifier |
| `task_id` | string | **yes** | Target task |
| `direction` | object | **yes** | `{id, description, parameters?}` |

Direction object:
- `id` (string, required) — Short machine-readable id, e.g. "d1", "heap-profile"
- `description` (string, required) — Human description of the approach
- `parameters` (object, optional) — Key-value map where each value describes what that parameter means

```json
{
  "operation": "record_direction",
  "plan_id": "sess_abc123",
  "task_id": "1",
  "direction": {
    "id": "d1",
    "description": "Heaptrack profiling with massif output format",
    "parameters": {
      "threshold_kb": "Memory threshold in KB to sample at. Lower values = finer granularity.",
      "sample_interval_ms": "How often to sample. Lower values catch shorter spikes."
    }
  }
}
```

Returns: `{ ok, operation, plan_id, task_id, directions_count }`

### `increment_stale` — Signal no progress

Increments `stale_count` by 1. Used by the Orchestrator (or manually) when
a task iteration produced no progress. `stale_count` resets to 0 on `complete`.
At ≥ 3 the Orchestrator auto-pivots; at ≥ 6 it escalates to human.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `plan_id` | string | **yes** | Plan identifier |

```json
{
  "operation": "increment_stale",
  "plan_id": "sess_abc123"
}
```

Returns: `{ ok, operation, plan_id, stale_count }`

### `status` — Full plan + progress snapshot

Returns the complete plan state with current progress. If `plan_id` is omitted,
tries to find the most recently modified plan in `~/.aman/plans/`.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `plan_id` | string | no | Plan identifier (auto-detects if omitted) |

```json
{
  "operation": "status",
  "plan_id": "sess_abc123"
}
```

Returns: `{ ok, plan_id, goal, milestones, success_criteria, round_cap, tasks, progress, findings_count, created_at, updated_at }`

### `resume` — Recover execution after session restart

Same as `status` but additionally computes and returns the `next_task` to execute.
Use this at the start of a new session to pick up where a previous session left off.

Priority order for next_task:
1. In-progress task (interrupted — resume it first)
2. First pending task with all deps met
3. First failed task (consider retrying)
4. `null` if no remaining work

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `plan_id` | string | no | Plan identifier (auto-detects if omitted) |
| `session_id` | string | no | Current session id (recorded for tracing) |

Returns: `{ ok, plan_id, goal, milestones, tasks, progress, next_task, next_milestone, findings_count, ... }`

## Workflow Patterns

### Simple task execution

```
planner.create → planner.set_tasks
  → planner.start(id="1") → planner.complete(id="1")
  → planner.start(id="2") → planner.complete(id="2")
  → planner.status  (verify all done)
```

### Long-horizon research with findings

```
planner.create(goal="Research X", milestones=[...])
  → planner.set_tasks(tasks=[...])
  → planner.start(id="1")
  → [exploration work...]
  → planner.append_finding(task_id="1", finding="...")
  → planner.append_finding(task_id="1", finding="...")
  → planner.record_direction(task_id="1", direction={...})
  → planner.complete(id="1", result="...")
  → planner.start(id="2")
  ...
```

### Cross-session resume

```
# Session 1:
planner.create(goal="...")
  → planner.set_tasks(...)
  → planner.start(id="1")
  → [work happens...]
  → planner.complete(id="1", ...)
  → planner.start(id="2")
  → [session ends]

# Session 2:
planner.resume
  → next_task: {id: "2", status: "in_progress", note: "task was interrupted"}
  → [continue work on task 2...]
```

### Stall recovery (reference)

Stall detection and pivot are handled automatically by the Orchestrator.
This section documents the underlying operations for advanced/manual use.

```json
// Signal no progress:
{"operation": "increment_stale", "plan_id": "sess_abc123"}

// After stale_count >= 3, record a new direction:
{"operation": "record_direction", "plan_id": "sess_abc123", "task_id": "3",
 "direction": {"id": "d2", "description": "Alternative approach"}}

// Revise tasks and resume:
planner.set_tasks(revised_tasks)
planner.resume
```

## Integration with Agent Skills

The planner tool is the **structured state layer**. Agent skills describe **methodology**:

| Skill | How it uses planner |
|-------|-------------------|
| `plan` | After exploring the codebase (read-only), calls `planner.create` + `planner.set_tasks` to persist the structured plan. The Orchestrator then picks up the plan automatically — no manual `start`/`complete` needed. |
| `writing-plans` | Describes plan quality standards. After writing a markdown plan, calls `planner.set_tasks` to produce the structured equivalent. |
| `todo` | For short-lived, single-session tasks, continue using in-memory todo lists. For long-lived tasks, delegate to `planner.create` + `planner.set_tasks`. |
| `subagent-driven-development` | Subagents call `planner.start` / `planner.complete` / `planner.fail` per task, using the plan_id from the orchestrator. |

## File Format Details

### `.plan` (JSON)

```json
{
  "plan_id": "sess_abc123",
  "goal": "Fix backpressure OOM in event-bus L4B",
  "milestones": [
    {"id": "m1", "description": "Identify root cause", "verification": "Can reproduce OOM"}
  ],
  "success_criteria": "1M events survive at 10x throughput",
  "round_cap": 15,
  "tasks": [
    {
      "id": "1",
      "title": "Profile memory",
      "description": "...",
      "depends_on": [],
      "milestone_id": "m1",
      "status": "completed",
      "result": "Found root cause: unbounded VecDeque",
      "directions_tried": [
        {
          "id": "d1",
          "description": "Heaptrack profiling",
          "parameters": {"threshold_kb": "Memory threshold in KB"}
        }
      ]
    }
  ],
  "created_at": "2026-06-20T10:30:00Z",
  "updated_at": "2026-06-20T11:45:00Z"
}
```

### `.progress` (JSON)

```json
{
  "plan_id": "sess_abc123",
  "iteration": 3,
  "current_task_id": "2",
  "current_milestone_id": "m2",
  "current_direction_id": "d2",
  "stale_count": 0,
  "last_progress_at": "2026-06-20T11:45:00Z",
  "last_session_id": "sess_xyz789",
  "retry_counts": {}
}
```

### `.findings.jsonl` (JSON Lines)

```jsonl
{"task_id":"1","seq":0,"ts":"2026-06-20T10:35:00Z","finding":"L4B consumers stall above 500rps","confidence":0.9}
{"task_id":"1","seq":1,"ts":"2026-06-20T10:40:00Z","finding":"VecDeque has no upper bound","confidence":1.0}
```

## Limitations

1. **Single-agent scope** — Plans live under `~/.aman/plans/` and are not agent-specific.
   Future multi-agent scenarios will need agent-scoped plans.
2. **Orchestrator handles dispatch** — Sub-agent spawning and stall detection are handled
   by the Orchestrator (autonomous background actor). The planner tool itself only manages
   state — you don't need to manually spawn sub-agents or check `stale_count`.
3. **File-based** — Plans survive on disk. There is no in-memory caching layer.
   Concurrent access by multiple processes may race; use plan-level locking if needed.
