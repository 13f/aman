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

This skill has two entry modes:

- **Act! (direct trigger):** The work item details and history are already in your
  prompt. Skip API queries, go straight to **Step 1: Understand the Work Item**.
- **Boredom/idle poll:** You need to discover work yourself. Start from
  **Step 0: Query Assigned Work Items** below.

### Step 0: Query Assigned Work Items (boredom mode only)

Find which agent you are:

```
GET http://127.0.0.1:9999/api/v1/team/projects
```

From the response, pick a project. Then list works **excluding items already
marked for review**:

```
GET http://127.0.0.1:9999/api/v1/team/projects/{project_key}/works?exclude_need_review=1
```

Look for work items where:
- The work item is in an active stage (not "done", "closed", "archived")
- The work item is assigned to you (check `assignee` field)
- Or: the work item is unassigned but in a stage with `auto_assign` policy

**If no work items are assigned to you:**
> No work items assigned. Agent is idle.

Stop. Do NOT fabricate tasks.

## Gateway API

The Aman gateway serves the Team API. All HTTP calls to team endpoints must use
the gateway base URL:

    http://127.0.0.1:9999

Full endpoint pattern: `http://127.0.0.1:9999/api/v1/team/projects/{project_key}/works/{work_id}/...`

If the gateway is not reachable at that address, try `http://localhost:9999`.

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

### Step 1: Understand the Work Item

When you receive a work item (via Act! or idle poll), the prompt already
includes the work history. Read it fully:

- **Title & Description** — what problem are you solving?
- **Output Type** — what deliverable is expected? (report, code, etc.)
- **Work History** — what has been done so far? Is this new or a resume?

### Step 1.5: Mark Work as In Progress

**Before you start working, move the work item to In Progress** so the kanban
board reflects the current state:

```
POST http://127.0.0.1:9999/api/v1/team/projects/{project_key}/works/{work_id}/complete
Content-Type: application/json

{
  "agent_id": "<your agent id>",
  "next_stage": "in_progress",
  "summary": "Starting work",
  "confidence": 1.0
}
```

This transitions the card from Todo → In Progress on the kanban board.
(Applies to both Act! and boredom-triggered work.)

### Step 2: Explore & Plan (Turn 1)

**Before writing any code or report, explore the relevant parts of the codebase.**
You cannot solve a problem you don't understand.

1. **Map the territory.** Use `list`, `grep`, and `read` to explore the
   directories, files, and modules relevant to the work description.
2. **Identify what needs to change.** Which files? What's the current behavior?
3. **Make a plan.** In your reply, state clearly:
   - What you found during exploration
   - What specific changes you will make
   - What files you will create/modify
   - What the expected outcome is

This plan becomes your Turn 1 reply. The harness will feed it back as context
for Turn 2 (execution).

**Minimum exploration before planning:** at least 3-5 tool calls (grep, read,
list) to understand the relevant code. Do NOT plan based on guesses.

### Step 3: Execute the Plan (Turn 2)

Execute each item in your plan:

1. **For code changes**: use `edit` or `write` to make changes, then verify.
2. **For analysis/reports**: gather all data first, then write the report in
   `aman_team/{work_id}/`.
3. **For multi-step work**: work through steps in order, recording progress.

For each step:
- Use the appropriate tools (file operations, grep, code execution, etc.)
- Record your thought process via comments on the work item
- Verify each change before moving on

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

When all steps are done and you believe the work is complete:

1. **Update the output info** so the kanban board reflects what was produced:
   ```
   POST /api/v1/team/projects/{project_key}/works/{work_id}/output
   Content-Type: application/json

   {
     "output_type": "code",
     "output_description": "<what was actually delivered>"
   }
   ```

2. **Move the work item to Review stage** (from In Progress). This signals
   to humans that the agent considers the work done and it's ready for review.
   The card will move to the "Review" column on the kanban board:
   ```
   POST http://127.0.0.1:9999/api/v1/team/projects/{project_key}/works/{work_id}/complete
   Content-Type: application/json

   {
     "agent_id": "<your agent id>",
     "next_stage": "review",
     "summary": "Brief description of what was completed and where to find the output",
     "confidence": 0.9
   }
   ```

3. Report the outcome succinctly.

**Important:** Use `next_stage: "review"` — this moves the card to the Review
column where a human can verify the work. The human will then move it to "done"
(accept) or "todo" (reject with comments). Do NOT set `next_stage: "done"` —
that skips human review.

If you encounter a problem you cannot solve:
- Record what you tried
- Set the work item to blocked/failed with a clear error message
- Do NOT retry indefinitely — 3 attempts per step is the maximum

If a safety gate triggers:
- Stop immediately
- Record the safety event
- Wait for human direction

## Tool Failure Recovery (CRITICAL)

**A tool error is NOT a reason to stop.** Most errors are fixable — wrong path,
missing argument, network blip. You MUST recover and continue.

### Error → Fix → Retry loop

1. **Read the error carefully.** What exactly failed?
2. **Fix the root cause.** Wrong directory? Try the correct one. Connection refused?
   Try the alternate URL. Missing file? Search for it.
3. **Retry with the fix.** Do NOT skip the step — complete it.
4. **Only give up after 3 attempts on the SAME step.** But "attempt" means trying
   the SAME broken call — fixing the arguments and trying again is a NEW attempt.

### Common errors and their fixes

| Error pattern | What it means | Fix |
|---|---|---|
| `No such file or directory` | Wrong path | List parent dir, find correct path, retry |
| `Connection refused` | Wrong URL/port | Try `http://127.0.0.1:9999` then `http://localhost:9999` |
| `not found` (HTTP 404) | Wrong endpoint | Check the API path, verify work_id/project_key |
| `Permission denied` | Can't access | Try a different path or report as blocker |

### Never do this

- ❌ Return an EMPTY reply. Always say what happened and what you'll try next.
- ❌ Abandon the work item after one tool error. Fix it.
- ❌ Skip a step because the tool failed. Do it another way.
- ❌ Say "I cannot proceed" without trying at least 2 alternative approaches.

If you truly cannot continue after 3 genuinely different attempts, post a
comment explaining what you tried and why it failed, then mark the work as
needing review with a clear failure summary.

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
7. **Move completed work to Review.** When you finish a work item, use
   `POST /complete` with `next_stage: "review"` (NOT "done"). This moves
   the card to the Review column for human verification.
8. **NEVER use the `db` tool on the Team database** (`~/.aman/team/projects/*/data.db`).
   All Team operations MUST go through the HTTP API. Direct DB writes bypass
   stage validation and corrupt the work item state. If an HTTP endpoint returns
   404, fix the URL — do NOT fall back to SQLite.
9. **Name output files descriptively.** Use the work item's title to derive a
   meaningful filename (e.g. `trace-log-analysis.md`, `backpressure-fix.patch`).
   Do NOT use generic names like `review-report.md` or `output.md`.
