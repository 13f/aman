# Configuration Reference

Aman uses a layered configuration system with YAML files and environment variable overrides.

## Configuration File (`aman.yaml`)

```yaml
event_bus:
  mode: in_memory              # "in_memory" | "persistent"
  max_queue_size: 10000        # Max events in queue before backpressure
  backpressure:
    level1_threshold: 0.80     # 80% → reduce poll rate
    level2_threshold: 0.90     # 90% → drop at-most-once events
    level3_threshold: 0.95     # 95% → block publishers
    level4_threshold: 0.98     # 98% → overflow to disk
  dedup:
    window_ms: 30000           # Deduplication window

persistence:
  wal_sync: fsync              # "fsync" | "batch"
  checkpoint_interval: 500     # WAL checkpoint every N events
  wal_rotate_bytes: 1073741824 # 1 GB per WAL segment
  overflow_max_bytes: 1073741824

runtime:
  drain_timeout_sec: 30
  workflow_recovery_timeout: 120
  force_enable_debug_endpoints: false

http:
  bind: "127.0.0.1:9090"
  token: ""                    # API auth token (empty = no auth)
  tls:                         # Optional TLS
    cert_path: ""
    key_path: ""

plugins:                       # Plugin config directory
  dir: "~/.aman/plugins"

soul:                          # SOUL.md path
  file: "~/.aman/SOUL.md"

sources:                       # Event source definitions
  - type: timer
    id: heartbeat
    config:
      interval_ms: 60000
      heartbeat: true
  - type: cron
    id: daily_report
    config:
      expression: "0 9 * * *"
      timezone: "UTC"
  - type: webhook
    id: github_events
    config:
      path: "/hooks/github"
      port: 9091
      trust_level: trusted

skills:
  dir: "~/.aman/skills"        # Auto-discovered skills directory
  search:
    enabled: true
  hot_reload:
    enabled: true
    debounce_ms: 500

secret:
  backend: env                 # "env" | "vault" | "aws" | "1password"
  cache_ttl_sec: 300
  retry_count: 3

workflow:
  timeout_defer_ms: 5000       # User events defer timeouts by 5s
  retry_cancel_conflict_defer_ms: 5000

dlq:
  ttl_days: 30
  max_manual_retries: 5
```

## Environment Variables

All config fields can be overridden via `AMAN_*` environment variables:

```bash
export AMAN_EVENT_BUS__MODE=persistent
export AMAN_EVENT_BUS__MAX_QUEUE_SIZE=50000
export AMAN_RUNTIME__DRAIN_TIMEOUT_SEC=60
```

Double underscores (`__`) separate nested keys. Environment variables take precedence over YAML config.

## Config Layers

| Layer | Source | Priority |
|---|---|---|
| 1 | Hardcoded defaults | Lowest |
| 2 | `aman.yaml` config file | Medium |
| 3 | `AMAN_*` environment variables | High |
| 4 | Runtime overrides (`POST /config/set`) | Highest |
