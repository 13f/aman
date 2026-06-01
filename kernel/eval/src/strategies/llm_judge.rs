// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! LLM-as-Judge evaluation strategy — use a separate LLM to score output quality.
//!
//! This strategy sends a templated prompt containing the target output to a
//! judge LLM, asking it to score the output across multiple dimensions.
//!
//! The judge LLM configuration comes from the `EvalConfig::llm` field, with
//! per-rule overrides available via `model_override` and `temperature`.

use async_trait::async_trait;
use kernel::AmanResult;
use serde_json::Value;

use crate::config::JudgeLlmConfig;
use crate::rule::EvalRule;
use crate::score::{EvalScore, ScoredDimension};
use crate::strategy::{EvalDimension, EvalStrategy, EvalStrategyType};
use crate::target::EvalTarget;

/// LLM-as-Judge strategy.
///
/// Requires an [`LlmJudgeExecutor`] to be provided — this is the bridge
/// that actually calls the LLM. By abstracting it behind a trait, the
/// eval crate stays decoupled from any specific LLM provider implementation.
pub struct LlmJudgeStrategy {
    /// The executor that performs the actual LLM call.
    executor: Option<Box<dyn LlmJudgeExecutor>>,
    /// Default judge configuration.
    default_config: Option<JudgeLlmConfig>,
}

impl LlmJudgeStrategy {
    /// Create a new strategy with the given executor and default config.
    #[must_use]
    pub fn new(
        executor: Option<Box<dyn LlmJudgeExecutor>>,
        default_config: Option<JudgeLlmConfig>,
    ) -> Self {
        Self {
            executor,
            default_config,
        }
    }

    /// Create a strategy without an executor (no-op: returns error scores).
    #[must_use]
    pub fn noop() -> Self {
        Self {
            executor: None,
            default_config: None,
        }
    }
}

/// Trait abstracting the actual LLM call for the judge.
///
/// Implementations connect to real LLM providers (OpenAI, DeepSeek, etc.)
/// and are provided by the runtime during startup.
#[async_trait]
pub trait LlmJudgeExecutor: Send + Sync {
    /// Send a prompt to the judge LLM and return the raw text response.
    async fn judge(
        &self,
        system_prompt: &str,
        user_prompt: &str,
        model: &str,
        temperature: f64,
    ) -> AmanResult<String>;
}

// ── Built-in executor: uses cognitive-llm ───────────────────────────

/// An [`LlmJudgeExecutor`] backed by [`cognitive_llm::simple::SimpleLlmClient`].
///
/// This is the default executor used in production. It takes an
/// [`cognitive_llm::simple::LlmApiConfig`] describing the judge endpoint and delegates
/// to [`SimpleLlmClient::chat_completion_with_retries`].
pub struct LlmApiJudgeExecutor {
    provider: cognitive_llm::simple::SimpleLlmClient,
    config: cognitive_llm::simple::LlmApiConfig,
    max_tokens: u64,
    timeout_secs: u64,
    retries: u32,
}

impl LlmApiJudgeExecutor {
    /// Create a new executor from a [`cognitive_llm::simple::LlmApiConfig`].
    #[must_use]
    pub fn new(config: cognitive_llm::simple::LlmApiConfig) -> Self {
        Self {
            provider: cognitive_llm::simple::SimpleLlmClient::new(),
            config,
            max_tokens: 1024,
            timeout_secs: 60,
            retries: 3,
        }
    }

    /// Create a new executor from individual fields — avoids the caller
    /// needing to depend on `cognitive_llm` directly.
    #[must_use]
    pub fn from_parts(
        base_url: String,
        api_key: Option<String>,
        model: String,
    ) -> Self {
        Self::new(cognitive_llm::simple::LlmApiConfig {
            base_url,
            api_key,
            model,
        })
    }

    /// Set max output tokens for judge calls (default 1024).
    #[must_use]
    pub fn with_max_tokens(mut self, max_tokens: u64) -> Self {
        self.max_tokens = max_tokens;
        self
    }

    /// Set timeout in seconds (default 60).
    #[must_use]
    pub fn with_timeout(mut self, timeout_secs: u64) -> Self {
        self.timeout_secs = timeout_secs;
        self
    }

