// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

use config::AmanConfig;
use daily_life::DailyLifeSystem;
use event_bus::{EventBus, InMemoryBus, InMemoryBusConfig};
use idle::{AgentIdleManager, BoredomActor};
use study::StudySystem;
use work::WorkSystem;
use kernel::agent::{AgentDescriptor, AgentInstance, AgentStatus, AgentSystemState};
use kernel::event::{Event, EventType};
use kernel::llm::LlmProvider;
use kernel::memory::MemoryProvider;
use kernel::trace::TraceStore;
use kernel::AmanResult;
use mcp_client::McpClientManager;
use serde_json::json;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

use super::session_store::SessionStore;

/// Agent 运行时注册表。
///
/// 管理 Agent 实例的注册、查询、状态变更，并在关键操作点发布 Agent 生命周期事件。
///
/// # 生命周期
/// - Phase 2：从 config.yaml 加载所有 Agent，注册到 Registry
/// - 运行时：可通过 API 动态注册/注销 Agent
/// - Phase 5→0 shutdown：Registry 自动清空
///
/// # 线程安全
/// AgentRegistry 内部使用 `RwLock<HashMap<String, AgentInstance>>`，
/// 所有方法都是非阻塞的（不涉及跨核锁），适合高频调用场景。
pub struct AgentRegistry {
    agents: RwLock<HashMap<String, AgentInstance>>,
    local_buses: RwLock<HashMap<String, Arc<dyn EventBus>>>,
    idle_managers: RwLock<HashMap<String, Arc<AgentIdleManager>>>,
    work_systems: RwLock<HashMap<String, Arc<WorkSystem>>>,
    study_systems: RwLock<HashMap<String, Arc<StudySystem>>>,
    daily_life_systems: RwLock<HashMap<String, Arc<DailyLifeSystem>>>,
    /// Per-agent session stores (None for disabled agents).
    session_stores: RwLock<HashMap<String, Option<Arc<SessionStore>>>>,
    /// Per-agent memory providers (knowledge graph).
    memory_providers: RwLock<HashMap<String, Arc<dyn MemoryProvider>>>,
    /// Per-agent LLM providers (API client).
    llm_providers: RwLock<HashMap<String, Arc<dyn LlmProvider>>>,
    /// Per-agent trace stores (task execution traces).
    trace_stores: RwLock<HashMap<String, Arc<dyn TraceStore>>>,
    /// Per-agent system state (idle / working / …), updated atomically by each system.
    system_states: RwLock<HashMap<String, Arc<std::sync::Mutex<AgentSystemState>>>>,
    /// Per-agent emotion evaluators (Some = active, None = emotions not configured).
    emotion_evaluators: RwLock<HashMap<String, Arc<super::emotion_evaluator::EmotionEvaluator>>>,
    /// Latest emotion IDs indexed by agent_id (read by SSE snapshot).
    emotion_latest: RwLock<HashMap<String, Arc<tokio::sync::Mutex<Option<String>>>>>,
    /// Per-agent MCP client managers (None = MCP not initialized for this agent).
    mcp_managers: RwLock<HashMap<String, Arc<mcp_client::McpClientManager>>>,
    bus: Arc<dyn EventBus>,
    /// Skill search index for BoredomActor tag-based skill lookup.
    skill_search: Option<Arc<skill::SkillSearch>>,
    /// Skill registry for BoredomActor direct skill execution.
    skill_registry: Option<Arc<skill::SkillRegistry>>,
}

impl AgentRegistry {
    /// 创建一个空的 AgentRegistry。
    pub fn new(bus: Arc<dyn EventBus>) -> Self {
        Self {
            agents: RwLock::new(HashMap::new()),
            local_buses: RwLock::new(HashMap::new()),
            idle_managers: RwLock::new(HashMap::new()),
            work_systems: RwLock::new(HashMap::new()),
            study_systems: RwLock::new(HashMap::new()),
            daily_life_systems: RwLock::new(HashMap::new()),
            session_stores: RwLock::new(HashMap::new()),
            memory_providers: RwLock::new(HashMap::new()),
            llm_providers: RwLock::new(HashMap::new()),
            trace_stores: RwLock::new(HashMap::new()),
            system_states: RwLock::new(HashMap::new()),
            emotion_evaluators: RwLock::new(HashMap::new()),
            emotion_latest: RwLock::new(HashMap::new()),
            mcp_managers: RwLock::new(HashMap::new()),
            bus,
            skill_search: None,
            skill_registry: None,
        }
    }

    /// Set the skill search index and registry (for BoredomActor).
    pub fn set_skill_index(
        &mut self,
        skill_search: Arc<skill::SkillSearch>,
        skill_registry: Arc<skill::SkillRegistry>,
    ) {
        self.skill_search = Some(skill_search);
        self.skill_registry = Some(skill_registry);
    }

