// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! Evaluation strategies — the different ways to score a target.
//!
//! Four built-in strategy types:
//! 1. `rule_based` — substring/regex pattern matching
//! 2. `assertion` — structural assertions on JSON content
//! 3. `heuristic` — weighted combination of extracted signals
//! 4. `llm_as_judge` — use a separate LLM to score the output

use async_trait::async_trait;
use kernel::AmanResult;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::rule::EvalRule;
use crate::score::EvalScore;
use crate::target::EvalTarget;

// ── Strategy trait ──────────────────────────────────────────────────────

/// The core trait that every evaluation strategy must implement.
#[async_trait]
pub trait EvalStrategy: Send + Sync {
    /// A short unique name for this strategy type (e.g., "rule_based").
    fn strategy_type(&self) -> &'static str;

    /// Evaluate a target against a rule using this strategy.
    async fn evaluate(&self, target: &EvalTarget, rule: &EvalRule) -> AmanResult<EvalScore>;
}

// ── Strategy type enum (for config deserialization) ────────────────────

/// Discriminated strategy definition, deserialized from YAML config.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EvalStrategyType {
    /// Pattern-matching strategy.
    RuleBased {
        /// Patterns to match against the target's text content.
        #[serde(default)]
        patterns: Vec<EvalPattern>,
    },
    /// Structural assertions on JSON content.
    Assertion {
        /// Assertions to check.
        #[serde(default)]
        assertions: Vec<EvalAssertion>,
    },
    /// Heuristic signal extraction and weighted combination.
    Heuristic {
        /// Factors to extract from the target.
        #[serde(default)]
        factors: Vec<HeuristicFactor>,
        /// How to map factors to scored dimensions.
        #[serde(default)]
        dimension_scores: Vec<HeuristicDimensionMapping>,
    },
    /// Use a separate LLM to judge the output.
    LlmAsJudge {
        /// Prompt template with `{{output}}`, `{{query}}`, `{{dimensions}}` placeholders.
        prompt_template: String,
        /// Dimensions the judge should score.
        #[serde(default)]
        dimensions: Vec<EvalDimension>,
        /// Optional override for the judge model.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model_override: Option<String>,
        /// Temperature for the judge LLM (lower = more consistent).
        #[serde(default = "default_judge_temperature")]
        temperature: f64,
    },
}

impl EvalStrategyType {
    /// Return the strategy type name for lookup in the engine's registry.
    #[must_use]
    pub fn strategy_type_name(&self) -> &'static str {
        match self {
            Self::RuleBased { .. } => "rule_based",
            Self::Assertion { .. } => "assertion",
            Self::Heuristic { .. } => "heuristic",
            Self::LlmAsJudge { .. } => "llm_as_judge",
        }
    }
}

const fn default_judge_temperature() -> f64 {
    0.3
}

// ── Rule-Based subtypes ─────────────────────────────────────────────────

/// A single pattern to match against target content.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvalPattern {
    /// Human-readable name for this pattern.
    pub name: String,
    /// Match via regex (preferred over substring for complex patterns).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub regex: Option<String>,
    /// Match via simple substring (case-insensitive).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub substring: Option<String>,
    /// Score assigned when this pattern matches [0.0, 1.0].
    pub score_if_matched: f64,
    /// Score assigned when this pattern does NOT match [0.0, 1.0].
    #[serde(default)]
    pub score_if_not_matched: f64,
    /// If true, a failure to match (for expected-good) or a match (for
    /// expected-bad) immediately causes a Fail outcome regardless of
    /// other dimension scores.
    #[serde(default)]
    pub required: bool,
}

// ── Assertion subtypes ──────────────────────────────────────────────────

/// A structural assertion to check against JSON content.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EvalAssertion {
    /// Check that a field exists at the given JSON pointer path.
    HasField {
        /// JSON pointer path, e.g., "/status", "/data/items".
        path: String,
    },
    /// Check that a field equals an expected value.
    FieldEquals {
        path: String,
        value: Value,
    },
    /// Check that a field has a specific JSON type.
    FieldType {
        path: String,
        /// Expected JSON type: "string", "number", "boolean", "object", "array", "null".
        json_type: String,
    },
    /// Check that a string or array field's length is within bounds.
    LengthBetween {
        path: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        min: Option<usize>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max: Option<usize>,
    },
    /// Check that a string field matches a regex pattern.
    RegexMatch {
        path: String,
        pattern: String,
    },
}

// ── Heuristic subtypes ──────────────────────────────────────────────────

/// A weighted signal to extract from the target.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HeuristicFactor {
    /// Factor name (used in dimension mappings).
    pub name: String,
    /// Weight of this factor when combined.
    pub weight: f64,
    /// How to extract the raw signal.
    pub extractor: HeuristicExtractor,
}

/// How to extract a numeric signal from a target.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HeuristicExtractor {
    /// Count occurrences of a pattern in the text content.
    CountMatches {
        pattern: String,
        #[serde(default)]
        is_regex: bool,
    },
    /// Score based on content length relative to bounds.
    ContentLength {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        min: Option<usize>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max: Option<usize>,
    },
    /// Check that specific keys exist in a JSON value.
    HasKeys {
        keys: Vec<String>,
    },
    /// Compute keyword density (ratio of keyword tokens to total tokens).
    KeywordDensity {
        keywords: Vec<String>,
    },
    /// Delegate to a custom named extractor registered with the engine.
    Custom {
        name: String,
    },
}

/// Maps one or more heuristic factors to a named scored dimension.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HeuristicDimensionMapping {
    /// The dimension name in the final score.
    pub dimension_name: String,
    /// Weight of this dimension in the aggregate.
    pub weight: f64,
    /// Which factor names feed into this dimension.
    pub factor_names: Vec<String>,
}

// ── LLM-as-Judge subtypes ───────────────────────────────────────────────

/// Metadata for a single dimension the judge LLM should score.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvalDimension {
    /// Short name (e.g., "correctness").
    pub name: String,
    /// Weight in the aggregate [0.0, 1.0].
    pub weight: f64,
    /// Human-readable description of what this dimension measures.
    pub description: String,
}

// ── Composite (future) ──────────────────────────────────────────────────

/// How to combine sub-scores in a composite strategy (reserved for future use).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AggregationMethod {
    /// Weighted arithmetic mean.
    WeightedAverage,
    /// Take the minimum across sub-scores.
    Minimum,
    /// Take the maximum across sub-scores.
    Maximum,
    /// Multiply sub-scores together.
    Product,
}
