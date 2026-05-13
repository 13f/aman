# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Build & Test Commands

```bash
# Build all
cargo build --workspace

# Release build (all 20 crates)
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
```

## Codebase Architecture

Workspace with 20 crates under `crates/`:

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
| `persistence` | WAL, StateStore, DLQ, overflow dir |
| `secret` | Multi-backend secrets, AES-256-GCM cache, rotation |
| `config` | 4-layer config loader, validation |
| `soul` | SOUL system prompt management |
| `runtime` | AgentRuntime, HTTP API (27 endpoints), lifecycle |
| `cli` | `aman` CLI binary |
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
