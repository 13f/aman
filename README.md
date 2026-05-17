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

## Architecture Overview

```
                     ┌──────────┐
                     │  Soul     │ (identity, boundaries, preferences)
                     └────┬─────┘
                          │ injects context
                          ▼
┌──────────┐    ┌──────────────────┐    ┌─────────────┐
│ Sources  │───▶│  Event Bus       │───▶│ Dispatcher  │
│ (cron,   │    │  (InMemory/      │    │ (rules,     │
│  webhook,│    │   Persistent)    │    │  matching)  │
│  fwatch) │    │  + Backpressure  │    └──────┬──────┘
└──────────┘    │  + Dedup         │           │
                │  + Retry Queue   │           ▼
                └──────────────────┘    ┌──────────────┐
                                        │  Pipeline    │
                ┌──────────────┐        │  Engine      │
                │  Workflow    │◀───────│  (Filter →   │
                │  Engine      │        │   Transform →│
                │  (state      │        │   Action)    │
                │   machine)   │        └──────┬───────┘
                └──────────────┘               │
                                        ┌──────┴───────┐
                                        │  DLQ / Retry  │
                                        └──────────────┘

┌──────────────┐    ┌──────────────┐    ┌──────────────┐
│  Plugins     │    │  Tool Runner │    │  Config      │
│  (.wasm/so)  │    │  (built-in:  │    │  + Secret    │
│              │    │   file, http,│    │  Resolver    │
│              │    │   exec, db)  │    │              │
└──────────────┘    └──────────────┘    └──────────────┘
```

## Project Status

v0.1.alpha.6

complete skills using skm-core & skm-select

skills-iteration.md

v0.1.alpha.5

complete & visualize idle

idle-design -> idle-milestones

v0.1.alpha.4

complete events

events-comparison -> events -> events-milestones

v0.1.alpha.3

profile/data directory

multi-agents-refactor

v0.1.alpha.2

chat with LLM

llm-chat-design -> llm-chat-architect -> llm-chat-milestones

v0.1.alpha.1

Core

agent-design -> architect-design -> milestone

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
