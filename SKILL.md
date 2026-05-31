# Skill Development Guide

Skills are the primary way to add custom behavior to aman. There are two distinct skill systems:

- **Event-driven skills** (`.yaml`/`.yml` files): Declarative skills with trigger conditions that fire on matching events.
- **LLM-instruction skills** (`SKILL.md` files): Agent Skills standard format with YAML frontmatter and a markdown body that the LLM reads as methodology.

This guide covers both systems.

---

## Event-Driven Skills (`.yaml` / `.yml`)

Create a `.yaml` file in `~/.aman/skills/<name>/`:

```yaml
---
name: invoice-summarizer
version: 0.1.0
description: Summarize incoming invoice PDFs and post results to Slack
triggers:
  - event_types: [file_created]
    sources: [filewatch:invoices]
    priorities: [normal]
    match_all: true
concurrency: serial       # serial | parallel | { limited: N }
tags:
  - billing
  - automation
idle_prompts:
  - "check for new invoices"
---
```

The skill body (anything after the final `---`) is stored but not executed as steps -- the skill system provides the triggers and concurrency management, and the body is available to LLM invocation flows.

### Trigger Conditions

Skills activate when events match their trigger conditions:

```yaml
triggers:
  - event_types: [timer_tick]       # EventType name (snake_case string)
    sources: [timer:cron-hourly]     # Source pattern
    priorities: [normal]             # high | normal | low
    match_all: true                  # ALL fields must match (default: any match)
```

Available trigger fields:

| Field | Description |
|---|---|
| `event_types` | List of `EventType` strings (e.g., `file_created`, `timer_tick`, `webhook_received`, `message_received`) |
| `sources` | List of source identifiers (e.g., `filewatch:invoices`, `timer:*`, `webhook:billing`) |
| `priorities` | List of priorities: `high`, `normal`, `low` |
| `match_all` | When `true`, ALL specified fields must match (AND logic). Default `false` means ANY field match suffices (OR logic). |

When all fields are empty, the trigger matches any event.

### Concurrency

```yaml
concurrency: serial       # One execution at a time (default)
concurrency: parallel     # Unlimited concurrent executions
concurrency:              # Limited to N concurrent executions
  limited: 3
```

### Built-in Tools

| Tool | Description |
|---|---|
| `read` | Read file contents |
| `write` | Write content to a file |
| `edit` | Apply string replacements in a file |
| `list` | List directory contents |
| `find` | Search for files by pattern |
| `grep` | Search file contents for text |
| `http` | HTTP requests (REST, GraphQL) |
| `exec` | Execute external commands (sandboxed). Supports `detach: true` for long-running background processes with progress events. |
| `db` | Parameterized database queries (SQLite) |
| `web_search` | Web search via configured provider |
| `web_fetch` | Fetch and extract content from a URL |

There is no single `file` tool. File operations are split into individual tools: `read`, `write`, `edit`, `list`, `find`, `grep`. There is no `llm` tool.

---

## LLM-Instruction Skills (SKILL.md)

The Agent Skills standard uses `SKILL.md` files with YAML frontmatter:

```markdown
---
name: my-skill
description: Does something useful
category: General
triggers:
  - "keyword trigger"
---

# Skill Methodology

Full markdown body with instructions, methodology, templates, and output format.
```

These are NOT event-driven. The LLM decides when to use them based on `name`, `description`, `category`, and `triggers` injected into its system prompt.

### SKILL.md Frontmatter Fields

| Field | Description |
|---|---|
| `name` (required) | Skill name, must match directory name |
| `description` (required) | Short description shown in the skill index |
| `category` | Grouping category for the skill index |
| `triggers` | List of keyword trigger strings for LLM matching |
| `metadata` | Arbitrary metadata (can include nested maps) |

---

## Skill System Architecture

### Lifecycle (Event-Driven Skills)

1. **Discovery**: aman recursively scans `~/.aman/skills/` on startup. Supported files: `.yaml`, `.yml`, and `SKILL.md`.
2. **Registration**: Each file is parsed and registered with the `SkillRegistry`. Duplicate names are rejected.
3. **Triggering**: The `SkillExecutor` iterates enabled skills and calls `skill_matches_event()` against each incoming event.
4. **Execution**: The skill's `execute()` method is invoked asynchronously with the event and context.
5. **Hot Reload**: The `HotReloadManager` uses `notify` file system watcher (500ms debounce) to detect file changes, additions, and deletions. Changes trigger automatic re-registration and route refresh.

### Lifecycle (LLM-Instruction Skills)

1. **Discovery**: `SkmRegistry` recursively walks `~/.aman/skills/` for `SKILL.md` files.
2. **Indexing**: Skills are indexed in a Tantivy in-memory search index for fast lookup by name, description, or tags.
3. **Selection**: The LLM selects a skill based on the skill index injected into its system prompt.
4. **Loading**: A `read_skill(skill: "...")` tool call reads the full SKILL.md body and injects it into the conversation.
5. **Activation**: An activation message signals the LLM to follow the skill's methodology.
6. **Hot Reload**: SKILL.md file changes are detected by the file watcher and the Tantivy index is updated.

### Hot Reload Details

- Uses the `notify` crate (cross-platform file watcher).
- Debounce period: 500ms (configurable via `with_debounce_ms`).
- Watches recursively for any file change under the skills directory.
- On reload:
  - New files are inserted.
  - Changed files are upserted (same version = updated, new version = new).
  - Deleted files are unregistered.
  - Invalid files (bad YAML, version parse failure) are logged but do not block other files.
  - The `RouteRefreshNotifier` is called when routes change (if configured).
  - Skill version history is saved via `SkillVersionManager`.

### Development Workflow

1. Create a skill directory: `mkdir -p ~/.aman/skills/my-skill`
2. Write `my-skill.yaml` or `SKILL.md` in that directory
3. aman auto-detects the skill (or restart `aman run`)
4. Test by injecting matching events: `aman event inject --type file_created --payload '{"path": "test.pdf"}'`
5. Edit the file -- aman hot-reloads changes automatically (within ~500ms debounce)

### Validation

Run `aman skill validate <path>` to check SKILL.md files against the agentskills.io specification:

| Rule | Description |
|---|---|
| R0 | Not a skill directory or SKILL.md file |
| R1 | Failed to parse SKILL.md frontmatter |
| R5 | SKILL.md not found in skill directory |
| R6 | Directory name must match frontmatter name |
| R7 | Unexpected files in skill directory (warning) |
| R8 | Empty trigger pattern (warning) |
| R9 | Unknown skill referenced in `related_skills` (warning) |
