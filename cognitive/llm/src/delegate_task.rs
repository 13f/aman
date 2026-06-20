// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! `delegate_task` tool — spawn anonymous sub-agents and collect their
//! results.
//!
//! Two operations (same pattern as `planner`):
//! - **spawn** (default): create a sub-agent and optionally wait for it.
//! - **collect**: retrieve the result of a previously spawned background
//!   sub-agent by its `agent_id`.
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
        "Spawn a temporary anonymous sub-agent to work on a subtask independently, \
         or collect the result of a previously spawned background sub-agent. \
         Two operations: \
         'spawn' (default) — create a sub-agent with its own ReAct loop. \
           Set background=true to run asynchronously and collect later. \
         'collect' — retrieve the result of a background sub-agent by agent_id. \
         Use for: parallel exploration, independent verification/audit, \
         or offloading self-contained work."
    }

    fn parameters(&self) -> &JsonSchema {
        static PARAMS: LazyLock<JsonSchema> = LazyLock::new(|| {
            JsonSchema::from(json!({
                "type": "object",
                "properties": {
                    "operation": {
                        "type": "string",
                        "description": "Which operation to perform: 'spawn' (default) or 'collect'.",
                        "enum": ["spawn", "collect"]
                    },
                    "prompt": {
                        "type": "string",
                        "description": "spawn: the task for the sub-agent to complete."
                    },
                    "model": {
                        "type": "string",
                        "description": "spawn: model name. Defaults to parent agent's model."
                    },
                    "system_prompt": {
                        "type": "string",
                        "description": "spawn: system prompt (soul) for the sub-agent."
                    },
                    "allowed_tools": {
                        "type": "array",
                        "items": {"type": "string"},
                        "description": "spawn: tool whitelist. Inherits parent policy if omitted."
                    },
                    "background": {
                        "type": "boolean",
                        "description": "spawn: if true, return immediately with agent_id. Collect later with operation='collect'."
                    },
                    "agent_id": {
                        "type": "string",
                        "description": "collect: the agent_id returned by a previous spawn with background=true."
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
                        "description": "The sub-agent's final reply text. Empty for background spawns."
                    },
                    "background": {
                        "type": "boolean",
                        "description": "Whether the spawn was background (collect returns background:false)."
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

        let operation = params
            .get("operation")
            .and_then(|v| v.as_str())
            .unwrap_or("spawn");

        match operation {
            "collect" => self.execute_collect(spawner.as_ref(), &params).await,
            _ => self.execute_spawn(spawner.as_ref(), &params).await,
        }
    }
}

impl DelegateTaskTool {
    async fn execute_spawn(
        &self,
        spawner: &dyn SubAgentSpawner,
        params: &Value,
    ) -> AmanResult<Value> {
        let prompt = params
            .get("prompt")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| Error::ConfigInvalid {
                message: "delegate_task spawn: missing required parameter 'prompt'"
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

        let model = model_override.unwrap_or("");
        let descriptor = AgentDescriptor {
            agent_id: String::new(),
            display_name: format!("subagent-{}", uuid::Uuid::new_v4()),
            provider: String::new(),
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

    async fn execute_collect(
        &self,
        spawner: &dyn SubAgentSpawner,
        params: &Value,
    ) -> AmanResult<Value> {
        let agent_id = params
            .get("agent_id")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| Error::ConfigInvalid {
                message: "delegate_task collect: missing required parameter 'agent_id'"
                    .to_owned(),
            })?;

        let result = spawner.collect_result(agent_id).await?;

        Ok(json!({
            "agent_id": result.agent_id,
            "session_id": result.session_id,
            "reply": result.reply,
            "background": false,
        }))
    }
}
