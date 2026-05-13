# Architecture Overview

Aman is an event-driven agent framework composed of 19+ Rust crates organized into a layered architecture.

## Layer Diagram

```
┌─────────────────────────────────────────────────────┐
│                   Application Layer                  │
│   CLI (aman)    Tauri Desktop    HTTP API (axum)    │
├─────────────────────────────────────────────────────┤
│                    Runtime Layer                     │
│   AgentRuntime    Phase Manager    SecretResolver   │
│   Health Check    Drain Handler    Config Loader    │
├─────────────────────────────────────────────────────┤
│                 Orchestration Layer                  │
│   Dispatcher    Pipeline Engine    Workflow Engine  │
│   Route Rules   Step Executor     State Machine    │
│   Fan Out       Compensation      Timeout Manager  │
├─────────────────────────────────────────────────────┤
│                 Capability Layer                     │
│   Skill Registry   Tool Runner    Plugin Loader    │
│   Skill Search     Built-in Tools  WASM Runtime    │
│   Hot Reload       Sandbox         SOUL System     │
├─────────────────────────────────────────────────────┤
│                  Event Bus Layer                     │
│   InMemoryBus     PersistentBus   Backpressure     │
│   Ordered Queue   Dedup Window    Retry Queue      │
│   Overflow Disk   Subscription    Bus Metrics      │
├─────────────────────────────────────────────────────┤
│                Event Source Layer                    │
│   SourceRegistry  TimerSource     CronSource       │
│   FileWatch       WebhookSource   SignalSource     │
│   SocketSource                                    │
├─────────────────────────────────────────────────────┤
│                  Persistence Layer                   │
│   Write Ahead Log   StateStore    Dead Letter Queue│
│   Checkpoint        SledStore     Overflow Mgmt     │
├─────────────────────────────────────────────────────┤
│                  Core Layer                          │
│   Kernel (types)  Macros (proc)  Config (schema)   │
│   Error types     Traits         Security          │
└─────────────────────────────────────────────────────┘
```

## Crate Map

| Crate | Path | Role |
|---|---|---|
| `kernel` | `crates/core` | Core types, traits, error types |
| `macros` | `crates/macros` | `#[skill]`, `#[plugin]` proc macros |
| `event-bus` | `crates/event-bus` | Event bus with backpressure, dedup, ordering |
| `dispatcher` | `crates/dispatcher` | Event routing and rule matching |
| `pipeline` | `crates/pipeline` | Pipeline step execution and compensation |
| `skill` | `crates/skill` | Skill registry, search, hot reload, versions |
| `tool` | `crates/tool` | Tool runner with built-in tools (file, http, exec, db) |
| `workflow` | `crates/workflow` | State machine engine with timeouts and recovery |
| `source` | `crates/source` | Event sources (timer, cron, filewatch, webhook, signal, socket) |
| `plugin` | `crates/plugin` | Plugin system (loader, lifecycle, isolation) |
| `soul` | `crates/soul` | SOUL identity system (boundaries, preferences) |
| `hook` | `crates/hook` | Hook system for lifecycle events |
| `persistence` | `crates/persistence` | WAL, StateStore, DLQ, overflow management |
| `secret` | `crates/secret` | Secret resolution with multiple backends |
| `config` | `crates/config` | Multi-layer configuration loading |
| `runtime` | `crates/runtime` | Agent runtime lifecycle and orchestration |
| `cli` | `crates/cli` | Command-line interface |
| `sdk` | `crates/sdk` | Public SDK with prelude re-exports |
| `tauri` | `crates/tauri` | Tauri v2 desktop application |

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
