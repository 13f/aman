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

## Dual-Layer Event Bus Architecture

aman uses a **dual-layer event bus** design: one **Global Bus** for infrastructure events and cross-agent communication, plus a **Per-Agent Local Bus** for each agent's internal high-throughput events.

```
┌─────────────────────────────────────────────────────────┐
│                    Global Event Bus                       │
│  (infrastructure events: gateway:*, source:*,            │
│   agent:lifecycle, agent:message, session:*)              │
│  low throughput, high reliability, globally visible       │
└────────────┬──────────────────────────────┬──────────────┘
             │                              │
    ┌────────▼────────┐            ┌───────▼────────┐
    │ Agent "cortana" │            │ Agent "coder"   │
    │   Local Bus     │            │   Local Bus     │
    │                  │            │                 │
    │ llm:call_started │            │ llm:call_started│
    │ llm:call_ended   │            │ llm:call_ended  │
    │ tool:dispatched  │            │ tool:dispatched │
    │ tool:completed   │            │ tool:completed  │
    │ agent:reply_*    │            │ agent:reply_*   │
    │ (high throughput, isolated)   │ (high throughput, isolated)│
    └──────────────────┘            └─────────────────┘
```

### Global Bus

Responsible for low-throughput, globally-visible infrastructure events:

| Category | Event Types |
|----------|-------------|
| Gateway lifecycle | `gateway:starting`, `gateway:ready`, `gateway:stopping` |
| File system | `FileCreated`, `FileChanged`, `FileDeleted` |
| Timer/Cron | `TimerTick`, `CronTick`, `Heartbeat` |
| External input | `WebhookReceived`, `SystemSignal` |
| Agent lifecycle | `agent:registered`, `agent:removed`, `agent:status_changed`, `agent:busy`, `agent:idle` |
| Agent response | `agent:reply_ready`, `agent:reply_interrupted` (final response to frontend) |
| Cross-agent | `agent:message` (routed by `to_agent` in payload) |
| Config/Skill | `ConfigChanged`, `SkillReloaded`, `soul_changed` |
| Session | `session:started`, `session:closed` |
| Message dispatch | `message:dispatch`, `message:completed` |
| Capability | `capability_available`, `capability_removed`, `capability_registry_updated` |

### Local Bus (one per agent)

Responsible for high-throughput, agent-internal events:

| Category | Event Types |
|----------|-------------|
| LLM calls | `llm:call_started`, `llm:call_ended`, `llm_error` |
| Streaming | `agent:reply_stream_start`, `agent:reply_chunk`, `agent:reply_stream_done`, `agent:reply_stream_error` |
| Tool execution | `tool:dispatched`, `tool:completed`, `tool:failed`, `tool:security_denied` |
| Token tracking | `agent:token_used` |
| ReAct internal | `agent:got_tool_calls`, `agent:tool_results_fed_back`, `agent:history_compressed`, `agent:config_warning` |
| Interrupt (internal) | `agent:reply_interrupted` (within `react_loop`) |

### Why Two Layers?

1. **Isolation** — Agent A's `llm:call_started` and `tool:dispatched` events are not visible to Agent B's subscribers
2. **Independent backpressure** — Agent A's event flood only affects Agent A's Local Bus queue, not Agent B or the Global Bus
3. **Cross-process ready** — Per-agent Local Bus is a prerequisite for running agents in separate processes/containers

### Routing Logic

When an agent-internal event is published, the publisher (ToolExecutor, LlmReActEngine, AgentHarness) resolves the target bus:

```rust
// Publish to local bus if available, fall back to global bus
match agent_registry.get_local_bus(agent_id).await {
    Some(local_bus) => local_bus.publish(event).await,
    None => global_bus.publish(event).await,  // single-agent / migration path
}
```

In single-agent configurations where no per-agent local bus is configured, all events flow through the Global Bus — preserving backwards compatibility.

### Configuration

Each agent can override its Local Bus config in `aman.yaml`:

```yaml
agents:
  cortana:
    display_name: Cortana
    provider: openai
    model: gpt-5.4-flash
    event_bus:
      max_queue_size: 2000   # override default (1000)
  coder:
    display_name: Coder
    provider: deepseek
    model: deepseek-v4-pro
    # not configured → uses default (max_queue_size: 1000)
```

### Implementation

