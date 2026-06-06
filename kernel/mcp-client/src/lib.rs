// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! MCP (Model Context Protocol) client for aman.
//!
//! Connects to external MCP servers (stdio subprocess or streamable HTTP),
//! discovers their tools, and registers them into aman's [`ToolRegistry`]
//! so the LLM can discover and invoke them.
//!
//! # Architecture
//!
//! Each aman agent gets its own [`McpClientManager`] — agents never share
//! MCP connections. Server definitions are stored in JSON files:
//!
//! - **Global**: `~/.aman/mcp_servers.json` — shared across all agents
//! - **Per-agent**: `~/.aman/agents/{key}/mcp_servers.json` — agent-specific
//!
//! At startup, each agent merges global + its own definitions (per-agent
//! overrides global on name collision), connects to all auto-connect servers,
//! and registers discovered tools into the shared [`ToolRegistry`].
//!
//! # Tool naming
//!
//! MCP tools are registered with the prefix `mcp.{agent_key}.{server_name}.`:
//! e.g. `mcp.aman.filesystem.read_file`.

#![forbid(unsafe_code)]

pub mod client;
pub mod config;
pub mod error;
pub mod manager;
pub mod tool;

pub use client::{McpClientHandle, McpToolInfo};
pub use config::{McpServerConfig, McpServersFile};
pub use error::{McpError, McpResult};
pub use manager::{McpClientManager, McpServerStatus};
pub use tool::McpToolWrapper;
