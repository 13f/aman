# HTTP API Reference

aman exposes an HTTP API on `127.0.0.1:9090` (configurable via `http.bind`).

## Authentication

Set `http.token` in config. Requests must include one of:

```
Authorization: Bearer <token>
x-aman-token: <token>
```

All endpoints except the ones listed under **Public Endpoints** require authentication.

## Public Endpoints (no auth required)

| Method | Path | Description |
|---|---|---|
| `GET` | `/health/live` | Process alive (Phase 0+) |
| `GET` | `/health/ready` | Ready to serve (Phase 5) |
| `GET` | `/health` | Alias for `/health/ready` |
| `GET` | `/health/llm` | LLM provider ready |
| `GET` | `/metrics` | Prometheus metrics (text/plain) |
| `GET` | `/ui/pages` | List UI plugin pages |
| `GET` | `/events/stream` | SSE event stream for frontend |

## Authenticated Endpoints

### Runtime Control

| Method | Path | Description |
|---|---|---|
| `POST` | `/agent/start` | Start runtime (idempotent) |
| `POST` | `/agent/shutdown` | Graceful shutdown (requires `x-aman-confirm: yes`) |
| `GET` | `/runtime/status` | Current phase, ready/live status |
| `GET` | `/runtime/config` | Bind address, runtime dir, skills dir, etc. |

### Skills

| Method | Path | Description |
|---|---|---|
| `GET` | `/skills` | List all skills |
| `GET` | `/llm-skills` | List LLM-visible skill triggers |
| `GET` | `/skills/search` | Search skills (`?q=` and `?limit=`) |
| `GET` | `/skill/{name}` | Get skill snapshot |
| `GET` | `/skill/{name}/content` | Get skill raw content |
| `POST` | `/skill/{name}/enable` | Enable skill |
| `POST` | `/skill/{name}/disable` | Disable skill |
| `GET` | `/skill/{name}/versions` | List skill version history |
| `POST` | `/skill/{name}/rollback` | Rollback skill (requires `x-aman-confirm: yes`) |
| `POST` | `/skills/reload` | Reload all skills from disk |

### Events

| Method | Path | Description |
|---|---|---|
| `POST` | `/inject-event` | Inject event into bus (debug, requires `force_enable_debug_endpoints`) |
| `POST` | `/events/push` | Push external event (supports agent_id, priority, delivery, ttl_ms) |
| `GET` | `/events/trace/{trace_id}` | Get event trace by trace ID |
| `GET` | `/events/dump/{id}` | Dump event by ID |
| `GET` | `/events/recent` | Recent events (`?limit=`, max 1000) |
| `GET` | `/events/types` | List known event types |

### Event Sources

| Method | Path | Description |
|---|---|---|
| `POST` | `/event-source/{id}/pause` | Pause event source |
| `POST` | `/event-source/{id}/resume` | Resume event source |
| `PUT` | `/event-source/{id}/config` | Update source config |
| `POST` | `/source/{id}/pause` | Alias for `/event-source/{id}/pause` |
| `POST` | `/source/{id}/resume` | Alias for `/event-source/{id}/resume` |
| `PUT` | `/source/{id}/config` | Alias for `/event-source/{id}/config` |
| `POST` | `/im-channel/{platform}/{instance}/reload` | Reload IM channel source |

### Plugins

| Method | Path | Description |
|---|---|---|
| `GET` | `/plugins` | List all plugins |
| `POST` | `/plugin/{name}/enable` | Enable plugin |
| `POST` | `/plugin/{name}/disable` | Disable plugin (requires `x-aman-confirm: yes`) |
| `POST` | `/plugin/install` | Install plugin (multipart tar.gz) |
| `POST` | `/plugin/{name}/uninstall` | Uninstall plugin |

### Workflows

| Method | Path | Description |
|---|---|---|
| `GET` | `/workflows` | List workflow definitions |
| `GET` | `/workflow/{name}` | Get workflow definition |
| `POST` | `/workflow/{name}/create` | Create workflow instance |
| `GET` | `/workflow-instances` | List workflow instances |
| `GET` | `/workflow-instance/{id}` | Get single workflow instance |
| `POST` | `/workflow-instance/{id}/retry` | Retry failed instance (requires `x-aman-confirm: yes`) |
| `POST` | `/workflow-instance/{id}/cancel` | Cancel instance (requires `x-aman-confirm: yes`) |

### DLQ

| Method | Path | Description |
|---|---|---|
| `GET` | `/dlq` | List DLQ entries (cursor pagination) |
| `GET` | `/dlq/depth` | DLQ depth count |
| `POST` | `/dlq/{id}/retry` | Retry DLQ event (requires `x-aman-confirm: yes`) |
| `POST` | `/dlq/{id}/discard` | Discard DLQ event |

