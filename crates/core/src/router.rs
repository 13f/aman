// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

use crate::agent::AgentInstance;
use async_trait::async_trait;

/// Strategy for routing incoming messages to the appropriate agent.
///
/// The default implementation selects the first enabled agent.
/// Custom implementations can route based on message content,
/// sender identity, round-robin, load balancing, etc.
#[async_trait]
pub trait AgentRouter: Send + Sync {
    /// Select an agent from the available list for the given user message.
    async fn route(&self, user_text: &str, agents: &[AgentInstance]) -> Option<AgentInstance>;
}
