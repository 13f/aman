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

/// Pick a directory via native OS dialog, then open a terminal in that
/// directory running the given command.
pub fn launch_code_agent(command: &str) -> Result<(), String> {
    let dir = pick_directory()?;
    launch_in_terminal(&dir, command)
}

/// Show a native folder-picker dialog.
///
/// Uses `rfd` (Rust File Dialog) which wraps NSOpenPanel on macOS,
/// IFileDialog on Windows, and zenity/kdialog/GTK on Linux — a single
/// implementation for all platforms.
fn pick_directory() -> Result<String, String> {
    rfd::FileDialog::new()
        .set_title("Select project directory")
        .pick_folder()
        .map(|p| p.to_string_lossy().to_string())
        .ok_or_else(|| "CANCELLED".to_owned())
}

// ── per-platform terminal launchers ──────────────────────────────────────

#[cfg(target_os = "macos")]
fn launch_in_terminal(dir: &str, command: &str) -> Result<(), String> {
    // Escape single quotes for the AppleScript string literal.
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
fn launch_in_terminal(dir: &str, command: &str) -> Result<(), String> {
    // Prefer Windows Terminal; fall back to classic conhost.
    if check_command_available("wt.exe") {
        return std::process::Command::new("wt.exe")
            .args(["-d", dir, "cmd", "/k", command])
            .spawn()
            .map(|_| ())
            .map_err(|e| format!("Failed to open Windows Terminal: {e}"));
    }

    // Classic cmd window via `start`. The first quoted arg to `start` is the
    // window title, so we supply one explicitly before the `cmd /k` payload.
    std::process::Command::new("cmd.exe")
        .args([
            "/c", "start", "aman — Code Agent", "cmd", "/k",
            &format!("cd /d \"{}\" && {}", dir, command),
        ])
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("Failed to open cmd: {e}"))
}
