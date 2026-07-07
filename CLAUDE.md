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

# New experience crate
cargo test -p experience

# New analytics crate
cargo test -p analytics

# Info-hub plugin
cargo test -p info-hub
```

## Directory Structure

```
aman/
├── kernel/              ← infrastructure crates (event bus, dispatcher, pipeline, …)
│   ├── core/            ← package: kernel — core types, traits, error types, redactor
│   ├── analytics/       ← trend detection, anomaly alerting, time-series analysis
│   ├── event-bus/       ← InMemoryBus w/ backpressure
│   ├── gateway/         ← agent gateway daemon (binary: aman)
│   ├── plugins/         ← messaging, memory-store, info-hub
│   └── ...
├── cognitive/           ← cognitive engine abstraction
│   ├── engine/          ← CognitiveEngine trait (engine-agnostic)
│   ├── llm/             ← LlmCognitiveEngine + providers (OpenAI, Anthropic, Local)
│   └── react/           ← Shared ReAct types (ChatMessage, ReActTurn, etc.) — zero kernel deps
├── desktop/             ← Tauri v2 desktop app
└── docs/                ← design docs, architecture diagrams
```

## Codebase Architecture

Workspace with ~40 crates:

### Infrastructure Crates (`kernel/`)

| Crate | Path | Purpose |
|---|---|---|
| `kernel` | `kernel/core` | Kernel types: Event, Error, Pipeline, Schema, Retry, Tool traits, redactor |
| `analytics` | `kernel/analytics` | Trend detection + anomaly alerting on trace/session/audit data. HTTP `POST /analytics/analyze`, CLI `aman analyze trends\|anomalies` |
| `macros` | `kernel/macros` | Proc macros |
| `event-bus` | `kernel/event-bus` | InMemoryBus w/ 6-level backpressure (L1→L2→L3→L4A→L4B→Critical), overflow-to-disk, dedup |
| `dispatcher` | `kernel/dispatcher` | Event routing: EventBus → Pipeline/Skill dispatch |
| `pipeline` | `kernel/pipeline` | PipelineDefinition, PipelineEngine (Serial/Parallel/Limited) |
| `workflow` | `kernel/workflow` | State machine engine, timeouts, ERROR recovery, retry |
| `source` | `kernel/source` | Timer, Cron, FileWatch, Webhook, Signal, Socket |
| `hook` | `kernel/hook` | Internal hook system |
| `skill` | `kernel/skill` | YAML/SKILL.md loading, Tantivy search, hot-reload |
| `tool` | `kernel/tool` | Tool runner, built-in tools (file/http/exec/db/planner) |
| `plugin` | `kernel/plugin` | WASM/Subprocess/InProcess plugin host, dependency graph |
| `soul` | `kernel/soul` | SOUL identity system (SOUL.md parsing, boundary checks) |
| `persistence` | `kernel/persistence` | WAL, StateStore, DLQ, overflow dir |
| `secret` | `kernel/secret` | Multi-backend secrets, AES-256-GCM cache, rotation |
| `config` | `kernel/config` | 4-layer config loader, validation |
| `context-manager` | `kernel/context-manager` | Token budgeting, context compression/rotation (LLM-specific) |
| `experience` | `kernel/experience` | Agent experience: tool strategies, patterns, anti-patterns stored in `EXP.md`. Event-driven experience extraction from workflow completions. |
| `gateway` | `kernel/gateway` | Agent gateway daemon — binary name `aman`. Built around `Agenverse` (agents universe), the top-level container that owns lifecycle, HTTP server, and `AgentRuntime`.
| `cli` | `kernel/cli` | `aman-cli` CLI binary (HTTP REST / JSON-RPC / gRPC client to gateway) |
| `sdk` | `kernel/sdk` | Pub re-export crate for external devs |
| `skm-core-patched` | `kernel/skm-core-patched` | Patched fork of skill-manager core (Tantivy index fixes) |
| `sandbox` | `kernel/sandbox` | OS-level sandbox: Landlock + Seccomp-BPF (Linux), Seatbelt (macOS), Job Objects + AppContainer network isolation (Windows) |

### Cognitive Engine Crates (`cognitive/`)

| Crate | Path | Purpose |
|---|---|---|
| `cognitive-engine` | `cognitive/engine` | **CognitiveEngine trait** — engine-agnostic abstraction: Observation → Decision. Includes: Experience translator (`EXP.md` strategy assessment), Grounding translator (Knowledge × Situation signals), Decision confidence tracking. No LLM dependencies. |
| `cognitive-llm` | `cognitive/llm` | **LlmCognitiveEngine** — Full ReAct loop (LLM retry, tool execution, streaming, token tracking). Providers: `LlmOpenaiProvider`, `LlmAnthropicProvider`, `LlmLocalProvider`. Shared SSE/HTTP utilities in `shared.rs`. |
| `cognitive-react` | `cognitive/react` | **Shared ReAct types** — ChatMessage, ReActTurn, TokenBudget, etc. Zero kernel dependencies (leaf crate). Both `kernel` and `cognitive-llm` depend on it. |

`LlmCognitiveEngine::process()` is the primary code path. The gateway delegates
via `AgentHarness::process_message()` → `process_message_v2()`.
Old `LlmReActEngine` (in gateway) has been removed. See
`docs/react-migration-checklist.md` for the full migration history.

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

### Self Module (`predefined/self/` → `~/.aman/self/`)

Shared Python infrastructure for subprocess plugins. Synced to the user data
directory at gateway startup (hash-tracked, preserves user modifications).

| Module | Purpose |
|---|---|
| `self/prompts/` | Prompt builders: SOUL, skills index, tools, reflection extraction. `load_skill_prompt()` strips YAML frontmatter from SKILL.md files. |
| `self/decisions/` | Skill routing (`parse_skill_command`, `match_skill_prefix`) and complexity assessment. |
| `self/memory/` | Memory extraction strategies. |
| `self/evolution/` | Self-improvement: prompt mutation, variant tracking, self-audit. |
| `self/jsonrpc.py` | `Bridge` class — reusable bidirectional JSON-RPC 2.0 bridge over stdin/stdout. All subprocess plugins use this instead of duplicating bridge code. |
| `self/html_utils.py` | HTML escaping (`esc`, `esc_js`), response builders (`html_response`, `json_response`), template loading, static file serving, MIME types. |
| `self/llm.py` | `LlmClient` — calls `POST /tools/llm_chat/execute`. Plugins import `from self.llm import LlmClient`. |
| `self/bridge.py` | One-shot CLI bridge for the Rust gateway (`SelfBridge`) — prompt assembly without a persistent Python process. |

### Skills as SKILL.md

Skills are defined as markdown files under `predefined/skills/`. Each
skill is a directory with a `SKILL.md` containing YAML frontmatter
(`name`, `category`, `description`) and methodology instructions. The
agent harness executes them via `direct_act`. Startup validation skills
live under `predefined/skills/startup/`.

## Key Design Rules

- No unsafe code (`#![forbid(unsafe_code)]` in every crate)
- All config validation in `config::AgentConfig::validate()`
- Events flow: Source → EventBus → Dispatcher → Pipeline/Skill → Workflow
- **Cognitive flow**: EventBus → Observation → CognitiveEngine::process() → Decision → EventBus
- Agenverse lifecycle: Phase 0→5 (startup), Phase 5→0 (shutdown). Agenverse is the top-level container created first in `main.rs`; `AgentRuntime` is stored in it via `OnceLock`. Shutdown is orchestrated by `Agenverse::shutdown()` (publish event → acquire gate → runtime phases → server stop). TUI polls `agenverse.shutdown_requested()` directly.
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

