# Events System

## EventType Enum

Defined in `crates/core/src/event.rs:9-26`:

```rust
pub enum EventType {
    // Standard system events
    FileCreated,
    FileChanged,
    FileDeleted,
    CronTick,
    TimerTick,
    Heartbeat,
    MessageReceived,
    WebhookReceived,
    SystemSignal,
    WorkflowStateChanged,
    // Plugin/Config events
    SkillLoaded,
    SkillReloaded,
    ConfigChanged,
    SecretRotated,
    // Security events
    InjectionDetected,
    // Dynamic events
    Custom(String),
}
```

## Event Struct

Defined in `crates/core/src/event.rs:146-158`:

| Field | Type | Description |
|-------|------|-------------|
| `id` | `Uuid` | UUID v7, unique event identifier |
| `source` | `SourceId` | Origin identifier (e.g. `"timer:heartbeat"`, `"chat-platform:tauri-desktop"`) |
| `event_type` | `EventType` | The event type |
| `timestamp` | `Timestamp` | Millisecond-precision timestamp |
| `priority` | `Priority` | Queue priority (normal, high, critical) |
| `delivery` | `DeliveryGuarantee` | AtMostOnce or AtLeastOnce |
| `dedup_key` | `Option<DedupKey>` | Deduplication key for AtLeastOnce events |
| `payload` | `Value` (serde_json) | Event data |
| `metadata` | `EventMetadata` | trace_id, parent_event_id, retry_count, max_retries, ttl_ms, lifespan_ms, created_at |

---

## Event Flow Architecture

- **Source components** (timer, cron, file_watch, webhook, socket, signal, chat-platform) produce `Event` objects.
- Sources are registered with the `SourceRegistry`, which calls `poll()` to collect events and calls `bus.publish(event)` to push them onto the `EventBus`.
- The `EventBus` (InMemoryBus or PersistentBus) routes events to matching subscribers based on `SubscriptionFilter`.
- Core subscribers:
  - **StoreAllEventsHandler** — subscribes to ALL events, records every event to the `EventStore` (in-memory ring buffer, indexed by trace_id)
  - **SkillEventDispatcher** — subscribes to ALL events, routes to matching skills; publishes `message:dispatch`/`message:completed` around each dispatch cycle
  - **LLMPlugin** — subscribes to `MessageReceived` events for chat processing; publishes `llm:call_started`/`llm:call_ended` around each LLM provider call
- Custom lifecycle events are published directly by runtime components:
  - **Gateway daemon** (`main.rs`): `gateway:starting`/`gateway:ready`/`gateway:stopping` at startup/shutdown boundaries
  - **HTTP handlers** (`http.rs`): `session:started`/`session:closed` on session create/close
  - **PipelineEngine** (via `ToolEventSink`): `tool:invoke`/`tool:completed`/`tool:failed` around tool execution
- The `EventStore` supports trace chain tracking via `trace_id` and `trace_prev` linking.
- Events that fail delivery go to the **Dead Letter Queue (DLQ)** for manual retry/discard.

---

## Standard Event Types

### File System Events

| Type | Source | Payload | Producer File:Line |
|------|--------|---------|-------------------|
| `FileCreated` | `file_watch:{path}` | `{"path":"...", "file_type":"file\|dir"}` | `crates/source/src/file_watch.rs:81,315` |
| `FileChanged` | `file_watch:{path}` | `{"path":"...", "file_type":"file\|dir"}` | `crates/source/src/file_watch.rs:82,324` |
| `FileDeleted` | `file_watch:{path}` | `{"path":"..."}` | `crates/source/src/file_watch.rs:83,318` |

Produced by `source::file_watch::FileWatchSource`. Uses `notify` crate to watch filesystem directories. Each file event carries the affected path and file type.

### Timer Events

| Type | Source | Payload | Producer File:Line |
|------|--------|---------|-------------------|
| `TimerTick` | `timer:{name}` | depends on timer config | `crates/source/src/timer.rs:113,122` |
| `Heartbeat` | `timer:{name}` | `{"heartbeat":true}` | `crates/source/src/timer.rs:113` |

Produced by `source::timer::TimerSource`. Configurable interval. When `interval_ms >= 60000`, produces `Heartbeat`; otherwise `TimerTick`.

### Cron Events

| Type | Source | Payload | Producer File:Line |
|------|--------|---------|-------------------|
| `CronTick` | `cron:{id}` | depends on cron config | `crates/source/src/cron.rs:119` |

Produced by `source::cron::CronSource`. Uses cron expressions (6-field: sec min hour dom mon dow). Managed through the runtime's `CronManager`.

