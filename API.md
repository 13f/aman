# HTTP API Reference

aman exposes an HTTP API on `127.0.0.1:9090` (configurable via `http.bind`).

## Authentication

Set `http.token` in config. Requests must include:

```
Authorization: Bearer <token>
```

## Endpoints

### Health

| Method | Path | Description |
|---|---|---|
| `GET` | `/health/live` | Process alive (Phase 0+) |
| `GET` | `/health/ready` | Ready to serve (Phase 5) |

### Runtime

| Method | Path | Description |
|---|---|---|
| `POST` | `/agent/start` | Start runtime (idempotent) |
| `POST` | `/agent/shutdown` | Graceful shutdown (requires `x-aman-confirm: yes`) |

### Events

| Method | Path | Description |
|---|---|---|
| `POST` | `/inject-event` | Inject event into bus (debug, requires `force_enable_debug_endpoints`) |
| `GET` | `/events/trace/{trace_id}` | Get event trace by trace ID |
| `GET` | `/events/dump/{id}` | Dump event by ID |

### Event Sources

| Method | Path | Description |
|---|---|---|
| `POST` | `/source/{id}/pause` | Pause event source |
| `POST` | `/source/{id}/resume` | Resume event source |
| `PUT` | `/source/{id}/config` | Update source config |

### Plugins

| Method | Path | Description |
|---|---|---|
| `GET` | `/plugin/list` | List all plugins |
| `POST` | `/plugin/{name}/enable` | Enable plugin |
| `POST` | `/plugin/{name}/disable` | Disable plugin |
| `POST` | `/plugin/install` | Install plugin (multipart tar.gz) |
| `POST` | `/plugin/{name}/uninstall` | Uninstall plugin |

### Workflows

| Method | Path | Description |
|---|---|---|
| `GET` | `/workflow/instances` | List workflow instances |
| `GET` | `/workflow/def/{name}` | Get workflow definition |
| `POST` | `/workflow/{id}/retry` | Retry failed instance |
| `POST` | `/workflow/{id}/cancel` | Cancel instance |

### DLQ

| Method | Path | Description |
|---|---|---|
| `GET` | `/dlq` | List DLQ entries (cursor pagination) |
| `POST` | `/dlq/{id}/retry` | Retry DLQ event (requires `x-aman-confirm: yes`) |
| `POST` | `/dlq/{id}/discard` | Discard DLQ event |

### Config

| Method | Path | Description |
|---|---|---|
| `POST` | `/config/set` | Update runtime config (audited) |

### Observability

| Method | Path | Description |
|---|---|---|
| `GET` | `/metrics` | Prometheus metrics |
| `GET` | `/audit-log` | Audit log (cursor pagination + filters) |

### Cron

| Method | Path | Description |
|---|---|---|
| `POST` | `/cron/add` | Add cron job |
| `POST` | `/cron/{id}/update` | Update cron job |
| `POST` | `/cron/{id}/remove` | Remove cron job |

## Headers

| Header | Used For |
|---|---|
| `Authorization: Bearer <token>` | API authentication |
| `x-aman-confirm: yes` | Confirm destructive actions (shutdown, dlq retry, plugin disable) |
| `x-aman-operator: <name>` | Operator name (audit logging) |

## Response Format

Success: `200 OK` with JSON body (or `text/plain` for `/metrics`).

Errors:
- `400 Bad Request` — Invalid input
- `401 Unauthorized` — Missing/invalid token
- `403 Forbidden` — Soul boundary blocked
- `404 Not Found` — Resource not found
- `409 Conflict` — Confirmation required or operation in progress
- `429 Too Many Requests` — Rate limited
- `503 Service Unavailable` — Not ready / draining
