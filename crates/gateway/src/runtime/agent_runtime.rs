// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

use config::{AgentConfig, BusMode};
use event_bus::{DiscardHook, EventBus, InMemoryBus, InMemoryBusConfig};
use kernel::context::ToolContext;
use kernel::event::{Event, EventType};
use kernel::hook::Hook;
use kernel::llm::LlmProvider;
use memory::{MemoryConfig, YantrikdbProvider};
use memory_store::MemoryStorePlugin;
use info_hub::InfoHubPlugin;
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
use std::sync::OnceLock;
use std::time::Duration;
use tokio::sync::{Mutex, RwLock};
use super::{AuditLogger, EventStore};
use super::SoulRuntime;
use soul::SoulHotReloadManager;
use tracing::instrument;
use workflow::WorkflowEngine;

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
    predefined_dir: PathBuf,
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
            predefined_dir: PathBuf::from("predefined"),
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

    #[must_use]
    pub fn with_predefined_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.predefined_dir = dir.into();
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
        let index_store = Arc::new(persistence::IndexStore::new(persistence::IndexRecord {
            project: persistence::IndexStore::CANONICAL_PROJECT.to_string(),
            index_version: persistence::IndexStore::INDEX_VERSION,
            build_hash: option_env!("AMAN_BUILD_HASH").unwrap_or(env!("CARGO_PKG_VERSION")).to_string(),
        })?);
        let audit = Arc::new(AuditLogger::new(2_000));

        let config = resolve_secrets_in_config(self.config, &self.runtime_dir, &audit)?;

        // Extract self-module config before self.config is consumed.
        let self_module_config = config.self_module.clone();
        let predefined_dir = self.predefined_dir.clone();

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
        // Sync built-in plugins from repo to ~/.aman/plugins/ (preserves user modifications)
        if let Err(e) = super::plugin_sync::sync_builtin_plugins() {
            tracing::error!(error = %e, "failed to sync built-in plugins");
        }
        // Sync built-in configs from repo to ~/.aman/ (preserves user modifications)
        if let Err(e) = super::config_sync::sync_builtin_configs() {
            tracing::error!(error = %e, "failed to sync built-in configs");
        }
        // Seed predefined agents into ~/.aman/agents/ for new users.
        let _seeded_agents = super::agent_seed::seed_builtin_agents();
        // Discover any agents manually copied into ~/.aman/agents/.
        let _discovered = super::agent_seed::discover_filesystem_agents();
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
        // Register code agent tools for available CLI coding tools (claude, codex, etc.)
        tool::install_code_agent_tools(&tools);
        // Register read_skill tool so the LLM can load SKILL.md instructions on demand.
        // Store the Arc so we can wire agent_registry after its creation (line ~574+).
        let read_skill_tool = Arc::new(ReadSkillTool {
            skills: llm_skills.clone(),
            agent_registry: OnceLock::new(),
        });
        let _ = tools.register(Arc::clone(&read_skill_tool) as Arc<dyn Tool>);
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
        super::session::SessionManager::register_workflow(&workflow_engine);

        let cron_manager = CronManager::with_runtime_dir(self.runtime_dir.clone());
        let plugin_installer = Arc::new(PluginInstaller::new(self.runtime_dir.join("plugins")));
        let event_store = Arc::new(EventStore::new(2_000, 500));

        let auth_registry = Arc::new(tool::auth::AuthRegistry::new());

        // Load config early so plugins that need LLM config can use it.
        let aman_cfg = config::AmanConfig::from_default_path()
            .map_err(|e| tracing::warn!(error = %e, "failed to load config, using defaults"))
            .ok();

        // ── Self-module bridge (Python prompt builders) ────────────
        let self_bridge = if self_module_config.enabled {
            let bridge = super::self_bridge::SelfBridge::new(
                &self_module_config,
                &predefined_dir,
            );
            tracing::info!(
                python = %self_module_config.python,
                script = %predefined_dir.join(&self_module_config.bridge_script).display(),
                "SelfBridge: Python prompt builders enabled"
            );
            bridge
        } else {
            super::self_bridge::SelfBridge::disabled()
        };

        // Load the built-in memory-store plugin (in-memory keyword-based provider).
        let memory_store_plugin = MemoryStorePlugin::new();
        let memory_store_candidate = PluginCandidate {
            manifest: PluginManifest {
                name: "memory-store".to_owned(),
                version: memory_store_plugin.version().clone(),
                depends_on: vec![],
                lifecycle: PluginLifecycleConfig { auto_start: true },
                exports: PluginExports {
                    memory_providers: vec!["in-memory".to_owned()],
                    ..Default::default()
                },
                config_schema: None,
                isolation: Some(PluginIsolationMode::InProcess),
                subprocess: None,
                wasm_path: None,
                capabilities: vec![],
                ui: None,
                runtime: None,
                min_version: None,
                entrypoint: None,
            },
            plugin: Box::new(memory_store_plugin),
            isolation: PluginIsolationMode::InProcess,
            subprocess: None,
            wasm_module_bytes: None,
        };

        // Resolve LLM config for info-hub (first try info_hub.llm, fall back to memory.llm)
        let resolve_llm = |llm: &serde_json::Value| -> Option<info_hub::ai::LlmConfig> {
            let provider_key = llm.get("provider")?.as_str()?;
            let model_id = llm.get("model")?.as_str()?;
            let p = aman_cfg.as_ref()?.providers.get(provider_key)?;
            let api_model = p.models.iter()
                .find(|m| m.id == model_id)
                .map(|m| m.model_id.clone())
                .unwrap_or_else(|| model_id.to_string());
            let api_key = get_llm_api_key_or_inline(provider_key, Some(p));
            Some(info_hub::ai::LlmConfig {
                base_url: p.base_url.clone(),
                api_key: Some(api_key),
                model: api_model,
            })
        };
        let info_hub_llm = aman_cfg
            .as_ref()
            .and_then(|c| c.info_hub.as_ref())
            .and_then(|v| v.get("llm"))
            .and_then(&resolve_llm)
            .or_else(|| {
                let llm = aman_cfg.as_ref()?.memory.as_ref()?.llm.as_ref()?;
                resolve_llm(&serde_json::to_value(llm).ok()?)
            });
        let info_hub_plugin = InfoHubPlugin::from_config_with_llm(
            aman_cfg.as_ref().and_then(|c| c.info_hub.as_ref()),
            info_hub_llm,
        );
        let info_hub_candidate = PluginCandidate {
            manifest: PluginManifest {
                name: "info-hub".to_owned(),
                version: info_hub_plugin.version().clone(),
                depends_on: vec![],
                lifecycle: PluginLifecycleConfig { auto_start: true },
                exports: PluginExports {
                    ..Default::default()
                },
                config_schema: None,
                isolation: Some(PluginIsolationMode::InProcess),
                subprocess: None,
                wasm_path: None,
                capabilities: vec![],
                ui: None,
                runtime: None,
                min_version: None,
                entrypoint: None,
            },
            plugin: Box::new(info_hub_plugin),
            isolation: PluginIsolationMode::InProcess,
            subprocess: None,
            wasm_module_bytes: None,
        };

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

        // ── Notification store ─────────────────────────────────────
        let notifications = Arc::new(notification::NotificationStore::new(500));

        // ── Subscribe notification subscriber ─────────────────────
        let notif_sub = notification::NotificationSubscriber::new(Arc::clone(&notifications));
        let _ = pollster::block_on(bus.subscribe(
            event_bus::SubscriptionFilter::default(),
            Box::new(notif_sub),
        ));

        // ── Agent registry ──────────────────────────────────────────
        let agent_registry = Arc::new(super::AgentRegistry::new(Arc::clone(&bus)));
        // Wire agent_registry into ReadSkillTool for per-agent skill filtering.
        read_skill_tool.set_agent_registry(Arc::clone(&agent_registry));

        // ── Plugin loading ──────────────────────────────────────────
        let mut all_candidates = vec![memory_store_candidate, info_hub_candidate];
        all_candidates.extend(self.extra_plugins);

        // Discover subprocess plugins from ~/.aman/plugins/
        let home_for_plugins = std::env::var("HOME")
            .or_else(|_| std::env::var("USERPROFILE"))
            .unwrap_or_else(|_| "/tmp".to_owned());
        let plugins_dir = PathBuf::from(&home_for_plugins).join(".aman").join("plugins");
        let discovered = plugin::discover_subprocess_plugins(&plugins_dir);
        if !discovered.is_empty() {
            tracing::info!(count = discovered.len(), dir = %plugins_dir.display(), "discovered subprocess plugins");
            all_candidates.extend(discovered);
        }
        let hook_registry = Arc::new(hook::HookRegistry::new());
        let memory_provider_registry = Arc::new(memory::MemoryProviderRegistry::new());
        let rpc_handler = Arc::new(RuntimeJsonRpcHandler::new(
            Arc::clone(&agent_registry),
            Arc::clone(&bus),
        ));
        let mut plugin_loader = PluginLoader::new(Arc::new(RuntimePluginRegistrar::new(
            Arc::clone(&skills),
            Arc::clone(&tools),
            Arc::clone(&hook_registry),
            Arc::clone(&memory_provider_registry),
        ))).with_method_handler(rpc_handler);
        if let Err(e) = pollster::block_on(plugin_loader.load_all(all_candidates)) {
            tracing::error!(error = %e, "failed to load built-in plugins");
        }

        // ── Per-agent resources ─────────────────────────────────────
        // Each agent gets its own SessionStore, YantrikDB, and LlmProvider.
        let home = std::env::var("HOME")
            .or_else(|_| std::env::var("USERPROFILE"))
            .unwrap_or_else(|_| "/tmp".to_owned());
        let embedding_config = aman_cfg
            .as_ref()
            .map(resolve_embedding_config)
            .unwrap_or_default();

        if let Some(ref cfg) = aman_cfg {
            for (agent_id, entry) in &cfg.agents {
                let agents_dir = PathBuf::from(&home)
                    .join(".aman")
                    .join("agents")
                    .join(agent_id);

                if !entry.enabled {
                    pollster::block_on(agent_registry.set_session_store(agent_id, None));
                    continue;
                }

                // -- SessionStore --
                let db_path = agents_dir.join("sessions.db");
                let sessions_dir = agents_dir.join("sessions");
                let store = match super::session_store::SessionStore::open(&db_path, &sessions_dir) {
                    Ok(s) => {
                        tracing::info!(path = %db_path.display(), agent = %agent_id, "session store opened");
                        Some(Arc::new(s))
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, agent = %agent_id, "session store init skipped");
                        None
                    }
                };
                pollster::block_on(agent_registry.set_session_store(agent_id, store));

                // -- MemoryProvider (YantrikDB) --
                let memory_dir = agents_dir.join("memory");
                if memory_dir.is_file() {
                    let bak = memory_dir.with_extension("memory.bak");
                    let _ = std::fs::rename(&memory_dir, &bak);
                    let _ = std::fs::create_dir_all(&memory_dir);
                    let new_path = memory_dir.join("yantrik.db");
                    if let Err(e) = std::fs::rename(&bak, &new_path) {
                        tracing::warn!(from = %bak.display(), to = %new_path.display(), error = %e, "failed to migrate yantrikdb");
                    } else {
                        tracing::info!(path = %new_path.display(), "migrated yantrikdb to memory/yantrik.db");
                    }
                } else {
                    std::fs::create_dir_all(&memory_dir)
                        .unwrap_or_else(|e| tracing::warn!(path = %memory_dir.display(), error = %e, "failed to create memory dir"));
                }
                let yantrik_path = memory_dir.join("yantrik.db");
                let memory_config = MemoryConfig {
                    db_path: yantrik_path.to_string_lossy().into_owned(),
                    agent_id: agent_id.clone(),
                    embedding: embedding_config.clone(),
                };
                match YantrikdbProvider::open(&memory_config) {
                    Ok(yantrikdb) => {
                        pollster::block_on(agent_registry.set_memory_provider(agent_id, Arc::new(yantrikdb)));
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, agent = %agent_id, "yantrikdb open failed, agent will have no memory");
                    }
                }

                // -- LlmProvider --
                if let Some(llm) = create_per_agent_llm_provider(cfg, agent_id, entry) {
                    pollster::block_on(agent_registry.set_llm_provider(agent_id, llm));
                }

                // -- TraceStore (task execution traces) --
                let traces_dir = agents_dir.join("traces");
                match persistence::JsonlTraceStore::open(&traces_dir) {
                    Ok(ts) => {
                        pollster::block_on(agent_registry.set_trace_store(agent_id, Arc::new(ts)));
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, agent = %agent_id, "trace store init skipped");
                    }
                }
            }
        }

        // ── Agent harness (ReAct loop orchestrator) ──────────────────
        let compressor_config = super::history_compressor::CompressorConfig {
            tail_budget_ratio: config.compression.tail_budget_ratio,
            protect_head_messages: config.compression.protect_head_messages,
            min_tail_messages: config.compression.min_tail_messages,
            max_tool_args_chars: config.compression.max_tool_args_chars,
            dedup_tool_outputs: config.compression.dedup_tool_outputs,
            summarize_tool_results: config.compression.summarize_tool_results,
            truncate_tool_args: config.compression.truncate_tool_args,
        };
        let agent_harness = Arc::new(super::agent_harness::AgentHarness::new(
            Arc::clone(&agent_registry),
            Arc::clone(&tools),
            Arc::clone(&bus),
            Box::new(DefaultPromptPipeline),
            Box::new(InMemorySessionHistory::new()),
            Box::new(kernel::budget::DefaultTokenBudgetPolicy::new()),
            Box::new(super::agent_harness::FirstEnabledAgentRouter),
            compressor_config,
        ));

        // ── Session manager ──────────────────────────────────────────
        let session_manager = Arc::new(super::session::SessionManager::new(
            Arc::clone(&workflow_engine),
            Arc::clone(&agent_registry),
            Arc::clone(&agent_harness),
            Arc::clone(&bus),
            Arc::clone(&audit),
        ));

        // ── Reflection runner (QueueDrained → session_extract) ────────
        let reflection_runner = Arc::new(super::reflection::ReflectionRunner::new());
        reflection_runner.set_agent_registry(Arc::clone(&agent_registry));
        let memory_llm_cfg = aman_cfg
            .as_ref()
            .and_then(|c| c.memory.as_ref())
            .and_then(|m| m.llm.clone());
        if let Some(cfg) = memory_llm_cfg.clone() {
            reflection_runner.set_memory_llm(cfg);
        }

        // Subscribe to QueueDrained events on the global bus (handles both
        // busy→empty transitions and cold-start QueueDrained from AgentIdleManager).
        {
            struct ReflectionSub {
                runner: Arc<super::reflection::ReflectionRunner>,
            }
            #[async_trait::async_trait]
            impl event_bus::EventHandler for ReflectionSub {
                async fn handle(&self, event: kernel::event::Event) -> kernel::AmanResult<()> {
                    self.runner.handle(event).await
                }
            }
            let _ = pollster::block_on(bus.subscribe(
                event_bus::SubscriptionFilter {
                    event_types: Some(vec![kernel::event::EventType::QueueDrained]),
                    sources: None,
                    priorities: None,
                    payload_match: None,
                },
                Box::new(ReflectionSub {
                    runner: reflection_runner,
                }),
            ));
        }

        // ── Sleep runner (Idle kind=Sleep → cognitive housekeeping) ──────
        let sleep_runner = Arc::new(super::sleep::SleepRunner::new());
        sleep_runner.set_agent_registry(Arc::clone(&agent_registry));
        let sleep_cfg = aman_cfg
            .as_ref()
            .map(|c| c.runtime.idle.sleep.clone())
            .unwrap_or_default();
        sleep_runner.set_sleep_config(sleep_cfg);
        if let Some(cfg) = memory_llm_cfg {
            sleep_runner.set_memory_llm(cfg);
        }

        // Subscribe to Idle events on the global bus (filtered to kind="sleep" in handle())
        {
            struct SleepSub {
                runner: Arc<super::sleep::SleepRunner>,
            }
            #[async_trait::async_trait]
            impl event_bus::EventHandler for SleepSub {
                async fn handle(&self, event: kernel::event::Event) -> kernel::AmanResult<()> {
                    self.runner.handle(event).await
                }
            }
            let _ = pollster::block_on(bus.subscribe(
                event_bus::SubscriptionFilter {
                    event_types: Some(vec![kernel::event::EventType::Idle]),
                    sources: None,
                    priorities: None,
                    payload_match: None,
                },
                Box::new(SleepSub {
                    runner: sleep_runner,
                }),
            ));
        }

        // ── Exploration runner (Idle kind=Exploration → external discovery) ──
        let exploration_runner = Arc::new(super::exploration::ExplorationRunner::new());
        exploration_runner.set_agent_registry(Arc::clone(&agent_registry));
        let exploration_cfg = aman_cfg
            .as_ref()
            .map(|c| c.runtime.idle.exploration.clone())
            .unwrap_or_default();
        exploration_runner.set_exploration_config(exploration_cfg);
        exploration_runner.set_global_bus(Arc::clone(&bus) as Arc<dyn event_bus::EventBus>);

        // Pass info-hub config so ExplorationRunner can use its adapters
        if let Some(ref cfg) = aman_cfg
            && let Some(info_hub_value) = &cfg.info_hub
                && let Ok(info_cfg) = serde_json::from_value::<info_hub::config::InfoHubConfig>(info_hub_value.clone()) {
                    exploration_runner.set_info_hub_config(info_cfg);
                }

        // Subscribe to Idle events on the global bus (filtered to kind="exploration" in handle())
        {
            struct ExplorationSub {
                runner: Arc<super::exploration::ExplorationRunner>,
            }
            #[async_trait::async_trait]
            impl event_bus::EventHandler for ExplorationSub {
                async fn handle(&self, event: kernel::event::Event) -> kernel::AmanResult<()> {
                    self.runner.handle(event).await
                }
            }
            let _ = pollster::block_on(bus.subscribe(
                event_bus::SubscriptionFilter {
                    event_types: Some(vec![kernel::event::EventType::Idle]),
                    sources: None,
                    priorities: None,
                    payload_match: None,
                },
                Box::new(ExplorationSub {
                    runner: exploration_runner,
                }),
            ));
        }

        // ── Meditation runner (idle depth 100+) ──────────────────────
        let meditation_runner = Arc::new(super::meditation::MeditationRunner::new());
        meditation_runner.set_agent_registry(Arc::clone(&agent_registry));
        let meditation_cfg = aman_cfg
            .as_ref()
            .map(|c| c.runtime.idle.meditation.clone())
            .unwrap_or_default();
        meditation_runner.set_meditation_config(meditation_cfg);
        meditation_runner.set_global_bus(Arc::clone(&bus) as Arc<dyn event_bus::EventBus>);

        {
            struct MeditationSub {
                runner: Arc<super::meditation::MeditationRunner>,
            }
            #[async_trait::async_trait]
            impl event_bus::EventHandler for MeditationSub {
                async fn handle(&self, event: kernel::event::Event) -> kernel::AmanResult<()> {
                    self.runner.handle(event).await
                }
            }
            let _ = pollster::block_on(bus.subscribe(
                event_bus::SubscriptionFilter {
                    event_types: Some(vec![kernel::event::EventType::Idle]),
                    sources: None,
                    priorities: None,
                    payload_match: None,
                },
                Box::new(MeditationSub {
                    runner: meditation_runner,
                }),
            ));
        }

        // ── Incubation runner (idle depth 200+) ──────────────────────
        let incubation_runner = Arc::new(super::incubation_runner::IncubationRunner::new());
        incubation_runner.set_agent_registry(Arc::clone(&agent_registry));
        let incubation_cfg = aman_cfg
            .as_ref()
            .map(|c| c.runtime.idle.incubation.clone())
            .unwrap_or_default();
        incubation_runner.set_incubation_config(incubation_cfg);
        incubation_runner.set_global_bus(Arc::clone(&bus) as Arc<dyn event_bus::EventBus>);

        {
            struct IncubationSub {
                runner: Arc<super::incubation_runner::IncubationRunner>,
            }
            #[async_trait::async_trait]
            impl event_bus::EventHandler for IncubationSub {
                async fn handle(&self, event: kernel::event::Event) -> kernel::AmanResult<()> {
                    self.runner.handle(event).await
                }
            }
            let _ = pollster::block_on(bus.subscribe(
                event_bus::SubscriptionFilter {
                    event_types: Some(vec![kernel::event::EventType::Idle]),
                    sources: None,
                    priorities: None,
                    payload_match: None,
                },
                Box::new(IncubationSub {
                    runner: incubation_runner,
                }),
            ));
        }

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

                // Prefer the session's owning agent from the event payload,
                // falling back to first-enabled-agent routing for events
                // that lack an explicit agent_id (e.g. external sources).
                let target_agent_id = event.payload.get("agent_id")
                    .and_then(|v| v.as_str())
                    .filter(|id| !id.is_empty());
                let agent = match target_agent_id {
                    Some(aid) => self.agent_harness.resolve_agent(aid).await,
                    None => self.agent_harness.resolve_first_enabled_agent(&text).await,
                };
                let agent = match agent {
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

        // ── Subscribe agent:reply_ready → update session state & persistence ──
        struct SessionReplyHandler {
            session_manager: Arc<super::session::SessionManager>,
        }
        #[async_trait::async_trait]
        impl event_bus::EventHandler for SessionReplyHandler {
            async fn handle(&self, event: kernel::event::Event) -> kernel::AmanResult<()> {
                let session_id = event.payload.get("session_id")
                    .and_then(|v| v.as_str()).unwrap_or("");
                let agent_id = event.payload.get("agent_id")
                    .and_then(|v| v.as_str()).unwrap_or("");
                let reply = event.payload.get("reply")
                    .and_then(|v| v.as_str()).unwrap_or("");

                if !session_id.is_empty() && !agent_id.is_empty() {
                    self.session_manager.handle_reply(session_id, agent_id, reply).await;
                }
                Ok(())
            }
        }
        let _ = pollster::block_on(bus.subscribe(
            event_bus::SubscriptionFilter {
                event_types: Some(vec![
                    EventType::Custom("agent:reply_ready".to_owned()),
                    EventType::Custom("agent:reply_interrupted".to_owned()),
                ]),
                sources: None,
                priorities: None,
                payload_match: None,
            },
            Box::new(SessionReplyHandler {
                session_manager: Arc::clone(&session_manager),
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

        // ── Global script hooks (from ~/.aman/hooks/) ───────────
        struct ScriptHookEventHandler {
            runner: Arc<hook::ScriptHookRunner>,
        }
        #[async_trait::async_trait]
        impl event_bus::EventHandler for ScriptHookEventHandler {
            async fn handle(&self, event: kernel::event::Event) -> kernel::AmanResult<()> {
                let event_type = event.event_type.as_str().to_owned();
                let _ = self.runner.run(&event_type, &event.payload).await?;
                Ok(())
            }
        }

        let script_hook_runner = {
            let mut hooks = Vec::new();

            // 1. Auto-discover global hooks from ~/.aman/hooks/<name>/config.yaml
            let hooks_dir = super::skill_sync::aman_data_dir().join("hooks");
            let _ = std::fs::create_dir_all(&hooks_dir);
            let discovered = config::discover_hooks(&hooks_dir);
            let mut seen_names: std::collections::HashSet<String> = std::collections::HashSet::new();
            for cfg in &discovered {
                seen_names.insert(cfg.name.clone());
                let runtime = kernel::script::ScriptRuntime::new(
                    &cfg.runtime,
                    cfg.min_version.as_deref(),
                );
                if let Err(e) = runtime.check_available() {
                    tracing::warn!(name = %cfg.name, error = %e, "global hook runtime unavailable, skipping");
                    continue;
                }
                let script_path = if cfg.script.is_absolute() {
                    cfg.script.clone()
                } else {
                    std::env::current_dir().unwrap_or_default().join(&cfg.script)
                };
                hooks.push(hook::ScriptHook::new(
                    &cfg.name,
                    cfg.on.clone(),
                    script_path,
                    runtime,
                ));
            }

            // 2. Manually configured hooks from aman.yaml (same name overrides discovered).
            if let Ok(aman_cfg) = config::AmanConfig::from_default_path() {
                for cfg in &aman_cfg.hooks {
                    if seen_names.contains(&cfg.name) {
                        tracing::info!(name = %cfg.name, "manual hook config overrides discovered hook");
                    }
                    let runtime = kernel::script::ScriptRuntime::new(
                        &cfg.runtime,
                        cfg.min_version.as_deref(),
                    );
                    if let Err(e) = runtime.check_available() {
                        tracing::warn!(name = %cfg.name, error = %e, "configured hook runtime unavailable, skipping");
                        continue;
                    }
                    let script_path = if cfg.script.is_absolute() {
                        cfg.script.clone()
                    } else {
                        std::env::current_dir().unwrap_or_default().join(&cfg.script)
                    };
                    hooks.retain(|h: &hook::ScriptHook| h.name() != cfg.name);
                    hooks.push(hook::ScriptHook::new(
                        &cfg.name,
                        cfg.on.clone(),
                        script_path,
                        runtime,
                    ));
                }
            }

            tracing::info!(count = hooks.len(), "global script hooks loaded");
            Arc::new(hook::ScriptHookRunner::new(hooks))
        };

        // Global hooks subscribe to the global event bus.
        let _ = pollster::block_on(bus.subscribe(
            event_bus::SubscriptionFilter::default(),
            Box::new(ScriptHookEventHandler {
                runner: Arc::clone(&script_hook_runner),
            }),
        ));

        let sse_state = super::sse::new_sse_state();
        let runtime = Arc::new(AgentRuntime {
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
            index_store,
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
            llm_skills: StdMutex::new(llm_skills),
            skill_registry,
            cascade_selector,
            notifications,
            agent_registry,
            agent_harness,
            session_manager,
            shutdown_notify: tokio::sync::Notify::new(),
            self_bridge,
            sse_broadcast: sse_state,
        });
        Ok(runtime)
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
    agent_registry: OnceLock<Arc<super::AgentRegistry>>,
}

impl ReadSkillTool {
    fn set_agent_registry(&self, registry: Arc<super::AgentRegistry>) {
        let _ = self.agent_registry.set(registry);
    }

    /// Filter skills by the given agent's allowed_skills.
    /// Returns all skills if the agent has no allowed_skills restriction.
    fn skills_for_agent<'a>(&'a self, agent_id: &str) -> Vec<&'a skill::SkillInfo> {
        let allowed = self
            .agent_registry
            .get()
            .and_then(|reg| pollster::block_on(reg.get(agent_id)))
            .and_then(|inst| inst.descriptor.allowed_skills);
        match allowed {
            Some(ref list) => self.skills.iter().filter(|s| list.contains(&s.name)).collect(),
            None => self.skills.iter().collect(),
        }
    }
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

    async fn execute(&self, params: serde_json::Value, ctx: ToolContext) -> kernel::AmanResult<serde_json::Value> {
        let skill_name = match params.get("skill").and_then(|v| v.as_str()) {
            Some(s) if !s.is_empty() => s,
            _ => return Ok(serde_json::json!({
                "name": "", "content": "", "error": "Missing required parameter: skill"
            })),
        };

        // Determine which skills are visible to the calling agent.
        let agent_id = ctx.base.extensions.get("agent_id").and_then(|v| v.as_str()).unwrap_or("");
        let visible_skills = self.skills_for_agent(agent_id);

        let skill = match visible_skills.iter().find(|s| s.name == skill_name) {
            Some(s) => s,
            None => {
                let available: Vec<&str> = visible_skills.iter().map(|s| s.name.as_str()).collect();
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

/// JSON-RPC method handler for subprocess plugins.
/// Gives plugins access to AgentRegistry and EventBus.
struct RuntimeJsonRpcHandler {
    agent_registry: Arc<super::AgentRegistry>,
    bus: Arc<dyn EventBus>,
}

impl RuntimeJsonRpcHandler {
    fn new(agent_registry: Arc<super::AgentRegistry>, bus: Arc<dyn EventBus>) -> Self {
        Self { agent_registry, bus }
    }
}

#[async_trait::async_trait]
impl kernel::plugin::JsonRpcMethodHandler for RuntimeJsonRpcHandler {
    async fn handle_method(
        &self,
        plugin_name: &str,
        method: &str,
        params: serde_json::Value,
    ) -> kernel::AmanResult<serde_json::Value> {
        match method {
            "aman.get_agents" => {
                let agents = self.agent_registry.list().await;
                let result: Vec<serde_json::Value> = agents
                    .iter()
                    .map(|a| {
                        serde_json::json!({
                            "id": a.descriptor.agent_id,
                            "name": a.descriptor.display_name,
                            "status": a.status,
                        })
                    })
                    .collect();
                Ok(serde_json::json!({"agents": result}))
            }
            "aman.emit_event" => {
                let event_type = params
                    .get("event_type")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let payload = params.get("payload").cloned().unwrap_or(serde_json::Value::Null);
                let event = kernel::event::Event::new(
                    format!("plugin:{plugin_name}"),
                    kernel::event::EventType::Custom(event_type.to_owned()),
                    payload,
                );
                self.bus.publish(event).await?;
                Ok(serde_json::json!({"ok": true}))
            }
            "aman.push_work_item" => {
                let agent_id = params
                    .get("agent_id")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| kernel::Error::ConfigInvalid {
                        message: "missing agent_id".to_owned(),
                    })?;
                let title = params
                    .get("title")
                    .and_then(|v| v.as_str())
                    .unwrap_or("untitled");
                let description = params
                    .get("description")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let item = work::WorkItem {
                    id: work::WorkItemId::new(),
                    title: title.to_owned(),
                    description: description.to_owned(),
                    steps: None,
                    priority: work::Priority::Normal,
                    timeout: None,
                    context: std::collections::HashMap::new(),
                    notify_on_complete: true,
                    created_at: kernel::types::Timestamp::now(),
                };
                let ws = self.agent_registry.get_work_system(agent_id).await.ok_or_else(|| {
                    kernel::Error::NotFound {
                        name: format!("agent:{agent_id}"),
                    }
                })?;
                ws.push_work_item(item, work::WorkItemSource::Kanban {
                    board_id: plugin_name.to_owned(),
                    scheduler: "subprocess-plugin".to_owned(),
                }).await.map_err(|e| kernel::Error::Unrecoverable {
                    message: format!("push_work_item failed: {e:?}"),
                })?;
                Ok(serde_json::json!({"ok": true}))
            }
            "aman.subscribe_events" => {
                let events: Vec<String> = params
                    .get("events")
                    .and_then(|v| v.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|v| v.as_str().map(String::from))
                            .collect()
                    })
                    .unwrap_or_default();
                tracing::info!(plugin = %plugin_name, count = events.len(), "plugin subscribed to events");
                // Full event→plugin forwarding requires bridge access.
                // For now, plugins use aman.handle_route for HTTP-triggered flows
                // and aman.emit_event for outbound events.
                Ok(serde_json::json!({"ok": true}))
            }
            "aman.register_workflow" => {
                let name = params
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown");
                tracing::info!(plugin = %plugin_name, workflow = %name, "plugin registered workflow");
                // Workflow registration is handled by the WorkflowEngine.
                // For now, accept the definition and log it.
                Ok(serde_json::json!({"ok": true, "workflow": name}))
            }
            other => Err(kernel::Error::Unrecoverable {
                message: format!("unknown rpc method: {other}"),
            }),
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
    index_store: Arc<persistence::IndexStore>,
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
    /// LLM-instruction skills (SKILL.md frontmatter, Agent Skills standard).
    llm_skills: StdMutex<Vec<skill::SkillInfo>>,
    /// skm-core registry for cascade selection (None if init failed).
    skill_registry: Option<skm_core::SkillRegistry>,
    /// Cascade selector for skill matching (None if init failed).
    cascade_selector: Option<skm_select::CascadeSelector>,
    /// Registry for tool authorization requests (native macOS dialogs).
    auth_registry: Arc<tool::auth::AuthRegistry>,
    /// Notification center — user-facing alerts (critical/warning).
    notifications: Arc<notification::NotificationStore>,
    /// Agent runtime registry — manages agent instances and lifecycle.
    agent_registry: Arc<super::AgentRegistry>,
    /// Agent harness — orchestrates the ReAct loop for agent message processing.
    agent_harness: Arc<super::agent_harness::AgentHarness>,
    /// Session manager — orchestrates session lifecycle, OCC, persistence,
    /// and system prompt caching independently of the chat transport.
    session_manager: Arc<super::session::SessionManager>,
    /// Notified when `shutdown()` completes. Used by `main.rs` to exit the
    /// process after an HTTP-initiated shutdown (the signal handler path
    /// would otherwise never be reached).
    shutdown_notify: tokio::sync::Notify,
    /// Python self-module bridge for prompt building (Phase 2+).
    self_bridge: super::self_bridge::SelfBridge,
    /// SSE broadcast state — fans out events and snapshots to connected clients.
    sse_broadcast: Arc<super::sse::SseBroadcastState>,
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

    /// Return the session store for the given agent.
    #[must_use]
    pub fn session_store_for_agent(&self, agent_id: &str) -> Option<Arc<super::session_store::SessionStore>> {
        pollster::block_on(self.agent_registry.get_session_store(agent_id))
    }

    /// Look up the per-agent trace store.
    pub fn trace_store_for_agent(&self, agent_id: &str) -> Option<Arc<dyn kernel::trace::TraceStore>> {
        pollster::block_on(self.agent_registry.get_trace_store(agent_id))
    }

    /// Return the first available session store (backward compat).
    #[must_use]
    pub fn session_store(&self) -> Option<Arc<super::session_store::SessionStore>> {
        pollster::block_on(self.agent_registry.first_session_store())
    }

    /// Find the session store that owns `session_id`, searching all agents.
    #[must_use]
    pub fn find_session_store(&self, session_id: &str) -> Option<Arc<super::session_store::SessionStore>> {
        pollster::block_on(async {
            let stores = self.agent_registry.all_session_stores().await;
            for s in &stores {
                if s.has_session(session_id) {
                    return Some(Arc::clone(s));
                }
            }
            // Fall back to first store (backward compat for pre-migration sessions).
            self.agent_registry.first_session_store().await
        })
    }

    /// Restore a persisted session (searches all per-agent stores).
    pub async fn restore_chat_session(&self, session_id: &str) -> Option<()> {
        self.session_manager.restore_session(session_id).await
    }

    /// Access the session manager.
    #[must_use]
    pub fn session_manager(&self) -> &super::session::SessionManager {
        &self.session_manager
    }

    #[must_use]
    pub fn self_bridge(&self) -> &super::self_bridge::SelfBridge {
        &self.self_bridge
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
    pub fn index_store(&self) -> Arc<persistence::IndexStore> {
        Arc::clone(&self.index_store)
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
    /// When `allowed_skills` is `Some`, only skills in that list are considered.
    ///
    /// Returns an empty vec when:
    /// - The selector is not initialized,
    /// - Selection fails,
    /// - No skill exceeds the confidence threshold.
    #[must_use]
    pub fn select_skills_for_text(&self, text: &str, allowed_skills: Option<&[String]>) -> Vec<String> {
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

        // Apply per-agent allowed_skills filter, if specified.
        let result = match allowed_skills {
            Some(allowed) => result.into_iter().filter(|name| {
                // The result contains the skill activation message text; we need
                // to match it against allowed names. Each message starts with
                // the skill name, so we check containment.
                allowed.iter().any(|s| name.contains(s.as_str()))
            }).collect(),
            None => result,
        };

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

    #[must_use]
    pub fn plugin_routes(&self) -> Vec<axum::Router<()>> {
        pollster::block_on(async { self.plugin_loader.lock().await.collect_routes() })
    }

    /// Returns a reference to the shutdown notification channel.
    ///
    /// The notification is sent when [`shutdown()`] completes, regardless of
    /// whether shutdown was triggered via HTTP, signal, or any other path.
    /// Callers can `.await` on `notified()` to wait for shutdown completion.
    #[must_use]
    pub fn shutdown_notify(&self) -> &tokio::sync::Notify {
        &self.shutdown_notify
    }

    #[must_use]
    pub fn plugin_manifests(&self) -> Vec<PluginManifest> {
        pollster::block_on(async { self.plugin_loader.lock().await.loaded_manifests().into_iter().cloned().collect() })
    }

    #[instrument(skip(self, event), fields(event_id = %event.id, source = %event.source, event_type = ?event.event_type))]
    pub async fn publish_event(&self, event: kernel::event::Event) -> AmanResult<()> {
        self.bus.publish(event).await
    }

    /// Publish an event to a specific agent's local bus, falling back to
    /// the global bus if the agent has no dedicated local bus.
    #[instrument(skip(self, event), fields(event_id = %event.id, agent_id = %agent_id))]
    pub async fn publish_event_to_agent(
        &self,
        agent_id: &str,
        event: kernel::event::Event,
    ) -> AmanResult<()> {
        match self.agent_registry.get_local_bus(agent_id).await {
            Some(local_bus) => local_bus.publish(event).await,
            None => self.bus.publish(event).await,
        }
    }

    /// Subscribe per-agent script hooks to the given agent's local event bus.
    ///
    /// Discovers hooks from `~/.aman/agents/<agent_id>/hooks/` and subscribes
    /// them to the agent's local bus. Hooks placed here only receive that
    /// specific agent's events (e.g. `agent:busy`, `tool:completed`).
    pub async fn subscribe_per_agent_hooks(&self, agent_id: &str) {
        let hooks_dir = super::skill_sync::aman_data_dir()
            .join("agents")
            .join(agent_id)
            .join("hooks");
        let _ = std::fs::create_dir_all(&hooks_dir);
        let discovered = config::discover_hooks(&hooks_dir);
        if discovered.is_empty() {
            return;
        }

        let mut hooks = Vec::new();
        for cfg in &discovered {
            let runtime = kernel::script::ScriptRuntime::new(
                &cfg.runtime,
                cfg.min_version.as_deref(),
            );
            if let Err(e) = runtime.check_available() {
                tracing::warn!(name = %cfg.name, agent = %agent_id, error = %e, "agent hook runtime unavailable, skipping");
                continue;
            }
            let script_path = if cfg.script.is_absolute() {
                cfg.script.clone()
            } else {
                std::env::current_dir().unwrap_or_default().join(&cfg.script)
            };
            hooks.push(hook::ScriptHook::new(
                &cfg.name,
                cfg.on.clone(),
                script_path,
                runtime,
            ));
        }
        if hooks.is_empty() {
            return;
        }

        let runner = Arc::new(hook::ScriptHookRunner::new(hooks));
        let global_bus = Arc::clone(&self.bus) as Arc<dyn event_bus::EventBus>;
        struct AgentHookHandler {
            runner: Arc<hook::ScriptHookRunner>,
            global_bus: Arc<dyn event_bus::EventBus>,
        }
        #[async_trait::async_trait]
        impl event_bus::EventHandler for AgentHookHandler {
            async fn handle(&self, event: kernel::event::Event) -> kernel::AmanResult<()> {
                let event_type = event.event_type.as_str().to_owned();
                let prevented = self.runner.run(&event_type, &event.payload).await?;
                if !prevented {
                    // Bubble event up to the global bus so global hooks see it.
                    let _ = self.global_bus.publish(event).await;
                }
                Ok(())
            }
        }
        if let Some(local_bus) = self.agent_registry.get_local_bus(agent_id).await {
            let _ = local_bus
                .subscribe(
                    event_bus::SubscriptionFilter::default(),
                    Box::new(AgentHookHandler { runner, global_bus }),
                )
                .await;
            tracing::info!(agent = %agent_id, count = discovered.len(), "agent script hooks subscribed to local bus");
        }
    }

    #[instrument(skip(self))]
    pub fn bus_metrics(&self) -> event_bus::BusMetrics {
        self.bus.metrics()
    }

    /// Clone the global event bus (used by SSE broadcaster for subscription).
    #[must_use]
    pub fn bus_cloned(&self) -> Arc<dyn EventBus> {
        Arc::clone(&self.bus)
    }

    /// SSE broadcast state for streaming events to connected clients.
    #[must_use]
    pub(crate) fn sse_broadcast(&self) -> Arc<super::sse::SseBroadcastState> {
        Arc::clone(&self.sse_broadcast)
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
        // Chat is a built-in capability provided by the gateway.
        new_registry.entry("chat".to_owned()).or_insert_with(|| {
            vec![CapabilityEntry {
                capability: "chat".to_owned(),
                plugin: "gateway".to_owned(),
                version: "0.1.0".to_owned(),
                status: CapabilityStatus::Healthy,
            }]
        });

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
            agent_registry: Arc::clone(&self.agent_registry),
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
        self.shutdown_notify.notify_one();
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
                    // Subscribe per-agent script hooks to each agent's local bus.
                    for agent in self.agent_registry.list().await {
                        self.subscribe_per_agent_hooks(&agent.descriptor.agent_id).await;
                    }
                }
                tracing::info!("Phase2: store");
                self.phase.store(RuntimePhase::Phase2 as u8, Ordering::Release);
            }
            RuntimePhase::Phase3 => {
                let _ = self.load_workflows_once();
                self.phase.store(RuntimePhase::Phase3 as u8, Ordering::Release);
            }
            RuntimePhase::Phase4 => {
                // Start per-agent idle loops
                self.agent_registry.start_all_idle_loops().await;

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
        tracing::info!(?phase, "bump_shutdown_phase enter");
        if !self.startup_pause.is_zero() {
            tokio::time::sleep(self.startup_pause).await;
        }

        match phase {
            RuntimePhase::Phase5 => {
                self.phase.store(RuntimePhase::Phase4 as u8, Ordering::Release);
            }
            RuntimePhase::Phase4 => {
                // Stop agent idle/work systems before draining the event bus.
                // Otherwise agents keep generating sleep/think events, which
                // reset the drain stall detector and prolong shutdown.
                self.agent_registry.stop_idle_systems().await;

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
                let mut last_pending = 0usize;
                let mut stall_ticks = 0u32;
                loop {
                    let metrics = self.bus.metrics();
                    let pending = metrics.queue_depth.high
                        + metrics.queue_depth.normal
                        + metrics.queue_depth.low
                        + metrics.retry_queue_depth;
                    if pending == 0 {
                        break;
                    }
                    if pending == last_pending {
                        stall_ticks += 1;
                        // 2 s of no progress → events are stuck, stop waiting
                        if stall_ticks >= 40 {
                            tracing::warn!(
                                pending,
                                "Event bus drain stalled — {} events cannot be consumed, proceeding with shutdown",
                                pending
                            );
                            break;
                        }
                    } else {
                        stall_ticks = 0;
                        last_pending = pending;
                    }
                    if started.elapsed() >= timeout {
                        tracing::warn!(
                            pending,
                            "Event bus drain timed out after {:?} — {} events still pending",
                            timeout, pending
                        );
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
                tracing::info!("Phase4: event bus drain complete");
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
                tracing::info!("Phase2: plugin unload and agent registry clear complete");
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
                tracing::info!("Phase1: WAL checkpoint complete");
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
    memory_providers: Arc<memory::MemoryProviderRegistry>,
}

impl RuntimePluginRegistrar {
    fn new(
        skills: Arc<skill::SkillRegistry>,
        tools: Arc<tool::ToolRegistry>,
        hooks: Arc<hook::HookRegistry>,
        memory_providers: Arc<memory::MemoryProviderRegistry>,
    ) -> Self {
        Self {
            skills,
            tools,
            source_ids: StdMutex::new(BTreeSet::new()),
            hooks,
            memory_providers,
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

    fn register_memory_provider(&self, provider: Arc<dyn kernel::memory::MemoryProvider>) -> AmanResult<()> {
        self.memory_providers.register(provider)
    }

    fn unregister_memory_provider(&self, provider_name: &str) -> AmanResult<()> {
        self.memory_providers.unregister(provider_name);
        Ok(())
    }
}

struct StoreAllEventsHandler {
    store: Arc<EventStore>,
    agent_registry: Arc<super::AgentRegistry>,
}

#[async_trait::async_trait]
impl event_bus::EventHandler for StoreAllEventsHandler {
    async fn handle(&self, event: kernel::event::Event) -> AmanResult<()> {
        // Persist session-related events to JSONL so conversation history
        // survives gateway restarts.
        let session_id = event.payload.get("session_id").and_then(|v| v.as_str());
        if let Some(sid) = session_id {
            let agent_id = event
                .payload
                .get("agent_id")
                .and_then(|v| v.as_str());
            let store = match agent_id {
                Some(aid) => self.agent_registry.get_session_store(aid).await,
                None => None,
            };
            // Fall back: search all stores for the session (handles events
            // published before agent resolution, e.g. MessageReceived).
            let store = store.or_else(|| {
                let stores = pollster::block_on(self.agent_registry.all_session_stores());
                for s in &stores {
                    if s.has_session(sid) {
                        return Some(Arc::clone(s));
                    }
                }
                None
            });
            if let Some(store) = store {
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

/// Map an embedder name to its expected output dimension.
///
/// Covers the four models registered in yantrikdb's embedder-download registry.
/// Returns `None` for unrecognized names (caller should fall back gracefully).
fn embedder_dim(name: &str) -> Option<usize> {
    match name {
        "potion-base-2M" => Some(64),
        "potion-base-8M" => Some(256),
        "potion-base-32M" => Some(512),
        "potion-multilingual-128M" => Some(256),
        _ => None,
    }
}

/// Resolve a [`memory::EmbeddingConfig`] from the top-level Aman config.
///
/// Cloud-mode (provider + model set) takes priority over download-mode (embedder
/// name fallback). When neither provider nor model is set, falls back to the
/// named embedder with its known dimension.
fn resolve_embedding_config(aman: &config::AmanConfig) -> memory::EmbeddingConfig {
    if let Some(ref mem) = aman.memory {
        let emb = &mem.embedding;
        let has_cloud = emb.provider.is_some() && emb.model.is_some();

        if has_cloud {
            let provider_key = emb.provider.as_ref().unwrap();
            let model_id = emb.model.as_ref().unwrap();

            // Resolve provider config (base_url, api_key).
            if let Some(p) = aman.providers.get(provider_key) {
                let api_model = p
                    .models
                    .iter()
                    .find(|m| m.id == *model_id)
                    .map(|m| m.model_id.clone())
                    .unwrap_or_else(|| model_id.clone());

                let api_key =
                    get_llm_api_key_or_inline(provider_key, Some(p));

                // Try Ollama native /api/embed first, then OpenAI-compatible /v1/embeddings.
                if let Ok(dim) = memory::OllamaEmbedder::detect_dim(
                    &p.base_url,
                    &api_model,
                ) {
                    tracing::info!(
                        provider = %provider_key,
                        model = %api_model,
                        dim,
                        "Resolved Ollama embedding config (dim auto-detected)"
                    );
                    return memory::EmbeddingConfig::Ollama {
                        base_url: p.base_url.clone(),
                        model: api_model,
                        dim,
                    };
                }

                match memory::RemoteEmbedder::detect_dim(
                    &p.base_url,
                    &api_key,
                    &api_model,
                ) {
                    Ok(dim) => {
                        tracing::info!(
                            provider = %provider_key,
                            model = %api_model,
                            dim,
                            "Resolved remote embedding config (dim auto-detected)"
                        );
                        return memory::EmbeddingConfig::Remote {
                            base_url: p.base_url.clone(),
                            api_key,
                            model: api_model,
                            dim,
                        };
                    }
                    Err(e) => {
                        tracing::warn!(
                            provider = %provider_key,
                            model = %api_model,
                            error = %e,
                            "Failed to detect embedding dimension; \
                             falling back to download mode"
                        );
                    }
                }
            } else {
                tracing::warn!(
                    provider = %provider_key,
                    "Memory embedding provider not found in providers map; \
                     falling back to download mode"
                );
            }
        }

        // Download mode: use the named embedder.
        let dim = embedder_dim(&emb.embedder).unwrap_or(256);
        tracing::info!(
            embedder = %emb.embedder,
            dim,
            "Resolved download embedding config"
        );
        return memory::EmbeddingConfig::Download {
            name: emb.embedder.clone(),
            dim,
        };
    }

    // No memory section at all — use the default (multilingual download).
    tracing::info!("No memory config found; using default potion-multilingual-128M download");
    memory::EmbeddingConfig::default()
}

/// Create an LLM provider for a specific agent from its config entry.
///
/// Returns `None` when the agent's provider is not found in the config.
fn create_per_agent_llm_provider(
    config: &config::AmanConfig,
    agent_id: &str,
    agent: &config::AgentEntryConfig,
) -> Option<Arc<dyn LlmProvider>> {
    let p = config.providers.get(&agent.provider)?;
    let api_key = get_llm_api_key_or_inline(&agent.provider, Some(p));
    let api_type = config
        .llm
        .as_ref()
        .map(|l| l.api_type.as_str())
        .or(p.api_type.as_deref())
        .unwrap_or("openai");
    tracing::info!(
        agent = %agent_id,
        provider = %agent.provider,
        model = %agent.model,
        api_key_len = api_key.len(),
        api_type = %api_type,
        "creating per-agent LLM provider"
    );
    Some(build_provider(&agent.provider, &api_key, &p.base_url, api_type))
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
    if let Some(config) = provider_config
        && let Some(ref inline) = config.api_key
            && !inline.is_empty() {
                return inline.clone();
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
