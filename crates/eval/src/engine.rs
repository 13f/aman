// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! Evaluation engine — central orchestrator that matches targets to rules
//! and dispatches to the appropriate strategy.

use std::collections::HashMap;
use std::sync::Arc;

use crate::config::EvalConfig;
use crate::rule::EvalRule;
use crate::score::EvalScore;
use crate::strategy::EvalStrategy;
use crate::target::EvalTarget;

/// Central orchestrator for the evaluation system.
///
/// Holds registered strategies and rules, and dispatches evaluation
/// targets to all matching enabled rules.
///
/// # Example
///
/// ```ignore
/// let engine = EvalEngine::from_config(&config)?;
/// let results = engine.evaluate(&target).await;
/// ```
pub struct EvalEngine {
    /// All configured rules.
    rules: Vec<EvalRule>,
    /// Strategy implementations, keyed by strategy type name.
    strategies: HashMap<String, Arc<dyn EvalStrategy>>,
    /// Historical evaluation results (bounded by `EvalConfig::max_results`).
    results: Vec<EvalScore>,
    /// Maximum number of results to retain in memory.
    max_results: usize,
    /// Random sampling rate for LLM output evaluation (0.0–1.0).
    sample_rate: f64,
    /// Fast RNG state for sampling decisions (not cryptographic).
    rng_state: u64,
}

impl EvalEngine {
    /// Create a new empty engine.
    #[must_use]
    pub fn new() -> Self {
        Self {
            rules: Vec::new(),
            strategies: HashMap::new(),
            results: Vec::new(),
            max_results: 1000,
            sample_rate: 1.0,
            rng_state: 0xDEAD_BEEF,
        }
    }

    /// Build an engine from an [`EvalConfig`].
    ///
    /// Strategies are registered but rules are NOT automatically added —
    /// call [`set_rules`] or [`add_rule`] after.
    ///
    /// [`set_rules`]: Self::set_rules
    /// [`add_rule`]: Self::add_rule
    #[must_use]
    pub fn with_config(config: &EvalConfig) -> Self {
        let mut engine = Self {
            max_results: config.max_results,
            sample_rate: config.sample_rate,
            ..Self::new()
        };
        let rules: Vec<EvalRule> = config
            .rules
            .iter()
            .cloned()
            .map(|r| {
                let mut rule = r.resolve();
                // Use config default threshold if rule doesn't specify one explicitly
                // (rules always have a threshold from resolve(), but we keep config's default
                // for consistency)
                if rule.threshold == 0.7 && config.default_threshold != 0.7 {
                    rule.threshold = config.default_threshold;
                }
                rule
            })
            .collect();
        engine.set_rules(rules);
        engine
    }

    /// Create a fully initialized engine from config with all built-in strategies registered.
    ///
    /// Call this once at startup. Strategies that need external dependencies
    /// (e.g., an LLM provider for `llm_as_judge`) must be registered separately
    /// via [`register_strategy`] before calling [`evaluate`].
    ///
    /// [`register_strategy`]: Self::register_strategy
    /// [`evaluate`]: Self::evaluate
    #[must_use]
    pub fn from_config(config: &EvalConfig) -> Self {
        Self::with_config(config)
    }

    /// Register or replace a named strategy implementation.
    pub fn register_strategy(&mut self, name: &str, strategy: Arc<dyn EvalStrategy>) {
        self.strategies.insert(name.to_owned(), strategy);
    }

    /// Add a single rule.
    pub fn add_rule(&mut self, rule: EvalRule) {
        self.rules.push(rule);
    }

    /// Replace all rules (e.g., on config reload).
    pub fn set_rules(&mut self, rules: Vec<EvalRule>) {
        self.rules = rules;
    }

    /// Return all registered rules.
    #[must_use]
    pub fn rules(&self) -> &[EvalRule] {
        &self.rules
    }

    /// Return a rule by ID.
    #[must_use]
    pub fn rule_by_id(&self, id: &str) -> Option<&EvalRule> {
        self.rules.iter().find(|r| r.id == id)
    }

    /// Find all enabled rules that apply to the given target kind.
    #[must_use]
    pub fn matching_rules(&self, target: &EvalTarget) -> Vec<&EvalRule> {
        let kind = target.kind();
        self.rules
            .iter()
            .filter(|r| r.enabled && r.applies_to_kind(kind))
            .collect()
    }

