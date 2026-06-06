// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! Per-agent MCP client manager.
//!
//! Each aman agent gets its own [`McpClientManager`], which holds
//! connections to the agent's MCP servers (global + per-agent config)
//! and registers their tools into the shared [`ToolRegistry`].

use std::collections::HashMap;
use std::sync::Arc;

use serde::Serialize;
use tokio::sync::RwLock;
use tracing::{info, warn};

use crate::client::McpClientHandle;
use crate::config::{McpServerConfig, McpServersFile};
use crate::error::{McpError, McpResult};
use crate::tool::McpToolWrapper;
use kernel::tool::Tool;

// ── Status types ───────────────────────────────────────────────────

/// Runtime status of a single MCP server for an agent.
#[derive(Debug, Clone, Serialize)]
pub struct McpServerStatus {
    /// Server name from config.
    pub name: String,
    /// Transport type.
    pub transport: String,
    /// Whether the server is currently connected.
    pub connected: bool,
    /// Number of tools registered from this server.
    pub tool_count: usize,
    /// Whether auto-connect is enabled.
    pub auto_connect: bool,
    /// Last error message, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

// ── Manager ────────────────────────────────────────────────────────

/// Per-agent MCP connection pool.
///
/// Manages the lifecycle of MCP server connections for a single agent:
/// connect, disconnect, tool discovery, and tool registration.
pub struct McpClientManager {
    /// The agent this manager belongs to.
    agent_key: String,
    /// Connected servers, keyed by server name.
    servers: RwLock<HashMap<String, Arc<McpClientHandle>>>,
    /// Tool names registered per server (for disconnect cleanup).
    registered_tools: RwLock<HashMap<String, Vec<String>>>,
    /// The shared tool registry.
    tools: Arc<tool::ToolRegistry>,
}

impl McpClientManager {
    /// Create a new MCP manager for an agent.
    #[must_use]
    pub fn new(agent_key: String, tools: Arc<tool::ToolRegistry>) -> Self {
        Self {
            agent_key,
            servers: RwLock::new(HashMap::new()),
            registered_tools: RwLock::new(HashMap::new()),
            tools,
        }
    }

    /// The agent key this manager belongs to.
    #[must_use]
    pub fn agent_key(&self) -> &str {
        &self.agent_key
    }

    /// The shared tool registry.
    #[must_use]
    pub fn tools(&self) -> &Arc<tool::ToolRegistry> {
        &self.tools
    }

    // ── Connect / disconnect ───────────────────────────────────────

    /// Connect to an MCP server and register its tools.
    ///
    /// If already connected to a server with this name, returns
    /// [`McpError::AlreadyConnected`].
    ///
    /// Currently only supports `"stdio"` transport.
    /// `"streamable-http"` will be added in a follow-up.
    pub async fn connect(&self, config: &McpServerConfig) -> McpResult<()> {
        // Check for duplicate.
        {
            let servers = self.servers.read().await;
            if servers.contains_key(&config.name) {
                return Err(McpError::AlreadyConnected {
                    server: config.name.clone(),
                });
            }
        }

        let transport = config.resolve_transport().ok_or_else(|| {
            McpError::ConnectionFailed {
                server: config.name.clone(),
                detail: "cannot determine transport — set 'command' for local subprocess or 'url' for remote HTTP".to_string(),
            }
        })?;

        let handle = match transport {
            "stdio" => {
                let command = config.command.as_ref().ok_or_else(|| {
                    McpError::ConnectionFailed {
                        server: config.name.clone(),
                        detail: "stdio transport requires 'command'".to_string(),
                    }
                })?;
                McpClientHandle::connect_stdio(
                    &config.name,
                    command,
                    &config.args,
                    &config.env,
                )
                .await?
            }
            "streamable-http" => {
                let url = config.url.as_ref().ok_or_else(|| {
                    McpError::ConnectionFailed {
                        server: config.name.clone(),
                        detail: "streamable-http transport requires 'url'".to_string(),
                    }
                })?;
                McpClientHandle::connect_http(
                    &config.name,
                    url,
                    &config.headers,
                )
                .await?
            }
            other => {
                return Err(McpError::ConnectionFailed {
                    server: config.name.clone(),
                    detail: format!(
                        "unsupported transport '{other}' — expected 'stdio' or 'streamable-http'"
                    ),
                });
            }
        };

        let tools = handle.discovered_tools.clone();
        let handle = Arc::new(handle);
        let server_name = config.name.clone();
        let agent_key = self.agent_key.clone();

        // Register tools.
        let mut tool_names = Vec::new();
        for tool_info in &tools {
            let wrapper = McpToolWrapper::new(
                &agent_key,
                &server_name,
                tool_info.clone(),
                Arc::clone(&handle),
            );
            let name = wrapper.name().to_owned();
            if let Err(e) = self.tools.register(Arc::new(wrapper)) {
                warn!(
                    tool = name,
                    error = %e,
                    "failed to register MCP tool (may already exist)"
                );
            } else {
                tool_names.push(name);
            }
        }

        // Store.
        self.servers.write().await.insert(server_name.clone(), handle);
        self.registered_tools
            .write()
            .await
            .insert(server_name.clone(), tool_names);

        info!(
            agent = %self.agent_key,
            server = %server_name,
            tool_count = tools.len(),
            "MCP server connected and tools registered"
        );

        Ok(())
    }

