// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! Sub-agent spawning abstraction.
//!
//! The [`SubAgentSpawner`] trait decouples the cognitive engine from the
//! gateway's concrete agent-harness infrastructure.  Tools (like
//! `delegate_task`) depend on this trait; the gateway provides the
//! implementation that actually creates and runs anonymous agents.

use async_trait::async_trait;
use kernel::agent::AgentDescriptor;
use kernel::react::SoulSnapshot;
use kernel::AmanResult;
use serde::{Deserialize, Serialize};

/// Result returned by [`SubAgentSpawner::spawn`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubAgentResult {
    /// The anonymous agent's unique id (format: `anon-{uuid}`).
    pub agent_id: String,
    /// Session id for this execution.
    pub session_id: String,
    /// The sub-agent's final reply text.
    /// Empty when `background` is true.
    pub reply: String,
    /// Whether this was a background (fire-and-forget) spawn.
    pub background: bool,
}

/// Trait for spawning anonymous, ephemeral sub-agents.
///
/// Implementations are responsible for:
/// - Resolving the LLM provider and model
/// - Inheriting tool policy from the parent agent
/// - Creating and running the anonymous agent
/// - Returning the result
#[async_trait]
pub trait SubAgentSpawner: Send + Sync {
    /// Spawn an anonymous sub-agent with the given descriptor, soul, and
    /// task prompt.
    ///
    /// When `background` is false, blocks until the sub-agent completes
    /// and returns the full reply.
    ///
    /// When `background` is true, returns immediately with the agent's
    /// metadata so the caller can collect results later.
    async fn spawn(
        &self,
        descriptor: AgentDescriptor,
        soul_snapshot: SoulSnapshot,
        prompt: String,
        background: bool,
    ) -> AmanResult<SubAgentResult>;
}
