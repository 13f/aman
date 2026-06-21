// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! Python bridge for self-module prompt builders.
//!
//! Wraps one-shot `python3 bridge.py <method>` calls with automatic fallback
//! to the Rust implementation on any failure. Designed for Phase 2:
//! Python-first with transparent Rust fallback.

use config::SelfConfig;
use std::path::{Path, PathBuf};
use std::process::Command;
use tracing::{debug, warn};

/// Wrapper around the Python self-module bridge script.
///
/// Each method shells out to `python3 bridge.py <method> <json-args>`,
/// reads stdout, and returns the result. On any error (missing Python,
/// script error, timeout), logs a warning and returns `None` so the
/// caller can fall back to the Rust implementation.
#[derive(Clone)]
pub struct SelfBridge {
    enabled: bool,
    python: String,
    bridge_script: PathBuf,
}

/// Extra context for [`SelfBridge::build_full_system_prompt`].
///
/// Groups optional session-level metadata that Python injects into
/// the VOLATILE and CONTEXT layers of the system prompt.
pub struct SystemPromptContext<'a> {
    /// Content of CLAUDE.md discovered from the working directory.
    pub claude_md_content: Option<&'a str>,
    /// Current working directory (for environment hints, git posture).
    pub cwd: Option<&'a str>,
    /// Platform identifier: "cli", "desktop", etc.
    pub platform: &'a str,
    /// LLM model name (for the timestamp line).
    pub model: Option<&'a str>,
    /// LLM provider name (for the timestamp line).
    pub provider: Option<&'a str>,
}

impl SelfBridge {
    /// Create a new bridge from config and the predefined directory path.
    ///
    /// Prefers `~/.aman/self/bridge.py` (user data dir, synced at startup)
    /// so that runtime modifications by the agent are picked up. Falls back
    /// to `predefined/self/bridge.py` (source tree) if the user copy is
    /// missing.
    #[must_use]
    pub fn new(config: &SelfConfig, predefined_dir: &Path) -> Self {
        let user_self = super::skill_sync::aman_data_dir().join(&config.bridge_script);
        let bridge_script = if user_self.exists() {
            user_self
        } else {
            predefined_dir.join(&config.bridge_script)
        };
        Self {
            enabled: config.enabled,
            python: config.python.clone(),
            bridge_script,
        }
    }

    /// Create a disabled bridge — always returns None (Rust fallback).
    #[must_use]
    pub fn disabled() -> Self {
        Self {
            enabled: false,
            python: String::new(),
            bridge_script: PathBuf::new(),
        }
    }

    // ── Public API ───────────────────────────────────────────────────

    /// Parse SOUL.md content and build the system prompt string.
    /// Returns `None` on any error — caller should fall back to Rust.
    pub fn build_soul_prompt(&self, soul_content: &str) -> Option<String> {
        let args = serde_json::json!({"content": soul_content});
        self.call("soul-prompt", &args)
    }

    /// Build the skills section of the system prompt from SkillInfo JSON.
    pub fn build_skills_prompt(&self, skills_json: &serde_json::Value) -> Option<String> {
        let args = serde_json::json!({"skills": skills_json});
        self.call("skills-prompt", &args)
    }

    /// Build the complete system prompt (three-layer Hermes-style assembly).
    ///
    /// Calls `python3 bridge.py system-prompt` which uses the unified
    /// `system_prompt.py` module. USER.md and MEMORY.md are auto-discovered
    /// by Python from ~/.aman/ — no need to pass paths unless overriding.
    pub fn build_full_system_prompt(
        &self,
        soul_content: &str,
        skills_json: &serde_json::Value,
        tools_json: &serde_json::Value,
        memory: Option<&str>,
        ctx: &SystemPromptContext<'_>,
    ) -> Option<String> {
        let mut args = serde_json::json!({
            "soul_content": soul_content,
            "skills": skills_json,
            "tools": tools_json,
            "memory": memory,
            "platform": ctx.platform,
        });
        if let Some(c) = ctx.claude_md_content {
            args["claude_md_content"] = serde_json::Value::String(c.to_owned());
        }
        if let Some(c) = ctx.cwd {
            args["cwd"] = serde_json::Value::String(c.to_owned());
        }
        if let Some(m) = ctx.model {
            args["model"] = serde_json::Value::String(m.to_owned());
        }
        if let Some(p) = ctx.provider {
            args["provider"] = serde_json::Value::String(p.to_owned());
        }
        self.call("system-prompt", &args)
    }

    /// Get the session extraction prompt template.
    pub fn extraction_prompt(&self) -> Option<String> {
        let args = serde_json::json!({});
        self.call("extraction-prompt", &args)
    }

