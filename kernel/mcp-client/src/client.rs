// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! MCP client handle — wraps a single rmcp connection.

use std::collections::BTreeMap;

use rmcp::handler::client::ClientHandler;
use rmcp::model::{
    CallToolRequestParams, CallToolResult, ClientInfo, Implementation, PaginatedRequestParams,
};
use rmcp::service::{RoleClient, RunningService};
use rmcp::ServiceExt;
use serde_json::{json, Value};
use tracing::info;

use crate::error::{McpError, McpResult};

// ── Tool info ──────────────────────────────────────────────────────

/// Metadata about a tool discovered from an MCP server.
#[derive(Debug, Clone)]
pub struct McpToolInfo {
    /// The tool's name on the MCP server (without prefix).
    pub name: String,
    /// Human-readable description for the LLM.
    pub description: String,
    /// JSON Schema describing the tool's input parameters.
    pub input_schema: Value,
}

// ── Client handle ──────────────────────────────────────────────────

/// A connected MCP client handle wrapping an rmcp peer.
///
/// Created via [`McpClientHandle::connect_stdio`].
pub struct McpClientHandle {
    /// The server name (from config).
    pub server_name: String,
    /// The rmcp peer — supports `list_tools`, `call_tool`, `cancel`.
    peer: RunningService<RoleClient, AmanClientHandler>,
    /// Tools discovered at connect time.
    pub discovered_tools: Vec<McpToolInfo>,
}

impl McpClientHandle {
    // ── Stdio connection ───────────────────────────────────────────

    /// Connect to an MCP server via stdio (child process).
    ///
    /// Spawns `command` with `args` and communicates over stdin/stdout.
    pub async fn connect_stdio(
        name: &str,
        command: &str,
        args: &[String],
        env: &BTreeMap<String, String>,
    ) -> McpResult<Self> {
        use tokio::process::Command;

        let mut cmd = Command::new(command);
        cmd.args(args);
        cmd.env_clear();
        for (k, v) in env {
            cmd.env(k, v);
        }
        // Always set PATH so the child can find executables
        if !env.contains_key("PATH") {
            if let Ok(path) = std::env::var("PATH") {
                cmd.env("PATH", path);
            }
        }

        info!(server = name, command, ?args, "connecting to MCP server via stdio");

        let transport = rmcp::transport::TokioChildProcess::new(cmd).map_err(|e| {
            McpError::ConnectionFailed {
                server: name.to_string(),
                detail: format!("failed to build transport: {e}"),
            }
        })?;

        let peer = AmanClientHandler
            .serve(transport)
            .await
            .map_err(|e| McpError::ConnectionFailed {
                server: name.to_string(),
                detail: format!("serve failed: {e}"),
            })?;

        let mut handle = Self {
            server_name: name.to_string(),
            peer,
            discovered_tools: Vec::new(),
        };

        handle.discovered_tools = handle.discover_tools().await?;
        info!(
            server = name,
            tool_count = handle.discovered_tools.len(),
            "MCP server connected"
        );

        Ok(handle)
    }

    // ── HTTP connection ────────────────────────────────────────────

    /// Connect to an MCP server via streamable HTTP.
    ///
    /// Uses rmcp's reqwest-based HTTP transport. Suitable for remote MCP
    /// servers or local servers that expose an HTTP endpoint.
    pub async fn connect_http(
        name: &str,
        url: &str,
        headers: &BTreeMap<String, String>,
    ) -> McpResult<Self> {
        use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;

        info!(server = name, url, "connecting to MCP server via streamable HTTP");

        let transport = if headers.is_empty() {
            rmcp::transport::StreamableHttpClientTransport::from_uri(url)
        } else {
            let mut http_headers = std::collections::HashMap::new();
            for (k, v) in headers {
                if let (Ok(name), Ok(value)) = (
                    http::HeaderName::from_bytes(k.as_bytes()),
                    http::HeaderValue::from_str(v),
                ) {
                    http_headers.insert(name, value);
                }
            }
            let config = StreamableHttpClientTransportConfig::with_uri(url)
                .custom_headers(http_headers);
            rmcp::transport::StreamableHttpClientTransport::from_config(config)
        };

        let peer = AmanClientHandler
            .serve(transport)
            .await
            .map_err(|e| McpError::ConnectionFailed {
                server: name.to_string(),
                detail: format!("HTTP serve failed: {e}"),
            })?;

        let mut handle = Self {
            server_name: name.to_string(),
            peer,
            discovered_tools: Vec::new(),
        };

        handle.discovered_tools = handle.discover_tools().await?;
        info!(
            server = name,
            tool_count = handle.discovered_tools.len(),
            "MCP server connected via HTTP"
        );

        Ok(handle)
    }

