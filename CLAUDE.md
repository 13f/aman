# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Build & Test Commands

```bash
# Build all
cargo build --workspace

# Release build (all ~40 crates)
cargo build --release --workspace

# Run all tests
cargo test --workspace

# Run single crate tests
cargo test -p workflow -p gateway

# Run a specific test
cargo test -p secret --test rotation_integration -- --nocapture

# Run benchmark
cargo bench -p pipeline

# Lint
cargo clippy --workspace -- -D warnings

# Docs
cargo doc --workspace --no-deps

# CLI (aman-cli — gateway client)
cargo run --release --bin aman-cli -- --help

# Gateway daemon
cargo run --release --bin aman -- --help

# Fix clippy auto-fixes
cargo clippy --fix --workspace -- -D warnings

# New cognitive engine crates
cargo test -p cognitive-engine -p cognitive-llm

# Info-hub plugin
cargo test -p info-hub
```

## Directory Structure

```
aman/
├── kernel/              ← infrastructure crates (event bus, dispatcher, pipeline, …)
│   ├── core/            ← package: kernel — core types, traits, error types, redactor
│   ├── event-bus/       ← InMemoryBus w/ backpressure
│   ├── gateway/         ← agent gateway daemon (binary: aman)
│   ├── plugins/         ← messaging, memory-store, info-hub
│   └── ...
├── cognitive/           ← cognitive engine abstraction (NEW)
│   ├── engine/          ← CognitiveEngine trait (engine-agnostic)
│   └── llm/             ← LlmCognitiveEngine (LLM-based implementation)
├── desktop/             ← Tauri v2 desktop app
└── docs/                ← design docs, architecture diagrams
```

## Codebase Architecture

Workspace with ~40 crates:

### Infrastructure Crates (`kernel/`)

| Crate | Path | Purpose |
|---|---|---|
| `kernel` | `kernel/core` | Kernel types: Event, Error, Pipeline, Schema, Retry, Tool traits, redactor |
| `macros` | `kernel/macros` | Proc macros |
| `event-bus` | `kernel/event-bus` | InMemoryBus w/ 6-level backpressure (L1→L2→L3→L4A→L4B→Critical), overflow-to-disk, dedup |
| `dispatcher` | `kernel/dispatcher` | Event routing: EventBus → Pipeline/Skill dispatch |
| `pipeline` | `kernel/pipeline` | PipelineDefinition, PipelineEngine (Serial/Parallel/Limited) |
| `workflow` | `kernel/workflow` | State machine engine, timeouts, ERROR recovery, retry |
| `source` | `kernel/source` | Timer, Cron, FileWatch, Webhook, Signal, Socket |
| `hook` | `kernel/hook` | Internal hook system |
| `skill` | `kernel/skill` | YAML/SKILL.md loading, Tantivy search, hot-reload |
| `tool` | `kernel/tool` | Tool runner, built-in tools (file/http/exec/db) |
| `plugin` | `kernel/plugin` | WASM/Subprocess/InProcess plugin host, dependency graph |
| `soul` | `kernel/soul` | SOUL identity system (SOUL.md parsing, boundary checks) |
| `persistence` | `kernel/persistence` | WAL, StateStore, DLQ, overflow dir |
| `secret` | `kernel/secret` | Multi-backend secrets, AES-256-GCM cache, rotation |
| `config` | `kernel/config` | 4-layer config loader, validation |
| `context-manager` | `kernel/context-manager` | Token budgeting, context compression/rotation (LLM-specific) |
| `gateway` | `kernel/gateway` | Agent gateway daemon — binary name `aman` |
| `cli` | `kernel/cli` | `aman-cli` CLI binary (HTTP REST / JSON-RPC / gRPC client to gateway) |
| `sdk` | `kernel/sdk` | Pub re-export crate for external devs |
| `skm-core-patched` | `kernel/skm-core-patched` | Patched fork of skill-manager core (Tantivy index fixes) |
| `sandbox` | `kernel/sandbox` | OS-level sandbox: Landlock (Linux), Seatbelt (macOS), Job Objects + AppContainer (Windows) |

### Cognitive Engine Crates (`cognitive/`)

| Crate | Path | Purpose |
|---|---|---|
| `cognitive-engine` | `cognitive/engine` | **CognitiveEngine trait** — engine-agnostic abstraction: Observation → Decision. No LLM dependencies. |
| `cognitive-llm` | `cognitive/llm` | **LlmCognitiveEngine** — LLM-based implementation. Consolidates: LlmProvider trait, ReAct engine, OpenAI provider, prompt pipeline, token budgeting, context management, simple HTTP client. Implements `CognitiveEngine`. |

### Agent Lifestyle Crates

| Crate | Path | Purpose |
|---|---|---|
| `lifecycle` | `kernel/lifecycle` | Agent lifecycle state machine (Phases 0→5 startup, 5→0 shutdown) |
| `idle` | `kernel/idle` | Idle mode: background observation, proactive suggestions |
| `daily-life` | `kernel/daily-life` | Daily-life automation: routines, schedules, habits |
| `work` | `kernel/work` | Work item processing: task intake, prioritization, execution |
| `study` | `kernel/study` | Study/learning system: knowledge acquisition, spaced repetition |
| `memory` | `kernel/memory` | Agent memory: episodic/semantic storage, retrieval |
| `notification` | `kernel/notification` | Notification delivery: push, email, messaging channels |
| `eval` | `kernel/eval` | Evaluation system: LLM output quality, work item assessment. Uses `cognitive-llm::simple` for LLM-as-Judge. |