    /// 从 config.yaml 的 agents 段加载所有 Agent。
    pub async fn load_from_config(&self, config: &AmanConfig) -> usize {
        let descriptors: Vec<AgentDescriptor> = config
            .agents
            .iter()
            .map(|(agent_id, entry)| {
                // Resolve per-agent tool config
                let (allowed_tools, denied_tools) = match &entry.tools {
                    Some(tc) => (tc.allow.clone(), tc.deny.clone()),
                    None => (None, vec![]),
                };

                // Resolve API model name from provider's model list.
                let api_model_id = config
                    .providers
                    .get(&entry.provider)
                    .and_then(|p| p.models.iter().find(|m| m.id == entry.model))
                    .map(|m| m.model_id.clone())
                    .unwrap_or_else(|| entry.model.clone());

                // Resolve model capabilities from the global models section.
                let model_params = config.models.get(&entry.model);

                AgentDescriptor {
                    agent_id: agent_id.clone(),
                    display_name: entry.display_name.clone(),
                    provider: entry.provider.clone(),
                    model: api_model_id,
                    soul_path: entry.system_prompt_override.as_ref().map(|_| {
                        let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
                        std::path::PathBuf::from(&home)
                            .join(".aman")
                            .join("agents")
                            .join(agent_id)
                            .join("SOUL.md")
                    }),
                    allowed_tools,
                    denied_tools,
                    allowed_skills: entry.skills.clone(),
                    enabled: entry.enabled,
                    max_context_tokens: model_params.map(|m| m.max_context_tokens),
                    max_output_tokens: model_params.map(|m| m.max_output_tokens),
                }
            })
            .collect();

        let count = descriptors.len();
        let mut agents = self.agents.write().await;
        for desc in descriptors {
            let agent_id = desc.agent_id.clone();
            let instance = AgentInstance::new(desc);
            agents.insert(agent_id.clone(), instance);
        }
        drop(agents);

        // Publish agent:registered events
        for desc in config.agents.keys() {
            let agent_id = desc.clone();
            let _ = self
                .bus
                .publish(Event::new(
                    "runtime:agent_registry",
                    EventType::Custom("agent:registered".to_owned()),
                    json!({ "agent_id": agent_id }),
                ))
                .await;
        }

        // Extract idle config values (resolve from partial or use defaults)
        let idle_enabled = config.runtime.idle.enabled;
        let idle_personality = config.runtime.idle.personality.clone();
        let arousal_initial = config.runtime.idle.arousal.initial_value;
        let arousal_half_life = config.runtime.idle.arousal.half_life_secs;

        // Create per-agent local event buses and idle managers
        for (agent_id, entry) in &config.agents {
            // Determine local bus config: per-agent override or default
            let queue_size = entry
                .event_bus
                .as_ref()
                .and_then(|b| b.max_queue_size)
                .unwrap_or(1_000);
            let local_bus: Arc<dyn EventBus> = Arc::new(InMemoryBus::new(InMemoryBusConfig {
                max_queue_size: queue_size,
                ..InMemoryBusConfig::default()
            }));
            self.set_local_bus(agent_id, Arc::clone(&local_bus)).await;

            // Per-agent shared system state for UI visibility
            let system_state: Arc<std::sync::Mutex<AgentSystemState>> =
                Arc::new(std::sync::Mutex::new(AgentSystemState::default()));
            {
                let mut states = self.system_states.write().await;
                states.insert(agent_id.clone(), Arc::clone(&system_state));
            }

            // Create per-agent idle manager only if both global idle is enabled
            // AND this specific agent is enabled.
            if idle_enabled && entry.enabled {
                let boredom_cfg = config
                    .runtime
                    .idle
                    .boredom
                    .as_ref();
                let boredom_actor = boredom_cfg.and_then(|cfg| {
                    match (&self.skill_search, &self.skill_registry) {
                        (Some(ss), Some(sr)) => {
                            let global_bus = Some(Arc::clone(&self.bus) as Arc<dyn EventBus>);
                            Some(Arc::new(BoredomActor::new(
                                cfg.clone(),
                                Arc::clone(ss),
                                Arc::clone(sr),
                                global_bus,
                            )))
                        }
                        _ => None,
                    }
                });
                let deferred_queue = {
                    let dir = super::agent_seed::aman_data_dir()
                        .join("agents")
                        .join(&agent_id)
                        .join("deferred");
                    match persistence::FileDeferredTaskQueue::open(&dir) {
                        Ok(q) => Some(Arc::new(q) as Arc<dyn kernel::deferred_task::DeferredTaskQueue>),
                        Err(e) => {
                            tracing::warn!(agent = %agent_id, error = %e, "Failed to open deferred task queue");
                            None
                        }
                    }
                };
                let idle_manager = Arc::new(AgentIdleManager::new(
                    agent_id.clone(),
                    Arc::clone(&local_bus) as Arc<dyn EventBus>,
                    Some(Arc::clone(&self.bus) as Arc<dyn EventBus>),
                    idle_personality.clone(),
                    arousal_initial,
                    arousal_half_life,
                    Some(Arc::clone(&system_state)),
                    boredom_actor,
                    deferred_queue,
                ));
                self.set_idle_manager(agent_id, idle_manager).await;
            }

            // Create per-agent work system if the agent is enabled.
            if entry.enabled {
                let work_system = Arc::new(WorkSystem::new(
                    agent_id.clone(),
                    config.runtime.work.clone(),
                    Arc::clone(&local_bus) as Arc<dyn EventBus>,
                    Arc::clone(&self.bus) as Arc<dyn EventBus>,
                    Some(Arc::clone(&system_state)),
                ));
                self.set_work_system(agent_id, work_system).await;

                let study_system = Arc::new(StudySystem::new(
                    agent_id.clone(),
                    config.runtime.study.clone(),
                    Arc::clone(&local_bus) as Arc<dyn EventBus>,
                    Arc::clone(&self.bus) as Arc<dyn EventBus>,
                    Some(Arc::clone(&system_state)),
                ));
                self.set_study_system(agent_id, study_system).await;

                let daily_system = Arc::new(DailyLifeSystem::new(
                    agent_id.clone(),
                    config.runtime.daily_life.clone(),
                    Arc::clone(&local_bus) as Arc<dyn EventBus>,
                    Arc::clone(&self.bus) as Arc<dyn EventBus>,
                    Some(Arc::clone(&system_state)),
                ));
                self.set_daily_life_system(agent_id, daily_system).await;
            }
        }

        count
    }

