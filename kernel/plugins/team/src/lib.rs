// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! Team plugin — multi-agent kanban scheduler.
//!
//! Architecture ref: docs/team-architect.md

#![forbid(unsafe_code)]

pub mod api;
pub mod config;
pub mod context_loader;
pub mod safety_gate;
pub mod scheduler;
pub mod store;
pub mod workflow_compiler;

use api::{team_api_routes, TeamApiState};
use async_trait::async_trait;
use config::TeamConfig;
use context_loader::ContextLoader;
use kernel::context::PluginContext;
use kernel::error::AmanResult;
use kernel::hook::Hook;
use kernel::memory::MemoryProvider;
use kernel::plugin::{Plugin, PluginDependency};
use kernel::skill::Skill;
use kernel::source::EventSource;
use kernel::tool::Tool;
use safety_gate::SafetyGateHandler;
use scheduler::{AgentDispatchInfo, TeamScheduler};
use semver::Version;
use std::path::PathBuf;
use std::sync::Arc;
use store::TeamStore;
use tracing::info;

/// The team plugin version.
const TEAM_VERSION: &str = "0.1.0";

/// Trait for accessing agent registry information.
///
/// Implemented by the gateway's AgentRuntime so the team plugin
/// can query agent capabilities and queue lengths without a
/// circular dependency on the gateway crate.
#[async_trait]
pub trait AgentRegistryAccess: Send + Sync {
    /// List summaries of all registered agents for the UI.
    fn list_agent_summaries(&self) -> Vec<api::AgentStatusResponse>;

    /// Get dispatch info for all eligible agents.
    async fn get_dispatch_infos(&self) -> Vec<AgentDispatchInfo<'_>>;
}

// ---------------------------------------------------------------------------
// TeamPlugin
// ---------------------------------------------------------------------------

/// The team plugin — a kanban scheduler that dispatches work items to agents.
pub struct TeamPlugin {
    name: String,
    version: Version,
    dependencies: Vec<PluginDependency>,
    /// Path to the team.yaml config file.
    config_path: PathBuf,
    /// Team configuration (loaded from team.yaml).
    config: Option<TeamConfig>,
    /// SQLite store for safety_log + context.
    store: Option<Arc<TeamStore>>,
    /// Work item dispatch scheduler.
    scheduler: Option<Arc<TeamScheduler>>,
    /// Safety gate handler.
    safety_gate: Option<Arc<SafetyGateHandler>>,
    /// Context file loader + cache.
    context_loader: Option<Arc<ContextLoader>>,
    /// Agent registry accessor (set by gateway at load time).
    agent_registry: Option<Arc<dyn AgentRegistryAccess>>,
}

impl TeamPlugin {
    /// Create a new team plugin.
    ///
    /// `config_path` should point to a `team.yaml` file, e.g.
    /// `~/.aman/teams/my-team/team.yaml`.
    #[must_use]
    pub fn new(config_path: PathBuf) -> Self {
        Self {
            name: "team".into(),
            version: Version::parse(TEAM_VERSION).unwrap(),
            dependencies: Vec::new(),
            config_path,
            config: None,
            store: None,
            scheduler: None,
            safety_gate: None,
            context_loader: None,
            agent_registry: None,
        }
    }

    /// Set the agent registry accessor (called by gateway during load).
    pub fn set_agent_registry(&mut self, registry: Arc<dyn AgentRegistryAccess>) {
        self.agent_registry = Some(registry);
    }

    /// Build the TeamApiState for route handlers.
    fn api_state(&self) -> Option<TeamApiState> {
        Some(TeamApiState {
            team_id: self.config.as_ref()?.team.name.clone(),
            store: Arc::clone(self.store.as_ref()?),
            scheduler: Arc::clone(self.scheduler.as_ref()?),
            agent_registry: self.agent_registry.clone(),
        })
    }
}

#[async_trait]
impl Plugin for TeamPlugin {
    fn name(&self) -> &str {
        &self.name
    }

    fn version(&self) -> &Version {
        &self.version
    }

    fn dependencies(&self) -> &[PluginDependency] {
        &self.dependencies
    }

    async fn on_load(&mut self, _ctx: PluginContext) -> AmanResult<()> {
        info!(
            path = %self.config_path.display(),
            "TeamPlugin: loading team config"
        );

        let config = TeamConfig::from_file(&self.config_path)
            .map_err(|e| kernel::Error::ConfigInvalid { message: format!("team config: {e}") })?;

        // Determine work directory
        let work_dir = config
            .work_dir
            .clone()
            .unwrap_or_else(|| self.config_path.parent().unwrap().to_path_buf());

        // Open (or create) the team database
        let db_path = work_dir.join("team.db");
        let store = Arc::new(
            TeamStore::open(&db_path)
                .map_err(|e| kernel::Error::ConfigInvalid { message: format!("team store: {e}") })?,
        );

        // Create the scheduler
        let scheduler = Arc::new(TeamScheduler::new(config.clone()));

        // Create the safety gate handler
        let safety_gate = Arc::new(
            SafetyGateHandler::new(config.safety_gates.clone(), TeamStore::clone(&store))
                .map_err(|e| kernel::Error::ConfigInvalid { message: format!("safety gate: {e}") })?,
        );

        // Create the context loader and load files
        let context_loader = Arc::new(ContextLoader::new(
            work_dir,
            config.context_files.clone(),
            TeamStore::clone(&store),
        ));
        // Load context files (best-effort — don't fail if one is missing)
        let _ = context_loader.load_all().await;

        self.config = Some(config);
        self.store = Some(store);
        self.scheduler = Some(scheduler);
        self.safety_gate = Some(safety_gate);
        self.context_loader = Some(context_loader);

        info!("TeamPlugin: loaded successfully");
        Ok(())
    }

    async fn on_unload(&mut self) -> AmanResult<()> {
        info!("TeamPlugin: unloading");
        self.config = None;
        self.store = None;
        self.scheduler = None;
        self.safety_gate = None;
        self.context_loader = None;
        Ok(())
    }

    async fn on_dependency_unloading(&self, _dep_name: &str) -> AmanResult<()> {
        Ok(())
    }

    fn event_sources(&self) -> Vec<Arc<dyn EventSource>> {
        Vec::new()
    }

    fn skills(&self) -> Vec<Arc<dyn Skill>> {
        Vec::new()
    }

    fn tools(&self) -> Vec<Arc<dyn Tool>> {
        Vec::new()
    }

    fn hooks(&self) -> Vec<Arc<dyn Hook>> {
        Vec::new()
    }

    fn memory_providers(&self) -> Vec<Arc<dyn MemoryProvider>> {
        Vec::new()
    }

    fn routes(&self) -> Option<axum::Router<()>> {
        self.api_state().map(team_api_routes)
    }
}