### Plugin Crates (`kernel/plugins/`)

| Crate | Purpose |
|---|---|
| `info-hub` | 信息中心: unified search across API, CLI, local DB. Uses `cognitive-llm::simple` for AI features. |
| `memory-store` | Memory storage backend plugin |
| `messaging-core` | Shared messaging abstractions (routing, @mention, formatting) |
| `messaging-telegram` | Telegram bot integration |
| `messaging-slack` | Slack bot integration |
| `messaging-discord` | Discord bot integration |
| `messaging-matrix` | Matrix bot integration |

### Dev Tooling

| Crate | Path | Purpose |
|---|---|---|
| `test-utils` | `kernel/test-utils` | Shared test helpers, fixtures, mock factories |

## Key Design Rules

- No unsafe code (`#![forbid(unsafe_code)]` in every crate)
- All config validation in `config::AgentConfig::validate()`
- Events flow: Source → EventBus → Dispatcher → Pipeline/Skill → Workflow
- **Cognitive flow**: EventBus → Observation → CognitiveEngine::process() → Decision → EventBus
- Runtime lifecycle: Phase 0→5 (startup), Phase 5→0 (shutdown)
- Error recovery in workflows: ERROR → RETRY event → last_active_state
- Backpressure: L1(~81%)→L2(~90%)→L3(~96%)→L4A(~98%/overflow)→L4B→Critical(100%)
- API auth: Bearer token, x-aman-operator, x-aman-confirm for destructive ops
- **Log safety**: NEVER use raw `println!` or `eprintln!` — they are forbidden by
  workspace-level `clippy::print_stdout` / `clippy::print_stderr` lints. Use:
  - `tracing::info!` / `tracing::error!` / etc. — all output goes through
    `RedactWriter` which strips API keys, tokens, and passwords automatically.
  - `safe_println!` / `safe_eprintln!` from `kernel::redactor` — for CLI code
    where `tracing` is not wired up. These apply the same redaction rules.
  - Existing legitimate uses (build.rs, JSON-RPC protocol wire, pre-tracing
    startup errors) carry explicit `#[allow(clippy::print_stdout)]` /
    `#[allow(clippy::print_stderr)]` with a comment explaining WHY.

## Redaction Module

`kernel::redactor` (`kernel/core/src/redactor.rs`) provides:
- `redact_sensitive_data(input: &str) -> Cow<str>` — redacts API keys, tokens,
  passwords, JWTs, and Bearer headers from arbitrary text.
- `contains_sensitive_data(input: &str) -> bool` — fast-path pre-check.
- 7 pre-compiled regex patterns covering: OpenAI/Anthropic keys (`sk-...`),
  AWS keys (`AKIA...`), JWTs (`eyJ...`), Bearer tokens, `key=value` secrets,
  JSON field secrets, and env-var-style tokens.
- Tests in the same file serve as the canonical list of what gets redacted.

## Cognitive Engine Architecture

The `CognitiveEngine` trait decouples the agent gateway from any specific model type:

```rust
// cognitive/engine/src/lib.rs
pub trait CognitiveEngine: Send + Sync {
    fn name(&self) -> &str;
    async fn process(&self, ctx: &CognitiveContext, observations: Vec<Observation>)
        -> Result<Vec<Decision>, CognitiveError>;
    fn subscribe(&self, listener: Arc<dyn CognitiveListener>);
    fn unsubscribe(&self, listener: &Arc<dyn CognitiveListener>);
    async fn reset_session(&self, session_id: &str) -> Result<(), CognitiveError>;
}
```

- **`Observation`** — input from the event bus (user message, tool result, timer, system event…)
- **`Decision`** — output action (reply text, call tool, delegate, wait…)
- **`CognitiveContext`** — agent identity, capabilities, memory (engine-agnostic)
- **`LlmCognitiveEngine`** (`cognitive/llm/`) — current implementation: ReAct loop + OpenAI API
- Future engines (world model, hybrid) implement the same trait

## CLI Architecture

The `aman` CLI (`kernel/cli/`) supports three protocols for communicating with the gateway:

- **HTTP REST** (default) — via `reqwest`. Every subcommand talks to the gateway's HTTP API.
- **stdio JSON-RPC** — `aman serve` reads JSON-RPC 2.0 requests from stdin, writes responses to stdout. Used for MCP integration and subprocess invocation by AI hosts.
- **gRPC** — protobuf-based API via tonic. Enable with `--grpc` flag on any subcommand. Lower latency for event push, streaming chat replies, and high-throughput metrics.

All three protocols share the same `AgentRuntime` methods — no new business logic per protocol.

**Current output behavior:**
- Remote queries (metrics, audit-log, plugin list, etc.) → raw JSON response body to stdout
- Mutating commands (enable, delete, retry, etc.) → silent on success, body to stderr on error
- `config show` → pretty-printed JSON via `serde_json::to_string_pretty`
- `skill validate` / `skill export` → human-readable text (local, no gateway needed)
- `aman run` → prints bind address to stdout, then blocks
- `metrics` supports `--format json` (only accepted value)
