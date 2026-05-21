use chat_source::ChatPlatformSource;
use config::{AgentConfig, BusMode};
use event_bus::{DiscardHook, EventBus, InMemoryBus, InMemoryBusConfig};
use idle::coordination::IdleCoordination;
use idle::detector::IdleDetector;
use idle::incubation::IncubationManager;
use kernel::context::ToolContext;
use kernel::event::{Event, EventType};
use kernel::hook::Hook;
use kernel::llm::LlmProvider;
use kernel::prompt::DefaultPromptPipeline;
use kernel::session_history::InMemorySessionHistory;
use kernel::schema::JsonSchema;
use kernel::skill::Skill;
use kernel::source::EventSource;
use kernel::tool::Tool;
use kernel::types::{BackpressureLevel, ToolMode};
use kernel::{AmanResult, Error};
use persistence::{DeadLetterQueue, InMemoryDeadLetterQueue, PersistentBus, WalSync, WriteAheadLog};
use kernel::plugin::Plugin;
use pipeline::ToolEventSink;
use plugin::{
    PluginCandidate, PluginExports, PluginIsolationMode, PluginLifecycleConfig,
    PluginExportRegistrar, PluginInstaller, PluginLoader, PluginManifest,
};
use serde::Serialize;
use serde_json::json;
use secret::{
    AwsSecretsManagerCliBackend, EnvSecretBackend, KeychainBackend, OnePasswordCliBackend,
    SecretBackend, SecretCacheFallbackConfig, SecretResolver, SecretResolverConfig, VaultCliBackend,
};
use source::{CronManager, CronSource, SourceRegistry};
use std::collections::{BTreeSet, HashMap};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};
use std::sync::Arc;
use std::sync::LazyLock;
use std::sync::Mutex as StdMutex;
use std::time::Duration;
use tokio::sync::{Mutex, RwLock};
use super::{AuditLogger, EventStore};
use super::SoulRuntime;
use soul::SoulHotReloadManager;
use tracing::instrument;
use workflow::{
    ErrorRecovery, StateDef, StateTimeout, Transition, TransitionFrom, TransitionTo, WorkflowDef,
    WorkflowEngine,
};

// ---------------------------------------------------------------------------
// Capability registry types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum CapabilityStatus {
    Healthy,
    Degraded,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CapabilityEntry {
    pub capability: String,
    pub plugin: String,
    pub version: String,
    pub status: CapabilityStatus,
}

// ---------------------------------------------------------------------------
// Runtime lifecycle
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimePhase {
    Phase0 = 0,
    Phase05 = 1,
    Phase1 = 2,
    Phase2 = 3,
    Phase3 = 4,
    Phase4 = 5,
    Phase5 = 6,
}

