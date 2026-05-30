// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! Evaluation tools — registered in the runtime so agents can self-evaluate.
//!
//! These tools are implemented here (not in a plugin) and registered directly
//! with the ToolRegistry during AgentRuntime startup.

use std::sync::{Arc, LazyLock};

use async_trait::async_trait;
use kernel::context::ToolContext;
use kernel::schema::JsonSchema;
use kernel::tool::{Tool, ToolResult};
use kernel::types::ToolMode;
use serde_json::{json, Value};
use tokio::sync::RwLock;

use crate::engine::EvalEngine;
use crate::target::EvalTarget;

// ── eval_run ────────────────────────────────────────────────────────────

/// `eval_run` — run a specific evaluation rule against given content.
pub struct EvalRunTool {
    engine: Arc<RwLock<EvalEngine>>,
}

impl EvalRunTool {
    #[must_use]
    pub fn new(engine: Arc<RwLock<EvalEngine>>) -> Self {
        Self { engine }
    }
}

#[async_trait]
impl Tool for EvalRunTool {
    fn name(&self) -> &str {
        "eval_run"
    }

    fn mode(&self) -> ToolMode {
        ToolMode::Local
    }

    fn description(&self) -> &str {
        "Run an evaluation rule against the given content and return the score."
    }

    fn parameters(&self) -> &JsonSchema {
        static PARAMS: LazyLock<JsonSchema> = LazyLock::new(|| {
            JsonSchema::from(json!({
                "type": "object",
                "required": ["rule_id", "content"],
                "properties": {
                    "rule_id": {"type": "string", "description": "ID of the evaluation rule to run"},
                    "content": {"type": "string", "description": "Text content to evaluate"},
                    "query": {"type": "string", "description": "Optional original query for context"},
                    "model": {"type": "string", "description": "Optional model name that produced this output"}
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
                    "rule_id": {"type": "string"},
                    "aggregate_score": {"type": "number"},
                    "outcome": {"type": "string"},
                    "dimensions": {"type": "array"}
                }
            }))
        });
        &RETURNS
    }

    async fn execute(&self, params: Value, _ctx: ToolContext) -> ToolResult {
        let rule_id = params["rule_id"].as_str().unwrap_or("");
        let content = params["content"].as_str().unwrap_or("");
        let query = params["query"].as_str().map(String::from);
        let model = params["model"].as_str().map(String::from);

        let engine = self.engine.read().await;
        let target = EvalTarget::LlmOutput {
            content: content.to_owned(),
            model,
            turn: 1,
            query,
        };

        // Find the specific rule or evaluate against all matching
        let results = if rule_id.is_empty() {
            engine.evaluate(&target).await
        } else if let Some(_rule) = engine.rule_by_id(rule_id) {
            // Evaluate only the requested rule by temporarily filtering
            // (we evaluate all matching and filter post-hoc for simplicity)
            engine
                .evaluate(&target)
                .await
                .into_iter()
                .filter(|s| s.rule_id == rule_id)
                .collect()
        } else {
            return Ok(json!({
                "error": format!("rule '{}' not found", rule_id)
            }));
        };

        Ok(serde_json::to_value(&results).unwrap_or_else(|e| {
            json!({"error": format!("failed to serialize results: {}", e)})
        }))
    }
}

// ── eval_score ──────────────────────────────────────────────────────────

/// `eval_score` — score content on custom dimensions using the judge LLM.
pub struct EvalScoreTool {
    engine: Arc<RwLock<EvalEngine>>,
}

impl EvalScoreTool {
    #[must_use]
    pub fn new(engine: Arc<RwLock<EvalEngine>>) -> Self {
        Self { engine }
    }
}

#[async_trait]
impl Tool for EvalScoreTool {
    fn name(&self) -> &str {
        "eval_score"
    }

    fn mode(&self) -> ToolMode {
        ToolMode::Local
    }

    fn description(&self) -> &str {
        "Score content on custom dimensions using the configured evaluation engine."
    }

