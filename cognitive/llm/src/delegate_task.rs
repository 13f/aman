// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! `delegate_task` tool — spawn an anonymous sub-agent to work on a
//! subtask in parallel, then return its result.
//!
//! Depends on [`super::SubAgentSpawner`] (trait), not on any gateway
//! type.  The gateway injects its concrete implementation at startup.

use std::sync::{Arc, LazyLock};

use kernel::agent::AgentDescriptor;
use kernel::context::ToolContext;
use kernel::react::SoulSnapshot;
use kernel::schema::JsonSchema;
use kernel::tool::Tool;
use kernel::types::{ExecutionModel, ToolMode};
use kernel::{AmanResult, Error};
use serde_json::{json, Value};

use super::subagent::SubAgentSpawner;

pub struct DelegateTaskTool {
    spawner: std::sync::OnceLock<Arc<dyn SubAgentSpawner>>,
}

impl DelegateTaskTool {
    pub fn new() -> Self {
        Self {
            spawner: std::sync::OnceLock::new(),
        }
    }

    /// Inject the concrete sub-agent spawner (called by the gateway at startup).
    pub fn set_spawner(&self, spawner: Arc<dyn SubAgentSpawner>) {
        let _ = self.spawner.set(spawner);
    }
}

impl Default for DelegateTaskTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl Tool for DelegateTaskTool {
    fn name(&self) -> &str {
        "delegate_task"
    }

    fn mode(&self) -> ToolMode {
        ToolMode::Local
    }

    fn description(&self) -> &str {
        "Spawn a temporary anonymous sub-agent to work on a subtask independently. \
         The sub-agent gets its own ReAct loop, context, and tool access. \
         Results are returned directly. \
         Use this for: parallel exploration of different approaches, \
         independent verification/audit of findings, or offloading \
         self-contained work so the main agent can continue."
    }

    fn parameters(&self) -> &JsonSchema {
        static PARAMS: LazyLock<JsonSchema> = LazyLock::new(|| {
            JsonSchema::from(json!({
                "type": "object",
                "required": ["prompt"],
                "properties": {
                    "prompt": {
                        "type": "string",
                        "description": "The task for the sub-agent to complete. Be specific about what to produce and how to report results."
                    },
                    "model": {
                        "type": "string",
                        "description": "Optional: model name for the sub-agent. Defaults to the parent agent's model."
                    },
                    "system_prompt": {
                        "type": "string",
                        "description": "Optional: system prompt (soul) for the sub-agent. Defaults to a generic assistant prompt."
                    },
                    "allowed_tools": {
                        "type": "array",
                        "items": {"type": "string"},
                        "description": "Optional: explicit tool whitelist. If omitted, inherits parent policy. Use ['*'] to allow all non-LLM tools."
                    },
                    "background": {
                        "type": "boolean",
                        "description": "If true, spawn and return immediately. If false (default), wait for completion."
                    }
                }
            }))
        });
        &PARAMS
    }

    fn returns(&self) -> &JsonSchema {
        static RETURNS: LazyLock<JsonSchema> = LazyLock::new(|| {
            JsonSchema::from(json!({
                "type": "object",
                "properties": {
                    "agent_id": {
                        "type": "string",
                        "description": "The anonymous agent id (format: anon-{uuid})."
                    },
                    "session_id": {
                        "type": "string",
                        "description": "Session id for this execution."
                    },
                    "reply": {
                        "type": "string",
                        "description": "The sub-agent's final reply. Empty when background=true."
                    },
                    "background": {
                        "type": "boolean",
                        "description": "Whether this was a background spawn."
                    }
                }
            }))
        });
        &RETURNS
    }

    fn execution_model(&self) -> ExecutionModel {
        ExecutionModel::SideEffect
    }

    async fn execute(&self, params: Value, _ctx: ToolContext) -> AmanResult<Value> {
        let spawner = self
            .spawner
            .get()
            .ok_or_else(|| Error::ConfigInvalid {
                message: "delegate_task: SubAgentSpawner not wired".to_owned(),
            })?;

        // ── Parse parameters ─────────────────────────────────────────
        let prompt = params
            .get("prompt")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| Error::ConfigInvalid {
                message: "delegate_task: missing required parameter 'prompt'"
                    .to_owned(),
            })?;

        let model_override = params
            .get("model")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty());

        let system_prompt = params
            .get("system_prompt")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .unwrap_or(
                "You are a capable assistant. Complete the assigned task \
                 thoroughly and return your results in a clear, structured format.",
            );

        let background = params
            .get("background")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let tool_override: Option<Option<Vec<String>>> = params
            .get("allowed_tools")
            .and_then(|v| v.as_array())
            .map(|arr| {
                let tools: Vec<String> = arr
                    .iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect();
                if tools.is_empty() || tools.iter().any(|t| t == "*") {
                    None
                } else {
                    Some(tools)
                }
            });

        // ── Build descriptor ─────────────────────────────────────────
        let model = model_override.unwrap_or("");
        let descriptor = AgentDescriptor {
            agent_id: String::new(),
            display_name: format!("subagent-{}", uuid::Uuid::new_v4()),
            provider: String::new(), // spawner resolves this
            model: model.to_owned(),
            soul_path: None,
            allowed_tools: tool_override.unwrap_or(None),
            denied_tools: Vec::new(),
            allowed_skills: None,
            enabled: true,
            max_context_tokens: None,
            max_output_tokens: None,
        };

        let soul = SoulSnapshot::new("subagent", system_prompt);

        // ── Spawn ────────────────────────────────────────────────────
        let result = spawner
            .spawn(descriptor, soul, prompt.to_owned(), background)
            .await?;

        Ok(json!({
            "agent_id": result.agent_id,
            "session_id": result.session_id,
            "reply": result.reply,
            "background": result.background,
        }))
    }
}