    /// Set retry count (default 3).
    #[must_use]
    pub fn with_retries(mut self, retries: u32) -> Self {
        self.retries = retries;
        self
    }
}

#[async_trait]
impl LlmJudgeExecutor for LlmApiJudgeExecutor {
    async fn judge(
        &self,
        system_prompt: &str,
        user_prompt: &str,
        _model: &str,
        temperature: f64,
    ) -> AmanResult<String> {
        self.provider
            .chat_completion_with_retries(
                &self.config,
                system_prompt,
                user_prompt,
                temperature,
                self.max_tokens,
                self.timeout_secs,
                self.retries,
            )
            .await
            .map_err(|e| kernel::Error::Unrecoverable {
                message: format!("judge LLM call failed: {e}"),
            })
    }
}

#[async_trait]
impl EvalStrategy for LlmJudgeStrategy {
    fn strategy_type(&self) -> &'static str {
        "llm_as_judge"
    }

    async fn evaluate(&self, target: &EvalTarget, rule: &EvalRule) -> AmanResult<EvalScore> {
        let EvalStrategyType::LlmAsJudge {
            prompt_template,
            dimensions,
            model_override,
            temperature,
        } = &rule.strategy
        else {
            return Err(kernel::Error::Unrecoverable {
                message: "expected LlmAsJudge strategy type".into(),
            });
        };

        // If no executor is configured, return an error score
        let executor = match &self.executor {
            Some(e) => e,
            None => {
                return Ok(EvalScore::from_error(
                    uuid::Uuid::now_v7().to_string(),
                    &rule.id,
                    target.id(),
                    "llm_as_judge",
                    "no judge LLM executor configured",
                ));
            }
        };

        // Build the dimensions description for the prompt
        let dims_desc = build_dimensions_description(dimensions);

        // Render the prompt template
        let text = target.text_content().unwrap_or("");
        let query = match target {
            EvalTarget::LlmOutput {
                query: Some(q), ..
            } => q.as_str(),
            _ => "",
        };
        let rendered = prompt_template
            .replace("{{output}}", text)
            .replace("{{query}}", query)
            .replace("{{dimensions}}", &dims_desc);

        // Determine model and temperature
        let model = model_override
            .clone()
            .or_else(|| self.default_config.as_ref().map(|c| c.model.clone()))
            .unwrap_or_else(|| "deepseek-v4-flash".into());
        let temp = *temperature;

        // Call the judge LLM
        let system = "You are an impartial evaluator. Always respond with valid JSON only.";
        match executor.judge(system, &rendered, &model, temp).await {
            Ok(response) => {
                // Parse JSON response
                match parse_judge_response(&response, dimensions, rule) {
                    Ok(mut score) => {
                        score.strategy = "llm_as_judge".into();
                        Ok(score)
                    }
                    Err(e) => Ok(EvalScore::from_error(
                        uuid::Uuid::now_v7().to_string(),
                        &rule.id,
                        target.id(),
                        "llm_as_judge",
                        format!("failed to parse judge response: {e}"),
                    )),
                }
            }
            Err(e) => Ok(EvalScore::from_error(
                uuid::Uuid::now_v7().to_string(),
                &rule.id,
                target.id(),
                "llm_as_judge",
                format!("judge LLM call failed: {e}"),
            )),
        }
    }
}

/// Build a human-readable description of the scoring dimensions for the prompt.
fn build_dimensions_description(dimensions: &[EvalDimension]) -> String {
    let mut out = String::from("Scoring dimensions:\n");
    for dim in dimensions {
        out.push_str(&format!(
            "- {} (weight: {}): {}\n",
            dim.name, dim.weight, dim.description
        ));
    }
    out
}

