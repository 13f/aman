// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! Evaluation rules — named, configurable evaluation specifications.

use serde::{Deserialize, Serialize};

use crate::strategy::EvalStrategyType;

/// A fully resolved evaluation rule, ready for use by the engine.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvalRule {
    /// Unique rule identifier.
    pub id: String,
    /// Human-readable name.
    pub name: String,
    /// Optional longer description of what this rule checks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// The strategy and its parameters.
    pub strategy: EvalStrategyType,
    /// Minimum aggregate score to pass (0.0–1.0).
    pub threshold: f64,
    /// Whether this rule is active.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Optional tags for filtering and grouping.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    /// Which target kinds this rule applies to (empty = all).
    /// Values: "llm_output", "tool_result", "task_result", "pipeline_result", "custom".
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub applies_to: Vec<String>,
}

const fn default_true() -> bool {
    true
}

impl EvalRule {
    /// Check whether this rule applies to a given target kind.
    #[must_use]
    pub fn applies_to_kind(&self, kind: &str) -> bool {
        self.applies_to.is_empty() || self.applies_to.iter().any(|k| k == kind)
    }
}

// ── Config-stage rule (before resolution) ───────────────────────────────

/// A rule in its raw config form (as it appears in YAML).
///
/// Some fields are optional at config time and filled with defaults
/// when resolved into an [`EvalRule`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvalConfigRule {
    /// Unique rule identifier.
    pub id: String,
    /// Human-readable name (defaults to `id` if omitted).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Optional description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// The strategy and its parameters.
    pub strategy: EvalStrategyType,
    /// Pass threshold [0.0, 1.0].
    #[serde(default = "default_threshold")]
    pub threshold: f64,
    /// Whether this rule is active.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Tags for filtering.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    /// Target kinds this rule applies to.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub applies_to: Vec<String>,
}

fn default_threshold() -> f64 {
    0.7
}

impl EvalConfigRule {
    /// Resolve into a fully-populated [`EvalRule`].
    #[must_use]
    pub fn resolve(self) -> EvalRule {
        EvalRule {
            name: self.name.unwrap_or_else(|| self.id.clone()),
            id: self.id,
            description: self.description,
            strategy: self.strategy,
            threshold: self.threshold.clamp(0.0, 1.0),
            enabled: self.enabled,
            tags: self.tags,
            applies_to: self.applies_to,
        }
    }
}
