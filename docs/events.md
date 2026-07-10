# Events System

## EventType Enum

Defined in `kernel/core/src/event.rs:11-37`:

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
    // Idle/Work system events
    Idle,              // produced by IdleDetector
    QueueDrained,      // produced by Dispatcher
    // Multi-agent
    AgentMessage,      // M7 cross-agent messaging
    // Evaluation
    EvaluationCompleted, // produced by Eval system
    // Dynamic events
    Custom(String),
}
```

### Wire Format (`as_str()`)

| Variant | Wire String | Variant | Wire String |
|---------|------------|---------|------------|
| `FileCreated` | `file_created` | `FileChanged` | `file_changed` |
| `FileDeleted` | `file_deleted` | `CronTick` | `cron_tick` |
| `TimerTick` | `timer_tick` | `Heartbeat` | `heartbeat` |
| `MessageReceived` | `message_received` | `WebhookReceived` | `webhook_received` |
| `SystemSignal` | `system_signal` | `WorkflowStateChanged` | `workflow_state_changed` |
| `SkillLoaded` | `skill_loaded` | `SkillReloaded` | `skill_reloaded` |
| `ConfigChanged` | `config_changed` | `SecretRotated` | `secret_rotated` |
| `InjectionDetected` | `injection_detected` | `Idle` | `idle` |
| `QueueDrained` | `system.queue_drained` | `AgentMessage` | `agent:message` |
| `EvaluationCompleted` | `eval:evaluation_completed` | `Custom(s)` | `s` (verbatim) |

Unrecognized strings deserialize as `Custom(value)`. Empty strings are rejected at parse time.

## Event Struct

Defined in `kernel/core/src/event.rs:187-203`:

| Field | Type | Description |
|-------|------|-------------|
| `id` | `Uuid` | UUID v7, unique event identifier |
| `source` | `SourceId` | Origin identifier (e.g. `"timer:heartbeat"`, `"chat-platform:tauri-desktop"`) |
| `event_type` | `EventType` | The event type |
| `timestamp` | `Timestamp` | Millisecond-precision timestamp |
| `priority` | `Priority` | Queue priority (`High`, `Normal`, `Low`) |
| `delivery` | `DeliveryGuarantee` | `AtMostOnce`, `AtLeastOnce`, or `ExactlyOnce` |
| `dedup_key` | `Option<DedupKey>` | Deduplication key for `AtLeastOnce` and `ExactlyOnce` events |
| `payload` | `Value` (serde_json) | Event data |
| `metadata` | `EventMetadata` | `trace_id`, `parent_event_id`, `retry_count`, `max_retries`, `ttl_ms`, `lifespan_ms`, `created_at` |
| `trust_level` | `Option<TrustLevel>` | Security sandbox: restricts sandboxed sources from publishing sensitive event types (see [Trust Level Enforcement](#trust-level-enforcement)) |

### Priority

`High` → `Normal` → `Low`. Under backpressure L1, `AtMostOnce` events are downgraded one level (`High`→`Normal`, `Normal`/`Low`→`Low`).

### Delivery Guarantee

| Value | Dedup | Description |
|-------|-------|-------------|
| `AtMostOnce` | None | Fire-and-forget; no dedup key generated |
| `AtLeastOnce` | Yes | Guaranteed delivery with retry; dedup key from UUID v7 or content hash |
| `ExactlyOnce` | Yes | Strongest guarantee; same dedup strategy as `AtLeastOnce` |

### Dedup Key Generation

- `AtMostOnce` events → no dedup key (`None`)
- UUID v7 events → key is the UUID string itself
- Other events → `source:event_type:blake3(payload)`

---

## Trust Level Enforcement

Introduced in Layer 4 security hardening (commit `0f73170`). The `Event` struct carries an optional `trust_level: Option<TrustLevel>` field, set by `SourceRegistry` when events are published from external sources.

### Trust Levels

| Level | Description | Can Publish Sensitive Events? |
|-------|-------------|------------------------------|
| `Trusted` | Internal system components, no restrictions | ✅ Yes |
| `Untrusted` | User-provided but reviewed; moderate restrictions | ❌ No (default) |
| `Sandboxed` | Isolated plugin/hook; strict resource limits, event publishing restrictions | ❌ No (rejected with `SecurityViolation` error) |

### Sensitive Event Types

`EventType::is_sensitive()` returns `true` for: `ConfigChanged`, `SecretRotated`, `InjectionDetected`.

### Admission Flow (in order)

When `InMemoryBus::publish()` is called, the following checks run in sequence:

1. **Trust enforcement** — events from sandboxed sources targeting sensitive types are rejected with `Error::SecurityViolation`
2. **Rate limiting** — per-source token bucket (`EventRateLimiter`, default disabled); `max_per_second` / `burst`
3. **Backpressure signal refresh** — recalculates queue usage and backpressure level
4. **Priority degradation** — `AtMostOnce` events downgraded at L1
5. **Idle event discard** — idle events silently dropped at any level above Normal
6. **Critical low-priority stop** — `Low` priority events dropped at Critical level
7. **AtMostOnce drop** — `AtMostOnce` events dropped at L2 and above
8. **Overflow to disk** — guaranteed-delivery events overflow to disk at L4A; if overflow dir is ≥80% full → L4B emergency → block
9. **Block** — guaranteed-delivery events blocked at L3+
10. **Queue full** — `BusFull` error when queue is completely full
11. **Dedup** — duplicate events silently dropped

### Configuration

```yaml
event_bus:
  reject_sandboxed_sensitive_events: true
  # Optional rate limiter (disabled by default)
  rate_limiter:
    max_per_second: 100
    burst: 200