### Config

| Method | Path | Description |
|---|---|---|
| `POST` | `/config/set` | Update runtime config (audited, requires `x-aman-confirm: yes`) |

### Chat Sessions

| Method | Path | Description |
|---|---|---|
| `GET` | `/chat/sessions` | List chat sessions (`?agent_id=`) |
| `POST` | `/chat/session/create` | Create new chat session |
| `GET` | `/chat/session/{id}/state` | Get session state + messages |
| `GET` | `/chat/session/{id}/history` | Get session message history |
| `POST` | `/chat/session/{id}/send` | Send message to session |
| `POST` | `/chat/session/{id}/close` | Close session |
| `POST` | `/chat/session/{id}/stop` | Stop generation |
| `POST` | `/chat/session/{id}/retry` | Retry last response |
| `POST` | `/chat/session/{id}/edit` | Edit a message |
| `DELETE` | `/chat/session/{id}` | Delete session |

### Soul

| Method | Path | Description |
|---|---|---|
| `GET` | `/soul/info` | Get soul name and last changed timestamp |
| `GET` | `/soul/raw` | Get raw soul content |
| `POST` | `/soul/update` | Update soul content |
| `GET` | `/soul/system-prompt` | Get compiled system prompt from soul |

### Notifications

| Method | Path | Description |
|---|---|---|
| `GET` | `/notifications` | List notifications (`?active_only`, `?severity`, `?limit`, `?offset`) |
| `GET` | `/notifications/unread-count` | Unread notification count |
| `POST` | `/notifications/{id}/dismiss` | Dismiss notification |
| `POST` | `/notifications/{id}/ack` | Acknowledge notification |
| `POST` | `/notifications/dismiss-all` | Dismiss all notifications |
| `POST` | `/notifications/test` | Push test notification (dev-only, no token configured) |

### Agents

| Method | Path | Description |
|---|---|---|
| `GET` | `/agents` | List all agents |
| `GET` | `/agent/{agent_id}` | Get agent details |
| `POST` | `/agent/{agent_id}/status` | Set agent status |
| `POST` | `/agent/{agent_id}/reload` | Reload agent from config |
| `GET` | `/agents/idle-availability` | Per-agent work/study/fun availability |

### Explore

| Method | Path | Description |
|---|---|---|
| `POST` | `/explore/start` | Start info-hub exploration pipeline |

### Idle-run

| Method | Path | Description |
|---|---|---|
| `POST` | `/idle-run` | Execute an idle-run skill (requires `tag` field, optional `agent_key`, `background`, `project_key`, `work_id`) |

### Capabilities

| Method | Path | Description |
|---|---|---|
| `GET` | `/capabilities` | List capability entries |

### Tool Auth

| Method | Path | Description |
|---|---|---|
| `POST` | `/tool-auth/respond` | Approve/deny tool auth request |
| `POST` | `/tools/{name}/execute` | Execute a tool by name |

### Cron

| Method | Path | Description |
|---|---|---|
| `POST` | `/cron/add` | Add cron job |
| `POST` | `/cron/{id}/update` | Update cron job |
| `POST` | `/cron/{id}/remove` | Remove cron job |

### Observability

| Method | Path | Description |
|---|---|---|
| `GET` | `/audit-log` | Audit log (cursor pagination + filters) |
| `GET` | `/debug/metrics` | Detailed internal metrics (queue depths, plugin health, etc.) |

## Headers

| Header | Used For |
|---|---|
| `Authorization: Bearer <token>` | API authentication (primary) |
| `x-aman-token: <token>` | API authentication (alternative) |
| `x-aman-confirm: yes` | Confirm destructive actions |
| `x-aman-operator: <name>` | Operator name (audit logging) |

Destructive actions requiring `x-aman-confirm: yes`: agent shutdown, plugin disable, skill rollback, workflow instance retry, workflow instance cancel, DLQ retry, config set.

## Response Format

Success: `200 OK` with JSON body (or `text/plain` for `/metrics`).

Errors:

- `400 Bad Request` — Invalid input (e.g. missing field, message blocked by content policy)
- `401 Unauthorized` — Missing/invalid token
- `403 Forbidden` — Soul boundary blocked, or debug endpoint not enabled
- `404 Not Found` — Resource not found
- `409 Conflict` — Confirmation required, version conflict, or operation in progress
- `422 Unprocessable Entity` — Unrecoverable error
- `500 Internal Server Error` — Unexpected server error
- `503 Service Unavailable` — Not ready / draining

## Plugin Routes

Plugins can contribute additional routes under `/api/v1`. These are registered dynamically during plugin loading and are not listed in this reference.