    // ── Tools ──────────────────────────────────────────────────────

    /// Discover the tools exposed by this MCP server.
    async fn discover_tools(&self) -> McpResult<Vec<McpToolInfo>> {
        let result = self
            .peer
            .list_tools(Some(PaginatedRequestParams::default()))
            .await
            .map_err(|e| McpError::TransportError {
                server: self.server_name.clone(),
                detail: format!("list_tools failed: {e}"),
            })?;

        Ok(result
            .tools
            .into_iter()
            .map(|t| McpToolInfo {
                name: t.name.to_string(),
                description: t.description.map_or_else(String::new, |d| d.to_string()),
                input_schema: serde_json::to_value(&*t.input_schema).unwrap_or(json!({})),
            })
            .collect())
    }

    /// Call a tool on this MCP server.
    pub async fn call_tool(&self, tool_name: &str, args: Value) -> McpResult<Value> {
        let arguments = match args {
            Value::Object(map) => Some(
                map.into_iter()
                    .collect::<serde_json::Map<String, Value>>(),
            ),
            _ => None,
        };

        let mut params = CallToolRequestParams::new(tool_name.to_string());
        if let Some(args) = arguments {
            params = params.with_arguments(args);
        }

        let result: CallToolResult = self.peer.call_tool(params).await.map_err(|e| {
            McpError::ToolCallFailed {
                tool: tool_name.to_string(),
                server: self.server_name.clone(),
                detail: format!("{e}"),
            }
        })?;

        // Extract text content from the result.
        let output = extract_text_from_content(&result.content);

        if let Some(structured) = result.structured_content {
            Ok(json!({
                "content": output,
                "data": structured,
                "is_error": result.is_error.unwrap_or(false),
            }))
        } else {
            Ok(json!({
                "content": output,
                "is_error": result.is_error.unwrap_or(false),
            }))
        }
    }

    // ── Lifecycle ──────────────────────────────────────────────────

    /// Cancel and disconnect from the MCP server.
    pub async fn cancel(self) {
        info!(server = %self.server_name, "cancelling MCP server connection");
        let _ = self.peer.cancel().await;
    }
}

// ── Client handler ─────────────────────────────────────────────────

/// Minimal MCP client handler for aman.
///
/// Uses the default implementations for all `ClientHandler` methods
/// except `get_info`.
#[derive(Debug, Clone)]
struct AmanClientHandler;

impl ClientHandler for AmanClientHandler {
    fn get_info(&self) -> ClientInfo {
        let mut info = ClientInfo::default();
        info.client_info = Implementation::new("aman", "0.1.0");
        info
    }
}

// ── Helpers ────────────────────────────────────────────────────────

/// Extract the primary text content from a vector of MCP content blocks.
fn extract_text_from_content(content: &[rmcp::model::Content]) -> String {
    content
        .iter()
        .filter_map(|block| {
            let v = serde_json::to_value(block).ok()?;

            if let Some(t) = v.get("text").and_then(|v| v.as_str()) {
                return Some(t.to_owned());
            }
            if let Some(t) = v.get("data").and_then(|v| v.as_str()) {
                return Some(t.to_owned());
            }

            Some(
                serde_json::to_string(&v)
                    .unwrap_or_else(|_| format!("{block:?}")),
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}