```

Sources can set `InMemoryBusConfig::reject_sandboxed_sensitive_events = false` to disable trust-level enforcement (for testing).

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
| Gateway lifecycle | `gateway:starting`, `gateway:ready` (from `main.rs`); `gateway:stopping` (from `Agenverse::shutdown()`) |
| File system | `FileCreated`, `FileChanged`, `FileDeleted` |
| Timer/Cron | `TimerTick`, `CronTick`, `Heartbeat` |
| External input | `WebhookReceived`, `SystemSignal` |
| Agent lifecycle | `agent:registered`, `agent:removed`, `agent:status_changed`, `agent:busy`, `agent:idle` |
| Agent response | `agent:reply_ready`, `agent:reply_interrupted` (final response to frontend) |
| Cross-agent | `agent:message` (routed by `to_agent` in payload) |
| Config/Skill | `ConfigChanged`, `SkillReloaded`, `soul_changed` |
| Session | `session:started` (from `session/mod.rs`), `session:closed` (from `http.rs`) |
| Message dispatch | `message:dispatch`, `message:completed` |
| Capability | `capability_available`, `capability_removed`, `capability_registry_updated` |
| Cognitive | `cognitive_state_changed` (ConsciousnessTranslator) |
| LLM health | `llm_backend_down`, `llm_backend_recovered`, `llm_backend_connected` |
| Agent lifecycle | `agent:reloaded` (distinct from `agent:registered`) |
| Auto-continue | `agent:auto_continue`, `agent:auto_continue_stopped` |

### Local Bus (one per agent)

Responsible for high-throughput, agent-internal events:

| Category | Event Types |
|----------|-------------|
| LLM calls | `llm:call_started`, `llm:call_ended`, `llm_error` |
| Streaming | `agent:reply_stream_start`, `agent:reply_chunk`, `agent:reply_stream_done`, `agent:reply_stream_error` |
| Tool execution | `tool:dispatched`, `tool:completed`, `tool:failed`, `tool:security_denied` |
| Token tracking | `agent:token_used` |
| ReAct internal | `agent:got_tool_calls`, `agent:tool_results_fed_back`, `agent:history_compressed`, `agent:config_warning` |
| **Work System** | `work.start_check`, `work.claim_task`, `work.claim_response`, `work.execute_step`, `work.step_complete`, `work.step_failed`, `work.review_task`, `work.review_complete`, `work.submit_result`, `work.cycle_done`, `work.delayed_work_tick`, `work.interrupt` |
| **Idle System** | `idle.system` (idle events from AgentIdleManager), `idle.cycle_completed` (exploration/meditation/incubation cycles), `idle.inspiration` (incubation insight), `agent:cold_start_done` (idle manager initialization complete) |

### Why Two Layers?

1. **Isolation** — Agent A's `llm:call_started` and `tool:dispatched` events are not visible to Agent B's subscribers
2. **Independent backpressure** — Agent A's event flood only affects Agent A's Local Bus queue, not Agent B or the Global Bus
3. **Cross-process ready** — Per-agent Local Bus is a prerequisite for running agents in separate processes/containers

### Routing Logic

When an agent-internal event is published, the publisher (ToolExecutor, LlmCognitiveEngine, AgentHarness) resolves the target bus:

```rust
// Publish to local bus if available, fall back to global bus
match agent_registry.get_local_bus(agent_id).await {
    Some(local_bus) => local_bus.publish(event).await,
    None => global_bus.publish(event).await,  // single-agent / migration path
}
```

In single-agent configurations where no per-agent local bus is configured, all events flow through the Global Bus — preserving backwards compatibility.

### Configuration

The top-level Global Bus config uses `EventBusConfig` (`kernel/config/src/lib.rs:97`) with fields: `max_queue_size`, `mode` (`Persistent` default), `persistence`, `stream_forwarder_capacity` (default 8192), plus the full backpressure and dedup settings.

Each agent can override its Local Bus config via `PartialEventBusConfig` (`kernel/config/src/lib.rs:850`) in `aman.yaml`:

```yaml
event_bus:
  max_queue_size: 10000   # Global Bus queue (default)
  mode: Persistent
agents:
  cortana:
    display_name: Cortana
    provider: openai
    model: gpt-5.4-flash
    event_bus:
      max_queue_size: 2000   # override default Local Bus (1000)
  coder:
    display_name: Coder
    provider: deepseek
    model: deepseek-v4-pro
    # not configured → uses default (max_queue_size: 1000)