impl RuntimePhase {
    fn from_u8(value: u8) -> Self {
        match value {
            0 => Self::Phase0,
            1 => Self::Phase05,
            2 => Self::Phase1,
            3 => Self::Phase2,
            4 => Self::Phase3,
            5 => Self::Phase4,
            _ => Self::Phase5,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeStatus {
    New,
    Starting,
    Ready,
    ShuttingDown,
    Shutdown,
}

pub struct AgentRuntimeBuilder {
    config: AgentConfig,
    runtime_dir: PathBuf,
    bind_addr: SocketAddr,
    api_token: Option<String>,
    startup_pause: Duration,
    soul_file: Option<PathBuf>,
    extra_plugins: Vec<plugin::PluginCandidate>,
}

impl AgentRuntimeBuilder {
    #[must_use]
    pub fn new(config: AgentConfig) -> Self {
        Self {
            config,
            runtime_dir: default_runtime_dir(),
            bind_addr: "127.0.0.1:9999".parse().expect("socket addr parse"),
            api_token: None,
            startup_pause: Duration::from_millis(0),
            soul_file: None,
            extra_plugins: vec![],
        }
    }

    #[must_use]
    pub fn with_runtime_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.runtime_dir = dir.into();
        self
    }

    #[must_use]
    pub fn with_bind_addr(mut self, addr: SocketAddr) -> Self {
        self.bind_addr = addr;
        self
    }

    #[must_use]
    pub fn with_api_token(mut self, token: Option<String>) -> Self {
        self.api_token = token;
        self
    }

    #[must_use]
    pub fn with_startup_pause(mut self, pause: Duration) -> Self {
        self.startup_pause = pause;
        self
    }

    #[must_use]
    pub fn with_soul(mut self, soul_file: impl Into<PathBuf>) -> Self {
        self.soul_file = Some(soul_file.into());
        self
    }

    /// Add an extra plugin candidate to load alongside the built-in LLM plugin.
    /// Primarily used in tests to verify plugin lifecycle behavior.
    #[must_use]
    pub fn with_extra_plugin(mut self, candidate: plugin::PluginCandidate) -> Self {
        self.extra_plugins.push(candidate);
        self
    }

    pub fn build(self) -> AmanResult<Arc<AgentRuntime>> {
        super::init_tracing();
        std::fs::create_dir_all(&self.runtime_dir)?;

        let dlq = Arc::new(InMemoryDeadLetterQueue::new(5));
        let audit = Arc::new(AuditLogger::new(2_000));

        // Extract idle config before self.config is moved
        let idle_enabled = self.config.idle.enabled;
        let idle_arousal_initial = self.config.idle.arousal.initial_value;
        let idle_arousal_half_life = self.config.idle.arousal.half_life_secs;
        let idle_personality = self.config.idle.personality.clone();

        let config = resolve_secrets_in_config(self.config, &self.runtime_dir, &audit)?;

        let inflight_pipelines = Arc::new(AtomicUsize::new(0));
        let inflight_skills = Arc::new(AtomicUsize::new(0));
        let metrics = super::metrics::MetricsRegistry::new();
        let audit_for_hook = Arc::clone(&audit);
        let (bus, persistent_bus) =
            build_runtime_bus(&config, &self.runtime_dir, config.event_bus.max_queue_size, Some(
                Arc::new(move |event: &Event, level: BackpressureLevel, reason: &str| {
                    audit_for_hook.record("system", "event.discard", event.source.to_string(), "ok",
                        format!("level={level:?} reason={reason} event_id={}", event.id));
                }),
            ))?;
        let bus: Arc<dyn EventBus> = Arc::new(bus);

        let sources = Arc::new(SourceRegistry::new(Arc::clone(&bus)));

        // Sync built-in skills from repo to ~/.aman/skills/ (preserves user modifications)
        if let Err(e) = super::skill_sync::sync_builtin_skills() {
            tracing::error!(error = %e, "failed to sync built-in skills");
        }
        let skills_dir = super::skill_sync::aman_data_dir().join("skills");
        let _ = std::fs::create_dir_all(&skills_dir);
        let llm_skills = skill::discover_llm_skills(&skills_dir);
        tracing::info!(count = llm_skills.len(), "discovered LLM instruction skills");

        // Build skm-core registry + cascade selector for intelligent skill matching.
        let (skill_registry, cascade_selector) = build_skill_selector(&skills_dir);
        tracing::info!(
            has_registry = skill_registry.is_some(),
            has_selector = cascade_selector.is_some(),
            "cascade selector initialized"
        );
        let skills = Arc::new(skill::SkillRegistry::new());
        let tools = Arc::new(tool::ToolRegistry::new());
        let _ = tool::install_builtin_tools(&tools);
        // Register read_skill tool so the LLM can load SKILL.md instructions on demand.
        let _ = tools.register(Arc::new(ReadSkillTool { skills: llm_skills.clone() }));
        let skill_search = Arc::new(skill::SkillSearch::new());
        let skill_versions = Arc::new(skill::SkillVersionManager::from_root(
            self.runtime_dir.join("skill-history"),
        ));
        let skill_hot_reload = Arc::new(
            skill::HotReloadManager::new(
                skills_dir.clone(),
                Arc::clone(&skills),
                Arc::clone(&skill_search),
            )
            .with_version_manager(Arc::clone(&skill_versions)),
        );
        let workflows_dir = self.runtime_dir.join("workflows");
        let _ = std::fs::create_dir_all(&workflows_dir);
        let workflow_engine = Arc::new(WorkflowEngine::new());
        // --- chat-session workflow definition ---
        let _ = workflow_engine.register_workflow(WorkflowDef {
            name: "chat-session".to_owned(),
            states: vec![
                StateDef { name: "ACTIVE".to_owned() },
                StateDef { name: "PROCESSING".to_owned() },
                StateDef { name: "IDLE".to_owned() },
                StateDef { name: "ERROR".to_owned() },
                StateDef { name: "RETRYING".to_owned() },
                StateDef { name: "TIMEOUT".to_owned() },
                StateDef { name: "CLOSED".to_owned() },
            ],
            initial_state: "ACTIVE".to_owned(),
            final_states: vec!["CLOSED".to_owned()],
            error_state: "ERROR".to_owned(),
            transitions: vec![
                Transition {
                    from: TransitionFrom::Specific("ACTIVE".to_owned()),
                    event: "MESSAGE_RECEIVED".to_owned(),
                    to: TransitionTo::Specific("PROCESSING".to_owned()),
                    guard: None, on_fail: None, action: None, on_action_failure: None,
                },
                Transition {
                    from: TransitionFrom::Specific("ACTIVE".to_owned()),
                    event: "SESSION_TIMEOUT".to_owned(),
                    to: TransitionTo::Specific("TIMEOUT".to_owned()),
                    guard: None, on_fail: None, action: None, on_action_failure: None,
                },
                Transition {
                    from: TransitionFrom::Specific("PROCESSING".to_owned()),
                    event: "LLM_REPLY_READY".to_owned(),
                    to: TransitionTo::Specific("IDLE".to_owned()),
                    guard: None, on_fail: None, action: None, on_action_failure: None,
                },
                Transition {
                    from: TransitionFrom::Specific("PROCESSING".to_owned()),
                    event: "LLM_STREAM_DONE".to_owned(),
                    to: TransitionTo::Specific("IDLE".to_owned()),
                    guard: None, on_fail: None, action: None, on_action_failure: None,
                },
                Transition {
                    from: TransitionFrom::Specific("PROCESSING".to_owned()),
                    event: "LLM_ERROR".to_owned(),
                    to: TransitionTo::Specific("ERROR".to_owned()),
                    guard: None, on_fail: None, action: None, on_action_failure: None,
                },
                Transition {
                    from: TransitionFrom::Specific("PROCESSING".to_owned()),
                    event: "STREAM_TIMEOUT".to_owned(),
                    to: TransitionTo::Specific("TIMEOUT".to_owned()),
                    guard: None, on_fail: None, action: None, on_action_failure: None,
                },
                Transition {
                    from: TransitionFrom::Specific("PROCESSING".to_owned()),
                    event: "SESSION_CLOSE_CMD".to_owned(),
                    to: TransitionTo::Specific("CLOSED".to_owned()),
                    guard: None, on_fail: None, action: None, on_action_failure: None,
                },
                Transition {
                    from: TransitionFrom::Specific("IDLE".to_owned()),
                    event: "MESSAGE_RECEIVED".to_owned(),
                    to: TransitionTo::Specific("PROCESSING".to_owned()),
                    guard: None, on_fail: None, action: None, on_action_failure: None,
                },
                Transition {
                    from: TransitionFrom::Specific("IDLE".to_owned()),
                    event: "SESSION_TIMEOUT".to_owned(),
                    to: TransitionTo::Specific("TIMEOUT".to_owned()),
                    guard: None, on_fail: None, action: None, on_action_failure: None,
                },
                Transition {
                    from: TransitionFrom::Specific("IDLE".to_owned()),
                    event: "SESSION_END".to_owned(),
                    to: TransitionTo::Specific("CLOSED".to_owned()),
                    guard: None, on_fail: None, action: None, on_action_failure: None,
                },
                Transition {
                    from: TransitionFrom::Specific("ERROR".to_owned()),
                    event: "RETRY_CMD".to_owned(),
                    to: TransitionTo::Specific("RETRYING".to_owned()),
                    guard: None, on_fail: None, action: None, on_action_failure: None,
                },
                Transition {
                    from: TransitionFrom::Specific("ERROR".to_owned()),
                    event: "SESSION_END".to_owned(),
                    to: TransitionTo::Specific("IDLE".to_owned()),
                    guard: None, on_fail: None, action: None, on_action_failure: None,
                },
                Transition {
                    from: TransitionFrom::Specific("ERROR".to_owned()),
                    event: "ABANDON_TIMEOUT".to_owned(),
                    to: TransitionTo::Specific("CLOSED".to_owned()),
                    guard: None, on_fail: None, action: None, on_action_failure: None,
                },
                Transition {
                    from: TransitionFrom::Specific("RETRYING".to_owned()),
                    event: "RETRY_STARTED".to_owned(),
                    to: TransitionTo::Specific("PROCESSING".to_owned()),
                    guard: None, on_fail: None, action: None, on_action_failure: None,
                },
                Transition {
                    from: TransitionFrom::Specific("RETRYING".to_owned()),
                    event: "RETRY_FAILED".to_owned(),
                    to: TransitionTo::Specific("ERROR".to_owned()),
                    guard: None, on_fail: None, action: None, on_action_failure: None,
                },
                Transition {
                    from: TransitionFrom::Specific("TIMEOUT".to_owned()),
                    event: "SESSION_END".to_owned(),
                    to: TransitionTo::Specific("CLOSED".to_owned()),
                    guard: None, on_fail: None, action: None, on_action_failure: None,
                },
                Transition {
                    from: TransitionFrom::Specific("TIMEOUT".to_owned()),
                    event: "MESSAGE_RECEIVED".to_owned(),
                    to: TransitionTo::Specific("IDLE".to_owned()),
                    guard: None, on_fail: None, action: None, on_action_failure: None,
                },
            ],
            state_timeouts: vec![
                StateTimeout {
                    state: "ACTIVE".to_owned(),
                    timeout_ms: 300_000,
                    on_timeout: TransitionTo::Specific("TIMEOUT".to_owned()),
                    on_timeout_alert: None,
                },
                StateTimeout {
                    state: "PROCESSING".to_owned(),
                    timeout_ms: 120_000,
                    on_timeout: TransitionTo::Specific("TIMEOUT".to_owned()),
                    on_timeout_alert: None,
                },
                StateTimeout {
                    state: "IDLE".to_owned(),
                    timeout_ms: 600_000,
                    on_timeout: TransitionTo::Specific("TIMEOUT".to_owned()),
                    on_timeout_alert: None,
                },
                StateTimeout {
                    state: "ERROR".to_owned(),
                    timeout_ms: 120_000,
                    on_timeout: TransitionTo::Specific("CLOSED".to_owned()),
                    on_timeout_alert: None,
                },
                StateTimeout {
                    state: "TIMEOUT".to_owned(),
                    timeout_ms: 120_000,
                    on_timeout: TransitionTo::Specific("CLOSED".to_owned()),
                    on_timeout_alert: None,
                },
            ],
            error_recovery: ErrorRecovery {
                auto_retry_count: 0,
                max_retry_count: 5,
                on_retry_failure: workflow::RetryFailurePolicy::ManualOnly,
                retry_backoff: Default::default(),
            },
        });
        let cron_manager = CronManager::with_runtime_dir(self.runtime_dir.clone());
        let plugin_installer = Arc::new(PluginInstaller::new(self.runtime_dir.join("plugins")));
        let event_store = Arc::new(EventStore::new(2_000, 500));

        let auth_registry = Arc::new(tool::auth::AuthRegistry::new());
        let llm_provider = create_llm_provider();

        // Load the built-in idle system plugin (handles idle personality progression).
        let idle_plugin = idle_system::IdleSystemPlugin::new();
        let idle_candidate = PluginCandidate {
            manifest: PluginManifest {
                name: "idle-system".to_owned(),
                version: idle_plugin.version().clone(),
                depends_on: vec![],
                lifecycle: PluginLifecycleConfig { auto_start: true },
                exports: PluginExports {
                    skills: vec![
                        "idle-daze".to_owned(),
                        "idle-boredom".to_owned(),
                        "idle-sleep".to_owned(),
                        "idle-exploration".to_owned(),
                        "idle-meditation".to_owned(),
                        "idle-waiting".to_owned(),
                        "idle-incubation".to_owned(),
                    ],
                    tools: vec![],
                    event_sources: vec![],
                    hooks: vec![],
                },
                config_schema: None,
                isolation: Some(PluginIsolationMode::InProcess),
                subprocess: None,
                wasm_path: None,
                capabilities: vec![],
                ui: None,
            },
            plugin: Box::new(idle_plugin),
            isolation: PluginIsolationMode::InProcess,
            subprocess: None,
            wasm_module_bytes: None,
        };

        // Load the built-in LLM plugin, idle-system plugin (and any extra plugins from builder).
        let mut all_candidates = vec![idle_candidate];
        all_candidates.extend(self.extra_plugins);
        let hook_registry = Arc::new(hook::HookRegistry::new());
        let mut plugin_loader = PluginLoader::new(Arc::new(RuntimePluginRegistrar::new(
            Arc::clone(&skills),
            Arc::clone(&tools),
            Arc::clone(&hook_registry),
        )));
        if let Err(e) = pollster::block_on(plugin_loader.load_all(all_candidates)) {
            tracing::error!(error = %e, "failed to load built-in plugins");
        }

        // Subscribe a handler that dispatches every event to matching skills.
        use kernel::context::SkillContext;
        use kernel::context::BaseContext;
        struct SkillEventDispatcher {
            executor: skill::SkillExecutor,
            bus: Arc<dyn event_bus::EventBus>,
        }
        #[async_trait::async_trait]
        impl event_bus::EventHandler for SkillEventDispatcher {
            async fn handle(&self, event: kernel::event::Event) -> kernel::AmanResult<()> {
                // Guard: skip our own internal events to prevent infinite dispatch
                // loops where message:dispatch or message:completed trigger another cycle.
                if event.source.as_str() == "skill:dispatcher" {
                    return Ok(());
                }
                let trace_id = event.metadata.trace_id;
                let is_idle = event.event_type == EventType::Idle;
                let ctx = SkillContext {
                    base: BaseContext::new(trace_id),
                    skill_name: None,
                    soul_name: None,
                };
                let result = self.executor.execute_matching(event, ctx).await;
                // Only emit dispatch/completed signals for non-idle events.
                // Idle ticks already flood the event store with triple patterns
                // (idle → dispatch → completed) and the signals carry no useful
                // information for other components.
                if !result.executed.is_empty() && !is_idle {
                    let _ = self.bus.publish(Event::new(
                        "skill:dispatcher",
                        EventType::Custom("message:dispatch".to_owned()),
                        json!({
                            "trace_id": trace_id.to_string(),
                        }),
                    )).await;
                    let _ = self.bus.publish(Event::new(
                        "skill:dispatcher",
                        EventType::Custom("message:completed".to_owned()),
                        json!({
                            "trace_id": trace_id.to_string(),
                            "executed": result.executed,
                            "failed": result.failed.iter().map(|f| json!({"name": f.skill_name, "error": f.message})).collect::<Vec<_>>(),
                        }),
                    )).await;
                }
                Ok(())
            }
        }
        let _ = pollster::block_on(bus.subscribe(
            event_bus::SubscriptionFilter::default(),
            Box::new(SkillEventDispatcher {
                executor: skill::SkillExecutor::new(Arc::clone(&skills)),
                bus: Arc::clone(&bus),
            }),
        ));

        let (soul_runtime, soul_manager) = if let Some(soul_file) = self.soul_file {
            let runtime = SoulRuntime::new(soul::Soul::from_file(&soul_file)?);
            let mut manager = runtime.build_hot_reload_manager(soul_file)?;
            manager.start_watching()?;
            (Some(runtime), Some(manager))
        } else {
            (None, None)
        };

        // Register the chat-platform source
        let chat_source = ChatPlatformSource::new_tauri_desktop();
        let chat_sender = Some(chat_source.sender());
        let _ = pollster::block_on(sources.register(
            Box::new(chat_source),
            source::SourceMode::Push,
            source::TrustLevel::Untrusted,
        ));

        // ── Idle system setup (M7) ──────────────────────────────
        let (idle_coord, incubation_manager) = if idle_enabled {
            let coord = Arc::new(IdleCoordination::new(
                idle_arousal_initial,
                idle_arousal_half_life,
            ));
            let detector = IdleDetector::new(
                "idle:detector",
                Arc::clone(&coord),
                idle_personality,
            );
            let _ = pollster::block_on(sources.register(
                Box::new(detector),
                source::SourceMode::Pull,
                source::TrustLevel::Untrusted,
            ));
            (Some(coord), Arc::new(IncubationManager::new()))
        } else {
            (None, Arc::new(IncubationManager::new()))
        };

        // ── Notification store ─────────────────────────────────────
        let notifications = Arc::new(notification::NotificationStore::new(500));

        // ── Session store (SQLite index + JSONL cleanup) ────────────
        let session_store = std::env::var("HOME").ok().and_then(|home| {
            let aman_cfg = config::AmanConfig::from_default_path().ok()?;
            let agent_key = aman_cfg.agents.keys().next()?;
            let agents_dir = PathBuf::from(&home).join(".aman").join("agents").join(agent_key);
            let db_path = agents_dir.join("sessions.db");
            let sessions_dir = agents_dir.join("sessions");
            match super::session_store::SessionStore::open(&db_path, &sessions_dir) {
                Ok(store) => {
                    tracing::info!(path = %db_path.display(), "session store opened");
                    Some(Arc::new(store))
                }
                Err(e) => {
                    tracing::warn!(error = %e, "session store init skipped");
                    None
                }
            }
        });

        // ── Subscribe notification subscriber ─────────────────────
        let notif_sub = notification::NotificationSubscriber::new(Arc::clone(&notifications));
        let _ = pollster::block_on(bus.subscribe(
            event_bus::SubscriptionFilter::default(),
            Box::new(notif_sub),
        ));

        // ── Agent registry ──────────────────────────────────────────
        let agent_registry = Arc::new(super::AgentRegistry::new(Arc::clone(&bus)));

        // ── Memory store (M5) ──────────────────────────────────
        let memory_store = Arc::new(super::memory_store::MemoryStore::new());

        // ── Agent harness (ReAct loop orchestrator) ──────────────────
        let agent_harness = Arc::new(super::agent_harness::AgentHarness::new(
            Arc::clone(&agent_registry),
            Arc::clone(&tools),
            Arc::clone(&bus),
            Arc::clone(&memory_store) as Arc<dyn kernel::memory::MemoryRetrieval>,
            llm_provider,
            Box::new(DefaultPromptPipeline),
            Box::new(InMemorySessionHistory::new()),
            Box::new(kernel::budget::DefaultTokenBudgetPolicy::new()),
            Box::new(super::agent_harness::FirstEnabledAgentRouter),
        ));

        // ── Subscribe STOP_GENERATION handler for agent interrupt (M6) ──
        struct StopGenerationHandler {
            agent_harness: Arc<super::agent_harness::AgentHarness>,
        }
        #[async_trait::async_trait]
        impl event_bus::EventHandler for StopGenerationHandler {
            async fn handle(&self, event: kernel::event::Event) -> kernel::AmanResult<()> {
                let session_id = event.payload.get("session_id").and_then(|v| v.as_str()).unwrap_or("");
                if !session_id.is_empty() {
                    self.agent_harness.interrupt_session(session_id);
                }
                Ok(())
            }
        }
        let _ = pollster::block_on(bus.subscribe(
            event_bus::SubscriptionFilter {
                event_types: Some(vec![kernel::event::EventType::Custom("STOP_GENERATION".to_owned())]),
                sources: None,
                priorities: None,
                payload_match: None,
            },
            Box::new(StopGenerationHandler {
                agent_harness: Arc::clone(&agent_harness),
            }),
        ));

        // ── Subscribe MESSAGE_RECEIVED handler to route messages to AgentHarness (T2.3) ──
        struct MessageReceivedHandler {
            agent_harness: Arc<super::agent_harness::AgentHarness>,
            soul_runtime: Option<SoulRuntime>,
        }
        #[async_trait::async_trait]
        impl event_bus::EventHandler for MessageReceivedHandler {
            async fn handle(&self, event: kernel::event::Event) -> kernel::AmanResult<()> {
                let session_id = event.payload.get("session_id")
                    .and_then(|v| v.as_str()).unwrap_or("").to_owned();
                let text = event.payload.get("text")
                    .and_then(|v| v.as_str()).unwrap_or("").to_owned();

                if session_id.is_empty() || text.is_empty() {
                    return Ok(());
                }

                // Prepend skill activation message if a skill was pre-selected by cascade.
                let text = event.payload.get("skill_activation_message")
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty())
                    .map(|activation| format!("{activation}\n\nUser query: {text}"))
                    .unwrap_or(text);

                // Resolve first enabled agent via the harness.
                let agent = match self.agent_harness.resolve_first_enabled_agent(&text).await {
                    Some(a) => a,
                    None => {
                        tracing::warn!("MessageReceivedHandler: no enabled agent found");
                        return Ok(());
                    }
                };
                let agent_id = agent.descriptor.agent_id.clone();
                let model = agent.descriptor.model.clone();

                // Build SoulSnapshot from pre-built prompt in event payload, or fall back
                // to current soul (which includes skill instructions from the HTTP handler).
                let soul_snapshot = event.payload.get("soul_system_prompt")
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty())
                    .map(|prompt| {
                        let name = self.soul_runtime.as_ref()
                            .map(|sr| sr.current_soul().name.clone())
                            .unwrap_or_else(|| "assistant".to_owned());
                        kernel::react::SoulSnapshot::new(name, prompt)
                    })
                    .unwrap_or_else(|| {
                        self.soul_runtime.as_ref()
                            .map(|sr| {
                                let soul = sr.current_soul();
                                kernel::react::SoulSnapshot::new(soul.name.clone(), soul.to_system_prompt())
                            })
                            .unwrap_or_else(|| kernel::react::SoulSnapshot::new("assistant", ""))
                    });

                // Spawn async ReAct processing — do not block the bus drain loop.
                self.agent_harness.spawn_process_message(
                    agent_id, session_id, text, model, soul_snapshot,
                );

                Ok(())
            }
        }
        let _ = pollster::block_on(bus.subscribe(
            event_bus::SubscriptionFilter {
                event_types: Some(vec![kernel::event::EventType::MessageReceived]),
                sources: None,
                priorities: None,
                payload_match: None,
            },
            Box::new(MessageReceivedHandler {
                agent_harness: Arc::clone(&agent_harness),
                soul_runtime: soul_runtime.clone(),
            }),
        ));

        // ── Subscribe agent:message handler for agent-to-agent routing (M7) ──
        struct AgentMessageHandler {
            agent_harness: Arc<super::agent_harness::AgentHarness>,
            soul_runtime: Option<SoulRuntime>,
        }
        #[async_trait::async_trait]
        impl event_bus::EventHandler for AgentMessageHandler {
            async fn handle(&self, event: kernel::event::Event) -> kernel::AmanResult<()> {
                let msg: kernel::agent::AgentMessage = match serde_json::from_value(event.payload) {
                    Ok(m) => m,
                    Err(e) => {
                        tracing::warn!("AgentMessageHandler: failed to parse AgentMessage: {e}");
                        return Ok(());
                    }
                };

                let agent = match self.agent_harness.resolve_agent(&msg.to_agent).await {
                    Some(a) => a,
                    None => {
                        tracing::warn!("AgentMessageHandler: target agent '{}' not found or disabled", msg.to_agent);
                        return Ok(());
                    }
                };
                let model = agent.descriptor.model.clone();

                // Build SoulSnapshot from current soul.
                let soul_snapshot = self.soul_runtime.as_ref()
                    .map(|sr| {
                        let soul = sr.current_soul();
                        kernel::react::SoulSnapshot::new(soul.name.clone(), soul.to_system_prompt())
                    })
                    .unwrap_or_else(|| kernel::react::SoulSnapshot::new("assistant", ""));

                // Construct user-facing text from the agent message payload.
                let text = format!(
                    "[Message from agent '{}']\n{}",
                    msg.from_agent,
                    msg.payload.get("text").and_then(|v| v.as_str()).unwrap_or("")
                );

                self.agent_harness.spawn_process_message(
                    msg.to_agent.clone(),
                    format!("agent:{}:{}", msg.from_agent, msg.message_id),
                    text,
                    model,
                    soul_snapshot,
                );

                Ok(())
            }
        }
        let _ = pollster::block_on(bus.subscribe(
            event_bus::SubscriptionFilter {
                event_types: Some(vec![kernel::event::EventType::AgentMessage]),
                sources: None,
                priorities: None,
                payload_match: None,
            },
            Box::new(AgentMessageHandler {
                agent_harness: Arc::clone(&agent_harness),
                soul_runtime: soul_runtime.clone(),
            }),
        ));

        Ok(Arc::new(AgentRuntime {
            config,
            runtime_dir: self.runtime_dir,
            bind_addr: self.bind_addr,
            api_token: self.api_token,
            bus,
            persistent_bus,
            sources,
            skills,
            tools,
            auth_registry,
            skill_search,
            skill_versions,
            skill_hot_reload,
            skill_stop: Arc::new(AtomicBool::new(false)),
            skill_thread: Mutex::new(None),
            workflow_engine,
            plugin_loader: Mutex::new(plugin_loader),
            cron_manager: Mutex::new(cron_manager),
            plugin_installer,
            dlq,
            audit,
            event_store,
            observer_attached: AtomicBool::new(false),
            observer_subscription: Mutex::new(None),
            soul_runtime,
            soul_manager: Mutex::new(soul_manager),
            soul_stop: Arc::new(AtomicBool::new(false)),
            soul_thread: Mutex::new(None),
            backpressure_stop: Arc::new(AtomicBool::new(false)),
            backpressure_task: Mutex::new(None),
            phase: AtomicU8::new(RuntimePhase::Phase0 as u8),
            status: RwLock::new(RuntimeStatus::New),
            transition_lock: Mutex::new(()),
            shutdown_requested: AtomicBool::new(false),
            startup_pause: self.startup_pause,
            inflight_pipelines,
            inflight_skills,
            metrics,
            capability_registry: Default::default(),
            chat_sender,
            idle_coord,
            incubation_manager,
            llm_skills: StdMutex::new(llm_skills),
            skill_registry,
            cascade_selector,
            session_store,
            notifications,
            agent_registry,
            agent_harness,
        }))
    }
}

