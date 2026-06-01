// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! Evaluation score types — dimension scores, outcomes, and aggregated results.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// A single scored dimension within an evaluation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScoredDimension {
    /// Human-readable dimension name (e.g., "correctness", "completeness").
    pub name: String,
    /// Normalized score in [0.0, 1.0].
    pub score: f64,
    /// Weight used during aggregation.
    pub weight: f64,
    /// Optional explanation for this dimension's score.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

impl ScoredDimension {
    /// Create a new scored dimension.
    #[must_use]
    pub fn new(name: impl Into<String>, score: f64, weight: f64) -> Self {
        Self {
            name: name.into(),
            score: score.clamp(0.0, 1.0),
            weight,
            reason: None,
        }
    }

    /// Attach a reason to this dimension.
    #[must_use]
    pub fn with_reason(mut self, reason: impl Into<String>) -> Self {
        self.reason = Some(reason.into());
        self
    }
}

/// Overall pass/fail/error outcome of an evaluation.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum EvalOutcome {
    /// All dimensions passed the threshold.
    Pass,
    /// One or more dimensions fell below the threshold.
    Fail {
        /// The threshold that was applied.
        threshold: f64,
        /// The computed aggregate score.
        aggregate_score: f64,
        /// Names of dimensions that failed.
        failing_dimensions: Vec<String>,
    },
    /// The evaluator itself encountered an error (fail-closed semantics).
    Error {
        /// Description of what went wrong.
        message: String,
    },
}

/// Complete aggregated score for a single evaluation run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvalScore {
    /// Unique ID for this evaluation result.
    pub id: String,
    /// The rule that produced this score.
    pub rule_id: String,
    /// Identifier for what was evaluated (from `EvalTarget::id()`).
    pub target_id: String,
    /// Strategy that produced this score ("rule_based", "llm_as_judge", etc.).
    pub strategy: String,
    /// Per-dimension scores.
    pub dimensions: Vec<ScoredDimension>,
    /// Weighted aggregate across all dimensions [0.0, 1.0].
    pub aggregate_score: f64,
    /// Overall outcome after comparing against threshold.
    pub outcome: EvalOutcome,
    /// The pass threshold that was used.
    pub threshold: f64,
    /// Unix timestamp in milliseconds.
    pub timestamp: i64,
    /// Arbitrary metadata attached by the strategy.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub metadata: HashMap<String, serde_json::Value>,
}

impl EvalScore {
    /// Create a new score from dimensions, computing aggregate and outcome.
    #[must_use]
    pub fn new(
        id: impl Into<String>,
        rule_id: impl Into<String>,
        target_id: impl Into<String>,
        strategy: impl Into<String>,
        dimensions: Vec<ScoredDimension>,
        threshold: f64,
    ) -> Self {
        let aggregate = Self::compute_aggregate(&dimensions);
        let outcome = Self::resolve_outcome(aggregate, threshold, &dimensions);
        Self {
            id: id.into(),
            rule_id: rule_id.into(),
            target_id: target_id.into(),
            strategy: strategy.into(),
            dimensions,
            aggregate_score: aggregate,
            outcome,
            threshold,
            timestamp: 0, // caller should set this
            metadata: HashMap::new(),
        }
    }

    /// Create an error score when the evaluator itself fails.
    #[must_use]
    pub fn from_error(
        id: impl Into<String>,
        rule_id: impl Into<String>,
        target_id: impl Into<String>,
        strategy: impl Into<String>,
        error: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            rule_id: rule_id.into(),
            target_id: target_id.into(),
            strategy: strategy.into(),
            dimensions: Vec::new(),
            aggregate_score: 0.0,
            outcome: EvalOutcome::Error {
                message: error.into(),
            },
            threshold: 0.0,
            timestamp: 0,
            metadata: HashMap::new(),
        }
    }

    /// Compute the weighted aggregate from a set of dimensions.
    #[must_use]
    pub fn compute_aggregate(dimensions: &[ScoredDimension]) -> f64 {
        let total_weight: f64 = dimensions.iter().map(|d| d.weight).sum();
        if total_weight <= 0.0 {
            return 0.0;
        }
        let weighted_sum: f64 = dimensions.iter().map(|d| d.score * d.weight).sum();
        (weighted_sum / total_weight).clamp(0.0, 1.0)
    }

    /// Determine the pass/fail outcome from an aggregate score and threshold.
    #[must_use]
    pub fn resolve_outcome(
        aggregate: f64,
        threshold: f64,
        dimensions: &[ScoredDimension],
    ) -> EvalOutcome {
        if aggregate >= threshold {
            EvalOutcome::Pass
        } else {
            let failing: Vec<String> = dimensions
                .iter()
                .filter(|d| d.score < threshold)
                .map(|d| d.name.clone())
                .collect();
            EvalOutcome::Fail {
                threshold,
                aggregate_score: aggregate,
                failing_dimensions: failing,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aggregate_weighted_average() {
        let dims = vec![
            ScoredDimension::new("a", 1.0, 0.5),
            ScoredDimension::new("b", 0.0, 0.5),
        ];
        assert!((EvalScore::compute_aggregate(&dims) - 0.5).abs() < 0.001);
    }

    #[test]
    fn aggregate_zero_total_weight() {
        let dims = vec![
            ScoredDimension::new("a", 1.0, 0.0),
            ScoredDimension::new("b", 0.0, 0.0),
        ];
        assert!((EvalScore::compute_aggregate(&dims) - 0.0).abs() < 0.001);
    }

    #[test]
    fn outcome_pass() {
        let dims = vec![ScoredDimension::new("a", 0.9, 1.0)];
        let outcome = EvalScore::resolve_outcome(0.9, 0.7, &dims);
        assert!(matches!(outcome, EvalOutcome::Pass));
    }

    #[test]
    fn outcome_fail() {
        let dims = vec![ScoredDimension::new("a", 0.3, 1.0)];
        let outcome = EvalScore::resolve_outcome(0.3, 0.7, &dims);
        assert!(matches!(outcome, EvalOutcome::Fail { .. }));
    }

    #[test]
    fn dim_score_clamped() {
        let dim = ScoredDimension::new("x", 1.5, 1.0);
        assert!((dim.score - 1.0).abs() < 0.001);
        let dim = ScoredDimension::new("x", -0.5, 1.0);
        assert!((dim.score - 0.0).abs() < 0.001);
    }
}
