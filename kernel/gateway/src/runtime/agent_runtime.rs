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
use messaging_core;
use kernel::session_history::InMemorySessionHistory;
use kernel::schema::JsonSchema;
use kernel::security::{ApprovalCache, ApprovedCapabilities, CapabilitySet};
use tool::auth::PluginApprovalRegistry;
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
    PluginSecurityManifest,
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

/// Snapshot of a single pending plugin capability approval request.
/// Returned by `GET /plugin-auth/pending` for rendering in the CLI, TUI,
/// or desktop UI.
#[derive(Debug, Clone, Serialize)]
pub struct PendingApprovalInfo {
    pub plugin_name: String,
    pub version: String,
    pub capabilities_summary: Vec<String>,
    pub capabilities: CapabilitySet,
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
    runtime_handle: Option<tokio::runtime::Handle>,
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
            runtime_handle: None,
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
    pub fn with_runtime_handle(mut self, handle: tokio::runtime::Handle) -> Self {
        self.runtime_handle = Some(handle);
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
        let tool_timeout_ms = config.runtime.tool_timeout_sec * 1000;

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
        // Sync built-in self module from repo to ~/.aman/self/ (preserves user modifications)
        if let Err(e) = super::self_sync::sync_builtin_self() {
            tracing::error!(error = %e, "failed to sync built-in self module");
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
        let llm_skills_arc = Arc::new(StdMutex::new(llm_skills.clone()));
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
        let llm_chat_tool = Arc::new(LlmChatTool {
            agent_registry: OnceLock::new(),
        });
        let _ = tools.register(Arc::clone(&llm_chat_tool) as Arc<dyn Tool>);
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

        let cron_manager = Arc::new(Mutex::new(
            CronManager::with_runtime_dir(self.runtime_dir.clone()),
        ));
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

        // ── Helper: load security manifest from a plugin's YAML file ──
        // The YAML is embedded at compile time via include_str!(). Only the
        // `security` section is extracted; the rest of the manifest is
        // constructed in code (with runtime version, etc.).
        fn load_security(yaml: &str) -> Option<PluginSecurityManifest> {
            #[derive(serde::Deserialize)]
            struct SecurityOnly {
                security: Option<PluginSecurityManifest>,
            }
            serde_yaml::from_str::<SecurityOnly>(yaml)
                .ok()
                .and_then(|s| s.security)
        }

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
                // Security manifest loaded from plugin.yaml (kernel/plugins/memory-store/)
                security: load_security(include_str!("../../../plugins/memory-store/plugin.yaml")),
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
                // Security manifest loaded from plugin.yaml (kernel/plugins/info-hub/)
                security: load_security(include_str!("../../../plugins/info-hub/plugin.yaml")),
            },
            plugin: Box::new(info_hub_plugin),
            isolation: PluginIsolationMode::InProcess,
            subprocess: None,
            wasm_module_bytes: None,
        };

        // ── Hook registry (created early so SkillEventDispatcher can drive it) ─
        let hook_registry = Arc::new(hook::HookRegistry::new());

        // Subscribe a handler that dispatches every event to matching skills.
        use kernel::context::SkillContext;
        use kernel::context::BaseContext;
        struct SkillEventDispatcher {
            executor: skill::SkillExecutor,
            bus: Arc<dyn event_bus::EventBus>,
            hooks: Arc<hook::HookRegistry>,
            hook_publisher: Arc<event_bus::BusEventPublisher>,
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

                // ── Fire SkillExecuting hooks ──────────────────────────
                // Build a HookContext that gives hooks access to the event bus
                // so they can push progress / status events during execution.
                let hook_ctx = kernel::context::HookContext {
                    base: BaseContext::new(trace_id),
                    hook_name: None,
                    event_bus: Some(Arc::clone(&self.hook_publisher) as Arc<dyn kernel::hook::EventPublisher>),
                };
                let _ = self.hooks
                    .execute(kernel::hook::HookPoint::SkillExecuting, hook_ctx.clone())
                    .await;

                let ctx = SkillContext {
                    base: BaseContext::new(trace_id),
                    skill_name: None,
                    soul_name: None,
                };
                let result = self.executor.execute_matching(event, ctx).await;

                // ── Fire SkillExecuted hooks ───────────────────────────
                let _ = self.hooks
                    .execute(kernel::hook::HookPoint::SkillExecuted, hook_ctx)
                    .await;

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
                hooks: Arc::clone(&hook_registry),
                hook_publisher: Arc::new(event_bus::BusEventPublisher::new(Arc::clone(&bus))),
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

        // ── Messaging channel state ────────────────────────────────
        let chat_session_store = Arc::new(messaging_core::ChatSessionStore::new());
        let channel_registry = Arc::new(messaging_core::ChannelRegistry::new());
        let sticky_router = Arc::new(messaging_core::StickyAgentRouter::new(
            aman_cfg
                .as_ref()
                .map(|c| c.agents.keys().cloned().collect::<Vec<_>>())
                .unwrap_or_default(),
        ));

        // ── Start IM channel sources from keychain config ──────────
        start_im_channel_sources(
            &sources,
            &channel_registry,
            &chat_session_store,
            &sticky_router,
        );

        // ── Subscribe notification subscriber ─────────────────────
        let notif_sub = notification::NotificationSubscriber::new(Arc::clone(&notifications));
        let _ = pollster::block_on(bus.subscribe(
            event_bus::SubscriptionFilter::default(),
            Box::new(notif_sub),
        ));

        // ── Agent registry ──────────────────────────────────────────
        let mut agent_registry_inner = super::AgentRegistry::new(Arc::clone(&bus));
        agent_registry_inner.set_skill_index(
            Arc::clone(&skill_search),
            Arc::clone(&skills),
        );
        let agent_registry = Arc::new(agent_registry_inner);
        // Wire agent_registry into ReadSkillTool for per-agent skill filtering.
        read_skill_tool.set_agent_registry(Arc::clone(&agent_registry));
        llm_chat_tool.set_agent_registry(Arc::clone(&agent_registry));

        // ── Plugin loading ──────────────────────────────────────────
        let mut all_candidates = vec![memory_store_candidate, info_hub_candidate];
        all_candidates.extend(self.extra_plugins);

        // Initialize the approval cache — generates ~/.aman/.security-key
        // on first startup and creates ~/.aman/approvals/ as needed.
        let aman_root = super::skill_sync::aman_data_dir();
        let approval_cache = match ApprovalCache::new(aman_root.clone()) {
            Ok(cache) => Some(cache),
            Err(e) => {
                tracing::error!(error = %e, "failed to initialize approval cache — plugin capability approvals will not be persisted");
                None
            }
        };

        // Clone the approval cache for runtime use (saving approvals after
        // user consent) before moving the original into PluginLoader.
        let approval_cache_runtime = approval_cache.clone();

        // Discover subprocess plugins from ~/.aman/plugins/
        let plugins_dir = aman_root.join("plugins");
        let discovered = plugin::discover_subprocess_plugins(&plugins_dir);
        if !discovered.is_empty() {
            tracing::info!(count = discovered.len(), dir = %plugins_dir.display(), "discovered subprocess plugins");
            all_candidates.extend(discovered);
        }

        // ── Pre-filter: check capability approvals ─────────────────
        // Plugins that are already approved are loaded immediately.
        // Plugins needing user approval are deferred and processed after
        // the HTTP/SSE layer is up. Built-in plugins declare minimal
        // capabilities in their security manifest; these are shown to the
        // user in the approval UI as the requested permission set.
        //
        // auto_approve_plugins defaults to false — all plugins (including
        // built-in) require explicit user approval.
        let auto_approve = aman_cfg
            .as_ref()
            .map(|c| c.runtime.security.auto_approve_plugins)
            .unwrap_or(false);
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        let mut approved_candidates = Vec::with_capacity(all_candidates.len());
        let mut pending_approvals: Vec<(PluginCandidate, CapabilitySet)> = Vec::new();

        for candidate in all_candidates {
            let plugin_name = candidate.manifest.name.clone();
            let needs_approval = match (&approval_cache_runtime, &candidate.manifest.security) {
                (Some(cache), Some(sec)) => {
                    match cache.check_approval(
                        &plugin_name,
                        &sec.requested_capabilities,
                        &candidate.manifest.version,
                    ) {
                        Ok(Some(needed_caps)) => {
                            if auto_approve {
                                // Auto-approve: persist and proceed
                                let mut caps = ApprovedCapabilities {
                                    plugin_version: candidate.manifest.version.to_string(),
                                    capabilities: sec.requested_capabilities.clone(),
                                    approved_at_ms: now_ms,
                                    approved_by: "auto".to_owned(),
                                    signature: String::new(),
                                };
                                if let Err(e) = cache.save(&plugin_name, &mut caps) {
                                    tracing::error!(
                                        plugin = %plugin_name,
                                        error = %e,
                                        "failed to auto-approve plugin capabilities — deferring"
                                    );
                                    Some(needed_caps)
                                } else {
                                    tracing::info!(
                                        plugin = %plugin_name,
                                        "auto-approved plugin capabilities (auto_approve_plugins=true)"
                                    );
                                    None
                                }
                            } else {
                                Some(needed_caps)
                            }
                        }
                        Ok(None) => None, // Already approved
                        Err(e) => {
                            tracing::error!(
                                plugin = %plugin_name,
                                error = %e,
                                "approval check failed — deferring plugin"
                            );
                            Some(sec.requested_capabilities.clone())
                        }
                    }
                }
                _ => None, // No security manifest or no cache — no approval needed
            };

            if let Some(caps) = needs_approval {
                let summary: Vec<String> = caps.summary();
                tracing::info!(
                    plugin = %plugin_name,
                    capabilities = ?summary,
                    "plugin requires capability approval — deferring"
                );
                pending_approvals.push((candidate, caps));
            } else {
                approved_candidates.push(candidate);
            }
        }

