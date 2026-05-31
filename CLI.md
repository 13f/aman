# CLI Reference

The `aman` CLI provides runtime management and administrative commands over HTTP REST (default) or gRPC (`--grpc`).

## Usage Pattern

```
aman <command> [subcommand] [options]
```

Most commands contact a running `aman run` or `aman serve` instance. Exceptions:
- `aman config show|validate` loads the config file locally
- `aman skill validate|export` operates on local files without a gateway

## Global Connection Options

These flags are accepted on **all commands that contact a remote gateway**:

```
--addr <ip:port>       Gateway address (default: 127.0.0.1:8080)
--token <token>        API token (default: $AMAN_API_TOKEN)
--operator <name>      Operator identity (sets x-aman-operator header)
--confirm              Confirm destructive operations (sets x-aman-confirm header)
--grpc                 Use gRPC transport instead of HTTP REST
```

Flags that apply only to the `run` and `serve` commands are documented inline.

## Commands

### `aman run`

Start the aman agent runtime (HTTP gateway).

```bash
aman run --config ~/.aman/config.yaml --soul ~/.aman/SOUL.md --bind 127.0.0.1:8080
```

**Options:**

| Flag | Description |
|------|-------------|
| `--config <path>` | Config file path (default: aman.yaml) |
| `--soul <path>` | SOUL.md system prompt path |
| `--bind <ip:port>` | Gateway bind address (default: 127.0.0.1:8080) |
| `--token <token>` | API token for incoming requests |
| `--daemon` | Daemonize process |
| `--log-level <level>` | Log level: trace, debug, info, warn, error (default: info) |

Prints the bind address to stdout on success, then blocks until SIGINT/SIGTERM.

### `aman serve`

Start the agent runtime in stdio JSON-RPC mode (for MCP and subprocess invocation).

```bash
aman serve --config ~/.aman/config.yaml --soul ~/.aman/SOUL.md
```

**Options:**

| Flag | Description |
|------|-------------|
| `--config <path>` | Config file path |
| `--soul <path>` | SOUL.md system prompt path |

Reads JSON-RPC 2.0 requests from stdin, writes responses to stdout.

### `aman health`

Check agent health.

```bash
aman health ready              # Full readiness check (runtime is live and accepting work)
```

**Subcommands:**

| Subcommand | Description |
|------------|-------------|
| `ready` | Full readiness check (live + accepting requests). Returns exit 0 if OK, exit 1 if not. |
| `live` | Not implemented yet. |

### `aman agent`

Start or shut down the agent runtime (lifecycle control).

```bash
aman agent start                    # Start agent processing
aman agent shutdown                 # Graceful shutdown
```

**Exit codes:** 0 = success, 3 = 409 Conflict (already started/stopped), 1 = other error.

### `aman metrics`

Fetch runtime metrics as JSON.

```bash
aman metrics [--format json]
```

`--format` only accepts `json` (the default output format).

### `aman audit-log`

Query the audit log.

```bash
aman audit-log [--action <a>] [--operator <o>] [--since-ms <ms>] [--until-ms <ms>] [--limit <n>] [--offset <n>]
```

All filters are optional. Returns JSON array.

### `aman event`

Inject, push, inspect, and list events.

```bash
aman event inject --source <s> --type <t> --payload <json>   # Inject an event (returns JSON)
aman event push --source <s> --type <t> --payload <json>|--payload-stdin [--agent <id>] [--priority <p>] [--delivery <d>] [--ttl-ms <ms>]   # Push event to agent queue
aman event types                                               # List registered event types
aman event dump --id <event_id>                                # Dump event details
aman event trace --trace-id <trace_id>                         # Get event trace by trace ID
```

### `aman dlq`

Manage dead letter queue entries.

```bash
aman dlq list [--reason <r>] [--source <s>] [--event-type <t>] [--limit <n>] [--offset <n>]   # List DLQ entries
aman dlq retry --id <id> [--reason <r>]                                                        # Retry a DLQ entry
aman dlq discard --id <id>                                                                      # Discard a DLQ entry
```

### `aman source`

Manage event sources (pause, resume, reconfigure).

```bash
aman source pause --id <id>          # Pause an event source
aman source resume --id <id>         # Resume a paused source
aman source config --id <id> --json <patch>   # Update source configuration (JSON merge patch)
```

### `aman cron`

Manage cron job definitions.

```bash
aman cron add --id <id> --expression <expr>        # Add a cron job
aman cron update --id <id> --json <patch>           # Update a cron job (JSON merge patch)
aman cron remove --id <id>                          # Remove a cron job
```

### `aman skill`

Manage and validate skills. Some subcommands work locally without a running gateway.

```bash
# Local commands (no gateway needed):
aman skill validate [path]          # Validate SKILL.md files against the spec
aman skill export <out_dir>         # Export skills to a spec-compliant directory tree

# Remote commands (gateway required):
aman skill list                     # List all registered skills (JSON)
aman skill search --q <query> [--limit <n>]   # Search skills by name/description
aman skill info --name <name>       # Show skill details (JSON)
aman skill enable --name <name>     # Enable a skill
aman skill disable --name <name>    # Disable a skill
aman skill version --name <name>    # Show version history (JSON)
aman skill rollback --name <name> --version <ver>   # Rollback to specific version
```

### `aman plugin`

Manage plugins.

```bash
aman plugin list                    # List installed plugins (JSON)
aman plugin enable --name <name>    # Enable a plugin
aman plugin disable --name <name>   # Disable a plugin
aman plugin install --file <path.tar.gz>   # Install plugin from archive
aman plugin uninstall --name <name>  # Uninstall a plugin
```

### `aman workflow`

Manage workflow instances.

```bash
aman workflow list                  # List workflow instances (JSON)
aman workflow show --id <id>        # Show instance details (JSON)
aman workflow retry --id <id>       # Retry failed instance
aman workflow cancel --id <id>      # Cancel instance
```

### `aman config`

Manage runtime configuration (local config file, no gateway needed).

```bash
aman config show [--config <path>] [--override <path>]                                   # Show effective config (pretty-printed JSON)
aman config validate [--config <path>] [--override <path>]                               # Validate config file
aman config set --override <path> --json <partial_agent_config_json> [--config <path>]   # Set runtime override values
```

### `aman --version`

Print version and exit.

```bash
aman --version
aman -V
```

## Exit Codes

| Code | Meaning |
|---|---|
| 0 | Success |
| 1 | General error (HTTP error, I/O failure, gRPC error) |
| 2 | Invalid arguments, missing required flags, or usage error |
| 3 | Conflict (HTTP 409 — e.g., already started, DLQ retry conflict) |
