# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Build & Test Commands

```bash
# Build all
cargo build --workspace

# Release build (all 21 crates)
cargo build --release --workspace

# Run all tests
cargo test --workspace

# Run single crate tests
cargo test -p workflow -p runtime

# Run a specific test
cargo test -p runtime --test e2e_integration workflow_error_retry_recovery -- --nocapture

# Run benchmark
cargo bench -p pipeline

# Lint
cargo clippy --workspace -- -D warnings

# Docs
cargo doc --workspace --no-deps

# CLI
cargo run --release --bin aman -- --help

# Fix clippy auto-fixes
cargo clippy --fix --workspace -- -D warnings

# Info-hub plugin
cargo test -p info-hub
```

## Codebase Architecture

Workspace with 21 crates under `crates/`:

| Crate | Purpose |
|---|---|
| `core` | Kernel types: Event, Error, Pipeline, Schema, Retry, Tool traits |
| `macros` | Proc macros |
| `event-bus` | InMemoryBus w/ 5-level backpressure, overflow-to-disk, dedup |
| `pipeline` | PipelineDefinition, PipelineEngine (Serial/Parallel/Limited) |
| `workflow` | State machine engine, timeouts, ERROR recovery, retry |
| `skill` | YAML/SKILL.md loading, Tantivy search, hot-reload |
| `tool` | Tool runner, built-in tools (file/http/exec/db) |
| `source` | Timer, Cron, FileWatch, Webhook, Signal, Socket |
| `plugin` | WASM/Subprocess/InProcess plugins, dependency graph |
| `info-hub` | 信息中心插件: unified search across API, CLI, local DB |
| `persistence` | WAL, StateStore, DLQ, overflow dir |
| `secret` | Multi-backend secrets, AES-256-GCM cache, rotation |
| `config` | 4-layer config loader, validation |
| `soul` | SOUL system prompt management |
| `runtime` | AgentRuntime, HTTP API (27 endpoints), lifecycle |
| `cli` | `aman` CLI binary (HTTP REST client to gateway) |
| `sdk` | Pub re-export crate for external devs |
| `tauri` | Tauri v2 desktop app |
| `hook`, `dispatcher` | (internal) |

## Key Design Rules

- No unsafe code (`#![forbid(unsafe_code)]` in every crate)
- All config validation in `config::AgentConfig::validate()`
- Events flow: Source → EventBus → Dispatcher → Pipeline/Skill → Workflow
- Runtime lifecycle: Phase 0→5 (startup), Phase 5→0 (shutdown)
- Error recovery in workflows: ERROR → RETRY event → last_active_state
- Backpressure: L1(80%)→L2(90%)→L3(95%)→L4A(98%/overflow)→L4B(critical)
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

`kernel::redactor` (crates/core/src/redactor.rs) provides:
- `redact_sensitive_data(input: &str) -> Cow<str>` — redacts API keys, tokens,
  passwords, JWTs, and Bearer headers from arbitrary text.
- `contains_sensitive_data(input: &str) -> bool` — fast-path pre-check.
- 7 pre-compiled regex patterns covering: OpenAI/Anthropic keys (`sk-...`),
  AWS keys (`AKIA...`), JWTs (`eyJ...`), Bearer tokens, `key=value` secrets,
  JSON field secrets, and env-var-style tokens.
- Tests in the same file serve as the canonical list of what gets redacted.

## CLI Architecture

The `aman` CLI (`crates/cli/`) supports three protocols for communicating with the gateway:

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