### Message Events

| Type | Source | Payload | Producer File:Line |
|------|--------|---------|-------------------|
| `MessageReceived` | `chat-platform:tauri-desktop` | `{"session_id":"...", "text":"...", "channel":"tauri_desktop", "message_id":"...", "client_timestamp":...}` | `crates/plugins/chat-source/src/lib.rs:147` |
| `MessageReceived` | `socket:{name}` | depends on socket protocol | `crates/source/src/socket.rs:116,147,184` |

Produced by the chat-platform source (from Tauri IPC) or socket source (TCP/UDP connections). The chat-platform source validates messages for length (max 4096 chars) and empty content.

### Webhook Events

| Type | Source | Payload | Producer File:Line |
|------|--------|---------|-------------------|
| `WebhookReceived` | `webhook:{name}` | from HTTP request body | `crates/source/src/webhook.rs:35` |

Produced by `source::webhook::WebhookSource`. Receives HTTP POST requests and converts the body (JSON) into the event payload.

### Signal Events

| Type | Source | Payload | Producer File:Line |
|------|--------|---------|-------------------|
| `SystemSignal` | `signal:{name}` | `{"signal":"SIGINT\|SIGTERM\|SIGHUP\|SIGUSR1\|SIGUSR2"}` | `crates/source/src/signal.rs:93,100` |

Produced by `source::signal::SignalSource`. Listens for OS signals. Each signal produces one event. SIGUSR1/SIGUSR2 also produce a second event with the signal name.

### Workflow Events

| Type | Source | Payload | Producer File:Line |
|------|--------|---------|-------------------|
| `WorkflowStateChanged` | `workflow:engine` | `{"instance_id":"...", "workflow_name":"...", "from_state":"...", "to_state":"...", "reason":"...", "is_final":bool}` | `crates/workflow/src/lib.rs:1114` |

Produced by the `WorkflowEngine` on every state transition. Records the workflow instance, old and new states, and the transition reason (Event, Timeout, ActionFailed, GuardRejected, RetryExceeded).

### Skill Events

| Type | Source | Payload | Producer File:Line |
|------|--------|---------|-------------------|
| `SkillReloaded` | `skill:hot_reload` | `{"inserted":[...], "updated_same_version":[...], "updated_new_version":[...], "removed":[...]}` | `crates/gateway/src/runtime/agent_runtime.rs:982` |

Auto-published by the runtime's skill hot-reload watcher when skill files change on disk. Contains lists of skills that were inserted, updated, or removed.

### Config Events

| Type | Source | Payload | Producer File:Line |
|------|--------|---------|-------------------|
| `ConfigChanged` | `config` | `{"changed_fields":["path.to.field",...], "meta":{"loaded_at_ms":..., "source_chain":[...]}}` | `crates/config/src/lib.rs:718` |

Produced by the config loader when config is modified. Lists the exact fields that changed and the config source chain.

### Reserved But Unused Event Types

The following EventType variants are defined in the enum but have no production publisher:
- `SkillLoaded` — defined, not published (skill loading uses `SkillReloaded` instead)
- `SecretRotated` — defined, not yet published (reserved for future secret rotation)
- `InjectionDetected` — defined, not yet published (reserved for future prompt injection detection)

---

## Custom Event Types

### Chat Session Control Events

| Literal Value | Producer | Purpose | Payload | File:Line |
|---|---|---|---|---|
| `SESSION_CLOSE_CMD` | HTTP handler `chat_session_close` | Close a chat session | `{"session_id":"...", "operator":"...", "reason":...}` | `crates/gateway/src/runtime/http.rs:1965` |
| `STOP_GENERATION` | HTTP handler `chat_session_stop` | Stop LLM generation | `{"session_id":"...", "operator":"..."}` | `crates/gateway/src/runtime/http.rs:2013` |
| `RETRY_CMD` | HTTP handler `chat_session_retry` | Retry last message | `{"session_id":"...", "operator":"..."}` | `crates/gateway/src/runtime/http.rs:2033` |
| `MESSAGE_EDITED` | HTTP handler `chat_session_edit` | Message edited | `{"session_id":"...", "message_event_id":"...", "new_text":"...", "operator":"..."}` | `crates/gateway/src/runtime/http.rs:2134` |

These control events drive the chat-session workflow state machine. They are published via `workflow_engine.handle_event()` (for close/retry) or `runtime.publish_event()` (for stop/edit).

### Session & Gateway Lifecycle Events