/// Parse the judge LLM's JSON response into an EvalScore.
fn parse_judge_response(
    response: &str,
    dimensions: &[EvalDimension],
    rule: &EvalRule,
) -> Result<EvalScore, String> {
    // Try to extract JSON from the response (handle markdown fences)
    let json_str = extract_json_block(response);

    let parsed: Value =
        serde_json::from_str(&json_str).map_err(|e| format!("invalid JSON: {e}"))?;

    // Extract scores object
    let scores_obj = parsed
        .get("scores")
        .ok_or("missing 'scores' field in judge response")?;

    let reasoning = parsed
        .get("reasoning")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    let mut scored_dims = Vec::with_capacity(dimensions.len());
    for dim in dimensions {
        let raw_score = scores_obj
            .get(&dim.name)
            .and_then(|v| v.as_f64())
            .unwrap_or(0.5); // default to middle if missing

        scored_dims.push(ScoredDimension {
            name: dim.name.clone(),
            score: raw_score.clamp(0.0, 1.0),
            weight: dim.weight,
            reason: if reasoning.is_empty() {
                None
            } else {
                Some(reasoning.to_owned())
            },
        });
    }

    let id = uuid::Uuid::now_v7().to_string();
    Ok(EvalScore::new(
        id,
        &rule.id,
        "llm_judge_target",
        "llm_as_judge",
        scored_dims,
        rule.threshold,
    ))
}

/// Extract a JSON block from text that may be wrapped in markdown fences.
fn extract_json_block(text: &str) -> String {
    let trimmed = text.trim();

    // Try ```json ... ``` block
    if let Some(inner) = trimmed
        .strip_prefix("```json")
        .and_then(|s| s.strip_suffix("```"))
    {
        return inner.trim().to_owned();
    }
    // Try ``` ... ``` block
    if let Some(inner) = trimmed
        .strip_prefix("```")
        .and_then(|s| s.strip_suffix("```"))
    {
        return inner.trim().to_owned();
    }
    // Return as-is
    trimmed.to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_json_plain() {
        let input = r#"{"scores": {"correctness": 0.8}}"#;
        let result = extract_json_block(input);
        assert_eq!(result, input);
    }

    #[test]
    fn extract_json_fenced() {
        let input = "```json\n{\"scores\": {\"correctness\": 0.8}}\n```";
        let result = extract_json_block(input);
        assert_eq!(result, "{\"scores\": {\"correctness\": 0.8}}");
    }

    #[test]
    fn extract_json_generic_fence() {
        let input = "```\n{\"scores\": {\"correctness\": 0.8}}\n```";
        let result = extract_json_block(input);
        assert_eq!(result, "{\"scores\": {\"correctness\": 0.8}}");
    }

    #[test]
    fn parse_valid_judge_response() {
        let dims = vec![
            EvalDimension {
                name: "correctness".into(),
                weight: 0.6,
                description: "Accuracy".into(),
            },
            EvalDimension {
                name: "clarity".into(),
                weight: 0.4,
                description: "Clarity".into(),
            },
        ];
        let rule = EvalRule {
            id: "test".into(),
            name: "Test".into(),
            description: None,
            strategy: EvalStrategyType::LlmAsJudge {
                prompt_template: "".into(),
                dimensions: dims.clone(),
                model_override: None,
                temperature: 0.3,
            },
            threshold: 0.7,
            enabled: true,
            tags: vec![],
            applies_to: vec![],
        };

        let response = r#"{"scores": {"correctness": 0.9, "clarity": 0.8}, "reasoning": "Good response"}"#;
        let score = parse_judge_response(response, &dims, &rule).unwrap();
        assert_eq!(score.dimensions.len(), 2);
        assert!((score.dimensions[0].score - 0.9).abs() < 0.001);
        assert!((score.dimensions[1].score - 0.8).abs() < 0.001);
    }

    #[test]
    fn parse_response_missing_dimension_defaults_to_0_5() {
        let dims = vec![EvalDimension {
            name: "missing_dim".into(),
            weight: 1.0,
            description: "test".into(),
        }];
        let rule = EvalRule {
            id: "test".into(),
            name: "Test".into(),
            description: None,
            strategy: EvalStrategyType::LlmAsJudge {
                prompt_template: "".into(),
                dimensions: dims.clone(),
                model_override: None,
                temperature: 0.3,
            },
            threshold: 0.7,
            enabled: true,
            tags: vec![],
            applies_to: vec![],
        };

        let response = r#"{"scores": {}, "reasoning": "no scores"}"#;
        let score = parse_judge_response(response, &dims, &rule).unwrap();
        assert!((score.dimensions[0].score - 0.5).abs() < 0.001);
    }
}
