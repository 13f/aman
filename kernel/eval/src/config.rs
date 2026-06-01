// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! Evaluation system configuration.

use serde::{Deserialize, Serialize};

use crate::rule::EvalConfigRule;

/// Top-level evaluation system configuration.
///
/// Lives under the `eval:` key in `AmanConfig`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct EvalConfig {
    /// Master enable/disable switch.
    pub enabled: bool,

    /// Default pass threshold for rules that don't specify one.
    #[serde(default = "default_threshold")]
    pub default_threshold: f64,

    /// Automatically evaluate outputs via hooks (ToolExecuted, PipelineCompleted, etc.).
    #[serde(default = "default_true")]
    pub auto_evaluate: bool,

    /// Persist evaluation results to the event store.
    #[serde(default = "default_true")]
    pub persist_results: bool,

    /// Maximum number of evaluation results to keep in the in-memory history.
    #[serde(default = "default_max_results")]
    pub max_results: usize,

    /// LLM-as-judge configuration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub llm: Option<JudgeLlmConfig>,

    /// Evaluation rules.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rules: Vec<EvalConfigRule>,

    /// How often to evaluate LLM outputs (1.0 = every output, 0.1 = 10%).
    /// Useful for reducing LLM-as-judge costs.
    #[serde(default = "default_sample_rate")]
    pub sample_rate: f64,
}

fn default_threshold() -> f64 {
    0.7
}
fn default_true() -> bool {
    true
}
const fn default_max_results() -> usize {
    1000
}
fn default_sample_rate() -> f64 {
    1.0
}

impl Default for EvalConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            default_threshold: 0.7,
            auto_evaluate: true,
            persist_results: true,
            max_results: 1000,
            llm: None,
            rules: Vec::new(),
            sample_rate: 1.0,
        }
    }
}

impl EvalConfig {
    /// Validate the configuration, returning a list of issues.
    #[must_use]
    pub fn validate(&self) -> Vec<String> {
        let mut issues = Vec::new();
        if self.default_threshold < 0.0 || self.default_threshold > 1.0 {
            issues.push(format!(
                "default_threshold {} is out of range [0.0, 1.0]",
                self.default_threshold
            ));
        }
        if self.sample_rate <= 0.0 || self.sample_rate > 1.0 {
            issues.push(format!(
                "sample_rate {} is out of range (0.0, 1.0]",
                self.sample_rate
            ));
        }
        for rule in &self.rules {
            if rule.threshold < 0.0 || rule.threshold > 1.0 {
                issues.push(format!(
                    "rule '{}' threshold {} is out of range [0.0, 1.0]",
                    rule.id, rule.threshold
                ));
            }
        }
        issues
    }
}

/// Configuration for the judge LLM used by the `llm_as_judge` strategy.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JudgeLlmConfig {
    /// Provider name (e.g., "deepseek", "openai").
    pub provider: String,
    /// Model ID (e.g., "deepseek-v4-flash").
    pub model: String,
    /// Temperature for judge calls (lower = more consistent scoring).
    #[serde(default = "default_judge_temperature")]
    pub temperature: f64,
    /// Optional provider-specific base URL override.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    /// Optional API key (falls back to provider config if not set).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
}

fn default_judge_temperature() -> f64 {
    0.3
}