    /// Reload a single agent from config, updating the in-memory instance
    /// and creating/destroying the idle manager as needed (e.g. after the
    /// user configures a provider for a previously-unconfigured agent).
    pub async fn reload_agent(&self, config: &AmanConfig, agent_id: &str) -> AmanResult<()> {
        let entry = config
            .agents
            .get(agent_id)
            .ok_or_else(|| kernel::Error::ConfigInvalid {
                message: format!("agent '{agent_id}' not found in config"),
            })?;

        // Resolve API model name from provider's model list.
        let api_model_id = config
            .providers
            .get(&entry.provider)
            .and_then(|p| p.models.iter().find(|m| m.id == entry.model))
            .map(|m| m.model_id.clone())
            .unwrap_or_else(|| entry.model.clone());

        let model_params = config.models.get(&entry.model);

        let (allowed_tools, denied_tools) = match &entry.tools {
            Some(tc) => (tc.allow.clone(), tc.deny.clone()),
            None => (None, vec![]),
        };

        let desc = AgentDescriptor {
            agent_id: agent_id.to_string(),
            display_name: entry.display_name.clone(),
            provider: entry.provider.clone(),
            model: api_model_id,
            soul_path: entry.system_prompt_override.as_ref().map(|_| {
                let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
                std::path::PathBuf::from(&home)
                    .join(".aman")
                    .join("agents")
                    .join(agent_id)
                    .join("SOUL.md")
            }),
            allowed_tools,
            denied_tools,
            allowed_skills: entry.skills.clone(),
            enabled: entry.enabled,
            max_context_tokens: model_params.map(|m| m.max_context_tokens),
            max_output_tokens: model_params.map(|m| m.max_output_tokens),
        };

        // Update the agent instance in the registry.
        let instance = AgentInstance::new(desc);
        {
            let mut agents = self.agents.write().await;
            agents.insert(agent_id.to_string(), instance);
        }

        // Ensure a local event bus exists for this agent.
        {
            let buses = self.local_buses.read().await;
            if !buses.contains_key(agent_id) {
                drop(buses);
                let queue_size = entry
                    .event_bus
                    .as_ref()
                    .and_then(|b| b.max_queue_size)
                    .unwrap_or(1_000);
                let local_bus: Arc<dyn EventBus> = Arc::new(InMemoryBus::new(InMemoryBusConfig {
                    max_queue_size: queue_size,
                    ..InMemoryBusConfig::default()
                }));
                self.set_local_bus(agent_id, Arc::clone(&local_bus)).await;
            }
        }

        // Create or destroy idle manager based on enabled state.
        let idle_enabled = config.runtime.idle.enabled && entry.enabled;
        let has_idle = {
            let managers = self.idle_managers.read().await;
            managers.contains_key(agent_id)
        };

        if idle_enabled && !has_idle {
            let local_bus = self
                .get_local_bus(agent_id)
                .await
                .unwrap_or_else(|| Arc::clone(&self.bus) as Arc<dyn EventBus>);
            let ss = self.get_or_create_system_state(agent_id).await;
            let personality = config.runtime.idle.personality.clone();
            let boredom_cfg = config
                .runtime
                .idle
                .boredom
                .as_ref();
            let boredom_actor = boredom_cfg.and_then(|cfg| {
                match (&self.skill_search, &self.skill_registry) {
                    (Some(ss_idx), Some(sr)) => {
                        let global_bus = Some(Arc::clone(&self.bus) as Arc<dyn EventBus>);
                        Some(Arc::new(BoredomActor::new(
                            cfg.clone(),
                            Arc::clone(ss_idx),
                            Arc::clone(sr),
                            global_bus,
                        )))
                    }
                    _ => None,
                }
            });
            let deferred_queue = {
                let dir = super::agent_seed::aman_data_dir()
                    .join("agents")
                    .join(agent_id)
                    .join("deferred");
                match persistence::FileDeferredTaskQueue::open(&dir) {
                    Ok(q) => Some(Arc::new(q) as Arc<dyn kernel::deferred_task::DeferredTaskQueue>),
                    Err(e) => {
                        tracing::warn!(agent = %agent_id, error = %e, "Failed to open deferred task queue");
                        None
                    }
                }
            };
            let idle_manager = Arc::new(AgentIdleManager::new(
                agent_id.to_string(),
                local_bus,
                Some(Arc::clone(&self.bus) as Arc<dyn EventBus>),
                personality,
                config.runtime.idle.arousal.initial_value,
                config.runtime.idle.arousal.half_life_secs,
                Some(ss),
                boredom_actor,
                deferred_queue,
            ));
            self.set_idle_manager(agent_id, idle_manager.clone()).await;
            // Start the idle loop immediately.
            idle_manager.start().await;
            tracing::info!(agent = %agent_id, "idle manager created and started after reload");
        } else if !idle_enabled && has_idle {
            // Agent was disabled — shut down the idle manager.
            if let Some(manager) = self.get_idle_manager(agent_id).await {
                let _ = manager.shutdown().await;
            }
            self.remove_idle_manager(agent_id).await;
            tracing::info!(agent = %agent_id, "idle manager shut down after reload");
        }

        // Create or destroy work system based on enabled state.
        let work_enabled = entry.enabled;
        let has_work = {
            let systems = self.work_systems.read().await;
            systems.contains_key(agent_id)
        };

        if work_enabled && !has_work {
            let local_bus = self
                .get_local_bus(agent_id)
                .await
                .unwrap_or_else(|| Arc::clone(&self.bus) as Arc<dyn EventBus>);
            let ss = self.get_or_create_system_state(agent_id).await;
            let work_system = Arc::new(WorkSystem::new(
                agent_id.to_string(),
                config.runtime.work.clone(),
                Arc::clone(&local_bus) as Arc<dyn EventBus>,
                Arc::clone(&self.bus) as Arc<dyn EventBus>,
                Some(ss),
            ));
            self.set_work_system(agent_id, work_system).await;
            tracing::info!(agent = %agent_id, "work system created after reload");
        } else if !work_enabled && has_work {
            if let Some(ws) = self.get_work_system(agent_id).await {
                ws.shutdown().await;
            }
            self.remove_work_system(agent_id).await;
            tracing::info!(agent = %agent_id, "work system shut down after reload");
        }

        // Create or destroy study system based on enabled state.
        let has_study = {
            let systems = self.study_systems.read().await;
            systems.contains_key(agent_id)
        };

        if work_enabled && !has_study {
            let local_bus = self
                .get_local_bus(agent_id)
                .await
                .unwrap_or_else(|| Arc::clone(&self.bus) as Arc<dyn EventBus>);
            let ss = self.get_or_create_system_state(agent_id).await;
            let study_system = Arc::new(StudySystem::new(
                agent_id.to_string(),
                config.runtime.study.clone(),
                Arc::clone(&local_bus) as Arc<dyn EventBus>,
                Arc::clone(&self.bus) as Arc<dyn EventBus>,
                Some(ss),
            ));
            self.set_study_system(agent_id, study_system).await;
            tracing::info!(agent = %agent_id, "study system created after reload");
        } else if !work_enabled && has_study {
            if let Some(ss) = self.get_study_system(agent_id).await {
                ss.shutdown().await;
            }
            self.remove_study_system(agent_id).await;
            tracing::info!(agent = %agent_id, "study system shut down after reload");
        }

        // Create or destroy daily-life system based on enabled state.
        let has_daily = {
            let systems = self.daily_life_systems.read().await;
            systems.contains_key(agent_id)
        };

        if work_enabled && !has_daily {
            let local_bus = self
                .get_local_bus(agent_id)
                .await
                .unwrap_or_else(|| Arc::clone(&self.bus) as Arc<dyn EventBus>);
            let ss = self.get_or_create_system_state(agent_id).await;
            let daily_system = Arc::new(DailyLifeSystem::new(
                agent_id.to_string(),
                config.runtime.daily_life.clone(),
                Arc::clone(&local_bus) as Arc<dyn EventBus>,
                Arc::clone(&self.bus) as Arc<dyn EventBus>,
                Some(ss),
            ));
            self.set_daily_life_system(agent_id, daily_system).await;
            tracing::info!(agent = %agent_id, "daily-life system created after reload");
        } else if !work_enabled && has_daily {
            if let Some(ds) = self.get_daily_life_system(agent_id).await {
                ds.shutdown().await;
            }
            self.remove_daily_life_system(agent_id).await;
            tracing::info!(agent = %agent_id, "daily-life system shut down after reload");
        }

        let _ = self
            .bus
            .publish(Event::new(
                "runtime:agent_registry",
                EventType::Custom("agent:reloaded".to_owned()),
                json!({ "agent_id": agent_id }),
            ))
            .await;

        Ok(())
    }

