use config::AmanConfig;
use event_bus::{EventBus, InMemoryBus, InMemoryBusConfig};
use idle::AgentIdleManager;
use kernel::agent::{AgentDescriptor, AgentInstance, AgentStatus};
use kernel::event::{Event, EventType};
use kernel::AmanResult;
use serde_json::json;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

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
    bus: Arc<dyn EventBus>,
}

impl AgentRegistry {
    /// 创建一个空的 AgentRegistry。
    pub fn new(bus: Arc<dyn EventBus>) -> Self {
        Self {
            agents: RwLock::new(HashMap::new()),
            local_buses: RwLock::new(HashMap::new()),
            idle_managers: RwLock::new(HashMap::new()),
            bus,
        }
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

            // Create per-agent idle manager only if both global idle is enabled
            // AND this specific agent is enabled (has a configured provider).
            if idle_enabled && entry.enabled {
                let idle_manager = Arc::new(AgentIdleManager::new(
                    agent_id.clone(),
                    local_bus,
                    Some(Arc::clone(&self.bus) as Arc<dyn EventBus>),
                    idle_personality.clone(),
                    arousal_initial,
                    arousal_half_life,
                ));
                self.set_idle_manager(agent_id, idle_manager).await;
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
            let idle_manager = Arc::new(AgentIdleManager::new(
                agent_id.to_string(),
                local_bus,
                Some(Arc::clone(&self.bus) as Arc<dyn EventBus>),
                config.runtime.idle.personality.clone(),
                config.runtime.idle.arousal.initial_value,
                config.runtime.idle.arousal.half_life_secs,
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

    /// 获取指定 Agent 的信息。
    pub async fn get(&self, agent_id: &str) -> Option<AgentInstance> {
        let agents = self.agents.read().await;
        agents.get(agent_id).cloned()
    }

    /// 列出所有已注册的 Agent。
    pub async fn list(&self) -> Vec<AgentInstance> {
        let agents = self.agents.read().await;
        agents.values().cloned().collect()
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
            .map(|a| a.descriptor.allowed_tools.clone())
            .flatten()
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

    /// 清空注册表（shutdown 时调用）。
    pub async fn clear(&self) {
        // Shut down all per-agent idle managers first
        let idle_managers: Vec<Arc<AgentIdleManager>> = {
            let managers = self.idle_managers.read().await;
            managers.values().cloned().collect()
        };
        for manager in &idle_managers {
            let _ = manager.shutdown().await;
        }

        let mut agents = self.agents.write().await;
        agents.clear();
        let mut buses = self.local_buses.write().await;
        buses.clear();
        let mut managers = self.idle_managers.write().await;
        managers.clear();
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

    /// 启动所有 Agent 的 idle 后台循环（在 Phase 4 调用）。
    pub async fn start_all_idle_loops(&self) {
        let managers = self.idle_managers.read().await;
        for manager in managers.values() {
            manager.start().await;
        }
    }
}