/// Concrete [`ToolEventSink`] that publishes tool lifecycle events to the EventBus.
///
/// Used to wire `PipelineEngine` tool events when it is configured with an
/// event bus. Currently `PipelineEngine` is not in the production tool path;
/// this sink is infrastructure for future wiring.
#[allow(dead_code)]
pub struct BusToolEventSink {
    bus: Arc<dyn EventBus>,
}

impl BusToolEventSink {
    #[allow(dead_code)]
    pub fn new(bus: Arc<dyn EventBus>) -> Self {
        Self { bus }
    }
}

#[async_trait::async_trait]
impl ToolEventSink for BusToolEventSink {
    async fn on_tool_invoke(&self, tool_name: &str, pipeline_id: &str, instance_id: &str) {
        let _ = self.bus.publish(Event::new(
            "pipeline:tool",
            EventType::Custom("tool:invoke".to_owned()),
            serde_json::json!({
                "tool_name": tool_name,
                "pipeline_id": pipeline_id,
                "instance_id": instance_id,
            }),
        )).await;
    }

    async fn on_tool_completed(&self, tool_name: &str, pipeline_id: &str, instance_id: &str, duration_ms: u64) {
        let _ = self.bus.publish(Event::new(
            "pipeline:tool",
            EventType::Custom("tool:completed".to_owned()),
            serde_json::json!({
                "tool_name": tool_name,
                "pipeline_id": pipeline_id,
                "instance_id": instance_id,
                "duration_ms": duration_ms,
            }),
        )).await;
    }

    async fn on_tool_failed(&self, tool_name: &str, pipeline_id: &str, instance_id: &str, error: &str) {
        let _ = self.bus.publish(Event::new(
            "pipeline:tool",
            EventType::Custom("tool:failed".to_owned()),
            serde_json::json!({
                "tool_name": tool_name,
                "pipeline_id": pipeline_id,
                "instance_id": instance_id,
                "error": error,
            }),
        )).await;
    }
}

