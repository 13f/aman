// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

use config::{AgentConfig, BusMode};
use event_bus::{try_publish, DiscardHook, EventBus, InMemoryBus, InMemoryBusConfig};
use kernel::context::ToolContext;
use kernel::event::{Event, EventType};
use kernel::hook::Hook;
use cognitive_llm;
use cognitive_react;
use kernel::llm::{LlmChatRequest, LlmProvider, LlmResponse};
use kernel::react::ParsedToolCall;
use memory::{MemoryConfig, YantrikdbProvider};
use memory_store::MemoryStorePlugin;
use info_hub::InfoHubPlugin;
use messaging_core;
use kernel::session_history::InMemorySessionHistory;
use kernel::schema::JsonSchema;
use kernel::security::{ApprovalCache, ApprovedCapabilities, CapabilitySet};
use tool::auth::PluginApprovalRegistry;
use tool::ToolSecurityConfig;
use kernel::skill::Skill;
use kernel::source::EventSource;
use kernel::tool::Tool;
use kernel::types::{BackpressureLevel, ExecutionModel, ToolMode};
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
use source::{CronJobConfig, CronSource, CronStore, SourceMode, SourceRegistry, TrustLevel};
use std::collections::{BTreeSet, HashMap};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::sync::LazyLock;
use std::sync::Mutex as StdMutex;
use std::sync::OnceLock;
use std::time::Duration;
use tokio::sync::{Mutex, RwLock};
use super::{AuditLogger, EventStore};
use super::agenverse::{Agenverse, RuntimePhase, RuntimeStatus};
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

pub struct AgentRuntimeBuilder {
    config: AgentConfig,
    runtime_dir: PathBuf,
    bind_addr: SocketAddr,
    api_token: Option<String>,
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