| Component | File | Role |
|-----------|------|------|
| `AgentRegistry::set_local_bus()` | `crates/gateway/src/runtime/agent_registry.rs:251` | Stores per-agent Local Bus |
| `AgentRegistry::get_local_bus()` | `crates/gateway/src/runtime/agent_registry.rs:257` | Lookup Local Bus by agent_id |
| `AgentRegistry::load_from_config()` | `crates/gateway/src/runtime/agent_registry.rs:95-110` | Creates Local Bus for each agent at startup |
| `AgentEntryConfig::event_bus` | `crates/config/src/lib.rs:422` | Per-agent `PartialEventBusConfig` |
| `ToolExecutor::publish_to_agent_bus()` | `crates/gateway/src/runtime/agent_harness.rs:114` | Tool events → Local Bus |
| `LlmReActEngine::publish_to_agent_bus()` | `crates/gateway/src/runtime/agent_harness.rs:298` | LLM events → Local Bus |
| `AgentHarness::publish_to_agent_bus()` | `crates/gateway/src/runtime/agent_harness.rs:487` | Stream/harness events → Local Bus |

---

## Event Flow Architecture

- **Source components** (timer, cron, file_watch, webhook, socket, signal, chat-platform) produce `Event` objects.
- Sources are registered with the `SourceRegistry`, which calls `poll()` to collect events and calls `bus.publish(event)` to push them onto the **Global Event Bus**.
- The **Global Bus** (InMemoryBus or PersistentBus) routes events to matching subscribers based on `SubscriptionFilter`.
- **Agent-internal events** (LLM calls, tool execution, streaming) are published to the **agent's Local Bus**, which has independent queue depth and backpressure.
  - When no Local Bus is configured (single-agent mode), agent-internal events fall back to the Global Bus.
- Core subscribers (on Global Bus):
  - **StoreAllEventsHandler** — subscribes to ALL events on Global Bus, records every event to the `EventStore` (in-memory ring buffer, indexed by trace_id)
  - **SkillEventDispatcher** — subscribes to ALL events on Global Bus, routes to matching skills; publishes `message:dispatch`/`message:completed` around each dispatch cycle
  - **MessageReceived handler** — subscribes to `MessageReceived` events on Global Bus, spawns ReAct processing via AgentHarness
- Custom lifecycle events are published directly by runtime components:
  - **Gateway daemon** (`main.rs`): `gateway:starting`/`gateway:ready`/`gateway:stopping` at startup/shutdown boundaries → Global Bus
  - **HTTP handlers** (`http.rs`): `session:started`/`session:closed` on session create/close → Global Bus
  - **PipelineEngine** (via `ToolEventSink`): `tool:invoke`/`tool:completed`/`tool:failed` around tool execution → Global Bus
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

| Literal Value | Bus | Purpose | Payload | Producer |
|---|---|---|---|---|
| `llm:call_started` | **Local** | LLM provider call initiated | `{"agent_id":"...","session_id":"...","turn":N}` | `LlmReActEngine` |
| `llm:call_ended` | **Local** | LLM provider call completed | `{"agent_id":"...","session_id":"...","turn":N,"success":bool}` | `LlmReActEngine` |
| `llm_error` | **Local** | LLM call error | `{"agent_id":"...","session_id":"...","turn":N,"error":"..."}` | `LlmReActEngine` / `AgentHarness` |
| `agent:token_used` | **Local** | Token usage estimate | `{"agent_id":"...","session_id":"...","turn":N,"tokens":N}` | `LlmReActEngine` |
| `agent:reply_ready` | **Global** | Agent response ready | `{"agent_id":"...","session_id":"...","reply":"...","turns_processed":N}` | `AgentHarness` |
| `agent:reply_interrupted` | **Global** | User stopped generation | `{"agent_id":"...","session_id":"..."}` | `AgentHarness` |
| `agent:reply_stream_start` | **Local** | Streaming response started | `{"agent_id":"...","session_id":"...","turn":N,"extra":{}}` | `AgentHarness` (stream forwarder) |
| `agent:reply_chunk` | **Local** | Streaming response delta | `{"agent_id":"...","session_id":"...","turn":N,"extra":{"delta":"..."}}` | `AgentHarness` (stream forwarder) |
| `agent:reply_stream_done` | **Local** | Streaming response complete | `{"agent_id":"...","session_id":"...","turn":N,"extra":{"finish_reason":"..."}}` | `AgentHarness` (stream forwarder) |
| `agent:reply_stream_error` | **Local** | Streaming error | `{"agent_id":"...","session_id":"...","error":"..."}` | `AgentHarness` (stream forwarder) |
| `tool:dispatched` | **Local** | Tool call dispatched | `{"agent_id":"...","session_id":"...","tool_call_id":"...","tool_name":"...","args":{...}}` | `ToolExecutor` |
| `tool:completed` | **Local** | Tool call succeeded | `{"agent_id":"...","session_id":"...","tool_call_id":"...","tool_name":"...","success":true,"duration_ms":N,"output":"..."}` | `ToolExecutor` |
| `tool:failed` | **Local** | Tool call failed | `{"agent_id":"...","session_id":"...","tool_call_id":"...","tool_name":"...","success":false,"duration_ms":N,"output":"..."}` | `ToolExecutor` |
| `tool:security_denied` | **Local** | Tool blocked by security | `{"agent_id":"...","session_id":"...","tool_call_id":"...","tool_name":"...","block_type":"hardline\|path_denied","reason":"..."}` | `ToolExecutor` |
| `agent:got_tool_calls` | **Local** | LLM requested tool calls | `{"agent_id":"...","session_id":"...","turn":N,"tool_calls":[...]}` | `AgentHarness` |
| `agent:tool_results_fed_back` | **Local** | Tool results fed to LLM | `{"agent_id":"...","session_id":"...","turn":N,"result_count":N}` | `AgentHarness` |
| `agent:history_compressed` | **Local** | Context window trimmed | `{"agent_id":"...","session_id":"...","turn":N,"messages_removed":N,"tokens_saved":N,"strategy":"truncate\|summarize"}` | `AgentHarness` |
| `agent:config_warning` | **Global** | Budget config not set | `{"agent_id":"...","session_id":"...","config_key":"...","message":"..."}` | `AgentHarness` |

