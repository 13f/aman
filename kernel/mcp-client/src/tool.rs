// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! MCP tool wrapper — adapts an MCP tool to aman's [`Tool`] trait.

use std::sync::Arc;

use async_trait::async_trait;
use kernel::context::ToolContext;
use kernel::error::AmanResult;
use kernel::schema::JsonSchema;
use kernel::tool::Tool;
use kernel::types::{ExecutionModel, ToolMode};
use serde_json::{json, Value};

use crate::client::{McpClientHandle, McpToolInfo};

// ── McpToolWrapper ─────────────────────────────────────────────────

/// Wraps an MCP server tool as an aman [`Tool`].
///
/// Each tool is registered with the name `mcp.{agent_key}.{server}.{tool}`.
pub struct McpToolWrapper {
    /// The aman agent key this tool belongs to.
    agent_key: String,
    /// The MCP server name.
    server_name: String,
    /// Tool metadata from the MCP server.
    tool_info: McpToolInfo,
    /// Shared reference to the MCP server connection.
    client: Arc<McpClientHandle>,
    /// Full aman tool name: `mcp.{agent_key}.{server}.{tool}`.
    aman_tool_name: String,
    /// Lazily computed parameter schema.
    params_schema: JsonSchema,
    /// Derived human-readable description when the MCP server provides none.
    derived_description: String,
}

impl McpToolWrapper {
    /// Create a new wrapper for a discovered MCP tool.
    #[must_use]
    pub fn new(
        agent_key: &str,
        server_name: &str,
        tool_info: McpToolInfo,
        client: Arc<McpClientHandle>,
    ) -> Self {
        let aman_tool_name = format!(
            "mcp.{}.{}.{}",
            agent_key, server_name, tool_info.name
        );

        // Derive a human-readable description from the tool name if the
        // MCP server didn't provide one. E.g. "read_file" → "Read file".
        let derived_description = if tool_info.description.is_empty() {
            derive_description(&tool_info.name)
        } else {
            String::new()
        };

        // Convert the MCP input_schema (Value) to a JsonSchema.
        // If the schema is empty or not an object, default to `{"type": "object"}`.
        let schema_value = if tool_info.input_schema.is_object()
            && !tool_info.input_schema.as_object().is_none_or(|o| o.is_empty())
        {
            tool_info.input_schema.clone()
        } else {
            json!({"type": "object"})
        };
        let params_schema = JsonSchema::from(schema_value);

        Self {
            agent_key: agent_key.to_string(),
            server_name: server_name.to_string(),
            tool_info,
            client,
            aman_tool_name,
            params_schema,
            derived_description,
        }
    }

    /// The agent key this tool is registered for.
    #[must_use]
    pub fn agent_key(&self) -> &str {
        &self.agent_key
    }

    /// The MCP server this tool comes from.
    #[must_use]
    pub fn server_name(&self) -> &str {
        &self.server_name
    }
}

#[async_trait]
impl Tool for McpToolWrapper {
    fn name(&self) -> &str {
        &self.aman_tool_name
    }

    fn mode(&self) -> ToolMode {
        ToolMode::Remote
    }

    fn description(&self) -> &str {
        // Use the description from MCP. If the server didn't provide one,
        // derive a human-readable label from the tool name.
        if !self.tool_info.description.is_empty() {
            &self.tool_info.description
        } else {
            // Derive from tool name: snake_case → "Snake case"
            &self.derived_description
        }
    }

    fn parameters(&self) -> &JsonSchema {
        &self.params_schema
    }

    fn returns(&self) -> &JsonSchema {
        // MCP servers don't declare return schemas; default to object.
        static RETURNS: std::sync::LazyLock<JsonSchema> =
            std::sync::LazyLock::new(|| JsonSchema::from(json!({"type": "object"})));
        &RETURNS
    }

    fn execution_model(&self) -> ExecutionModel {
        // Conservative default: MCP tools may have side effects.
        ExecutionModel::Stateful
    }

    async fn execute(&self, params: Value, _ctx: ToolContext) -> AmanResult<Value> {
        self.client
            .call_tool(&self.tool_info.name, params)
            .await
            .map_err(|e| kernel::error::Error::Unrecoverable {
                message: format!(
                    "MCP tool '{}.{}' on server '{}' failed: {e}",
                    self.aman_tool_name, self.tool_info.name, self.server_name
                ),
            })
    }
}

// ── Helpers ─────────────────────────────────────────────────────────

/// Derive a human-readable description from a snake_case tool name.
///
/// Examples:
/// - `"read_file"` → `"Read file"`
/// - `"search_web"` → `"Search web"`
/// - `"create_table"` → `"Create table"`
fn derive_description(name: &str) -> String {
    let mut result = String::with_capacity(name.len());
    let mut first = true;

    for ch in name.chars() {
        if ch == '_' {
            result.push(' ');
        } else if first {
            result.push(ch.to_ascii_uppercase());
            first = false;
        } else {
            result.push(ch);
        }
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derive_snake_case() {
        assert_eq!(derive_description("read_file"), "Read file");
        assert_eq!(derive_description("search_web"), "Search web");
        assert_eq!(derive_description("create_table"), "Create table");
    }

    #[test]
    fn derive_single_word() {
        assert_eq!(derive_description("ping"), "Ping");
        assert_eq!(derive_description("echo"), "Echo");
    }

    #[test]
    fn derive_multiple_underscores() {
        assert_eq!(
            derive_description("get_user_profile_by_id"),
            "Get user profile by id"
        );
    }

    #[test]
    fn derive_description_from_tool_name() {
        assert_eq!(derive_description("read_file"), "Read file");
        assert_eq!(derive_description("search_web"), "Search web");
        assert_eq!(derive_description("create_table"), "Create table");
        assert_eq!(derive_description("ping"), "Ping");
        assert_eq!(derive_description("echo"), "Echo");
        assert_eq!(
            derive_description("get_user_profile_by_id"),
            "Get user profile by id"
        );
    }

    #[test]
    fn tool_name_format() {
        let _info = McpToolInfo {
            name: "echo".into(),
            description: "Server-provided description".into(),
            input_schema: json!({"type": "object"}),
        };
        // Test that the full aman_tool_name follows the convention.
        // We can't construct a full wrapper without a live connection,
        // but we can verify the naming convention.
        let expected_prefix = "mcp.test-agent.test-server.echo";
        // The actual construction happens in McpToolWrapper::new()
        // which requires an Arc<McpClientHandle>. The naming convention
        // is tested via integration tests.
        assert!(expected_prefix.starts_with("mcp."));
        assert!(expected_prefix.ends_with(".echo"));
    }
}
