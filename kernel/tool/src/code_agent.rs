#![forbid(unsafe_code)]
// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! Code agent tool — wraps external CLI coding tools (Claude Code, Codex, etc.)
//! as Aman tools so agents can delegate coding tasks to them.
//!
//! Each tool is registered when the gateway starts, based on which CLI commands
//! are available on PATH.

use kernel::context::ToolContext;
use kernel::error::Error;
use kernel::schema::JsonSchema;
use kernel::tool::{Tool, ToolResult};
use kernel::types::ToolMode;
use serde::{Deserialize, Serialize};
use std::process::{Command, Stdio};
use std::sync::LazyLock;
use std::time::Duration;

// ---------------------------------------------------------------------------
// Config types (shared with tauri crate)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodeAgentConfig {
    pub key: String,
    pub display_name: String,
    pub command: String,
    pub description: String,
    #[serde(default)]
    pub tool_args: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CodeAgentsFile {
    #[serde(default)]
    agents: Vec<CodeAgentConfig>,
}

/// Embedded built-in code agents, kept current with each release.
const BUILTIN_JSON: &str = include_str!("../../../predefined/agents/code-agents.json");

// ---------------------------------------------------------------------------
// Tool implementation
// ---------------------------------------------------------------------------

pub struct CodeAgentTool {
    /// Tool name, e.g. `code-claude`
    name: String,
    /// The CLI command to run, e.g. `claude`
    command: String,
    /// Human description injected into the system prompt
    description: String,
    /// Argument template for non-interactive invocation. `{prompt}` is
    /// replaced with the user's task. e.g. `["-p", "{prompt}"]`
    tool_args: Vec<String>,
}

impl CodeAgentTool {
    pub fn new(config: &CodeAgentConfig) -> Self {
        Self {
            name: config.command.clone(),
            command: config.command.clone(),
            description: config.description.clone(),
            tool_args: config.tool_args.clone(),
        }
    }
}

#[async_trait::async_trait]
impl Tool for CodeAgentTool {
    fn name(&self) -> &str {
        &self.name
    }

    fn mode(&self) -> ToolMode {
        ToolMode::Sandbox
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn parameters(&self) -> &JsonSchema {
        static PARAMS: LazyLock<JsonSchema> = LazyLock::new(|| {
            JsonSchema::from(serde_json::json!({
                "type": "object",
                "required": ["prompt"],
                "properties": {
                    "prompt": {
                        "type": "string",
                        "description": "The coding task to delegate. Be specific about files, changes, and expected output."
                    },
                    "cwd": {
                        "type": "string",
                        "description": "Working directory for the code agent. Defaults to the agent's runtime directory if omitted."
                    }
                }
            }))
        });
        &PARAMS
    }

    fn returns(&self) -> &JsonSchema {
        static RETURNS: LazyLock<JsonSchema> = LazyLock::new(|| {
            JsonSchema::from(serde_json::json!({
                "type": "object",
                "properties": {
                    "stdout": {"type": "string"},
                    "stderr": {"type": "string"},
                    "exit_code": {"type": "integer"}
                }
            }))
        });
        &RETURNS
    }

    async fn execute(&self, params: serde_json::Value, ctx: ToolContext) -> ToolResult {
        let prompt = params["prompt"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        if prompt.is_empty() {
            return Err(Error::ConfigInvalid {
                message: "prompt is required for code agent tool".into(),
            });
        }

        let cwd = params["cwd"]
            .as_str()
            .map(|s| s.to_string())
            .unwrap_or_else(|| {
                ctx.working_directory
                    .clone()
                    .unwrap_or_else(|| ".".to_string())
            });

        // Build argument list, substituting {prompt}
        let args: Vec<String> = self
            .tool_args
            .iter()
            .map(|a| a.replace("{prompt}", &prompt))
            .collect();

        let command = self.command.clone();
        tokio::task::spawn_blocking(move || {
            let args_ref: Vec<&str> = args.iter().map(String::as_str).collect();
            let mut child = Command::new(&command)
                .args(&args_ref)
                .current_dir(&cwd)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .map_err(|e| Error::Unrecoverable {
                    message: format!("Failed to spawn {command}: {e}"),
                })?;

            // Wait with timeout (5 minutes for coding tasks)
            let timeout = Duration::from_secs(300);
            match wait_timeout(&mut child, timeout) {
                Ok(Some(status)) => {
                    let mut stdout_buf = Vec::new();
                    if let Some(mut out) = child.stdout.take() {
                        let _ = std::io::Read::read_to_end(&mut out, &mut stdout_buf);
                    }
                    let mut stderr_buf = Vec::new();
                    if let Some(mut err) = child.stderr.take() {
                        let _ = std::io::Read::read_to_end(&mut err, &mut stderr_buf);
                    }
                    let stdout = String::from_utf8_lossy(&stdout_buf).to_string();
                    let stderr = String::from_utf8_lossy(&stderr_buf).to_string();
                    let exit_code = status.code().unwrap_or(-1);
                    Ok(serde_json::json!({
                        "stdout": stdout,
                        "stderr": stderr,
                        "exit_code": exit_code,
                    }))
                }
                Ok(None) => {
                    let _ = child.kill();
                    Err(Error::Timeout)
                }
                Err(e) => Err(e),
            }
        })
        .await
        .unwrap_or_else(|e| Err(Error::Unrecoverable {
            message: format!("Task join error: {e}"),
        }))
    }
}

/// Wait for a child process to exit, with a timeout.
fn wait_timeout(
    child: &mut std::process::Child,
    timeout: Duration,
) -> Result<Option<std::process::ExitStatus>, Error> {
    let start = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Ok(Some(status)),
            Ok(None) => {
                if start.elapsed() >= timeout {
                    return Ok(None);
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(e) => return Err(Error::Unrecoverable {
                message: format!("try_wait failed: {e}"),
            }),
        }
    }
}

// ---------------------------------------------------------------------------
// Loading
// ---------------------------------------------------------------------------

/// Check whether `command` is available on PATH.
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

/// Load code agent configs from the built-in predefined file.
pub fn load_builtin_code_agent_configs() -> Vec<CodeAgentConfig> {
    serde_json::from_str::<CodeAgentsFile>(BUILTIN_JSON)
        .map(|f| f.agents)
        .unwrap_or_default()
}

/// List code agent configs for CLI tools that are actually available on PATH.
pub fn available_code_agent_configs() -> Vec<CodeAgentConfig> {
    load_builtin_code_agent_configs()
        .into_iter()
        .filter(|c| check_command_available(&c.command))
        .collect()
}