    /// 注册一个 Agent。
    pub async fn register(&self, descriptor: AgentDescriptor) -> AmanResult<()> {
        let agent_id = descriptor.agent_id.clone();
        let instance = AgentInstance::new(descriptor);
        {
            let mut agents = self.agents.write().await;
            agents.insert(agent_id.clone(), instance);
        }
        let _ = self
            .bus
            .publish(Event::new(
                "runtime:agent_registry",
                EventType::Custom("agent:registered".to_owned()),
                json!({ "agent_id": agent_id }),
            ))
            .await;
        Ok(())
    }

    /// 注销一个 Agent。
    pub async fn unregister(&self, agent_id: &str, reason: &str) -> AmanResult<()> {
        {
            let mut agents = self.agents.write().await;
            agents.remove(agent_id);
        }
        let _ = self
            .bus
            .publish(Event::new(
                "runtime:agent_registry",
                EventType::Custom("agent:removed".to_owned()),
                json!({ "agent_id": agent_id, "reason": reason }),
            ))
            .await;
        Ok(())
    }

    /// 获取指定 Agent 的信息（含当前 system_state）。
    pub async fn get(&self, agent_id: &str) -> Option<AgentInstance> {
        let agents = self.agents.read().await;
        let mut instance = agents.get(agent_id).cloned()?;
        // Populate live system state
        if let Some(ss) = self.get_system_state(agent_id).await {
            instance.system_state = *ss.lock().expect("system_state lock");
        }
        Some(instance)
    }

