---
name: kanban-worker
version: "1.0.0"
category: agent
react_mode: direct
description: >
  Background idle-run skill that queries the Team kanban for work items
  assigned to the current agent, executes them if found, or reports idle
  if the queue is empty. Designed for boredom-triggered autonomous execution.
tags:
  - idle_run
  - work
  - kanban
  - worker
triggers:
  - "check my work"
  - "process work items"
  - "what's on my board"
  - "看板任务"
  - "我的任务"
idle_prompts:
  - "Check the kanban board for any work items assigned to you ({agent_id}). Process them if found."
  - "Look at your assigned tasks on the team board and work on the highest priority one."
  - "Query the kanban for tasks assigned to {agent_id}. If there are none, report that you're idle."
---

# Kanban Worker — Autonomous Work Item Execution

## Overview

This skill runs autonomously (triggered by the idle/boredom system) to check
for work items assigned to the current agent and execute them. It is designed
to be called without user interaction — the agent checks its queue, picks up
work, and reports progress.

## Output Directory & Deliverables

Every work item has a dedicated output directory for storing deliverables:

```
{work_dir}/aman_team/{work_id}/
```

This directory is created automatically when the work item is created. **All
output files (code, reports, assets, etc.) must be placed in this directory.**
Do NOT scatter output files across the project or leave them in temporary
locations.

### Reading the Output Type

When you pick up a work item, check its `output_type` and `output_description`
fields in the work item response. These tell you what kind of deliverable is
expected:

- `code` → produce source code files. The `output_description` will specify
  language, framework, and target modules.
- `report` → produce a report document. The `output_description` gives the
  expected scope.
- `ppt` / `image` / `video` / `audio` / `3d_model` / `design` / `prototype` →
  produce the corresponding media/asset files.
- `data` → produce a dataset (CSV, JSON, etc.).
- `document` / `spreadsheet` → produce documents or spreadsheets.
- If empty → the work item doesn't have a specified output type yet. You can
  infer it from the work description and set it when you finish.

### Writing Output Files

```
{work_dir}/aman_team/{work_id}/{filename}
```

Examples:
- `aman_team/work-1716900000000/fix_backpressure.patch`
- `aman_team/work-1716900000000/q2_analysis_report.md`
- `aman_team/work-1716900000000/sales_data_q2.csv`

### Updating Output Info

When you complete (or make significant progress on) the work item, update its
output type and description so the kanban board reflects what was produced:

```
POST /api/v1/team/projects/{project_key}/works/{work_id}/output
Content-Type: application/json

{
  "output_type": "code",
  "output_description": "Rust — fix event-bus crate backpressure.rs L4B threshold, add 3 unit tests"
}
```

If the work item already had output_type set, verify it's still accurate and
update `output_description` with what was actually delivered.

## Core Rule: Exit Early If No Work

**Before doing anything else**, query your assigned work items. If the
response is empty or no items are assigned to you, output a brief summary
and stop. Do NOT fabricate tasks. Do NOT try other tools to "find" work.

## Workflow

### Step 1: Query Assigned Work Items

Find which agent you are:

```
GET /api/v1/team/projects
```

From the response, pick a project. Then list works:

```
GET /api/v1/team/projects/{project_key}/works
```

Look through the results for work items where:
- The work item is in an active stage (not "done", "closed", "archived")
- The work item is assigned to you (check the `assignee` or `agent_id` field)
- Or: the work item is unassigned but in a stage with `auto_assign` policy

**If no work items are assigned to you:** output the following and STOP:

> No work items assigned. Agent is idle.

Do NOT invoke any other tools. Do NOT try to create work items. Do NOT
search for work elsewhere. The idle check is complete.

### Step 2: Read Work Item Context

Once you've found an assigned work item, read its full context:

```
GET /api/v1/team/projects/{project_key}/works/{work_id}/context?raw=1&max_lines=200
```

This returns the JSONL event history (thoughts, tool calls, responses,
human directions) for this work item. Use this to understand:
- What has been done so far (check `step_complete` events)
- What the current step is
- Any human directions or safety gates
- Whether this is a new item or a resume ("断点续传")

### Step 3: Execute the Current Step

Based on the work item's description, steps, and context:

1. **If the work item has predefined steps**: work through them in order.
   Mark each step complete after finishing it.
2. **If the work item has no steps**: analyze the description and determine
   the next action. Break complex work into smaller steps.
3. **If resuming**: skip completed steps, continue from the first incomplete one.

For each step:
- Use the appropriate tools (file operations, HTTP calls, code execution, etc.)
- Record your thought process
- Record tool call inputs and outputs

### Step 4: Update Progress

After completing a step, append to the work item context:

```
POST /api/v1/team/projects/{project_key}/works/{work_id}/comment
Content-Type: application/json

{
  "event_type": "step_complete",
  "step_index": 0,
  "total_steps": 3,
  "summary": "Analyzed the backpressure module and identified the OOM root cause",
  "success": true
}
```

Or for general progress updates:

```
POST /api/v1/team/projects/{project_key}/works/{work_id}/comment
Content-Type: application/json

{
  "event_type": "thought",
  "content": "The queue threshold needs to be lowered from 10K to 5K..."
}
```

### Step 5: Complete or Escalate

When all steps are done:
- Mark the work item as complete
- Report the outcome

If you encounter a problem you cannot solve:
- Record what you tried
- Set the work item to blocked/failed with a clear error message
- Do NOT retry indefinitely — 3 attempts per step is the maximum

If a safety gate triggers:
- Stop immediately
- Record the safety event
- Wait for human direction

## Important Rules

1. **One work item at a time.** Don't try to parallelize across multiple items.
2. **Respect the stage.** Don't execute items in "backlog" unless the scheduler
   moved them to an active stage.
3. **Record everything.** Every thought, tool call, and response should be
   appended to the work item context. The next agent (or your next run) needs
   the full history.
4. **Don't create work items.** This skill EXECUTES existing work. Use the
   `team` skill to create new ones.
5. **Idle is a valid outcome.** Finding no work is not an error. Report it
   cleanly and exit.
6. **Stop on repeated failures.** If the same tool call fails 3 times with
   the same error, record the failure and move on or stop.
