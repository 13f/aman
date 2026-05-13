# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.1.0] - 2026-05-13

### Added

- Initial release of Aman Agent Framework
- Core type system: Event, EventType, Priority, DeliveryGuarantee, Timestamp, TraceId
- Event bus with 5-level backpressure, dedup window, per-source ordering, retry queue
- Event sources: Timer, Cron, FileWatch, Webhook, Signal, Socket
- Dispatcher + Pipeline engine with Serial/Parallel/Limited concurrency models
- Pipeline compensation engine (reverse-order, timeout-protected, retry-aware)
- Skill system with YAML/SKILL.md loading, Tantivy full-text search, hot-reload, version management
- Tool runner with 6-step execution pipeline (validate → sandbox → execute → cleanup)
- 4 built-in tools: file, http, exec, db (with sandbox constraints)
- Workflow state machine engine with timeout, ERROR recovery, retry, archiving
- Plugin system with dependency graph, topological load, 3 isolation modes (InProcess, Subprocess, Wasm)
- SOUL system prompt management with hot-reload and boundary checking
- Persistence layer: WAL (append/checkpoint/replay/rotate), Sled StateStore, DLQ, overflow
- Secret management: multi-backend (Env/Vault/AWS/1Password), AES-256-GCM cache, two-phase rotation
- Configuration: 4-layer loading (defaults → file → env → override), validation
- Runtime orchestration: Phase 0→5 startup, Phase 5→0 graceful shutdown, drain timeout
- HTTP API: 27 endpoints (agent lifecycle, sources, skills, workflows, plugins, cron, DLQ, logs)
- API security: token auth, audit logging, confirmation requirements
- CLI: `aman run` + `skill`/`plugin`/`event`/`workflow`/`config`/`dlq`/`health` subcommands
- Observability: tracing with `#[instrument]`, Prometheus metrics (12+ metrics), audit logging (10+ types)
- Tauri v2 desktop application: Dashboard, Skill Editor, Workflow Board, SOUL Editor, Plugin Manager, DLQ
- SDK crate with prelude re-exports for external Skill/Plugin authors
- E2E integration tests: workflow timeout, DLQ lifecycle, backpressure, secret rotation audit
- CI/CD pipeline (GitHub Actions: clippy + test + doc on Linux/macOS)
