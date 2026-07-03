# aman

> a man. an agent man.


[![CI](https://github.com/jerin/aman/actions/workflows/ci.yml/badge.svg)](https://github.com/jerin/aman/actions/workflows/ci.yml)

> ⚠️ **This project has no Meme / Token / Coin. Beware of scams.**

> ⚠️ **ALPHA SOFTWARE — USE AT YOUR OWN RISK**
>
> Data structures and storage formats may change. See `SECURITY_HARNESS.md` for the
> full security architecture (input/output sanitization, content filtering, OS
> sandbox, and audit logging are all implemented and active).

An event-driven agent framework for building safe, observable, and extensible
autonomous systems. Supports OpenAI, Anthropic (Claude), and local models
(Ollama/llama.cpp/vLLM) via a unified LLM provider interface.

## System Dependencies

All platforms require the **Rust toolchain** ([rustup](https://rustup.rs)),
**Node.js & npm** (for frontend build steps in the gateway crate), and
**ripgrep** (`rg`) as a runtime dependency for the built-in grep tool.

Protobuf code generation uses `prost-build` (pure Rust) — **`protoc` is not
required**.

### Linux (Ubuntu/Debian)

```bash
# CLI-only build (gateway + cli)
sudo apt-get install -y build-essential pkg-config

# Full desktop build (Tauri + all crates) — adds GTK, WebKit, audio, keyring
sudo apt-get install -y \
  libwebkit2gtk-4.1-dev \
  libappindicator3-dev \
  libayatana-appindicator3-dev \
  librsvg2-dev \
  patchelf \
  libssl-dev \
  libdbus-1-dev \
  libasound2-dev
```

`libwebkit2gtk-4.1-dev` pulls in GTK 3, Cairo, Pango, GLib, Soup 3,
JavaScriptCore, and Wayland as transitive dependencies.

If you only need the CLI, build with `cargo build -p gateway -p cli` to skip
the desktop crate and avoid the Tauri/GTK dependency tree.

### macOS

```bash
# Install Xcode Command Line Tools (provides cc, clang, system frameworks)
xcode-select --install

# Runtime dependency
brew install ripgrep
```

No additional system packages are needed — WebKit, CoreAudio, Security
(Keychain), and other frameworks are provided by the OS SDK.

### Windows

1. Install **Visual Studio Build Tools** or **Visual Studio 2022** with the
   "Desktop development with C++" workload. This provides `cl.exe`, `link.exe`,
   and the Windows SDK.

2. **WebView2** is required for the Tauri desktop app. It is built into
   Windows 11 and Windows 10 (October 2018 update or later). For older
   Windows 10 builds, install the
   [WebView2 Runtime](https://developer.microsoft.com/microsoft-edge/webview2/).

3. Install **Node.js** from [nodejs.org](https://nodejs.org) and **ripgrep**
   via `winget install BurntSushi.ripgrep.MSVC` or `choco install ripgrep`.

4. Install Rust

5. Run script

```powershell
powershell -ExecutionPolicy Bypass -File .\scripts\install-gateway.ps1 -Release -Run
```

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

The script builds `aman` in release mode and installs it to
`~/.aman/bin/aman`. The Tauri app spawns it from that path when you
click "Start" in the Dashboard.

### Option B: CLI Only (no GUI)

> **Note**: The CLI binary is named `aman-cli` (to avoid conflicting with the
> gateway daemon binary `aman`). All examples in documentation use `aman` as a
> shorthand -- substitute `aman-cli` if you installed via `cargo install --path
> kernel/cli`.

```bash
# Install the CLI
cargo install --path kernel/cli

# Create a minimal config at ~/.aman/config.yaml
cat > ~/.aman/config.yaml << 'EOF'
security:
  secrets_mode: env
EOF

# Install and run the gateway directly
./scripts/install-gateway.sh --release
~/.aman/bin/aman
```

The gateway binary at `~/.aman/bin/aman` is the same process the
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

## Agent Configuration

aman supports multiple agents, each with its own provider, model, soul,
and optional subsystems. Configure them under the `agents` key in
`~/.aman/config.yaml`.

### Minimal multi-agent setup

```yaml
# ~/.aman/config.yaml
providers:
  openai:
    display_name: OpenAI
    base_url: https://api.openai.com/v1
  deepseek:
    display_name: DeepSeek
    base_url: https://api.deepseek.com/v1

model:
  default: gpt-5
  provider: openai

agents:
  health:
    display_name: Health
    provider: deepseek
    model: deepseek-v4-flash
```

Each agent stores its data under `~/.aman/agents/{agent_id}/`:
```
~/.aman/agents/health/
├── SOUL.md          # agent identity, boundaries, personality
├── memory/          # long-term memory (YantrikDB)
├── sessions/        # chat session JSONL files
├── sessions.db      # session metadata (SQLite)
├── traces/          # task execution traces
└── emotions/        # (optional) visual emotion images
    ├── data.json    # emotion definitions
    ├── calm.png
    ├── happy.png
    └── ...
```

### Emotion System

Agents can display visual emotion images instead of Unicode emojis.
Place a valid `emotions/` directory under the agent's data directory
with a `data.json` and matching image files.

**Desktop UI behaviour:**
- If `emotions/` is present and valid → displays images for each agent state
- If `emotions/` is missing or invalid → falls back to emoji display

**LLM-driven emotion evaluation** (optional):
When `emotion` is configured at the top level, the gateway periodically
evaluates each agent's emotional state using an LLM. It collects recent
session messages and trace records, then picks the best-matching emotion
from the agent's `emotions/data.json`.

```yaml
# ~/.aman/config.yaml
emotion:
  enabled: true
  provider: deepseek          # provider for emotion classification
  model: deepseek-v4-flash    # cheap/fast model recommended
  interval_secs: 45           # how often to re-evaluate
  temperature: 0.3            # low for consistent results
  max_context_messages: 10    # recent messages to include
```

**Gating**: the emotion evaluator only starts for agents that have a
valid `emotions/` directory. If `data.json` is missing, unparseable,
or any referenced image file is absent, the evaluator is silently
skipped and the UI falls back to the state-based emoji mapping.

### Emotion data.json format

```json
{
  "img_ext": "png",
  "items": [
    { "id": "happy",   "tags": ["愉悦", "开心", "happy"],   "description": "微笑，眼睛微弯" },
    { "id": "focused", "tags": ["专注", "focused"],         "description": "眼神集中，眉头微压" },
    { "id": "working", "tags": ["工作中", "working"],       "description": "认真、略带严肃" },
    { "id": "sleeping","tags": ["睡觉", "sleeping"],        "description": "双眼紧闭，眉毛舒展" }
  ]
}
```

Each `id` must have a corresponding image file: `{id}.{img_ext}`
(e.g. `happy.png`). The evaluator and UI both validate this at startup.

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

A defense-in-depth architecture covering input, execution, output, data, and plugin isolation layers.
See [SECURITY_HARNESS.md](SECURITY_HARNESS.md) for the full security catalog.

| Layer | Mechanisms |
|-------|-----------|
| **Plugin Sandbox** | 4-layer isolation: WASM fuel metering (100M instructions) + epoch interruption; OS-level subprocess sandbox (Landlock on Linux, Seatbelt on macOS, Job Objects + AppContainer on Windows); capability-based access control with first-time approval + auto-approval on reload; event bus rate limiting + trust-level enforcement (Sandboxed sources blocked from publishing sensitive event types) |
| **Input** | Three-tier prompt-injection sanitization (block / replace-msg / replace-token); trust-level gates (Trusted / Untrusted / Sandboxed); system prompt hardening |
| **Execution** | Hardline tool blocks — `rm -rf /`, fork bombs, raw disk writes, permission escalations, `DROP TABLE` — **not approvable**; user auth flow with 60s timeout + per-session cache |
| **Output** | Fail-closed validation on every LLM reply (secret leak, prompt leak, tool injection); 7-pattern log redaction on every line via `RedactWriter`; `#![deny(unsafe_code)]` across 21+ crates |
| **Data** | Secrets: AES-256-GCM encrypted at rest, `mlock` + `zeroize` in memory, multi-backend resolution (env / keychain / 1Password / Vault / AWS). WAL with `fsync` + atomic writes, permission-restricted cache files (`0o600`/`0o700`) |
| **Infra** | 5-level event bus backpressure (DoS protection); event deduplication (Bloom + LRU); Bearer token auth + `x-aman-confirm` for destructive ops; config cross-reference validation at startup |

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

## Cognitive Engine

The `CognitiveEngine` trait (`cognitive/engine/`) defines the agent's "brain" contract:
Observation → Decision. The current LLM-based implementation (`cognitive/llm/`)
wraps a ReAct loop (Think → Act → Observe) orchestrating LLM calls, tool execution,
streaming responses, and token budget management. Future engines (world model, hybrid)
implement the same trait.

See [docs/harness.md](docs/harness.md) for the full ReAct loop architecture.


## License

[AGPL-3.0](LICENSE)
