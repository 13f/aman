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

impl SelfBridge {
    /// Create a new bridge from config and the predefined directory path.
    #[must_use]
    pub fn new(config: &SelfConfig, predefined_dir: &Path) -> Self {
        let bridge_script = predefined_dir.join(&config.bridge_script);
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

    /// Assemble the complete system prompt (soul + date + tools + memory).
    pub fn build_full_prompt(
        &self,
        soul_prompt: &str,
        tools_json: &serde_json::Value,
        memory: Option<&str>,
    ) -> Option<String> {
        let args = serde_json::json!({
            "soul_prompt": soul_prompt,
            "tools": tools_json,
            "memory": memory,
        });
        self.call("full-prompt", &args)
    }

    /// Get the session extraction prompt template.
    pub fn extraction_prompt(&self) -> Option<String> {
        let args = serde_json::json!({});
        self.call("extraction-prompt", &args)
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