/// Tool that allows the LLM to load a skill's full SKILL.md instructions on demand.
///
/// The system prompt instructs the LLM to use `read_skill` when it needs more
/// than the skill name+description index. This tool resolves skill names to
/// the on-disk SKILL.md file and returns the full content.
struct ReadSkillTool {
    skills: Vec<skill::SkillInfo>,
}

#[async_trait::async_trait]
impl Tool for ReadSkillTool {
    fn name(&self) -> &str {
        "read_skill"
    }

    fn mode(&self) -> ToolMode {
        ToolMode::Local
    }

    fn description(&self) -> &str {
        "Load a skill's full SKILL.md instructions by name. Skills contain specialized knowledge, step-by-step methodologies, analysis frameworks, and output templates for specific tasks (e.g., IPO research, code review, data analysis). Call this with the skill name to get its complete instructions."
    }

    fn parameters(&self) -> &JsonSchema {
        static PARAMS: LazyLock<JsonSchema> = LazyLock::new(|| {
            JsonSchema::from(serde_json::json!({
                "type": "object",
                "required": ["skill"],
                "properties": {
                    "skill": {
                        "type": "string",
                        "description": "The name of the skill to load (e.g. \"ipo-research\")"
                    }
                }
            }))
        });
        &PARAMS
    }

    fn returns(&self) -> &JsonSchema {
        static RETURNS: LazyLock<JsonSchema> = LazyLock::new(|| {
            JsonSchema::from(serde_json::json!({
                "type": "object",
                "properties": {
                    "name": {"type": "string"},
                    "content": {"type": "string"},
                    "error": {"type": "string"}
                }
            }))
        });
        &RETURNS
    }

    async fn execute(&self, params: serde_json::Value, _ctx: ToolContext) -> kernel::AmanResult<serde_json::Value> {
        let skill_name = match params.get("skill").and_then(|v| v.as_str()) {
            Some(s) if !s.is_empty() => s,
            _ => return Ok(serde_json::json!({
                "name": "", "content": "", "error": "Missing required parameter: skill"
            })),
        };

        let skill = match self.skills.iter().find(|s| s.name == skill_name) {
            Some(s) => s,
            None => {
                let available: Vec<&str> = self.skills.iter().map(|s| s.name.as_str()).collect();
                return Ok(serde_json::json!({
                    "name": skill_name, "content": "",
                    "error": format!("Skill '{skill_name}' not found. Available skills: [{}]", available.join(", "))
                }));
            }
        };

        match std::fs::read_to_string(&skill.path) {
            Ok(content) => Ok(serde_json::json!({
                "name": skill.name, "content": content,
            })),
            Err(e) => Ok(serde_json::json!({
                "name": skill_name, "content": "",
                "error": format!("Failed to read skill file: {e}")
            })),
        }
    }
}

pub struct AgentRuntime {
    config: AgentConfig,
    runtime_dir: PathBuf,
    bind_addr: SocketAddr,
    api_token: Option<String>,
    bus: Arc<dyn EventBus>,
    persistent_bus: Option<Arc<PersistentBus>>,
    sources: Arc<SourceRegistry>,
    skills: Arc<skill::SkillRegistry>,
    tools: Arc<tool::ToolRegistry>,
    skill_search: Arc<skill::SkillSearch>,
    skill_versions: Arc<skill::SkillVersionManager>,
    skill_hot_reload: Arc<skill::HotReloadManager>,
    skill_stop: Arc<AtomicBool>,
    skill_thread: Mutex<Option<std::thread::JoinHandle<()>>>,
    workflow_engine: Arc<WorkflowEngine>,
    plugin_loader: Mutex<PluginLoader>,
    cron_manager: Mutex<CronManager>,
    plugin_installer: Arc<PluginInstaller>,
    dlq: Arc<InMemoryDeadLetterQueue>,
    audit: Arc<AuditLogger>,
    event_store: Arc<EventStore>,
    observer_attached: AtomicBool,
    observer_subscription: Mutex<Option<event_bus::SubscriptionId>>,
    soul_runtime: Option<SoulRuntime>,
    soul_manager: Mutex<Option<SoulHotReloadManager>>,
    soul_stop: Arc<AtomicBool>,
    soul_thread: Mutex<Option<std::thread::JoinHandle<()>>>,
    backpressure_stop: Arc<AtomicBool>,
    backpressure_task: Mutex<Option<tokio::task::JoinHandle<()>>>,
    phase: AtomicU8,
    status: RwLock<RuntimeStatus>,
    transition_lock: Mutex<()>,
    shutdown_requested: AtomicBool,
    startup_pause: Duration,
    inflight_pipelines: Arc<AtomicUsize>,
    inflight_skills: Arc<AtomicUsize>,
    metrics: super::metrics::MetricsRegistry,
    capability_registry: RwLock<HashMap<String, Vec<CapabilityEntry>>>,
    chat_sender: Option<tokio::sync::mpsc::UnboundedSender<Event>>,
    /// Idle system coordination state.
    idle_coord: Option<Arc<IdleCoordination>>,
    /// Incubation manager for background idle threads.
    incubation_manager: Arc<IncubationManager>,
    /// LLM-instruction skills (SKILL.md frontmatter, Agent Skills standard).
    llm_skills: StdMutex<Vec<skill::SkillInfo>>,
    /// skm-core registry for cascade selection (None if init failed).
    skill_registry: Option<skm_core::SkillRegistry>,
    /// Cascade selector for skill matching (None if init failed).
    cascade_selector: Option<skm_select::CascadeSelector>,
    /// Registry for tool authorization requests (native macOS dialogs).
    auth_registry: Arc<tool::auth::AuthRegistry>,
    /// SQLite-backed session index (persists across restarts).
    session_store: Option<Arc<super::session_store::SessionStore>>,
    /// Notification center — user-facing alerts (critical/warning).
    notifications: Arc<notification::NotificationStore>,
    /// Agent runtime registry — manages agent instances and lifecycle.
    agent_registry: Arc<super::AgentRegistry>,
    /// Agent harness — orchestrates the ReAct loop for agent message processing.
    agent_harness: Arc<super::agent_harness::AgentHarness>,
}

impl AgentRuntime {
    #[must_use]
    pub fn bind_addr(&self) -> SocketAddr {
        self.bind_addr
    }

    #[must_use]
    pub fn api_token(&self) -> Option<&str> {
        self.api_token.as_deref()
    }

    #[must_use]
    pub fn risky_capabilities_enabled(&self) -> bool {
        self.config.security.risky_capabilities_enabled
    }

    #[must_use]
    pub fn auth_registry(&self) -> Arc<tool::auth::AuthRegistry> {
        Arc::clone(&self.auth_registry)
    }

    #[must_use]
    pub fn runtime_dir(&self) -> &Path {
        &self.runtime_dir
    }

    #[must_use]
    pub fn session_store(&self) -> Option<&super::session_store::SessionStore> {
        self.session_store.as_ref().map(|arc| arc.as_ref())
    }

    #[must_use]
    pub fn plugin_installer(&self) -> Arc<PluginInstaller> {
        Arc::clone(&self.plugin_installer)
    }

    #[must_use]
    pub fn dlq(&self) -> Arc<InMemoryDeadLetterQueue> {
        Arc::clone(&self.dlq)
    }

    #[must_use]
    pub fn audit(&self) -> Arc<AuditLogger> {
        Arc::clone(&self.audit)
    }

    #[must_use]
    pub fn notifications(&self) -> Arc<notification::NotificationStore> {
        Arc::clone(&self.notifications)
    }

    #[must_use]
    pub fn agent_registry(&self) -> Arc<super::AgentRegistry> {
        Arc::clone(&self.agent_registry)
    }

    #[must_use]
    pub fn agent_harness(&self) -> Arc<super::agent_harness::AgentHarness> {
        Arc::clone(&self.agent_harness)
    }

    #[must_use]
    pub fn event_store(&self) -> Arc<EventStore> {
        Arc::clone(&self.event_store)
    }

    #[must_use]
    pub fn soul_runtime(&self) -> Option<SoulRuntime> {
        self.soul_runtime.clone()
    }

    #[must_use]
    pub fn inject_skill_context(&self, context: kernel::context::SkillContext) -> kernel::context::SkillContext {
        match &self.soul_runtime {
            Some(soul) => soul.inject_skill_context(context),
            None => context,
        }
    }

    #[must_use]
    pub fn inject_pipeline_context(
        &self,
        context: kernel::context::PipelineContext,
    ) -> kernel::context::PipelineContext {
        match &self.soul_runtime {
            Some(soul) => soul.inject_pipeline_context(context),
            None => context,
        }
    }

    #[must_use]
    pub fn inject_tool_context(&self, context: kernel::context::ToolContext) -> kernel::context::ToolContext {
        match &self.soul_runtime {
            Some(soul) => soul.inject_tool_context(context),
            None => context,
        }
    }

    #[must_use]
    pub fn phase(&self) -> RuntimePhase {
        RuntimePhase::from_u8(self.phase.load(Ordering::Acquire))
    }

    pub async fn status(&self) -> RuntimeStatus {
        *self.status.read().await
    }

    #[must_use]
    pub fn is_ready(&self) -> bool {
        self.phase() == RuntimePhase::Phase5
    }

    #[must_use]
    pub fn is_live(&self) -> bool {
        match self.status.try_read() {
            Ok(guard) => *guard != RuntimeStatus::Shutdown,
            Err(_) => true,
        }
    }

    #[must_use]
    pub fn sources(&self) -> Arc<SourceRegistry> {
        Arc::clone(&self.sources)
    }

    #[must_use]
    pub fn skills(&self) -> Arc<skill::SkillRegistry> {
        Arc::clone(&self.skills)
    }

    #[must_use]
    pub fn tools(&self) -> Arc<tool::ToolRegistry> {
        Arc::clone(&self.tools)
    }

    #[must_use]
    pub fn skill_search(&self) -> Arc<skill::SkillSearch> {
        Arc::clone(&self.skill_search)
    }

    #[must_use]
    pub fn skill_versions(&self) -> Arc<skill::SkillVersionManager> {
        Arc::clone(&self.skill_versions)
    }

    #[must_use]
    pub fn skills_dir(&self) -> PathBuf {
        super::skill_sync::aman_data_dir().join("skills")
    }

    pub fn reload_skills_now(&self) -> AmanResult<skill::HotReloadReport> {
        self.skill_hot_reload.reload_once()
    }

    /// Returns all LLM-instruction skills discovered on disk.
    #[must_use]
    pub fn llm_skills(&self) -> Vec<skill::SkillInfo> {
        self.llm_skills.lock().unwrap().clone()
    }

