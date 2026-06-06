// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! MCP server configuration and JSON file I/O.
//!
//! Stores server definitions in `mcp_servers.json` files — independent from
//! `config.yaml` (same pattern as `cards.json`):
//!
//! - **Global**: `~/.aman/mcp_servers.json`
//! - **Per-agent**: `~/.aman/agents/{key}/mcp_servers.json`

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

// ── Server definition ──────────────────────────────────────────────

/// A single MCP server definition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerConfig {
    /// Unique name for this server connection.
    pub name: String,

    /// Transport type: `"auto"`, `"stdio"`, or `"streamable-http"`.
    ///
    /// When `"auto"` (the default), the transport is inferred:
    /// - `command` is set → `"stdio"` (local subprocess)
    /// - `url` is set → `"streamable-http"` (remote HTTP)
    /// - both set → `"stdio"` wins (local takes precedence)
    /// - neither set → error at connect time
    #[serde(default = "default_transport")]
    pub transport: String,

    /// For stdio transport: the command to spawn (e.g. `"npx"`, `"uvx"`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,

    /// For stdio transport: arguments to the command.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,

    /// For streamable-http transport: the base URL of the MCP server.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,

    /// Optional environment variables for the subprocess (stdio only).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,

    /// Optional HTTP headers for streamable-http transport.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub headers: BTreeMap<String, String>,

    /// Connect automatically on agent startup.
    #[serde(default = "default_true")]
    pub auto_connect: bool,
}

fn default_transport() -> String {
    "auto".to_string()
}

fn default_true() -> bool {
    true
}

impl McpServerConfig {
    /// Resolve the effective transport for this server.
    ///
    /// When `transport` is `"auto"`:
    /// - `command` is set → stdio (local subprocess)
    /// - `url` is set → streamable-http (remote)
    /// - both set → stdio wins (local takes precedence)
    /// - neither set → returns `None` (error at connect time)
    ///
    /// When `transport` is explicitly `"stdio"` or `"streamable-http"`,
    /// returns that value regardless of which fields are set.
    #[must_use]
    pub fn resolve_transport(&self) -> Option<&str> {
        match self.transport.as_str() {
            "stdio" => Some("stdio"),
            "streamable-http" => Some("streamable-http"),
            "auto" | _ => {
                let has_command = self.command.as_ref().is_some_and(|c| !c.trim().is_empty());
                let has_url = self.url.as_ref().is_some_and(|u| !u.trim().is_empty());

                if has_command {
                    Some("stdio")
                } else if has_url {
                    Some("streamable-http")
                } else {
                    None
                }
            }
        }
    }
}

// ── JSON file format ───────────────────────────────────────────────

/// Top-level format of a `mcp_servers.json` file.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct McpServersFile {
    #[serde(default)]
    pub servers: Vec<McpServerConfig>,
}

impl McpServersFile {
    // ── Paths ──

    /// Path to the global servers file: `~/.aman/mcp_servers.json`.
    #[must_use]
    pub fn global_path() -> PathBuf {
        aman_data_dir().join("mcp_servers.json")
    }

    /// Path to a per-agent servers file: `~/.aman/agents/{key}/mcp_servers.json`.
    #[must_use]
    pub fn agent_path(agent_key: &str) -> PathBuf {
        aman_data_dir().join("agents").join(agent_key).join("mcp_servers.json")
    }

    // ── Load ──

    /// Load the global servers file.
    ///
    /// Returns `Ok(None)` if the file does not exist (first run).
    pub fn load_global() -> Result<Option<Self>, String> {
        Self::load_file(&Self::global_path())
    }

    /// Load a per-agent servers file.
    ///
    /// Returns `Ok(None)` if the file does not exist.
    pub fn load_for_agent(agent_key: &str) -> Result<Option<Self>, String> {
        Self::load_file(&Self::agent_path(agent_key))
    }

    fn load_file(path: &PathBuf) -> Result<Option<Self>, String> {
        if !path.exists() {
            return Ok(None);
        }
        let raw = std::fs::read_to_string(path)
            .map_err(|e| format!("读取 {} 失败: {e}", path.display()))?;
        if raw.trim().is_empty() {
            return Ok(Some(Self::default()));
        }
        serde_json::from_str(&raw)
            .map(Some)
            .map_err(|e| format!("解析 {} 失败: {e}", path.display()))
    }

    // ── Save ──

    /// Persist to the global servers file.
    pub fn save_global(&self) -> Result<(), String> {
        self.save_file(&Self::global_path())
    }

    /// Persist to a per-agent servers file.
    pub fn save_for_agent(&self, agent_key: &str) -> Result<(), String> {
        let path = Self::agent_path(agent_key);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("创建 agent 目录失败: {e}"))?;
        }
        self.save_file(&path)
    }

    fn save_file(&self, path: &PathBuf) -> Result<(), String> {
        let json = serde_json::to_string_pretty(self)
            .map_err(|e| format!("序列化 MCP servers 失败: {e}"))?;
        std::fs::write(path, json)
            .map_err(|e| format!("写入 {} 失败: {e}", path.display()))
    }

    // ── Merge ──

    /// Merge global and per-agent server definitions.
    ///
    /// Per-agent entries with the same `name` override global ones
    /// (so an agent can pin a specific version or add extra args).
    /// Per-agent-only entries are appended.
    #[must_use]
    pub fn merge(global: &[McpServerConfig], agent: &[McpServerConfig]) -> Vec<McpServerConfig> {
        let mut merged: BTreeMap<String, McpServerConfig> = BTreeMap::new();

        for s in global {
            merged.insert(s.name.clone(), s.clone());
        }
        // Per-agent overrides global on name collision.
        for s in agent {
            merged.insert(s.name.clone(), s.clone());
        }

        merged.into_values().collect()
    }
}

// ── Helpers ────────────────────────────────────────────────────────

/// The aman data directory: `~/.aman/`.
fn aman_data_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    PathBuf::from(home).join(".aman")
}
