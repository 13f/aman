# aman

> a man. an agent man.


[![CI](https://github.com/jerin/aman/actions/workflows/ci.yml/badge.svg)](https://github.com/jerin/aman/actions/workflows/ci.yml)

> ⚠️ **This project has no Meme / Token / Coin. Beware of scams.**

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

aman is built around an **event-driven architecture**:

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
11. **Agent Harness** is the ReAct (Think-Act-Observe) loop engine that orchestrates LLM calls, tool execution, and result feedback with streaming SSE support, token budget management, context assembly, and multi-turn iterations

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
│  fwatch) │    │  + Backpressure  │    └──┬──────┬───┘      └────────┬─────────┘
└──────────┘    │  + Dedup         │       │      │                   │
                │  + Retry Queue   │       │      ▼                   ▼
                └──────────────────┘       │  ┌──────────────┐  ┌──────────┐
                                           │  │  Pipeline    │  │  Notif.  │
                ┌──────────────┐            │  │  Engine      │  │  Store   │
                │  Workflow    │◀───────────│  │  (Filter →   │  └────┬─────┘
                │  Engine      │            │  │   Transform →│       │
                │  (state      │            │  │   Action)    │       │
                │   machine)   │            │  └──────┬───────┘       │
                └──────────────┘            │         │               │
                                           │  ┌──────┴───────┐      │
                                           │  │  DLQ / Retry │      │
                                           │  └──────────────┘      │
                                           │                        │
                ┌──────────────────┐        │                        │
                │  AgentHarness    │◀───────┘                        │
                │  (ReAct Loop)    │                                │
                │                  │                                │
                │  ┌─ LLM Call ──┐ │                                │
                │  │ Tool Exec   │ │                                │
                │  │ Context Asm │ │                                │
                │  │ Token Budget│ │                                │
                │  │ Interrupt   │ │                                │
                │  └─────────────┘ │                                │
                └────────┬─────────┘                                │
                         │                                         │
                         ▼                                         ▼
┌──────────────┐    ┌──────────────┐    ┌──────────────┐      ┌──────────────┐
│  Plugins     │    │  Tool Runner │    │  Config      │      │  HTTP API    │
│  (.wasm/so)  │    │  (built-in:  │    │  + Secret    │      │  (Gateway)   │
│              │    │   file, http,│    │  Resolver    │      └──────┬───────┘
│              │    │   exec, db)  │    │  + Secure    │             │
│              │    │   web_search │    │    Cache     │             ▼
└──────────────┘    └──────────────┘    │  (mlock+     │      ┌──────────────┐
                                        │   zeroize)   │      │  Tauri       │
                                        └──────────────┘      │  Desktop     │
                                                              │  (bell +     │
                                                              │   overlay)   │
                                                              └──────────────┘
```

## Secret Security

API keys and credentials are stored in the OS-native credential store (macOS Keychain, Windows Credential Manager, Linux Secret Service) via the `keyring` crate. Values are read **once at first access** and cached in secure memory for the application lifetime:

- **`mlock(2)`** via the `secrets` crate locks cached secrets in physical RAM, preventing page-out to swap (disk)
- **`zeroize`** on drop automatically zeroes memory when cache entries are removed or the process exits
- **Guard pages and underflow canaries** detect out-of-bounds access to secret memory regions
- **Core dump exclusion** is enabled by default in release builds

```
┌─────────────────────────────────────────────────────┐
│  Process Memory                                      │
│                                                      │
│  ┌──────────┐    first read     ┌─────────────────┐  │
│  │ Keychain  │ ────────────────▶│  SecretVec<u8>   │  │
│  │ (macOS /  │                  │  (mlocked,       │  │
│  │  Windows/ │ ◀────────────────│   zeroized on    │  │
│  │  Linux)   │    subsequent    │   drop/remove)   │  │
│  │           │    reads from    └─────────────────┘  │
│  │           │    cache only                         │
│  └──────────┘                                        │
└─────────────────────────────────────────────────────┘
```

## Agent Harness

Agent Harness is the runtime engine that connects aman's event infrastructure to LLM agent behaviors:

**ReAct Loop** — Think-Act-Observe iteration:
1. Assemble context (SOUL system prompt + conversation history + tool schemas + memory)
2. Call LLM with tools parameter for structured function calling
3. If the LLM returns tool calls → execute tools → append results → loop back to step 2
4. If the LLM returns text → deliver final reply and exit

**Streaming** — SSE-based real-time output with `agent:reply_stream_start`, `agent:reply_chunk`, `agent:reply_stream_done` events forwarded to the frontend via EventBus

**Tool Execution** — Async tool dispatch with per-agent permission checks, lifecycle events (`tool:dispatched` / `tool:completed` / `tool:failed`), and result feedback into the conversation

**Token Budget** — Model-aware budget tracking with automatic history compression when approaching limits

**Interrupt Support** — Per-session interrupt flags for `/stop` support via a `STOP_GENERATION` event

**Key Events** — `agent:reply_chunk` (delta text), `agent:reply_ready` (full reply), `tool:dispatched/completed/failed`, `agent:reply_stream_error`, `agent:history_compressed`

## Project Status

v0.1.alpha.10 — Agent Harness & Secret Security

Agent Harness with full ReAct loop (Think-Act-Observe), streaming SSE support, tool dispatch/completion events, token budget management, history compression, and interrupt handling. Process-wide secret cache using `mlock(2)` and auto `zeroize` via the `secrets` crate — keys are read once from the system keychain and stored in non-swappable protected memory.

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

[AGPL-3.0](LICENSE)