    /// 列出所有已注册的 Agent（含当前 system_state）。
    pub async fn list(&self) -> Vec<AgentInstance> {
        let agents = self.agents.read().await;
        let mut instances: Vec<AgentInstance> = agents.values().cloned().collect();
        let states = self.system_states.read().await;
        for instance in &mut instances {
            if let Some(ss) = states.get(&instance.descriptor.agent_id) {
                instance.system_state = *ss.lock().expect("system_state lock");
            }
        }
        instances
    }

    /// 更新 Agent 的状态。
    pub async fn set_status(
        &self,
        agent_id: &str,
        new_status: AgentStatus,
    ) -> AmanResult<()> {
        let old_status = {
            let mut agents = self.agents.write().await;
            let instance = agents.get_mut(agent_id).ok_or_else(|| {
                kernel::Error::ConfigInvalid {
                    message: format!("agent '{agent_id}' not found"),
                }
            })?;
            let old = instance.status;
            instance.status = new_status;
            old
        };

        if old_status != new_status {
            let _ = self
                .bus
                .publish(Event::new(
                    "runtime:agent_registry",
                    EventType::Custom("agent:status_changed".to_owned()),
                    json!({
                        "agent_id": agent_id,
                        "old_status": old_status,
                        "new_status": new_status,
                    }),
                ))
                .await;
        }
        Ok(())
    }

    /// 设置 Agent 的活跃 session_id。
    pub async fn set_active_session(
        &self,
        agent_id: &str,
        session_id: Option<String>,
    ) -> AmanResult<()> {
        let mut agents = self.agents.write().await;
        let instance = agents.get_mut(agent_id).ok_or_else(|| {
            kernel::Error::ConfigInvalid {
                message: format!("agent '{agent_id}' not found"),
            }
        })?;
        instance.active_session_id = session_id;
        Ok(())
    }

    /// 获取该 Agent 允许的 Tool 名称列表。
    ///
    /// 返回 None 表示全部可用，Some(list) 表示白名单。
    #[must_use]
    pub async fn allowed_tools(&self, agent_id: &str) -> Option<Vec<String>> {
        let agents = self.agents.read().await;
        agents
            .get(agent_id)
            .and_then(|a| a.descriptor.allowed_tools.clone())
    }

    /// 检查该 Agent 是否有权限使用指定的 Tool。
    #[must_use]
    pub async fn tool_allowed(&self, agent_id: &str, tool_name: &str) -> bool {
        let agents = self.agents.read().await;
        let Some(instance) = agents.get(agent_id) else {
            return false;
        };
        let desc = &instance.descriptor;

        // Check denylist first
        if desc.denied_tools.iter().any(|d| d == tool_name) {
            return false;
        }

        // Check allowlist: None = all allowed
        match &desc.allowed_tools {
            Some(allow_list) => allow_list.iter().any(|a| a == tool_name || a == "*"),
            None => true,
        }
    }

    /// Stops all agent idle managers and work systems without clearing state.
    ///
    /// Called during Phase 4 shutdown to prevent agents from generating new
    /// events while the event bus is being drained. The full state cleanup
    /// happens later in Phase 2 via [`clear()`].
    pub async fn stop_idle_systems(&self) {
        let idle_managers: Vec<Arc<AgentIdleManager>> = {
            let managers = self.idle_managers.read().await;
            managers.values().cloned().collect()
        };
        for manager in &idle_managers {
            let _ = manager.shutdown().await;
        }

        let work_systems: Vec<Arc<WorkSystem>> = {
            let systems = self.work_systems.read().await;
            systems.values().cloned().collect()
        };
        for ws in &work_systems {
            ws.shutdown().await;
        }

        let study_systems: Vec<Arc<StudySystem>> = {
            let systems = self.study_systems.read().await;
            systems.values().cloned().collect()
        };
        for ss in &study_systems {
            ss.shutdown().await;
        }

        let daily_systems: Vec<Arc<DailyLifeSystem>> = {
            let systems = self.daily_life_systems.read().await;
            systems.values().cloned().collect()
        };
        for ds in &daily_systems {
            ds.shutdown().await;
        }
    }