    /// Disconnect from an MCP server and unregister its tools.
    pub async fn disconnect(&self, server_name: &str) -> McpResult<()> {
        // Unregister tools first.
        if let Some(tool_names) = self.registered_tools.write().await.remove(server_name) {
            for name in &tool_names {
                let _ = self.tools.unregister(name);
            }
            info!(
                agent = %self.agent_key,
                server = %server_name,
                tool_count = tool_names.len(),
                "MCP tools unregistered"
            );
        }

        // Cancel connection.
        if let Some(handle) = self.servers.write().await.remove(server_name) {
            if let Ok(handle) = Arc::try_unwrap(handle) {
                handle.cancel().await;
            }
            info!(agent = %self.agent_key, server = %server_name, "MCP server disconnected");
        } else {
            return Err(McpError::ServerNotFound {
                server: server_name.to_string(),
            });
        }

        Ok(())
    }

    // ── Bulk operations ────────────────────────────────────────────

    /// Load config from global + per-agent JSON files, then connect all
    /// servers marked `auto_connect: true`.
    pub async fn connect_all_from_config(&self) {
        let merged = match Self::load_merged_config(&self.agent_key) {
            Ok(cfgs) => cfgs,
            Err(e) => {
                warn!(agent = %self.agent_key, error = %e, "failed to load MCP config");
                return;
            }
        };

        let to_connect: Vec<_> = merged.iter().filter(|c| c.auto_connect).cloned().collect();

        if to_connect.is_empty() {
            return;
        }

        info!(
            agent = %self.agent_key,
            count = to_connect.len(),
            "auto-connecting MCP servers"
        );

        for config in &to_connect {
            if let Err(e) = self.connect(config).await {
                warn!(
                    agent = %self.agent_key,
                    server = %config.name,
                    error = %e,
                    "auto-connect failed for MCP server"
                );
            }
        }
    }

    /// Disconnect all servers and unregister all tools.
    pub async fn disconnect_all(&self) {
        let server_names: Vec<String> = {
            self.servers.read().await.keys().cloned().collect()
        };

        for name in &server_names {
            let _ = self.disconnect(name).await;
        }

        info!(agent = %self.agent_key, "all MCP servers disconnected");
    }

    // ── Status ─────────────────────────────────────────────────────

    /// Return the runtime status of all connected servers.
    pub async fn list_servers(&self) -> Vec<McpServerStatus> {
        let servers = self.servers.read().await;
        let registered = self.registered_tools.read().await;

        servers
            .iter()
            .map(|(name, _handle)| {
                let tool_count = registered.get(name).map_or(0, |v| v.len());
                McpServerStatus {
                    name: name.clone(),
                    transport: String::new(),
                    connected: true,
                    tool_count,
                    auto_connect: true,
                    error: None,
                }
            })
            .collect()
    }

    /// Return the merged list of all server configs (connected or not).
    #[must_use]
    pub fn load_merged_config(agent_key: &str) -> Result<Vec<McpServerConfig>, String> {
        let global = McpServersFile::load_global()?
            .unwrap_or_default()
            .servers;
        let agent = McpServersFile::load_for_agent(agent_key)?
            .unwrap_or_default()
            .servers;
        Ok(McpServersFile::merge(&global, &agent))
    }
}

impl Drop for McpClientManager {
    fn drop(&mut self) {
        info!(agent = %self.agent_key, "McpClientManager dropped");
    }
}