    /// Check whether an LLM output should be sampled for evaluation.
    ///
    /// Uses a simple xorshift-based sampling decision. When `sample_rate` is
    /// 1.0, always returns true.
    #[must_use]
    pub fn should_sample(&mut self) -> bool {
        if self.sample_rate >= 1.0 {
            return true;
        }
        // Simple xorshift PRNG for sampling
        self.rng_state ^= self.rng_state.wrapping_shl(13);
        self.rng_state ^= self.rng_state.wrapping_shr(17);
        self.rng_state ^= self.rng_state.wrapping_shl(5);
        let normalized = (self.rng_state as f64) / (u64::MAX as f64);
        normalized < self.sample_rate
    }

    /// Evaluate a target against all matching enabled rules.
    ///
    /// Returns a vector of scores, one per matching rule. Rules whose
    /// strategy is not registered are silently skipped (a warning is logged).
    pub async fn evaluate(&self, target: &EvalTarget) -> Vec<EvalScore> {
        let matching = self.matching_rules(target);
        let mut results = Vec::with_capacity(matching.len());

        for rule in matching {
            let strategy_name = rule.strategy.strategy_type_name();
            match self.strategies.get(strategy_name) {
                Some(strategy) => match strategy.evaluate(target, rule).await {
                    Ok(mut score) => {
                        // Set timestamp if not already set
                        if score.timestamp == 0 {
                            score.timestamp = chrono_now_millis();
                        }
                        results.push(score);
                    }
                    Err(e) => {
                        let error_score = EvalScore::from_error(
                            format!("err-{}", uuid::Uuid::now_v7()),
                            &rule.id,
                            target.id(),
                            strategy_name,
                            e.to_string(),
                        );
                        results.push(error_score);
                    }
                },
                None => {
                    tracing::warn!(
                        strategy = strategy_name,
                        rule_id = %rule.id,
                        "strategy not registered, skipping rule"
                    );
                }
            }
        }

        results
    }

    /// Return a snapshot of recent evaluation results (newest first).
    #[must_use]
    pub fn recent_results(&self) -> &[EvalScore] {
        &self.results
    }

    /// Get results filtered by rule ID.
    #[must_use]
    pub fn results_by_rule(&self, rule_id: &str) -> Vec<&EvalScore> {
        self.results.iter().filter(|r| r.rule_id == rule_id).collect()
    }

    /// Store a result in the in-memory history, respecting `max_results`.
    pub fn store_result(&mut self, score: EvalScore) {
        self.results.push(score);
        if self.results.len() > self.max_results {
            let excess = self.results.len() - self.max_results;
            self.results.drain(0..excess);
        }
    }

    /// Number of registered strategies.
    #[must_use]
    pub fn strategy_count(&self) -> usize {
        self.strategies.len()
    }

    /// Number of configured rules.
    #[must_use]
    pub fn rule_count(&self) -> usize {
        self.rules.len()
    }

    /// Number of stored results.
    #[must_use]
    pub fn result_count(&self) -> usize {
        self.results.len()
    }
}

impl Default for EvalEngine {
    fn default() -> Self {
        Self::new()
    }
}