## Agenverse Architecture

`Agenverse` (`kernel/gateway/src/runtime/agenverse.rs`) is the top-level container
that represents the "agents universe" — the world that exists independently of any
individual agent. Created first in `main.rs` via `Arc::new(Agenverse::new(...))`.

```
Agenverse {
    phase, status, shutdown_requested, shutdown_notify,     ← lifecycle state machine
    runtime: OnceLock<Arc<AgentRuntime>>,                   ← set after build
    server: Mutex<Option<HttpServerHandle>>,                ← set after serve
}
```

Key methods:
- `shutdown()` — full graceful shutdown orchestration (publish event → acquire gate → runtime phases → server stop). Idempotent.
- `shutdown_requested()` — polled by TUI to detect external shutdown (e.g. desktop app)
- `runtime()` / `set_runtime()` — access/store the built `AgentRuntime`
- `set_server_handle()` — store HTTP server handle for shutdown
- `agent_count()` — number of agents currently alive in the agenverse

The agenverse can run with zero agents — `build()` and `start()` succeed on
an empty `agents: {}` config. Agent lifecycle events (`agent:registered`,
`agent:removed`, `agent:reloaded`, `agent:status_changed`) flow through the
event bus owned by the agenverse.

## Cognitive Engine Architecture

The `CognitiveEngine` trait decouples the agent gateway from any specific model type.
`LlmCognitiveEngine::process()` runs a complete ReAct loop internally:

```
Gateway(AgentHarness)
  → process_message() → process_message_v2()
    → LlmCognitiveEngine::process(observations)
      ├── LLM call (retry 5×, exponential backoff)
      ├── OutputValidator + ContentFilter
      ├── If tool_calls: execute (parallel/serial, retry 3×, security)
      └── Loop until Finished or max_turns → return Decision::Reply
```

### LLM Provider Configuration

Set `llm.api_type` in config to choose the backend:

| `api_type` | Provider | Notes |
|---|---|---|
| `openai` (default) | `LlmOpenaiProvider` | OpenAI-compatible API |
| `anthropic` | `LlmAnthropicProvider` | Claude models via `/v1/messages` |
| `local` | `LlmLocalProvider` | Ollama/llama.cpp/vLLM (default `http://localhost:11434/v1`) |

The provider is resolved in `build_provider()` (`agent_runtime.rs`) and adapted
via `wrap_cognitive_provider()` to bridge `kernel::LlmProvider` ↔ `cognitive_llm::LlmProvider`.

### Cognitive Translation Layer