    fn parameters(&self) -> &JsonSchema {
        static PARAMS: LazyLock<JsonSchema> = LazyLock::new(|| {
            JsonSchema::from(json!({
                "type": "object",
                "required": ["content", "dimensions"],
                "properties": {
                    "content": {"type": "string", "description": "Content to evaluate"},
                    "dimensions": {
                        "type": "array",
                        "description": "Dimensions to score",
                        "items": {
                            "type": "object",
                            "properties": {
                                "name": {"type": "string"},
                                "description": {"type": "string"},
                                "weight": {"type": "number"}
                            }
                        }
                    },
                    "query": {"type": "string", "description": "Original query for context"}
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
                    "aggregate_score": {"type": "number"},
                    "dimensions": {"type": "array"}
                }
            }))
        });
        &RETURNS
    }

    async fn execute(&self, params: Value, _ctx: ToolContext) -> ToolResult {
        let content = params["content"].as_str().unwrap_or("");
        let query = params["query"].as_str().map(String::from);

        let target = EvalTarget::LlmOutput {
            content: content.to_owned(),
            model: None,
            turn: 1,
            query,
        };

        let engine = self.engine.read().await;
        let results = engine.evaluate(&target).await;

        Ok(serde_json::to_value(&results).unwrap_or_else(|e| {
            json!({"error": format!("failed to serialize: {}", e)})
        }))
    }
}

// ── eval_list_rules ─────────────────────────────────────────────────────

/// `eval_list_rules` — list all configured evaluation rules.
pub struct EvalListRulesTool {
    engine: Arc<RwLock<EvalEngine>>,
}

impl EvalListRulesTool {
    #[must_use]
    pub fn new(engine: Arc<RwLock<EvalEngine>>) -> Self {
        Self { engine }
    }
}

#[async_trait]
impl Tool for EvalListRulesTool {
    fn name(&self) -> &str {
        "eval_list_rules"
    }

    fn mode(&self) -> ToolMode {
        ToolMode::Local
    }

    fn description(&self) -> &str {
        "List all configured evaluation rules with their IDs, names, and strategies."
    }

    fn parameters(&self) -> &JsonSchema {
        static PARAMS: LazyLock<JsonSchema> = LazyLock::new(|| {
            JsonSchema::from(json!({
                "type": "object",
                "properties": {
                    "tags": {
                        "type": "array",
                        "items": {"type": "string"},
                        "description": "Filter rules by tags"
                    }
                }
            }))
        });
        &PARAMS
    }

    fn returns(&self) -> &JsonSchema {
        static RETURNS: LazyLock<JsonSchema> = LazyLock::new(|| {
            JsonSchema::from(json!({
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "id": {"type": "string"},
                        "name": {"type": "string"},
                        "strategy": {"type": "string"},
                        "threshold": {"type": "number"},
                        "enabled": {"type": "boolean"},
                        "tags": {"type": "array"}
                    }
                }
            }))
        });
        &RETURNS
    }

    async fn execute(&self, params: Value, _ctx: ToolContext) -> ToolResult {
        let engine = self.engine.read().await;
        let filter_tags: Vec<String> = params["tags"]
            .as_array()
            .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
            .unwrap_or_default();

        let rules: Vec<Value> = engine
            .rules()
            .iter()
            .filter(|r| {
                filter_tags.is_empty()
                    || filter_tags
                        .iter()
                        .any(|t| r.tags.iter().any(|rt| rt == t))
            })
            .map(|r| {
                json!({
                    "id": r.id,
                    "name": r.name,
                    "strategy": r.strategy.strategy_type_name(),
                    "threshold": r.threshold,
                    "enabled": r.enabled,
                    "tags": r.tags,
                    "applies_to": r.applies_to,
                })
            })
            .collect();

        Ok(Value::Array(rules))
    }
}

// ── eval_get_results ────────────────────────────────────────────────────

