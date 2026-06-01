#![forbid(unsafe_code)]
// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! Code agent launcher — external CLI coding tools (Claude Code, Codex, etc.).
//!
//! Code agents are defined in `predefined/agents/code-agents.json` (built-in,
//! kept up-to-date with each release) and `~/.aman/code-agents.json` (user
//! overrides/additions). They are not managed by the Aman gateway and have no
//! idle system. Availability is determined by checking whether the CLI tool
//! is on PATH.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use crate::models::CodeAgentEntry;

// ---------------------------------------------------------------------------
// Persisted config types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodeAgentConfig {
    pub key: String,
    pub display_name: String,
    pub command: String,
    pub description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CodeAgentsFile {
    #[serde(default)]
    agents: Vec<CodeAgentConfig>,
}

/// Embedded built-in code agents, kept current with each release.
const BUILTIN_JSON: &str = include_str!("../../predefined/agents/code-agents.json");

// ---------------------------------------------------------------------------
// Paths
// ---------------------------------------------------------------------------

fn aman_agents_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    PathBuf::from(home)
        .join(".aman")
        .join("agents")
        .join("code-agents.json")
}

// ---------------------------------------------------------------------------
// Command availability check
// ---------------------------------------------------------------------------

/// Check whether `command` is available on the system PATH.
fn check_command_available(command: &str) -> bool {
    if cfg!(target_os = "windows") {
        std::process::Command::new("where")
            .arg(command)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    } else {
        std::process::Command::new("which")
            .arg(command)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    }
}

// ---------------------------------------------------------------------------
// Load / merge
// ---------------------------------------------------------------------------

/// Load the code agent list from `~/.aman/agents/code-agents.json` (synced by
/// gateway with hash comparison — user modifications are preserved across
/// builtin updates). Falls back to the embedded builtin if the file doesn't
/// exist (gateway hasn't started yet or first run).
pub fn load_code_agents() -> Result<Vec<CodeAgentEntry>, String> {
    let path = aman_agents_path();

    let agents: Vec<CodeAgentConfig> = if path.exists() {
        let raw = std::fs::read_to_string(&path)
            .map_err(|e| format!("读取 {} 失败: {e}", path.display()))?;
        let file: CodeAgentsFile = serde_json::from_str(&raw)
            .map_err(|e| format!("解析 {} 失败: {e}", path.display()))?;
        file.agents
    } else {
        let builtin: CodeAgentsFile = serde_json::from_str(BUILTIN_JSON)
            .map_err(|e| format!("解析内置 code-agents.json 失败: {e}"))?;
        builtin.agents
    };

    let entries: Vec<CodeAgentEntry> = agents
        .into_iter()
        .map(|c| {
            let available = check_command_available(&c.command);
            CodeAgentEntry {
                key: c.key,
                display_name: c.display_name,
                command: c.command,
                description: c.description,
                available,
            }
        })
        .collect();

    Ok(entries)
}

// ---------------------------------------------------------------------------
// Terminal launch (UI side — opens a visible terminal window)
// ---------------------------------------------------------------------------

/// Pick a directory via native dialog, then open a terminal in that directory
/// running the given command.
pub fn launch_code_agent(command: &str) -> Result<(), String> {
    let dir = pick_directory()?;
    launch_in_terminal(&dir, command)
}

#[cfg(target_os = "macos")]
fn pick_directory() -> Result<String, String> {
    let output = std::process::Command::new("osascript")
        .args([
            "-e",
            "POSIX path of (choose folder with prompt \"Select project directory:\")",
        ])
        .output()
        .map_err(|e| format!("Failed to open folder picker: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr.contains("User cancelled") || stderr.trim().is_empty() {
            return Err("CANCELLED".to_owned());
        }
        return Err(format!("Folder picker error: {}", stderr.trim()));
    }

    let dir = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if dir.is_empty() {
        return Err("CANCELLED".to_owned());
    }
    Ok(dir)
}

#[cfg(target_os = "linux")]
fn pick_directory() -> Result<String, String> {
    let output = std::process::Command::new("zenity")
        .args([
            "--file-selection",
            "--directory",
            "--title=Select project directory",
        ])
        .output()
        .or_else(|_| {
            std::process::Command::new("kdialog")
                .args(["--getexistingdirectory", ""])
                .output()
        })
        .map_err(|e| format!("Failed to open folder picker: {e}"))?;

    if !output.status.success() {
        return Err("CANCELLED".to_owned());
    }
    let dir = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if dir.is_empty() {
        return Err("CANCELLED".to_owned());
    }
    Ok(dir)
}

#[cfg(target_os = "windows")]
fn pick_directory() -> Result<String, String> {
    Err("Code agents are not yet supported on Windows.".to_owned())
}

#[cfg(target_os = "macos")]
fn launch_in_terminal(dir: &str, command: &str) -> Result<(), String> {
    let dir_escaped = dir.replace('\'', "'\\''");
    let script = format!(
        "tell application \"Terminal\" to do script \"cd '{dir_escaped}' && {command}\""
    );
    std::process::Command::new("osascript")
        .args(["-e", &script])
        .spawn()
        .map_err(|e| format!("Failed to open Terminal: {e}"))?;
    Ok(())
}

#[cfg(target_os = "linux")]
fn launch_in_terminal(dir: &str, command: &str) -> Result<(), String> {
    let terminals: &[(&str, &[&str])] = &[
        ("gnome-terminal", &["--working-directory", dir, "--", command]),
        ("konsole", &["--workdir", dir, "-e", command]),
        ("xfce4-terminal", &["--working-directory", dir, "-e", command]),
        ("x-terminal-emulator", &["-e", &format!("cd '{}' && {}", dir, command)]),
    ];

    for (term, args) in terminals {
        if check_command_available(term) {
            return std::process::Command::new(term)
                .args(*args)
                .spawn()
                .map(|_| ())
                .map_err(|e| format!("Failed to open {term}: {e}"));
        }
    }
    Err("No supported terminal emulator found (gnome-terminal, konsole, xfce4-terminal, x-terminal-emulator).".to_owned())
}

#[cfg(target_os = "windows")]
fn launch_in_terminal(_dir: &str, _command: &str) -> Result<(), String> {
    Err("Code agents are not yet supported on Windows.".to_owned())
}
