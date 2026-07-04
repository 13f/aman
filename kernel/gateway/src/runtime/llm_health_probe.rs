// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! LLM 后端健康半探针（Half-Probe）。
//!
//! 订阅 `CronTick` 事件，周期性检查所有处于 Down/Degraded 状态的 LLM 后端。
//!
//! # 设计
//!
//! 直接复用 registry 中已有的 `llm_providers`（每个都有 base_url + api_key），
//! 不新建数据结构。探针遍历所有 provider，按 base_url 去重后对每个
//! Down/Degraded 的后端做一次轻量调用验证是否恢复。

#![forbid(unsafe_code)]

use std::collections::HashSet;
use std::sync::Arc;

use async_trait::async_trait;
use event_bus::EventHandler;
use kernel::event::{Event, EventType};
use kernel::llm::LlmProvider;
use kernel::AmanResult;

use super::agent_registry::AgentRegistry;
use super::backend_health::BackendStatus;

/// LLM 后端健康探针。
pub struct LlmHealthProbe {
    registry: Arc<AgentRegistry>,
}

impl LlmHealthProbe {
    /// 创建一个新的 LlmHealthProbe。
    pub fn new(registry: Arc<AgentRegistry>) -> Self {
        Self { registry }
    }

    /// 执行一轮探针检查。
    pub async fn probe_all(&self) {
        // 收集所有 provider 实例
        let providers = self.registry.get_all_llm_providers().await;

        // 按 base_url 去重（多个 agent 可能共享同一个 provider）
        let mut seen = HashSet::new();
        let unique: Vec<_> = providers
            .into_iter()
            .filter(|(_, p)| seen.insert(p.base_url().to_owned()))
            .collect();

        if unique.is_empty() {
            return;
        }

        for (_agent_id, provider) in &unique {
            let base_url = provider.base_url();

            let health = match self.registry.get_backend_health(base_url).await {
                Some(h) => h,
                None => continue,
            };

            let status = health.status();
            if status != BackendStatus::Down && status != BackendStatus::Degraded {
                continue;
            }

            match Self::probe_with_provider(provider).await {
                Ok(()) => {
                    let config = self.registry.backend_health_registry().config();
                    if let Some(ev) = health.record_success(config) {
                        tracing::info!(base_url = %base_url, "llm_health_probe: recovered");
                        super::reflection::publish_health_event(&self.registry, ev);
                    }
                }
                Err(e) => {
                    let config = self.registry.backend_health_registry().config();
                    if let Some(ev) = health.record_failure(&e, config) {
                        tracing::warn!(base_url = %base_url, error = %e, "llm_health_probe: still down");
                        super::reflection::publish_health_event(&self.registry, ev);
                    }
                }
            }
        }
    }

    /// 使用已有的 LLM provider 做轻量健康检查。
    async fn probe_with_provider(provider: &Arc<dyn LlmProvider>) -> Result<(), String> {
        use kernel::react::{ChatMessage, ChatMessageRole};

        let req = kernel::llm::LlmChatRequest {
            model: String::new(),
            system_prompt: String::new(),
            messages: vec![ChatMessage {
                role: ChatMessageRole::User,
                content: "ping".into(),
                tool_call_id: None,
                tool_name: None,
                tool_calls: None,
                reasoning_content: String::new(),
            }],
            tools: vec![],
            max_output_tokens: 1,
            response_format: None,
        };

        // 流式调用，callback 忽略输出
        match provider.chat_completion(req, None).await {
            Ok(_) => Ok(()),
            Err(e) => Err(format!("probe failed: {e}")),
        }
    }
}

#[async_trait]
impl EventHandler for LlmHealthProbe {
    async fn handle(&self, event: Event) -> AmanResult<()> {
        if event.event_type == EventType::CronTick && event.source.as_str() == "llm_health_probe" {
            self.probe_all().await;
        }
        Ok(())
    }
}
