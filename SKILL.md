# Skill Development Guide

Skills are YAML-defined capabilities that trigger on events and execute tools. They are the primary way to add custom behavior to aman.

## Skill Definition (SKILL.md)

Create a `SKILL.md` file in `~/.aman/skills/<name>/`:

```yaml
---
name: invoice-summarizer
version: 1.0.0
description: Summarize incoming invoice PDFs and post results to Slack
author: ops
triggers:
  - on: file_created
    match:
      source: filewatch:invoices
      payload_contains:
        extension: ".pdf"
concurrency: serial       # serial | parallel | limited(N)
tools:
  - file        # read the PDF
  - http        # post to Slack
steps:
  - tool: file
    params:
      path: "{{ event.payload.path }}"
  - tool: llm
    params:
      prompt: "Summarize this invoice: {{ steps[0].result }}"
  - tool: http
    params:
      url: "https://hooks.slack.com/..."
      method: POST
      body: "{{ steps[1].result }}"
timeout_sec: 30
```

## Trigger Conditions

Skills activate when events match their trigger conditions:

```yaml
triggers:
  - on: timer_tick          # EventType name (snake_case)
    match:
      source: "timer:*"     # Source pattern with wildcard
      priority: normal      # high | normal | low
      payload_contains:
        key: "value"        # Payload field must contain this value
```

## Built-in Tools

| Tool | Description |
|---|---|
| `file` | Read, write, delete, move files |
| `http` | HTTP requests (REST, GraphQL) |
| `exec` | Execute external commands (sandboxed) |
| `db` | Parameterized database queries |

## Skill Lifecycle

1. **Discovery**: aman scans `~/.aman/skills/` on startup
2. **Registration**: Each `SKILL.md` is parsed and registered
3. **Triggering**: Matching events activate the skill
4. **Execution**: Tools run sequentially with timeout protection
5. **Hot Reload**: Editing `SKILL.md` triggers automatic reload

## Example: Simple HTTP Webhook

```yaml
---
name: webhook-echo
version: 0.1.0
description: Echo incoming webhook payloads
triggers:
  - on: message_received
    match:
      source: "webhook:*"
tools:
  - http
steps:
  - tool: http
    params:
      url: "https://example.com/echo"
      method: POST
      body: "{{ event.payload }}"
```

## Development Workflow

1. Create a skill directory: `mkdir -p ~/.aman/skills/my-skill`
2. Write `SKILL.md` in that directory
3. aman auto-detects the skill (or restart `aman run`)
4. Test by injecting matching events: `aman event inject --type file_created --payload '{"path": "test.pdf"}'`
5. Edit the SKILL.md — aman hot-reloads changes automatically