        if !pending_approvals.is_empty() {
            tracing::info!(
                count = pending_approvals.len(),
                "plugins deferred pending user capability approval"
            );
        }

        let memory_provider_registry = Arc::new(memory::MemoryProviderRegistry::new());
        let rpc_handler = Arc::new(RuntimeJsonRpcHandler::new(
            Arc::clone(&agent_registry),
            Arc::clone(&bus),
        ));
        rpc_handler.set_notifications(Arc::clone(&notifications));
        rpc_handler.set_cron_manager(Arc::clone(&cron_manager));
        let mut plugin_loader_builder = PluginLoader::new(Arc::new(RuntimePluginRegistrar::new(
            Arc::clone(&skills),
            Arc::clone(&tools),
            Arc::clone(&hook_registry),
            Arc::clone(&memory_provider_registry),
        ))).with_method_handler(rpc_handler);
        if let Some(cache) = approval_cache {
            plugin_loader_builder = plugin_loader_builder.with_approval_cache(cache);
        }
        let mut plugin_loader = plugin_loader_builder;
        if let Err(e) = pollster::block_on(plugin_loader.load_all(approved_candidates)) {
            tracing::error!(error = %e, "failed to load built-in plugins");
        }

        // ── Evaluation engine ─────────────────────────────────────────
        let _eval_engine = match &aman_cfg.as_ref().and_then(|c| c.eval.as_ref()) {
            Some(eval_cfg) if eval_cfg.enabled => {
                tracing::info!(
                    rules = eval_cfg.rules.len(),
                    auto_evaluate = eval_cfg.auto_evaluate,
                    "initializing eval engine"
                );
                let engine = eval::engine::EvalEngine::from_config(eval_cfg);
                let engine = Arc::new(tokio::sync::RwLock::new(engine));

                // Register built-in strategies
                {
                    let mut eng = pollster::block_on(engine.write());
                    eng.register_strategy(
                        "rule_based",
                        std::sync::Arc::new(eval::strategies::rule_based::RuleBasedStrategy),
                    );
                    eng.register_strategy(
                        "assertion",
                        std::sync::Arc::new(eval::strategies::assertion::AssertionStrategy),
                    );
                    eng.register_strategy(
                        "heuristic",
                        std::sync::Arc::new(eval::strategies::heuristic::HeuristicStrategy),
                    );
                    // LLM-as-judge: resolve the judge LLM provider and create executor
                    let judge_executor = eval_cfg.llm.as_ref().and_then(|judge_cfg| {
                        resolve_judge_executor(judge_cfg, aman_cfg.as_ref())
                    });
                    if let Some(executor) = judge_executor {
                        let strategy = eval::strategies::llm_judge::LlmJudgeStrategy::new(
                            Some(Box::new(executor)),
                            eval_cfg.llm.clone(),
                        );
                        eng.register_strategy("llm_as_judge", std::sync::Arc::new(strategy));
                    } else {
                        tracing::info!(
                            "llm_as_judge strategy registered without executor \
                             (no judge LLM configured — set eval.llm in config)"
                        );
                        eng.register_strategy(
                            "llm_as_judge",
                            std::sync::Arc::new(
                                eval::strategies::llm_judge::LlmJudgeStrategy::noop(),
                            ),
                        );
                    }
                }

                // Register eval tools directly with the tool registry
                for t in eval::tools::create_eval_tools(Arc::clone(&engine)) {
                    let _ = tools.register(t);
                }

                // Register eval hook
                if eval_cfg.auto_evaluate {
                    let eval_hook = eval::hook::EvalHook::new(Arc::clone(&engine));
                    let _ = hook_registry.register(std::sync::Arc::new(eval_hook));
                    tracing::info!("eval hook registered for automatic evaluation");
                }

                tracing::info!("eval engine initialized");
                Some(engine)
            }
            _ => {
                tracing::debug!("eval engine disabled (no config or enabled=false)");
                None
            }
        };


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

