---
name: cron
category: agent
description: Manage per-agent cron (scheduled) jobs — create, list, update, and remove recurring tasks that fire on a cron schedule. Jobs persist across gateway restarts. Use when asked to schedule a recurring task, set up a reminder, or manage timed automation.
version: 1.0.0
metadata:
  tags: [cron, scheduling, automation, jobs, timer]
  related_tools: [aman.add_cron_job, aman.update_cron_job, aman.remove_cron_job, aman.list_cron_jobs]
---

# Cron Job Management

Create and manage scheduled recurring tasks that fire on a cron expression.
Jobs are persisted to `~/.aman/agents/{agent_key}/cron/jobs.json` and survive
gateway restarts automatically.

## Quick Reference

| Operation | Tool | Key Params |
|-----------|------|-----------|
| Create | `aman.add_cron_job` | `id`, `expression`, `agent_key` |
| List | `aman.list_cron_jobs` | `agent_key` |
| Update | `aman.update_cron_job` | `id`, `expression`?, `timezone`?, `agent_key` |
| Delete | `aman.remove_cron_job` | `id`, `agent_key` |

All operations require `agent_key` identifying which agent owns the job.
Use the current agent's key.

## Creating a Cron Job

```json
{
  "id": "daily-report",
  "expression": "0 9 * * 1-5",
  "agent_key": "default"
}
```

- **`id`** — unique identifier (re-using an existing id replaces it).  Use
  `kebab-case` names that describe the task (`weekly-review`, `morning-checkin`).
- **`expression`** — 5-field cron: `minute hour day-of-month month day-of-week`.
  See [Cron Expression Reference](#cron-expression-reference) below.
- **`agent_key`** — the agent that owns this job.  Defaults to empty string if
  omitted, but always specify it explicitly.

### Timezone

Cron jobs default to UTC.  To use a local timezone, pass `timezone` when
updating (no `timezone` param on create — create first, then update):

```json
// aman.update_cron_job
{
  "id": "daily-report",
  "expression": "0 9 * * 1-5",
  "timezone": "Asia/Shanghai",
  "agent_key": "default"
}
```

## Listing Cron Jobs

```json
// aman.list_cron_jobs
{ "agent_key": "default" }
```

Returns all persisted jobs with their metadata (expression, timezone,
enabled status, last run timestamp, last status).

## Updating a Cron Job

```json
// aman.update_cron_job
{
  "id": "daily-report",
  "expression": "0 10 * * 1-5",
  "timezone": "America/New_York",
  "agent_key": "default"
}
```

Both `expression` and `timezone` are optional — only the fields you provide
are changed.  Passing neither is a no-op.

## Removing a Cron Job

```json
// aman.remove_cron_job
{ "id": "daily-report", "agent_key": "default" }
```

This shuts down the running job and removes it from both the registry and
the persisted `jobs.json`.

## Cron Expression Reference

Standard 5-field format: `minute hour dom month dow`

| Field    | Values   | Special characters      |
|----------|----------|-------------------------|
| minute   | 0–59     | `*` `,` `-` `/`         |
| hour     | 0–23     | `*` `,` `-` `/`         |
| dom      | 1–31     | `*` `,` `-` `/`         |
| month    | 1–12     | `*` `,` `-` `/`         |
| dow      | 0–7 (0=Sun) | `*` `,` `-` `/`     |

### Common Patterns

| Pattern           | Meaning                      |
|-------------------|------------------------------|
| `*/5 * * * *`     | Every 5 minutes              |
| `0 * * * *`       | Every hour, on the hour      |
| `0 9 * * 1-5`     | Weekdays at 9:00 AM          |
| `0 0 1 * *`       | Midnight on the 1st of month |
| `*/30 8-18 * * 1-5` | Every 30 min during business hours, weekdays |
| `0 9,17 * * *`    | 9:00 AM and 5:00 PM daily    |

## Design Conventions

- **One job per concern** — don't overload a single job with multiple
  unrelated tasks.  If you need two different schedules, create two jobs.
- **Descriptive IDs** — `morning-standup-reminder`, not `job1`.
- **Check before creating** — call `aman.list_cron_jobs` first to avoid
  accidental duplicates.  Creating a job with an existing id overwrites it.
- **Timezone matters** — default is UTC.  If the user wants "9 AM my time",
  ask what timezone they're in, then set it via `aman.update_cron_job`.
- **Jobs survive restarts** — there is no need to recreate them after
  the gateway reboots.  Stale jobs should be explicitly removed.

## When to Use Cron

Good fits:
- Recurring reminders ("check email every 30 minutes")
- Scheduled reports ("summarize open issues every Monday at 9 AM")
- Periodic maintenance ("clean up old sessions every Sunday at 3 AM")
- Polling external state ("check CI status every 5 minutes")

Not a good fit:
- One-shot delayed tasks — use `TimerSource` or a deferred task queue instead
- High-frequency (< 1 minute) polling — cron has second-level precision at best
- Event-driven triggers — use a WebhookSource or FileWatchSource instead