    /// 清空注册表（shutdown 时调用）。
    pub async fn clear(&self) {
        // Shut down all per-agent emotion evaluators
        let eval_keys: Vec<String> = {
            let evaluators = self.emotion_evaluators.read().await;
            evaluators.keys().cloned().collect()
        };
        for agent_id in &eval_keys {
            self.stop_emotion_evaluator(agent_id).await;
        }

        // Shut down all per-agent idle managers first
        let idle_managers: Vec<Arc<AgentIdleManager>> = {
            let managers = self.idle_managers.read().await;
            managers.values().cloned().collect()
        };
        for manager in &idle_managers {
            let _ = manager.shutdown().await;
        }

        // Shut down all per-agent work systems
        let work_systems: Vec<Arc<WorkSystem>> = {
            let systems = self.work_systems.read().await;
            systems.values().cloned().collect()
        };
        for ws in &work_systems {
            ws.shutdown().await;
        }

        // Shut down all per-agent study systems
        let study_systems: Vec<Arc<StudySystem>> = {
            let systems = self.study_systems.read().await;
            systems.values().cloned().collect()
        };
        for ss in &study_systems {
            ss.shutdown().await;
        }

        // Shut down all per-agent daily-life systems
        let daily_systems: Vec<Arc<DailyLifeSystem>> = {
            let systems = self.daily_life_systems.read().await;
            systems.values().cloned().collect()
        };
        for ds in &daily_systems {
            ds.shutdown().await;
        }

        let mut agents = self.agents.write().await;
        agents.clear();
        let mut buses = self.local_buses.write().await;
        buses.clear();
        let mut managers = self.idle_managers.write().await;
        managers.clear();
        let mut systems = self.work_systems.write().await;
        systems.clear();
        let mut study_systems = self.study_systems.write().await;
        study_systems.clear();
        let mut daily_systems = self.daily_life_systems.write().await;
        daily_systems.clear();
        let mut states = self.system_states.write().await;
        states.clear();
        let mut stores = self.session_stores.write().await;
        stores.clear();
        let mut memories = self.memory_providers.write().await;
        memories.clear();
        let mut llms = self.llm_providers.write().await;
        llms.clear();
        let mut traces = self.trace_stores.write().await;
        traces.clear();
    }

    /// 设置 Agent 的 Local EventBus。
    pub async fn set_local_bus(&self, agent_id: &str, local_bus: Arc<dyn EventBus>) {
        let mut buses = self.local_buses.write().await;
        buses.insert(agent_id.to_owned(), local_bus);
    }

    /// 获取 Agent 的 Local EventBus。
    pub async fn get_local_bus(&self, agent_id: &str) -> Option<Arc<dyn EventBus>> {
        let buses = self.local_buses.read().await;
        buses.get(agent_id).cloned()
    }

    /// Return all agent_id → local_bus pairs for observer subscriptions.
    pub async fn all_local_buses(&self) -> Vec<(String, Arc<dyn EventBus>)> {
        let buses = self.local_buses.read().await;
        buses.iter().map(|(k, v)| (k.clone(), Arc::clone(v))).collect()
    }

    /// 移除 Agent 的 IdleManager。
    pub async fn remove_idle_manager(&self, agent_id: &str) {
        let mut managers = self.idle_managers.write().await;
        managers.remove(agent_id);
    }

    /// 设置 Agent 的 IdleManager。
    pub async fn set_idle_manager(&self, agent_id: &str, manager: Arc<AgentIdleManager>) {
        let mut managers = self.idle_managers.write().await;
        managers.insert(agent_id.to_owned(), manager);
    }

    /// 获取 Agent 的 IdleManager。
    pub async fn get_idle_manager(&self, agent_id: &str) -> Option<Arc<AgentIdleManager>> {
        let managers = self.idle_managers.read().await;
        managers.get(agent_id).cloned()
    }

    /// 获取 Agent 的 IdleCoordination（用于 harness 交互）。
    pub async fn get_idle_coordination(&self, agent_id: &str) -> Option<Arc<idle::IdleCoordination>> {
        let managers = self.idle_managers.read().await;
        managers.get(agent_id).map(|m| Arc::clone(m.coordination()))
    }

    /// 设置 Agent 的 WorkSystem。
    pub async fn set_work_system(&self, agent_id: &str, system: Arc<WorkSystem>) {
        let mut systems = self.work_systems.write().await;
        systems.insert(agent_id.to_owned(), system);
    }

    /// 获取 Agent 的 WorkSystem。
    pub async fn get_work_system(&self, agent_id: &str) -> Option<Arc<WorkSystem>> {
        let systems = self.work_systems.read().await;
        systems.get(agent_id).cloned()
    }

    /// 移除 Agent 的 WorkSystem。
    pub async fn remove_work_system(&self, agent_id: &str) {
        let mut systems = self.work_systems.write().await;
        systems.remove(agent_id);
    }

    // ── StudySystem ─────────────────────────────────────────────────

    pub async fn set_study_system(&self, agent_id: &str, system: Arc<StudySystem>) {
        let mut systems = self.study_systems.write().await;
        systems.insert(agent_id.to_owned(), system);
    }

    pub async fn get_study_system(&self, agent_id: &str) -> Option<Arc<StudySystem>> {
        let systems = self.study_systems.read().await;
        systems.get(agent_id).cloned()
    }

    pub async fn remove_study_system(&self, agent_id: &str) {
        let mut systems = self.study_systems.write().await;
        systems.remove(agent_id);
    }

    // ── DailyLifeSystem ─────────────────────────────────────────────

    pub async fn set_daily_life_system(&self, agent_id: &str, system: Arc<DailyLifeSystem>) {
        let mut systems = self.daily_life_systems.write().await;
        systems.insert(agent_id.to_owned(), system);
    }

    pub async fn get_daily_life_system(&self, agent_id: &str) -> Option<Arc<DailyLifeSystem>> {
        let systems = self.daily_life_systems.read().await;
        systems.get(agent_id).cloned()
    }