```

### Implementation

| Component | File | Role |
|-----------|------|------|
| `AgentRegistry::set_local_bus()` | `kernel/gateway/src/runtime/agent_registry.rs:850` | Stores per-agent Local Bus |
| `AgentRegistry::get_local_bus()` | `kernel/gateway/src/runtime/agent_registry.rs:856` | Lookup Local Bus by agent_id |
| `AgentRegistry::load_from_config()` | `kernel/gateway/src/runtime/agent_registry.rs:119` | Creates Local Bus for each agent at startup |
| `AgentEntryConfig::event_bus` | `kernel/config/src/lib.rs:850` | `Option<PartialEventBusConfig>` per-agent override |
| `LlmCognitiveEngine` tool publishers | `cognitive/llm/src/lib.rs` (521+) | Tool events → agent Local Bus |
| `AgentHarness` bus publishers | `kernel/gateway/src/runtime/agent_harness.rs` | Stream/harness events → Local Bus |

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
  - **BusToolEventSink** (in `agent_runtime.rs`): `tool:invoke`/`tool:completed`/`tool:failed` around Pipeline execution (currently not wired in production); production tool events published by `LlmCognitiveEngine` to Local Bus
- The `EventStore` supports trace chain tracking via `trace_id` and `trace_prev` linking.
- Events that fail delivery go to the **Dead Letter Queue (DLQ)** for manual retry/discard.

---

## Standard Event Types

### File System Events

| Type | Source | Payload | Producer File:Line |
|------|--------|---------|-------------------|
| `FileCreated` | `file_watch:{path}` | `{"path":"...", "file_type":"file\|dir"}` | `kernel/source/src/file_watch.rs:83,317` |
| `FileChanged` | `file_watch:{path}` | `{"path":"...", "file_type":"file\|dir"}` | `kernel/source/src/file_watch.rs:84,327` |
| `FileDeleted` | `file_watch:{path}` | `{"path":"..."}` | `kernel/source/src/file_watch.rs:85,318` |

Produced by `source::file_watch::FileWatchSource`. Uses `notify` crate to watch filesystem directories. Each file event carries the affected path and file type.

### Timer Events

| Type | Source | Payload | Producer File:Line |
|------|--------|---------|-------------------|
| `TimerTick` | `timer:{name}` | depends on timer config | `kernel/source/src/timer.rs:118` |
| `Heartbeat` | `timer:{name}` | `{"heartbeat":true}` | `kernel/source/src/timer.rs:116` |

Produced by `source::timer::TimerSource`. Configurable interval. When `interval_ms >= 60000`, produces `Heartbeat`; otherwise `TimerTick`.

### Cron Events

| Type | Source | Payload | Producer File:Line |
|------|--------|---------|-------------------|
| `CronTick` | `cron:{id}` | depends on cron config | `kernel/source/src/cron.rs:136` |

Produced by `source::cron::CronSource`. Uses cron expressions (5 or 6 fields). Managed through `SourceRegistry` as a standard `EventSource` — scheduling is driven by the registry's background `poll_loop`.

### Message Events

| Type | Source | Payload | Producer File:Line |
|------|--------|---------|-------------------|
| `MessageReceived` | `gateway:http` | `{"session_id":"...", "text":"...", "channel":"tauri_desktop", "message_id":"...", "client_timestamp":...}` | `kernel/gateway/src/runtime/http.rs:3030` |
| `MessageReceived` | `socket:{name}` | depends on socket protocol | `kernel/source/src/socket.rs:117,148,185` |

Produced by the HTTP handler (from Tauri desktop via REST) or socket source (TCP/UDP connections). The desktop channel validates messages for length (max 4096 chars) and empty content. There is no `chat-source` plugin crate; HTTP-sourced `MessageReceived` events have SourceId `"gateway:http"`.

### Webhook Events

| Type | Source | Payload | Producer File:Line |
|------|--------|---------|-------------------|
| `WebhookReceived` | `webhook:{name}` | from HTTP request body | `kernel/source/src/webhook.rs:38` |

Produced by `source::webhook::WebhookSource`. Receives HTTP POST requests and converts the body (JSON) into the event payload.

### Signal Events

| Type | Source | Payload | Producer File:Line |
|------|--------|---------|-------------------|
| `SystemSignal` | `signal:{name}` | `{"signal":"SIGINT\|SIGTERM\|SIGHUP\|SIGUSR1\|SIGUSR2"}` | `kernel/source/src/signal.rs:94,101` |

Produced by `source::signal::SignalSource`. Listens for OS signals. Each signal produces one event. SIGUSR1/SIGUSR2 also produce a second event with the signal name.

### Workflow Events

| Type | Source | Payload | Producer File:Line |
|------|--------|---------|-------------------|
| `WorkflowStateChanged` | `workflow:engine` | `{"instance_id":"...", "workflow_name":"...", "from_state":"...", "to_state":"...", "reason":"...", "is_final":bool}` | `kernel/workflow/src/lib.rs:1201` |

Produced by the `WorkflowEngine` on every state transition. Records the workflow instance, old and new states, and the transition reason (Event, Timeout, ActionFailed, GuardRejected, RetryExceeded).

### Skill Events

| Type | Source | Payload | Producer File:Line |
|------|--------|---------|-------------------|
| `SkillReloaded` | `skill:hot_reload` | `{"inserted":[...], "updated_same_version":[...], "updated_new_version":[...], "removed":[...]}` | `kernel/gateway/src/runtime/agent_runtime.rs:4888` |

Auto-published by the runtime's skill hot-reload watcher when skill files change on disk. Contains lists of skills that were inserted, updated, or removed.

### Config Events

| Type | Source | Payload | Producer File:Line |
|------|--------|---------|-------------------|
| `ConfigChanged` | `config` | `{"changed_fields":["path.to.field",...], "meta":{"loaded_at_ms":..., "source_chain":[...]}}` | `kernel/config/src/lib.rs:1657` |

Produced by the config loader when config is modified. Lists the exact fields that changed and the config source chain.

### Reserved But Unused Event Types

The following EventType variants are defined in the enum but currently have no production publisher:
- `SkillLoaded` — defined, not published (skill loading uses `SkillReloaded` instead)
- `SecretRotated` — defined, not yet published (reserved for future secret rotation)
- `InjectionDetected` — defined, not yet published (reserved for future prompt injection detection)

### Recently Activated Event Types

These were previously reserved but are now published in production:
- `AgentMessage` — **now published** by the a2a (agent-to-agent) session system. See [Agent-to-Agent Events](#agent-to-agent-a2a-events) below.
- `EvaluationCompleted` — **now published** by `EvalHook` after each successful evaluation run. See [Evaluation Events](#evaluation-events) below.

### Cognitive & Emotion Events

| Literal Value | Bus | Purpose | Payload | Producer |
|---|---|---|---|---|
| `cognitive_state_changed` | **Global** | Consciousness state transition | `{"from":"...","to":"..."}` (Lucid/Groggy/Catatonic/Coma) | `ConsciousnessTranslator` |
| `emotion:evaluated` | **Global** / Local | Emotion assessment result | `{"emotion":"...","valence":...,"arousal":...}` | Eval/emotion system |

### LLM Backend Health Events

| Literal Value | Bus | Purpose | Payload | Producer |
|---|---|---|---|---|
| `llm_backend_down` | **Global** | LLM provider unreachable | `{"provider":"...","error":"..."}` | Consciousness/circuit-breaker |
| `llm_backend_recovered` | **Global** | LLM provider recovered | `{"provider":"..."}` | Consciousness/circuit-breaker |
| `llm_backend_connected` | **Global** | LLM provider connected | `{"provider":"..."}` | Connection establishment |

### Plan Events

| Literal Value | Bus | Purpose | Payload | Producer |
|---|---|---|---|---|
| `plan:created` | **Local** | Execution plan created | `{"plan_id":"...","steps":[...]}` | Planner tool |
| `plan:resumed` | **Local** | Execution plan resumed | `{"plan_id":"...","from_step":N}` | Planner tool |

### Auto-Continue Events

| Literal Value | Bus | Purpose | Payload | Producer |
|---|---|---|---|---|
| `agent:auto_continue` | **Global** | Agent initiated auto-continue | `{"agent_id":"...","session_id":"...","reason":"..."}` | AgentHarness |
| `agent:auto_continue_stopped` | **Global** | Auto-continue halted | `{"agent_id":"...","session_id":"...","reason":"..."}` | AgentHarness |

### Agent Reload Event

| Literal Value | Bus | Purpose | Payload | Producer |
|---|---|---|---|---|
| `agent:reloaded` | **Global** | Agent configuration/hot-reload applied | `{"agent_id":"..."}` | Runtime reload path |

---

## Custom Event Types (Reserved Constants)

### Chat Session Control Events

| Literal Value | Producer | Purpose | Payload | File:Line |
|---|---|---|---|---|
| `SESSION_CLOSE_CMD` | HTTP handler `chat_session_close` | Close a chat session | `{"session_id":"...", "operator":"...", "reason":...}` | `kernel/gateway/src/runtime/http.rs:3110` |
| `STOP_GENERATION` | HTTP handler `chat_session_stop` | Stop LLM generation | `{"session_id":"...", "operator":"..."}` | `kernel/gateway/src/runtime/http.rs:3191` |
| `RETRY_CMD` | HTTP handler `chat_session_retry` | Retry last message | `{"session_id":"...", "operator":"..."}` | `kernel/gateway/src/runtime/http.rs:3232` |
| `MESSAGE_EDITED` | HTTP handler `chat_session_edit` | Message edited | `{"session_id":"...", "message_event_id":"...", "new_text":"...", "operator":"..."}` | `kernel/gateway/src/runtime/http.rs:3314` |

These control events drive the chat-session workflow state machine. They are published via `workflow_engine.handle_event()` (for close/retry) or `runtime.publish_event()` (for stop/edit).

### Session & Gateway Lifecycle Events

| Literal Value | Producer | Purpose | Payload | File:Line |
|---|---|---|---|---|
| `session:started` | `session::SessionManager` | Chat session created | `{"session_id":"...","session_type":"...","operator":"..."}` | `kernel/gateway/src/runtime/session/mod.rs:493` |
| `session:closed` | `http.rs::chat_session_close()` | Chat session closed | `{"session_id":"...","operator":"..."}` | `kernel/gateway/src/runtime/http.rs:3137` |
| `gateway:starting` | `main.rs` | Gateway daemon starting | `{"bind":"..."}` | `kernel/gateway/src/main.rs:168-172` |
| `gateway:ready` | `main.rs` | Gateway ready to serve | `{"bind":"...","addr":"..."}` | `kernel/gateway/src/main.rs:243-247` |
| `gateway:stopping` | `Agenverse::shutdown()` | Gateway shutting down | `{}` | `kernel/gateway/src/runtime/agenverse.rs:249` |

Published at lifecycle boundaries: `gateway:starting`/`gateway:ready` from `main.rs` at startup, `gateway:stopping` from `Agenverse::shutdown()` during graceful shutdown. `session:started` is published by the `SessionManager` on session creation; `session:closed` is published from the HTTP handler. Both carry the `session_id` for trace chain correlation.

> `session:timeout` is reserved in the milestone plan but deferred — production currently lacks a timeout polling loop for workflow instances.

### LLM & Agent Events (Published by AgentHarness / LlmCognitiveEngine)

| Literal Value | Bus | Purpose | Payload | Producer |
|---|---|---|---|---|
| `llm:call_started` | **Local** | LLM provider call initiated | `{"agent_id":"...","session_id":"...","turn":N}` | `LlmCognitiveEngine` |
| `llm:call_ended` | **Local** | LLM provider call completed | `{"agent_id":"...","session_id":"...","turn":N,"success":bool}` | `LlmCognitiveEngine` |
| `llm_error` | **Local** | LLM call error | `{"agent_id":"...","session_id":"...","turn":N,"error":"..."}` | `LlmCognitiveEngine` / `AgentHarness` |
| `agent:token_used` | **Local** | Token usage estimate | `{"agent_id":"...","session_id":"...","turn":N,"tokens":N}` | `LlmCognitiveEngine` |
| `agent:reply_ready` | **Global** | Agent response ready | `{"agent_id":"...","session_id":"...","reply":"...","turns_processed":N}` | `AgentHarness` |
| `agent:reply_interrupted` | **Global** | User stopped generation | `{"agent_id":"...","session_id":"...","error":"..."}` | `AgentHarness` (`agent_harness.rs:363`) |
| `agent:idle` | **Global** | Agent returned to idle state (post-processing cleanup) | `{"agent_id":"...","session_id":"..."}` | `AgentHarness` |
| `agent:reply_stream_start` | **Local** | Streaming response started | `{"agent_id":"...","session_id":"...","turn":N,"extra":{}}` | `AgentHarness` (stream forwarder) |
| `agent:reply_chunk` | **Local** | Streaming response delta | `{"agent_id":"...","session_id":"...","turn":N,"extra":{"delta":"..."}}` | `AgentHarness` (stream forwarder) |
| `agent:reply_stream_done` | **Local** | Streaming response complete | `{"agent_id":"...","session_id":"...","turn":N,"extra":{"finish_reason":"..."}}` | `AgentHarness` (stream forwarder) |
| `agent:reply_stream_error` | **Local** | Streaming error | `{"agent_id":"...","session_id":"...","error":"..."}` | `AgentHarness` (stream forwarder) |
| `tool:dispatched` | **Local** | Tool call dispatched | `{"agent_id":"...","session_id":"...","tool_call_id":"...","tool_name":"...","args":{...}}` | `LlmCognitiveEngine` (ReAct loop) |
| `tool:completed` | **Local** | Tool call succeeded | `{"agent_id":"...","session_id":"...","tool_call_id":"...","tool_name":"...","success":true,"duration_ms":N,"output":"..."}` | `cognitive/llm/src/lib.rs:521` (ReAct tool executor) |
| `tool:failed` | **Local** | Tool call failed | `{"agent_id":"...","session_id":"...","tool_call_id":"...","tool_name":"...","success":false,"duration_ms":N,"output":"..."}` | `LlmCognitiveEngine` (ReAct loop) |
| `tool:security_denied` | **Local** | Tool blocked by security | `{"agent_id":"...","session_id":"...","tool_call_id":"...","tool_name":"...","block_type":"hardline\|path_denied","reason":"..."}` | `kernel/tool/src/lib.rs` (ToolExecutor / PermissionReviewer) |
| `agent:got_tool_calls` | **Local** | LLM requested tool calls | `{"agent_id":"...","session_id":"...","turn":N,"tool_calls":[...]}` | `AgentHarness` |
| `agent:tool_results_fed_back` | **Local** | Tool results fed to LLM | `{"agent_id":"...","session_id":"...","turn":N,"result_count":N}` | `AgentHarness` |
| `agent:history_compressed` | **Local** | Context window trimmed | `{"agent_id":"...","session_id":"...","turn":N,"messages_removed":N,"tokens_saved":N,"strategy":"truncate\|summarize"}` | `AgentHarness` |
| `agent:config_warning` | **Global** | Budget config not set | `{"agent_id":"...","session_id":"...","config_key":"...","message":"..."}` | `AgentHarness` |
| `eval:evaluation_completed` | **Global** | Evaluation run completed | `{"target_kind":"...","target_id":"...","rule_id":"...","strategy":"...","aggregate_score":...,"threshold":...,"outcome":"...","dimensions":{...},"hook_point":"..."}` | `EvalHook` |

Published by `AgentHarness` and `LlmCognitiveEngine` to track the ReAct loop lifecycle. `llm:call_started`/`llm:call_ended` bracket each LLM provider invocation. Streaming events (`agent:reply_stream_*`) deliver real-time response content. Tool events (`tool:dispatched/completed/failed`) track tool execution within the loop. `agent:idle` is published after every message processing completion (or error) to signal the Idle System.

> **Historical note**: The producer was previously `LlmReActEngine` (deleted in the ReAct migration). All LLM lifecycle events are now produced by `LlmCognitiveEngine` (`cognitive/llm/`), which encapsulates the full ReAct loop internally. The `AgentHarness` layer publishes higher-level agent events and delegates processing to `LlmCognitiveEngine::process()` via `process_message_v2()`.

**Bus routing**: Agent-internal events (LLM calls, tool execution, streaming, token tracking, ReAct internals) are published to the **Local Bus** so that one agent's internal events don't pollute another agent's event stream or trigger global backpressure. Agent lifecycle events (`agent:reply_ready`, `agent:reply_interrupted`, `agent:idle`, `agent:config_warning`) remain on the **Global Bus** for frontend visibility. When no Local Bus is configured (single-agent mode), all events fall back to the Global Bus.

### Message Dispatch Events

| Literal Value | Producer | Purpose | Payload | File:Line |
|---|---|---|---|---|
| `message:dispatch` | `SkillEventDispatcher` | Event routed to skill(s) for processing | `{"trace_id":"...","event_id":"...","event_type":"...","source":"..."}` | `kernel/gateway/src/runtime/agent_runtime.rs:464` |
| `message:completed` | `SkillEventDispatcher` | Skill(s) finished processing | `{"trace_id":"...","executed":[...],"failed":[...]}` | `kernel/gateway/src/runtime/agent_runtime.rs:471` |

Published by the `SkillEventDispatcher` — a catch-all EventBus subscriber registered in the runtime builder. On every incoming event, `message:dispatch` fires before routing to matching skills and `message:completed` fires after all matching skills have executed (success or failure).

### Tool Events

| Literal Value | Producer | Purpose | Payload | File:Line |
|---|---|---|---|---|
| `tool:invoke` | `pipeline:tool` | Tool execution started | `{"tool_name":"...","pipeline_id":"...","instance_id":"..."}` | `kernel/gateway/src/runtime/agent_runtime.rs:2321` |
| `tool:completed` | `pipeline:tool` / `cognitive-engine` | Tool execution succeeded | `{"tool_name":"...","pipeline_id":"...","instance_id":"...","duration_ms":N}` | `agent_runtime.rs:2333` (Pipeline) / `cognitive/llm/src/lib.rs:521` (production) |
| `tool:failed` | `pipeline:tool` | Tool execution failed | `{"tool_name":"...","pipeline_id":"...","instance_id":"...","error":"..."}` | `agent_runtime.rs:2346` |

Defined via the `ToolEventSink` trait (in `kernel/pipeline/src/lib.rs`) and wired into `PipelineEngine::execute_tool_with_retry()`. The `BusToolEventSink` implementation in the gateway crate (`kernel/gateway/src/runtime/agent_runtime.rs:2319-2357`) converts these sink callbacks into EventBus publishes.

> **Architecture note**: `BusToolEventSink` is currently not wired in production (marked `#[allow(dead_code)]`); it is infrastructure for when `PipelineEngine` is adopted in the production tool path. The **live** production `tool:completed` publisher is `cognitive/llm/src/lib.rs:521` (source `"cognitive-engine"`) — the ReAct loop's tool executor publishes tool results directly to the agent's Local Bus.