Translates system signals into the agent's "subjective experience" — not UI
metrics, but internal states that modulate behavior. Exposed as both
infrastructure (gateway auto-wiring) and **Tools** (callable from any skill):

#### Infrastructure (auto-wired at gateway startup)

| Translator | Input | Output | Behavior Modulation |
|---|---|---|---|
| **Consciousness** (`CognitiveState`) | LLM backend health | Lucid/Groggy/Catatonic/Coma | Catatonic/Coma → skip LLM; Groggy → reduce retries |
| **Grounding** (`Grounding`) | Memory retrieval + user message | Knowledge × Situation (2-axis) | Vague → force clarification; Overloaded → compress first |
| **Experience Extractor** (`ExperienceExtractor`) | workflow::completed events | Auto-write to EXP.md | Workflow outcomes → EXP entries |

#### Tools (registered via `install_cognitive_tools`, callable from SKILL.md)

| Tool | Function | When to Call |
|---|---|---|
| `assess-grounding` | Knowledge × Situation signal evaluation | Skill entry: decide if scout/clarify needed |
| `experience-recall` | Query EXP.md by task tag | Skill entry: check for past strategies/gotchas |
| `experience-record` | Write/update EXP.md entries | Skill exit: persist learnings |
| `check-consciousness` | Read current Lucid/Groggy/Catatonic/Coma state | Optional: information display |

#### Pipeline Orchestration

Complex cognitive flows are defined as YAML in `predefined/pipelines/` and
synced to `~/.aman/pipelines/` at startup (hash-based user modification
preservation, same pattern as skills/SOUL.md):

| Pipeline | Steps |
|---|---|
| `01-complex-plan.yaml` | scout → brainstorm → review → execute → extract-exp |

New pipelines: drop a YAML into `predefined/pipelines/`, register in
`kernel/pipeline/src/sync.rs`, rebuild.

Design: `docs/cognitive-memory.md` (based on 彭超's Agentic 之道).
Tools: `docs/dev-guide.md` §2.8.

### Security (all layers now active)

| Layer | Status |
|---|---|
| OutputValidator (secret leak, prompt leak, tool injection) | ✅ Wired in `LlmCognitiveEngine::process()` |
| ContentFilter (PII, API keys, credit cards) | ✅ Wired in `LlmCognitiveEngine::process()` |
| InputSanitizer + InjectionDetector | ✅ Wired in HTTP handler |
| SystemPromptHardener | ✅ Wired in `self_bridge.rs` |
| Hardline tool blocks (exec/file/db) | ✅ In `ToolExecutor` + `LlmCognitiveEngine` |
| PermissionReviewer (tool sensitivity) | ✅ In `ToolExecutor` |
| OS Sandbox (Landlock + Seccomp / Seatbelt / JobObjects+AppContainer) | ✅ In `SubprocessSandbox` + `apply_to_command()` |
| Capability-based access (ApprovalCache) | ✅ In plugin loader |

## Analytics Architecture

The `analytics` crate (`kernel/analytics/`) provides time-series analysis of agent
operational data. It consumes traces, sessions, and audit logs via an
`AnalyticsDataSource` trait (implemented by the gateway to avoid circular deps).

```rust
// kernel/analytics/src/lib.rs
#[async_trait]
pub trait AnalyticsEngine: Send + Sync {
    async fn analyze(&self, request: AnalysisRequest) -> AmanResult<AnalysisReport>;
}
```

- **Data sources**: `SqliteTraceStore` (time-range queries), `SessionStore`
  (session lifecycle), `AuditLogger` (operational events).
- **Trend detection**: Simple Moving Average crossover, OLS linear regression
  (R² confidence), rate-of-change (>20% threshold). Buckets auto-scale: hourly
  for ≤3-day windows, daily for longer.
- **Anomaly detection**: Z-score (|z| > 2.5 → Warning, > 3.5 → Critical),
  neighbor-median spike detection (3× multiplier), plus 4 predefined threshold
  rules (error rate > 50%, failure rate > 30%, tool failure > 25%, zero activity).
- **9 tracked metrics**: throughput, success rate, error rate, avg/P95 duration,
  tool failure rate, tool latency, session count, avg messages per session.
- **Time range**: Defaults to *today* (midnight UTC → now). Accepts ISO 8601,
  `"today"`, `"yesterday"`, `"now"` shortcuts.
- **Output**: `AnalysisReport` with `Vec<Trend>`, `Vec<Anomaly>`, and
  `ReportSummary` (total traces, success rate, avg/P95 duration, critical count).

### Integration

| Entry Point | Description |
|---|---|
| `POST /analytics/analyze` | HTTP endpoint accepting `AnalysisRequest` JSON → `AnalysisReport` JSON |
| `aman analyze trends\|anomalies` | CLI subcommand with `--from`/`--to`/`--agent` flags |
| `GatewayAnalyticsDataSource` | Bridges `AgentRuntime` stores to `AnalyticsDataSource` trait |

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
- `aman analyze trends|anomalies` → runs analytics engine, prints JSON report to stdout
- `metrics` supports `--format json` (only accepted value)