    pub async fn remove_daily_life_system(&self, agent_id: &str) {
        let mut systems = self.daily_life_systems.write().await;
        systems.remove(agent_id);
    }

    // ── Per-agent session store ──────────────────────────────────────

    /// Set the session store for an agent (None if disabled or init failed).
    pub async fn set_session_store(&self, agent_id: &str, store: Option<Arc<SessionStore>>) {
        let mut stores = self.session_stores.write().await;
        stores.insert(agent_id.to_owned(), store);
    }

    /// Get the session store for an agent.
    pub async fn get_session_store(&self, agent_id: &str) -> Option<Arc<SessionStore>> {
        let stores = self.session_stores.read().await;
        stores.get(agent_id).cloned().flatten()
    }

    /// Return the first available session store (backward compat).
    pub async fn first_session_store(&self) -> Option<Arc<SessionStore>> {
        let stores = self.session_stores.read().await;
        stores.values().find_map(|v| v.clone())
    }

    /// Return all non-None session stores (for searching across agents).
    pub async fn all_session_stores(&self) -> Vec<Arc<SessionStore>> {
        let stores = self.session_stores.read().await;
        stores.values().filter_map(|v| v.clone()).collect()
    }

    // ── Per-agent memory provider ────────────────────────────────────

    /// Set the memory provider for an agent.
    pub async fn set_memory_provider(&self, agent_id: &str, provider: Arc<dyn MemoryProvider>) {
        let mut providers = self.memory_providers.write().await;
        providers.insert(agent_id.to_owned(), provider);
    }

    /// Get the memory provider for an agent.
    pub async fn get_memory_provider(&self, agent_id: &str) -> Option<Arc<dyn MemoryProvider>> {
        let providers = self.memory_providers.read().await;
        providers.get(agent_id).cloned()
    }

    // ── Per-agent LLM provider ───────────────────────────────────────

    /// Set the LLM provider for an agent.
    pub async fn set_llm_provider(&self, agent_id: &str, provider: Arc<dyn LlmProvider>) {
        let mut providers = self.llm_providers.write().await;
        providers.insert(agent_id.to_owned(), provider);
    }

    /// Get the LLM provider for an agent.
    pub async fn get_llm_provider(&self, agent_id: &str) -> Option<Arc<dyn LlmProvider>> {
        let providers = self.llm_providers.read().await;
        providers.get(agent_id).cloned()
    }

    // ── Per-agent trace store ─────────────────────────────────────────

    /// Set the trace store for an agent.
    pub async fn set_trace_store(&self, agent_id: &str, store: Arc<dyn TraceStore>) {
        let mut stores = self.trace_stores.write().await;
        stores.insert(agent_id.to_owned(), store);
    }

    /// Get the trace store for an agent.
    pub async fn get_trace_store(&self, agent_id: &str) -> Option<Arc<dyn TraceStore>> {
        let stores = self.trace_stores.read().await;
        stores.get(agent_id).cloned()
    }

    // ── Per-agent system state ───────────────────────────────────────

    /// Get the shared system state for an agent.
    pub async fn get_system_state(&self, agent_id: &str) -> Option<Arc<std::sync::Mutex<AgentSystemState>>> {
        let states = self.system_states.read().await;
        states.get(agent_id).cloned()
    }

    /// Get or create the shared system state for an agent.
    async fn get_or_create_system_state(&self, agent_id: &str) -> Arc<std::sync::Mutex<AgentSystemState>> {
        let states = self.system_states.read().await;
        if let Some(ss) = states.get(agent_id) {
            return Arc::clone(ss);
        }
        drop(states);
        let ss = Arc::new(std::sync::Mutex::new(AgentSystemState::default()));
        let mut states = self.system_states.write().await;
        states.entry(agent_id.to_owned()).or_insert_with(|| Arc::clone(&ss));
        ss
    }

    /// Atomically update the system state for an agent.
    pub async fn set_system_state(&self, agent_id: &str, state: AgentSystemState) {
        let ss = self.get_or_create_system_state(agent_id).await;
        *ss.lock().expect("system_state lock") = state;
    }

    /// Set the human-readable activity description for an agent.
    /// Shown in the UI so users can see what the agent is doing right now.
    pub async fn set_activity(&self, agent_id: &str, activity: impl Into<String>) {
        let mut agents = self.agents.write().await;
        if let Some(instance) = agents.get_mut(agent_id) {
            instance.activity = activity.into();
        }
    }

    // ── Emotion evaluator management ───────────────────────────────────