    /// Load a skill's full SKILL.md body content by name (Level 2 of Progressive Disclosure).
    /// Returns `None` if no skill with that name is installed.
    #[must_use]
    pub fn read_skill(&self, name: &str) -> Option<String> {
        let skills = self.llm_skills.lock().unwrap();
        let path = skills.iter().find(|s| s.name == name)?.path.clone();
        let raw = std::fs::read_to_string(&path).ok()?;
        Some(skill::formatting::strip_frontmatter(&raw).trim().to_owned())
    }

    /// Use the cascade selector to find the top-1 matching skill for `text`.
    ///
    /// Returns the full SKILL.md content of the highest-confidence skill whose
    /// confidence is `Medium` or higher. This is Level 2 of Progressive
    /// Disclosure — the system pre-loads the best-matching skill so the LLM
    /// doesn't need multiple `read_skill` tool calls while avoiding conflicting
    /// instructions from multiple skills.
    ///
    /// Returns an empty vec when:
    /// - The selector is not initialized,
    /// - Selection fails,
    /// - No skill exceeds the confidence threshold.
    #[must_use]
    pub fn select_skills_for_text(&self, text: &str) -> Vec<String> {
        let registry = match self.skill_registry.as_ref() {
            Some(r) => r,
            None => {
                tracing::debug!("select_skills_for_text: no skill_registry (None)");
                return vec![];
            }
        };
        let selector = match self.cascade_selector.as_ref() {
            Some(s) => s,
            None => {
                tracing::debug!("select_skills_for_text: no cascade_selector (None)");
                return vec![];
            }
        };

        // Log available skills in the registry for diagnostics
        let catalog_len = pollster::block_on(registry.len());
        tracing::debug!("select_skills_for_text: registry has {catalog_len} skills, query=\"{text}\"");

        let ctx = skm_select::SelectionContext::new();
        let outcome = match pollster::block_on(selector.select(text, registry, &ctx)) {
            Ok(o) => o,
            Err(e) => {
                tracing::warn!("select_skills_for_text: cascade select error: {e}");
                return vec![];
            }
        };

        tracing::debug!(
            "select_skills_for_text: cascade used {:?}, {} results, latency={:?}",
            outcome.strategies_used,
            outcome.selected.len(),
            outcome.total_latency,
        );
        for r in &outcome.selected {
            tracing::debug!(
                "  -> skill={}, confidence={:?}, score={}, strategy={}",
                r.skill.as_ref(),
                r.confidence,
                r.score,
                r.strategy,
            );
        }

        // Top-1: only return the single highest-confidence skill at Medium+ level.
        let result: Vec<String> = outcome
            .selected
            .into_iter()
            .filter(|r| r.confidence >= skm_select::Confidence::Medium)
            .max_by_key(|r| r.confidence as u8)
            .and_then(|r| {
                let skill_name = r.skill.as_ref();
                let skills = self.llm_skills.lock().unwrap();
                let skill = skills.iter().find(|s| s.name == skill_name)?;
                let msg = skill::formatting::build_skill_activation_message(skill);
                if msg.is_none() {
                    tracing::warn!(
                        "select_skills_for_text: cascade matched \"{skill_name}\" but build_skill_activation_message returned None"
                    );
                }
                msg
            })
            .into_iter()
            .collect();

        if result.is_empty() {
            tracing::debug!("select_skills_for_text: no skill met Medium+ threshold");
        } else {
            tracing::info!("select_skills_for_text: activating skill");
        }

        result
    }

    #[must_use]
    pub fn workflow_engine(&self) -> Arc<WorkflowEngine> {
        Arc::clone(&self.workflow_engine)
    }