Published by AgentHarness to track the ReAct loop lifecycle. `llm:call_started`/`llm:call_ended` bracket each LLM provider invocation. Streaming events (`agent:reply_stream_*`) deliver real-time response content. Tool events (`tool:dispatched/completed/failed`) track tool execution within the loop.

**Bus routing**: Agent-internal events (LLM calls, tool execution, streaming, token tracking, ReAct internals) are published to the **Local Bus** so that one agent's internal events don't pollute another agent's event stream or trigger global backpressure. Agent lifecycle events (`agent:reply_ready`, `agent:reply_interrupted`, `agent:config_warning`) remain on the **Global Bus** for frontend visibility. When no Local Bus is configured (single-agent mode), all events fall back to the Global Bus.

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

### Bus Components

| Component | File | Purpose |
|---|---|---|
| `EventBus` trait | `crates/event-bus/src/lib.rs:211` | `publish()`, `subscribe()`, `unsubscribe()`, `metrics()`, `try_dequeue()`, `wait_for_event()` |
| `InMemoryBus` | `crates/event-bus/src/lib.rs:394` | In-memory implementation with priority queues, used for both Global and Local buses |
| `InMemoryBusConfig` | `crates/event-bus/src/lib.rs:167` | Backpressure thresholds (L1:80%, L2:90%, L3:95%, L4A:98%, L4B:Critical) |
| `SubscriptionFilter` | `crates/event-bus/src/lib.rs:57` | Filter by `event_types`, `sources`, `priorities`, `payload_match` |
| `OverflowDir` | `crates/event-bus/src/overflow.rs` | Disk overflow when queue is full (Level 4A) |
| `BackpressureController` | `crates/event-bus/src/backpressure.rs` | 5-level backpressure (Normal → L1 → L2 → L3 → L4A → L4B → Critical) |
| `DedupWindow` | `crates/event-bus/src/dedup.rs` | Deduplication by `dedup_key` (30s window default) |
| `RetryQueue` | `crates/event-bus/src/retry_queue.rs` | Retry for AtLeastOnce delivery with exponential backoff |
| `PersistentBus` | `crates/persistence/src/persistent_bus.rs` | WAL-backed persistent event bus (Global Bus only) |

### Dual-Layer Configuration

| Layer | Default `max_queue_size` | Backpressure Scope | Config Source |
|---|---|---|---|
| **Global Bus** | 10,000 | System-wide (all agents + sources share one queue) | `event_bus.max_queue_size` in `aman.yaml` |
| **Local Bus** (per-agent) | 1,000 | Per-agent (agent's own events only) | `agents.<id>.event_bus.max_queue_size` in `aman.yaml` |

Both buses use the same 5-level backpressure mechanism. When an agent's Local Bus queue fills, only that agent's publishers are affected — other agents continue unaffected. The Global Bus backpressure affects all sources and cross-agent communication.

### AgentRegistry Bus Management

`AgentRegistry` (`crates/gateway/src/runtime/agent_registry.rs`) stores per-agent Local Buses in a `RwLock<HashMap<String, Arc<dyn EventBus>>>`:

- `set_local_bus(agent_id, bus)` — called during `load_from_config()` to create each agent's Local Bus
- `get_local_bus(agent_id) -> Option<Arc<dyn EventBus>>` — called by `ToolExecutor`, `LlmReActEngine`, and `AgentHarness` to resolve the correct bus for each agent's internal events
- `clear()` — removes all Local Buses alongside agent instances during shutdown

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