    /// Initialize (or re-create) the emotion evaluator for an agent.
    ///
    /// Automatically gates on the existence of a valid `emotions/` directory.
    /// If emotions aren't configured, this is a no-op and the desktop will
    /// fall back to the state-based emoji mapping.
    ///
    /// Should be called AFTER session_store, trace_store, and the LLM
    /// provider have been set up.
    pub async fn init_emotion_evaluator(
        &self,
        agent_id: &str,
        session_store: Option<Arc<SessionStore>>,
        trace_store: Option<Arc<dyn kernel::trace::TraceStore>>,
        llm_config: super::emotion_evaluator::EmotionLlmConfig,
        eval_config: super::emotion_evaluator::EmotionEvalConfig,
    ) {
        // Stop any existing evaluator first.
        self.stop_emotion_evaluator(agent_id).await;

        let ss = self.get_or_create_system_state(agent_id).await;
        let bus = Arc::clone(&self.bus) as Arc<dyn EventBus>;

        let Some(evaluator) = super::emotion_evaluator::EmotionEvaluator::new(
            agent_id.to_owned(),
            session_store,
            trace_store,
            llm_config,
            eval_config,
            bus,
            ss,
        ) else {
            // No valid emotions — store None so SSE can skip.
            let mut latest = self.emotion_latest.write().await;
            latest.remove(agent_id);
            return;
        };

        // Store the latest-emotion handle for SSE snapshots.
        let handle = evaluator.latest_emotion_handle();
        {
            let mut latest = self.emotion_latest.write().await;
            latest.insert(agent_id.to_owned(), handle);
        }

        let arc_eval = Arc::new(evaluator);
        // NOTE: start() is called later in start_all_emotion_evaluators()
        // during Phase 4, when the Tokio runtime is active.

        let mut evaluators = self.emotion_evaluators.write().await;
        evaluators.insert(agent_id.to_owned(), arc_eval);
    }

    /// Stop and remove the emotion evaluator for an agent.
    pub async fn stop_emotion_evaluator(&self, agent_id: &str) {
        if let Some(eval) = {
            let mut evaluators = self.emotion_evaluators.write().await;
            evaluators.remove(agent_id)
        } {
            eval.stop();
        }
        let mut latest = self.emotion_latest.write().await;
        latest.remove(agent_id);
    }

    /// Get the latest emotion ID for an agent (for the SSE snapshot).
    pub async fn get_latest_emotion(&self, agent_id: &str) -> Option<String> {
        let latest = self.emotion_latest.read().await;
        let handle = latest.get(agent_id)?;
        handle.lock().await.clone()
    }

    // ── Idle loop management ─────────────────────────────────────────

    /// 启动所有 Agent 的 idle 后台循环（在 Phase 4 调用）。
    pub async fn start_all_idle_loops(&self) {
        let managers = self.idle_managers.read().await;
        for manager in managers.values() {
            manager.start().await;
        }
    }

    /// Start all emotion evaluator background loops (Phase 4).
    /// Must be called after the Tokio runtime is active — the evaluators
    /// use `tokio::spawn` internally.
    pub async fn start_all_emotion_evaluators(&self) {
        let evaluators = self.emotion_evaluators.read().await;
        for eval in evaluators.values() {
            eval.start();
        }
        if !evaluators.is_empty() {
            tracing::info!(count = evaluators.len(), "emotion evaluators started");
        }
    }

    // ── MCP ─────────────────────────────────────────────────────────

    /// Get the MCP client manager for a specific agent.
    pub async fn get_mcp_manager(
        &self,
        agent_id: &str,
    ) -> Option<Arc<McpClientManager>> {
        self.mcp_managers.read().await.get(agent_id).cloned()
    }

    /// Initialize MCP for a specific agent.
    ///
    /// Loads global + per-agent configs, creates a [`McpClientManager`],
    /// and spawns auto-connection in the background.
    pub async fn init_mcp_for_agent(
        &self,
        agent_id: &str,
        tools: Arc<tool::ToolRegistry>,
    ) -> Option<Arc<McpClientManager>> {
        // Check if already initialized
        {
            let managers = self.mcp_managers.read().await;
            if managers.contains_key(agent_id) {
                return managers.get(agent_id).cloned();
            }
        }

        let manager = Arc::new(McpClientManager::new(
            agent_id.to_string(),
            tools,
        ));

        // Store before connecting so the manager is visible
        self.mcp_managers
            .write()
            .await
            .insert(agent_id.to_string(), Arc::clone(&manager));

        // Spawn auto-connect in background (non-blocking)
        let mgr = Arc::clone(&manager);
        tokio::spawn(async move {
            mgr.connect_all_from_config().await;
        });

        tracing::info!(agent = %agent_id, "MCP client manager initialized");

        Some(manager)
    }

    /// Deinitialize MCP for a specific agent.
    ///
    /// Disconnects all MCP servers and unregisters their tools.
    pub async fn deinit_mcp_for_agent(&self, agent_id: &str) {
        if let Some(manager) = self.mcp_managers.write().await.remove(agent_id) {
            manager.disconnect_all().await;
            tracing::info!(agent = %agent_id, "MCP client manager removed");
        }
    }

    /// Reload MCP config for a specific agent.
    ///
    /// Disconnects current connections and re-connects with updated config.
    pub async fn reload_mcp_for_agent(&self, agent_id: &str) -> Result<(), String> {
        let manager = self.get_mcp_manager(agent_id).await.ok_or_else(|| {
            format!("MCP manager not found for agent '{agent_id}'")
        })?;

        manager.disconnect_all().await;
        manager.connect_all_from_config().await;

        Ok(())
    }

    /// Initialize MCP for all registered agents.
    pub async fn init_mcp_all(&self, tools: Arc<tool::ToolRegistry>) {
        let agent_ids: Vec<String> = self.agents.read().await.keys().cloned().collect();
        for agent_id in &agent_ids {
            self.init_mcp_for_agent(agent_id, Arc::clone(&tools)).await;
        }
    }
}