### Capability Events

| Literal Value | Purpose | Payload | File:Line |
|---|---|---|---|
| `capability_available` | New capability registered | `{"capability":"..."}` | `kernel/gateway/src/runtime/agent_runtime.rs:4676` |
| `capability_removed` | Capability unregistered | `{"capability":"...","reason":"..."}` | `kernel/gateway/src/runtime/agent_runtime.rs:4683` |
| `capability_registry_updated` | Full capability registry refresh | `{"available":[...], "added":[...], "removed":[...]}` | `kernel/gateway/src/runtime/agent_runtime.rs:4690` |

Published by the capability registry during startup and plugin hot-load/unload. The `registry_updated` event is a summary and does not enter the WAL.

### Soul Events

| Literal Value | Purpose | Payload | File:Line |
|---|---|---|---|
| `soul_changed` | SOUL file modified on disk | `{"name":"...", "boundaries":[...], "preferences":{...}}` | `kernel/soul/src/lib.rs:384` |

Published by the SOUL hot-reload manager when the SOUL.md file changes. Contains the new soul name, boundaries, and preferences. This event is also passed through to the EventStore for audit purposes.

### Evaluation Events

| Literal Value | Purpose | Payload | File:Line |
|---|---|---|---|
| `eval:evaluation_completed` | Eval system completed an evaluation | `{"target_kind":"...","target_id":"...","rule_id":"...","strategy":"...","aggregate_score":...,"threshold":...,"outcome":"...","dimensions":{...},"hook_point":"..."}` | `kernel/eval/src/hook.rs:127` |

