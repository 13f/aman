// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! Gateway implementation of [`cognitive_llm::subagent::SubAgentSpawner`].
//!
//! Bridges the cognitive-layer trait to the gateway's concrete
//! [`super::agent_harness::AgentHarness::spawn_anonymous`] and
//! [`AgentRegistry`] for LLM provider resolution and tool-policy
//! inheritance.
//!
//! Also maintains a pending-handle store so background sub-agents can
//! be collected later via [`SubAgentSpawner::collect_result`].

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use cognitive_llm::subagent::{SubAgentResult, SubAgentSpawner};
use kernel::agent::AgentDescriptor;
use kernel::react::SoulSnapshot;
use kernel::{AmanResult, Error};
use tokio::sync::RwLock;

use super::agent_harness::{AgentHarness, AnonymousAgentHandle};
use super::agent_registry::AgentRegistry;

pub struct GatewaySubAgentSpawner {
    registry: Arc<AgentRegistry>,
    harness: Arc<AgentHarness>,
    /// Pending background sub-agent handles, keyed by agent_id.
    /// Inserted on `spawn(background=true)`, removed on `collect_result`.
    pending_handles: RwLock<HashMap<String, AnonymousAgentHandle>>,
}

impl GatewaySubAgentSpawner {
    pub fn new(registry: Arc<AgentRegistry>, harness: Arc<AgentHarness>) -> Self {
        Self {
            registry,
            harness,
            pending_handles: RwLock::new(HashMap::new()),
        }
    }

    /// Resolve the parent agent from the registry.
    /// Returns the first enabled agent.
    async fn resolve_parent_agent_id(&self) -> Option<String> {
        for agent in self.registry.list().await {
            if agent.descriptor.enabled {
                return Some(agent.descriptor.agent_id.clone());
            }
        }
        None
    }

    /// Merge the user-supplied descriptor with the parent agent's defaults.
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

        if descriptor.provider.is_empty() {
            descriptor.provider = pd.provider.clone();
        }
        if descriptor.model.is_empty() {
            descriptor.model = pd.model.clone();
        }
        if descriptor.allowed_tools.is_none() {
            descriptor.allowed_tools = pd.allowed_tools.clone();
        }
        if descriptor.denied_tools.is_empty() {
            descriptor.denied_tools = pd.denied_tools.clone();
        }
        if descriptor.allowed_skills.is_none() {
            descriptor.allowed_skills = pd.allowed_skills.clone();
        }
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
        let parent_id = self
            .resolve_parent_agent_id()
            .await
            .ok_or_else(|| Error::ConfigInvalid {
                message: "SubAgentSpawner: no enabled parent agent found".to_owned(),
            })?;

        self.merge_with_parent(&parent_id, &mut descriptor).await?;

        let llm_provider = self
            .registry
            .get_llm_provider(&parent_id)
            .await
            .ok_or_else(|| Error::ConfigInvalid {
                message: format!(
                    "SubAgentSpawner: no LLM provider for agent '{parent_id}'"
                ),
            })?;

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
            // Store for later collection
            self.pending_handles
                .write()
                .await
                .insert(agent_id.clone(), handle);
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

    async fn collect_result(&self, agent_id: &str) -> AmanResult<SubAgentResult> {
        let handle = self
            .pending_handles
            .write()
            .await
            .remove(agent_id)
            .ok_or_else(|| Error::NotFound {
                name: format!(
                    "sub-agent '{agent_id}' not found in pending handles \
                     (already collected, never spawned, or expired)"
                ),
            })?;

        // Snapshot before wait() consumes the handle
        let sid = handle.session_id.clone();

        let reply = handle.wait().await?;

        Ok(SubAgentResult {
            agent_id: agent_id.to_owned(),
            session_id: sid,
            reply,
            background: false,
        })
    }
}
