# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- **Gateway daemon crate**: replaces the former `runtime` crate as the central control plane; Tauri
  desktop app communicates via HTTP (not embedded runtime). 86+ HTTP API endpoints (up from 27).
- **Agent runtime**: AgentRegistry, AgentHarness with ReAct loop engine, multi-agent coordination
  with agent-to-agent messaging, per-agent tool access control, token budget with history compression.
- **LLM integration**: Hermes-style protocol with conversation history and system prompt caching,
  OpenAI provider extracted as pluggable `llm-provider-openai`, Ollama native /api/embed support,
  live model listing dropdown per agent provider/model fields, `LlmConfig` with top-level
  `llm.api_type`, config-driven `max_output_tokens`.
- **Idle system**: two-axis depth-arousal model with per-agent AgentIdleManager, Sleep/Meditation/
  Incubation/Reflection runners, BoredomActor with weighted random tag selection, work-pressure
  dynamic weight scaling, configurable cooldowns, manual idle-run trigger in Chat sessions panel.
- **Plugin ecosystem**: Team kanban plugin with JSON-RPC subprocess bridge, info-hub plugin with
  unified search (API, CLI, DB, RSS) and AI processing pipeline, memory-store plugin, messaging
  plugins for Telegram/Slack/Discord/Matrix with multi-instance support, proxy support, hot reload.
- **Protocol support**: stdio JSON-RPC 2.0 (`aman serve`) and gRPC (tonic/prost) alongside the
  existing HTTP REST protocol — all three share the same `AgentRuntime` methods.
- **Frontend pages**: Home with agent avatar grid and finance skill cards, Chat with IM-style UI
  and Markdown rendering, Work kanban board, Notification center with overlay and bell widget,
  Integration (ThirdPartyServices), Providers, Compact sidebar mode, Agents page, Finance tab.
- **Code Agents**: external CLI coding tools registered on Home page with auto tool registration;
  Kimi and Grok code agents with icons, skills, and configs.
- **Lifecycle engines**: `lifecycle`, `work`, `study`, `daily-life` crates — passive push queue
  models with config-driven script hooks, auto-discovery from `~/.aman/hooks/<name>/config.yaml`.
- **Memory system**: `memory` crate with `MemoryProvider` trait, `YantrikdbProvider`, `RemoteEmbedder`,
  config-aware provider selection, plugin export hooks, `TraceStore` for idle cognitive runners.
- **Tool system**: `read`/`write`/`edit`/`list`/`find`/`grep` (ripgrep) replacing legacy file tool,
  `web_fetch` tool with proxy support, `ExecutionModel` for automatic tool call parallelization,
  tool security checks, simplified file operations system prompt guide.
- **Skill system**: skm-core/skm-select integration for spec-compliant skill loading, `/skill`
  slash-command with autocomplete picker, `create-skill` meta skill, `kanban-worker` skill,
  declarative skills with YAML frontmatter support, `idle_prompt` field in SKILL.md.
- **Eval system**: `eval` crate for LLM output and work item quality assessment.
- **Script hooks**: `ScriptRuntime`, `ScriptHook`, config-driven event hooks with multiple event
  types per entry, directory-based hook scoping, event bubbling with `prevented` flag.
- **Dual-layer event bus**: per-agent local buses within the global bus, per-agent idle system.
- **Configuration**: `secrets_mode` (env/keyring/1password), `GatewayConfig` with port/auto-connect,
  per-agent `allowed_skills`, `ReviewConfig` with `review_depth`, `IncubationConfig` with
  `cancel_timeout_secs`, `IdleConfig` with cooldowns.
- **Security**: AGPL-3.0 license, log redaction via compile-time `println!`/`eprintln!` prevention
  and `RedactWriter` (7 regex patterns), redaction module in `kernel::redactor`, comprehensive
  security-harness documentation, dev signing script for macOS keychain prompts.
- **Observability**: SSE connection replacing 5 polling loops, tracing redaction layer,
  `AmanExistence` provenance type, ai signal type registration, `proof.bin` embedded at compile
  time, `TraceStore` queries wired into Reflection, `DeferredTaskQueue` in idle loop.
- **Notification system**: `notification` crate with in-memory/disk backends, unread counts,
  dismiss/ack endpoints, SSE-pushed real-time delivery.
- **Python self-modules**: `self/` Python modules for agent prompt building, gateway calls Python
  self-module instead of Rust fallbacks for prompt construction.
- **Index store**: Chinese-language index store support (`e037b9e`).
- **Gateway utilities**: gateway.sh (build, sign, install, launch), `--version` flag, UI pages
  endpoint for dynamic plugin pages, compact sidebar toggle, UI design tokens with light/dark theme.

### Changed

- **Runtime replaced by Gateway**: the standalone `runtime` crate was removed; its functionality
  now lives inside the `gateway` crate. Tauri app starts/stops/restarts the gateway subprocess
  and talks to it via HTTP, SSE, and JSON-RPC.
- **License**: relicensed from MIT to AGPL-3.0 (`e2f5c2f`).
- **Manifest format**: skill and plugin manifests consolidated into single `.manifest.json` format;
  built-in manifests further consolidated into `builtin.json`.