/// Get current time in milliseconds since Unix epoch.
///
/// Kept as a free function so we can avoid coupling the engine to any
/// particular time library. Uses `std::time::SystemTime`.
fn chrono_now_millis() -> i64 {
    use std::time::SystemTime;
    match SystemTime::now().duration_since(SystemTime::UNIX_EPOCH) {
        Ok(d) => d.as_millis() as i64,
        Err(_) => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::score::ScoredDimension;
    use async_trait::async_trait;
    use kernel::AmanResult;

    /// A mock strategy that always returns a fixed score.
    struct MockStrategy {
        name: &'static str,
        score: f64,
    }

    #[async_trait]
    impl EvalStrategy for MockStrategy {
        fn strategy_type(&self) -> &'static str {
            self.name
        }
        async fn evaluate(&self, target: &EvalTarget, rule: &EvalRule) -> AmanResult<EvalScore> {
            Ok(EvalScore::new(
                uuid::Uuid::now_v7().to_string(),
                &rule.id,
                target.id(),
                self.name,
                vec![ScoredDimension::new("mock", self.score, 1.0)],
                rule.threshold,
            ))
        }
    }

    #[test]
    fn empty_engine_matches_nothing() {
        let engine = EvalEngine::new();
        let target = EvalTarget::LlmOutput {
            content: "test".into(),
            model: None,
            turn: 1,
            query: None,
        };
        assert!(engine.matching_rules(&target).is_empty());
    }

    #[test]
    fn rule_matching_by_target_kind() {
        let mut engine = EvalEngine::new();
        engine.add_rule(EvalRule {
            id: "r1".into(),
            name: "LLM only".into(),
            description: None,
            strategy: crate::strategy::EvalStrategyType::RuleBased {
                patterns: vec![],
            },
            threshold: 0.7,
            enabled: true,
            tags: vec![],
            applies_to: vec!["llm_output".into()],
        });
        engine.add_rule(EvalRule {
            id: "r2".into(),
            name: "All targets".into(),
            description: None,
            strategy: crate::strategy::EvalStrategyType::RuleBased {
                patterns: vec![],
            },
            threshold: 0.7,
            enabled: true,
            tags: vec![],
            applies_to: vec![],
        });

        let llm_target = EvalTarget::LlmOutput {
            content: "hi".into(),
            model: None,
            turn: 1,
            query: None,
        };
        let tool_target = EvalTarget::ToolResult {
            tool_name: "search".into(),
            input: serde_json::Value::Null,
            output: serde_json::Value::String("ok".into()),
        };

        assert_eq!(engine.matching_rules(&llm_target).len(), 2); // r1 + r2
        assert_eq!(engine.matching_rules(&tool_target).len(), 1); // r2 only
    }

    #[test]
    fn disabled_rules_skipped() {
        let mut engine = EvalEngine::new();
        engine.add_rule(EvalRule {
            id: "disabled".into(),
            name: "Disabled rule".into(),
            description: None,
            strategy: crate::strategy::EvalStrategyType::RuleBased {
                patterns: vec![],
            },
            threshold: 0.7,
            enabled: false,
            tags: vec![],
            applies_to: vec![],
        });

        let target = EvalTarget::Custom {
            label: "test".into(),
            content: serde_json::Value::Null,
        };
        assert!(engine.matching_rules(&target).is_empty());
    }

    #[test]
    fn should_sample_always_true_at_rate_1() {
        let mut engine = EvalEngine::new();
        engine.sample_rate = 1.0;
        for _ in 0..100 {
            assert!(engine.should_sample());
        }
    }

    #[test]
    fn store_results_respects_max() {
        let mut engine = EvalEngine::new();
        engine.max_results = 3;
        for i in 0..5 {
            engine.store_result(EvalScore::new(
                format!("id-{i}"),
                "rule",
                "target",
                "mock",
                vec![ScoredDimension::new("x", 0.5, 1.0)],
                0.7,
            ));
        }
        assert_eq!(engine.result_count(), 3);
    }

    #[tokio::test]
    async fn unregistered_strategy_warns_but_doesnt_crash() {
        let mut engine = EvalEngine::new();
        engine.add_rule(EvalRule {
            id: "orphan".into(),
            name: "Orphan rule".into(),
            description: None,
            strategy: crate::strategy::EvalStrategyType::RuleBased {
                patterns: vec![],
            },
            threshold: 0.7,
            enabled: true,
            tags: vec![],
            applies_to: vec![],
        });

        let target = EvalTarget::Custom {
            label: "test".into(),
            content: serde_json::Value::Null,
        };
        let results = engine.evaluate(&target).await;
        // Strategy not registered → no scores produced
        assert!(results.is_empty());
    }

    #[tokio::test]
    async fn evaluate_with_registered_strategy() {
        let mut engine = EvalEngine::new();
        engine.register_strategy("rule_based", Arc::new(MockStrategy { name: "rule_based", score: 0.9 }));
        engine.add_rule(EvalRule {
            id: "test".into(),
            name: "Test".into(),
            description: None,
            strategy: crate::strategy::EvalStrategyType::RuleBased {
                patterns: vec![],
            },
            threshold: 0.7,
            enabled: true,
            tags: vec![],
            applies_to: vec![],
        });

        let target = EvalTarget::Custom {
            label: "test".into(),
            content: serde_json::Value::Null,
        };
        let results = engine.evaluate(&target).await;
        assert_eq!(results.len(), 1);
        assert!((results[0].aggregate_score - 0.9).abs() < 0.001);
    }
}
