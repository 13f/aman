// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

use crate::context::PluginContext;
use crate::error::AmanResult;
use crate::hook::Hook;
use crate::memory::MemoryProvider;
use crate::skill::Skill;
use crate::source::EventSource;
use crate::tool::Tool;
use async_trait::async_trait;
use semver::Version;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Arc;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginDependency {
    pub name: String,
    pub version_range: String,
}

#[async_trait]
pub trait Plugin: Send + Sync {
    fn name(&self) -> &str;
    fn version(&self) -> &Version;
    fn dependencies(&self) -> &[PluginDependency];

    async fn on_load(&mut self, ctx: PluginContext) -> AmanResult<()>;
    async fn on_unload(&mut self) -> AmanResult<()>;
    async fn on_dependency_unloading(&self, dep_name: &str) -> AmanResult<()>;

    fn event_sources(&self) -> Vec<Arc<dyn EventSource>>;
    fn skills(&self) -> Vec<Arc<dyn Skill>>;
    fn tools(&self) -> Vec<Arc<dyn Tool>>;
    fn hooks(&self) -> Vec<Arc<dyn Hook>> {
        vec![]
    }
    fn memory_providers(&self) -> Vec<Arc<dyn MemoryProvider>> {
        vec![]
    }

    /// Plugin-contributed HTTP routes. Merged under `/api/v1` by AgentRuntime.
    fn routes(&self) -> Option<axum::Router<()>> {
        None
    }
}

/// Handler for JSON-RPC method calls from subprocess plugins.
///
/// Implemented by the host runtime (AgentRuntime) to give subprocess plugins
/// access to aman services: EventBus, AgentRegistry, WorkflowEngine, etc.
#[async_trait]
pub trait JsonRpcMethodHandler: Send + Sync {
    /// Handle a JSON-RPC method call from a subprocess plugin.
    /// Returns the JSON result or an error.
    async fn handle_method(
        &self,
        plugin_name: &str,
        method: &str,
        params: Value,
    ) -> AmanResult<Value>;
}

/// A no-op handler used when no host runtime is available (e.g., tests).
pub struct NoopJsonRpcHandler;

#[async_trait]
impl JsonRpcMethodHandler for NoopJsonRpcHandler {
    async fn handle_method(&self, _plugin_name: &str, method: &str, _params: Value) -> AmanResult<Value> {
        Err(crate::Error::Unrecoverable {
            message: format!("no json-rpc handler available for method: {method}"),
        })
    }
}
