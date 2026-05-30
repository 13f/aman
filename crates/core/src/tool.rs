// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

use crate::context::ToolContext;
use crate::error::AmanResult;
use crate::schema::JsonSchema;
use crate::types::{ExecutionModel, ToolMode};
use async_trait::async_trait;
use serde_json::Value;

pub type ToolResult = AmanResult<Value>;

#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn mode(&self) -> ToolMode;
    /// Human-readable description of what this tool does.
    /// Used when building tool schemas for the LLM.
    fn description(&self) -> &str { "" }
    fn parameters(&self) -> &JsonSchema;
    fn returns(&self) -> &JsonSchema;
    /// How this tool's calls should be scheduled relative to each other.
    ///
    /// Default is [`ExecutionModel::Independent`] — most tools are read-only
    /// and can run concurrently. Override for stateful or side-effect tools.
    fn execution_model(&self) -> ExecutionModel { ExecutionModel::Independent }

    async fn execute(&self, params: Value, ctx: ToolContext) -> ToolResult;
}