    pub async fn plugin_loader(&self) -> tokio::sync::MutexGuard<'_, PluginLoader> {
        self.plugin_loader.lock().await
    }

    #[instrument(skip(self), fields(event_id = %event.id, source = %event.source, event_type = ?event.event_type))]
    pub async fn publish_event(&self, event: kernel::event::Event) -> AmanResult<()> {
        self.bus.publish(event).await
    }

    #[instrument(skip(self))]
    pub fn bus_metrics(&self) -> event_bus::BusMetrics {
        self.bus.metrics()
    }

    /// Log a config change audit record with the details of what changed.
    #[instrument(skip(self), fields(fields = ?changed_fields))]
    pub fn log_config_change(&self, operator: &str, changed_fields: &[String]) {
        self.audit.record(
            operator,
            "config.set",
            "config",
            "ok",
            changed_fields.join(","),
        );
    }

    #[must_use]
    pub fn inflight_pipelines(&self) -> usize {
        self.inflight_pipelines.load(Ordering::Acquire)
    }

    #[must_use]
    pub fn inflight_skills(&self) -> usize {
        self.inflight_skills.load(Ordering::Acquire)
    }

    /// Returns a clone of the chat message sender, if available.
    #[must_use]
    pub fn chat_sender(&self) -> Option<tokio::sync::mpsc::UnboundedSender<Event>> {
        self.chat_sender.clone()
    }

    #[must_use]
    pub fn metrics(&self) -> &super::metrics::MetricsRegistry {
        &self.metrics
    }

    // ---------------------------------------------------------------------------
    // Capability registry
    // ---------------------------------------------------------------------------

    /// Returns the list of currently available capability names.
    /// Before Phase 2, returns an empty array.
    #[must_use]
    pub async fn get_capabilities(&self) -> Vec<String> {
        self.capability_registry
            .read()
            .await
            .keys()
            .cloned()
            .collect()
    }

    /// Returns detailed capability entries.
    #[must_use]
    pub async fn get_capability_entries(&self) -> HashMap<String, Vec<CapabilityEntry>> {
        self.capability_registry.read().await.clone()
    }

    /// Check if a specific capability is available.
    pub async fn has_capability(&self, capability: &str) -> bool {
        // The chat capability is available whenever a chat-platform source is
        // configured, even if no plugin explicitly registers it.
        if capability == "chat" {
            if self.chat_sender.is_some() {
                return true;
            }
        }
        self.capability_registry
            .read()
            .await
            .contains_key(capability)
    }

    /// Scans all loaded plugins and refreshes the capability registry.
    /// Called from Phase 2 and on plugin hot-load/unload.
    pub async fn refresh_capabilities(&self) -> AmanResult<()> {
        let mut registry = self.capability_registry.write().await;
        let old_caps: Vec<String> = registry.keys().cloned().collect();

        let mut new_registry: HashMap<String, Vec<CapabilityEntry>> = HashMap::new();
        let raw_caps = {
            let loader = self.plugin_loader.lock().await;
            let caps = loader.collect_capabilities();
            drop(loader);
            caps
        };
        for (capability, plugins) in &raw_caps {
            let entries: Vec<CapabilityEntry> = plugins
                .iter()
                .map(|(plugin, version)| CapabilityEntry {
                    capability: capability.clone(),
                    plugin: plugin.clone(),
                    version: version.clone(),
                    status: CapabilityStatus::Healthy,
                })
                .collect();
            new_registry.insert(capability.clone(), entries);
        }
        // When the chat-platform source is configured, always register the
        // "chat" capability regardless of plugin declarations.
        if self.chat_sender.is_some() {
            new_registry.entry("chat".to_owned()).or_insert_with(|| {
                vec![CapabilityEntry {
                    capability: "chat".to_owned(),
                    plugin: "chat-platform".to_owned(),
                    version: "0.1.0".to_owned(),
                    status: CapabilityStatus::Healthy,
                }]
            });
        }

        *registry = new_registry;

        let new_caps: Vec<String> = registry.keys().cloned().collect();
        drop(registry);

        // Publish change events for newly added capabilities
        for cap in &new_caps {
            if !old_caps.contains(cap) {
                self.publish_capability_event("capability_available", cap, None)
                    .await;
            }
        }
        // Publish removal events for capabilities that disappeared
        for cap in &old_caps {
            if !new_caps.contains(cap) {
                self.publish_capability_event("capability_removed", cap, None)
                    .await;
            }
        }
        // Publish full registry update event (does not enter WAL)
        let event = Event::new(
            "runtime:capability_registry",
            EventType::Custom("capability_registry_updated".to_owned()),
            json!({
                "available": new_caps,
                "added": new_caps.iter().filter(|c| !old_caps.contains(c)).cloned().collect::<Vec<_>>(),
                "removed": old_caps.iter().filter(|c| !new_caps.contains(c)).cloned().collect::<Vec<_>>(),
            }),
        );
        let _ = self.bus.publish(event).await;
        Ok(())
    }

    /// Publish a capability-related event to the event bus.
    async fn publish_capability_event(
        &self,
        event_type: &str,
        capability: &str,
        reason: Option<&str>,
    ) {
        let mut payload = json!({
            "capability": capability,
        });
        if let Some(reason) = reason {
            payload["reason"] = json!(reason);
        }
        let event = Event::new(
            "runtime:capability",
            EventType::Custom(event_type.to_owned()),
            payload,
        );
        let _ = self.bus.publish(event).await;
    }

    /// Update the SOUL file content and trigger a hot reload.
    pub async fn update_soul(&self, content: &str) -> AmanResult<()> {
        let mut slot = self.soul_manager.lock().await;
        let manager = slot.as_mut().ok_or_else(|| Error::ConfigInvalid {
            message: "no SOUL configured".to_owned(),
        })?;
        std::fs::write(manager.soul_file(), content)?;
        let _ = manager.reload_now()?;
        Ok(())
    }

    pub fn enqueue_dlq(
        &self,
        event: kernel::event::Event,
        reason: impl Into<String>,
        ttl_days: u64,
    ) -> AmanResult<String> {
        self.dlq.enqueue(event, reason, ttl_days)
    }

    #[instrument(skip(self))]
    pub async fn start(&self) -> AmanResult<()> {
        let _guard = self.transition_lock.lock().await;
        self.ensure_observer_subscribed().await?;
        self.ensure_soul_watching().await?;
        self.ensure_skill_watching().await?;
        self.ensure_backpressure_watching().await?;

        let current = *self.status.read().await;
        match current {
            RuntimeStatus::Ready => return Ok(()),
            RuntimeStatus::ShuttingDown | RuntimeStatus::Shutdown => {
                return Err(Error::InvalidStateTransition {
                    message: "runtime is shutting down".to_owned(),
                });
            }
            RuntimeStatus::Starting => return Ok(()),
            RuntimeStatus::New => {}
        }

        self.shutdown_requested.store(false, Ordering::Release);
        *self.status.write().await = RuntimeStatus::Starting;
        self.phase.store(RuntimePhase::Phase0 as u8, Ordering::Release);

        tracing::info!("runtime start: Phase0");
        self.bump_phase(RuntimePhase::Phase0).await?;
        tracing::info!("runtime start: Phase05");
        self.bump_phase(RuntimePhase::Phase05).await?;
        tracing::info!("runtime start: Phase1");
        self.bump_phase(RuntimePhase::Phase1).await?;
        tracing::info!("runtime start: Phase2");
        self.bump_phase(RuntimePhase::Phase2).await?;
        tracing::info!("runtime start: Phase3");
        self.bump_phase(RuntimePhase::Phase3).await?;
        tracing::info!("runtime start: Phase4");
        self.bump_phase(RuntimePhase::Phase4).await?;
        tracing::info!("runtime start: Phase5");
        self.bump_phase(RuntimePhase::Phase5).await?;

        *self.status.write().await = RuntimeStatus::Ready;
        Ok(())
    }

    async fn ensure_observer_subscribed(&self) -> AmanResult<()> {
        if self
            .observer_attached
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return Ok(());
        }
        let subscription_filter = event_bus::SubscriptionFilter::default();
        let handler = Box::new(StoreAllEventsHandler {
            store: Arc::clone(&self.event_store),
            session_store: self.session_store.clone(),
        });
        let id = self.bus.subscribe(subscription_filter, handler).await?;
        *self.observer_subscription.lock().await = Some(id);
        Ok(())
    }

    async fn ensure_soul_watching(&self) -> AmanResult<()> {
        let Some(runtime) = self.soul_runtime.clone() else {
            return Ok(());
        };

        let mut slot = self.soul_thread.lock().await;
        if slot.is_some() {
            return Ok(());
        }

        let Some(mut manager) = self.soul_manager.lock().await.take() else {
            return Ok(());
        };

        let stop = Arc::clone(&self.soul_stop);
        let bus = Arc::clone(&self.bus);
        let audit = Arc::clone(&self.audit);
        let handle = tokio::runtime::Handle::current();

        let join = std::thread::spawn(move || {
            while !stop.load(Ordering::Acquire) {
                match runtime.poll_once(&mut manager, Duration::from_millis(200)) {
                    Ok(Some(event)) => {
                        let name = event
                            .payload
                            .get("name")
                            .and_then(|value| value.as_str())
                            .unwrap_or("soul");
                        audit.record("system", "soul.changed", name, "ok", "");
                        handle.spawn({
                            let bus = Arc::clone(&bus);
                            async move {
                                let _ = bus.publish(event).await;
                            }
                        });
                    }
                    Ok(None) => {}
                    Err(error) => {
                        audit.record("system", "soul.changed", "soul", "error", error.to_string());
                        std::thread::sleep(Duration::from_millis(200));
                    }
                }
            }
        });

        *slot = Some(join);
        Ok(())
    }

    async fn ensure_skill_watching(&self) -> AmanResult<()> {
        let mut slot = self.skill_thread.lock().await;
        if slot.is_some() {
            return Ok(());
        }

        let stop = Arc::clone(&self.skill_stop);
        stop.store(false, Ordering::Release);
        let bus = Arc::clone(&self.bus);
        let audit = Arc::clone(&self.audit);
        let hot_reload = Arc::clone(&self.skill_hot_reload);
        let handle = tokio::runtime::Handle::current();
        let join = std::thread::spawn(move || {
            let _ = hot_reload.start_watching();
            while !stop.load(Ordering::Acquire) {
                match hot_reload.poll_once(Duration::from_millis(200)) {
                    Ok(Some(report)) => {
                        audit.record("system", "skill.reload", "skills", "ok", "");
                        handle.spawn({
                            let bus = Arc::clone(&bus);
                            let event = Event::new(
                                "skill:hot_reload",
                                EventType::SkillReloaded,
                                json!({
                                    "inserted": report.inserted,
                                    "updated_same_version": report.updated_same_version,
                                    "updated_new_version": report.updated_new_version,
                                    "removed": report.removed,
                                }),
                            );
                            async move {
                                let _ = bus.publish(event).await;
                            }
                        });
                    }
                    Ok(None) => {}
                    Err(error) => {
                        audit.record("system", "skill.reload", "skills", "error", error.to_string());
                        std::thread::sleep(Duration::from_millis(200));
                    }
                }
            }
            hot_reload.stop_watching();
        });

        *slot = Some(join);
        Ok(())
    }

    async fn stop_soul_watching(&self) {
        if self.soul_runtime.is_none() {
            return;
        }
        self.soul_stop.store(true, Ordering::Release);
        if let Some(join) = self.soul_thread.lock().await.take() {
            let _ = tokio::task::spawn_blocking(move || join.join()).await;
        }
    }

    async fn stop_skill_watching(&self) {
        self.skill_stop.store(true, Ordering::Release);
        if let Some(join) = self.skill_thread.lock().await.take() {
            let _ = tokio::task::spawn_blocking(move || join.join()).await;
        }
    }

    async fn ensure_backpressure_watching(&self) -> AmanResult<()> {
        let mut slot = self.backpressure_task.lock().await;
        if slot.is_some() {
            return Ok(());
        }
        self.backpressure_stop.store(false, Ordering::Release);
        let stop = Arc::clone(&self.backpressure_stop);
        let bus = Arc::clone(&self.bus);
        let sources = Arc::clone(&self.sources);
        let audit = Arc::clone(&self.audit);
        let join = tokio::spawn(async move {
            let mut last_level: Option<kernel::types::BackpressureLevel> = None;
            loop {
                if stop.load(Ordering::Acquire) {
                    break;
                }
                let metrics = bus.metrics();
                let level = metrics.backpressure_level;
                if last_level != Some(level) {
                    let _ = sources.apply_backpressure(level).await;
                    audit.record(
                        "system",
                        "backpressure.apply",
                        "sources",
                        "ok",
                        format!("{level:?}"),
                    );
                    last_level = Some(level);
                }
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
        });
        *slot = Some(join);
        Ok(())
    }

    async fn stop_backpressure_watching(&self) {
        self.backpressure_stop.store(true, Ordering::Release);
        if let Some(join) = self.backpressure_task.lock().await.take() {
            let _ = join.await;
        }
    }

    #[instrument(skip(self))]
    pub async fn shutdown(&self) -> AmanResult<()> {
        self.shutdown_requested.store(true, Ordering::Release);

        let _guard = self.transition_lock.lock().await;

        let current = *self.status.read().await;
        if current == RuntimeStatus::Shutdown {
            return Ok(());
        }
        *self.status.write().await = RuntimeStatus::ShuttingDown;

        self.bump_shutdown_phase(RuntimePhase::Phase5).await?;
        self.bump_shutdown_phase(RuntimePhase::Phase4).await?;
        self.bump_shutdown_phase(RuntimePhase::Phase3).await?;
        self.bump_shutdown_phase(RuntimePhase::Phase2).await?;
        self.bump_shutdown_phase(RuntimePhase::Phase1).await?;
        self.bump_shutdown_phase(RuntimePhase::Phase05).await?;
        self.bump_shutdown_phase(RuntimePhase::Phase0).await?;

        self.stop_soul_watching().await;
        self.stop_skill_watching().await;
        self.stop_backpressure_watching().await;

        *self.status.write().await = RuntimeStatus::Shutdown;
        Ok(())
    }

    async fn bump_phase(&self, phase: RuntimePhase) -> AmanResult<()> {
        if self.shutdown_requested.load(Ordering::Acquire) && phase != RuntimePhase::Phase0 {
            self.bump_shutdown_phase(self.phase()).await?;
            *self.status.write().await = RuntimeStatus::Shutdown;
            return Err(Error::InvalidStateTransition {
                message: "startup interrupted by shutdown".to_owned(),
            });
        }

        if !self.startup_pause.is_zero() {
            tokio::time::sleep(self.startup_pause).await;
        }

        tracing::info!(?phase, "bump_phase enter");
        match phase {
            RuntimePhase::Phase0 => {
                self.phase.store(RuntimePhase::Phase0 as u8, Ordering::Release);
            }
            RuntimePhase::Phase05 => {
                self.phase.store(RuntimePhase::Phase05 as u8, Ordering::Release);
            }
            RuntimePhase::Phase1 => {
                if let Some(persistent) = &self.persistent_bus {
                    let _ = persistent.recover_from_wal().await?;
                    let _ = persistent.recover_from_overflow()?;
                }
                self.phase.store(RuntimePhase::Phase1 as u8, Ordering::Release);
            }
            RuntimePhase::Phase2 => {
                let _ = self.skill_hot_reload.reload_once()?;
                tracing::info!("Phase2: plugin_loader.lock");
                {
                    let _loader = self.plugin_loader.lock().await;
                }
                tracing::info!("Phase2: refresh_capabilities");
                let _ = self.refresh_capabilities().await;
                tracing::info!("Phase2: load agents from config");
                if let Ok(aman_cfg) = config::AmanConfig::from_default_path() {
                    let count = self.agent_registry.load_from_config(&aman_cfg).await;
                    tracing::info!(count, "agents loaded from config");
                }
                tracing::info!("Phase2: store");
                self.phase.store(RuntimePhase::Phase2 as u8, Ordering::Release);
            }
            RuntimePhase::Phase3 => {
                let _ = self.load_workflows_once();
                self.phase.store(RuntimePhase::Phase3 as u8, Ordering::Release);
            }
            RuntimePhase::Phase4 => {
                let snapshots = self.sources.list().await;
                for source in snapshots {
                    if self.shutdown_requested.load(Ordering::Acquire) {
                        break;
                    }
                    tracing::info!(id = %source.id, "starting source");
                    self.sources.start(&source.id).await?;
                    tracing::info!(id = %source.id, "source started");
                }
                self.phase.store(RuntimePhase::Phase4 as u8, Ordering::Release);
            }
            RuntimePhase::Phase5 => {
                self.phase.store(RuntimePhase::Phase5 as u8, Ordering::Release);
            }
        }
        Ok(())
    }

    fn load_workflows_once(&self) -> AmanResult<()> {
        let root = self.runtime_dir.join("workflows");
        let Ok(entries) = std::fs::read_dir(&root) else {
            return Ok(());
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            if !matches!(
                path.extension().and_then(|ext| ext.to_str()),
                Some("yaml") | Some("yml")
            ) {
                continue;
            }
            let content = std::fs::read_to_string(&path)?;
            let workflow = serde_yaml::from_str::<workflow::WorkflowDef>(&content).map_err(|error| {
                Error::ConfigInvalid {
                    message: format!("invalid workflow yaml {}: {error}", path.display()),
                }
            })?;
            let _ = self.workflow_engine.register_workflow(workflow);
        }
        Ok(())
    }

    async fn bump_shutdown_phase(&self, phase: RuntimePhase) -> AmanResult<()> {
        if !self.startup_pause.is_zero() {
            tokio::time::sleep(self.startup_pause).await;
        }

        match phase {
            RuntimePhase::Phase5 => {
                self.phase.store(RuntimePhase::Phase4 as u8, Ordering::Release);
            }
            RuntimePhase::Phase4 => {
                // Phase 4.5: stop idle system (IncubationManager, cancel idle workflows)
                let cancelled = self.incubation_manager.shutdown_all().await;
                if cancelled > 0 {
                    tracing::info!(cancelled, "phase4.5: cancelled idle incubation threads");
                }
                // Reset idle coordination if active (cancels any running idle workflow)
                if let Some(coord) = &self.idle_coord {
                    coord.reset_idle_signal().await;
                }

                let snapshots = self.sources.list().await;
                for source in snapshots {
                    let _ = self.sources.shutdown(&source.id).await;
                }
                let timeout = Duration::from_secs(
                    self.config
                        .runtime
                        .drain_timeout_sec
                        .min(self.config.runtime.tool_timeout_sec.saturating_sub(1)),
                );
                let started = tokio::time::Instant::now();
                loop {
                    let metrics = self.bus.metrics();
                    let pending = metrics.queue_depth.high
                        + metrics.queue_depth.normal
                        + metrics.queue_depth.low
                        + metrics.retry_queue_depth;
                    if pending == 0 {
                        break;
                    }
                    if started.elapsed() >= timeout {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
                self.phase.store(RuntimePhase::Phase3 as u8, Ordering::Release);
            }
            RuntimePhase::Phase3 => {
                self.phase.store(RuntimePhase::Phase2 as u8, Ordering::Release);
            }
            RuntimePhase::Phase2 => {
                {
                    let mut loader = self.plugin_loader.lock().await;
                    if let Err(error) = loader.unload_all().await {
                        self.audit.record(
                            "system",
                            "plugin.unload_all",
                            "plugins",
                            "error",
                            format!("{error:?}"),
                        );
                    }
                }
                // Clear agent registry during shutdown
                self.agent_registry.clear().await;
                tracing::info!("Phase2 shutdown: agent registry cleared");
                self.phase.store(RuntimePhase::Phase1 as u8, Ordering::Release);
            }
            RuntimePhase::Phase1 => {
                if let Some(persistent) = &self.persistent_bus {
                    let wal = persistent.wal();
                    let guard = wal
                        .lock()
                        .expect("persistent bus wal mutex should not be poisoned");
                    if let Some(offset) = guard.last_offset_written() {
                        if let Err(error) = guard.final_checkpoint(offset) {
                            self.audit.record(
                                "system",
                                "wal.final_checkpoint",
                                "event_bus",
                                "error",
                                format!("{error:?}"),
                            );
                        } else {
                            self.audit.record(
                                "system",
                                "wal.final_checkpoint",
                                "event_bus",
                                "ok",
                                format!("{offset}"),
                            );
                        }
                    }
                }
                self.phase.store(RuntimePhase::Phase05 as u8, Ordering::Release);
            }
            RuntimePhase::Phase05 => {
                self.phase.store(RuntimePhase::Phase0 as u8, Ordering::Release);
            }
            RuntimePhase::Phase0 => {
                self.phase.store(RuntimePhase::Phase0 as u8, Ordering::Release);
            }
        }
        Ok(())
    }

    pub async fn add_cron_job(&self, id: String, expression: String, caller: &str) -> AmanResult<()> {
        let ctx = source_context_for_cron();
        let job = CronSource::new(id, expression)?;
        self.cron_manager
            .lock()
            .await
            .add_with_caller(job, ctx, caller)
            .await
    }

    pub async fn update_cron_job(
        &self,
        id: &str,
        config: serde_json::Value,
        caller: &str,
    ) -> AmanResult<()> {
        self.cron_manager
            .lock()
            .await
            .update_with_caller(id, config, caller)
            .await
    }

    pub async fn remove_cron_job(&self, id: &str, caller: &str) -> AmanResult<()> {
        self.cron_manager.lock().await.remove_with_caller(id, caller)
    }

    pub async fn enable_plugin(&self, plugin_name: &str) -> AmanResult<()> {
        self.plugin_loader.lock().await.enable_plugin(plugin_name)
    }

    pub async fn disable_plugin(&self, plugin_name: &str) -> AmanResult<()> {
        self.plugin_loader.lock().await.disable_plugin(plugin_name)
    }

    pub async fn uninstall_plugin(&self, plugin_name: &str) -> AmanResult<()> {
        let mut loader = self.plugin_loader.lock().await;
        self.plugin_installer
            .uninstall(Some(&mut loader), plugin_name)
            .await
    }
}

struct RuntimePluginRegistrar {
    skills: Arc<skill::SkillRegistry>,
    tools: Arc<tool::ToolRegistry>,
    source_ids: StdMutex<BTreeSet<String>>,
    hooks: Arc<hook::HookRegistry>,
}

impl RuntimePluginRegistrar {
    fn new(
        skills: Arc<skill::SkillRegistry>,
        tools: Arc<tool::ToolRegistry>,
        hooks: Arc<hook::HookRegistry>,
    ) -> Self {
        Self {
            skills,
            tools,
            source_ids: StdMutex::new(BTreeSet::new()),
            hooks,
        }
    }
}

impl PluginExportRegistrar for RuntimePluginRegistrar {
    fn register_skill(&self, skill: Arc<dyn Skill>) -> AmanResult<()> {
        self.skills.register(skill)
    }

    fn unregister_skill(&self, skill_name: &str) -> AmanResult<()> {
        self.skills.unregister(skill_name)
    }

    fn register_tool(&self, tool: Arc<dyn Tool>) -> AmanResult<()> {
        self.tools.register(tool)
    }

    fn unregister_tool(&self, tool_name: &str) -> AmanResult<()> {
        self.tools.unregister(tool_name)
    }

    fn register_event_source(&self, source: Arc<dyn EventSource>) -> AmanResult<()> {
        self.source_ids
            .lock()
            .expect("plugin source ids lock")
            .insert(source.id().to_owned());
        Ok(())
    }

    fn unregister_event_source(&self, source_id: &str) -> AmanResult<()> {
        let _ = self
            .source_ids
            .lock()
            .expect("plugin source ids lock")
            .remove(source_id);
        Ok(())
    }

    fn register_hook(&self, hook: Arc<dyn Hook>) -> AmanResult<()> {
        self.hooks.register(hook)
    }

    fn unregister_hook(&self, hook_name: &str) -> AmanResult<()> {
        self.hooks.unregister(hook_name);
        Ok(())
    }
}

struct StoreAllEventsHandler {
    store: Arc<EventStore>,
    session_store: Option<Arc<super::session_store::SessionStore>>,
}

#[async_trait::async_trait]
impl event_bus::EventHandler for StoreAllEventsHandler {
    async fn handle(&self, event: kernel::event::Event) -> AmanResult<()> {
        // Persist session-related events to JSONL so conversation history
        // survives gateway restarts.
        let session_id = event.payload.get("session_id").and_then(|v| v.as_str());
        if let Some(sid) = session_id {
            if let Some(ref store) = self.session_store {
                let entry = serde_json::json!({
                    "event_id": event.id.to_string(),
                    "event_type": format!("{:?}", event.event_type),
                    "source": event.source,
                    "timestamp_ms": event.timestamp.as_millis(),
                    "payload": event.payload,
                });
                let _ = store.append_session_event(sid, &entry);
            }
        }

        self.store.record(event);
        Ok(())
    }
}

fn default_runtime_dir() -> PathBuf {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let pid = std::process::id();
    std::env::temp_dir().join(format!("aman-runtime-{pid}-{nanos}"))
}

fn build_runtime_bus(
    config: &AgentConfig,
    runtime_dir: &Path,
    max_queue_size: usize,
    discard_hook: Option<DiscardHook>,
) -> AmanResult<(RuntimeBus, Option<Arc<PersistentBus>>)> {
    let mut inner_bus = InMemoryBus::new(InMemoryBusConfig {
        max_queue_size,
        ..InMemoryBusConfig::default()
    });
    if let Some(hook) = discard_hook {
        inner_bus.set_discard_hook(hook);
    }
    let bus = Arc::new(inner_bus);

    match config.event_bus.mode {
        BusMode::InMemory => Ok((RuntimeBus::InMemory(bus), None)),
        BusMode::Persistent => {
            let wal_dir = runtime_dir.join("wal");
            std::fs::create_dir_all(&wal_dir)?;
            let wal = WriteAheadLog::new(&wal_dir, 1024 * 1024 * 1024, WalSync::Fsync)?;
            let persistent = Arc::new(PersistentBus::new(Arc::clone(&bus), wal));
            Ok((
                RuntimeBus::Persistent {
                    bus,
                    persistent: Arc::clone(&persistent),
                },
                Some(persistent),
            ))
        }
    }
}

fn resolve_secrets_in_config(
    config: AgentConfig,
    runtime_dir: &Path,
    audit: &AuditLogger,
) -> AmanResult<AgentConfig> {
    let mut value = serde_json::to_value(&config)?;
    let mut resolver = SecretResolver::new().with_config(secret_resolver_config(runtime_dir));
    #[cfg(test)]
    if let Some(backend) = test_secret_backend() {
        resolver = resolver.with_backend(backend);
    }
    resolver = resolver
        .with_backend(Box::new(OnePasswordCliBackend::default()))
        .with_backend(Box::new(AwsSecretsManagerCliBackend::default()))
        .with_backend(Box::new(VaultCliBackend::default()))
        .with_backend(Box::new(EnvSecretBackend));
    let _ = resolver.resolve_all(&mut value)?;

    for record in resolver.audit_log() {
        audit.record(
            "system",
            "secret.resolve",
            "config",
            "ok",
            format!(
                "keys={}, trigger={}",
                record.affected_keys.join(","),
                record.trigger_source,
            ),
        );
    }

    Ok(serde_json::from_value(value)?)
}

fn secret_resolver_config(runtime_dir: &Path) -> SecretResolverConfig {
    let mut config = SecretResolverConfig::default();
    let key_hex = std::env::var("AMAN_SECRET_CACHE_KEY_HEX")
        .ok()
        .map(|v| v.trim().to_owned())
        .filter(|v| !v.is_empty());
    if let Some(key_hex) = key_hex {
        let dir = runtime_dir.join("secret-cache");
        let _ = std::fs::create_dir_all(&dir);
        config.cache_fallback = Some(SecretCacheFallbackConfig {
            dir,
            ttl_ms: 7 * 24 * 60 * 60 * 1000,
            key_hex,
        });
    }
    config
}

#[cfg(test)]
mod tests {
    use super::resolve_secrets_in_config;
    use config::AgentConfig;
    use super::super::AuditLogger;

    #[test]
    fn resolve_secrets_replaces_placeholders() {
        let mut config = AgentConfig::default();
        config.source.watch_patterns = vec!["${test://pattern}".to_owned()];
        let audit = AuditLogger::new(100);
        let resolved = resolve_secrets_in_config(config, &std::env::temp_dir(), &audit).expect("resolve");
        assert_eq!(resolved.source.watch_patterns, vec!["resolved".to_owned()]);
    }
}

#[cfg(test)]
fn test_secret_backend() -> Option<Box<dyn SecretBackend>> {
    Some(Box::new(TestSecretBackend))
}

#[cfg(test)]
struct TestSecretBackend;

#[cfg(test)]
impl SecretBackend for TestSecretBackend {
    fn get(&self, key: &str) -> AmanResult<Option<String>> {
        if key == "test://pattern" {
            return Ok(Some("resolved".to_owned()));
        }
        Ok(None)
    }

    fn priority(&self) -> u32 {
        1_000
    }

    fn name(&self) -> &'static str {
        "test"
    }
}

enum RuntimeBus {
    InMemory(Arc<InMemoryBus>),
    Persistent {
        bus: Arc<InMemoryBus>,
        persistent: Arc<PersistentBus>,
    },
}

#[async_trait::async_trait]
impl EventBus for RuntimeBus {
    async fn publish(&self, event: kernel::event::Event) -> AmanResult<()> {
        match self {
            Self::InMemory(bus) => bus.publish(event).await,
            Self::Persistent { persistent, .. } => persistent.publish(event).await,
        }
    }

    async fn subscribe(
        &self,
        filter: event_bus::SubscriptionFilter,
        handler: Box<dyn event_bus::EventHandler>,
    ) -> AmanResult<event_bus::SubscriptionId> {
        match self {
            Self::InMemory(bus) => bus.subscribe(filter, handler).await,
            Self::Persistent { bus, .. } => bus.subscribe(filter, handler).await,
        }
    }

    async fn unsubscribe(&self, id: event_bus::SubscriptionId) {
        match self {
            Self::InMemory(bus) => bus.unsubscribe(id).await,
            Self::Persistent { bus, .. } => bus.unsubscribe(id).await,
        }
    }

    fn metrics(&self) -> event_bus::BusMetrics {
        match self {
            Self::InMemory(bus) => bus.metrics(),
            Self::Persistent { bus, .. } => bus.metrics(),
        }
    }

    fn backpressure_level(&self) -> kernel::types::BackpressureLevel {
        match self {
            Self::InMemory(bus) => bus.backpressure_level(),
            Self::Persistent { bus, .. } => bus.backpressure_level(),
        }
    }

    fn can_poll(&self) -> bool {
        match self {
            Self::InMemory(bus) => bus.can_poll(),
            Self::Persistent { bus, .. } => bus.can_poll(),
        }
    }

    fn try_dequeue(&self) -> Option<kernel::event::Event> {
        match self {
            Self::InMemory(bus) => bus.try_dequeue(),
            Self::Persistent { bus, .. } => bus.try_dequeue(),
        }
    }

    async fn wait_for_event(
        &self,
        timeout: std::time::Duration,
    ) -> Result<kernel::event::Event, event_bus::WaitForEventTimeout> {
        match self {
            Self::InMemory(bus) => bus.wait_for_event(timeout).await,
            Self::Persistent { bus, .. } => bus.wait_for_event(timeout).await,
        }
    }
}

fn source_context_for_cron() -> kernel::context::SourceContext {
    kernel::context::SourceContext {
        base: kernel::context::BaseContext::new(kernel::types::TraceId::new()),
        source_name: Some("cron_manager".to_owned()),
    }
}

/// Build LlmConfig from AmanConfig (providers, model / agents) + Keychain API key.
///
/// Create an LLM provider from configuration.
///
/// Reads the default `aman.model` first. If not set, falls back to the first
/// configured agent. If no agent is found either, falls back to environment variables.
/// Uses `api_type` from the matching provider config to select the implementation.
fn create_llm_provider() -> Arc<dyn LlmProvider> {
    if let Ok(aman) = config::AmanConfig::from_default_path() {
        // Priority 0: top-level llm.api_type overrides per-provider api_type
        let llm_api_type = aman.llm.as_ref().map(|l| l.api_type.as_str());

        // Priority 1: default model config
        if let Some(model) = &aman.model {
            let provider_key = &model.provider;
            let p = aman.providers.get(provider_key);
            let base_url = p.map(|p| p.base_url.clone())
                .unwrap_or_else(|| model.base_url.clone());
            let api_key = get_llm_api_key_or_inline(provider_key, p);
            let api_type = llm_api_type
                .or_else(|| p.map(|p| p.api_type.as_str()))
                .unwrap_or("openai");
            tracing::info!(
                provider = %provider_key,
                model = %model.default,
                api_key_len = api_key.len(),
                api_type = %api_type,
                "create_llm_provider: using default model config"
            );
            return build_provider(provider_key, &api_key, &base_url, api_type);
        }

        // Priority 2: first configured agent (provider + model)
        for (_key, agent) in &aman.agents {
            if let Some(p) = aman.providers.get(&agent.provider) {
                let api_key = get_llm_api_key_or_inline(&agent.provider, Some(p));
                let api_type = llm_api_type
                    .or_else(|| Some(p.api_type.as_str()))
                    .unwrap_or("openai");
                tracing::info!(
                    agent_key = %_key,
                    provider = %agent.provider,
                    model = %agent.model,
                    api_key_len = api_key.len(),
                    api_type = %api_type,
                    "create_llm_provider: using agent config"
                );
                return build_provider(&agent.provider, &api_key, &p.base_url, api_type);
            }
            tracing::warn!(
                agent_key = %_key,
                provider = %agent.provider,
                "create_llm_provider: agent provider not found in config"
            );
        }
    }
    // Priority 3: environment variables
    let api_key = std::env::var("AMAN_DEFAULT_API_KEY").unwrap_or_default();
    let base_url = std::env::var("AMAN_DEFAULT_BASE_URL")
        .unwrap_or_else(|_| "https://api.openai.com/v1".to_owned());
    tracing::info!(
        api_key_len = api_key.len(),
        "create_llm_provider: using env var fallback"
    );
    build_provider("default", &api_key, &base_url, "openai")
}

/// Build the appropriate LlmProvider based on api_type.
fn build_provider(_provider_key: &str, api_key: &str, base_url: &str, api_type: &str) -> Arc<dyn LlmProvider> {
    match api_type {
        "openai" => {
            Arc::new(llm_provider_openai::LlmOpenaiProvider::new(
                api_key.to_owned(),
                base_url.to_owned(),
            ))
        }
        other => {
            tracing::error!(
                api_type = %other,
                "unsupported LLM provider type, falling back to openai"
            );
            Arc::new(llm_provider_openai::LlmOpenaiProvider::new(
                api_key.to_owned(),
                base_url.to_owned(),
            ))
        }
    }
}

/// Get API key for a provider from Keychain, falling back to env var.
fn get_llm_api_key(provider_key: &str) -> String {
    let backend = KeychainBackend;
    if let Ok(Some(key)) = backend.get(&format!("aman.providers.{provider_key}.api_key")) {
        return key;
    }
    let env_var = format!(
        "AMAN_PROVIDER_{}_API_KEY",
        provider_key
            .to_ascii_uppercase()
            .chars()
            .map(|c| if c.is_alphanumeric() || c == '_' { c } else { '_' })
            .collect::<String>()
    );
    std::env::var(env_var).unwrap_or_default()
}

/// Get API key checking Keychain → env var → inline provider config.
fn get_llm_api_key_or_inline(
    provider_key: &str,
    provider_config: Option<&config::ProviderConfig>,
) -> String {
    let key = get_llm_api_key(provider_key);
    if !key.is_empty() {
        return key;
    }
    if let Some(config) = provider_config {
        if let Some(ref inline) = config.api_key {
            if !inline.is_empty() {
                return inline.clone();
            }
        }
    }
    String::new()
}

/// Build an optional skm-core registry + cascade selector for skill matching.
///
/// Uses trigger-only strategy (no embedding model needed). Returns `(None, None)`
/// if the skills directory has no valid SKILL.md files or the registry fails to
/// initialize. This is non-fatal — the runtime falls back to listing all skills
/// in the prompt and letting the LLM decide.
fn build_skill_selector(
    skills_dir: &Path,
) -> (Option<skm_core::SkillRegistry>, Option<skm_select::CascadeSelector>) {
    let registry = match pollster::block_on(skm_core::SkillRegistry::new(&[skills_dir])) {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(error = %e, "failed to create skm-core SkillRegistry");
            return (None, None);
        }
    };

    let trigger = match pollster::block_on(skm_select::TriggerStrategy::from_registry(&registry)) {
        Ok(t) => t,
        Err(e) => {
            tracing::warn!(error = %e, "failed to build trigger strategy");
            return (Some(registry), None);
        }
    };

    let mut cascade_builder = skm_select::CascadeSelector::builder()
        .with_triggers(trigger);

    // Attempt to add semantic strategy (embedding similarity) as second cascade
    // level. If the embedding model fails to initialize (e.g. first launch model
    // download, OOM, unsupported platform), gracefully fall back to trigger-only.
    if let Some(semantic) = build_semantic_strategy(&registry) {
        tracing::info!("semantic cascade strategy initialized");
        cascade_builder = cascade_builder.with_semantic(
            semantic.0,
            semantic.1,
            semantic.2,
        );
    } else {
        tracing::info!("semantic cascade strategy not available — trigger only");
    }

    (Some(registry), Some(cascade_builder.build()))
}