Published by `EvalHook` (implements the `Hook` trait) when triggered at registered `HookPoint`s (e.g., `SkillExecuted`, `AgentReady`). Each evaluation result produces one `EvaluationCompleted` event. Results are also stored in-memory in the `EvalEngine` and accessible via the `eval_get_results` tool.

**Configuration:** The `EvalHook` accepts an optional `EvalEventPublisher` callback — wired to the event bus at gateway startup via `EvalHook::with_event_publisher()`. Without a publisher, evaluation results are stored in-memory only (no events emitted).

### Idle System Events

The Idle System produces several custom events not covered by the `EventType::Idle` enum variant (which is produced by `idle.system` in `kernel/idle/src/types.rs:109`).

| Literal Value | Bus | Purpose | Payload | Producer |
|---|---|---|---|---|
| `idle.cycle_completed` | **Local** | Exploration/meditation/incubation cycle finished | `{"cycle_type":"...","duration_ms":N,"output":{...}}` | Exploration (`runtime/exploration.rs:449`), Meditation (`runtime/meditation.rs:426`), Incubation (`runtime/incubation_runner.rs:500`) |
| `idle.inspiration` | **Local** | Incubation insight discovered | `{"prompt":"...","inspiration":"..."}` | Incubation (`runtime/incubation_runner.rs:389`) |
| `agent:cold_start_done` | **Global** + Local | Idle manager initialization complete | `{"agent_id":"..."}` | `kernel/idle/src/manager.rs:610` |

> **Note on naming**: `IdleKind` variants (`Daze`, `Boredom`, `Sleep`, `Exploration`, `Meditation`, `Incubation`, `WakeUp`) are **not** separate event strings. They are serialized inside the `IdleEvent.kind` payload of the single `idle.system` event.

### Agent-to-Agent (A2A) Events

The a2a system enables independent agent-to-agent communication sessions. When Agent A sends a message to Agent B, `AgentMessage` events flow through the Global Bus:

| Literal Value | Bus | Purpose | Payload | Producer |
|---|---|---|---|---|
| `agent:message` | **Global** | Cross-agent message | `{"message_id":"...","from_agent":"...","to_agent":"...","session_id":"...","text":"...","reply_to":"..."}` | `AgentHarness::send_agent_message()` → `AgentMessageHandler` |