- **Work/Study/DailyLife**: refactored from direct implementation to passive push queue architecture
  (v2 passive-queue, v3 lifecycle-engine).
- **Chat sessions**: isolated per agent; session creation moved to per-agent; sessions persisted
  on creation; resume on gateway restart; per-agent session store, YantrikDB, and LLM provider.
- **Agent state widget**: now `systemState`-driven with per-state emoji display, per-agent
  `AgentSystemState` with Chatting state.
- **Event bus**: event bubbling with hook `prevented` flag, stall detection in drain loop,
  poison-safe `RwLock` in `ChannelRegistry`/`ChatSessionStore`/`StickyAgentRouter`.
- **Frontend**: UI design token overhaul with CSS custom properties; `chat-input` extracted as
  standalone web component; `SendButton` reusable component; collapsible sidebar menus.
- **Skill system**: inject all matching skills into system prompt (not just top 1); reject
  Hermes-style nested metadata; YAML metadata optional with `---` frontmatter support.
- **Project casing**: unified to lowercase "aman" throughout.
- **Gateway binary**: spawned from installed path (`gateway` binary) instead of `cargo run`;
  daemonized with graceful shutdown and shutdown animation.
- **Prompt system**: Hermes-style flow replaces direct ReAct; `prompt_budget` removed in favor
  of `context_window` directly; `max_output_tokens` only used in preflight check.
- **Dependencies**: 9 unused crate dependencies removed across 7 crates; orphaned `humantime`/`quote`
  removed; Cargo.lock reverted to pre-update to avoid Tauri 2.11.2 regression.
- **Chat sessions**: Enter sends message (Ctrl+Enter for newline), IME composition ignored,
  session title inline-editable with persistence.

### Fixed

- Streaming: garbled text, blinking cursor, duplicate replies, tool call chain completion
- Session persistence: messages and state saved after LLM reply, restored on gateway restart
- Agent routing: messages routed to session's owning agent; correct restore_session filtering
- Gateway startup panics from nested runtime drop and missing memory directory
- Gateway shutdown: graceful Ctrl+C handling, second Ctrl+C force-quits, stall detection in
  drain loop prevents 30s timeout blockage
- Idle system: depth reset on queue drain, arousal boost on real events; agent_id (snake_case)
  consistency; char-boundary-safe reflection truncation; skip event in tracing spans
- LLM: config max_output_tokens passed correctly; max_tokens omitted when unconfigured;
  config_warning event emitted; context_window retry/trim logic corrected
- UI: a11y warning by replacing empty href with button; app icon programmatic loading in dev mode;
  agent state widget shows per-state emoji; session ordering by date; nav disabled when gateway down
- Plugin system: team plugin body parsing, stage_history migration for old dbs, agent list population,
  context.py missing, stale task->work references; JSON-RPC message misclassification
- Skill loading: YAML metadata optional; nested Hermes metadata rejection; default version for
  declarative skills; string triggers accepted in SKILL.md
- Clippy: all warnings resolved across workspace (multiple rounds)
- Gateway status: numeric phase parsing in Tauri (was always "stopped")
- Security: API key leak in truncation; keychain blocking at startup; socks5->socks5h for remote DNS;
  poison-safe RwLock in channel/session stores
- Reflection: decoupled from Sleep, triggered on QueueDrained events; backfill via Sleep Phase 1;
  `reflected_at` reset on session update; context overflow prevented
- Embedders: replaced `reqwest::blocking` with `ureq` to prevent nested runtime panic
- Proxy: raw TCP for IM channel reload; `curl --noproxy` fallback; proxy detection extracted to
  `kernel::proxy`
- Team plugin: kanban drag-and-drop; agent assignment; stage dropdown; boards import with two-step
  UI; page reload after import; work detail modal move bug

### Removed

- `runtime` crate (inlined into `gateway` crate)
- SkillEditor UI page (Tauri)
- `ChatPlatformSource` (dead code)
- Dashboard "Connect" button (replaced by auto-connect)
- `gateway.sh` script
- Cert/keychain files from repository
- Default agent fallthrough — user must @mention an agent
- Code Agents from Chat page (moved to Home page with terminal hint)
- `file` tool (replaced by read/write/edit/list/find/grep)
- `chat-input` from workspace members (moved to shared/frontend, compiled independently)
- Steps sidebar (unused)
- `prompt_budget` config (context_window used directly)
- Built-in Test button for messaging channels
- Rust fallbacks for prompt building (replaced by Python self-module)
- 9 unused crate dependencies

### Documentation

- Events system documentation with Mermaid diagrams and framework comparison
- Security-harness comprehensive doc covering all security mechanisms
- Plugin/event/hook developer guide with sample code
- Idle design docs: `idle-design.md`, `idle-patch.md` with readiness matrix and implementation phases
- Work/Study/DailyLife design docs for lifecycle engine refactor (v3)
- `skills/README.md` tracking third-party skill sources
- Dev-guide: find vs grep comparison, external event push API/CLI section
- Acknowledgments for openclaw and hermes agent with project links
- README: Quick Start, Secrets Configuration, notification center, architecture diagram,
  project status, scam disclaimer, WIP warning about unstable data structures
- Hermes content-to-skill documentation

## [0.1.0] - 2026-05-13

### Added

- Initial release of aman Agent Framework
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