/// `eval_get_results` — query recent evaluation results.
pub struct EvalGetResultsTool {
    engine: Arc<RwLock<EvalEngine>>,
}

impl EvalGetResultsTool {
    #[must_use]
    pub fn new(engine: Arc<RwLock<EvalEngine>>) -> Self {
        Self { engine }
    }
}

#[async_trait]
impl Tool for EvalGetResultsTool {
    fn name(&self) -> &str {
        "eval_get_results"
    }

    fn mode(&self) -> ToolMode {
        ToolMode::Local
    }

    fn description(&self) -> &str {
        "Retrieve recent evaluation results, optionally filtered by rule ID."
    }

    fn parameters(&self) -> &JsonSchema {
        static PARAMS: LazyLock<JsonSchema> = LazyLock::new(|| {
            JsonSchema::from(json!({
                "type": "object",
                "properties": {
                    "rule_id": {"type": "string", "description": "Filter by rule ID"},
                    "limit": {"type": "integer", "description": "Max results to return (default 20)", "default": 20}
                }
            }))
        });
        &PARAMS
    }

    fn returns(&self) -> &JsonSchema {
        static RETURNS: LazyLock<JsonSchema> = LazyLock::new(|| {
            JsonSchema::from(json!({
                "type": "array",
                "items": {"type": "object"}
            }))
        });
        &RETURNS
    }

    async fn execute(&self, params: Value, _ctx: ToolContext) -> ToolResult {
        let engine = self.engine.read().await;
        let rule_id = params["rule_id"].as_str();
        let limit = params["limit"].as_u64().unwrap_or(20).min(100) as usize;

        let results: Vec<&crate::score::EvalScore> = match rule_id {
            Some(rid) => {
                let mut v = engine.results_by_rule(rid);
                v.truncate(limit);
                v
            }
            None => {
                let all = engine.recent_results();
                all.iter().rev().take(limit).collect()
            }
        };

        Ok(serde_json::to_value(&results).unwrap_or_else(|e| {
            json!({"error": format!("serialization failed: {}", e)})
        }))
    }
}

// ── eval_define ─────────────────────────────────────────────────────────

/// `eval_define` — dynamically define a new evaluation rule at runtime.
pub struct EvalDefineRuleTool {
    engine: Arc<RwLock<EvalEngine>>,
}

impl EvalDefineRuleTool {
    #[must_use]
    pub fn new(engine: Arc<RwLock<EvalEngine>>) -> Self {
        Self { engine }
    }
}

#[async_trait]
impl Tool for EvalDefineRuleTool {
    fn name(&self) -> &str {
        "eval_define"
    }

    fn mode(&self) -> ToolMode {
        ToolMode::Local
    }

    fn description(&self) -> &str {
        "Define a new evaluation rule at runtime (temporary, lost on restart)."
    }

    fn parameters(&self) -> &JsonSchema {
        static PARAMS: LazyLock<JsonSchema> = LazyLock::new(|| {
            JsonSchema::from(json!({
                "type": "object",
                "required": ["id", "strategy"],
                "properties": {
                    "id": {"type": "string", "description": "Unique rule identifier"},
                    "name": {"type": "string", "description": "Human-readable name"},
                    "strategy": {"type": "string", "description": "Strategy type: rule_based, assertion, heuristic, llm_as_judge"},
                    "threshold": {"type": "number", "description": "Pass threshold (0.0-1.0)", "default": 0.7},
                    "applies_to": {"type": "array", "items": {"type": "string"}, "description": "Target kinds this rule applies to"},
                    "tags": {"type": "array", "items": {"type": "string"}, "description": "Tags for filtering"}
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
                    "ok": {"type": "boolean"},
                    "message": {"type": "string"}
                }
            }))
        });
        &RETURNS
    }

