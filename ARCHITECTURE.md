# Architecture Overview

aman is an event-driven agent framework composed of 38 Rust crates (30 core + 8 plugins) organized into a layered architecture.

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
│   LLM Provider (trait)                                   │
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

Plugin crates live under `crates/plugins/` (LLM providers, messaging channels, memory stores) and are loaded dynamically at runtime.
```

## Crate Map

### Core & Infrastructure

| Crate | Path | Role |
|---|---|---|
| `kernel` | `crates/core` | Core types, traits, error types, redactor |
| `macros` | `crates/macros` | `#[skill]`, `#[plugin]` proc macros |
| `config` | `crates/config` | 4-layer configuration loading, validation |
| `secret` | `crates/secret` | Multi-backend secrets, AES-256-GCM cache, rotation |
| `hook` | `crates/hook` | Internal hook system for lifecycle events |
| `persistence` | `crates/persistence` | WAL, StateStore, DLQ, overflow management |
| `skm-core-patched` | `crates/skm-core-patched` | Patched fork of skill-manager core (Tantivy fixes) |

### Event System

| Crate | Path | Role |
|---|---|---|
| `event-bus` | `crates/event-bus` | Event bus with 5-level backpressure, dedup, ordering |
| `source` | `crates/source` | Event sources (timer, cron, filewatch, webhook, signal, socket) |
| `dispatcher` | `crates/dispatcher` | Event routing and rule matching |

### Orchestration

| Crate | Path | Role |
|---|---|---|
| `pipeline` | `crates/pipeline` | Pipeline step execution and compensation |
| `workflow` | `crates/workflow` | State machine engine with timeouts and recovery |

### Capability

| Crate | Path | Role |
|---|---|---|
| `skill` | `crates/skill` | Skill registry, Tantivy search, hot reload, versions |
| `tool` | `crates/tool` | Tool runner with built-in tools (file, http, exec, db) |
| `plugin` | `crates/plugin` | Plugin host (loader, lifecycle, WASM/subprocess/in-process) |
| `llm-api` | `crates/llm-api` | LLM provider abstraction (trait-based, swappable backend) |
| `soul` | `crates/soul` | SOUL identity system (boundaries, preferences, hot-reload) |

### Agent Lifestyle

| Crate | Path | Role |
|---|---|---|
| `lifecycle` | `crates/lifecycle` | Agent lifecycle state machine (Phases 0→5, 5→0) |
| `idle` | `crates/idle` | Idle mode: background observation, proactive suggestions |
| `daily-life` | `crates/daily-life` | Daily-life automation: routines, schedules, habits |
| `work` | `crates/work` | Work item processing: intake, prioritization, execution |
| `study` | `crates/study` | Study/learning system: knowledge acquisition |
| `memory` | `crates/memory` | Agent memory: episodic/semantic storage, retrieval |
| `notification` | `crates/notification` | Notification delivery across channels |
| `eval` | `crates/eval` | Evaluation: LLM output quality, work item assessment |

### Entry Points

| Crate | Path | Role |
|---|---|---|
| `gateway` | `crates/gateway` | Agent gateway: HTTP API (85+ endpoints), lifecycle, SSE (replaced `runtime`) |
| `cli` | `crates/cli` | `aman` CLI (HTTP REST / JSON-RPC / gRPC client) |
| `sdk` | `crates/sdk` | Public SDK with prelude re-exports |
| `tauri` | `crates/tauri` | Tauri v2 desktop application |

### Plugins (`crates/plugins/`)

| Crate | Path | Role |
|---|---|---|
| `info-hub` | `crates/plugins/info-hub` | Unified search across API, CLI, local DB |
| `llm-provider-openai` | `crates/plugins/llm-provider-openai` | OpenAI-compatible LLM provider |
| `memory-store` | `crates/plugins/memory-store` | Memory storage backend |
| `messaging-core` | `crates/plugins/messaging-core` | Shared messaging abstractions |
| `messaging-telegram` | `crates/plugins/messaging-telegram` | Telegram bot integration |
| `messaging-slack` | `crates/plugins/messaging-slack` | Slack bot integration |
| `messaging-discord` | `crates/plugins/messaging-discord` | Discord bot integration |
| `messaging-matrix` | `crates/plugins/messaging-matrix` | Matrix bot integration |

### Dev Tooling

| Crate | Path | Role |
|---|---|---|
| `test-utils` | `crates/test-utils` | Shared test helpers, fixtures, mock factories |

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

For detailed design rationale, see [agent-design.md](docs/agent-design.md) and [architect-design.md](docs/architect-design.md).
