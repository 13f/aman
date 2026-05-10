use crate::context::ToolContext;
use crate::error::AmanResult;
use crate::schema::JsonSchema;
use crate::types::ToolMode;
use async_trait::async_trait;
use serde_json::Value;

pub type ToolResult = AmanResult<Value>;

#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn mode(&self) -> ToolMode;
    fn parameters(&self) -> &JsonSchema;
    fn returns(&self) -> &JsonSchema;

    async fn execute(&self, params: Value, ctx: ToolContext) -> ToolResult;
}