**Key behaviors:**
- A2A sessions use a separate session namespace (`a2a:{from_agent}:{to_agent}:{uuid}`) and don't trigger `session:started`/`session:closed` events
- Self-published `AgentMessage` events are ignored to prevent echo loops
- `agent:reply_ready` and `agent:idle` events are suppressed for a2a sessions (frontend-only concepts)
- Replies carry a `reply_to` field referencing the original message UUID for chain tracking
- SOUL.md is loaded directly from the agent data directory (`~/.aman/agents/{agent}/SOUL.md`)
- `AgentMessageHandler` subscribes to `agent:message` on the Global Bus and spawns `process_message_v2()` for the target agent

**Agent messaging tools:**

| Tool | Purpose |
|------|---------|
| `agent_list` | Discover available agents and their capabilities |
| `agent_send_message` | Send a message to another agent, optionally wait for reply |
| `aman.get_agents` (JSON-RPC) | List all agents with capabilities |
| `aman.send_agent_message` (JSON-RPC) | Send agent-to-agent message via JSON-RPC |

**Architecture:**
```
Agent A (LLM calls agent_send_message)
  → AgentHarness::send_agent_message()
    → publish EventType::AgentMessage on Global Bus
      → AgentMessageHandler::handle()
        → resolve target agent
        → spawn process_message_v2() on target agent's harness
          → Agent B executes ReAct loop
            → reply published as AgentMessage with reply_to
              → Agent A receives reply
```

### Agent ReAct Internal Events

| Literal Value | Bus | Purpose | Payload | Producer |
|---|---|---|---|---|
| `agent:idle` | **Global** | Agent returned to idle state after processing | `{"agent_id":"...","session_id":"..."}` | `AgentHarness` (post-LLM cleanup) |
| `agent:reply_stream_error` | **Local** | Streaming reply error | `{"agent_id":"...","session_id":"...","error":"..."}` | `AgentHarness` |

Published after message processing completes (or errors), signaling the Idle System that the agent is ready for idle depth tracking.

### Workflow Control Events

| Literal Value | Purpose | Payload | Producer File:Line |
|---|---|---|---|
| `retry` | Manual retry of errored workflow | `{"operator":"..."}` | `kernel/gateway/src/runtime/http.rs:682` |
| `cancel` | Cancel pending retry | `{"operator":"..."}` | `kernel/gateway/src/runtime/http.rs:725` |
| `retry` | Auto-retry by workflow engine | `{"auto_retry":true, "attempt":N}` | `kernel/workflow/src/lib.rs:1111` |

Published by HTTP handlers and the workflow engine's auto-retry mechanism.

---

## Work System Events

The Work System (`kernel/work/`) models task discovery, claiming, execution, and review as an event-driven state machine. Each agent instance owns a private WorkSystem that publishes internal flow events to the agent's **Local Bus**. External board events originate from kanban/team plugins on the **Global Bus** and are injected into the agent's Local Bus for processing.

### Architecture

```
┌──────────────────────────────────────────────────────────────────┐
│                      Agent Local Bus                              │
│                                                                   │
│  ┌─────────────────────────────────────────────────────────────┐ │
│  │                    Work System State Machine                  │ │
│  │                                                               │ │
│  │  IDLE ──▶ CHECKING ──▶ CLAIMING ──▶ EXECUTING ──▶ REVIEWING  │ │
│  │    ▲                      │              │            │       │ │
│  │    │                      ▼              ▼            ▼       │ │
│  │    └──── Interrupt ──── (any state) ──── saves checkpoint    │ │
│  │                                                               │ │
│  │  Chain: ExecuteStep(0) → StepComplete → ExecuteStep(1) → ...  │ │
│  │  (keeps Bus non-empty → Idle System won't trigger)           │ │
│  └─────────────────────────────────────────────────────────────┘ │
│                                                                   │
│  Work internal events (work.*) ──▶ Local Bus only                 │
│  External board events ──▶ routed from Global Bus                 │
└──────────────────────────────────────────────────────────────────┘
                                 ▲
                                 │ (TaskBoardUpdated, WorkTick)
┌────────────────────────────────┴─────────────────────────────────┐
│                      Global Event Bus                             │
│  kanban.plugin ──▶ publish(TaskBoardUpdated)                      │
│  team.plugin   ──▶ publish(WorkTick)                              │
└──────────────────────────────────────────────────────────────────┘
```

### Event Routing

Work events are routed to the agent's Local Bus by the agent's event handler:

| Event Kind | Bus | Direction |
|-----------|-----|-----------|
| `kanban.task_board_updated` | Global → Local (injected) | External → Work System |
| `team.work_tick` | Global → Local (injected) | External → Work System |
| `work.work_tick` | Global → Local (injected) | External → Work System |
| `work.delayed_work_tick` | Local (self-scheduled) | Timer → Work System |
| `work.start_check` through `work.cycle_done` | Local | Internal state machine |
| `work.interrupt` | Local | AgentScheduler → Work System |

### Work State Machine

Five states, fully event-driven. The `Interrupt` event is highest priority — any active state receiving `Interrupt` saves a checkpoint and returns to `IDLE`.

| Current State | Event | Next State | Notes |
|---|---|---|---|
| `IDLE` | `TaskBoardUpdated` / `WorkTick` / `DelayedWorkTick` (cooldown passed) | `CHECKING` | Ignores tick if cooldown not elapsed |
| `CHECKING` | `StartCheck` + tasks available | `CLAIMING` | Selects best task via personality strategy |
| `CHECKING` | `StartCheck` + no tasks | `IDLE` | Schedules `DelayedWorkTick` for next poll |
| `CLAIMING` | `ClaimResponse(success=true)` | `EXECUTING` | Decomposes task into steps, posts first `ExecuteStep` |
| `CLAIMING` | `ClaimResponse(success=false)` | `IDLE` | Injects frustration, applies backoff |
| `EXECUTING` | `StepComplete` + more steps | `EXECUTING` | Chains next `ExecuteStep` (keeps Bus non-empty) |
| `EXECUTING` | `StepComplete` + last step | `REVIEWING` | Posts `ReviewTask` |
| `EXECUTING` | `StepFailed` (retryable) | `EXECUTING` | Retries same step |
| `EXECUTING` | `StepFailed` (non-retryable) | `IDLE` | Abandons task |
| `REVIEWING` | `ReviewComplete(passed=true)` | `IDLE` | Submits result, injects satisfaction |
| `REVIEWING` | `ReviewComplete(passed=false)` | `IDLE` | Submits failed result, injects disappointment |
| **Any active** | **`Interrupt`** | **`IDLE`** | Saves checkpoint, cancels delayed ticks |

### Work System Event Reference

All work events are published as `EventType::Custom("<kind>")` with the event payload serialized as JSON under the `work_event_type` tag.

