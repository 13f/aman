# CLI Reference

The `aman` CLI provides runtime management and administrative commands.

## Global Options

```
--config <path>      Config file path (default: aman.yaml)
--soul <path>        SOUL.md path
--log-level <level>  Log level (trace, debug, info, warn, error) [default: info]
```

## Commands

### `aman run`

Start the aman agent runtime.

```bash
aman run --config ~/.aman/config.yaml --soul ~/.aman/SOUL.md
```

### `aman health`

Check agent health.

```bash
aman health live     # Process alive check
aman health ready    # Full readiness check
```

### `aman skill`

Manage skills.

```bash
aman skill list                  # List all registered skills
aman skill search <query>        # Search skills by name/description
aman skill info <name>           # Show skill details
aman skill enable <name>         # Enable a skill
aman skill disable <name>        # Disable a skill
aman skill version <name>        # Show version history
aman skill rollback <name> <ver> # Rollback to specific version
```

### `aman plugin`

Manage plugins.

```bash
aman plugin list                 # List installed plugins
aman plugin enable <name>        # Enable a plugin
aman plugin disable <name>       # Disable a plugin
aman plugin install <file>       # Install plugin from tar.gz
aman plugin uninstall <name>     # Uninstall a plugin
```

### `aman event`

Inject and inspect events.

```bash
aman event inject --type <type> --payload '{"key": "val"}'
aman event trace <trace_id>      # Get event trace
aman event dump <id>             # Dump event details
```

### `aman workflow`

Manage workflow instances.

```bash
aman workflow list               # List workflow instances
aman workflow show <id>          # Show instance details
aman workflow retry <id>         # Retry failed instance
aman workflow cancel <id>        # Cancel instance
```

### `aman config`

Manage runtime configuration.

```bash
aman config show                 # Show current config
aman config validate             # Validate config file
aman config set <key> <value>    # Update config at runtime
```

### `aman dlq`

Manage dead letter queue.

```bash
aman dlq list                    # List DLQ entries
aman dlq retry <id>              # Retry DLQ event
aman dlq discard <id>            # Discard DLQ event
```

## Exit Codes

| Code | Meaning |
|---|---|
| 0 | Success |
| 1 | General error |
| 2 | Config validation error |
| 3 | Runtime already running |
| 4 | Runtime not running |
