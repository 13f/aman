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

> Secrets cached in memory are locked with `mlock(2)` and auto-zeroed on
> drop — they never touch swap. In `keyring` mode, the OS keychain is read
> once at startup and the values stay in protected memory for the process
> lifetime.

## Core Concepts

- **Events** — typed JSON payloads with trace IDs flow from Sources through
  the Event Bus to Dispatcher, Pipelines, Skills, and Workflows.
- **Agent Harness** — ReAct loop (Think → Act → Observe) orchestrating
  LLM calls, tool execution, streaming SSE, and token budget management.
- **Skills & Plugins** — YAML-defined capabilities triggered by event
  patterns; WASM/subprocess plugins extend the runtime.
- **Tauri Desktop App** — cross-platform GUI for chat, dashboard,
  workflows, plugins, and settings. Manages the gateway process.
- **DLQ & Retry** — failed events go to a dead-letter queue for manual
  or automated retry with full trace context.

## Security

A defense-in-depth architecture covering input, execution, output, and data layers.
See [docs/security-harness.md](docs/security-harness.md) for the full catalog.

| Layer | Mechanisms |
|-------|-----------|
| **Input** | Three-tier prompt-injection sanitization (block / replace-msg / replace-token); trust-level gates (Trusted / Untrusted / Sandboxed); system prompt hardening |
| **Execution** | Hardline tool blocks — `rm -rf /`, fork bombs, raw disk writes, permission escalations, `DROP TABLE` — **not approvable**; user auth flow with 60s timeout + per-session cache |
| **Output** | Fail-closed validation on every LLM reply (secret leak, prompt leak, tool injection); 7-pattern log redaction on every line via `RedactWriter`; `#![forbid(unsafe_code)]` across 21+ crates |
| **Data** | Secrets: AES-256-GCM encrypted at rest, `mlock` + `zeroize` in memory, multi-backend resolution (env / keychain / 1Password / Vault / AWS). WAL with `fsync` + atomic writes, permission-restricted cache files (`0o600`/`0o700`) |
| **Infra** | 4-level event bus backpressure (DoS protection); event deduplication (Bloom + LRU); Bearer token auth + `x-aman-confirm` for destructive ops; config cross-reference validation at startup |

Each layer is **fail-closed**: timeouts, validation failures, and unrecognized
inputs all default to rejecting the action.

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

## Agent Harness

The ReAct loop (Think → Act → Observe) orchestrates LLM calls, tool execution,
and streaming responses. See [docs/harness.md](docs/harness.md) for the full
architecture — context assembly, token budgeting, SSE streaming, tool dispatch,
interrupt handling, and event lifecycle.


## License

[AGPL-3.0](LICENSE)