                // -- EmotionEvaluator (LLM-driven emotion updates) --
                // Gated on: global emotion.enabled AND valid emotions/ directory per agent.
                if let Some(ref emotion_cfg) = cfg.emotion {
                    if emotion_cfg.enabled {
                        if let Some(ref provider) = cfg.providers.get(&emotion_cfg.provider) {
                            let api_key =
                                get_llm_api_key_or_inline(&emotion_cfg.provider, Some(provider));
                            let eval_llm = super::emotion_evaluator::EmotionLlmConfig {
                                base_url: provider.base_url.clone(),
                                api_key: if api_key.is_empty() { None } else { Some(api_key) },
                                model: emotion_cfg.model.clone(),
                            };
                            // Read max_output_tokens from the existing models.<model> config.
                            let max_tokens = cfg
                                .models
                                .get(&emotion_cfg.model)
                                .map(|m| m.max_output_tokens as u64)
                                .unwrap_or(0);
                            let eval_cfg = super::emotion_evaluator::EmotionEvalConfig {
                                interval_secs: emotion_cfg.interval_secs,
                                temperature: emotion_cfg.temperature,
                                max_context_messages: emotion_cfg.max_context_messages,
                                max_tokens,
                            };
                            let ss = pollster::block_on(agent_registry.get_session_store(agent_id));
                            let ts = pollster::block_on(agent_registry.get_trace_store(agent_id));
                            pollster::block_on(agent_registry.init_emotion_evaluator(
                                agent_id,
                                ss,
                                ts,
                                eval_llm,
                                eval_cfg,
                            ));
                        } else {
                            tracing::warn!(
                                agent = %agent_id,
                                provider = %emotion_cfg.provider,
                                "emotion provider not found in config, skipping emotion evaluator"
                            );
                        }
                    }
                }
            }
        }


        // ── Agent harness (ReAct loop orchestrator) ──────────────────
        let compressor_config = context_manager::CompressorConfig {
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
            Box::new(self_bridge.prompt_pipeline()),
            Box::new(InMemorySessionHistory::new()),
            Box::new(context_manager::DefaultTokenBudgetPolicy::new()),
            Box::new(super::agent_harness::FirstEnabledAgentRouter),
            compressor_config,
            tool_timeout_ms,
            self.runtime_handle.clone().expect("runtime_handle must be set before build()"),
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
        let memory_llm_for_incubation = memory_llm_cfg.clone();
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
        if let Some(cfg) = memory_llm_for_incubation {
            incubation_runner.set_memory_llm(cfg);
        }

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
            self_bridge: super::self_bridge::SelfBridge,
            session_manager: Arc<super::session::SessionManager>,
            agent_registry: Arc<super::agent_registry::AgentRegistry>,
            llm_skills: Arc<StdMutex<Vec<skill::SkillInfo>>>,
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

                // Ensure a session record exists for this message.
                // Background/boredom sessions may not have been created yet
                // through the normal create_session path.
                let session_type = event.payload.get("session_type")
                    .and_then(|v| v.as_str())
                    .unwrap_or("background");
                if let Err(e) = self.session_manager
                    .ensure_session(&session_id, &agent_id, session_type)
                    .await
                {
                    tracing::warn!(
                        session_id = %session_id,
                        agent_id = %agent_id,
                        error = %e,
                        "MessageReceivedHandler: failed to ensure session"
                    );
                }

                // Persist the incoming message to the session JSONL eagerly.
                // This guards against any gap in the StoreAllEventsHandler path
                // (e.g. race between ensure_session and StoreAllEvents dispatch)
                // that could drop the first user message of an IM-chat session.
                if let Some(store) = self.agent_registry.get_session_store(&agent_id).await {
                    let entry = serde_json::json!({
                        "event_id": event.id.to_string(),
                        "event_type": format!("{:?}", event.event_type),
                        "source": event.source,
                        "timestamp_ms": event.timestamp.as_millis(),
                        "payload": event.payload,
                    });
                    if let Err(e) = store.append_session_event(&session_id, &entry) {
                        tracing::warn!(
                            session_id = %session_id,
                            error = %e,
                            "MessageReceivedHandler: failed to persist message to session JSONL"
                        );
                    }
                } else {
                    tracing::warn!(
                        agent_id = %agent_id,
                        session_id = %session_id,
                        "MessageReceivedHandler: no session store found for agent"
                    );
                }

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
                                let prompt = self.self_bridge
                                    .build_soul_prompt(&soul.raw)
                                    .unwrap_or_else(|| soul.raw.clone());
                                kernel::react::SoulSnapshot::new(soul.name.clone(), prompt)
                            })
                            .unwrap_or_else(|| kernel::react::SoulSnapshot::new("assistant", ""))
                    });

                // Extract background metadata for notification on completion
                let background = event.payload.get("background")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let skill_name: Option<String> = event.payload.get("skill_name")
                    .and_then(|v| v.as_str())
                    .map(String::from);

                // Resolve the execution mode for this skill (declared or auto-detected)
                let react_mode = skill_name.as_ref().and_then(|name| {
                    let skills = self.llm_skills.lock().ok()?;
                    skills.iter()
                        .find(|s| s.name == *name)
                        .map(|s| s.react_mode)
                });

                // Set agent status immediately (synchronously, before the
                // spawned task runs) so the idle detector doesn't race and
                // fire a boredom activity while this message is queued.
                // Skip for background boredom messages — the boredom actor
                // already set the system_state before publishing.
                if !background {
                    let is_work = super::session::work_session::parse_work_session_id(&session_id).is_some();
                    let _ = self.agent_registry.set_status(&agent_id, kernel::agent::AgentStatus::Busy).await;
                    self.agent_registry.set_system_state(
                        &agent_id,
                        if is_work { kernel::agent::AgentSystemState::Working }
                        else { kernel::agent::AgentSystemState::Chatting },
                    ).await;
                }

                // Spawn async ReAct processing — do not block the bus drain loop.
                self.agent_harness.spawn_process_message(
                    agent_id, session_id, text, model, soul_snapshot,
                    skill_name, react_mode, background,
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
                self_bridge: self_bridge.clone(),
                session_manager: Arc::clone(&session_manager),
                agent_registry: Arc::clone(&agent_registry),
                llm_skills: Arc::clone(&llm_skills_arc),
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
                    // Skip workflow state transition if no instance exists
                    // (e.g. legacy sessions created before ensure_session was added).
                    if self.session_manager.workflow_engine()
                        .get_instance(session_id).is_none()
                    {
                        tracing::debug!(
                            session_id,
                            agent_id,
                            "SessionReplyHandler: skipping handle_reply — no workflow instance"
                        );
                        return Ok(());
                    }
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

        // ── Subscribe agent:reply_ready → deliver replies via messaging channels ──
        struct ChatReplyHandler {
            chat_session_store: Arc<messaging_core::ChatSessionStore>,
            channel_registry: Arc<messaging_core::ChannelRegistry>,
        }
        #[async_trait::async_trait]
        impl event_bus::EventHandler for ChatReplyHandler {
            async fn handle(&self, event: kernel::event::Event) -> kernel::AmanResult<()> {
                let session_id = event
                    .payload
                    .get("session_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let reply = event
                    .payload
                    .get("reply")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");

                if session_id.is_empty() || reply.is_empty() {
                    return Ok(());
                }

                // Only act if this session was initiated from a chat platform.
                let Some(target) = self.chat_session_store.get(session_id) else {
                    return Ok(());
                };

                // Look up the sender for this platform source.
                let Some(sender) = self.channel_registry.get(&target.source_id) else {
                    tracing::debug!(
                        source_id = %target.source_id,
                        session_id = %session_id,
                        "ChatReplyHandler: no sender registered for source"
                    );
                    return Ok(());
                };

                // Show typing indicator so the user sees the bot is working.
                let _ = sender.send_typing(&target).await;

                // Deliver the reply.
                if let Err(e) = sender.send_text(&target, reply).await {
                    tracing::error!(
                        error = %e,
                        session_id = %session_id,
                        platform = ?target.platform,
                        "ChatReplyHandler: failed to send chat reply"
                    );
                }

                Ok(())
            }
        }
        let _ = pollster::block_on(bus.subscribe(
            event_bus::SubscriptionFilter {
                event_types: Some(vec![EventType::Custom("agent:reply_ready".to_owned())]),
                sources: None,
                priorities: None,
                payload_match: None,
            },
            Box::new(ChatReplyHandler {
                chat_session_store: Arc::clone(&chat_session_store),
                channel_registry: Arc::clone(&channel_registry),
            }),
        ));

        // ── Subscribe streaming reply events → progressive message editing ──
        //
        // When the LLM streams its response token-by-token, this handler
        // sends a placeholder message on start, progressively edits it with
        // each chunk, and applies MarkdownV2 formatting on the final edit.
        //
        // Design rules (Telegram-specific, but trait defaults keep other
        // platforms working):
        //  • Plain text only during streaming — unclosed markup tokens
        //    (e.g. `**bold`, ```fence) would cause HTTP 400 parse errors.
        //  • MarkdownV2 only on the final update, when the text is complete.
        //  • send_typing is called once on stream start for the typing
        //    indicator (lasts ~5 s on Telegram).
        #[allow(dead_code)]
        struct StreamingChatReplyHandler {
            chat_session_store: Arc<messaging_core::ChatSessionStore>,
            channel_registry: Arc<messaging_core::ChannelRegistry>,
            streamed_sessions: Arc<StdMutex<std::collections::HashSet<String>>>,
            // Active streaming sessions: session_id → (handle, target, accumulated_text)
            // Uses tokio::sync::Mutex because we must not hold a std Mutex
            // across .await points (update_stream does an HTTP call).
            active_streams: Mutex<
                std::collections::HashMap<
                    String,
                    (messaging_core::sender::StreamHandle, messaging_core::ChatTarget, String),
                >,
            >,
        }

        #[async_trait::async_trait]
        impl event_bus::EventHandler for StreamingChatReplyHandler {
            async fn handle(&self, event: kernel::event::Event) -> kernel::AmanResult<()> {
                let session_id = event
                    .payload
                    .get("session_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_owned();

                if session_id.is_empty() {
                    return Ok(());
                }

                let etype = match &event.event_type {
                    EventType::Custom(s) => s.as_str(),
                    _ => return Ok(()),
                };

                match etype {
                    "agent:reply_stream_start" => {
                        // Only act if this session originated from a chat platform.
                        let Some(target) = self.chat_session_store.get(&session_id) else {
                            return Ok(());
                        };
                        let Some(sender) = self.channel_registry.get(&target.source_id) else {
                            return Ok(());
                        };

                        // Show typing indicator while the LLM warms up.
                        if let Err(e) = sender.send_typing(&target).await {
                            tracing::warn!(
                                error = %e,
                                session_id = %session_id,
                                "StreamingChatReply: send_typing failed"
                            );
                        }

                        // Send placeholder message, return handle.
                        match sender.begin_stream(&target).await {
                            Ok(handle) => {
                                tracing::debug!(
                                    session_id = %session_id,
                                    handle = %handle,
                                    "StreamingChatReply: stream started"
                                );
                                let mut streams = self.active_streams.lock().await;
                                streams.insert(
                                    session_id.clone(),
                                    (handle, target, String::new()),
                                );
                            }
                            Err(e) => {
                                tracing::error!(
                                    error = %e,
                                    session_id = %session_id,
                                    "StreamingChatReply: begin_stream failed"
                                );
                            }
                        }
                    }

                    "agent:reply_chunk" => {
                        let delta = event
                            .payload
                            .get("extra")
                            .and_then(|v| v.get("delta"))
                            .and_then(|v| v.as_str())
                            .unwrap_or("");

                        if delta.is_empty() {
                            return Ok(());
                        }

                        // Extract state, update accumulated, release lock
                        // before the HTTP call.
                        let (handle, target, accumulated, should_update) = {
                            let mut streams = self.active_streams.lock().await;
                            if let Some((handle, target, accumulated)) =
                                streams.get_mut(&session_id)
                            {
                                accumulated.push_str(delta);
                                // Throttle edits: only update every N chars to
                                // reduce flicker and API calls. Always update
                                // early (first 32 chars) for quick feedback.
                                let char_count = accumulated.chars().count();
                                let should_update =
                                    char_count <= 32 || char_count % 16 == 0;
                                (
                                    handle.clone(),
                                    target.clone(),
                                    accumulated.clone(),
                                    should_update,
                                )
                            } else {
                                return Ok(());
                            }
                        };

                        if should_update {
                            let Some(sender) =
                                self.channel_registry.get(&target.source_id)
                            else {
                                return Ok(());
                            };
                            // Plain-text only — unclosed markup would 400.
                            if let Err(e) = sender
                                .update_stream(&target, &handle, &accumulated, false)
                                .await
                            {
                                tracing::warn!(
                                    error = %e,
                                    session_id = %session_id,
                                    "StreamingChatReply: update_stream (chunk) failed"
                                );
                            }
                        }
                    }

                    "agent:reply_stream_done" => {
                        // Remove from active streams, release lock, then
                        // do the final edit outside the lock.
                        let entry = {
                            let mut streams = self.active_streams.lock().await;
                            streams.remove(&session_id)
                        };

                        if let Some((handle, target, accumulated)) = entry {
                            let sender = match self.channel_registry.get(&target.source_id) {
                                Some(s) => s,
                                None => return Ok(()),
                            };

                            if accumulated.is_empty() {
                                // LLM returned no text — clean up the
                                // placeholder (don't mark as streamed;
                                // ChatReplyHandler may still have content).
                                let _ = sender.cancel_stream(&target, &handle).await;
                                tracing::debug!(
                                    session_id = %session_id,
                                    "StreamingChatReply: empty reply — placeholder deleted"
                                );
                            } else {
                                // Final edit — safe to apply MarkdownV2 now
                                // that the text is complete.
                                if let Err(e) = sender
                                    .update_stream(&target, &handle, &accumulated, true)
                                    .await
                                {
                                    tracing::error!(
                                        error = %e,
                                        session_id = %session_id,
                                        "StreamingChatReply: final update_stream failed"
                                    );
                                    // Fall back to plain text so the user
                                    // at least sees something.
                                    let _ = sender.send_text(&target, &accumulated).await;
                                }

                                // Mark as streamed so ChatReplyHandler
                                // skips the duplicate send_text.
                                {
                                    let mut streamed = self
                                        .streamed_sessions
                                        .lock()
                                        .unwrap_or_else(|e| e.into_inner());
                                    streamed.insert(session_id.clone());
                                }

                                tracing::debug!(
                                    session_id = %session_id,
                                    chars = accumulated.chars().count(),
                                    "StreamingChatReply: stream completed"
                                );
                            }
                        }
                    }

                    "agent:reply_stream_error" => {
                        let entry = {
                            let mut streams = self.active_streams.lock().await;
                            streams.remove(&session_id)
                        };

                        if let Some((handle, target, _accumulated)) = entry {
                            let sender = match self.channel_registry.get(&target.source_id) {
                                Some(s) => s,
                                None => return Ok(()),
                            };
                            // Delete the placeholder so it doesn't linger.
                            let _ = sender.cancel_stream(&target, &handle).await;

                            tracing::warn!(
                                session_id = %session_id,
                                "StreamingChatReply: stream errored — placeholder deleted"
                            );
                        }
                    }

                    _ => {}
                }

                Ok(())
            }
        }
        // ── DISABLED: streaming reply handler — progressive message editing
        // caused flicker on Telegram (message appears, gets edited, appears
        // as deleted/re-sent). Code kept for future re-enablement.
        // The typing indicator is now sent from ChatReplyHandler instead.
        // let _ = pollster::block_on(bus.subscribe(
        //     event_bus::SubscriptionFilter {
        //         event_types: Some(vec![
        //             EventType::Custom("agent:reply_stream_start".to_owned()),
        //             EventType::Custom("agent:reply_chunk".to_owned()),
        //             EventType::Custom("agent:reply_stream_done".to_owned()),
        //             EventType::Custom("agent:reply_stream_error".to_owned()),
        //         ]),
        //         sources: None,
        //         priorities: None,
        //         payload_match: None,
        //     },
        //     Box::new(StreamingChatReplyHandler {
        //         chat_session_store: Arc::clone(&chat_session_store),
        //         channel_registry: Arc::clone(&channel_registry),
        //         streamed_sessions: Arc::clone(&streamed_sessions),
        //         active_streams: Mutex::new(std::collections::HashMap::new()),
        //     }),
        // ));

        // ── Subscribe work item event forwarder for dual-write ──
        // Forwards selected agent events from work item sessions to the
        // global bus as "work:item:event" so the Python team plugin can
        // write them to ~/.aman/team/projects/{project}/works/{work}.jsonl.
        //
        // Only forwards events that are meaningful for work context:
        // tool results, agent replies, and errors.  Skips streaming chunks,
        // internal bus events, and its own forwarded events (loop prevention).
        struct WorkItemEventHandler {
            bus: Arc<dyn EventBus>,
        }
        #[async_trait::async_trait]
        impl event_bus::EventHandler for WorkItemEventHandler {
            async fn handle(&self, event: kernel::event::Event) -> kernel::AmanResult<()> {
                // Prevent infinite loop — don't re-wrap our own events.
                if event.source.as_str() == "gateway:work_item" {
                    return Ok(());
                }

                let session_id = event
                    .payload
                    .get("session_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                if let Some((agent_id, project_key, work_id)) =
                    super::session::work_session::parse_work_session_id(session_id)
                {
                    // Only forward events that are useful for work context.
                    // Skip streaming chunks (per-token), internal bus signals,
                    // and events that don't carry meaningful work data.
                    let etype = format!("{:?}", event.event_type);
                    let should_forward = etype.contains("tool:completed")
                        || etype.contains("tool:failed")
                        || etype.contains("agent:reply_ready")
                        || etype.contains("agent:reply_stream_error")
                        || etype.contains("MessageReceived");
                    if !should_forward {
                        return Ok(());
                    }

                    let _ = self
                        .bus
                        .publish(Event::new(
                            "gateway:work_item",
                            EventType::Custom("work:item:event".to_owned()),
                            json!({
                                "session_id": session_id,
                                "agent_id": agent_id,
                                "project_key": project_key,
                                "work_id": work_id,
                                "source_event": event.source,
                                "source_event_type": etype,
                                "payload": event.payload,
                            }),
                        ))
                        .await;
                }
                Ok(())
            }
        }
        let _ = pollster::block_on(bus.subscribe(
            event_bus::SubscriptionFilter::default(),
            Box::new(WorkItemEventHandler {
                bus: Arc::clone(&bus),
            }),
        ));

        // ── Subscribe agent:message handler for agent-to-agent routing (M7) ──
        struct AgentMessageHandler {
            agent_harness: Arc<super::agent_harness::AgentHarness>,
            soul_runtime: Option<SoulRuntime>,
            self_bridge: super::self_bridge::SelfBridge,
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

                // Build SoulSnapshot from current soul (via Python self bridge).
                let soul_snapshot = self.soul_runtime.as_ref()
                    .map(|sr| {
                        let soul = sr.current_soul();
                        let prompt = self.self_bridge
                            .build_soul_prompt(&soul.raw)
                            .unwrap_or_else(|| soul.raw.clone());
                        kernel::react::SoulSnapshot::new(soul.name.clone(), prompt)
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
                    None,  // skill_name
                    None,  // react_mode
                    false, // background
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
                self_bridge: self_bridge.clone(),
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

        let plugin_approval_registry = Arc::new(PluginApprovalRegistry::new());
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
            plugin_approval_registry,
            approval_cache: approval_cache_runtime,
            pending_plugin_approvals: Mutex::new(pending_approvals),
            skill_search,
            skill_versions,
            skill_hot_reload,
            skill_stop: Arc::new(AtomicBool::new(false)),
            skill_thread: Mutex::new(None),
            workflow_engine,
            plugin_loader: Mutex::new(plugin_loader),
            cron_manager: Arc::clone(&cron_manager),
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
            llm_skills: llm_skills_arc,
            skill_registry,
            cascade_selector,
            notifications,
            chat_session_store,
            channel_registry,
            sticky_router,
            agent_registry,
            agent_harness,
            session_manager,
            shutdown_notify: tokio::sync::Notify::new(),
            self_bridge,
            sse_broadcast: sse_state,
            hook_registry,
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

/// Tool that lets plugins (and agents) call LLM chat completion through
/// a specific agent's LLM provider.  Registered at startup; the
/// `agent_registry` is wired after creation via [`LlmChatTool::set_agent_registry`].
struct LlmChatTool {
    agent_registry: OnceLock<Arc<super::AgentRegistry>>,
}

impl LlmChatTool {
    fn set_agent_registry(&self, registry: Arc<super::AgentRegistry>) {
        let _ = self.agent_registry.set(registry);
    }
}

#[async_trait::async_trait]
impl Tool for LlmChatTool {
    fn name(&self) -> &str {
        "llm_chat"
    }

    fn mode(&self) -> ToolMode {
        ToolMode::Local
    }

    fn description(&self) -> &str {
        "Call an agent's LLM provider for chat completion. Use this to run structured analysis, generate text, or evaluate prompts without going through the full agent ReAct loop. Requires agent_id to select which agent's LLM config to use."
    }

    fn parameters(&self) -> &JsonSchema {
        static PARAMS: LazyLock<JsonSchema> = LazyLock::new(|| {
            JsonSchema::from(serde_json::json!({
                "type": "object",
                "required": ["agent_id", "user_prompt"],
                "properties": {
                    "agent_id": {
                        "type": "string",
                        "description": "Which agent's LLM provider to use"
                    },
                    "system_prompt": {
                        "type": "string",
                        "description": "System instructions for the model"
                    },
                    "user_prompt": {
                        "type": "string",
                        "description": "The user message / task description"
                    },
                    "temperature": {
                        "type": "number",
                        "description": "Sampling temperature (0.0–2.0, default 0.3)"
                    },
                    "max_tokens": {
                        "type": "integer",
                        "description": "Maximum tokens in the response (default 4000)"
                    },
                    "response_format": {
                        "type": "string",
                        "description": "When set to 'json_object', the model is constrained to output valid JSON"
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
                    "content": { "type": "string" },
                    "finish_reason": { "type": "string" }
                }
            }))
        });
        &RETURNS
    }

    async fn execute(
        &self,
        params: serde_json::Value,
        _ctx: ToolContext,
    ) -> kernel::AmanResult<serde_json::Value> {
        let agent_id = params
            .get("agent_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::ConfigInvalid {
                message: "agent_id is required".to_owned(),
            })?;
        let system_prompt = params
            .get("system_prompt")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let user_prompt = params
            .get("user_prompt")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::ConfigInvalid {
                message: "user_prompt is required".to_owned(),
            })?;
        let requested_tokens = params
            .get("max_tokens")
            .and_then(|v| v.as_u64())
            .unwrap_or(4000) as u32;
        let response_format = params
            .get("response_format")
            .and_then(|v| v.as_str())
            .map(|s| s.to_owned());

        let reg = self.agent_registry.get().ok_or_else(|| Error::ConfigInvalid {
            message: "agent_registry not wired".to_owned(),
        })?;

        let provider = reg
            .get_llm_provider(agent_id)
            .await
            .ok_or_else(|| Error::NotFound {
                name: format!("LLM provider for agent '{agent_id}'"),
            })?;

        // Resolve the agent's configured model and token budget.
        // Cap requested max_tokens to the agent's configured limit so
        // plugins can't bypass the budget set in config.yaml.
        let (model, configured_max_out) = reg
            .get(agent_id)
            .await
            .map(|instance| {
                let m = instance.descriptor.model.clone();
                let out = instance.descriptor.max_output_tokens.unwrap_or(0) as u64;
                (m, out)
            })
            .unwrap_or_default();
        let max_tokens = if configured_max_out > 0 && requested_tokens > configured_max_out as u32 {
            configured_max_out as u32
        } else {
            requested_tokens
        };

        let req = kernel::llm::LlmChatRequest {
            model,
            system_prompt: system_prompt.to_owned(),
            messages: vec![kernel::react::ChatMessage::user(user_prompt.to_owned())],
            tools: vec![],
            max_output_tokens: max_tokens,
            response_format,
        };

        let resp = provider.chat_completion(req, None).await.map_err(|e| {
            Error::Unrecoverable {
                message: format!("llm_chat failed: {e}"),
            }
        })?;

        Ok(serde_json::json!({
            "content": resp.content,
            "finish_reason": resp.finish_reason,
        }))
    }
}

/// JSON-RPC method handler for subprocess plugins.
/// Gives plugins access to AgentRegistry and EventBus.
struct RuntimeJsonRpcHandler {
    agent_registry: Arc<super::AgentRegistry>,
    bus: Arc<dyn EventBus>,
    notifications: OnceLock<Arc<notification::NotificationStore>>,
    cron_manager: OnceLock<Arc<Mutex<CronManager>>>,
}

impl RuntimeJsonRpcHandler {
    fn new(agent_registry: Arc<super::AgentRegistry>, bus: Arc<dyn EventBus>) -> Self {
        Self {
            agent_registry,
            bus,
            notifications: OnceLock::new(),
            cron_manager: OnceLock::new(),
        }
    }

    fn set_notifications(&self, store: Arc<notification::NotificationStore>) {
        let _ = self.notifications.set(store);
    }

    fn set_cron_manager(&self, cm: Arc<Mutex<CronManager>>) {
        let _ = self.cron_manager.set(cm);
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
                            "system_state": a.system_state,
                            "activity": a.activity,
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
                    })?
                    .to_owned();
                let title = params
                    .get("title")
                    .and_then(|v| v.as_str())
                    .unwrap_or("untitled")
                    .to_owned();
                let description = params
                    .get("description")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_owned();
                let priority_str = params
                    .get("priority")
                    .and_then(|v| v.as_str())
                    .unwrap_or("normal");
                let priority = match priority_str {
                    "critical" => work::Priority::Critical,
                    "high" => work::Priority::High,
                    "low" => work::Priority::Low,
                    _ => work::Priority::Normal,
                };
                let item = work::WorkItem {
                    id: work::WorkItemId::new(),
                    title: title.clone(),
                    description: description.clone(),
                    steps: None,
                    priority,
                    timeout: None,
                    context: std::collections::HashMap::new(),
                    notify_on_complete: true,
                    created_at: kernel::types::Timestamp::now(),
                };

                let context = params.get("context").and_then(|v| v.as_object());
                let source_type = context
                    .and_then(|c| c.get("source"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("kanban");

                let ws = self.agent_registry.get_work_system(&agent_id).await.ok_or_else(|| {
                    kernel::Error::NotFound {
                        name: format!("agent:{agent_id}"),
                    }
                })?;

                // ── Determine WorkItemSource and session info ──────────
                if source_type == "startup" {
                    let idea_slug = context
                        .and_then(|c| c.get("idea_slug"))
                        .and_then(|v| v.as_str())
                        .filter(|s| !s.is_empty());
                    let skill = context
                        .and_then(|c| c.get("skill"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("validate");

                    ws.push_work_item(item, work::WorkItemSource::Startup {
                        idea_slug: idea_slug.unwrap_or("unknown").to_owned(),
                        skill: skill.to_owned(),
                    }).await.map_err(|e| kernel::Error::Unrecoverable {
                        message: format!("push_work_item failed: {e:?}"),
                    })?;

                    if let Some(slug) = idea_slug {
                        let session_id = super::session::work_session::startup_session_id(
                            &agent_id, slug,
                        );

                        let mut text = format!(
                            "[WORK ITEM — Startup]\n\
                             Idea: {slug}  Skill: {skill}\n\
                             Title: {title}\n\
                             Description: {description}\n"
                        );

                        let work_context = context
                            .and_then(|c| c.get("work_context"))
                            .and_then(|v| v.as_str())
                            .unwrap_or("");
                        if !work_context.is_empty() {
                            text.push('\n');
                            text.push_str(work_context);
                        }

                        let event = kernel::event::Event::new(
                            "gateway:push_work_item",
                            kernel::event::EventType::MessageReceived,
                            serde_json::json!({
                                "session_id": session_id,
                                "agent_id": agent_id,
                                "text": text,
                                "idea_slug": slug,
                                "skill_name": "startup-worker",
                                "session_type": "background",
                                "background": false,
                            }),
                        );
                        if let Err(e) = self.bus.publish(event).await {
                            tracing::warn!(
                                agent_id = %agent_id,
                                idea_slug = slug,
                                error = %e,
                                "push_work_item: failed to publish startup MessageReceived event",
                            );
                        }
                    }
                } else {
                    // ── Kanban / default path ──────────────────────────
                    ws.push_work_item(item, work::WorkItemSource::Kanban {
                        board_id: plugin_name.to_owned(),
                        scheduler: "subprocess-plugin".to_owned(),
                    }).await.map_err(|e| kernel::Error::Unrecoverable {
                        message: format!("push_work_item failed: {e:?}"),
                    })?;

                    let project_key = context
                        .and_then(|c| c.get("project_key"))
                        .and_then(|v| v.as_str())
                        .filter(|s| !s.is_empty());
                    let work_id = context
                        .and_then(|c| c.get("work_id"))
                        .and_then(|v| v.as_str())
                        .filter(|s| !s.is_empty());

                    if let (Some(pk), Some(wid)) = (project_key, work_id) {
                        let session_id = super::session::work_session::work_session_id(
                            &agent_id, pk, wid,
                        );

                        let stage_id = context
                            .and_then(|c| c.get("stage_id"))
                            .and_then(|v| v.as_str())
                            .unwrap_or("");
                        let work_context = context
                            .and_then(|c| c.get("work_context"))
                            .and_then(|v| v.as_str())
                            .unwrap_or("");

                        // Publish a MessageReceived event with the kanban-worker skill.
                        // The skill (in ~/.aman/skills/) defines the workflow; the backend
                        // only passes data.
                        //
                        // NOTE: use push_str to build the text — work_context may contain
                        // JSON tool-call payloads with { } that would panic format!().
                        let mut text = format!(
                            "[WORK ITEM — Kanban Act!]\n\
                             Project: {pk}  Work ID: {wid}  Stage: {stage_id}\n\
                             Title: {title}\n\
                             Description: {description}\n"
                        );
                        if !work_context.is_empty() {
                            text.push('\n');
                            text.push_str(work_context);
                        }

                        let event = kernel::event::Event::new(
                            "gateway:push_work_item",
                            kernel::event::EventType::MessageReceived,
                            serde_json::json!({
                                "session_id": session_id,
                                "agent_id": agent_id,
                                "text": text,
                                "project_key": pk,
                                "work_id": wid,
                                "stage_id": stage_id,
                                "session_type": "background",
                                "background": false,
                                "skill_name": "kanban-worker",
                            }),
                        );
                        if let Err(e) = self.bus.publish(event).await {
                            tracing::warn!(
                                agent_id = %agent_id,
                                project_key = pk,
                                work_id = wid,
                                error = %e,
                                "push_work_item: failed to publish MessageReceived event",
                            );
                        }
                    }
                }

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
                Ok(serde_json::json!({"ok": true, "workflow": name}))
            }
            "aman.send_notification" => {
                let store = self.notifications.get().ok_or_else(|| {
                    kernel::Error::Unrecoverable {
                        message: "notification store not initialized".to_owned(),
                    }
                })?;
                let title = params
                    .get("title")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let message = params
                    .get("message")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let severity = params
                    .get("severity")
                    .and_then(|v| v.as_str())
                    .unwrap_or("info");
                let category = params
                    .get("category")
                    .and_then(|v| v.as_str())
                    .unwrap_or("plugin");
                let action_label = params
                    .get("action_label")
                    .and_then(|v| v.as_str());
                let action_route = params
                    .get("action_route")
                    .and_then(|v| v.as_str());
                let n = notification::Notification {
                    id: uuid::Uuid::now_v7().to_string(),
                    severity: match severity {
                        "critical" => notification::Severity::Critical,
                        "warning" => notification::Severity::Warning,
                        _ => notification::Severity::Info,
                    },
                    category: match category {
                        "plugin" => notification::Category::Plugin,
                        "idle" => notification::Category::Idle,
                        "security" => notification::Category::Security,
                        "workflow" => notification::Category::Workflow,
                        "llm" => notification::Category::Llm,
                        "skill" => notification::Category::Skill,
                        _ => notification::Category::Plugin,
                    },
                    title: title.to_owned(),
                    message: message.to_owned(),
                    dismissed: false,
                    dismissible: true,
                    created_at: std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_millis() as i64,
                    event_id: None,
                    source: Some(format!("plugin:{plugin_name}")),
                    action_label: action_label.map(|s| s.to_owned()),
                    action_route: action_route.map(|s| s.to_owned()),
                };
                store.push(n);
                Ok(serde_json::json!({"ok": true}))
            }
            "aman.add_cron_job" => {
                let id = params
                    .get("id")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| kernel::Error::ConfigInvalid {
                        message: "id is required".to_owned(),
                    })?;
                let expression = params
                    .get("expression")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| kernel::Error::ConfigInvalid {
                        message: "cron expression is required".to_owned(),
                    })?;
                // Validation is handled by CronSource::new below
                let caller = format!("plugin:{plugin_name}");
                // Basic validation: non-empty and has at least spaces for fields
                if expression.trim().is_empty() {
                    return Err(kernel::Error::ConfigInvalid {
                        message: "cron expression is empty".to_owned(),
                    });
                }
                let cm = self
                    .cron_manager
                    .get()
                    .ok_or_else(|| kernel::Error::Unrecoverable {
                        message: "cron manager not initialized".to_owned(),
                    })?;
                let mut guard = cm.lock().await;
                let ctx = kernel::context::SourceContext {
                    base: kernel::context::BaseContext {
                        trace_id: kernel::types::TraceId::new(),
                        timeout_ms: None,
                        labels: Default::default(),
                        extensions: Default::default(),
                        event_bus: None,
                    },
                    source_name: Some(id.to_owned()),
                };
                let cron_source = source::CronSource::new(id.to_owned(), expression)
                    .map_err(|e| kernel::Error::Unrecoverable {
                        message: format!("invalid cron source: {e}"),
                    })?;
                guard
                    .add_with_caller(cron_source, ctx, &caller)
                    .await
                    .map_err(|e| kernel::Error::Unrecoverable {
                        message: format!("add_cron_job failed: {e}"),
                    })?;
                tracing::info!(
                    plugin = %plugin_name,
                    cron_id = %id,
                    expression = %expression,
                    "plugin registered cron job"
                );
                Ok(serde_json::json!({"ok": true, "id": id}))
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
    cron_manager: Arc<Mutex<CronManager>>,
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
    llm_skills: Arc<StdMutex<Vec<skill::SkillInfo>>>,
    /// skm-core registry for cascade selection (None if init failed).
    skill_registry: Option<skm_core::SkillRegistry>,
    /// Cascade selector for skill matching (None if init failed).
    cascade_selector: Option<skm_select::CascadeSelector>,
    /// Registry for tool authorization requests (native macOS dialogs).
    auth_registry: Arc<tool::auth::AuthRegistry>,
    /// Registry for in-flight plugin capability approval requests.
    plugin_approval_registry: Arc<PluginApprovalRegistry>,
    /// Clone of the approval cache for runtime use (saving approved capabilities).
    approval_cache: Option<ApprovalCache>,
    /// Plugin candidates deferred pending user capability approval.
    pending_plugin_approvals: Mutex<Vec<(PluginCandidate, CapabilitySet)>>,
    /// Notification center — user-facing alerts (critical/warning).
    notifications: Arc<notification::NotificationStore>,
    /// Chat session store — maps session IDs to chat targets for reply routing.
    chat_session_store: Arc<messaging_core::ChatSessionStore>,
    /// Channel registry — maps source IDs to MessageSender instances.
    channel_registry: Arc<messaging_core::ChannelRegistry>,
    /// Sticky agent router — @mention-based agent affinity for chat platforms.
    sticky_router: Arc<messaging_core::StickyAgentRouter>,
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
    /// Hook registry — in-process hooks registered by plugins. Driven by the
    /// SkillEventDispatcher so hooks fire at SkillExecuting / SkillExecuted.
    hook_registry: Arc<hook::HookRegistry>,
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

    /// Return the plugin capability approval registry.
    #[must_use]
    pub fn plugin_approval_registry(&self) -> Arc<PluginApprovalRegistry> {
        Arc::clone(&self.plugin_approval_registry)
    }

    /// Return a reference to the approval cache (for saving approved capabilities).
    #[must_use]
    pub fn approval_cache(&self) -> Option<&ApprovalCache> {
        self.approval_cache.as_ref()
    }

    /// Take a pending plugin candidate by name (used after user approval).
    pub async fn take_pending_plugin_candidate(
        &self,
        plugin_name: &str,
    ) -> Option<(PluginCandidate, CapabilitySet)> {
        let mut guard = self.pending_plugin_approvals.lock().await;
        let idx = guard
            .iter()
            .position(|(c, _)| c.manifest.name == plugin_name);
        idx.map(|i| guard.remove(i))
    }

    /// Remove a pending plugin candidate by name (used after user denial).
    pub async fn remove_pending_plugin_candidate(&self, plugin_name: &str) -> bool {
        let mut guard = self.pending_plugin_approvals.lock().await;
        let before = guard.len();
        guard.retain(|(c, _)| c.manifest.name != plugin_name);
        guard.len() != before
    }

    /// Number of pending plugin approval requests.
    #[must_use]
    pub async fn pending_plugin_approvals_count(&self) -> usize {
        self.pending_plugin_approvals.lock().await.len()
    }

    /// Return a snapshot of pending plugin approvals for listing via the API.
    /// Each entry contains the plugin name, version, and requested capabilities
    /// summary. The caller (HTTP endpoint or TUI) can render these for user
    /// review before approving or denying.
    pub async fn pending_plugin_approvals_list(&self) -> Vec<PendingApprovalInfo> {
        self.pending_plugin_approvals
            .lock()
            .await
            .iter()
            .map(|(candidate, caps)| PendingApprovalInfo {
                plugin_name: candidate.manifest.name.clone(),
                version: candidate.manifest.version.to_string(),
                capabilities_summary: caps.summary(),
                capabilities: caps.clone(),
            })
            .collect()
    }

    /// Emit `plugin_auth_required` events for all deferred plugin approvals.
    ///
    /// Called after the HTTP server and SSE broadcast are running so the
    /// desktop client can receive the events and show native dialogs.
    pub async fn emit_pending_plugin_approvals(&self) {
        // Extract summary data while holding the lock, then release.
        // We must NOT take() the vec — the TUI and HTTP API need to read
        // the same list after events have been emitted.
        let pending_info: Vec<(String, String, Vec<String>, CapabilitySet)> = {
            let guard = self.pending_plugin_approvals.lock().await;
            guard
                .iter()
                .map(|(candidate, needed_caps)| {
                    (
                        candidate.manifest.name.clone(),
                        candidate.manifest.version.to_string(),
                        needed_caps.summary(),
                        needed_caps.clone(),
                    )
                })
                .collect()
        };

        if pending_info.is_empty() {
            return;
        }

        tracing::info!(
            count = pending_info.len(),
            "emitting plugin_auth_required events for deferred plugin approvals"
        );

        for (plugin_name, version, summary, needed_caps) in &pending_info {
            // Register a pending approval so the HTTP endpoint can resolve it
            let _rx = self.plugin_approval_registry.register(plugin_name.clone());

            // Publish event to the EventBus — flows through SseBusHandler → SSE → desktop
            let event = Event::new(
                "aman:plugin-auth",
                EventType::Custom("plugin_auth_required".to_owned()),
                serde_json::json!({
                    "plugin_name": plugin_name,
                    "version": version,
                    "capabilities_summary": summary,
                    "capabilities": needed_caps,
                }),
            );
            if let Err(e) = self.bus.publish(event).await {
                tracing::error!(
                    plugin = %plugin_name,
                    error = %e,
                    "failed to publish plugin_auth_required event"
                );
            } else {
                tracing::info!(
                    plugin = %plugin_name,
                    "published plugin_auth_required event"
                );
            }
        }
    }

    /// Synchronous convenience wrapper for resolving a plugin approval from
    /// the TUI or other non-async contexts. Persists the decision via the
    /// approval cache and loads/removes the plugin accordingly.
    ///
    /// Returns `Ok(true)` if the plugin was approved and loaded,
    /// `Ok(false)` if denied, or an error if something went wrong.
    pub fn resolve_plugin_approval_sync(
        &self,
        plugin_name: &str,
        approved: bool,
    ) -> AmanResult<bool> {
        pollster::block_on(async {
            self.plugin_approval_registry()
                .resolve(plugin_name, approved);

            if approved {
                let candidate = self.take_pending_plugin_candidate(plugin_name).await;
                match candidate {
                    Some((candidate, approved_caps)) => {
                        // Persist approval with BLAKE3 signature
                        if let Some(cache) = self.approval_cache() {
                            let now_ms = std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .unwrap_or_default()
                                .as_millis() as u64;
                            let mut caps = ApprovedCapabilities {
                                plugin_version: candidate.manifest.version.to_string(),
                                capabilities: approved_caps,
                                approved_at_ms: now_ms,
                                approved_by: "tui".to_owned(),
                                signature: String::new(),
                            };
                            cache.save(plugin_name, &mut caps)?;
                            tracing::info!(
                                plugin = %plugin_name,
                                "plugin capability approval persisted via TUI"
                            );
                        }
                        let mut loader = self.plugin_loader().await;
                        loader.load_plugin(candidate).await?;
                        tracing::info!(
                            plugin = %plugin_name,
                            "plugin loaded after TUI capability approval"
                        );
                        Ok(true)
                    }
                    None => {
                        tracing::warn!(
                            plugin = %plugin_name,
                            "resolve_plugin_approval_sync: no pending candidate found"
                        );
                        Err(Error::NotFound {
                            name: format!(
                                "pending plugin approval for '{plugin_name}'"
                            ),
                        })
                    }
                }
            } else {
                self.remove_pending_plugin_candidate(plugin_name).await;
                tracing::info!(
                    plugin = %plugin_name,
                    "plugin capability approval denied via TUI"
                );
                Ok(false)
            }
        })
    }

    /// Return the in-process hook registry (plugins register hooks here).
    #[must_use]
    pub fn hook_registry(&self) -> Arc<hook::HookRegistry> {
        Arc::clone(&self.hook_registry)
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
    pub fn chat_session_store(&self) -> Arc<messaging_core::ChatSessionStore> {
        Arc::clone(&self.chat_session_store)
    }

    #[must_use]
    pub fn channel_registry(&self) -> Arc<messaging_core::ChannelRegistry> {
        Arc::clone(&self.channel_registry)
    }

    #[must_use]
    pub fn sticky_router(&self) -> Arc<messaging_core::StickyAgentRouter> {
        Arc::clone(&self.sticky_router)
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

    /// Hot-reload an IM channel source from config (env vars or keychain).
    pub async fn reload_im_channel_source(&self, platform: &str, instance: &str) -> AmanResult<()> {
        let secrets_mode = self.config.security.secrets_mode;

        match platform {
            "telegram" => {
                let (token, allowed_chat_ids) = if secrets_mode.prefer_env() {
                    let token = std::env::var("AMAN_BOT_TELEGRAM_TOKEN")
                        .ok()
                        .filter(|s| !s.is_empty())
                        .ok_or_else(|| Error::NotFound {
                            name: "env var AMAN_BOT_TELEGRAM_TOKEN".into(),
                        })?;
                    let allowed_chat_ids: Vec<i64> = std::env::var("AMAN_BOT_TELEGRAM_ALLOWED_CHAT_IDS")
                        .ok()
                        .map(|ids| ids.split(',').filter_map(|s| s.trim().parse().ok()).collect())
                        .unwrap_or_default();
                    (token, allowed_chat_ids)
                } else {
                    use secret::{KeychainBackend, SecretBackend};
                    let backend = KeychainBackend;
                    let token_key = format!("aman.bot.telegram.{instance}.token");
                    let chat_ids_key = format!("aman.bot.telegram.{instance}.allowed_chat_ids");
                    let token = backend
                        .get(&token_key)?
                        .ok_or_else(|| Error::NotFound {
                            name: format!("keychain key {token_key}"),
                        })?;
                    if token.is_empty() {
                        return Err(Error::config_invalid("telegram bot token is empty"));
                    }
                    let allowed_chat_ids: Vec<i64> = backend
                        .get(&chat_ids_key)
                        .ok()
                        .flatten()
                        .map(|ids| ids.split(',').filter_map(|s| s.trim().parse().ok()).collect())
                        .unwrap_or_default();
                    (token, allowed_chat_ids)
                };

                let source_id = if instance == "default" {
                    "chat:telegram:default".to_owned()
                } else {
                    format!("chat:telegram:{instance}")
                };

                // Shut down old source if running.
                self.sources.shutdown(&source_id).await.ok();
                self.sources.unregister(&source_id).await.ok();

                // Register sender for reply routing.
                let sender = Arc::new(messaging_telegram::sender::TelegramSender::new(&token));
                self.channel_registry.register(source_id.clone(), sender);

                // Create and register the fresh source.
                let source = messaging_telegram::source::TelegramSource::new(
                    source_id.clone(),
                    &token,
                    allowed_chat_ids,
                )
                .with_registries(
                    Arc::clone(&self.sticky_router),
                    Arc::clone(&self.chat_session_store),
                );

                self.sources
                    .register(
                        Box::new(source),
                        source::SourceMode::Push,
                        source::TrustLevel::Untrusted,
                    )
                    .await?;

                self.sources.start(&source_id).await?;

                tracing::info!(
                    source_id = %source_id,
                    instance = %instance,
                    "hot-reloaded telegram IM channel source"
                );
                Ok(())
            }
            _ => Err(Error::NotFound {
                name: format!("unsupported platform for hot-reload: {platform}"),
            }),
        }
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

        // Subscribe to the global bus — captures MessageReceived, reply_ready,
        // skill:completed, and streaming events published there.
        let handler = Box::new(StoreAllEventsHandler {
            store: Arc::clone(&self.event_store),
            agent_registry: Arc::clone(&self.agent_registry),
        });
        let id = self.bus.subscribe(subscription_filter, handler).await?;
        *self.observer_subscription.lock().await = Some(id);

        // Also subscribe to each agent's local bus so that per-agent events
        // (tool:progress, tool:completed, llm:call_started, agent:busy, etc.)
        // are persisted to the session JSONL file.
        for (agent_id, local_bus) in self.agent_registry.all_local_buses().await {
            let h = Box::new(StoreAllEventsHandler {
                store: Arc::clone(&self.event_store),
                agent_registry: Arc::clone(&self.agent_registry),
            });
            if let Err(e) = local_bus
                .subscribe(event_bus::SubscriptionFilter::default(), h)
                .await
            {
                tracing::warn!(
                    agent_id = %agent_id,
                    error = %e,
                    "ensure_observer_subscribed: failed to subscribe to agent local bus"
                );
            }
        }

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
                match config::AmanConfig::from_default_path() {
                    Ok(aman_cfg) => {
                        let count = self.agent_registry.load_from_config(&aman_cfg).await;
                        tracing::info!(count, "agents loaded from config");
                        // Subscribe per-agent script hooks to each agent's local bus.
                        for agent in self.agent_registry.list().await {
                            self.subscribe_per_agent_hooks(&agent.descriptor.agent_id).await;
                        }
                    }
                    Err(e) => {
                        tracing::error!(error = %e, "Phase2: failed to load config, agents not loaded");
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
                // Start emotion evaluators (require Tokio runtime)
                self.agent_registry.start_all_emotion_evaluators().await;
                // Initialize MCP clients for all agents
                self.agent_registry.init_mcp_all(self.tools()).await;

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
                // Stop SSE background tasks first — they hold Arc<AgentRuntime>
                // refs and their never-ending loops would prevent Tokio's
                // multi-threaded Runtime::drop() from ever returning.
                self.sse_broadcast.stop_background_tasks().await;
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

        // --- Filter: skip streaming and internal events that bloat session files ---
        if let Some(sid) = session_id {
            // Skip per-token streaming chunks — the final reply_ready captures it all.
            // Also skip stream lifecycle markers and internal bus events that don't
            // contribute to conversation history.
            let etype = format!("{:?}", event.event_type);
            if etype.contains("reply_chunk")
                || etype.contains("reply_stream_start")
                || etype.contains("reply_stream_done")
                || etype.contains("llm:call_started")
                || etype.contains("llm:call_ended")
                || etype.contains("agent:busy")
                || etype.contains("agent:idle")
                || etype.contains("agent:got_tool_calls")
                || etype.contains("agent:tool_results_fed_back")
                || etype.contains("agent:history_compressed")
                || etype.contains("agent:reply_interrupted")
                || etype.contains("work:item:event")       // duplicate wrapper, not conversation
                || etype.contains("tool:dispatched")        // internal forwarding event
            {
                self.store.record(event);
                return Ok(());
            }

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
                if let Err(e) = store.append_session_event(sid, &entry) {
                    tracing::warn!(
                        session_id = %sid,
                        event_type = %format!("{:?}", event.event_type),
                        error = %e,
                        "StoreAllEvents: failed to append session event to JSONL"
                    );
                }
            } else {
                // No session store found — log for events that should
                // always have a store (e.g. MessageReceived, reply_ready).
                // This helps diagnose IM-channel session-persistence gaps.
                let etype = format!("{:?}", event.event_type);
                if etype.contains("MessageReceived")
                    || etype.contains("reply_ready")
                    || etype.contains("reply_chunk")
                {
                    tracing::warn!(
                        session_id = %sid,
                        event_type = %etype,
                        agent_id = %agent_id.unwrap_or("?"),
                        "StoreAllEvents: no session store found for conversation event"
                    );
                }
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

/// Scan keychain for configured IM channel bots and register them as
/// event sources.  Called during `build()` so the sources are registered
/// before Phase 4 starts them.
fn start_im_channel_sources(
    sources: &Arc<SourceRegistry>,
    channel_registry: &Arc<messaging_core::ChannelRegistry>,
    chat_session_store: &Arc<messaging_core::ChatSessionStore>,
    sticky_router: &Arc<messaging_core::StickyAgentRouter>,
) {
    let secrets_mode = config::AmanConfig::from_default_path()
        .map(|c| c.runtime.security.secrets_mode)
        .unwrap_or_default();

    // ── Telegram ─────────────────────────────────────────────────
    // Env var naming:
    //   AMAN_BOT_TELEGRAM_TOKEN              → default instance
    //   AMAN_BOT_TELEGRAM_{INSTANCE}_TOKEN   → named instance (uppercase)
    //   e.g. AMAN_BOT_TELEGRAM_WORK_TOKEN, AMAN_BOT_TELEGRAM_PERSONAL_TOKEN
    let instances = if secrets_mode.prefer_env() {
        let mut found = vec![];
        for inst in &["default", "work", "personal", "trading"] {
            let token_var = if *inst == "default" {
                "AMAN_BOT_TELEGRAM_TOKEN".to_owned()
            } else {
                format!("AMAN_BOT_TELEGRAM_{}_TOKEN", inst.to_ascii_uppercase())
            };
            if std::env::var(&token_var).ok().filter(|s| !s.is_empty()).is_some() {
                found.push(inst.to_string());
            }
        }
        found
    } else {
        use secret::{KeychainBackend, SecretBackend};
        let backend = KeychainBackend;
        let mut found = vec![];
        for inst in &["default", "work", "personal", "trading"] {
            let token_key = format!("aman.bot.telegram.{inst}.token");
            if backend.get(&token_key).ok().flatten().filter(|s| !s.is_empty()).is_some() {
                found.push(inst.to_string());
            }
        }
        found
    };

    for instance in instances {
        let (token, bot_username, allowed_chat_ids) = if secrets_mode.prefer_env() {
            let token_var = if instance == "default" {
                "AMAN_BOT_TELEGRAM_TOKEN".to_owned()
            } else {
                format!("AMAN_BOT_TELEGRAM_{}_TOKEN", instance.to_ascii_uppercase())
            };
            let username_var = if instance == "default" {
                "AMAN_BOT_TELEGRAM_USERNAME".to_owned()
            } else {
                format!("AMAN_BOT_TELEGRAM_{}_USERNAME", instance.to_ascii_uppercase())
            };
            let chat_ids_var = if instance == "default" {
                "AMAN_BOT_TELEGRAM_ALLOWED_CHAT_IDS".to_owned()
            } else {
                format!("AMAN_BOT_TELEGRAM_{}_ALLOWED_CHAT_IDS", instance.to_ascii_uppercase())
            };
            let token = std::env::var(&token_var).ok().filter(|s| !s.is_empty()).unwrap_or_default();
            let bot_username = std::env::var(&username_var).ok().unwrap_or_default();
            let allowed_chat_ids: Vec<i64> = std::env::var(&chat_ids_var)
                .ok()
                .map(|ids| ids.split(',').filter_map(|s| s.trim().parse().ok()).collect())
                .unwrap_or_default();
            (token, bot_username, allowed_chat_ids)
        } else {
            use secret::{KeychainBackend, SecretBackend};
            let backend = KeychainBackend;
            let token_key = format!("aman.bot.telegram.{instance}.token");
            let username_key = format!("aman.bot.telegram.{instance}.username");
            let chat_ids_key = format!("aman.bot.telegram.{instance}.allowed_chat_ids");
            let token = backend.get(&token_key).ok().flatten().unwrap_or_default();
            let bot_username = backend.get(&username_key).ok().flatten().unwrap_or_default();
            let allowed_chat_ids: Vec<i64> = backend
                .get(&chat_ids_key)
                .ok()
                .flatten()
                .map(|ids| ids.split(',').filter_map(|s| s.trim().parse().ok()).collect())
                .unwrap_or_default();
            (token, bot_username, allowed_chat_ids)
        };

        if token.is_empty() {
            continue;
        }

        let source_id = if instance == "default" {
            "chat:telegram:default".to_owned()
        } else {
            format!("chat:telegram:{instance}")
        };

        tracing::info!(
            source_id = %source_id,
            instance = %instance,
            username = %bot_username,
            allowed_chat_count = allowed_chat_ids.len(),
            mode = ?secrets_mode,
            "starting telegram IM channel source"
        );

        // Register sender for reply routing.
        let sender = Arc::new(messaging_telegram::sender::TelegramSender::new(&token));
        channel_registry.register(source_id.clone(), sender);

        // Create and register the event source.
        let source = messaging_telegram::source::TelegramSource::new(
            source_id.clone(),
            &token,
            allowed_chat_ids,
        )
        .with_registries(Arc::clone(sticky_router), Arc::clone(chat_session_store));

        let _ = pollster::block_on(sources.register(
            Box::new(source),
            source::SourceMode::Push,
            source::TrustLevel::Untrusted,
        ));
    }

    // TODO: Slack, Discord, Matrix — same pattern when their crates are wired.
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

/// Resolve the judge LLM executor from eval config + provider config.
///
/// Looks up the provider named in `EvalConfig::llm.provider` in the top-level
/// `providers` map, resolves the API key and model, and builds an
/// [`eval::strategies::llm_judge::LlmApiJudgeExecutor`].
fn resolve_judge_executor(
    judge_cfg: &eval::config::JudgeLlmConfig,
    aman: Option<&config::AmanConfig>,
) -> Option<eval::strategies::llm_judge::LlmApiJudgeExecutor> {
    let aman = aman?;
    let provider = aman.providers.get(&judge_cfg.provider)?;

    // Resolve the actual model ID from the provider's model list
    let api_model = provider
        .models
        .iter()
        .find(|m| m.id == judge_cfg.model)
        .map(|m| m.model_id.clone())
        .unwrap_or_else(|| judge_cfg.model.clone());

    let api_key = judge_cfg
        .api_key
        .clone()
        .unwrap_or_else(|| get_llm_api_key_or_inline(&judge_cfg.provider, Some(provider)));

    let base_url = judge_cfg
        .base_url
        .clone()
        .unwrap_or_else(|| provider.base_url.clone());

    Some(
        eval::strategies::llm_judge::LlmApiJudgeExecutor::from_parts(
            base_url,
            Some(api_key),
            api_model,
        )
        .with_max_tokens(1024)
        .with_timeout(60)
        .with_retries(3),
    )
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
    // Check env var first (instant), fall back to keychain.
    let env_var = format!(
        "AMAN_PROVIDER_{}_API_KEY",
        provider_key
            .to_ascii_uppercase()
            .chars()
            .map(|c| if c.is_alphanumeric() || c == '_' { c } else { '_' })
            .collect::<String>()
    );
    let from_env = std::env::var(&env_var).unwrap_or_default();
    if !from_env.is_empty() {
        return from_env;
    }
    // Keychain access may block on first use (macOS authorization prompt).
    let backend = KeychainBackend;
    if let Ok(Some(key)) = backend.get(&format!("aman.providers.{provider_key}.api_key")) {
        return key;
    }
    String::new()
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
