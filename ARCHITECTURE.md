# Architecture Overview

aman is an event-driven agent framework composed of ~40 Rust crates organized into a layered architecture.

## Layer Diagram

```
┌──────────────────────────────────────────────────────────┐
│                    Application Layer                      │
│   CLI (aman)      Tauri Desktop      HTTP API (axum)     │
├──────────────────────────────────────────────────────────┤
│                     Gateway Layer                         │
│   AgentRuntime    Phase Manager      SecretResolver      │
│   Health Check    Drain Handler      Config Loader       │
│   SSE Stream      Plugin Routes      Chat Sessions       │
├──────────────────────────────────────────────────────────┤
│                  Cognitive Engine Layer                   │
│   CognitiveEngine (trait)     LlmCognitiveEngine (impl)  │
│   Observation → Decision      Full ReAct loop + 3 providers │
│   cognitive-react (shared)    OpenAI / Anthropic / Local │
├──────────────────────────────────────────────────────────┤
│                  Agent Lifestyle Layer                    │
│   Lifecycle    Idle (boredom)    Daily-Life    Work      │
│   Study        Memory            Notification   Eval     │
├──────────────────────────────────────────────────────────┤
│                  Orchestration Layer                      │
│   Dispatcher      Pipeline Engine     Workflow Engine    │
│   Route Rules     Step Executor       State Machine      │
│   Fan Out         Compensation        Timeout Manager    │
├──────────────────────────────────────────────────────────┤
│                  Capability Layer                         │
│   Skill Registry    Tool Runner       Plugin Loader      │
│   Skill Search      Built-in Tools    WASM Runtime       │
│   Hot Reload        Sandbox           SOUL System        │
├──────────────────────────────────────────────────────────┤
│                   Event Bus Layer                         │
│   InMemoryBus       PersistentBus     Backpressure       │
│   Ordered Queue     Dedup Window      Retry Queue        │
│   Overflow Disk     Subscription      Bus Metrics        │
├──────────────────────────────────────────────────────────┤
│                 Event Source Layer                        │
│   SourceRegistry    TimerSource       CronSource         │
│   FileWatch         WebhookSource     SignalSource       │
│   SocketSource                                        │
├──────────────────────────────────────────────────────────┤
│                   Persistence Layer                       │
│   Write Ahead Log   StateStore        Dead Letter Queue  │
│   Checkpoint        SledStore         Overflow Mgmt      │
├──────────────────────────────────────────────────────────┤
│                    Core Layer                             │
│   Kernel (types)    Macros (proc)     Config (schema)    │
│   Secret (multi)    Redactor          Hook System        │
└──────────────────────────────────────────────────────────┘

Plugin crates live under `kernel/plugins/` (messaging channels, memory stores) and are loaded dynamically at runtime.
```

## Directory Structure

```
aman/
├── kernel/              ← infrastructure crates
│   ├── core/            ← package: kernel — core types, traits, redactor
│   ├── event-bus/       ← event bus with backpressure
│   ├── gateway/         ← agent gateway daemon (binary: aman)
│   ├── plugins/         ← messaging, memory-store, info-hub
│   └── ...
├── cognitive/           ← cognitive engine abstraction
│   ├── engine/          ← CognitiveEngine trait (engine-agnostic)
│   └── llm/             ← LlmCognitiveEngine (LLM implementation)
├── desktop/             ← Tauri v2 desktop app
└── docs/                ← design docs, diagrams
```

## Crate Map

### Core & Infrastructure

| Crate | Path | Role |
|---|---|---|
| `kernel` | `kernel/core` | Core types, traits, error types, redactor |
| `macros` | `kernel/macros` | `#[skill]`, `#[plugin]` proc macros |
| `config` | `kernel/config` | 4-layer configuration loading, validation |
| `secret` | `kernel/secret` | Multi-backend secrets, AES-256-GCM cache, rotation |
| `hook` | `kernel/hook` | Internal hook system for lifecycle events |
| `persistence` | `kernel/persistence` | WAL, StateStore, DLQ, overflow management |
| `skm-core-patched` | `kernel/skm-core-patched` | Patched fork of skill-manager core (Tantivy fixes) |

### Event System

| Crate | Path | Role |
|---|---|---|
| `event-bus` | `kernel/event-bus` | Event bus with 5-level backpressure, dedup, ordering |
| `source` | `kernel/source` | Event sources (timer, cron, filewatch, webhook, signal, socket) |
| `dispatcher` | `kernel/dispatcher` | Event routing and rule matching |

### Orchestration

| Crate | Path | Role |
|---|---|---|
| `pipeline` | `kernel/pipeline` | Pipeline step execution and compensation |
| `workflow` | `kernel/workflow` | State machine engine with timeouts and recovery |

### Cognitive Engine (`cognitive/`)

| Crate | Path | Role |
|---|---|---|
| `cognitive-engine` | `cognitive/engine` | **CognitiveEngine trait** — engine-agnostic abstraction: Observation → Decision. No LLM dependencies. |
| `cognitive-llm` | `cognitive/llm` | **LlmCognitiveEngine** — LLM-based implementation. Consolidates: LlmProvider trait, ReAct types, OpenAI provider, prompt pipeline, context manager, simple HTTP client. Implements `CognitiveEngine`. |