| Literal Value | Producer | Purpose | Payload | File:Line |
|---|---|---|---|---|
| `session:started` | `http.rs::chat_session_create()` | Chat session created | `{"session_id":"...","session_type":"...","operator":"..."}` | `crates/gateway/src/runtime/http.rs:1665` |
| `session:closed` | `http.rs::chat_session_close()` | Chat session closed | `{"session_id":"...","operator":"..."}` | `crates/gateway/src/runtime/http.rs:2003` |
| `gateway:starting` | `main.rs` | Gateway daemon starting | `{"bind":"..."}` | `crates/gateway/src/main.rs:119-123` |
| `gateway:ready` | `main.rs` | Gateway ready to serve | `{"bind":"...","addr":"..."}` | `crates/gateway/src/main.rs:134-138` |
| `gateway:stopping` | `main.rs` | Gateway shutting down | `{}` | `crates/gateway/src/main.rs:162-166` |

Published by the gateway daemon at lifecycle boundaries: before starting the runtime, after start succeeds, and before graceful shutdown. `session:started`/`session:closed` are published from HTTP handler endpoints and carry the `session_id` for trace chain correlation.

> `session:timeout` is reserved in the milestone plan but deferred — production currently lacks a timeout polling loop for workflow instances.

### LLM & Agent Events (Published by AgentHarness)

| Literal Value | Purpose | Payload | Producer |
|---|---|---|---|
| `llm:call_started` | LLM provider call initiated | `{"session_id":"...","model":"...","input_tokens_estimate":N,"original_message_id":"...","soul_name":"..."}` | `agent_harness.rs` |
| `llm:call_ended` | LLM provider call completed successfully | `{"session_id":"...","model":"...","input_tokens_estimate":N,"output_tokens_estimate":N,"latency_ms":N,"original_message_id":"...","soul_name":"..."}` | `agent_harness.rs` |
| `llm_error` | LLM call error | `{"session_id":"...", "error":"..."}` | `agent_harness.rs` |
| `agent:reply_ready` | Agent response ready (non-streaming fallback) | `{"session_id":"...", "reply":"..."}` | `agent_harness.rs` |
| `agent:reply_stream_start` | Streaming response started | `{"session_id":"..."}` | `agent_harness.rs` |
| `agent:reply_chunk` | Streaming response delta | `{"session_id":"...", "extra":{"delta":"..."}}` | `agent_harness.rs` |
| `agent:reply_stream_done` | Streaming response complete | `{"session_id":"..."}` | `agent_harness.rs` |
| `agent:reply_stream_error` | Streaming response error | `{"session_id":"...", "error":"..."}` | `agent_harness.rs` |
| `tool:dispatched` | Tool call dispatched to agent | `{"session_id":"...","tool_call_id":"...","tool_name":"...","args":{...}}` | `agent_harness.rs` |
| `tool:completed` | Tool call succeeded | `{"session_id":"...","tool_call_id":"...","output":"..."}` | `agent_harness.rs` |
| `tool:failed` | Tool call failed | `{"session_id":"...","tool_call_id":"...","output":"..."}` | `agent_harness.rs` |

Published by AgentHarness to track the ReAct loop lifecycle. `llm:call_started`/`llm:call_ended` bracket each LLM provider invocation. Streaming events (`agent:reply_stream_*`) deliver real-time response content. Tool events (`tool:dispatched/completed/failed`) track tool execution within the loop.

### Message Dispatch Events

| Literal Value | Producer | Purpose | Payload | File:Line |
|---|---|---|---|---|
| `message:dispatch` | `SkillEventDispatcher` | Event routed to skill(s) for processing | `{"trace_id":"...","event_id":"...","event_type":"...","source":"..."}` | `crates/gateway/src/runtime/agent_runtime.rs:422` |
| `message:completed` | `SkillEventDispatcher` | Skill(s) finished processing | `{"trace_id":"...","executed":[...],"failed":[...]}` | `crates/gateway/src/runtime/agent_runtime.rs:433` |

Published by the `SkillEventDispatcher` — a catch-all EventBus subscriber registered in the runtime builder. On every incoming event, `message:dispatch` fires before routing to matching skills and `message:completed` fires after all matching skills have executed (success or failure).

### Tool Events

| Literal Value | Producer | Purpose | Payload | File:Line |
|---|---|---|---|---|
| `tool:invoke` | `PipelineEngine` | Tool execution started | `{"tool_name":"...","pipeline_id":"...","instance_id":"..."}` | `crates/pipeline/src/lib.rs:591` |
| `tool:completed` | `PipelineEngine` | Tool execution succeeded | `{"tool_name":"...","pipeline_id":"...","instance_id":"...","duration_ms":N}` | `crates/pipeline/src/lib.rs:599` |
| `tool:failed` | `PipelineEngine` | Tool execution failed | `{"tool_name":"...","pipeline_id":"...","instance_id":"...","error":"..."}` | `crates/pipeline/src/lib.rs:605,612` |

