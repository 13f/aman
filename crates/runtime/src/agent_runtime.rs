use config::{AgentConfig, BusMode};
use event_bus::{DiscardHook, EventBus, InMemoryBus, InMemoryBusConfig};
use kernel::types::BackpressureLevel;
use kernel::event::{Event, EventType};
use kernel::skill::Skill;
use kernel::source::EventSource;
use kernel::tool::Tool;
use kernel::{AmanResult, Error};
use persistence::{DeadLetterQueue, InMemoryDeadLetterQueue, PersistentBus, WalSync, WriteAheadLog};
use plugin::{PluginExportRegistrar, PluginInstaller, PluginLoader};
use serde_json::json;
use secret::{
    AwsSecretsManagerCliBackend, EnvSecretBackend, OnePasswordCliBackend, SecretCacheFallbackConfig,
    SecretResolver, SecretResolverConfig, VaultCliBackend,
};
use source::{CronManager, CronSource, SourceRegistry};
use std::collections::BTreeSet;
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
use workflow::WorkflowEngine;

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

#[derive(Clone)]
pub struct AgentRuntimeBuilder {
    config: AgentConfig,
    runtime_dir: PathBuf,
    bind_addr: SocketAddr,
    api_token: Option<String>,
    startup_pause: Duration,
    soul_file: Option<PathBuf>,
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
        let plugin_loader = PluginLoader::new(Arc::new(RuntimePluginRegistrar::new(
            Arc::clone(&skills),
            Arc::clone(&tools),
        )));
        let cron_manager = CronManager::with_runtime_dir(self.runtime_dir.clone());
        let plugin_installer = Arc::new(PluginInstaller::new(self.runtime_dir.join("plugins")));
        let event_store = Arc::new(EventStore::new(2_000, 500));

        let (soul_runtime, soul_manager) = if let Some(soul_file) = self.soul_file {
            let runtime = SoulRuntime::new(soul::Soul::from_file(&soul_file)?);
            let mut manager = runtime.build_hot_reload_manager(soul_file)?;
            manager.start_watching()?;
            (Some(runtime), Some(manager))
        } else {
            (None, None)
        };

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

    #[must_use]
    pub fn metrics(&self) -> &crate::metrics::MetricsRegistry {
        &self.metrics
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
                let _loader = self.plugin_loader.lock().await;
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
                    let _ = self.sources.start(&source.id).await?;
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
    runtime_dir: &PathBuf,
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
        .with_backend(Box::new(EnvSecretBackend::default()));
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
use secret::SecretBackend;

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
