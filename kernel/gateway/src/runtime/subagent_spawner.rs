// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! Gateway implementation of [`cognitive_llm::subagent::SubAgentSpawner`].
//!
//! Bridges the cognitive-layer trait to the gateway's concrete
//! [`AgentHarness::spawn_anonymous`] and [`AgentRegistry`] for LLM
//! provider resolution and tool-policy inheritance.

use std::sync::Arc;

use async_trait::async_trait;
use cognitive_llm::subagent::{SubAgentResult, SubAgentSpawner};
use kernel::agent::AgentDescriptor;
use kernel::react::SoulSnapshot;
use kernel::{AmanResult, Error};

use super::agent_harness::AgentHarness;
use super::agent_registry::AgentRegistry;

pub struct GatewaySubAgentSpawner {
    registry: Arc<AgentRegistry>,
    harness: Arc<AgentHarness>,
}

impl GatewaySubAgentSpawner {
    pub fn new(registry: Arc<AgentRegistry>, harness: Arc<AgentHarness>) -> Self {
        Self { registry, harness }
    }

    /// Resolve the parent agent from [`ToolContext`] extensions.
    /// Returns the first enabled agent if the context has no agent_id.
    async fn resolve_parent_agent_id(&self) -> Option<String> {
        // Walk the registry for the first enabled agent as the "parent".
        // In practice, ToolContext carries agent_id; we use this as
        // fallback for contexts where extensions are not populated.
        for agent in self.registry.list().await {
            if agent.descriptor.enabled {
                return Some(agent.descriptor.agent_id.clone());
            }
        }
        None
    }

    /// Merge the user-supplied descriptor with the parent agent's defaults.
    /// Fields that the caller left empty/unset are inherited from the parent.
    async fn merge_with_parent(
        &self,
        parent_agent_id: &str,
        descriptor: &mut AgentDescriptor,
    ) -> AmanResult<()> {
        let parent = self
            .registry
            .get(parent_agent_id)
            .await
            .ok_or_else(|| Error::ConfigInvalid {
                message: format!(
                    "SubAgentSpawner: parent agent '{parent_agent_id}' not found"
                ),
            })?;

        let pd = &parent.descriptor;

        // Inherit provider if not specified
        if descriptor.provider.is_empty() {
            descriptor.provider = pd.provider.clone();
        }

        // Inherit model if not specified
        if descriptor.model.is_empty() {
            descriptor.model = pd.model.clone();
        }

        // Inherit tool policy if not explicitly overridden
        // allowed_tools: None in the user descriptor means "inherit"
        // allowed_tools: Some(...) means "use this explicit list"
        if descriptor.allowed_tools.is_none() {
            descriptor.allowed_tools = pd.allowed_tools.clone();
        }
        if descriptor.denied_tools.is_empty() {
            descriptor.denied_tools = pd.denied_tools.clone();
        }

        // Inherit skill policy
        if descriptor.allowed_skills.is_none() {
            descriptor.allowed_skills = pd.allowed_skills.clone();
        }

        // Inherit token limits if not specified
        if descriptor.max_context_tokens.is_none() {
            descriptor.max_context_tokens = pd.max_context_tokens;
        }
        if descriptor.max_output_tokens.is_none() {
            descriptor.max_output_tokens = pd.max_output_tokens;
        }

        Ok(())
    }
}

#[async_trait]
impl SubAgentSpawner for GatewaySubAgentSpawner {
    async fn spawn(
        &self,
        mut descriptor: AgentDescriptor,
        soul_snapshot: SoulSnapshot,
        prompt: String,
        background: bool,
    ) -> AmanResult<SubAgentResult> {
        // ── Resolve parent and merge defaults ────────────────────────
        let parent_id = self
            .resolve_parent_agent_id()
            .await
            .ok_or_else(|| Error::ConfigInvalid {
                message: "SubAgentSpawner: no enabled parent agent found".to_owned(),
            })?;

        self.merge_with_parent(&parent_id, &mut descriptor).await?;

        // ── Get LLM provider for the parent ──────────────────────────
        let llm_provider = self
            .registry
            .get_llm_provider(&parent_id)
            .await
            .ok_or_else(|| Error::ConfigInvalid {
                message: format!(
                    "SubAgentSpawner: no LLM provider for agent '{parent_id}'"
                ),
            })?;

        // ── Spawn ────────────────────────────────────────────────────
        let handle = self.harness.spawn_anonymous(
            descriptor,
            soul_snapshot,
            prompt,
            llm_provider,
            background,
        );

        let agent_id = handle.agent_id.clone();
        let session_id = handle.session_id.clone();

        if background {
            Ok(SubAgentResult {
                agent_id,
                session_id,
                reply: String::new(),
                background: true,
            })
        } else {
            let reply = handle.wait().await?;
            Ok(SubAgentResult {
                agent_id,
                session_id,
                reply,
                background: false,
            })
        }
    }
}
