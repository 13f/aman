# aman

> a man. an agent man.


[![CI](https://github.com/jerin/aman/actions/workflows/ci.yml/badge.svg)](https://github.com/jerin/aman/actions/workflows/ci.yml)

> ⚠️ **This project has no Meme / Token / Coin. Beware of scams.**

> ⚠️ **WORK IN PROGRESS — USE AT YOUR OWN RISK**
>
> - Data structures, storage formats, and policies are **not yet finalized** and may change without notice.
> - Critical harness features — **sandboxing, input/output sanitization, permission review, content filtering, and audit logging** — are **not yet implemented**. The agent may execute arbitrary actions without guardrails.

An event-driven agent framework for building safe, observable, and extensible autonomous systems.

## Quick Start

### Option A: Tauri Desktop App

```bash
# Build and install the gateway binary
./scripts/install-gateway.sh

# Build and run the desktop app
cargo tauri dev    # dev mode
cargo tauri build  # production bundle (.app / .dmg)
```

**macOS note**: If you see a permission error when running
`install-gateway.sh`, make it executable first:

```bash
chmod +x scripts/install-gateway.sh
```

The script builds `gateway` in release mode and installs it to
`~/.aman/bin/gateway`. The Tauri app spawns it from that path when you
click "Start" in the Dashboard.

### Option B: CLI Only (no GUI)

```bash
# Install the CLI
cargo install --path crates/cli

# Create a minimal config at ~/.aman/config.yaml
cat > ~/.aman/config.yaml << 'EOF'
security:
  secrets_mode: env
EOF

# Install and run the gateway directly
./scripts/install-gateway.sh --release
~/.aman/bin/gateway
```

The gateway binary at `~/.aman/bin/gateway` is the same process the
Tauri app manages — it's the complete agent runtime with HTTP API,
WebSocket, gRPC, LLM orchestration, and all plugins. You can run it
standalone without the GUI.

## Secrets Configuration

aman supports two modes for storing API keys, tokens, and credentials.

### `secrets_mode: keyring` (recommended for desktop)

Keys are stored in the OS-native credential store — macOS Keychain,
Windows Credential Manager, or Linux Secret Service. Use the **Tauri
desktop app** to manage keys via the **Integration** page (3rd-party
service keys and IM channel bot tokens), or the **Providers** page
(LLM provider API keys).

```yaml
# ~/.aman/config.yaml
security:
  secrets_mode: keyring
```

The Integration navigation item is visible in the Tauri app sidebar
when this mode is active (hidden when `secrets_mode: env`).

### `secrets_mode: env` (recommended for servers / CLI)

All secrets are read from environment variables. No keychain prompts,
no GUI needed. Set these variables in your shell profile or systemd
service file.

**LLM Providers** — set one per provider you configure:

```bash
export AMAN_PROVIDER_OPENAI_API_KEY="sk-..."
export AMAN_PROVIDER_ANTHROPIC_API_KEY="sk-ant-..."
export AMAN_PROVIDER_DEEPSEEK_API_KEY="sk-..."
```

The naming convention is `AMAN_PROVIDER_{NAME}_API_KEY` where `{NAME}`
is the provider key in your `config.yaml`, uppercased with non‑alphanumeric
characters replaced by `_`.

**IM Channels** — optional, for Telegram / Slack / Discord / Matrix bots:

```bash
# Telegram (default instance)
export AMAN_BOT_TELEGRAM_TOKEN="123456:ABC..."
export AMAN_BOT_TELEGRAM_USERNAME="my_bot"
export AMAN_BOT_TELEGRAM_ALLOWED_CHAT_IDS="111,222"

# Telegram (named instances)
export AMAN_BOT_TELEGRAM_WORK_TOKEN="..."
export AMAN_BOT_TELEGRAM_PERSONAL_TOKEN="..."

# Slack
export AMAN_BOT_SLACK_BOT_TOKEN="xoxb-..."
export AMAN_BOT_SLACK_APP_TOKEN="xapp-..."

# Discord
export AMAN_BOT_DISCORD_TOKEN="..."

# Matrix
export AMAN_BOT_MATRIX_HOMESERVER_URL="https://matrix.org"
export AMAN_BOT_MATRIX_ACCESS_TOKEN="syt_..."
```

**Config file**:

```yaml
# ~/.aman/config.yaml
security:
  secrets_mode: env
```

When `secrets_mode: env`, the Tauri app hides the **Integration**
sidebar item because keychain writes would be ignored at runtime.

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
