# aman

> a man. an agent man.


[![CI](https://github.com/jerin/aman/actions/workflows/ci.yml/badge.svg)](https://github.com/jerin/aman/actions/workflows/ci.yml)

An event-driven agent framework for building safe, observable, and extensible autonomous systems.

## Quick Start

```bash
# Install the CLI
cargo install --path crates/cli

# Create a minimal config
cat > aman.yaml << 'EOF'
event_bus:
  mode: in_memory
  max_queue_size: 10000
EOF

# Run the agent
aman run --config aman.yaml
```

## Core Concepts

Aman is built around an **event-driven architecture**:

1. **Events** flow through the system as typed JSON payloads with trace IDs
2. **Event Sources** (cron, file watch, webhook, timer) produce events
3. **Dispatcher** routes events to **Pipelines** or **Skills** based on rules
4. **Pipelines** chain steps (Filter → Transform → Action) with built-in retry and compensation
5. **Skills** are YAML-defined capabilities triggered by event patterns, executing tools like HTTP requests, file operations, and shell commands
6. **Workflows** model long-running business processes as state machines with timeouts, guards, and error recovery
7. **Plugins** extend functionality through shared libraries or WASM modules
8. **DLQ** captures failed events for manual or automated retry
9. **Notification Center** transforms system events into user-facing alerts (critical/warning) with a ring-buffer store, HTTP API, and Tauri bridge for desktop overlay and bell widgets
10. **Tauri Desktop App** provides a cross-platform GUI with pages for dashboard, events, workflows, plugins, chat, and settings

## Architecture Overview

```
                     ┌──────────┐
                     │  Soul     │ (identity, boundaries, preferences)
                     └────┬─────┘
                          │ injects context
                          ▼
┌──────────┐    ┌──────────────────┐    ┌─────────────┐      ┌──────────────────┐
│ Sources  │───▶│  Event Bus       │───▶│ Dispatcher  │      │ Notification     │
│ (cron,   │    │  (InMemory/      │    │ (rules,     │      │ Subscriber       │
│  webhook,│    │   Persistent)    │    │  matching)  │      │ (critical/warn)  │
│  fwatch) │    │  + Backpressure  │    └──────┬──────┘      └────────┬─────────┘
└──────────┘    │  + Dedup         │           │                      │
                │  + Retry Queue   │           ▼                      ▼
                └──────────────────┘    ┌──────────────┐      ┌──────────────────┐
                                        │  Pipeline    │      │ Notification     │
                ┌──────────────┐        │  Engine      │      │ Store            │
                │  Workflow    │◀───────│  (Filter →   │      │ (ring buffer)    │
                │  Engine      │        │   Transform →│      └────────┬─────────┘
                │  (state      │        │   Action)    │               │
                │   machine)   │        └──────┬───────┘               │
                └──────────────┘               │                       ▼
                                        ┌──────┴───────┐      ┌──────────────────┐
                                        │  DLQ / Retry │      │  HTTP API        │
                                        └──────────────┘      │  (Gateway)       │
                                                              └────────┬─────────┘
                                                                       │
┌──────────────┐    ┌──────────────┐    ┌──────────────┐               ▼
│  Plugins     │    │  Tool Runner │    │  Config      │      ┌──────────────────┐
│  (.wasm/so)  │    │  (built-in:  │    │  + Secret    │      │  Tauri Desktop   │
│              │    │   file, http,│    │  Resolver    │      │  (bell + overlay)│
│              │    │   exec, db)  │    │              │      └──────────────────┘
└──────────────┘    └──────────────┘    └──────────────┘
```

## Project Status

v0.1.alpha.9 — Notification Center

notification center with two-tier alerts (critical/warning), Tauri overlay popup + sidebar bell widget, EventBus subscriber → ring buffer → HTTP API → Tauri bridge

v0.1.alpha.8 — Session Index & Native Tools

SQLite session index with paginated session list UI, native tool execution (file/http/exec/db), web search, Keychain-backed secret management

v0.1.alpha.7 — Skills 2.0

Complete skills system using skm-core & skm-select, JSON manifest with build script, third-party skill source tracking

v0.1.alpha.6 — Idle System

Two-axis depth-arousal idle model, sidebar visualization widget with tooltip, processing/reflection states

v0.1.alpha.5 — Events

Structured event system with comparison analysis, typed payloads, trace IDs

v0.1.alpha.4 — Multi-Agent & Profile

profile/data directory layout, multi-agent refactor

v0.1.alpha.3 — LLM Chat

chat with LLM, session management

v0.1.alpha.2 — Core Foundation

agent-design → architect-design → milestone

| Milestone | Status |
|---|---|
| M1 Foundation | ✅ Done |
| M2 Event Bus | ✅ Done |
| M3 Event Sources | ✅ Done |
| M4 Dispatcher + Pipeline | ✅ Done |
| M5 Skill + Tool | ✅ Done |
| M6 Workflow Engine | ✅ Done |
| M7 Plugin System | ✅ Done |
| M8 Persistence Layer | ✅ Done |
| M9 Security & Config | ✅ Done |
| M10 Runtime + API | ✅ Done |
| M11 Observability | ✅ Done |
| M12 Tauri Desktop | ✅ Done |
| M13 Integration & Polish | ✅ Done |

## License

MIT
