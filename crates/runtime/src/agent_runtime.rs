use chat_source::ChatPlatformSource;
use config::{AgentConfig, BusMode};
use event_bus::{DiscardHook, EventBus, InMemoryBus, InMemoryBusConfig};
use kernel::types::BackpressureLevel;
use kernel::event::{Event, EventType};
use kernel::skill::Skill;
use kernel::source::EventSource;
use kernel::tool::Tool;
use kernel::{AmanResult, Error};
use persistence::{DeadLetterQueue, InMemoryDeadLetterQueue, PersistentBus, WalSync, WriteAheadLog};
use kernel::plugin::Plugin;
use plugin::{
    PluginCandidate, PluginExports, PluginIsolationMode, PluginLifecycleConfig,
    PluginExportRegistrar, PluginInstaller, PluginLoader, PluginManifest,
};
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
use std::sync::Mutex as StdMutex;
use std::time::Duration;
use tokio::sync::{Mutex, RwLock};
use crate::{AuditLogger, EventStore};
use crate::SoulRuntime;
use soul::SoulHotReloadManager;
use tracing::instrument;
use workflow::{
    ErrorRecovery, StateDef, StateTimeout, Transition, TransitionFrom, TransitionTo, WorkflowDef,
    WorkflowEngine,
};

// ---------------------------------------------------------------------------
// Capability registry types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CapabilityStatus {
    Healthy,
    Degraded,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq)]
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
        crate::init_tracing();
        std::fs::create_dir_all(&self.runtime_dir)?;

        let dlq = Arc::new(InMemoryDeadLetterQueue::new(5));
        let audit = Arc::new(AuditLogger::new(2_000));

        let config = resolve_secrets_in_config(self.config, &self.runtime_dir, &audit)?;

        let inflight_pipelines = Arc::new(AtomicUsize::new(0));
        let inflight_skills = Arc::new(AtomicUsize::new(0));
        let metrics = crate::metrics::MetricsRegistry::new();
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
        let skills_dir = self.runtime_dir.join("skills");
        let _ = std::fs::create_dir_all(&skills_dir);
        let skills = Arc::new(skill::SkillRegistry::new());
        let tools = Arc::new(tool::ToolRegistry::new());
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

        // Build LLM config for the built-in LLM plugin.
        let llm_config = build_llm_config();
        let llm_plugin = llm_plugin::LlmPlugin::new(Arc::clone(&bus), llm_config);

        // Load the built-in LLM plugin via PluginLoader.
        let llm_candidate = PluginCandidate {
            manifest: PluginManifest {
                name: "llm-plugin".to_owned(),
                version: llm_plugin.version().clone(),
                depends_on: vec![],
                lifecycle: PluginLifecycleConfig { auto_start: true },
                exports: PluginExports {
                    skills: vec![],
                    tools: vec![],
                    event_sources: vec![],
                },
                config_schema: None,
                isolation: Some(PluginIsolationMode::InProcess),
                subprocess: None,
                wasm_path: None,
                capabilities: vec!["chat".to_owned()],
                ui: None,
            },
            plugin: Box::new(llm_plugin),
            isolation: PluginIsolationMode::InProcess,
            subprocess: None,
            wasm_module_bytes: None,
        };

        // Load the built-in LLM plugin (and any extra plugins from builder).
        let mut all_candidates = vec![llm_candidate];
        all_candidates.extend(self.extra_plugins);
        let mut plugin_loader = PluginLoader::new(Arc::new(RuntimePluginRegistrar::new(
            Arc::clone(&skills),
            Arc::clone(&tools),
        )));
        if let Err(e) = pollster::block_on(plugin_loader.load_all(all_candidates)) {
            tracing::error!(error = %e, "failed to load built-in LLM plugin");
        }

        // Subscribe a handler that dispatches every event to matching skills.
        use kernel::context::SkillContext;
        use kernel::context::BaseContext;
        struct SkillEventDispatcher {
            executor: skill::SkillExecutor,
        }
        #[async_trait::async_trait]
        impl event_bus::EventHandler for SkillEventDispatcher {
            async fn handle(&self, event: kernel::event::Event) -> kernel::AmanResult<()> {
                let ctx = SkillContext {
                    base: BaseContext::new(event.metadata.trace_id),
                    skill_name: None,
                    soul_name: None,
                };
                self.executor.execute_matching(event, ctx).await;
                Ok(())
            }
        }
        let _ = pollster::block_on(bus.subscribe(
            event_bus::SubscriptionFilter::default(),
            Box::new(SkillEventDispatcher {
                executor: skill::SkillExecutor::new(Arc::clone(&skills)),
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
        }))
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
    metrics: crate::metrics::MetricsRegistry,
    capability_registry: RwLock<HashMap<String, Vec<CapabilityEntry>>>,
    chat_sender: Option<tokio::sync::mpsc::UnboundedSender<Event>>,
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
    pub fn runtime_dir(&self) -> &Path {
        &self.runtime_dir
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
        self.runtime_dir.join("skills")
    }

    pub fn reload_skills_now(&self) -> AmanResult<skill::HotReloadReport> {
        self.skill_hot_reload.reload_once()
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
    pub fn metrics(&self) -> &crate::metrics::MetricsRegistry {
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
        let loader = self.plugin_loader.lock().await;
        let raw_caps = loader.collect_capabilities();
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
        drop(loader);
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

        self.bump_phase(RuntimePhase::Phase0).await?;
        self.bump_phase(RuntimePhase::Phase05).await?;
        self.bump_phase(RuntimePhase::Phase1).await?;
        self.bump_phase(RuntimePhase::Phase2).await?;
        self.bump_phase(RuntimePhase::Phase3).await?;
        self.bump_phase(RuntimePhase::Phase4).await?;
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
                {
                    let _loader = self.plugin_loader.lock().await;
                }
                let _ = self.refresh_capabilities().await;
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
                    self.sources.start(&source.id).await?;
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
                // Phase 4.5: drain LLM plugin sessions before Event Bus drain
                let drained = self.drain_llm_plugin("phase4_shutdown").await;
                if drained > 0 {
                    tracing::info!(drained, "phase4.5: drained LLM plugin sessions");
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
        // Phase 4.5 drain: drain LLM plugin sessions before disabling the plugin
        self.drain_llm_plugin("plugin.disable").await;
        self.plugin_loader.lock().await.disable_plugin(plugin_name)
    }

    pub async fn uninstall_plugin(&self, plugin_name: &str) -> AmanResult<()> {
        // Phase 4.5 drain: drain LLM plugin sessions before uninstalling the plugin
        self.drain_llm_plugin("plugin.uninstall").await;
        let mut loader = self.plugin_loader.lock().await;
        self.plugin_installer
            .uninstall(Some(&mut loader), plugin_name)
            .await
    }

    /// Phase 4.5 drain: mark LLM capability as Degraded.
    ///
    /// The LLM plugin no longer uses the Skill system — its sessions are
    /// drained in Phase 2 via `PluginLoader::unload_all` →
    /// `LlmPlugin::on_unload` → `drain_sessions`. This function ensures the
    /// "chat" capability is marked Degraded so the frontend refuses new
    /// requests before the shutdown drains the event bus.
    ///
    /// Called during plugin disable/uninstall and Phase 4→3 shutdown.
    pub async fn drain_llm_plugin(&self, action: &str) -> usize {
        // Mark capability as Degraded (refuse new requests)
        {
            let mut registry = self.capability_registry.write().await;
            for entries in registry.values_mut() {
                for entry in entries.iter_mut() {
                    if entry.plugin == "llm-plugin" || entry.capability == "chat" {
                        entry.status = CapabilityStatus::Degraded;
                    }
                }
            }
        }

        tracing::info!(action, "llm capability marked degraded");
        0
    }
}

struct RuntimePluginRegistrar {
    skills: Arc<skill::SkillRegistry>,
    tools: Arc<tool::ToolRegistry>,
    source_ids: StdMutex<BTreeSet<String>>,
}

impl RuntimePluginRegistrar {
    fn new(skills: Arc<skill::SkillRegistry>, tools: Arc<tool::ToolRegistry>) -> Self {
        Self {
            skills,
            tools,
            source_ids: StdMutex::new(BTreeSet::new()),
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
}

struct StoreAllEventsHandler {
    store: Arc<EventStore>,
}

#[async_trait::async_trait]
impl event_bus::EventHandler for StoreAllEventsHandler {
    async fn handle(&self, event: kernel::event::Event) -> AmanResult<()> {
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
    use crate::AuditLogger;

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
}

fn source_context_for_cron() -> kernel::context::SourceContext {
    kernel::context::SourceContext {
        base: kernel::context::BaseContext::new(kernel::types::TraceId::new()),
        source_name: Some("cron_manager".to_owned()),
    }
}

/// Build LlmConfig from AmanConfig (providers, model / agents) + Keychain API key.
///
/// Reads the default `aman.model` first. If not set, falls back to the first
/// configured agent (provider + model) so users who only configure agents
/// (without a top-level `model:` in config.yaml) still get a working LLM config.
/// If no agent is found either, falls back to environment variables.
fn build_llm_config() -> llm_plugin::LlmConfig {
    let mut sessions_dir = None;
    if let Ok(aman) = config::AmanConfig::from_default_path() {
        // Compute sessions dir from first configured agent.
        if let Some(first_key) = aman.agents.keys().next() {
            let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
            sessions_dir = Some(
                PathBuf::from(&home)
                    .join(".aman")
                    .join("agents")
                    .join(first_key)
                    .join("sessions")
                    .to_string_lossy()
                    .to_string(),
            );
        }

        // Priority 1: default model config
        if let Some(model) = &aman.model {
            let provider_key = &model.provider;
            let base_url = aman
                .providers
                .get(provider_key)
                .map(|p| p.base_url.clone())
                .unwrap_or_else(|| model.base_url.clone());
            let api_key = get_llm_api_key_or_inline(provider_key, aman.providers.get(provider_key));
            let key_len = api_key.len();
            tracing::info!(
                provider = %provider_key,
                model = %model.default,
                api_key_len = key_len,
                "build_llm_config: using default model config"
            );
            return llm_plugin::LlmConfig {
                provider_key: provider_key.clone(),
                api_key,
                base_url,
                model: model.default.clone(),
                sessions_dir,
            };
        }

        // Priority 2: first configured agent (provider + model)
        for (_key, agent) in &aman.agents {
            if let Some(provider_config) = aman.providers.get(&agent.provider) {
                let api_key = get_llm_api_key_or_inline(&agent.provider, Some(provider_config));
                let key_len = api_key.len();
                tracing::info!(
                    agent_key = %_key,
                    provider = %agent.provider,
                    model = %agent.model,
                    api_key_len = key_len,
                    "build_llm_config: using agent config"
                );
                return llm_plugin::LlmConfig {
                    provider_key: agent.provider.clone(),
                    api_key,
                    base_url: provider_config.base_url.clone(),
                    model: agent.model.clone(),
                    sessions_dir,
                };
            }
            tracing::warn!(
                agent_key = %_key,
                provider = %agent.provider,
                "build_llm_config: agent provider not found in config"
            );
        }
    }
    // Priority 3: environment variables
    let api_key = std::env::var("AMAN_DEFAULT_API_KEY").unwrap_or_default();
    tracing::info!(
        api_key_len = api_key.len(),
        "build_llm_config: using env var fallback"
    );
    llm_plugin::LlmConfig {
        provider_key: "default".to_owned(),
        api_key,
        base_url: std::env::var("AMAN_DEFAULT_BASE_URL")
            .unwrap_or_else(|_| "https://api.openai.com/v1".to_owned()),
        model: std::env::var("AMAN_DEFAULT_MODEL").unwrap_or_else(|_| "gpt-4o".to_owned()),
        sessions_dir,
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