Defined via the `ToolEventSink` trait and wired into `PipelineEngine::execute_tool_with_retry()`. The `BusToolEventSink` implementation in the gateway crate (`crates/gateway/src/runtime/agent_runtime.rs:518-550`) converts these sink callbacks into EventBus publishes.

> **Architecture note**: `PipelineEngine` is currently not in the production chat flow (the LLM plugin calls the provider directly via `rig::agent::prompt()`). Tool events fire from the `PipelineEngine` path used in tests and the dispatcher crate. Production tool events will be added when `PipelineEngine` or `ToolRunner` is wired into the production path.

### Capability Events

| Literal Value | Purpose | Payload | File:Line |
|---|---|---|---|
| `capability_available` | New capability registered | `{"capability":"..."}` | `crates/gateway/src/runtime/agent_runtime.rs:833` |
| `capability_removed` | Capability unregistered | `{"capability":"...","reason":"..."}` | `crates/gateway/src/runtime/agent_runtime.rs:833` |
| `capability_registry_updated` | Full capability registry refresh | `{"available":[...], "added":[...], "removed":[...]}` | `crates/gateway/src/runtime/agent_runtime.rs:807` |

Published by the capability registry during startup and plugin hot-load/unload. The `registry_updated` event is a summary and does not enter the WAL.

### Soul Events

| Literal Value | Purpose | Payload | File:Line |
|---|---|---|---|
| `soul_changed` | SOUL file modified on disk | `{"name":"...", "boundaries":[...], "preferences":{...}}` | `crates/soul/src/lib.rs:384` |

Published by the SOUL hot-reload manager when the SOUL.md file changes. Contains the new soul name, boundaries, and preferences. This event is also passed through to the EventStore for audit purposes.

### Workflow Control Events

| Literal Value | Purpose | Payload | Producer File:Line |
|---|---|---|---|
| `retry` | Manual retry of errored workflow | `{"operator":"..."}` | `crates/gateway/src/runtime/http.rs:636` |
| `cancel` | Cancel pending retry | `{"operator":"..."}` | `crates/gateway/src/runtime/http.rs:694` |
| `retry` | Auto-retry by workflow engine | `{"auto_retry":true, "attempt":N}` | `crates/workflow/src/lib.rs:1027` |

Published by HTTP handlers and the workflow engine's auto-retry mechanism.

---

## HTTP API Event Endpoints

All endpoints in `crates/gateway/src/runtime/http.rs`:

| Method | Route | Purpose | Response |
|--------|-------|---------|----------|
| `POST` | `/inject-event` | Inject a custom event (requires risky_capabilities_enabled) | `{"id":"..."}` |
| `GET` | `/events/dump/{id}` | Retrieve a single event by UUID | Full `Event` JSON |
| `GET` | `/events/recent` | Recent events (max 1000, default 50) | `{"events":[...]}` |
| `GET` | `/events/trace/{trace_id}` | All events in a trace chain | `{"trace_id":"...", "events":[...], "cycle_detected":bool}` |
| `POST` | `/event-source/{id}/pause` | Pause an event source | — |
| `POST` | `/event-source/{id}/resume` | Resume an event source | — |
| `PUT` | `/event-source/{id}/config` | Reconfigure an event source | — |

---

## Tauri Frontend Events

Bridged from Rust backend to Svelte frontend:

| Event Name | Direction | Polling | Listeners | File:Line |
|---|---|---|---|---|
| `menu:reload_skills` | Menu action → Frontend | On demand | Handled via Tauri menu event | `crates/tauri/src/lib.rs:38` |
| `metrics:updated` | Background task → Frontend | Every 2s | `Dashboard.svelte`, `DebugPanel.svelte` | `crates/tauri/src/lib.rs:180` |
| `event:processed` | Background task → Frontend | Every 1s | `Chat.svelte:936`, `DebugPanel.svelte:102` | `crates/tauri/src/lib.rs:200` |

The background tasks poll the gateway HTTP API:
- `metrics:updated` — polls `GET /debug/metrics` every 2 seconds
- `event:processed` — polls `GET /events/recent` every 1 second, de-duplicates by event ID

---

## Chat-Session Workflow Transitions

