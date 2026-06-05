// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! Agent filesystem operations.
//!
//! Manages the `~/.aman/agents/` directory tree — each agent has a
//! subdirectory containing `SOUL.md`, `memory/`, and `sessions/`.

use std::fs;
use std::path::PathBuf;

/// The base directory for all agent data.
#[must_use]
pub fn agents_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    PathBuf::from(home).join(".aman").join("agents")
}

/// Initialize a new agent's directory structure.
///
/// Creates `{agents_dir}/{key}/` with `SOUL.md`, `memory/`, and `sessions/`.
/// Returns an error if the directory already exists.
pub fn init_agent_dir(key: &str, soul_content: &str) -> Result<(), String> {
    let agent_dir = agents_dir().join(key);
    if agent_dir.exists() {
        return Err(format!("Agent '{key}' 已存在"));
    }

    fs::create_dir_all(agent_dir.join("memory"))
        .map_err(|e| format!("创建 agent 目录失败: {e}"))?;
    fs::create_dir_all(agent_dir.join("sessions"))
        .map_err(|e| format!("创建 sessions 目录失败: {e}"))?;
    fs::write(agent_dir.join("SOUL.md"), soul_content)
        .map_err(|e| format!("写入 SOUL.md 失败: {e}"))?;

    Ok(())
}

/// Remove an agent's directory tree entirely.
pub fn remove_agent_dir(key: &str) -> Result<(), String> {
    let agent_dir = agents_dir().join(key);
    if !agent_dir.exists() {
        return Err(format!("Agent '{key}' 不存在"));
    }
    fs::remove_dir_all(&agent_dir)
        .map_err(|e| format!("删除 agent 目录失败: {e}"))
}

/// Read the raw contents of an agent's `SOUL.md`.
pub fn read_soul(key: &str) -> Result<String, String> {
    let path = agents_dir().join(key).join("SOUL.md");
    fs::read_to_string(&path)
        .map_err(|e| format!("读取 SOUL.md 失败: {e}"))
}

/// Overwrite an agent's `SOUL.md` file.
pub fn write_soul(key: &str, content: &str) -> Result<(), String> {
    let path = agents_dir().join(key).join("SOUL.md");
    fs::write(&path, content)
        .map_err(|e| format!("写入 SOUL.md 失败: {e}"))
}

/// List all agent subdirectory names under the agents directory.
pub fn list_agent_dirs() -> Vec<String> {
    let dir = agents_dir();
    if !dir.exists() {
        return Vec::new();
    }
    let mut keys = Vec::new();
    if let Ok(entries) = fs::read_dir(&dir) {
        for entry in entries.flatten() {
            if entry.file_type().is_ok_and(|t| t.is_dir())
                && let Ok(name) = entry.file_name().into_string() {
                    keys.push(name);
                }
        }
    }
    keys.sort();
    keys
}

/// Return the path to an agent's emotions directory.
#[must_use]
pub fn emotions_dir(key: &str) -> PathBuf {
    agents_dir().join(key).join("emotions")
}

/// Extract a short summary (first 3 non-empty lines) from an agent's `SOUL.md`.
#[must_use]
pub fn soul_summary(key: &str) -> String {
    match read_soul(key) {
        Ok(content) => content
            .lines()
            .filter(|l| !l.trim().is_empty())
            .take(3)
            .collect::<Vec<_>>()
            .join("\n"),
        Err(_) => String::new(),
    }
}
