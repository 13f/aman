// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! MCP server manager — read/write JSON config files directly.
//!
//! MCP server definitions are stored in `mcp_servers.json` files,
//! independent from `config.yaml` (same pattern as `cards.json`):
//!
//! - **Global**: `~/.aman/mcp_servers.json`
//! - **Per-agent**: `~/.aman/agents/{key}/mcp_servers.json`

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

use crate::models::McpServerEntry;

// ── Types ──────────────────────────────────────────────────────────

/// A single MCP server definition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerConfig {
    pub name: String,
    #[serde(default = "default_transport")]
    pub transport: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub headers: BTreeMap<String, String>,
    #[serde(default = "default_true")]
    pub auto_connect: bool,
}

/// Top-level format of a `mcp_servers.json` file.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct McpServersFile {
    #[serde(default)]
    servers: Vec<McpServerConfig>,
}

fn default_transport() -> String {
    "auto".to_string()
}

fn default_true() -> bool {
    true
}

// ── Paths ──────────────────────────────────────────────────────────

fn aman_data_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    PathBuf::from(home).join(".aman")
}

fn global_path() -> PathBuf {
    aman_data_dir().join("mcp_servers.json")
}

fn agent_path(agent_key: &str) -> PathBuf {
    aman_data_dir().join("agents").join(agent_key).join("mcp_servers.json")
}

// ── Load / Save ────────────────────────────────────────────────────

fn load_file(path: &PathBuf) -> Result<McpServersFile, String> {
    if !path.exists() {
        return Ok(McpServersFile::default());
    }
    let raw = std::fs::read_to_string(path)
        .map_err(|e| format!("读取 {} 失败: {e}", path.display()))?;
    if raw.trim().is_empty() {
        return Ok(McpServersFile::default());
    }
    serde_json::from_str(&raw)
        .map_err(|e| format!("解析 {} 失败: {e}", path.display()))
}

fn save_file(file: &McpServersFile, path: &PathBuf) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("创建目录失败: {e}"))?;
    }
    let json = serde_json::to_string_pretty(file)
        .map_err(|e| format!("序列化失败: {e}"))?;
    std::fs::write(path, json)
        .map_err(|e| format!("写入 {} 失败: {e}", path.display()))
}

// ── Public API ─────────────────────────────────────────────────────

/// Load all MCP server definitions (global + all per-agent).
pub fn load_all_mcp_servers() -> Result<Vec<McpServerEntry>, String> {
    let mut entries = Vec::new();

    // 1. Global
    let global = load_file(&global_path())?;
    for s in &global.servers {
        entries.push(config_to_entry(s, "global"));
    }

    // 2. Per-agent
    let agents_dir = aman_data_dir().join("agents");
    if let Ok(dir_entries) = std::fs::read_dir(&agents_dir) {
        for entry in dir_entries.flatten() {
            if !entry.file_type().is_ok_and(|t| t.is_dir()) {
                continue;
            }
            let agent_key = entry.file_name().to_string_lossy().to_string();
            let agent_file = agent_path(&agent_key);
            if let Ok(file) = load_file(&agent_file) {
                for s in &file.servers {
                    entries.push(config_to_entry(s, &agent_key));
                }
            }
        }
    }

    Ok(entries)
}

/// Add an MCP server definition.
///
/// If `agent_key` is None, write to the global file.
/// Otherwise write to the specified agent's file.
pub fn add_mcp_server(
    config: McpServerConfig,
    agent_key: Option<&str>,
) -> Result<(), String> {
    match agent_key {
        None => {
            let mut file = load_file(&global_path())?;
            if file.servers.iter().any(|s| s.name == config.name) {
                return Err(format!("MCP server '{}' 已存在", config.name));
            }
            file.servers.push(config);
            save_file(&file, &global_path())
        }
        Some(key) => {
            let path = agent_path(key);
            let mut file = load_file(&path)?;
            if file.servers.iter().any(|s| s.name == config.name) {
                return Err(format!("MCP server '{}' 已存在于 agent '{key}'", config.name));
            }
            file.servers.push(config);
            save_file(&file, &path)
        }
    }
}

/// Remove an MCP server definition.
pub fn remove_mcp_server(name: &str, agent_key: Option<&str>) -> Result<(), String> {
    match agent_key {
        None => {
            let path = global_path();
            let mut file = load_file(&path)?;
            let len_before = file.servers.len();
            file.servers.retain(|s| s.name != name);
            if file.servers.len() == len_before {
                return Err(format!("MCP server '{name}' 不存在"));
            }
            save_file(&file, &path)
        }
        Some(key) => {
            let path = agent_path(key);
            let mut file = load_file(&path)?;
            let len_before = file.servers.len();
            file.servers.retain(|s| s.name != name);
            if file.servers.len() == len_before {
                return Err(format!("MCP server '{name}' 不存在于 agent '{key}'"));
            }
            save_file(&file, &path)
        }
    }
}

// ── Helpers ────────────────────────────────────────────────────────

fn config_to_entry(config: &McpServerConfig, source: &str) -> McpServerEntry {
    McpServerEntry {
        name: config.name.clone(),
        transport: config.transport.clone(),
        command: config.command.clone(),
        args: config.args.clone(),
        url: config.url.clone(),
        env: config.env.clone(),
        headers: config.headers.clone(),
        auto_connect: config.auto_connect,
        source: source.to_string(),
        connected: false,
        tool_count: 0,
        error: None,
    }
}