The chat-session workflow (`crates/gateway/src/runtime/agent_runtime.rs:202-352`) is driven by events. The mapping from event types to workflow transitions:

| Current State | Event | Next State |
|---|---|---|
| `ACTIVE` | `MESSAGE_RECEIVED` | `PROCESSING` |
| `ACTIVE` | `SESSION_TIMEOUT` (300s) | `TIMEOUT` |
| `PROCESSING` | `LLM_REPLY_READY` | `IDLE` |
| `PROCESSING` | `LLM_STREAM_DONE` | `IDLE` |
| `PROCESSING` | `LLM_ERROR` | `ERROR` |
| `PROCESSING` | `STREAM_TIMEOUT` (120s) | `TIMEOUT` |
| `PROCESSING` | `SESSION_CLOSE_CMD` | `CLOSED` |
| `IDLE` | `MESSAGE_RECEIVED` | `PROCESSING` |
| `IDLE` | `SESSION_TIMEOUT` (600s) | `TIMEOUT` |
| `IDLE` | `SESSION_END` | `CLOSED` |
| `ERROR` | `RETRY_CMD` | `RETRYING` |
| `ERROR` | `SESSION_END` | `IDLE` |
| `ERROR` | `ABANDON_TIMEOUT` (120s) | `CLOSED` |
| `RETRYING` | `RETRY_STARTED` | `PROCESSING` |
| `RETRYING` | `RETRY_FAILED` | `ERROR` |
| `TIMEOUT` | `SESSION_END` | `CLOSED` |
| `TIMEOUT` | `MESSAGE_RECEIVED` | `IDLE` |

---

## Event Bus Internals

| Component | File | Purpose |
|---|---|---|
| `EventBus` trait | `crates/event-bus/src/lib.rs:197` | `publish()`, `subscribe()`, `unsubscribe()`, `metrics()` |
| `InMemoryBus` | `crates/event-bus/src/lib.rs` | In-memory implementation with priority queues |
| `InMemoryBusConfig` | `crates/event-bus/src/lib.rs` | Backpressure thresholds (L1:80%, L2:90%, L3:95%, L4A:98%) |
| `SubscriptionFilter` | `crates/event-bus/src/lib.rs:52` | Filter by `event_types`, `sources`, `min_priority` |
| `OverflowHandler` | `crates/event-bus/src/overflow.rs` | Disk overflow when queue is full |
| `DedupHandler` | `crates/event-bus/src/dedup.rs` | Deduplication by `dedup_key` |
| `RetryQueue` | `crates/event-bus/src/retry_queue.rs` | Retry for AtLeastOnce delivery |
| `PersistentBus` | `crates/persistence/src/persistent_bus.rs` | WAL-backed persistent event bus |

---

## Event Store

Defined in `crates/gateway/src/runtime/event_store.rs`:

- **Capacity**: Global cap (configurable) + per-trace cap (max events per trace_id)
- **`record(event)`**: Stores event by ID, indexes by trace_id, builds trace_children graph from `payload.trace_prev`
- **`get(id)`**: Retrieves single event by UUID
- **`trace(trace_id)`**: Returns all events sharing the trace_id
- **`trace_chain(trace_id)`**: BFS traversal of trace ancestors + descendants
- **`recent(count)`**: Most recent N events by insertion order

The `StoreAllEventsHandler` (`crates/gateway/src/runtime/agent_runtime.rs:903-907`) is a catch-all subscriber that records every published event into the EventStore.

---

## Dead Letter Queue (DLQ)

`crates/persistence/src/dlq.rs` — `InMemoryDeadLetterQueue`:

| Method | Purpose |
|---|---|
| `enqueue(event, reason, ttl_days)` | Move failed event to DLQ |
| `retry(id, operator, reason)` | Re-publish event from DLQ |
| `discard(id)` | Remove event from DLQ permanently |
| `list(filter)` | Query DLQ entries with optional filters |
| `depth()` | Current DLQ size |

Events enter DLQ primarily through the pipeline engine when a step fails and compensation cannot proceed. DLQ entries are re-publishable through the HTTP API (`POST /dlq/{id}/retry`).

---

## Audit Log

`crates/gateway/src/runtime/audit.rs` — `AuditLogger`:

- Ring buffer of `AuditRecord` with fields: `operator`, `action`, `target`, `outcome`, `detail`, `timestamp`
- Default capacity: 2000 records
- Recorded at ~100+ call sites across HTTP handlers and the runtime
- Queryable via `GET /audit-log` with filters: `action`, `operator`, `since_ms`, `until_ms`, `limit`, `offset`