    /// Get the entity extraction prompt for incubation memory indexing.
    pub fn entity_extraction_prompt(&self) -> Option<String> {
        let args = serde_json::json!({});
        self.call("entity-extraction-prompt", &args)
    }

    /// Parse a slash-command string. Returns `(skill_name, user_input)`.
    pub fn parse_skill_command(&self, text: &str) -> Option<(String, String)> {
        let args = serde_json::json!({"text": text});
        let output = self.call("parse-command", &args)?;
        let parsed: serde_json::Value = serde_json::from_str(&output).ok()?;
        if parsed.is_null() {
            return None;
        }
        let skill_name = parsed.get("skill_name")?.as_str()?.to_owned();
        let user_input = parsed.get("user_input")?.as_str()?.to_owned();
        Some((skill_name, user_input))
    }

    /// Match skills by prefix. Returns list of matching skill names.
    pub fn match_skill_prefix(
        &self,
        prefix: &str,
        skills_json: &serde_json::Value,
    ) -> Option<Vec<String>> {
        let args = serde_json::json!({
            "prefix": prefix,
            "skills": skills_json,
        });
        let output = self.call("match-prefix", &args)?;
        let names: Vec<String> = serde_json::from_str(&output).ok()?;
        Some(names)
    }

    // ── Internal ─────────────────────────────────────────────────────

    fn call(&self, method: &str, args: &serde_json::Value) -> Option<String> {
        if !self.enabled {
            return None;
        }

        let args_str = args.to_string();

        let result = Command::new(&self.python)
            .arg(&self.bridge_script)
            .arg(method)
            .arg(&args_str)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .output();

        match result {
            Ok(output) if output.status.success() => {
                let stdout = String::from_utf8_lossy(&output.stdout).trim().to_owned();
                if stdout.is_empty() {
                    warn!(method, "SelfBridge: empty stdout");
                    None
                } else {
                    debug!(method, len = stdout.len(), "SelfBridge: call succeeded");
                    Some(stdout)
                }
            }
            Ok(output) => {
                let stderr = String::from_utf8_lossy(&output.stderr);
                warn!(
                    method,
                    exit_code = ?output.status.code(),
                    stderr = %stderr.trim(),
                    "SelfBridge: call failed, falling back to Rust"
                );
                None
            }
            Err(e) => {
                warn!(
                    method,
                    error = %e,
                    "SelfBridge: failed to spawn Python, falling back to Rust"
                );
                None
            }
        }
    }
}

/// Build a complete system prompt without Python — pure Rust fallback.
///
/// Assembles: soul_prompt + skills_prompt + date + tools + file ops +
/// tool format + web reminders. Used when the Python bridge is unavailable
/// or disabled.
pub fn build_system_prompt_fallback(
    soul_prompt: &str,
    skills_prompt: &str,
    tools: &[kernel::react::ToolDescriptor],
) -> String {
    let mut parts: Vec<String> = Vec::new();
    parts.push(soul_prompt.to_owned());
    if !skills_prompt.is_empty() {
        parts.push(skills_prompt.to_owned());
    }
    parts.push(format!("Current date: {}", super::date_util::current_date_string()));
    if !tools.is_empty() {
        let tool_list: Vec<String> = tools
            .iter()
            .map(|t| format!("- {}: {} (parameters: {})", t.name, t.description, t.parameters))
            .collect();
        parts.push(format!(
            "\n## Available Tools\nYou can use these tools when responding:\n{}",
            tool_list.join("\n")
        ));
        parts.push(
            "\n## File Operations (safe, no shell)\n\
             - read(path): read file contents\n\
             - write(path, content): write file (auto-creates parent dirs)\n\
             - edit(file_path, old_string, new_string): replace exact matching text in file\n\
             - list(path): list directory entries\n\
             - find(pattern, base): search files by name (recursive, case-insensitive)\n\
             - grep(pattern, path, glob?): search file contents via ripgrep (multi-threaded)"
                .to_owned(),
        );
        parts.push(
            "\nWhen you need to use a tool, respond with a JSON tool call in the format:\
             \n```tool_call\n{\"name\": \"tool_name\", \"arguments\": {...}}\n```"
                .to_owned(),
        );
        parts.push(
            "\nImportant: If the user asks about current events, recent data, prices, dates, \
             or any time-sensitive information, use the web_search tool first rather than \
             relying on your training data. For example, search for \"recent\" or \"today\" \
             queries instead of answering from memory."
                .to_owned(),
        );
        parts.push(
            "\nTo read the full content of a web page, fetch a specific URL, or download raw \
             data from an API endpoint, use the web_fetch tool. Typical flow: find URLs \
             via web_search, then read them via web_fetch."
                .to_owned(),
        );
    }
    parts.join("\n\n")
}
