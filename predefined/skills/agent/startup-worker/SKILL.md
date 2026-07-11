---
name: startup-worker
version: "1.0.0"
category: agent
react_mode: direct
description: >
  Work-system integration skill for the Startup plugin. Called when a startup
  task (validation, strategy, execution, reflection) is pushed to the agent's
  WorkSystem. The agent acknowledges the task; the actual work is handled by
  the Startup plugin's validation pipeline. This skill exists so the agent's
  system_state transitions to Working while startup operations are in progress.
tags:
  - idle_run
  - work
  - startup
  - worker
triggers:
  - "check startup work"
  - "validate startup idea"
  - "startup task"
  - "创业验证"
idle_prompts:
  - "Check if there are any pending startup validation tasks assigned to you ({agent_id})."
  - "Look for startup work items assigned to {agent_id}. If there are pending validations, report their status."
---

# Startup Worker — Work System Integration

## Overview

This skill is a lightweight work-system bridge for the Startup plugin. When the
Startup plugin pushes a work item (validation, strategy, execution), the agent
receives it via `MessageReceived` → `process_message` → `direct_act`.

The actual work (LLM calls, SurrealDB updates, scoring) is handled by the
Startup plugin process — **not by this skill**. The agent's role is to:

1. Acknowledge the work item
2. Note the task details (idea slug, skill type)
3. Exit cleanly so the harness returns to Idle when the plugin finishes

## Two Entry Modes

### Direct Act! (from plugin push)

The startup context is already in your prompt (idea slug, skill type, title,
description). Read the context and acknowledge the task:

> Acknowledged startup task: {skill} for idea `{idea_slug}`. The Startup
> plugin's validation pipeline is handling the actual LLM analysis and data
> processing. This work item keeps the agent state in sync with the Work System.

Then exit. The plugin will complete the work item when its pipeline finishes.

### Idle Poll (boredom trigger)

Query the Startup plugin API to discover pending tasks:

```
GET http://127.0.0.1:9999/api/v1/startup/api/ideas
```

Look for ideas with `status: "in_validation"` — these have active pipelines
running. If none are found, report idle.

If you find running validations, check their progress:

```
GET http://127.0.0.1:9999/api/v1/startup/api/ideas/{slug}/validation-status
```

Report the current phase and estimated completion.

## Gateway API

All startup API calls use the Aman gateway:

```
http://127.0.0.1:9999
```

Full endpoint pattern: `http://127.0.0.1:9999/api/v1/startup/api/...`

If the gateway is unreachable at `127.0.0.1:9999`, try `http://localhost:9999`.

## Core Rule: The Plugin Does the Work

**Do NOT attempt to run startup validation yourself.** The Startup plugin has
specialized LLM prompts, SurrealDB storage, and scoring logic. Trying to
replicate this from the agent would produce inconsistent results.

Instead:
- Acknowledge the task
- Report the current status (query the plugin API)
- Exit

## Workflow

### Step 1: Read the Context

Your prompt contains the startup task details:
- **Idea Slug** — which idea is being worked on
- **Skill** — validate, landing_page, gtm, pricing_page, outreach, etc.
- **Title & Description** — human-readable summary

### Step 2: Acknowledge or Query

**If you have context** (Act! trigger): Acknowledge the task and exit.
**If you don't have context** (idle/boredom): Query the startup API for pending work.

### Step 3: Report Status

For monitoring, query the validation status endpoint and include it in your response:

```
GET /api/v1/startup/api/ideas/{slug}/validation-status
```

### Step 4: Exit

Output a concise status report. The harness will return to Idle, and the
Startup plugin will continue working in the background. When the pipeline
completes, the plugin emits a `startup:decided` event and the work item
is marked complete.

## Important Rules

1. **Don't run validation yourself.** The plugin is the authority for startup analysis.
2. **Don't modify startup data.** All SurrealDB writes go through the plugin.
3. **One status check is enough.** Don't poll repeatedly — the plugin handles its own progress.
4. **Idle is valid.** No pending startup tasks? Report it and exit.
5. **API errors are not blockers.** If the startup API is unreachable, report it and exit.

## Final Step: Mark Session When No Output Was Produced

After the workflow completes (or exits early with no pending tasks), judge
whether you produced any meaningful **output**: a status report with actual
validation data, a detected task, a triggered action, or a concrete state
change in any system.

If you truly produced **no output** — e.g. no startup tasks were pending, the
API was unreachable so no data could be reported, or the only thing you did was
report "no work" — you **MUST** make one final tool call to flag this session
as deletable:

```json
session({
  "marker": "deletable",
  "data": {
    "deletable": true,
    "reason": "<one sentence: why nothing was produced>"
  }
})
```

This writes a `session:marker` event to the session's persisted JSONL. Downstream
automation (sleep-phase cleanup) and the UI (delete button) use it to recognize
the session produced nothing of value. **Only call this when you genuinely have
nothing to show.** Never mark a session deletable if you produced real output.