    pub fn build(self, agenverse: Arc<Agenverse>) -> AmanResult<Arc<AgentRuntime>> {
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
        // Sync built-in pipeline YAMLs from repo to ~/.aman/pipelines/ (preserves user modifications)
        if let Err(e) = pipeline::sync_builtin_pipelines() {
            tracing::error!(error = %e, "failed to sync built-in pipelines");
        }
        // Seed predefined agents into ~/.aman/agents/ for new users.
        let _seeded_agents = super::agent_seed::seed_builtin_agents();
        // Discover any agents manually copied into ~/.aman/agents/.
        let _discovered = super::agent_seed::discover_filesystem_agents();
        let skills_dir = super::skill_sync::aman_data_dir().join("skills");
        if let Err(e) = std::fs::create_dir_all(&skills_dir) {
            tracing::warn!(path = %skills_dir.display(), error = %e, "failed to create skills directory");
        }
        let llm_skills = skill::discover_llm_skills(&skills_dir);
        tracing::info!(count = llm_skills.len(), "discovered LLM instruction skills");
        // Apply platform/environment filtering to discovered skills.
        let llm_skills = skill::filter_skills_by_runtime(llm_skills);
        tracing::info!(count = llm_skills.len(), "skills after platform/environment filtering");
        let llm_skills_arc = Arc::new(StdMutex::new(llm_skills.clone()));

        let skills = Arc::new(skill::SkillRegistry::new());
        let tools = Arc::new(tool::ToolRegistry::new());
        if let Err(e) = tool::install_builtin_tools(&tools) {
            tracing::warn!(error = %e, "failed to install builtin tools");
        }
        // Register code agent tools for available CLI coding tools (claude, codex, etc.)
        tool::install_code_agent_tools(&tools);
        // Register cognitive tools (assess-grounding, experience-recall, etc.)
        if let Err(e) = tool::install_cognitive_tools(&tools) {
            tracing::warn!(error = %e, "failed to install cognitive tools");
        }
        // Register skill_view tool so the LLM can load SKILL.md instructions on demand.
        // Store the Arc so we can wire agent_registry after its creation.
        let skill_view_tool = Arc::new(SkillViewTool {
            skills: llm_skills.clone(),
            agent_registry: OnceLock::new(),
        });
        if let Err(e) = tools.register(Arc::clone(&skill_view_tool) as Arc<dyn Tool>) {
            tracing::warn!(error = %e, "failed to register skill_view tool");
        }
        let llm_chat_tool = Arc::new(LlmChatTool {
            agent_registry: OnceLock::new(),
        });
        if let Err(e) = tools.register(Arc::clone(&llm_chat_tool) as Arc<dyn Tool>) {
            tracing::warn!(error = %e, "failed to register llm_chat tool");
        }
        let agent_send_message_tool = Arc::new(AgentSendMessageTool {
            bus: OnceLock::new(),
        });
        if let Err(e) = tools.register(Arc::clone(&agent_send_message_tool) as Arc<dyn Tool>) {
            tracing::warn!(error = %e, "failed to register agent_send_message tool");
        }
        agent_send_message_tool.set_bus(Arc::clone(&bus));
        let agent_list_tool = Arc::new(AgentListTool {
            agent_registry: OnceLock::new(),
        });
        if let Err(e) = tools.register(Arc::clone(&agent_list_tool) as Arc<dyn Tool>) {
            tracing::warn!(error = %e, "failed to register agent_list tool");
        }
        // Register delegate_task tool for anonymous sub-agent spawning.
        // Returns the Arc so we can wire GatewaySubAgentSpawner after
        // the agent harness is created.
        let delegate_task_tool = install_delegate_task_tool(&tools);
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
        if let Err(e) = std::fs::create_dir_all(&workflows_dir) {
            tracing::warn!(path = %workflows_dir.display(), error = %e, "failed to create workflows directory");
        }
        let workflow_engine = Arc::new(WorkflowEngine::new());
        super::session::SessionManager::register_workflow(&workflow_engine);

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
        let memory_store_candidate = PluginCandidate::InProcess {
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
        let info_hub_candidate = PluginCandidate::InProcess {
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
                    try_publish(&*self.bus, Event::new(
                        "skill:dispatcher",
                        EventType::Custom("message:dispatch".to_owned()),
                        json!({
                            "trace_id": trace_id.to_string(),
                        }),
                    )).await;
                    try_publish(&*self.bus, Event::new(
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

        // ── Subscribe experience extractor ────────────────────────
        // Subscribes to workflow::completed events globally. Each event updates
        // the agent-level EXP.md based on workflow outcome.
        let exp_extractor = super::experience_extractor::ExperienceExtractor::new(
            "global",  // Global subscription — could be per-agent in future
            Arc::clone(&workflow_engine),
        );
        let _ = pollster::block_on(bus.subscribe(
            event_bus::SubscriptionFilter {
                event_types: Some(vec![
                    kernel::event::EventType::Custom("workflow::completed".to_owned()),
                ]),
                ..Default::default()
            },
            Box::new(exp_extractor),
        ));

        // ── Agent registry ──────────────────────────────────────────
        let mut agent_registry_inner = super::AgentRegistry::new(Arc::clone(&bus));
        agent_registry_inner.set_skill_index(
            Arc::clone(&skill_search),
            Arc::clone(&skills),
        );
        let agent_registry = Arc::new(agent_registry_inner);
        // Wire agent_registry into SkillViewTool for per-agent skill filtering.
        skill_view_tool.set_agent_registry(Arc::clone(&agent_registry));
        llm_chat_tool.set_agent_registry(Arc::clone(&agent_registry));
        agent_list_tool.set_agent_registry(Arc::clone(&agent_registry));

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
            let plugin_name = plugin::plugin_manifest_name(&candidate).to_string();
            let needs_approval = match (&approval_cache_runtime, plugin::plugin_manifest_security(&candidate)) {
                (Some(cache), Some(sec)) => {
                    match cache.check_approval(
                        &plugin_name,
                        &sec.requested_capabilities,
                        plugin::plugin_manifest_version(&candidate),
                    ) {
                        Ok(Some(needed_caps)) => {
                            if auto_approve {
                                // Auto-approve: persist and proceed
                                let mut caps = ApprovedCapabilities {
                                    plugin_version: plugin::plugin_manifest_version(&candidate).to_string(),
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
        rpc_handler.set_sources(Arc::clone(&sources));
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
                    if let Err(e) = tools.register(t) {
                        tracing::warn!(error = %e, "failed to register eval tool");
                    }
                }

                // Register eval hook
                if eval_cfg.auto_evaluate {
                    let hook_bus = Arc::clone(&bus);
                    let eval_hook = eval::hook::EvalHook::new(Arc::clone(&engine))
                        .with_event_publisher(Box::new(move |event| {
                            let bus = Arc::clone(&hook_bus);
                            tokio::spawn(async move {
                                event_bus::try_publish(&*bus, event).await;
                            });
                        }));
                    let _ = hook_registry.register(std::sync::Arc::new(eval_hook));
                    tracing::info!("eval hook registered for automatic evaluation (with event publishing)");
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
                    if let Err(e) = std::fs::rename(&memory_dir, &bak) {
                        tracing::warn!(from = %memory_dir.display(), to = %bak.display(), error = %e, "yantrikdb migration: failed to back up memory dir");
                    }
                    if let Err(e) = std::fs::create_dir_all(&memory_dir) {
                        tracing::warn!(path = %memory_dir.display(), error = %e, "yantrikdb migration: failed to recreate memory dir");
                    }
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
                    // Register BackendHealth for this agent's base_url (shared across agents)
                    // and record the agent_id → base_url mapping for later lookup.
                    if let Some(provider) = cfg.providers.get(&entry.provider) {
                        let base_url = provider.base_url.clone();
                        let health = pollster::block_on(
                            agent_registry.get_or_insert_backend_health(&base_url),
                        );
                        let _ = health; // stored in registry, used by record_success/failure
                        pollster::block_on(agent_registry.set_agent_base_url(agent_id, &base_url));
                    }
                    // Initialize CognitiveStateMachine for this agent.
                    let _cog = pollster::block_on(agent_registry.init_cognitive_state(
                        agent_id,
                        super::CognitiveStateConfig::default(),
                    ));
                } else {
                    // 没有可用的 LLM provider（首次启动 / 配置缺失）：
                    // 直接将 CognitiveState 设为 Coma，不做无意义的探针查询。
                    let cog = pollster::block_on(agent_registry.init_cognitive_state(
                        agent_id,
                        super::CognitiveStateConfig::default(),
                    ));
                    let _ = cog.force_coma();
                    tracing::warn!(
                        agent = %agent_id,
                        "No LLM provider available, agent starts in Coma state"
                    );
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
                if let Some(ref emotion_cfg) = cfg.emotion
                    && emotion_cfg.enabled {
                        if let Some(provider) = cfg.providers.get(&emotion_cfg.provider) {
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
        let tool_security = ToolSecurityConfig {
            allowed_paths: config.security.allowed_paths.clone(),
            network_allowed: config.security.network_allowed,
            command_allowlist: config.security.command_allowlist.clone(),
            allowlist_enabled: config.security.tool_security_enabled,
        };
        let agent_harness = Arc::new(super::agent_harness::AgentHarness::new(
            Arc::clone(&agent_registry),
            Arc::clone(&tools),
            Arc::clone(&bus),
            Box::new(InMemorySessionHistory::new()),
            Box::new(context_manager::DefaultTokenBudgetPolicy::new()),
            Box::new(super::agent_harness::FirstEnabledAgentRouter),
            compressor_config,
            tool_timeout_ms,
            config.event_bus.stream_forwarder_capacity,
            Some(tool_security),
            self.runtime_handle.clone().expect("runtime_handle must be set before build()"),
        ));
        // Wire GatewaySubAgentSpawner into delegate_task tool
        let subagent_spawner = Arc::new(super::subagent_spawner::GatewaySubAgentSpawner::new(
            Arc::clone(&agent_registry),
            Arc::clone(&agent_harness),
        ));
        delegate_task_tool.set_spawner(subagent_spawner.clone());

        // ── Create orchestrators for each agent ────────────────────────
        // Orchestrators subscribe to plan:created / plan:resumed events
        // and autonomously iterate through plan tasks.
        {
            let agents = pollster::block_on(agent_registry.list());
            for agent in agents {
                let orchestrator = Arc::new(super::orchestrator::Orchestrator::new(
                    agent.descriptor.agent_id.clone(),
                    Arc::clone(&agent_registry),
                    Arc::clone(&tools),
                    Arc::clone(&subagent_spawner),
                ));
                pollster::block_on(agent_registry.set_orchestrator(
                    &agent.descriptor.agent_id,
                    Arc::clone(&orchestrator),
                ));

                // Subscribe to plan lifecycle events on the agent's local bus.
                let local_bus =
                    pollster::block_on(agent_registry.get_local_bus(&agent.descriptor.agent_id));
                if let Some(local_bus) = local_bus {
                    let handler = Box::new(super::orchestrator::PlanEventHandler::new(
                        Arc::clone(&orchestrator),
                    ));
                    let _ = pollster::block_on(local_bus.subscribe(
                        event_bus::SubscriptionFilter {
                            event_types: Some(vec![
                                EventType::Custom("plan:created".to_owned()),
                                EventType::Custom("plan:resumed".to_owned()),
                            ]),
                            sources: None,
                            priorities: None,
                            payload_match: None,
                        },
                        handler,
                    ));
                }
            }
        }

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
            reflection_runner.set_memory_llm(cfg.clone());
            // Build and wire a dedicated LLM provider for memory work. Without
            // this, reflection silently uses the agent's main provider and
            // ignores `memory.llm.provider` — a latent bug exposed when the
            // operator configures separate backends per agent and for memory.
            if let Some(provider) = build_memory_llm_provider(aman_cfg.as_ref(), &cfg) {
                reflection_runner.set_memory_llm_provider(provider);
            } else {
                tracing::warn!(
                    provider = %cfg.provider,
                    model = %cfg.model,
                    "Reflection dedicated memory.llm provider could not be built — \
                     reflection will fall back to per-agent providers"
                );
            }
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

        // Subscribe to cold-start-done events on the global bus.
        // AgentIdleManager publishes this after its first QueueDrained (cold-start
        // or busy→empty) — the signal that an agent's AgentStatus should flip
        // from Preparing to Idle.
        {
            struct ColdStartDoneSub {
                registry: Arc<super::agent_registry::AgentRegistry>,
            }
            #[async_trait::async_trait]
            impl event_bus::EventHandler for ColdStartDoneSub {
                async fn handle(&self, event: kernel::event::Event) -> kernel::AmanResult<()> {
                    if let Some(agent_id) = event.payload.get("agent_id").and_then(|v| v.as_str()) {
                        if let Err(e) = self.registry.mark_cold_start_complete(agent_id).await {
                            tracing::warn!(agent = %agent_id, error = %e, "mark_cold_start_complete failed");
                        }
                    }
                    Ok(())
                }
            }
            let _ = pollster::block_on(bus.subscribe(
                event_bus::SubscriptionFilter {
                    event_types: Some(vec![kernel::event::EventType::Custom(
                        idle::COLD_START_DONE_EVENT.to_owned(),
                    )]),
                    sources: None,
                    priorities: None,
                    payload_match: None,
                },
                Box::new(ColdStartDoneSub {
                    registry: Arc::clone(&agent_registry),
                }),
            ));
        }

        // ── Sleep actor (Idle kind=Sleep → cognitive housekeeping) ──────
        let sleep_cfg = aman_cfg
            .as_ref()
            .map(|c| c.runtime.idle.sleep.clone())
            .unwrap_or_default();
        let sleep_actor_config = idle::SleepActorConfig {
            max_cpu_seconds: sleep_cfg.max_cpu_seconds,
            short_term_retention_days: sleep_cfg.short_term_retention_days,
            cache_expiry_days: sleep_cfg.cache_expiry_days,
            stale_background_retention_days: sleep_cfg.stale_background_retention_days,
            stale_background_min_reply_chars: sleep_cfg.stale_background_min_reply_chars,
            sleep_cooldown_secs: sleep_cfg.cooldown_secs,
            wakeup_delay_secs: sleep_cfg.wakeup_delay_secs,
            wakeup_poll_steps: sleep_cfg.wakeup_poll_steps,
        };
        let memory_llm_for_incubation = memory_llm_cfg.clone();
        let sleep_housekeeper = Arc::new(super::sleep::GatewaySleepHousekeeper::new(
            Arc::clone(&agent_registry),
            memory_llm_cfg,
            sleep_actor_config.clone(),
        ));
        let sleep_actor = idle::SleepActor::new(
            sleep_actor_config,
            sleep_housekeeper as Arc<dyn idle::SleepHousekeeper>,
        );

        // Subscribe to Idle events on the global bus (SleepActor filters to kind="sleep")
        {
            let _ = pollster::block_on(bus.subscribe(
                event_bus::SubscriptionFilter {
                    event_types: Some(vec![kernel::event::EventType::Idle]),
                    sources: None,
                    priorities: None,
                    payload_match: None,
                },
                Box::new(sleep_actor),
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
        incubation_runner.set_self_bridge(self_bridge.clone());

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

                // Resolve the continuation mode the HTTP handler detected.
                // "continue" => user is resuming a prior task — the cognitive
                // engine should inject a structured session summary instead of
                // appending a fresh turn.
                let continuation_mode = match event.payload.get("continuation_mode").and_then(|v| v.as_str()) {
                    Some("continue") => super::agent_harness::ContinuationMode::Continue,
                    _ => super::agent_harness::ContinuationMode::Fresh,
                };

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

                // Transition workflow: ACTIVE → PROCESSING (or IDLE → PROCESSING).
                // Must happen before spawning LLM processing so that
                // LLM_REPLY_READY can transition PROCESSING → IDLE later.
                let transition_event = Event::new(
                    "session:control",
                    EventType::Custom("MESSAGE_RECEIVED".to_owned()),
                    json!({
                        "session_id": session_id,
                        "agent_id": agent_id,
                    }),
                );
                if let Err(e) = self.session_manager
                    .workflow_engine()
                    .handle_event(&session_id, transition_event)
                    .await
                {
                    tracing::warn!(
                        session_id = %session_id,
                        agent_id = %agent_id,
                        error = %e,
                        "MessageReceivedHandler: failed to transition session workflow on MESSAGE_RECEIVED"
                    );
                }

                // ── CLOSED / TIMEOUT resurrection: drive IDLE → PROCESSING ──
                // A session that was CLOSED (or is still in TIMEOUT) by the
                // time a new message arrives only advances to IDLE on the first
                // MESSAGE_RECEIVED (CLOSED → IDLE, TIMEOUT → IDLE).  The
                // harness then spawns engine.process() and later publishes
                // LLM_REPLY_READY, which expects the session to be in
                // PROCESSING — without a second transition the reply would
                // fail ("no transition from IDLE on LLM_REPLY_READY") and
                // the reply would be silently dropped.
                //
                // If the first transition landed us in IDLE, fire a second
                // MESSAGE_RECEIVED synchronously so the session is in
                // PROCESSING before the async task runs.  This is a tight
                // synchronous sequence (no await between the two) so the
                // timeout poller cannot interleave a TIMEOUT → CLOSED in
                // between.  A parallel second message racing us here is
                // harmless: it would either (a) also drive IDLE → PROCESSING
                // (no-op, already there) or (b) arrive after we're already
                // in PROCESSING (its MESSAGE_RECEIVED fails, logs a warn,
                // but the in-flight task keeps running).
                if self.session_manager.workflow_engine()
                    .get_instance(&session_id)
                    .map(|inst| inst.current_state == "IDLE")
                    .unwrap_or(false)
                {
                    let transition_event_2 = Event::new(
                        "session:control",
                        EventType::Custom("MESSAGE_RECEIVED".to_owned()),
                        json!({
                            "session_id": session_id,
                            "agent_id": agent_id,
                        }),
                    );
                    if let Err(e) = self.session_manager
                        .workflow_engine()
                        .handle_event(&session_id, transition_event_2)
                        .await
                    {
                        tracing::warn!(
                            session_id = %session_id,
                            agent_id = %agent_id,
                            error = %e,
                            "MessageReceivedHandler: failed to drive resurrected session IDLE → PROCESSING"
                        );
                    }
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
                // For background/idle_run sessions without a pre-built prompt, build the
                // full system prompt (soul + skills + tools + date + discipline + hints)
                // matching foreground chat sessions.
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
                                let skills = self.llm_skills.lock()
                                    .ok()
                                    .map(|g| g.clone())
                                    .unwrap_or_default();
                                let cwd = std::env::current_dir()
                                    .ok()
                                    .and_then(|p| p.to_str().map(String::from));
                                let prompt = pollster::block_on(
                                    self.agent_harness.build_full_system_prompt(
                                        &agent_id, &soul.raw, &skills,
                                        &self.self_bridge, cwd.as_deref(),
                                    ),
                                );
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
                if !background {
                    let is_work = super::session::work_session::parse_work_session_id(&session_id).is_some();
                    let _ = self.agent_registry.set_status(&agent_id, kernel::agent::AgentStatus::Busy).await;
                    self.agent_registry.set_system_state(
                        &agent_id,
                        if is_work { kernel::agent::AgentSystemState::Working }
                        else { kernel::agent::AgentSystemState::Chatting },
                    ).await;
                } else if skill_name.is_some() {
                    // Background idle_run (boredom): the idle manager / HTTP skill
                    // endpoint already set system_state correctly based on the
                    // boredom tag (fun→DailyLife, work→Working, study→Studying,
                    // prize→Prize). Only set the activity to the chosen skill name
                    // so the UI reflects what the agent is doing right away —
                    // leave system_state untouched to preserve that tag mapping.
                    self.agent_registry.set_activity(
                        &agent_id,
                        skill_name.as_deref().unwrap_or(""),
                    ).await;
                }

                // Spawn async ReAct processing — do not block the bus drain loop.
                // `continuation_mode` already reflects the HTTP-layer intent
                // (Continue for "继续" / "continue" / /continue; Fresh otherwise).
                self.agent_harness.spawn_process_message(
                    agent_id, session_id, text, model, soul_snapshot,
                    skill_name, react_mode, background,
                    continuation_mode,
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
                    // Skip if the session is already CLOSED.  A reply arriving
                    // after the session has been closed belongs to the previous
                    // (now-abandoned) task — delivering it would either fail
                    // ("no transition from CLOSED on LLM_REPLY_READY",
                    // before the DAG change above) or poison a newly reopened
                    // session (PROCESSING→IDLE flipping the fresh session to
                    // IDLE before it even processes anything).  The CLOSED
                    // session is re-opened by the *next* incoming message, not
                    // by a stale reply.
                    if let Some(inst) = self.session_manager.workflow_engine()
                        .get_instance(session_id)
                    {
                        if inst.current_state == "CLOSED" {
                            tracing::warn!(
                                session_id,
                                agent_id,
                                "SessionReplyHandler: dropping reply for CLOSED session"
                            );
                            return Ok(());
                        }
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
            self_bridge: super::self_bridge::SelfBridge,
            bus: Arc<dyn EventBus>,
            a2a_base: PathBuf,
        }
        #[async_trait::async_trait]
        impl event_bus::EventHandler for AgentMessageHandler {
            async fn handle(&self, event: kernel::event::Event) -> kernel::AmanResult<()> {
                // Ignore events published by this handler itself — these are
                // reply AgentMessages bouncing back. Only process original
                // messages from tools, plugins, or other agents.
                if event.source.as_str() == "agent-message-handler" {
                    return Ok(());
                }

                let msg: kernel::agent::AgentMessage = match serde_json::from_value(event.payload) {
                    Ok(m) => m,
                    Err(e) => {
                        tracing::warn!("AgentMessageHandler: failed to parse AgentMessage: {e}");
                        return Ok(());
                    }
                };

                // Resolve target agent
                let agent = match self.agent_harness.resolve_agent(&msg.to_agent).await {
                    Some(a) => a,
                    None => {
                        tracing::warn!("AgentMessageHandler: target agent '{}' not found or disabled", msg.to_agent);
                        return Ok(());
                    }
                };

                // ── A2A session path ──
                let mut ids = [msg.from_agent.as_str(), msg.to_agent.as_str()];
                ids.sort_unstable();
                let a2a_dir = self.a2a_base.join(format!("{}__{}", ids[0], ids[1]));
                let _ = std::fs::create_dir_all(&a2a_dir);

                // Use existing session_id or create a new one
                let a2a_sid = msg.session_id.clone().unwrap_or_else(|| {
                    uuid::Uuid::now_v7().to_string().replace('-', "")
                });
                let jsonl_path = a2a_dir.join(format!("{}.jsonl", a2a_sid));

                // Load conversation history
                let history: Vec<String> = std::fs::read_to_string(&jsonl_path)
                    .unwrap_or_default()
                    .lines()
                    .filter(|l| !l.is_empty())
                    .map(|l| {
                        if let Ok(v) = serde_json::from_str::<serde_json::Value>(l) {
                            let f = v["from_agent"].as_str().unwrap_or("");
                            let t = v["text"].as_str().unwrap_or("");
                            format!("[{}]: {t}", f)
                        } else { String::new() }
                    })
                    .collect();

                // Append incoming message to jsonl
                let ts = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis();
                let entry = serde_json::json!({
                    "timestamp_ms": ts,
                    "from_agent": msg.from_agent,
                    "to_agent": msg.to_agent,
                    "content_type": msg.content_type,
                    "text": msg.payload.get("text").and_then(|v| v.as_str()).unwrap_or(""),
                    "message_id": msg.message_id.to_string(),
                    "reply_to": msg.reply_to.map(|u| u.to_string()),
                });
                if let Err(e) = std::fs::OpenOptions::new().create(true).append(true).open(&jsonl_path)
                    .and_then(|mut f| { use std::io::Write; writeln!(f, "{}", serde_json::to_string(&entry).unwrap_or_default()) })
                {
                    tracing::warn!(path=%jsonl_path.display(), error=%e, "AgentMessageHandler: failed to write jsonl");
                }

                // Build system prompt with agent identity + conversation context
                // Load SOUL.md from the agent's data directory.
                // Avoid self_bridge warnings when SoulRuntime returns empty content.
                let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
                let soul_path = PathBuf::from(&home)
                    .join(".aman").join("agents").join(&msg.to_agent).join("SOUL.md");
                let soul_raw = std::fs::read_to_string(&soul_path).unwrap_or_default();
                let cwd = std::env::current_dir().ok().and_then(|p| p.to_str().map(String::from));
                let base_prompt = pollster::block_on(
                    self.agent_harness.build_full_system_prompt(
                        &msg.to_agent, &soul_raw, &[], &self.self_bridge, cwd.as_deref(),
                    ),
                );
                let history_block = if history.is_empty() {
                    String::new()
                } else {
                    format!("\n\n## Conversation with {}\n{}", msg.from_agent,
                        history.iter().rev().take(20).rev().cloned().collect::<Vec<_>>().join("\n"))
                };
                let system_prompt = format!(
                    "You are agent '{}'.{}{}",
                    msg.to_agent,
                    if base_prompt.is_empty() { String::new() } else { format!("\n\n{base_prompt}") },
                    history_block,
                );
                let soul_snapshot = kernel::react::SoulSnapshot::new(
                    agent.descriptor.display_name.clone(),
                    system_prompt,
                );

                let text = format!(
                    "[Message from agent '{}']\n{}",
                    msg.from_agent,
                    msg.payload.get("text").and_then(|v| v.as_str()).unwrap_or("")
                );

                // Process inline via process_message_v2
                let bus = Arc::clone(&self.bus);
                let harness = Arc::clone(&self.agent_harness);
                let to_agent = msg.to_agent.clone();
                let from_agent = msg.from_agent.clone();
                let incoming_message_id = msg.message_id;
                let session_id = a2a_sid.clone();
                let model = agent.descriptor.model.clone();
                let jsonl_path_clone = jsonl_path.clone();

                // Only auto-reply to original messages (reply_to=None).
                // Messages that already have reply_to set are replies
                // themselves — the LLM must explicitly call agent_send_message
                // to continue. This prevents infinite ping-pong loops.
                let is_original = msg.reply_to.is_none();

                tokio::spawn(async move {
                    let sid = format!("a2a:{}", session_id);
                    match harness.process_message_v2(&to_agent, &sid, &text, &model, soul_snapshot, None, false, super::agent_harness::ContinuationMode::Fresh).await {
                        Ok(reply) => {
                            if !is_original {
                                // Message was a reply — process but don't auto-respond.
                                // Write the reply to jsonl for record-keeping only.
                                let ts = std::time::SystemTime::now()
                                    .duration_since(std::time::UNIX_EPOCH)
                                    .unwrap_or_default()
                                    .as_millis();
                                let reply_entry = serde_json::json!({
                                    "timestamp_ms": ts,
                                    "from_agent": &to_agent,
                                    "to_agent": &from_agent,
                                    "content_type": "result_sharing",
                                    "text": &reply,
                                    "message_id": uuid::Uuid::new_v4().to_string(),
                                    "reply_to": incoming_message_id.to_string(),
                                });
                                let _ = std::fs::OpenOptions::new().create(true).append(true).open(&jsonl_path_clone)
                                    .and_then(|mut f| { use std::io::Write; writeln!(f, "{}", serde_json::to_string(&reply_entry).unwrap_or_default()) });
                                return;
                            }
                            let reply_message_id = uuid::Uuid::new_v4();
                            // Write reply to jsonl
                            let ts = std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .unwrap_or_default()
                                .as_millis();
                            let reply_entry = serde_json::json!({
                                "timestamp_ms": ts,
                                "from_agent": &to_agent,
                                "to_agent": &from_agent,
                                "content_type": "result_sharing",
                                "text": &reply,
                                "message_id": reply_message_id.to_string(),
                                "reply_to": incoming_message_id.to_string(),
                            });
                            let _ = std::fs::OpenOptions::new().create(true).append(true).open(&jsonl_path_clone)
                                .and_then(|mut f| { use std::io::Write; writeln!(f, "{}", serde_json::to_string(&reply_entry).unwrap_or_default()) });

                            // Send reply back to sender
                            let reply_msg = kernel::agent::AgentMessage {
                                message_id: reply_message_id,
                                from_agent: to_agent,
                                to_agent: from_agent,
                                content_type: kernel::agent::AgentMessageType::ResultSharing,
                                payload: serde_json::json!({"text": reply}),
                                reply_to: Some(incoming_message_id),
                                session_id: Some(session_id),
                            };
                            let _ = bus.publish(kernel::event::Event::new(
                                "agent-message-handler",
                                kernel::event::EventType::AgentMessage,
                                serde_json::to_value(&reply_msg).unwrap_or_default(),
                            )).await;
                        }
                        Err(e) => {
                            tracing::error!(%e, "AgentMessageHandler: process_message_v2 failed");
                        }
                    }
                });

                Ok(())
            }
        }
        let a2a_base = std::env::var("HOME")
            .map(|h| PathBuf::from(h).join(".aman").join("a2a"))
            .unwrap_or_else(|_| PathBuf::from("/tmp/aman/a2a"));
        let _ = pollster::block_on(bus.subscribe(
            event_bus::SubscriptionFilter {
                event_types: Some(vec![kernel::event::EventType::AgentMessage]),
                sources: None,
                priorities: None,
                payload_match: None,
            },
            Box::new(AgentMessageHandler {
                agent_harness: Arc::clone(&agent_harness),
                self_bridge: self_bridge.clone(),
                bus: Arc::clone(&bus),
                a2a_base,
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
            if let Err(e) = std::fs::create_dir_all(&hooks_dir) {
                tracing::warn!(path = %hooks_dir.display(), error = %e, "failed to create hooks directory");
            }
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
        let sse_state = super::sse::new_sse_state(config.runtime.sse_messages_capacity);
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
            timeout_poll_task: Mutex::new(None),
            timeout_poll_notify: Arc::new(tokio::sync::Notify::new()),
            agenverse,
            inflight_pipelines,
            inflight_skills,
            metrics,
            capability_registry: Default::default(),
            llm_skills: llm_skills_arc,
            notifications,
            chat_session_store,
            channel_registry,
            sticky_router,
            agent_registry,
            agent_harness,
            session_manager,
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
        try_publish(&*self.bus, Event::new(
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
        try_publish(&*self.bus, Event::new(
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
        try_publish(&*self.bus, Event::new(
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
/// The system prompt instructs the LLM to use `skill_view` when it needs more
/// than the skill name+description index.
///
/// # Usage
///
/// - `skill_view(name)` — returns the full methodology, the skill's base
///   directory, and a listing of supporting files with their absolute paths.
/// - `skill_view(name, file_path="prompts/foo.md")` — reads a specific file
///   from within the skill directory (path-traversal protected).
struct SkillViewTool {
    skills: Vec<skill::SkillInfo>,
    agent_registry: OnceLock<Arc<super::AgentRegistry>>,
}

impl SkillViewTool {
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
impl Tool for SkillViewTool {
    fn name(&self) -> &str {
        "skill_view"
    }

    fn mode(&self) -> ToolMode {
        ToolMode::Local
    }

    fn description(&self) -> &str {
        "View a skill by name. Without file_path: returns the skill's full methodology, base directory, and a listing of all supporting files (scripts, templates, etc.) with their absolute paths. With file_path: reads a specific file from within the skill directory (e.g. prompts, templates, data files). Always use this instead of raw filesystem reads for skill files — it enforces path-traversal protection."
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
                    },
                    "file_path": {
                        "type": "string",
                        "description": "Optional: a path relative to the skill directory (e.g. \"prompts/bazi-prompt.md\"). When provided, reads and returns that specific file instead of the full skill listing."
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
                    "directory": {"type": "string"},
                    "file_path": {"type": "string"},
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

        let skill_dir = skill.path.parent().unwrap_or_else(|| Path::new("."));
        let file_path = params.get("file_path").and_then(|v| v.as_str()).filter(|s| !s.is_empty());

        // ── file_path mode: read a specific supporting file ──────────
        if let Some(fp) = file_path {
            match skill::execution::resolve_skill_file_path(skill_dir, fp) {
                Some(resolved) => match std::fs::read_to_string(&resolved) {
                    Ok(content) => Ok(serde_json::json!({
                        "name": skill.name,
                        "file_path": fp,
                        "content": content,
                    })),
                    Err(e) => Ok(serde_json::json!({
                        "name": skill_name,
                        "file_path": fp,
                        "content": "",
                        "error": format!("Failed to read file '{fp}': {e}")
                    })),
                },
                None => Ok(serde_json::json!({
                    "name": skill_name,
                    "file_path": fp,
                    "content": "",
                    "error": format!("Invalid file_path '{fp}': path traversal rejected or path is absolute")
                })),
            }
        } else {
            // ── Full skill view: methodology + directory + supporting files ──
            let raw = match std::fs::read_to_string(&skill.path) {
                Ok(c) => c,
                Err(e) => return Ok(serde_json::json!({
                    "name": skill_name, "content": "",
                    "error": format!("Failed to read skill file: {e}")
                })),
            };
            let body = skill::formatting::strip_frontmatter(&raw).trim().to_owned();
            let (dir_header, supporting_files_footer) =
                skill::execution::build_skill_directory_context(skill_dir);
            let content = format!("{dir_header}\n{body}{supporting_files_footer}");

            Ok(serde_json::json!({
                "name": skill.name,
                "content": content,
                "directory": skill_dir.display().to_string(),
            }))
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
            .and_then(|s| match s {
                "json_object" => Some(kernel::llm::ResponseFormat::JsonObject),
                _ => None,
            });

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

// ── AgentSendMessageTool ──────────────────────────────────────────────────

/// Built-in tool that lets an LLM send a structured message to another agent.
///
/// Published as `EventType::AgentMessage` on the global bus, where
/// [`AgentMessageHandler`] picks it up and routes it to the target agent's
/// ReAct loop.
struct AgentSendMessageTool {
    bus: OnceLock<Arc<dyn EventBus>>,
}

impl AgentSendMessageTool {
    fn set_bus(&self, bus: Arc<dyn EventBus>) {
        let _ = self.bus.set(bus);
    }
}

#[async_trait::async_trait]
impl Tool for AgentSendMessageTool {
    fn name(&self) -> &str {
        "agent_send_message"
    }

    fn mode(&self) -> ToolMode {
        ToolMode::Local
    }

    fn execution_model(&self) -> ExecutionModel {
        ExecutionModel::SideEffect
    }

    fn description(&self) -> &str {
        "Send a structured message to another agent. Use this when you need help from or want to delegate work to another agent. The target agent will receive your message and respond autonomously."
    }

    fn parameters(&self) -> &JsonSchema {
        static PARAMS: LazyLock<JsonSchema> = LazyLock::new(|| {
            JsonSchema::from(serde_json::json!({
                "type": "object",
                "required": ["from_agent", "to_agent", "content_type", "text"],
                "properties": {
                    "from_agent": {
                        "type": "string",
                        "description": "Your agent_id (the sender)"
                    },
                    "to_agent": {
                        "type": "string",
                        "description": "The agent_id of the target agent (e.g. \"reviewer\", \"coder\")"
                    },
                    "content_type": {
                        "type": "string",
                        "enum": ["task_delegation", "result_sharing", "status_query"],
                        "description": "Type of message: task_delegation (ask agent to do work), result_sharing (share completed work), status_query (ask about progress)"
                    },
                    "text": {
                        "type": "string",
                        "description": "The message body — describe what you need, include relevant context, file paths, etc."
                    },
                    "reply_to": {
                        "type": "string",
                        "description": "Optional: message_id of a previous message this is replying to"
                    },
                    "session_id": {
                        "type": "string",
                        "description": "Optional: a2a session id to continue an existing conversation. If absent, a new session is created."
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
                    "ok": {"type": "boolean"},
                    "message_id": {"type": "string"},
                    "to_agent": {"type": "string"},
                    "session_id": {"type": "string", "description": "Use this in subsequent messages to continue the same a2a conversation"}
                }
            }))
        });
        &RETURNS
    }

    async fn execute(&self, params: serde_json::Value, _ctx: ToolContext) -> kernel::AmanResult<serde_json::Value> {
        let from_agent = params["from_agent"]
            .as_str()
            .ok_or_else(|| kernel::Error::ConfigInvalid {
                message: "missing from_agent".to_owned(),
            })?
            .to_owned();
        let to_agent = params["to_agent"]
            .as_str()
            .ok_or_else(|| kernel::Error::ConfigInvalid {
                message: "missing to_agent".to_owned(),
            })?
            .to_owned();
        let content_type_str = params["content_type"].as_str().unwrap_or("task_delegation");
        let content_type = match content_type_str {
            "task_delegation" => kernel::agent::AgentMessageType::TaskDelegation,
            "result_sharing" => kernel::agent::AgentMessageType::ResultSharing,
            "status_query" => kernel::agent::AgentMessageType::StatusQuery,
            other => {
                return Err(kernel::Error::ConfigInvalid {
                    message: format!("unknown content_type: {other}"),
                });
            }
        };
        let text = params["text"].as_str().unwrap_or("").to_owned();
        let reply_to = params["reply_to"]
            .as_str()
            .and_then(|s| uuid::Uuid::parse_str(s).ok());
        let session_id = params["session_id"].as_str().map(|s| s.to_owned());

        let msg = kernel::agent::AgentMessage {
            message_id: uuid::Uuid::new_v4(),
            from_agent,
            to_agent: to_agent.clone(),
            content_type,
            payload: serde_json::json!({"text": text}),
            reply_to,
            session_id,
        };
        let message_id_str = msg.message_id.to_string();
        let event_payload = serde_json::to_value(&msg).map_err(|e| {
            kernel::Error::ConfigInvalid {
                message: format!("serialize AgentMessage: {e}"),
            }
        })?;

        let bus = self.bus.get().ok_or_else(|| kernel::Error::ConfigInvalid {
            message: "AgentSendMessageTool: event bus not wired".to_owned(),
        })?;
        bus.publish(kernel::event::Event::new(
            "tool:agent_send_message",
            kernel::event::EventType::AgentMessage,
            event_payload,
        ))
        .await
        .map_err(|e| kernel::Error::ConfigInvalid {
            message: format!("publish AgentMessage: {e}"),
        })?;

        Ok(serde_json::json!({
            "ok": true,
            "message_id": message_id_str,
            "to_agent": to_agent,
            "session_id": msg.session_id.clone()
        }))
    }
}

// ── AgentListTool ─────────────────────────────────────────────────────────

/// Built-in tool that lets an LLM discover available agents and their
/// capabilities before sending messages or delegating work.
struct AgentListTool {
    agent_registry: OnceLock<Arc<super::AgentRegistry>>,
}

impl AgentListTool {
    fn set_agent_registry(&self, registry: Arc<super::AgentRegistry>) {
        let _ = self.agent_registry.set(registry);
    }
}

#[async_trait::async_trait]
impl Tool for AgentListTool {
    fn name(&self) -> &str {
        "agent_list"
    }

    fn mode(&self) -> ToolMode {
        ToolMode::Local
    }

    fn execution_model(&self) -> ExecutionModel {
        ExecutionModel::Independent
    }

    fn description(&self) -> &str {
        "List all available agents with their capabilities, status, and queue information. Use this before calling agent_send_message to discover which agents exist and what they can do."
    }

    fn parameters(&self) -> &JsonSchema {
        static PARAMS: LazyLock<JsonSchema> = LazyLock::new(|| {
            JsonSchema::from(serde_json::json!({
                "type": "object",
                "properties": {
                    "capability_filter": {
                        "type": "string",
                        "description": "Optional: only return agents that have this capability tag (e.g. \"review\", \"code\")"
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
                    "agents": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "agent_id": {"type": "string"},
                                "display_name": {"type": "string"},
                                "status": {"type": "string"},
                                "capabilities": {"type": "array", "items": {"type": "string"}},
                                "queue_length": {"type": "integer"},
                                "queue_max_size": {"type": "integer"}
                            }
                        }
                    }
                }
            }))
        });
        &RETURNS
    }

    async fn execute(&self, params: serde_json::Value, _ctx: ToolContext) -> kernel::AmanResult<serde_json::Value> {
        let registry = self.agent_registry.get().ok_or_else(|| {
            kernel::Error::ConfigInvalid {
                message: "AgentListTool: agent registry not wired".to_owned(),
            }
        })?;

        let cap_filter: Option<String> = params
            .get("capability_filter")
            .and_then(|v| v.as_str())
            .map(|s| s.to_lowercase());

        let agents = registry.list().await;
        let result: Vec<serde_json::Value> = agents
            .iter()
            .filter(|a| {
                if let Some(ref filter) = cap_filter {
                    a.descriptor
                        .capabilities
                        .iter()
                        .any(|c| c.to_lowercase().contains(filter))
                } else {
                    true
                }
            })
            .map(|a| {
                serde_json::json!({
                    "agent_id": a.descriptor.agent_id,
                    "display_name": a.descriptor.display_name,
                    "status": a.status,
                    "capabilities": a.descriptor.capabilities,
                    "queue_length": 0,  // queue_length is per WorkSystem, not snapshot here
                    "queue_max_size": a.descriptor.queue_max_size,
                })
            })
            .collect();

        Ok(serde_json::json!({"agents": result}))
    }
}

/// JSON-RPC method handler for subprocess plugins.
/// Gives plugins access to AgentRegistry and EventBus.
struct RuntimeJsonRpcHandler {
    agent_registry: Arc<super::AgentRegistry>,
    bus: Arc<dyn EventBus>,
    notifications: OnceLock<Arc<notification::NotificationStore>>,
    sources: OnceLock<Arc<SourceRegistry>>,
}

impl RuntimeJsonRpcHandler {
    fn new(agent_registry: Arc<super::AgentRegistry>, bus: Arc<dyn EventBus>) -> Self {
        Self {
            agent_registry,
            bus,
            notifications: OnceLock::new(),
            sources: OnceLock::new(),
        }
    }

    fn set_notifications(&self, store: Arc<notification::NotificationStore>) {
        let _ = self.notifications.set(store);
    }

    fn set_sources(&self, sources: Arc<SourceRegistry>) {
        let _ = self.sources.set(sources);
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
                            "capabilities": a.descriptor.capabilities,
                            "queue_max_size": a.descriptor.queue_max_size,
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
            "aman.send_agent_message" => {
                let from_agent = params
                    .get("from_agent")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| kernel::Error::ConfigInvalid {
                        message: "missing from_agent".to_owned(),
                    })?
                    .to_owned();
                let to_agent = params
                    .get("to_agent")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| kernel::Error::ConfigInvalid {
                        message: "missing to_agent".to_owned(),
                    })?
                    .to_owned();
                let content_type_str = params
                    .get("content_type")
                    .and_then(|v| v.as_str())
                    .unwrap_or("task_delegation");
                let content_type = match content_type_str {
                    "task_delegation" => kernel::agent::AgentMessageType::TaskDelegation,
                    "result_sharing" => kernel::agent::AgentMessageType::ResultSharing,
                    "status_query" => kernel::agent::AgentMessageType::StatusQuery,
                    other => {
                        return Err(kernel::Error::ConfigInvalid {
                            message: format!("unknown content_type: {other}"),
                        });
                    }
                };
                let payload = params
                    .get("payload")
                    .cloned()
                    .unwrap_or(serde_json::Value::Null);
                let reply_to = params
                    .get("reply_to")
                    .and_then(|v| v.as_str())
                    .and_then(|s| uuid::Uuid::parse_str(s).ok());
                let session_id = params
                    .get("session_id")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_owned());

                let msg = kernel::agent::AgentMessage {
                    message_id: uuid::Uuid::new_v4(),
                    from_agent,
                    to_agent,
                    content_type,
                    payload,
                    reply_to,
                    session_id,
                };
                let event_payload = serde_json::to_value(&msg).map_err(|e| {
                    kernel::Error::ConfigInvalid {
                        message: format!("serialize AgentMessage: {e}"),
                    }
                })?;
                self.bus
                    .publish(kernel::event::Event::new(
                        "gateway:send_agent_message",
                        kernel::event::EventType::AgentMessage,
                        event_payload,
                    ))
                    .await
                    .map_err(|e| kernel::Error::ConfigInvalid {
                        message: format!("publish AgentMessage: {e}"),
                    })?;
                Ok(serde_json::json!({
                    "ok": true,
                    "message_id": msg.message_id.to_string()
                }))
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
                if expression.trim().is_empty() {
                    return Err(kernel::Error::ConfigInvalid {
                        message: "cron expression is empty".to_owned(),
                    });
                }
                let agent_key = params
                    .get("agent_key")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let sr = self
                    .sources
                    .get()
                    .ok_or_else(|| kernel::Error::Unrecoverable {
                        message: "source registry not initialized".to_owned(),
                    })?;
                let cron_source = source::CronSource::new(id.to_owned(), expression)
                    .map_err(|e| kernel::Error::Unrecoverable {
                        message: format!("invalid cron source: {e}"),
                    })?;
                sr.register(
                    Box::new(cron_source),
                    source::SourceMode::Pull,
                    source::TrustLevel::Untrusted,
                )
                .await
                .map_err(|e| kernel::Error::Unrecoverable {
                    message: format!("register cron source failed: {e}"),
                })?;
                sr.start(id).await.map_err(|e| kernel::Error::Unrecoverable {
                    message: format!("start cron source failed: {e}"),
                })?;

                // Persist to agent's cron directory.
                let store = source::CronStore::new(agent_cron_dir(agent_key));
                if let Err(e) = store
                    .add(&source::CronJobConfig::new(id.to_owned(), expression))
                    .await
                {
                    tracing::warn!(
                        plugin = %plugin_name,
                        cron_id = %id,
                        error = %e,
                        "plugin registered cron job but failed to persist"
                    );
                }
                tracing::info!(
                    plugin = %plugin_name,
                    cron_id = %id,
                    expression = %expression,
                    "plugin registered cron job"
                );
                Ok(serde_json::json!({"ok": true, "id": id}))
            }
            "aman.update_cron_job" => {
                let id = params
                    .get("id")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| kernel::Error::ConfigInvalid {
                        message: "id is required".to_owned(),
                    })?;
                let expression = params
                    .get("expression")
                    .and_then(|v| v.as_str());
                let timezone = params
                    .get("timezone")
                    .and_then(|v| v.as_str());
                let agent_key = params
                    .get("agent_key")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let sr = self
                    .sources
                    .get()
                    .ok_or_else(|| kernel::Error::Unrecoverable {
                        message: "source registry not initialized".to_owned(),
                    })?;
                // Reconfigure the running source if expression or timezone changed.
                if let Some(expr) = expression {
                    let mut config = serde_json::json!({"expression": expr});
                    if let Some(tz) = timezone {
                        config["timezone"] = serde_json::Value::String(tz.to_owned());
                    }
                    sr.reconfigure(id, config)
                        .await
                        .map_err(|e| kernel::Error::Unrecoverable {
                            message: format!("reconfigure cron source failed: {e}"),
                        })?;
                }
                // Persist.
                if let Some(expr) = expression {
                    let store = source::CronStore::new(agent_cron_dir(agent_key));
                    let tz = timezone.unwrap_or("UTC");
                    if let Err(e) = store.update(id, expr, tz).await {
                        tracing::warn!(
                            plugin = %plugin_name,
                            cron_id = %id,
                            error = %e,
                            "updated cron job but failed to persist"
                        );
                    }
                }
                tracing::info!(
                    plugin = %plugin_name,
                    cron_id = %id,
                    "plugin updated cron job"
                );
                Ok(serde_json::json!({"ok": true, "id": id}))
            }
            "aman.remove_cron_job" => {
                let id = params
                    .get("id")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| kernel::Error::ConfigInvalid {
                        message: "id is required".to_owned(),
                    })?;
                let agent_key = params
                    .get("agent_key")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let sr = self
                    .sources
                    .get()
                    .ok_or_else(|| kernel::Error::Unrecoverable {
                        message: "source registry not initialized".to_owned(),
                    })?;
                sr.shutdown(id).await.map_err(|e| kernel::Error::Unrecoverable {
                    message: format!("shutdown cron source failed: {e}"),
                })?;
                sr.unregister(id).await.map_err(|e| kernel::Error::Unrecoverable {
                    message: format!("unregister cron source failed: {e}"),
                })?;
                // Remove from persistence.
                let store = source::CronStore::new(agent_cron_dir(agent_key));
                if let Err(e) = store.remove(id).await {
                    tracing::warn!(
                        plugin = %plugin_name,
                        cron_id = %id,
                        error = %e,
                        "removed cron job but failed to persist"
                    );
                }
                tracing::info!(
                    plugin = %plugin_name,
                    cron_id = %id,
                    "plugin removed cron job"
                );
                Ok(serde_json::json!({"ok": true, "id": id}))
            }
            "aman.list_cron_jobs" => {
                let agent_key = params
                    .get("agent_key")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let store = source::CronStore::new(agent_cron_dir(agent_key));
                let jobs = store.load().await.map_err(|e| kernel::Error::Unrecoverable {
                    message: format!("failed to load cron jobs: {e}"),
                })?;
                let list: Vec<serde_json::Value> = jobs
                    .iter()
                    .map(|j| {
                        serde_json::json!({
                            "id": j.id,
                            "name": j.name,
                            "expression": j.expression,
                            "timezone": j.timezone,
                            "enabled": j.enabled,
                            "created_at": j.created_at,
                            "updated_at": j.updated_at,
                            "last_run_at": j.last_run_at,
                            "last_status": j.last_status,
                            "last_error": j.last_error,
                        })
                    })
                    .collect();
                Ok(serde_json::json!({"jobs": list, "agent_key": agent_key}))
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
    /// Join handle + shutdown notify for the workflow timeout polling task.
    /// The `Notify` is used to instantly wake the task on shutdown (instead
    /// of a `yield_now` spin-wait that could delay exit by up to one tick).
    timeout_poll_task: Mutex<Option<tokio::task::JoinHandle<()>>>,
    timeout_poll_notify: Arc<tokio::sync::Notify>,
    agenverse: Arc<Agenverse>,
    inflight_pipelines: Arc<AtomicUsize>,
    inflight_skills: Arc<AtomicUsize>,
    metrics: super::metrics::MetricsRegistry,
    capability_registry: RwLock<HashMap<String, Vec<CapabilityEntry>>>,
    /// LLM-instruction skills (SKILL.md frontmatter, Agent Skills standard).
    llm_skills: Arc<StdMutex<Vec<skill::SkillInfo>>>,
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

    /// Returns the configured UI locale (default: English).
    #[must_use]
    pub fn locale(&self) -> i18n::Locale {
        self.config.ui.locale
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
            .position(|(c, _)| plugin::plugin_manifest_name(c) == plugin_name);
        idx.map(|i| guard.remove(i))
    }

    /// Remove a pending plugin candidate by name (used after user denial).
    pub async fn remove_pending_plugin_candidate(&self, plugin_name: &str) -> bool {
        let mut guard = self.pending_plugin_approvals.lock().await;
        let before = guard.len();
        guard.retain(|(c, _)| plugin::plugin_manifest_name(c) != plugin_name);
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
                plugin_name: plugin::plugin_manifest_name(candidate).to_string(),
                version: plugin::plugin_manifest_version(candidate).to_string(),
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
                        plugin::plugin_manifest_name(candidate).to_string(),
                        plugin::plugin_manifest_version(candidate).to_string(),
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
                                plugin_version: plugin::plugin_manifest_version(&candidate).to_string(),
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

    /// Force-kill a running session: set interrupt flag (graceful) + abort the
    /// tokio task (immediate, even mid-HTTP-call) + reclaim the session's
    /// detached subprocesses + reset agent state.
    ///
    /// Returns `Ok(true)` if a task was found and aborted, `Ok(false)` if no
    /// running task existed for this session.
    pub async fn kill_session(&self, session_id: &str, operator: &str) -> AmanResult<bool> {
        // 1. Set interrupt flag (graceful — stops between ReAct turns)
        self.agent_harness.interrupt_session(session_id);

        // 2. Force-abort the tokio task (immediate — even mid-HTTP-call).
        //    NOTE: aborting the tokio task does NOT kill detached children —
        //    their `Child` handle lives inside a fire-and-forget monitor thread
        //    (see tool/src/lib.rs ExecTool detach). So we must kill them here.
        let aborted = self.agent_harness.abort_task(session_id);

        // 3. Reclaim this session's detached subprocesses. Without this, an
        //    idle-run `exec(detach:true)` script (e.g. the Luck key-gen) would
        //    outlive the killed session until the gateway shuts down.
        let killed = self.tools.kill_children_for(session_id);
        if killed > 0 {
            tracing::info!(session = %session_id, killed, "kill_session: reclaimed detached subprocesses");
        }

        // 4. Find the agent that owns this session and reset its state
        let agent_id = self.agent_registry.agent_id_for_session(session_id).await;
        if let Some(aid) = agent_id {
            reset_agent_status(&self.agent_registry, &aid).await;
            // Flip the session workflow state machine PROCESSING → IDLE so the
            // session doesn't stay stuck in PROCESSING after being kill/abort.
            // (kill/abort drop the task future directly, bypassing the harness
            // error path that would otherwise publish agent:reply_interrupted.)
            self.session_manager.handle_reply(session_id, &aid, "").await;
        }

        // 5. Audit log
        self.audit().record(
            operator,
            "chat.session.kill",
            format!("session:{session_id}"),
            if aborted { "aborted" } else { "no_task_found" },
            if killed > 0 { format!("killed_children:{killed}") } else { String::new() },
        );

        // 6. Emit event so the frontend can update in real-time
        let _ = self.bus.publish(Event::new(
            "chat:control",
            EventType::Custom("SESSION_KILLED".into()),
            serde_json::json!({ "session_id": session_id, "operator": operator, "killed_children": killed }),
        )).await;

        Ok(aborted)
    }
}

// ---------------------------------------------------------------------------
// Free functions — helpers shared between kill_session, the timeout poller, and
// the shutdown flush. Kept outside the impl so the spawned timeout-polling task
// (which does not have access to &self) can call them.
// ---------------------------------------------------------------------------

/// Resolve the agent owning a session.
///
/// Prefers the workflow instance's `data["agent_id"]` (always set at creation)
/// over the registry's `active_session_id` reverse lookup, which can return
/// None if the harness has already cleared it (e.g. on the success path).
async fn resolve_agent_for_session(
    workflow_engine: &workflow::WorkflowEngine,
    registry: &super::AgentRegistry,
    session_id: &str,
) -> Option<String> {
    if let Some(inst) = workflow_engine.get_instance(session_id)
        && let Some(id) = inst.data.get("agent_id").and_then(|v| v.as_str())
    {
        return Some(id.to_owned());
    }
    registry.agent_id_for_session(session_id).await
}

/// Reset a single agent's registry status back to idle — the canonical reset
/// used by kill_session, the PROCESSING-timeout poller, and the shutdown flush
/// so all three paths stay in sync.
async fn reset_agent_status(registry: &super::AgentRegistry, agent_id: &str) {
    let _ = registry
        .set_status(agent_id, kernel::agent::AgentStatus::Idle)
        .await;
    registry.set_system_state(agent_id, kernel::agent::AgentSystemState::Idle).await;
    registry.set_activity(agent_id, "").await;
    let _ = registry.set_active_session(agent_id, None).await;
}

impl AgentRuntime {
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
        self.agenverse.phase()
    }

    /// Expose the runtime config (desktop reads `drain_timeout_sec` to
    /// size its graceful-shutdown POST timeout).
    pub fn runtime_cfg(&self) -> &config::RuntimeConfig {
        &self.config.runtime
    }

    pub async fn status(&self) -> RuntimeStatus {
        self.agenverse.status().await
    }

    #[must_use]
    pub fn is_ready(&self) -> bool {
        self.agenverse.is_ready()
    }

    #[must_use]
    pub fn is_live(&self) -> bool {
        self.agenverse.is_live()
    }

    /// Whether a shutdown has been requested (e.g. via HTTP from the desktop
    /// app). The TUI polls this to know when to exit.
    #[must_use]
    pub fn shutdown_requested(&self) -> bool {
        self.agenverse.shutdown_requested()
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
            "slack" => {
                let (bot_token, app_token) = if secrets_mode.prefer_env() {
                    let token = std::env::var("AMAN_BOT_SLACK_TOKEN")
                        .ok()
                        .filter(|s| !s.is_empty())
                        .ok_or_else(|| Error::NotFound {
                            name: "env var AMAN_BOT_SLACK_TOKEN".into(),
                        })?;
                    let app_token = std::env::var("AMAN_BOT_SLACK_APP_TOKEN")
                        .ok()
                        .unwrap_or_default();
                    (token, app_token)
                } else {
                    use secret::{KeychainBackend, SecretBackend};
                    let backend = KeychainBackend;
                    let token_key = format!("aman.bot.slack.{instance}.token");
                    let app_token_key = format!("aman.bot.slack.{instance}.app_token");
                    let token = backend
                        .get(&token_key)?
                        .ok_or_else(|| Error::NotFound {
                            name: format!("keychain key {token_key}"),
                        })?;
                    if token.is_empty() {
                        return Err(Error::config_invalid("slack bot token is empty"));
                    }
                    let app_token = backend
                        .get(&app_token_key)
                        .ok()
                        .flatten()
                        .unwrap_or_default();
                    (token, app_token)
                };

                let source_id = if instance == "default" {
                    "chat:slack:default".to_owned()
                } else {
                    format!("chat:slack:{instance}")
                };

                self.sources.shutdown(&source_id).await.ok();
                self.sources.unregister(&source_id).await.ok();

                let sender = Arc::new(messaging_slack::sender::SlackSender::new(&bot_token));
                self.channel_registry.register(source_id.clone(), sender);

                let source = messaging_slack::source::SlackSource::new(
                    source_id.clone(),
                    &bot_token,
                    &app_token,
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
                    "hot-reloaded slack IM channel source"
                );
                Ok(())
            }
            "discord" => {
                let bot_token = if secrets_mode.prefer_env() {
                    std::env::var("AMAN_BOT_DISCORD_TOKEN")
                        .ok()
                        .filter(|s| !s.is_empty())
                        .ok_or_else(|| Error::NotFound {
                            name: "env var AMAN_BOT_DISCORD_TOKEN".into(),
                        })?
                } else {
                    use secret::{KeychainBackend, SecretBackend};
                    let backend = KeychainBackend;
                    let token_key = format!("aman.bot.discord.{instance}.token");
                    let token = backend
                        .get(&token_key)?
                        .ok_or_else(|| Error::NotFound {
                            name: format!("keychain key {token_key}"),
                        })?;
                    if token.is_empty() {
                        return Err(Error::config_invalid("discord bot token is empty"));
                    }
                    token
                };

                let source_id = if instance == "default" {
                    "chat:discord:default".to_owned()
                } else {
                    format!("chat:discord:{instance}")
                };

                self.sources.shutdown(&source_id).await.ok();
                self.sources.unregister(&source_id).await.ok();

                let sender = Arc::new(messaging_discord::sender::DiscordSender::new(&bot_token));
                self.channel_registry.register(source_id.clone(), sender);

                let source = messaging_discord::source::DiscordSource::new(
                    source_id.clone(),
                    &bot_token,
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
                    "hot-reloaded discord IM channel source"
                );
                Ok(())
            }
            "matrix" => {
                let (homeserver_url, username, password) = if secrets_mode.prefer_env() {
                    let url = std::env::var("AMAN_BOT_MATRIX_HOMESERVER_URL")
                        .ok()
                        .filter(|s| !s.is_empty())
                        .ok_or_else(|| Error::NotFound {
                            name: "env var AMAN_BOT_MATRIX_HOMESERVER_URL".into(),
                        })?;
                    let username = std::env::var("AMAN_BOT_MATRIX_USERNAME")
                        .ok()
                        .filter(|s| !s.is_empty())
                        .ok_or_else(|| Error::NotFound {
                            name: "env var AMAN_BOT_MATRIX_USERNAME".into(),
                        })?;
                    let password = std::env::var("AMAN_BOT_MATRIX_PASSWORD")
                        .ok()
                        .unwrap_or_default();
                    (url, username, password)
                } else {
                    use secret::{KeychainBackend, SecretBackend};
                    let backend = KeychainBackend;
                    let url_key = format!("aman.bot.matrix.{instance}.homeserver_url");
                    let username_key = format!("aman.bot.matrix.{instance}.username");
                    let password_key = format!("aman.bot.matrix.{instance}.password");
                    let url = backend
                        .get(&url_key)?
                        .ok_or_else(|| Error::NotFound {
                            name: format!("keychain key {url_key}"),
                        })?;
                    if url.is_empty() {
                        return Err(Error::config_invalid("matrix homeserver url is empty"));
                    }
                    let username = backend
                        .get(&username_key)?
                        .ok_or_else(|| Error::NotFound {
                            name: format!("keychain key {username_key}"),
                        })?;
                    let password = backend
                        .get(&password_key)
                        .ok()
                        .flatten()
                        .unwrap_or_default();
                    (url, username, password)
                };

                let source_id = format!("chat:matrix:{username}");

                self.sources.shutdown(&source_id).await.ok();
                self.sources.unregister(&source_id).await.ok();

                let sender = Arc::new(messaging_matrix::sender::MatrixSender::new(
                    &homeserver_url,
                    &password,
                ));
                self.channel_registry.register(source_id.clone(), sender);

                let source = messaging_matrix::source::MatrixSource::new(
                    source_id.clone(),
                    &homeserver_url,
                    &username,
                    &password,
                    "aman-agent",
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
                    homeserver = %homeserver_url,
                    "hot-reloaded matrix IM channel source"
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
    /// The notification is sent when [`Self::shutdown`] completes, regardless of
    /// whether shutdown was triggered via HTTP, signal, or any other path.
    /// Callers can `.await` on `notified()` to wait for shutdown completion.
    #[must_use]
    pub fn shutdown_notify(&self) -> &tokio::sync::Notify {
        self.agenverse.shutdown_notify()
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
        if let Err(e) = std::fs::create_dir_all(&hooks_dir) {
            tracing::warn!(path = %hooks_dir.display(), error = %e, "failed to create agent hooks directory");
        }
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
                    try_publish(&*self.global_bus, event).await;
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
        try_publish(&*self.bus, event).await;
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
        try_publish(&*self.bus, event).await;
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
        self.agenverse.try_acquire_start_gate().await?;
        self.ensure_observer_subscribed().await?;
        self.ensure_soul_watching().await?;
        self.ensure_skill_watching().await?;
        self.ensure_backpressure_watching().await?;
        self.ensure_timeout_polling().await?;

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

        self.agenverse.mark_ready().await;
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
                                try_publish(&*bus, event).await;
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
                                try_publish(&*bus, event).await;
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

    // ── Workflow timeout polling ────────────────────────────────────────
    //
    // The session workflow declares `PROCESSING` with a 120s timeout that
    // transitions to `TIMEOUT`. Because `WorkflowEngine` is "lazy" (it only
    // evaluates timeouts when `handle_timeouts()` is called), a poller is
    // required for the timeout to actually fire in production. Without this,
    // a session whose task was silently dropped (e.g. LLM provider hang with
    // no stream timeout) would stay stuck in PROCESSING forever.

    /// Spawn the background timeout-polling task (idempotent).
    async fn ensure_timeout_polling(&self) -> AmanResult<()> {
        let mut slot = self.timeout_poll_task.lock().await;
        if slot.is_some() {
            return Ok(());
        }
        let notify = Arc::clone(&self.timeout_poll_notify);
        let workflow_engine = Arc::clone(&self.workflow_engine);
        let registry = Arc::clone(&self.agent_registry);
        let join = tokio::spawn(async move {
            let mut interval =
                tokio::time::interval(std::time::Duration::from_secs(10));
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                // Wait for either the next tick or a shutdown notification.
                // `tokio::select!` races both; if `notify.notified()` fires
                // first we break immediately (no spin-wait).
                tokio::select! {
                    _ = interval.tick() => {}
                    _ = notify.notified() => break,
                }
                let now = kernel::types::Timestamp::now();
                match workflow_engine.handle_timeouts(now).await {
                    Ok(results) => {
                        for r in &results {
                            if r.transitioned
                                && r.reason == workflow::TransitionReason::Timeout
                            {
                                tracing::info!(
                                    instance_id = %r.instance_id,
                                    from_state = %r.from_state,
                                    to_state = %r.to_state,
                                    "workflow timeout fired"
                                );
                                // A timed-out session that was in PROCESSING means
                                // the agent's async task likely hung or was dropped
                                // without triggering the harness's status-reset path.
                                // Reset the owning agent's registry state so the UI
                                // does not permanently show `chatting:processing`.
                                if r.from_state == "PROCESSING" {
                                    let agent_id = resolve_agent_for_session(
                                        &workflow_engine,
                                        &registry,
                                        &r.instance_id,
                                    )
                                    .await;
                                    if let Some(aid) = agent_id {
                                        reset_agent_status(&registry, &aid).await;
                                        // Recover the session from TIMEOUT back to
                                        // IDLE so it can accept new messages.  The
                                        // timeout already moved it PROCESSING →
                                        // TIMEOUT; firing SESSION_RESET drives the
                                        // TIMEOUT → IDLE transition.  (We must NOT
                                        // call session_manager.handle_reply() here —
                                        // that fires LLM_REPLY_READY, which has no
                                        // transition from TIMEOUT and would only
                                        // emit a spurious WARN.)
                                        let reset_event = Event::new(
                                            "session:control",
                                            EventType::Custom("SESSION_RESET".to_owned()),
                                            json!({
                                                "session_id": &r.instance_id,
                                                "agent_id": &aid,
                                            }),
                                        );
                                        if let Err(e) = workflow_engine
                                            .handle_event(&r.instance_id, reset_event)
                                            .await
                                        {
                                            tracing::warn!(
                                                agent_id = %aid,
                                                session_id = %r.instance_id,
                                                error = %e,
                                                "failed to reset session from TIMEOUT to IDLE"
                                            );
                                        }
                                        tracing::info!(
                                            agent_id = %aid,
                                            session_id = %r.instance_id,
                                            "reset agent + session status after PROCESSING timeout"
                                        );
                                    } else {
                                        tracing::warn!(
                                            session_id = %r.instance_id,
                                            "PROCESSING timeout fired but no agent resolved; \
                                             registry status may be stale"
                                        );
                                    }
                                }
                            }
                        }
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "workflow handle_timeouts failed");
                    }
                }
            }
        });
        *slot = Some(join);
        Ok(())
    }

    /// Stop the background timeout-polling task. Wakes the task instantly
    /// (via `Notify`) so it does not wait for the next 10s tick to notice.
    async fn stop_timeout_polling(&self) {
        self.timeout_poll_notify.notify_one();
        if let Some(join) = self.timeout_poll_task.lock().await.take() {
            let _ = join.await;
        }
    }

    /// Flush every PROCESSING session to IDLE before the process exits.
    ///
    /// Called after `abort_all_tasks()` during Phase5 shutdown. Force-aborted
    /// tasks bypass the harness error path that would otherwise drive
    /// `handle_reply()` (PROCESSING → IDLE + SQLite upsert). Without this,
    /// their workflow instances would remain `PROCESSING` across a restart
    /// even though no agent is actually processing them.
    ///
    /// We scan the workflow engine's live instances (not the SQLite session
    /// store) because `sessions.state` is only ever written with the
    /// post-transition value (IDLE/TIMEOUT/etc.) — it never holds the literal
    /// `"PROCESSING"` string. The workflow engine is the source of truth for
    /// the current state.
    async fn flush_processing_sessions_on_shutdown(&self) {
        let instances = self.workflow_engine.list_instances();
        let mut flushed = 0;
        for inst in &instances {
            if inst.current_state == "PROCESSING" {
                // Prefer the workflow instance's agent_id (always set); fall
                // back to the registry reverse lookup.
                let agent_id = resolve_agent_for_session(
                    &self.workflow_engine,
                    &self.agent_registry,
                    &inst.id,
                )
                .await;
                if let Some(aid) = agent_id {
                    // Reset the owning agent's registry status so the UI does
                    // not permanently show `chatting:processing` after restart.
                    reset_agent_status(&self.agent_registry, &aid).await;
                    // Flip the session workflow state PROCESSING → IDLE.
                    self.session_manager.handle_reply(&inst.id, &aid, "").await;
                    flushed += 1;
                    tracing::debug!(
                        session_id = %inst.id,
                        agent_id = %aid,
                        "flushed PROCESSING → IDLE + reset agent status on shutdown"
                    );
                }
            }
        }
        if flushed > 0 {
            tracing::info!(
                flushed,
                "shutdown: flushed PROCESSING → IDLE for {} session(s)",
                flushed
            );
        }
    }

    #[instrument(skip(self))]
    pub async fn shutdown(&self) -> AmanResult<()> {
        if self.agenverse.try_acquire_shutdown_gate().await.is_err() {
            return Ok(());
        }

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
        self.stop_timeout_polling().await;

        self.agenverse.mark_shutdown().await;
        Ok(())
    }

    async fn bump_phase(&self, phase: RuntimePhase) -> AmanResult<()> {
        if self.agenverse.shutdown_requested() && phase != RuntimePhase::Phase0 {
            self.bump_shutdown_phase(self.agenverse.phase()).await?;
            self.agenverse.mark_shutdown().await;
            return Err(Error::InvalidStateTransition {
                message: "startup interrupted by shutdown".to_owned(),
            });
        }

        let startup_pause = self.agenverse.startup_pause();
        if !startup_pause.is_zero() {
            tokio::time::sleep(startup_pause).await;
        }

        tracing::info!(?phase, "bump_phase enter");
        match phase {
            RuntimePhase::Phase0 => {
                self.agenverse.set_phase(RuntimePhase::Phase0);
            }
            RuntimePhase::Phase05 => {
                self.agenverse.set_phase(RuntimePhase::Phase05);
            }
            RuntimePhase::Phase1 => {
                if let Some(persistent) = &self.persistent_bus {
                    let _ = persistent.recover_from_wal().await?;
                    let _ = persistent.recover_from_overflow()?;
                }
                self.agenverse.set_phase(RuntimePhase::Phase1);
            }
            RuntimePhase::Phase2 => {
                let _ = self.skill_hot_reload.reload_once()?;
                tracing::info!("Phase2: plugin_loader.lock");
                {
                    let _loader = self.plugin_loader.lock().await;
                }
                tracing::info!("Phase2: refresh_capabilities");
                if let Err(e) = self.refresh_capabilities().await {
                    tracing::warn!(error = %e, "Phase2: failed to refresh capabilities; capabilities may be stale");
                }
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
                self.agenverse.set_phase(RuntimePhase::Phase2);
            }
            RuntimePhase::Phase3 => {
                if let Err(e) = self.load_workflows_once() {
                    tracing::warn!(error = %e, "Phase3: failed to load workflows; no workflows available");
                }
                self.agenverse.set_phase(RuntimePhase::Phase3);
            }
            RuntimePhase::Phase4 => {
                // Start per-agent idle loops
                self.agent_registry.start_all_idle_loops().await;
                // Start emotion evaluators (require Tokio runtime)
                self.agent_registry.start_all_emotion_evaluators().await;
                // Start cognitive state monitors (propagate to idle/arousal)
                Arc::clone(&self.agent_registry)
                    .start_all_cognitive_monitors()
                    .await;

                // Start LLM health probe (periodic backend availability check)
                self.start_llm_health_probe().await;
                // Initialize MCP clients for all agents (only when enabled in config)
                if self.config.mcp.enabled {
                    self.agent_registry.init_mcp_all(self.tools()).await;
                }

                // Restore persisted cron jobs for every agent before starting
                // sources.  New sources are registered (but not yet started);
                // the existing loop below calls start() on everything.
                for agent in self.agent_registry.list().await {
                    let store = CronStore::new(agent_cron_dir(&agent.descriptor.agent_id));
                    match store.load().await {
                        Ok(jobs) => {
                            for job in &jobs {
                                if !job.enabled {
                                    continue;
                                }
                                match CronSource::new(&job.id, &job.expression) {
                                    Ok(mut cron_source) => {
                                        // Restore saved timezone.
                                        if job.timezone != "UTC" {
                                            let _ = cron_source
                                                .reconfigure(serde_json::json!({
                                                    "timezone": &job.timezone,
                                                }))
                                                .await;
                                        }
                                        if let Err(e) = self
                                            .sources
                                            .register(
                                                Box::new(cron_source),
                                                SourceMode::Pull,
                                                TrustLevel::Untrusted,
                                            )
                                            .await
                                        {
                                            tracing::warn!(
                                                agent = %agent.descriptor.agent_id,
                                                job = %job.id,
                                                error = %e,
                                                "failed to restore cron job (already registered?)"
                                            );
                                        }
                                    }
                                    Err(e) => {
                                        tracing::warn!(
                                            agent = %agent.descriptor.agent_id,
                                            job = %job.id,
                                            error = %e,
                                            "failed to restore cron job (invalid expression)"
                                        );
                                    }
                                }
                            }
                            if !jobs.is_empty() {
                                tracing::info!(
                                    agent = %agent.descriptor.agent_id,
                                    count = jobs.len(),
                                    "restored cron jobs"
                                );
                            }
                        }
                        Err(e) => {
                            tracing::warn!(
                                agent = %agent.descriptor.agent_id,
                                error = %e,
                                "failed to load cron jobs"
                            );
                        }
                    }
                }

                let snapshots = self.sources.list().await;
                for source in snapshots {
                    if self.agenverse.shutdown_requested() {
                        break;
                    }
                    tracing::info!(id = %source.id, "starting source");
                    self.sources.start(&source.id).await?;
                    tracing::info!(id = %source.id, "source started");
                }
                self.agenverse.set_phase(RuntimePhase::Phase4);
            }
            RuntimePhase::Phase5 => {
                self.agenverse.set_phase(RuntimePhase::Phase5);
            }
        }
        Ok(())
    }

    /// 启动 LLM 健康探针。
    ///
    /// 注册一个每分钟执行的 cron job，周期性检查所有 Down/Degraded 后端的
    /// 健康状态。使用 `GET /models` 轻量请求（不消耗 token）。
    async fn start_llm_health_probe(&self) {
        let registry = Arc::clone(&self.agent_registry);
        let bus = self.bus_cloned();

        // 创建探针 EventHandler
        let probe = super::llm_health_probe::LlmHealthProbe::new(registry);

        // 订阅 CronTick 事件
        let subscription_filter =
            event_bus::SubscriptionFilter::default();
        let probe_handler = Box::new(probe);
        match bus.subscribe(subscription_filter, probe_handler).await {
            Ok(_sub_id) => {
                tracing::info!("llm_health_probe: subscribed to CronTick");
            }
            Err(e) => {
                tracing::warn!(error = %e, "llm_health_probe: failed to subscribe");
                return;
            }
        }

        // 注册 cron job：每分钟执行一次
        match source::CronSource::new("llm_health_probe", "*/1 * * * *") {
            Ok(cron_source) => {
                let sr = self.sources();
                match sr
                    .register(
                        Box::new(cron_source),
                        source::SourceMode::Pull,
                        source::TrustLevel::Untrusted,
                    )
                    .await
                {
                    Ok(()) => {
                        match sr.start("llm_health_probe").await {
                            Ok(()) => {
                                tracing::info!("llm_health_probe: cron job started (every 1min)");
                            }
                            Err(e) => {
                                tracing::warn!(
                                    error = %e,
                                    "llm_health_probe: failed to start cron job"
                                );
                            }
                        }
                    }
                    Err(e) => {
                        tracing::warn!(
                            error = %e,
                            "llm_health_probe: failed to register cron source"
                        );
                    }
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, "llm_health_probe: failed to create cron source");
            }
        }
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
        let startup_pause = self.agenverse.startup_pause();
        if !startup_pause.is_zero() {
            tokio::time::sleep(startup_pause).await;
        }

        match phase {
            RuntimePhase::Phase5 => {
                // ── Interrupt all active agent sessions ──────────────────
                // Signal every in-flight ReAct loop to stop at its next
                // check-point. Most sessions will finish within a few seconds
                // once the LLM call or tool execution completes.
                self.agent_harness.interrupt_all_sessions();

                // Wait for agents to drain naturally (grace period).
                // We poll every 200 ms and stop as soon as every agent is
                // Idle, or give up after the configured drain timeout.
                let grace = Duration::from_secs(
                    self.config
                        .runtime
                        .drain_timeout_sec
                        .clamp(3, 10),
                );
                let deadline = tokio::time::Instant::now() + grace;
                let mut last_log = tokio::time::Instant::now();
                let log_interval = Duration::from_secs(1);
                loop {
                    let agents = self.agent_registry.list().await;
                    let busy: Vec<&str> = agents
                        .iter()
                        .filter(|a| a.status == kernel::agent::AgentStatus::Busy)
                        .map(|a| a.descriptor.agent_id.as_str())
                        .collect();
                    if busy.is_empty() {
                        tracing::info!(
                            total = agents.len(),
                            "all agents idle — proceeding with shutdown"
                        );
                        break;
                    }
                    // Log progress once per second so the operator can see
                    // which agents are still draining.
                    if last_log.elapsed() >= log_interval {
                        let remaining = deadline
                            .checked_duration_since(tokio::time::Instant::now())
                            .map(|d| d.as_secs_f32())
                            .unwrap_or(0.0);
                        tracing::info!(
                            busy = busy.len(),
                            total = agents.len(),
                            agents = ?busy,
                            remaining_secs = remaining,
                            "draining active agent sessions"
                        );
                        last_log = tokio::time::Instant::now();
                    }
                    if tokio::time::Instant::now() >= deadline {
                        tracing::warn!(
                            busy = busy.len(),
                            agents = ?busy,
                            "agents did not become idle within {:?} — forcing abort",
                            grace
                        );
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(200)).await;
                }

                // Force-cancel any task that didn't respond to the interrupt.
                self.agent_harness.abort_all_tasks();

                // Flush any session still stuck in PROCESSING to IDLE before
                // the process exits. Force-aborted tasks bypass the harness
                // error path that would otherwise drive handle_reply(), so
                // their SQLite records would otherwise remain PROCESSING
                // across a restart.
                self.flush_processing_sessions_on_shutdown().await;

                // Kill every tool-spawned child process that is still running.
                self.tools.kill_all_children();

                // Stop SSE background tasks — they hold Arc<AgentRuntime>
                // refs and their never-ending loops would prevent Tokio's
                // multi-threaded Runtime::drop() from ever returning.
                self.sse_broadcast.stop_background_tasks().await;
                // Brief yield so tracing subscribers flush before the next
                // phase transition (helpful when the outer shutdown timeout
                // is tight).
                tokio::time::sleep(Duration::from_millis(50)).await;
                self.agenverse.set_phase(RuntimePhase::Phase4);
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
                self.agenverse.set_phase(RuntimePhase::Phase3);
            }
            RuntimePhase::Phase3 => {
                self.agenverse.set_phase(RuntimePhase::Phase2);
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
                self.agenverse.set_phase(RuntimePhase::Phase1);
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
                self.agenverse.set_phase(RuntimePhase::Phase05);
            }
            RuntimePhase::Phase05 => {
                self.agenverse.set_phase(RuntimePhase::Phase0);
            }
            RuntimePhase::Phase0 => {
                self.agenverse.set_phase(RuntimePhase::Phase0);
            }
        }
        Ok(())
    }

    pub async fn add_cron_job(
        &self,
        id: String,
        expression: String,
        agent_key: &str,
        _caller: &str,
    ) -> AmanResult<()> {
        let job = CronSource::new(&id, &expression)?;
        self.sources
            .register(Box::new(job), SourceMode::Pull, TrustLevel::Untrusted)
            .await?;
        self.sources.start(&id).await?;

        // Persist to agent's cron directory.
        let store = CronStore::new(agent_cron_dir(agent_key));
        store
            .add(&CronJobConfig::new(id.clone(), expression))
            .await?;
        Ok(())
    }

    pub async fn update_cron_job(
        &self,
        id: &str,
        config: serde_json::Value,
        agent_key: &str,
        _caller: &str,
    ) -> AmanResult<()> {
        self.sources.reconfigure(id, config.clone()).await?;

        // Persist expression/timezone updates.
        if let (Some(expression), Some(timezone)) = (
            config.get("expression").and_then(|v| v.as_str()),
            config.get("timezone").and_then(|v| v.as_str()),
        ) {
            let store = CronStore::new(agent_cron_dir(agent_key));
            store.update(id, expression, timezone).await?;
        } else if let Some(expression) = config.get("expression").and_then(|v| v.as_str()) {
            let store = CronStore::new(agent_cron_dir(agent_key));
            store.update(id, expression, "UTC").await?;
        }
        Ok(())
    }

    pub async fn remove_cron_job(&self, id: &str, agent_key: &str, _caller: &str) -> AmanResult<()> {
        self.sources.shutdown(id).await?;
        self.sources.unregister(id).await?;

        // Remove from agent's cron directory.
        let store = CronStore::new(agent_cron_dir(agent_key));
        store.remove(id).await?;
        Ok(())
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

/// Returns the per-agent cron directory: `~/.aman/agents/{agent_key}/cron`.
fn agent_cron_dir(agent_key: &str) -> PathBuf {
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .unwrap_or_else(|_| "/tmp".to_owned());
    PathBuf::from(home)
        .join(".aman")
        .join("agents")
        .join(agent_key)
        .join("cron")
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
        if let Err(e) = std::fs::create_dir_all(&dir) {
            tracing::warn!(path = %dir.display(), error = %e, "failed to create secret cache directory");
        }
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
    use super::super::{Agenverse, RuntimePhase, RuntimeStatus};
    use config::AgentConfig;
    use std::sync::Arc;
    use std::time::Duration;
    use super::super::AuditLogger;

    #[test]
    fn resolve_secrets_replaces_placeholders() {
        let mut config = AgentConfig::default();
        config.source.watch_patterns = vec!["${test://pattern}".to_owned()];
        let audit = AuditLogger::new(100);
        let resolved = resolve_secrets_in_config(config, &std::env::temp_dir(), &audit).expect("resolve");
        assert_eq!(resolved.source.watch_patterns, vec!["resolved".to_owned()]);
    }

    #[tokio::test]
    async fn lifecycle_initial_state() {
        let lc = Agenverse::new(Duration::from_millis(0));
        assert_eq!(lc.phase(), RuntimePhase::Phase0);
        assert_eq!(lc.status().await, RuntimeStatus::New);
        assert!(!lc.is_ready());
        assert!(lc.is_live());
        assert!(!lc.shutdown_requested());
        assert_eq!(lc.startup_pause(), Duration::from_millis(0));
    }

    #[tokio::test]
    async fn lifecycle_set_phase_roundtrip() {
        let lc = Agenverse::new(Duration::from_millis(0));
        for phase in [
            RuntimePhase::Phase0,
            RuntimePhase::Phase05,
            RuntimePhase::Phase1,
            RuntimePhase::Phase2,
            RuntimePhase::Phase3,
            RuntimePhase::Phase4,
            RuntimePhase::Phase5,
        ] {
            lc.set_phase(phase);
            assert_eq!(lc.phase(), phase);
        }
    }

    #[tokio::test]
    async fn lifecycle_mark_ready() {
        let lc = Agenverse::new(Duration::from_millis(0));
        lc.set_phase(RuntimePhase::Phase5);
        lc.mark_ready().await;
        assert_eq!(lc.status().await, RuntimeStatus::Ready);
        assert!(lc.is_ready());
        assert!(lc.is_live());
    }

    #[tokio::test]
    async fn lifecycle_start_gate_from_new() {
        let lc = Agenverse::new(Duration::from_millis(0));
        assert!(lc.try_acquire_start_gate().await.is_ok());
        assert_eq!(lc.status().await, RuntimeStatus::Starting);
        assert_eq!(lc.phase(), RuntimePhase::Phase0);
        assert!(!lc.shutdown_requested());
    }

    #[tokio::test]
    async fn lifecycle_start_gate_idempotent() {
        let lc = Agenverse::new(Duration::from_millis(0));
        assert!(lc.try_acquire_start_gate().await.is_ok());
        assert!(lc.try_acquire_start_gate().await.is_ok());
        assert_eq!(lc.status().await, RuntimeStatus::Starting);

        lc.mark_ready().await;
        assert!(lc.try_acquire_start_gate().await.is_ok());
        assert_eq!(lc.status().await, RuntimeStatus::Ready);
    }

    #[tokio::test]
    async fn lifecycle_start_gate_rejected_after_shutdown_request() {
        let lc = Agenverse::new(Duration::from_millis(0));
        lc.try_acquire_shutdown_gate().await.expect("shutdown gate");
        assert!(lc.try_acquire_start_gate().await.is_err());
    }

    #[tokio::test]
    async fn lifecycle_shutdown_gate_and_mark_shutdown() {
        let lc = Agenverse::new(Duration::from_millis(0));
        assert!(lc.try_acquire_shutdown_gate().await.is_ok());
        assert!(lc.shutdown_requested());
        assert_eq!(lc.status().await, RuntimeStatus::ShuttingDown);
        assert!(lc.is_live());

        // Idempotent while shutting down.
        assert!(lc.try_acquire_shutdown_gate().await.is_ok());

        lc.mark_shutdown().await;
        assert_eq!(lc.status().await, RuntimeStatus::Shutdown);
        assert!(!lc.is_live());
        assert!(lc.try_acquire_shutdown_gate().await.is_err());
    }

    #[tokio::test]
    async fn lifecycle_shutdown_notifies_waiters() {
        let lc = Arc::new(Agenverse::new(Duration::from_millis(0)));
        let lc2 = Arc::clone(&lc);
        let waiter = tokio::spawn(async move { lc2.wait_shutdown_complete().await });

        // Give the spawned task time to register its interest.
        tokio::task::yield_now().await;
        lc.notify_shutdown_complete();
        waiter.await.expect("waiter completed");
    }
}

#[cfg(test)]
mod build_tests {
    use super::AgentRuntimeBuilder;
    use super::super::Agenverse;
    use config::AgentConfig;
    use std::sync::Arc;
    use tempfile::TempDir;

    /// Smoke test: verify that AgentRuntime::build() accepts the default
    /// configuration without panicking or returning an unrecoverable error.
    ///
    /// This is a *characterization* test — it documents what the minimum
    /// viable configuration looks like. If `build()` starts requiring new
    /// fields or external services, this test will catch it.
    #[test]
    fn test_build_with_default_config_and_temp_dir() {
        let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
        let _enter = rt.enter();
        let tmp = TempDir::new().expect("temp dir");
        let agenverse = Arc::new(Agenverse::new(std::time::Duration::from_millis(0)));
        let result = AgentRuntimeBuilder::new(AgentConfig::default())
            .with_runtime_dir(tmp.path().to_path_buf())
            .with_predefined_dir("predefined")
            .with_runtime_handle(rt.handle().clone())
            .build(Arc::clone(&agenverse));

        assert!(
            result.is_ok(),
            "AgentRuntime::build() with default config + temp dir should succeed, got: {:?}",
            result.err()
        );
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

    // ── Slack ─────────────────────────────────────────────────
    // Env var naming: AMAN_BOT_SLACK_TOKEN, AMAN_BOT_SLACK_APP_TOKEN
    let slack_instances = if secrets_mode.prefer_env() {
        let mut found = vec![];
        { let inst = &"default";
            let token_var = "AMAN_BOT_SLACK_TOKEN";
            if std::env::var(token_var).ok().filter(|s| !s.is_empty()).is_some() {
                found.push(inst.to_string());
            }
        }
        found
    } else {
        use secret::{KeychainBackend, SecretBackend};
        let backend = KeychainBackend;
        let mut found = vec![];
        { let inst = &"default";
            let token_key = format!("aman.bot.slack.{inst}.token");
            if backend.get(&token_key).ok().flatten().filter(|s| !s.is_empty()).is_some() {
                found.push(inst.to_string());
            }
        }
        found
    };

    for instance in slack_instances {
        let (bot_token, app_token) = if secrets_mode.prefer_env() {
            let token = std::env::var("AMAN_BOT_SLACK_TOKEN").ok().filter(|s| !s.is_empty()).unwrap_or_default();
            let app_token = std::env::var("AMAN_BOT_SLACK_APP_TOKEN").ok().unwrap_or_default();
            (token, app_token)
        } else {
            use secret::{KeychainBackend, SecretBackend};
            let backend = KeychainBackend;
            let token_key = format!("aman.bot.slack.{instance}.token");
            let app_token_key = format!("aman.bot.slack.{instance}.app_token");
            let token = backend.get(&token_key).ok().flatten().unwrap_or_default();
            let app_token = backend.get(&app_token_key).ok().flatten().unwrap_or_default();
            (token, app_token)
        };

        if bot_token.is_empty() {
            continue;
        }

        let source_id = if instance == "default" {
            "chat:slack:default".to_owned()
        } else {
            format!("chat:slack:{instance}")
        };

        tracing::info!(
            source_id = %source_id,
            instance = %instance,
            mode = ?secrets_mode,
            "starting slack IM channel source"
        );

        let sender = Arc::new(messaging_slack::sender::SlackSender::new(&bot_token));
        channel_registry.register(source_id.clone(), sender);

        let source = messaging_slack::source::SlackSource::new(
            source_id.clone(),
            &bot_token,
            &app_token,
        )
        .with_registries(Arc::clone(sticky_router), Arc::clone(chat_session_store));

        let _ = pollster::block_on(sources.register(
            Box::new(source),
            source::SourceMode::Push,
            source::TrustLevel::Untrusted,
        ));
    }

    // ── Discord ─────────────────────────────────────────────────
    // Env var naming: AMAN_BOT_DISCORD_TOKEN
    let discord_instances = if secrets_mode.prefer_env() {
        let mut found = vec![];
        { let inst = &"default";
            let token_var = "AMAN_BOT_DISCORD_TOKEN";
            if std::env::var(token_var).ok().filter(|s| !s.is_empty()).is_some() {
                found.push(inst.to_string());
            }
        }
        found
    } else {
        use secret::{KeychainBackend, SecretBackend};
        let backend = KeychainBackend;
        let mut found = vec![];
        { let inst = &"default";
            let token_key = format!("aman.bot.discord.{inst}.token");
            if backend.get(&token_key).ok().flatten().filter(|s| !s.is_empty()).is_some() {
                found.push(inst.to_string());
            }
        }
        found
    };

    for instance in discord_instances {
        let bot_token = if secrets_mode.prefer_env() {
            std::env::var("AMAN_BOT_DISCORD_TOKEN").ok().filter(|s| !s.is_empty()).unwrap_or_default()
        } else {
            use secret::{KeychainBackend, SecretBackend};
            let backend = KeychainBackend;
            let token_key = format!("aman.bot.discord.{instance}.token");
            backend.get(&token_key).ok().flatten().unwrap_or_default()
        };

        if bot_token.is_empty() {
            continue;
        }

        let source_id = if instance == "default" {
            "chat:discord:default".to_owned()
        } else {
            format!("chat:discord:{instance}")
        };

        tracing::info!(
            source_id = %source_id,
            instance = %instance,
            mode = ?secrets_mode,
            "starting discord IM channel source"
        );

        let sender = Arc::new(messaging_discord::sender::DiscordSender::new(&bot_token));
        channel_registry.register(source_id.clone(), sender);

        let source = messaging_discord::source::DiscordSource::new(
            source_id.clone(),
            &bot_token,
        )
        .with_registries(Arc::clone(sticky_router), Arc::clone(chat_session_store));

        let _ = pollster::block_on(sources.register(
            Box::new(source),
            source::SourceMode::Push,
            source::TrustLevel::Untrusted,
        ));
    }

    // ── Matrix ─────────────────────────────────────────────────
    // Env var naming: AMAN_BOT_MATRIX_HOMESERVER_URL, AMAN_BOT_MATRIX_USERNAME, AMAN_BOT_MATRIX_PASSWORD
    let matrix_instances = if secrets_mode.prefer_env() {
        let mut found = vec![];
        { let inst = &"default";
            let url_var = "AMAN_BOT_MATRIX_HOMESERVER_URL";
            if std::env::var(url_var).ok().filter(|s| !s.is_empty()).is_some() {
                found.push(inst.to_string());
            }
        }
        found
    } else {
        use secret::{KeychainBackend, SecretBackend};
        let backend = KeychainBackend;
        let mut found = vec![];
        { let inst = &"default";
            let url_key = format!("aman.bot.matrix.{inst}.homeserver_url");
            if backend.get(&url_key).ok().flatten().filter(|s| !s.is_empty()).is_some() {
                found.push(inst.to_string());
            }
        }
        found
    };

    for instance in matrix_instances {
        let (homeserver_url, username, password) = if secrets_mode.prefer_env() {
            let url = std::env::var("AMAN_BOT_MATRIX_HOMESERVER_URL").ok().filter(|s| !s.is_empty()).unwrap_or_default();
            let username = std::env::var("AMAN_BOT_MATRIX_USERNAME").ok().filter(|s| !s.is_empty()).unwrap_or_default();
            let password = std::env::var("AMAN_BOT_MATRIX_PASSWORD").ok().unwrap_or_default();
            (url, username, password)
        } else {
            use secret::{KeychainBackend, SecretBackend};
            let backend = KeychainBackend;
            let url_key = format!("aman.bot.matrix.{instance}.homeserver_url");
            let username_key = format!("aman.bot.matrix.{instance}.username");
            let password_key = format!("aman.bot.matrix.{instance}.password");
            let url = backend.get(&url_key).ok().flatten().unwrap_or_default();
            let username = backend.get(&username_key).ok().flatten().unwrap_or_default();
            let password = backend.get(&password_key).ok().flatten().unwrap_or_default();
            (url, username, password)
        };

        if homeserver_url.is_empty() || username.is_empty() {
            continue;
        }

        let source_id = format!("chat:matrix:{username}");

        tracing::info!(
            source_id = %source_id,
            instance = %instance,
            homeserver = %homeserver_url,
            mode = ?secrets_mode,
            "starting matrix IM channel source"
        );

        let sender = Arc::new(messaging_matrix::sender::MatrixSender::new(
            &homeserver_url,
            &password,
        ));
        channel_registry.register(source_id.clone(), sender);

        let source = messaging_matrix::source::MatrixSource::new(
            source_id.clone(),
            &homeserver_url,
            &username,
            &password,
            "aman-agent",
        )
        .with_registries(Arc::clone(sticky_router), Arc::clone(chat_session_store));

        let _ = pollster::block_on(sources.register(
            Box::new(source),
            source::SourceMode::Push,
            source::TrustLevel::Untrusted,
        ));
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

                // OpenAI-compatible /v1/embeddings endpoint.
                // Works with Ollama (since v0.1.28), oMLX, LM Studio, OpenAI, etc.
                match memory::OpenAiEmbedder::detect_dim(
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
///
/// Supported types:
/// - `"openai"` → OpenAI-compatible API (also used for DeepSeek, etc.)
/// - `"anthropic"` → Anthropic Messages API (Claude models)
/// - `"local"` → Local OpenAI-compatible endpoint (Ollama, llama.cpp, vLLM)
fn build_provider(_provider_key: &str, api_key: &str, base_url: &str, api_type: &str) -> Arc<dyn LlmProvider> {
    match api_type {
        "openai" => {
            Arc::new(llm_provider_openai::LlmOpenaiProvider::new(
                api_key.to_owned(),
                base_url.to_owned(),
            ))
        }
        "anthropic" => {
            let cognitive: Arc<dyn cognitive_llm::provider::LlmProvider> = Arc::new(
                cognitive_llm::anthropic::LlmAnthropicProvider::new(
                    base_url, api_key, "claude-sonnet-4-6",
                ),
            );
            wrap_cognitive_provider(cognitive)
        }
        "local" => {
            let cognitive: Arc<dyn cognitive_llm::provider::LlmProvider> = Arc::new(
                cognitive_llm::local::LlmLocalProvider::new(base_url),
            );
            wrap_cognitive_provider(cognitive)
        }
        other => {
            tracing::warn!(
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

/// Build a dedicated LLM provider for memory/extraction work from the
/// `memory.llm` section of the config.
///
/// Returns `None` when the configured provider key is not found in the
/// `providers:` map or the provider config cannot be resolved. Reflection
/// falls back to per-agent providers in that case.
fn build_memory_llm_provider(
    aman_cfg: Option<&config::AmanConfig>,
    mem_cfg: &config::MemoryLlmConfig,
) -> Option<Arc<dyn LlmProvider>> {
    let aman_cfg = aman_cfg?;
    let p = aman_cfg.providers.get(&mem_cfg.provider)?;
    let api_key = get_llm_api_key_or_inline(&mem_cfg.provider, Some(p));
    let api_type = aman_cfg
        .llm
        .as_ref()
        .map(|l| l.api_type.as_str())
        .or(p.api_type.as_deref())
        .unwrap_or("openai");
    tracing::info!(
        provider = %mem_cfg.provider,
        model = %mem_cfg.model,
        api_type = %api_type,
        "building dedicated memory LLM provider for reflection"
    );
    Some(build_provider(&mem_cfg.provider, &api_key, &p.base_url, api_type))
}

/// Wrap a `cognitive_llm::provider::LlmProvider` as a `kernel::llm::LlmProvider`.
fn wrap_cognitive_provider(
    inner: Arc<dyn cognitive_llm::provider::LlmProvider>,
) -> Arc<dyn LlmProvider> {
    struct Adapter(Arc<dyn cognitive_llm::provider::LlmProvider>);
    #[async_trait::async_trait]
    impl LlmProvider for Adapter {
        fn name(&self) -> &str { self.0.name() }
        fn base_url(&self) -> &str { self.0.base_url() }
        async fn chat_completion(&self, req: LlmChatRequest, cb: Option<Arc<dyn Fn(kernel::llm::StreamEvent) + Send + Sync>>) -> Result<LlmResponse, kernel::Error> {
            let cr = cognitive_llm::provider::LlmChatRequest {
                model: req.model, system_prompt: req.system_prompt,
                messages: req.messages.iter().map(|m| cognitive_react::ChatMessage {
                    role: match m.role { kernel::react::ChatMessageRole::System => cognitive_react::ChatMessageRole::System, kernel::react::ChatMessageRole::User => cognitive_react::ChatMessageRole::User, kernel::react::ChatMessageRole::Assistant => cognitive_react::ChatMessageRole::Assistant, kernel::react::ChatMessageRole::Tool => cognitive_react::ChatMessageRole::Tool },
                    content: m.content.clone(), tool_call_id: m.tool_call_id.clone(), tool_name: m.tool_name.clone(), tool_calls: m.tool_calls.clone(), reasoning_content: m.reasoning_content.clone(),
                }).collect(),
                tools: req.tools.iter().map(|t| cognitive_react::ToolDescriptor { name: t.name.clone(), description: t.description.clone(), parameters: t.parameters.clone() }).collect(),
                max_output_tokens: req.max_output_tokens,
                response_format: req.response_format.as_ref().map(|f| match f { kernel::llm::ResponseFormat::JsonObject => cognitive_llm::provider::ResponseFormat::JsonObject, kernel::llm::ResponseFormat::JsonSchema { name, schema, strict } => cognitive_llm::provider::ResponseFormat::JsonSchema { name: name.clone(), schema: schema.clone(), strict: *strict } }),
            };
            let ccb = cb.map(|c| { let c = c; Arc::new(move |e| c(match e { cognitive_llm::provider::StreamEvent::Start => kernel::llm::StreamEvent::Start, cognitive_llm::provider::StreamEvent::Chunk(s) => kernel::llm::StreamEvent::Chunk(s), cognitive_llm::provider::StreamEvent::Done { finish_reason } => kernel::llm::StreamEvent::Done { finish_reason }, cognitive_llm::provider::StreamEvent::Error(s) => kernel::llm::StreamEvent::Error(s), })) as Arc<dyn Fn(kernel::llm::StreamEvent) + Send + Sync> });
            self.0.chat_completion(cr, ccb).await.map(|r| LlmResponse { content: r.content, finish_reason: r.finish_reason, tool_calls: r.tool_calls.into_iter().map(|c| ParsedToolCall { id: c.id, tool_name: c.tool_name, args: c.args }).collect(), reasoning_content: r.reasoning_content }).map_err(|e| kernel::Error::Unrecoverable { message: e })
        }
    }
    Arc::new(Adapter(inner))
}

/// Adapter that implements `cognitive_llm::provider::LlmProvider` by
/// delegating to a legacy `kernel::llm::LlmProvider`.
///
/// The two traits share the same method shape but are distinct in Rust's
/// type system because they live in different crates. This shim is the
/// minimum-viable bridge that lets the gateway build a `CognitiveEngine`
/// out of its existing `LlmProvider` instances.
///
/// Long-term, the duplicate type definitions in `kernel/core/src/llm.rs`
/// and `kernel/core/src/react.rs` should be extracted into a leaf crate
/// (see P1 roadmap in `docs/code-review-20260614.md`).
#[allow(dead_code)] // Exposed as public API for future engine migration.
struct KernelLlmProviderAdapter {
    inner: Arc<dyn LlmProvider>,
}

#[async_trait::async_trait]
impl cognitive_llm::provider::LlmProvider for KernelLlmProviderAdapter {
    fn name(&self) -> &str {
        self.inner.name()
    }

    fn base_url(&self) -> &str {
        self.inner.base_url()
    }

    async fn chat_completion(
        &self,
        req: cognitive_llm::provider::LlmChatRequest,
        cb: Option<Arc<dyn Fn(cognitive_llm::provider::StreamEvent) + Send + Sync>>,
    ) -> Result<cognitive_llm::provider::LlmResponse, String> {
        // Convert `cognitive_llm` types to `kernel` types, delegate, then
        // convert back. Both type pairs have identical struct shapes, so the
        // field-by-field copy is purely mechanical. This works because the
        // gateway only ever constructs LlmChatRequest/ChatMessage in a few
        // well-defined code paths; for the engine wrapper path we only need
        // round-trip identity.
        let kernel_req = kernel::llm::LlmChatRequest {
            model: req.model,
            system_prompt: req.system_prompt,
            messages: req
                .messages
                .into_iter()
                .map(convert_chat_message_kernel_to_cognitive)
                .collect(),
            tools: req
                .tools
                .into_iter()
                .map(|t| kernel::react::ToolDescriptor {
                    name: t.name,
                    description: t.description,
                    parameters: t.parameters,
                })
                .collect(),
            max_output_tokens: req.max_output_tokens,
            response_format: req.response_format.map(convert_response_format),
        };
        // Stream callback adaptation: the trait uses different StreamEvent
        // types. For the wrapper path we only need the non-streaming case,
        // so we drop the callback (LlmCognitiveEngine's default usage does
        // not stream). This is a deliberate simplification — see P1 roadmap.
        let kernel_cb = cb.map(|_f| -> Arc<dyn Fn(kernel::llm::StreamEvent) + Send + Sync> {
            Arc::new(|_evt| {})
        });
        let resp = self
            .inner
            .chat_completion(kernel_req, kernel_cb)
            .await
            .map_err(|e| e.to_string())?;
        Ok(cognitive_llm::provider::LlmResponse {
            content: resp.content,
            finish_reason: resp.finish_reason,
            tool_calls: resp
                .tool_calls
                .into_iter()
                .map(|c| cognitive_llm::react::ParsedToolCall {
                    id: c.id,
                    tool_name: c.tool_name,
                    args: c.args,
                })
                .collect(),
            reasoning_content: resp.reasoning_content,
            // The old kernel::llm::LlmResponse carries no usage data;
            // the emit site falls back to the byte heuristic.
            usage: None,
        })
    }
}

#[allow(dead_code)] // Exposed as public API for future engine migration.
/// Convert cognitive_llm's ResponseFormat to kernel's ResponseFormat.
/// Both enums have identical variants; this is a mechanical field-by-field copy.
fn convert_response_format(
    fmt: cognitive_llm::provider::ResponseFormat,
) -> kernel::llm::ResponseFormat {
    match fmt {
        cognitive_llm::provider::ResponseFormat::JsonObject => kernel::llm::ResponseFormat::JsonObject,
        cognitive_llm::provider::ResponseFormat::JsonSchema { name, schema, strict } => {
            kernel::llm::ResponseFormat::JsonSchema { name, schema, strict }
        }
    }
}

fn convert_chat_message_kernel_to_cognitive(
    m: cognitive_llm::react::ChatMessage,
) -> kernel::react::ChatMessage {
    kernel::react::ChatMessage {
        role: match m.role {
            cognitive_llm::react::ChatMessageRole::System => kernel::react::ChatMessageRole::System,
            cognitive_llm::react::ChatMessageRole::User => kernel::react::ChatMessageRole::User,
            cognitive_llm::react::ChatMessageRole::Assistant => {
                kernel::react::ChatMessageRole::Assistant
            }
            cognitive_llm::react::ChatMessageRole::Tool => kernel::react::ChatMessageRole::Tool,
        },
        content: m.content,
        tool_call_id: m.tool_call_id,
        tool_name: m.tool_name,
        tool_calls: m.tool_calls,
        reasoning_content: m.reasoning_content,
    }
}

// ---------------------------------------------------------------------------
// Reverse adapter: cognitive_llm::LlmProvider → kernel::llm::LlmProvider
// ---------------------------------------------------------------------------
// The adapter above wraps a kernel provider as a cognitive one. This adapter
// does the reverse: wraps a cognitive provider (like the new Anthropic impl)
// for use by the gateway, which still consumes kernel::llm::LlmProvider.

/// Adapter that implements `kernel::llm::LlmProvider` by delegating to a
/// `cognitive_llm::provider::LlmProvider`.
///
/// Enables the Anthropic provider (and future cognitive providers) to be
/// used immediately by the gateway without waiting for the full trait
/// unification (P1 leaf-crate extraction).
///
/// # Example
///
/// ```ignore
/// let anthropic = LlmAnthropicProvider::new("https://api.anthropic.com/v1", "key", "claude-sonnet-4-6");
/// let wrapped = CognitiveLlmProviderAdapter::new(Arc::new(anthropic)).into_kernel_provider();
/// registry.set_llm_provider("my-agent", wrapped).await;
/// ```
#[allow(dead_code)] // Public API for future cognitive provider registration.
pub struct CognitiveLlmProviderAdapter {
    inner: Arc<dyn cognitive_llm::provider::LlmProvider>,
}

impl CognitiveLlmProviderAdapter {
    /// Wrap a cognitive LLM provider for use with the gateway.
    #[allow(dead_code)] // Used by tests and external consumers.
    pub fn new(inner: Arc<dyn cognitive_llm::provider::LlmProvider>) -> Self {
        Self { inner }
    }

    /// Consume the adapter and return an `Arc<dyn kernel::llm::LlmProvider>` suitable
    /// for `AgentRegistry::set_llm_provider()`.
    #[allow(dead_code)] // Used by tests and external consumers.
    pub fn into_kernel_provider(self) -> Arc<dyn kernel::llm::LlmProvider> {
        Arc::new(self)
    }
}

#[cfg(test)]
mod cognitive_adapter_tests {
    use super::*;

    /// Stub cognitive provider that returns a fixed response.
    struct StubCognitiveProvider {
        response: String,
    }

    #[async_trait::async_trait]
    impl cognitive_llm::provider::LlmProvider for StubCognitiveProvider {
        fn name(&self) -> &str {
            "stub"
        }

        fn base_url(&self) -> &str {
            "http://localhost:11434/v1"
        }

        async fn chat_completion(
            &self,
            _req: cognitive_llm::provider::LlmChatRequest,
            _cb: Option<Arc<dyn Fn(cognitive_llm::provider::StreamEvent) + Send + Sync>>,
        ) -> Result<cognitive_llm::provider::LlmResponse, String> {
            Ok(cognitive_llm::provider::LlmResponse {
                content: self.response.clone(),
                finish_reason: "stop".to_owned(),
                tool_calls: vec![],
                reasoning_content: String::new(),
                usage: None,
            })
        }
    }

    #[tokio::test]
    async fn adapter_forwards_chat_completion() {
        let stub = Arc::new(StubCognitiveProvider {
            response: "Hello from cognitive provider".to_owned(),
        });
        let adapter = CognitiveLlmProviderAdapter::new(stub).into_kernel_provider();

        let req = kernel::llm::LlmChatRequest {
            model: "test".into(),
            system_prompt: String::new(),
            messages: vec![],
            tools: vec![],
            max_output_tokens: 100,
            response_format: None,
        };

        let resp = adapter.chat_completion(req, None).await.expect("should succeed");
        assert_eq!(resp.content, "Hello from cognitive provider");
        assert_eq!(resp.finish_reason, "stop");
        assert!(resp.tool_calls.is_empty());
    }

    #[tokio::test]
    async fn adapter_name_delegates() {
        let stub = Arc::new(StubCognitiveProvider {
            response: String::new(),
        });
        let adapter = CognitiveLlmProviderAdapter::new(stub);
        assert_eq!(adapter.name(), "stub");
    }
}

#[allow(dead_code)] // Used by CognitiveLlmProviderAdapter (via test).
fn convert_chat_message_to_cognitive(m: &kernel::react::ChatMessage) -> cognitive_llm::react::ChatMessage {
    cognitive_llm::react::ChatMessage {
        role: match m.role {
            kernel::react::ChatMessageRole::System => cognitive_llm::react::ChatMessageRole::System,
            kernel::react::ChatMessageRole::User => cognitive_llm::react::ChatMessageRole::User,
            kernel::react::ChatMessageRole::Assistant => cognitive_llm::react::ChatMessageRole::Assistant,
            kernel::react::ChatMessageRole::Tool => cognitive_llm::react::ChatMessageRole::Tool,
        },
        content: m.content.clone(),
        tool_call_id: m.tool_call_id.clone(),
        tool_name: m.tool_name.clone(),
        tool_calls: m.tool_calls.clone(),
        reasoning_content: m.reasoning_content.clone(),
    }
}

#[allow(dead_code)] // Used by CognitiveLlmProviderAdapter (via test).
fn convert_response_format_to_cognitive(
    fmt: &kernel::llm::ResponseFormat,
) -> cognitive_llm::provider::ResponseFormat {
    match fmt {
        kernel::llm::ResponseFormat::JsonObject => cognitive_llm::provider::ResponseFormat::JsonObject,
        kernel::llm::ResponseFormat::JsonSchema { name, schema, strict } => {
            cognitive_llm::provider::ResponseFormat::JsonSchema {
                name: name.clone(),
                schema: schema.clone(),
                strict: *strict,
            }
        }
    }
}

#[allow(dead_code)] // Used by CognitiveLlmProviderAdapter (via test).
fn convert_stream_event_from_cognitive(evt: cognitive_llm::provider::StreamEvent) -> kernel::llm::StreamEvent {
    match evt {
        cognitive_llm::provider::StreamEvent::Start => kernel::llm::StreamEvent::Start,
        cognitive_llm::provider::StreamEvent::Chunk(s) => kernel::llm::StreamEvent::Chunk(s),
        cognitive_llm::provider::StreamEvent::Done { finish_reason } => {
            kernel::llm::StreamEvent::Done { finish_reason }
        }
        cognitive_llm::provider::StreamEvent::Error(s) => kernel::llm::StreamEvent::Error(s),
    }
}

#[async_trait::async_trait]
impl kernel::llm::LlmProvider for CognitiveLlmProviderAdapter {
    fn name(&self) -> &str {
        self.inner.name()
    }

    fn base_url(&self) -> &str {
        self.inner.base_url()
    }

    async fn chat_completion(
        &self,
        req: kernel::llm::LlmChatRequest,
        cb: Option<Arc<dyn Fn(kernel::llm::StreamEvent) + Send + Sync>>,
    ) -> Result<kernel::llm::LlmResponse, kernel::Error> {
        let cognitive_req = cognitive_llm::provider::LlmChatRequest {
            model: req.model,
            system_prompt: req.system_prompt,
            messages: req
                .messages
                .iter()
                .map(convert_chat_message_to_cognitive)
                .collect(),
            tools: req
                .tools
                .into_iter()
                .map(|t| cognitive_llm::react::ToolDescriptor {
                    name: t.name,
                    description: t.description,
                    parameters: t.parameters,
                })
                .collect(),
            max_output_tokens: req.max_output_tokens,
            response_format: req.response_format.as_ref().map(convert_response_format_to_cognitive),
        };

        let cognitive_cb = cb.map(|f| {
            let cb: Arc<dyn Fn(kernel::llm::StreamEvent) + Send + Sync> = f;
            Arc::new(move |evt: cognitive_llm::provider::StreamEvent| {
                cb(convert_stream_event_from_cognitive(evt))
            }) as Arc<dyn Fn(cognitive_llm::provider::StreamEvent) + Send + Sync>
        });

        let resp = self
            .inner
            .chat_completion(cognitive_req, cognitive_cb)
            .await
            .map_err(|e| kernel::Error::Unrecoverable {
                message: format!("cognitive LLM provider error: {e}"),
            })?;

        Ok(kernel::llm::LlmResponse {
            content: resp.content,
            finish_reason: resp.finish_reason,
            tool_calls: resp
                .tool_calls
                .into_iter()
                .map(|c| kernel::react::ParsedToolCall {
                    id: c.id,
                    tool_name: c.tool_name,
                    args: c.args,
                })
                .collect(),
            reasoning_content: resp.reasoning_content,
        })
    }
}

/// Register the `delegate_task` tool and return its `Arc` so the caller
/// can wire [`GatewaySubAgentSpawner`] after the agent harness is created.
fn install_delegate_task_tool(
    tools: &tool::ToolRegistry,
) -> Arc<cognitive_llm::delegate_task::DelegateTaskTool> {
    let tool = Arc::new(cognitive_llm::delegate_task::DelegateTaskTool::new());
    if let Err(e) = tools.register(Arc::clone(&tool) as Arc<dyn kernel::tool::Tool>) {
        tracing::warn!(error = %e, "failed to register delegate_task tool");
    }
    tool
}

/// Wrap a provider into an LLM-based `CognitiveEngine`.
///
/// This is the bridge that lets new code target the engine-agnostic
/// `CognitiveEngine` trait instead of the concrete `LlmProvider` API.
/// The wrapper keeps the legacy `LlmProvider` flow intact (the gateway's
/// runtime continues to call `LlmProvider::chat_completion` for backwards
/// compatibility) and exposes the engine trait as a parallel API surface
/// for future migration paths.
///
/// See `docs/code-review-20260614.md` P0-3 for context: the trait
/// abstraction is now reachable from the gateway even though the deeper
/// type unification is blocked by a workspace dependency cycle.
#[allow(dead_code)] // Exposed as public API for future engine migration.
#[must_use]
pub fn build_cognitive_engine(
    provider: Arc<dyn LlmProvider>,
    model: impl Into<String>,
) -> Arc<dyn cognitive_engine::CognitiveEngine> {
    Arc::new(cognitive_llm::LlmCognitiveEngine::new(
        Arc::new(KernelLlmProviderAdapter { inner: provider }),
        cognitive_llm::LlmEngineConfig {
            model: model.into(),
            ..Default::default()
        },
    ))
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

// ---------------------------------------------------------------------------
// Integration tests — agent message pipeline
// ---------------------------------------------------------------------------

#[cfg(test)]
mod agent_message_tests {
    use super::*;
    use event_bus::{EventBus, InMemoryBus, InMemoryBusConfig};
    use kernel::tool::Tool;

    /// Test helper: an EventHandler that records events into a Vec.
    /// Uses the SharedHandler pattern from event_bus tests so the
    /// inner Arc can be retained for assertions after subscribe.
    struct CapturingHandler {
        events: std::sync::Mutex<Vec<kernel::event::Event>>,
    }

    #[async_trait::async_trait]
    impl event_bus::EventHandler for CapturingHandler {
        async fn handle(&self, event: kernel::event::Event) -> kernel::AmanResult<()> {
            self.events.lock().unwrap().push(event);
            Ok(())
        }
    }

    /// Wraps an Arc<CapturingHandler> so we can pass it to subscribe
    /// while keeping a reference for assertions.
    struct SharedCapturingHandler(Arc<CapturingHandler>);

    #[async_trait::async_trait]
    impl event_bus::EventHandler for SharedCapturingHandler {
        async fn handle(&self, event: kernel::event::Event) -> kernel::AmanResult<()> {
            self.0.handle(event).await
        }
    }

    /// Verify the full agent message pipeline:
    /// AgentSendMessageTool::execute() → EventType::AgentMessage on bus →
    /// subscriber receives the event with correct fields.
    #[tokio::test]
    async fn agent_send_message_tool_publishes_event() {
        let bus: Arc<dyn EventBus> = Arc::new(InMemoryBus::new(InMemoryBusConfig::default()));

        let tool = AgentSendMessageTool {
            bus: OnceLock::new(),
        };
        tool.set_bus(Arc::clone(&bus));

        let inner = Arc::new(CapturingHandler {
            events: std::sync::Mutex::new(Vec::new()),
        });
        let handler = SharedCapturingHandler(Arc::clone(&inner));
        let sub = bus
            .subscribe(
                event_bus::SubscriptionFilter {
                    event_types: Some(vec![EventType::AgentMessage]),
                    sources: None,
                    priorities: None,
                    payload_match: None,
                },
                Box::new(handler),
            )
            .await
            .expect("subscribe");

        // Execute the tool
        let result = tool
            .execute(
                serde_json::json!({
                    "from_agent": "coder",
                    "to_agent": "reviewer",
                    "content_type": "task_delegation",
                    "text": "Please review PR #42"
                }),
                ToolContext::default(),
            )
            .await
            .expect("tool execute");

        assert_eq!(result["ok"], true);
        let message_id = result["message_id"].as_str().unwrap();
        assert!(!message_id.is_empty());
        assert_eq!(result["to_agent"], "reviewer");

        // Verify the event arrived on the bus
        let events = inner.events.lock().unwrap().clone();
        assert_eq!(events.len(), 1, "expected 1 event, got {}", events.len());
        let event = &events[0];
        assert_eq!(event.event_type, EventType::AgentMessage);

        // Parse the AgentMessage payload
        let msg: kernel::agent::AgentMessage =
            serde_json::from_value(event.payload.clone()).expect("deserialize AgentMessage");
        assert_eq!(msg.from_agent, "coder");
        assert_eq!(msg.to_agent, "reviewer");
        assert_eq!(msg.content_type, kernel::agent::AgentMessageType::TaskDelegation);
        assert_eq!(msg.payload["text"], "Please review PR #42");
        assert_eq!(msg.message_id.to_string(), message_id);

        // Cleanup
        let _ = bus.unsubscribe(sub).await;
    }

    #[tokio::test]
    async fn agent_send_message_supports_all_content_types() {
        let bus: Arc<dyn EventBus> = Arc::new(InMemoryBus::new(InMemoryBusConfig::default()));
        let tool = AgentSendMessageTool {
            bus: OnceLock::new(),
        };
        tool.set_bus(Arc::clone(&bus));

        for ct_str in ["task_delegation", "result_sharing", "status_query"] {
            let result = tool
                .execute(
                    serde_json::json!({
                        "from_agent": "a",
                        "to_agent": "b",
                        "content_type": ct_str,
                        "text": "hello"
                    }),
                    ToolContext::default(),
                )
                .await
                .unwrap_or_else(|e| panic!("execute {ct_str}: {e}"));
            assert_eq!(result["ok"], true, "failed for {ct_str}");
        }
    }

    #[tokio::test]
    async fn agent_send_message_supports_reply_to() {
        let bus: Arc<dyn EventBus> = Arc::new(InMemoryBus::new(InMemoryBusConfig::default()));
        let tool = AgentSendMessageTool {
            bus: OnceLock::new(),
        };
        tool.set_bus(Arc::clone(&bus));

        let reply_id = uuid::Uuid::new_v4();

        let inner = Arc::new(CapturingHandler {
            events: std::sync::Mutex::new(Vec::new()),
        });
        let handler = SharedCapturingHandler(Arc::clone(&inner));
        let sub = bus
            .subscribe(
                event_bus::SubscriptionFilter {
                    event_types: Some(vec![EventType::AgentMessage]),
                    sources: None,
                    priorities: None,
                    payload_match: None,
                },
                Box::new(handler),
            )
            .await
            .expect("subscribe");

        let result = tool
            .execute(
                serde_json::json!({
                    "from_agent": "coder",
                    "to_agent": "reviewer",
                    "content_type": "result_sharing",
                    "text": "Done with #42",
                    "reply_to": reply_id.to_string()
                }),
                ToolContext::default(),
            )
            .await
            .expect("execute");

        assert_eq!(result["ok"], true);

        let events = inner.events.lock().unwrap().clone();
        assert_eq!(events.len(), 1);
        let msg: kernel::agent::AgentMessage = serde_json::from_value(events[0].payload.clone()).unwrap();
        assert_eq!(msg.reply_to, Some(reply_id));

        let _ = bus.unsubscribe(sub).await;
    }
}