/// Attempt to build a [`SemanticStrategy`] for the cascade selector.
///
/// Uses BGE-M3 (fastembed ONNX, 1024-dim) for local embedding inference.
/// The embedding index is cached to `~/.aman/cache/embeddings.bin` to avoid
/// re-embedding all skills on every startup. Cache is invalidated automatically
/// when skill content changes (tracked via content_hash in skm-core).
///
/// Returns `None` if:
/// - The BGE-M3 model cannot be loaded (first launch downloads ~100MB)
/// - The embedding index fails to build or load from cache
/// - The platform does not support ONNX inference
fn build_semantic_strategy(
    registry: &skm_core::SkillRegistry,
) -> Option<(
    Arc<dyn skm_embed::EmbeddingProvider>,
    skm_embed::EmbeddingIndex,
    skm_select::SemanticConfig,
)> {
    let cache_dir = super::skill_sync::aman_data_dir().join("cache");
    let _ = std::fs::create_dir_all(&cache_dir);
    let cache_path = cache_dir.join("embeddings.bin");

    let provider = Arc::new(skm_embed::BgeM3Provider::new().ok()?);

    let index = match skm_embed::EmbeddingIndex::load_cached(&cache_path, registry) {
        Ok(Some(cached)) => {
            tracing::info!("loaded cached embedding index ({} skills)", cached.len());
            cached
        }
        _ => {
            tracing::info!("building embedding index from skill registry...");
            let idx = pollster::block_on(
                skm_embed::EmbeddingIndex::build(registry, provider.as_ref(), Default::default()),
            )
            .ok()?;
            if let Err(e) = idx.save(&cache_path) {
                tracing::warn!(error = %e, "failed to cache embedding index");
            }
            tracing::info!("embedding index built ({} skills)", idx.len());
            idx
        }
    };

    let config = skm_select::SemanticConfig::default()
        .with_top_k(3)
        .with_min_score(0.65);

    Some((provider, index, config))
}