### Capability

| Crate | Path | Role |
|---|---|---|
| `skill` | `kernel/skill` | Skill registry, Tantivy search, hot reload, versions |
| `tool` | `kernel/tool` | Tool runner with built-in tools (file, http, exec, db) |
| `plugin` | `kernel/plugin` | Plugin host (loader, lifecycle, WASM/subprocess/in-process) |
| `soul` | `kernel/soul` | SOUL identity system (boundaries, preferences, hot-reload) |
| `context-manager` | `kernel/context-manager` | Token budgeting, context compression, rotation (LLM-specific) |

### Agent Lifestyle

| Crate | Path | Role |
|---|---|---|
| `lifecycle` | `kernel/lifecycle` | Agent lifecycle state machine (Phases 0→5, 5→0) |
| `idle` | `kernel/idle` | Idle mode: background observation, proactive suggestions |
| `daily-life` | `kernel/daily-life` | Daily-life automation: routines, schedules, habits |
| `work` | `kernel/work` | Work item processing: intake, prioritization, execution |
| `study` | `kernel/study` | Study/learning system: knowledge acquisition |
| `memory` | `kernel/memory` | Agent memory: episodic/semantic storage, retrieval |
| `notification` | `kernel/notification` | Notification delivery across channels |
| `eval` | `kernel/eval` | Evaluation: LLM output quality, work item assessment |

### Entry Points

| Crate | Path | Role |
|---|---|---|
| `gateway` | `kernel/gateway` | Agent gateway: HTTP API (85+ endpoints), lifecycle, SSE |
| `cli` | `kernel/cli` | `aman` CLI (HTTP REST / JSON-RPC / gRPC client) |
| `sdk` | `kernel/sdk` | Public SDK with prelude re-exports |
| `aman-tauri-lib` | `desktop` | Tauri v2 desktop application |

### Plugins (`kernel/plugins/`)

| Crate | Path | Role |
|---|---|---|
| `info-hub` | `kernel/plugins/info-hub` | Unified search across API, CLI, local DB |
| `memory-store` | `kernel/plugins/memory-store` | Memory storage backend |
| `messaging-core` | `kernel/plugins/messaging-core` | Shared messaging abstractions |
| `messaging-telegram` | `kernel/plugins/messaging-telegram` | Telegram bot integration |
| `messaging-slack` | `kernel/plugins/messaging-slack` | Slack bot integration |
| `messaging-discord` | `kernel/plugins/messaging-discord` | Discord bot integration |
| `messaging-matrix` | `kernel/plugins/messaging-matrix` | Matrix bot integration |

> **Note**: The `llm-provider-openai` plugin has been consolidated into `cognitive-llm` (OpenAI provider + `LlmOpenaiTool`). The `llm-api` crate has been merged into `cognitive-llm::simple`.

### Dev Tooling

| Crate | Path | Role |
|---|---|---|
| `test-utils` | `kernel/test-utils` | Shared test helpers, fixtures, mock factories |

## Data Flow

```
Source Event
    │
    ▼
Event Bus ────▶ WAL (Persistent mode)
    │
    ▼
Dispatcher ────▶ Pipeline (Filter → Transform → Action)
    │                 │
    │                 ▼
    │              Output Event ──▶ Event Bus (recursive)
    │                 │
    │                 ▼
    │              DLQ (on failure)
    │
    ▼
Skill (on trigger match)
    │
    ▼
Tool Runner (file | http | exec | db)
    │
    ▼
ActionResult ──▶ Event Bus (response events)

Cognitive Engine Flow:
  EventBus → Observation → CognitiveEngine::process() → Decision → EventBus
    │                                                        │
    │  UserMessage, ToolCompleted,              Reply, CallTools,
    │  TimerFired, SystemEvent                  Delegate, WaitFor
    │
    └── LlmCognitiveEngine internally runs: ReAct loop → LlmProvider::chat_completion()

Workflow Engine
  ─ handles events for state transitions
  ─ manages timeouts and error recovery
  ─ triggers Pipeline actions on transitions
```

## Key Design Decisions

1. **Event-driven**: All components communicate through typed events, not direct method calls
2. **Layered backpressure**: 5 levels from rate reduction to disk overflow to full stop
3. **Compensation-first**: Every Pipeline step can define an inverse operation for rollback
4. **SOUL identity**: Agent personality and boundaries are enforced at runtime through SOUL.md
5. **Plugin isolation**: Three modes (in-process, subprocess, WASM) for different trust levels
6. **Crash recovery**: WAL + overflow directory scanning ensures at-least-once delivery
7. **Cognitive Engine decoupling**: The `CognitiveEngine` trait separates the agent "brain" from the event infrastructure. Today's LLM-based engine (`LlmCognitiveEngine`) can be swapped for a world model or hybrid engine without changing the gateway.

For detailed design rationale, see [agent-design.md](docs/agent-design.md) and [architect-design.md](docs/architect-design.md).