    async fn execute(&self, params: Value, _ctx: ToolContext) -> ToolResult {
        let id = match params["id"].as_str() {
            Some(s) => s.to_owned(),
            None => return Ok(json!({"ok": false, "message": "missing 'id' field"})),
        };
        let name = params["name"]
            .as_str()
            .unwrap_or(&id)
            .to_owned();
        let strategy_type = params["strategy"].as_str().unwrap_or("rule_based");
        let threshold = params["threshold"].as_f64().unwrap_or(0.7).clamp(0.0, 1.0);
        let applies_to: Vec<String> = params["applies_to"]
            .as_array()
            .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
            .unwrap_or_default();
        let tags: Vec<String> = params["tags"]
            .as_array()
            .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
            .unwrap_or_default();

        // Build a simple rule with the requested strategy
        let strategy = match strategy_type {
            "rule_based" => crate::strategy::EvalStrategyType::RuleBased {
                patterns: vec![],
            },
            "assertion" => crate::strategy::EvalStrategyType::Assertion {
                assertions: vec![],
            },
            "heuristic" => crate::strategy::EvalStrategyType::Heuristic {
                factors: vec![],
                dimension_scores: vec![],
            },
            "llm_as_judge" => crate::strategy::EvalStrategyType::LlmAsJudge {
                prompt_template: String::new(),
                dimensions: vec![],
                model_override: None,
                temperature: 0.3,
            },
            _ => {
                return Ok(json!({
                    "ok": false,
                    "message": format!("unknown strategy type: {}", strategy_type)
                }))
            }
        };

        let rule = crate::rule::EvalRule {
            id: id.clone(),
            name,
            description: Some("Dynamically defined at runtime".into()),
            strategy,
            threshold,
            enabled: true,
            tags,
            applies_to,
        };

        let mut engine = self.engine.write().await;
        engine.add_rule(rule);

        Ok(json!({"ok": true, "message": format!("rule '{}' added", id)}))
    }
}

// ── Helpers ─────────────────────────────────────────────────────────────

/// Create all 5 eval tools sharing the same engine instance.
#[must_use]
pub fn create_eval_tools(engine: Arc<RwLock<EvalEngine>>) -> Vec<Arc<dyn Tool>> {
    vec![
        Arc::new(EvalRunTool::new(engine.clone())),
        Arc::new(EvalScoreTool::new(engine.clone())),
        Arc::new(EvalListRulesTool::new(engine.clone())),
        Arc::new(EvalGetResultsTool::new(engine.clone())),
        Arc::new(EvalDefineRuleTool::new(engine.clone())),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::EvalEngine;

    #[tokio::test]
    async fn eval_list_rules_returns_rules() {
        let engine = Arc::new(RwLock::new(EvalEngine::new()));
        let tool = EvalListRulesTool::new(engine.clone());

        let result = tool
            .execute(serde_json::json!({}), ToolContext::default())
            .await
            .unwrap();
        assert!(result.is_array());
    }

    #[tokio::test]
    async fn eval_define_then_list() {
        let engine = Arc::new(RwLock::new(EvalEngine::new()));
        let define = EvalDefineRuleTool::new(engine.clone());
        let list = EvalListRulesTool::new(engine.clone());

        // Define a rule
        let result = define
            .execute(
                serde_json::json!({
                    "id": "my_rule",
                    "strategy": "rule_based",
                    "threshold": 0.8
                }),
                ToolContext::default(),
            )
            .await
            .unwrap();
        assert_eq!(result["ok"], true);

        // List should include it
        let result = list
            .execute(serde_json::json!({}), ToolContext::default())
            .await
            .unwrap();
        assert!(result.as_array().unwrap().iter().any(|r| r["id"] == "my_rule"));
    }

    #[tokio::test]
    async fn eval_get_results_initially_empty() {
        let engine = Arc::new(RwLock::new(EvalEngine::new()));
        let tool = EvalGetResultsTool::new(engine.clone());

        let result = tool
            .execute(
                serde_json::json!({"limit": 10}),
                ToolContext::default(),
            )
            .await
            .unwrap();
        assert!(result.is_array());
        assert!(result.as_array().unwrap().is_empty());
    }
}