#### External / Board Events

| Kind | Source | Payload | Producer |
|------|--------|---------|----------|
| `kanban.task_board_updated` | kanban plugin | `{"work_event_type":"task_board_updated","board_id":"...","change_type":"task_added\|task_removed\|task_updated\|stage_bulk_move"}` | kanban plugin on Global Bus |
| `team.work_tick` | team plugin | `{"work_event_type":"work_tick","triggered_by":"cron\|webhook\|manual"}` | team plugin on Global Bus |

#### Delayed Timer Events

| Kind | Source | Payload | Producer |
|------|--------|---------|----------|
| `work.delayed_work_tick` | `work.system` | `{"work_event_type":"delayed_work_tick","fire_at":<timestamp_ms>,"reason":"..."}` | `WorkSystem::schedule_delayed_tick()` via `tokio::spawn` |

#### Internal State Machine Events

| Kind | Source | Payload | Producer |
|------|--------|---------|----------|
| `work.start_check` | `work.system` | `{"work_event_type":"start_check"}` | `WorkSystem::handle()` after transitioning to `CHECKING` |
| `work.claim_task` | `work.system` | `{"work_event_type":"claim_task",...task_brief}` | `WorkSystem::handle()` after task selection |
| `work.claim_response` | `work.system` | `{"work_event_type":"claim_response","task":{...},"success":bool,"reason":...}` | `WorkSystem::handle_claim_task()` after board response |
| `work.execute_step` | `work.system` | `{"work_event_type":"execute_step","task_id":"...","step_index":N}` | `WorkSystem::handle()` to execute each step |
| `work.step_complete` | `work.system` | `{"work_event_type":"step_complete","task_id":"...","step_index":N,"output":{...}}` | `WorkSystem::handle_execute_step()` on step success |
| `work.step_failed` | `work.system` | `{"work_event_type":"step_failed","task_id":"...","step_index":N,"error":{...}}` | `WorkSystem::handle_execute_step()` on step failure |
| `work.review_task` | `work.system` | `{"work_event_type":"review_task",...task_brief}` | `WorkSystem::handle()` when all steps complete |
| `work.review_complete` | `work.system` | `{"work_event_type":"review_complete","task_id":"...","passed":bool,"feedback":...}` | `WorkSystem::handle_review_task()` after verification |
| `work.submit_result` | `work.system` | `{"work_event_type":"submit_result","task_id":"...","result":{...}}` | `WorkSystem::handle_review_task()` to board |
| `work.cycle_done` | `work.system` | `{"work_event_type":"cycle_done","task_id":"...","outcome":"completed\|failed\|abandoned","duration":{...}}` | `WorkSystem::handle_review_task()` on work cycle end |

#### System Interrupt Events

| Kind | Source | Payload | Producer |
|------|--------|---------|----------|
| `work.interrupt` | AgentScheduler | `{"work_event_type":"interrupt","reason":"user_query\|study_activated\|daily_activated\|shutdown","by_system":"core\|study\|daily_life"}` | `AgentScheduler::activate_system()` when switching subsystems |

### Work System Trace Events

In addition to EventBus events, the Work System records structured trace events for observability. These are written to the agent's private `TraceStore`:

| Trace Event | When | Fields |
|------------|------|--------|
| `CheckStarted` | Task board poll begins | `candidates_count` |
| `ClaimAttempted` | Claim request sent/received | `task_id`, `outcome` (Success / TaskTakenByOther / PermissionDenied / BoardUnavailable) |
| `StepExecuted` | Each step completes (success or failure) | `task_id`, `step_index`, `duration`, `success`, `error` |
| `ReviewCompleted` | Review finishes | `task_id`, `passed`, `confidence` |
| `CycleCompleted` | Full work cycle ends | `task_id`, `outcome`, `total_duration`, `steps_completed`, `steps_failed` |
| `Interrupted` | Work interrupted by scheduler | `task_id`, `reason`, `by_system` |

### Work Personality Configuration

Each agent's work behavior is configured via `WorkPersonality` in `aman.yaml`:

```yaml
work:
  personality:
    auto_claim: true
    capabilities: [code, refactor, fix, review]
    max_concurrent: 2
    work_cooldown: 60s
    claim_retry:
      base_delay: 30s
      backoff_multiplier: 2.0
      max_delay: 300s
      max_consecutive_failures: 5
    selection:
      type: weighted
      priority_weight: 0.4
      match_weight: 0.4
      age_weight: 0.2
    decomposition:
      max_step_duration: 120s
      isolate_llm_calls: true
      isolate_tool_calls: true
  board:
    type: kanban
    poll_interval: 30s
    query:
      stages: [backlog, wip]
      limit: 20
  review:
    auto_verify: true
    require_human_approval_for:
      - "git push --force"
      - "rm -rf"
      - "DROP TABLE"
    timeout: 120s
```

### Implementation Files

| Component | File | Role |
|-----------|------|------|
| `WorkState` / `WorkEvent` / `WorkContext` | `kernel/work/src/types.rs` | Core type definitions |
| `WorkPersonality` / `TaskSelectionStrategy` | `kernel/work/src/personality.rs` | Agent work behavior config |
| `WorkConfig` / `BoardConfig` / `ReviewConfig` | `kernel/work/src/config.rs` | YAML config + validation |
| `WorkSystem::handle()` | `kernel/work/src/system.rs` | State machine engine |
| `WorkBoardClient` trait | `kernel/work/src/system.rs` | Board abstraction (kanban/team) |
| `WorkTraceEvent` | `kernel/work/src/trace.rs` | Trace store event types |

### Idle System Coordination

The Work System and Idle System coordinate through the Event Bus:

- **Bus non-empty → Idle suppressed**: During task execution, the chain of `ExecuteStep` → `StepComplete` → next `ExecuteStep` keeps the agent's Local Bus non-empty, naturally preventing the Idle System from triggering.
- **Bus empty → Idle active**: After `WorkCycleDone` and before the next `DelayedWorkTick`, the Bus is empty, allowing Idle System to run (Daze → Boredom → ...).
- **Feedback loop**: Task outcomes inject `Satisfaction` / `Frustration` / `Disappointment` signals into the Idle System's arousal tracker, affecting future idle behavior.

---

## HTTP API Event Endpoints

All endpoints in `kernel/gateway/src/runtime/http.rs`:

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
| `menu:reload_skills` | Menu action → Frontend | On demand | Handled via Tauri menu event | `desktop/src/lib.rs:38` |
| `metrics:updated` | Background task → Frontend | Every 2s | `Dashboard.svelte`, `DebugPanel.svelte` | `desktop/src/lib.rs:180` |
| `event:processed` | Background task → Frontend | Every 1s | `Chat.svelte:936`, `DebugPanel.svelte:102` | `desktop/src/lib.rs:200` |

The background tasks poll the gateway HTTP API:
- `metrics:updated` — polls `GET /debug/metrics` every 2 seconds
- `event:processed` — polls `GET /events/recent` every 1 second, de-duplicates by event ID

---

## Chat-Session Workflow Transitions

The chat-session workflow (`kernel/gateway/src/runtime/agent_runtime.rs:202-352`) is driven by events. The mapping from event types to workflow transitions:

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
| `EventBus` trait | `kernel/event-bus/src/lib.rs:226` | `publish()`, `subscribe()`, `unsubscribe()`, `try_dequeue()`, `wait_for_event()`, `metrics()`, `backpressure_level()`, `can_poll()` |
| `InMemoryBus` | `kernel/event-bus/src/lib.rs:439` | In-memory implementation with priority queues, used for both Global and Local buses |
| `InMemoryBusConfig` | `kernel/event-bus/src/lib.rs:171` | Backpressure thresholds (L1:80.97%, L2:90.11%, L3:95.97%, L4A:98.11%), retry, dedup, overflow, rate limiting |
| `SubscriptionFilter` | `kernel/event-bus/src/lib.rs:62` | Filter by `event_types`, `sources`, `priorities`, `payload_match` |
| `OverflowDir` | `kernel/event-bus/src/overflow.rs` | Disk overflow when queue is full (Level 4A) |
| `BackpressureController` | `kernel/event-bus/src/backpressure.rs` | 7-level backpressure (Normal → L1 → L2 → L3 → L4A → L4B → Critical) |
| `DedupWindow` | `kernel/event-bus/src/dedup.rs` | Two-level dedup: Bloom filter + LRU cache (30s window default) |
| `RetryQueue` | `kernel/event-bus/src/retry_queue.rs` | Retry for AtLeastOnce delivery with exponential/sequence backoff |
| `EventRateLimiter` | `kernel/event-bus/src/rate_limiter.rs` | Per-source token-bucket rate limiter |
| `PersistentBus` | `kernel/persistence/src/persistent_bus.rs` | WAL-backed persistent event bus (Global Bus only) |

### Dual-Layer Configuration

| Layer | Default `max_queue_size` | Backpressure Scope | Config Source |
|---|---|---|---|
| **Global Bus** | 10,000 | System-wide (all agents + sources share one queue) | `event_bus.max_queue_size` in `aman.yaml` |
| **Local Bus** (per-agent) | 1,000 | Per-agent (agent's own events only) | `agents.<id>.event_bus.max_queue_size` in `aman.yaml` |

Both buses use the same 7-level backpressure mechanism. When an agent's Local Bus queue fills, only that agent's publishers are affected — other agents continue unaffected. The Global Bus backpressure affects all sources and cross-agent communication.

### Backpressure Levels (7-Level Hierarchy)

| Level | Queue Usage | Default Threshold | Behavior |
|-------|-------------|-------------------|----------|
| **Normal** | 0–80.97% | `< 0.8097` | All events processed normally |
| **L1** | 80.97–90.11% | `≥ 0.8097` | Priority degradation: `AtMostOnce` events downgraded one level (High→Normal, Normal/Low→Low). Idle events silently discarded above Normal. |
| **L2** | 90.11–95.97% | `≥ 0.90109` | `AtMostOnce` events dropped. Idle events discarded. |
| **L3** | 95.97–98.11% | `≥ 0.9597` | Guaranteed-delivery events blocked (`Error::BackpressureBlocked(L3)`). `pause_publishers = true`. `can_poll()` returns `false`. |
| **L4A** | 98.11–99.99% | `≥ 0.98110` | Guaranteed-delivery events overflow to disk (`OverflowDir`). Non-guaranteed events still blocked. |
| **L4B** | L4A + overflow dir ≥ 80% full | — | Emergency fallback: instead of overflowing, return `RetryLater(L3)` error. `OverflowDirEmergency` event logged. |
| **Critical** | 100% | `≥ 1.0` | All `Low` priority events dropped. Queue-full → `BusFull` error rejects all publishes. `pause_publishers = true`. `can_poll()` = `false`. |

Key implementation details:
- **Idle events** (`Idle`, `QueueDrained`) are silently discarded at any level above Normal — they never consume queue capacity under pressure
- **L4A → L4B transition** is dynamic: when the overflow directory usage exceeds 80%, new overflow attempts are rejected
- **Recovery** is automatic: as the queue drains below each threshold, backpressure eases back to Normal
- **Backpressure events** (`BackpressureEventLog`) record level transitions, drops, blocks, overflows, and emergency states; bounded at `backpressure_event_limit` (default: 128)

### AgentRegistry Bus Management

`AgentRegistry` (`kernel/gateway/src/runtime/agent_registry.rs`) stores per-agent Local Buses in a `RwLock<HashMap<String, Arc<dyn EventBus>>>`:

- `set_local_bus(agent_id, bus)` — called during `load_from_config()` to create each agent's Local Bus
- `get_local_bus(agent_id) -> Option<Arc<dyn EventBus>>` — called by `LlmCognitiveEngine` and `AgentHarness` to resolve the correct bus for each agent's internal events
- `clear()` — removes all Local Buses alongside agent instances during shutdown

---

## Event Store

Defined in `kernel/gateway/src/runtime/event_store.rs`:

- **Capacity**: Global cap (configurable) + per-trace cap (max events per trace_id)
- **`record(event)`**: Stores event by ID, indexes by trace_id, builds trace_children graph from `payload.trace_prev`
- **`get(id)`**: Retrieves single event by UUID
- **`trace(trace_id)`**: Returns all events sharing the trace_id
- **`trace_chain(trace_id)`**: BFS traversal of trace ancestors + descendants
- **`recent(count)`**: Most recent N events by insertion order

The `StoreAllEventsHandler` (`kernel/gateway/src/runtime/agent_runtime.rs:5707`) is a catch-all subscriber that records every published event into the EventStore.

---

## Dead Letter Queue (DLQ)

`kernel/persistence/src/dlq.rs` — `InMemoryDeadLetterQueue`:

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

`kernel/gateway/src/runtime/audit.rs` — `AuditLogger`:

- Ring buffer of `AuditRecord` with fields: `id`, `operator`, `action`, `target`, `outcome`, `detail`, `timestamp`
- Capacity set at call site (typically 2000 records)
- Recorded at ~100+ call sites across HTTP handlers and the runtime
- Queryable via `GET /audit-log` with filters: `action`, `operator`, `since_ms`, `until_ms`, `limit`, `offset`
