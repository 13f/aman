// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! Rule-based evaluation strategy — substring and regex pattern matching.

use async_trait::async_trait;
use kernel::AmanResult;

use crate::rule::EvalRule;
use crate::score::{EvalScore, ScoredDimension};
use crate::strategy::{EvalPattern, EvalStrategy, EvalStrategyType};
use crate::target::EvalTarget;

/// Rule-based strategy: match patterns against target text content.
pub struct RuleBasedStrategy;

#[async_trait]
impl EvalStrategy for RuleBasedStrategy {
    fn strategy_type(&self) -> &'static str {
        "rule_based"
    }

    async fn evaluate(&self, target: &EvalTarget, rule: &EvalRule) -> AmanResult<EvalScore> {
        let EvalStrategyType::RuleBased { patterns } = &rule.strategy else {
            return Err(kernel::Error::Unrecoverable {
                message: "expected RuleBased strategy type".into(),
            });
        };

        let text = target.text_content().unwrap_or("");
        let lower = text.to_lowercase();
        let mut dimensions = Vec::with_capacity(patterns.len());

        for pattern in patterns {
            let matched = Self::matches_pattern(pattern, &lower);
            let score = if matched {
                pattern.score_if_matched
            } else {
                pattern.score_if_not_matched
            };

            dimensions.push(ScoredDimension {
                name: pattern.name.clone(),
                score,
                weight: if pattern.required { 2.0 } else { 1.0 },
                reason: Some(if matched {
                    "pattern matched".into()
                } else {
                    "pattern not matched".into()
                }),
            });

            // Required patterns that fail → immediate Fail
            if pattern.required && !matched {
                let id = uuid::Uuid::now_v7().to_string();
                let mut score = EvalScore::new(
                    id,
                    &rule.id,
                    target.id(),
                    "rule_based",
                    dimensions,
                    rule.threshold,
                );
                score.outcome = crate::score::EvalOutcome::Fail {
                    threshold: rule.threshold,
                    aggregate_score: 0.0,
                    failing_dimensions: vec![pattern.name.clone()],
                };
                return Ok(score);
            }
        }

        let id = uuid::Uuid::now_v7().to_string();
        Ok(EvalScore::new(
            id,
            &rule.id,
            target.id(),
            "rule_based",
            dimensions,
            rule.threshold,
        ))
    }
}

impl RuleBasedStrategy {
    fn matches_pattern(pattern: &EvalPattern, lower_text: &str) -> bool {
        if let Some(regex_str) = &pattern.regex {
            // Build regex once — in production we'd cache these, but for
            // startup-time validation the cost is fine.
            match regex::Regex::new(regex_str) {
                Ok(re) => re.is_match(lower_text),
                Err(e) => {
                    tracing::warn!(
                        pattern = %pattern.name,
                        regex = %regex_str,
                        error = %e,
                        "invalid regex in eval pattern, treating as non-match"
                    );
                    false
                }
            }
        } else if let Some(sub) = &pattern.substring {
            lower_text.contains(&sub.to_lowercase())
        } else {
            // No matcher defined — treat as pass
            true
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rule::EvalRule;
    use crate::strategy::EvalPattern;
    use crate::target::EvalTarget;

    fn llm_target(content: &str) -> EvalTarget {
        EvalTarget::LlmOutput {
            content: content.into(),
            model: None,
            turn: 1,
            query: None,
        }
    }

    fn make_rule(patterns: Vec<EvalPattern>) -> EvalRule {
        EvalRule {
            id: "test".into(),
            name: "Test Rule".into(),
            description: None,
            strategy: EvalStrategyType::RuleBased { patterns },
            threshold: 0.7,
            enabled: true,
            tags: vec![],
            applies_to: vec![],
        }
    }

    #[tokio::test]
    async fn substring_match() {
        let strategy = RuleBasedStrategy;
        let rule = make_rule(vec![EvalPattern {
            name: "no_secrets".into(),
            substring: Some("sk-".into()),
            regex: None,
            score_if_matched: 0.0,
            score_if_not_matched: 1.0,
            required: false,
        }]);

        let target = llm_target("Here is my key: sk-proj-abc123");
        let score = strategy.evaluate(&target, &rule).await.unwrap();
        assert_eq!(score.dimensions[0].score, 0.0);
        assert!(score.dimensions[0].name == "no_secrets");
    }

    #[tokio::test]
    async fn clean_output_passes() {
        let strategy = RuleBasedStrategy;
        let rule = make_rule(vec![EvalPattern {
            name: "no_secrets".into(),
            substring: Some("sk-".into()),
            regex: None,
            score_if_matched: 0.0,
            score_if_not_matched: 1.0,
            required: false,
        }]);

        let target = llm_target("This is a safe response.");
        let score = strategy.evaluate(&target, &rule).await.unwrap();
        assert_eq!(score.dimensions[0].score, 1.0);
    }

    #[tokio::test]
    async fn required_pattern_immediate_fail() {
        let strategy = RuleBasedStrategy;
        let rule = make_rule(vec![EvalPattern {
            name: "must_have_solution".into(),
            substring: Some("SOLUTION".into()),
            regex: None,
            score_if_matched: 1.0,
            score_if_not_matched: 0.0,
            required: true,
        }]);

        let target = llm_target("This response is missing the keyword.");
        let score = strategy.evaluate(&target, &rule).await.unwrap();
        assert!(matches!(score.outcome, crate::score::EvalOutcome::Fail { .. }));
    }

    #[tokio::test]
    async fn regex_pattern() {
        let strategy = RuleBasedStrategy;
        let rule = make_rule(vec![EvalPattern {
            name: "has_number".into(),
            substring: None,
            regex: Some(r"\d+".into()),
            score_if_matched: 1.0,
            score_if_not_matched: 0.0,
            required: false,
        }]);

        let target = llm_target("There are 42 items.");
        let score = strategy.evaluate(&target, &rule).await.unwrap();
        assert_eq!(score.dimensions[0].score, 1.0);
    }

    #[tokio::test]
    async fn invalid_regex_treated_as_non_match() {
        let strategy = RuleBasedStrategy;
        let rule = make_rule(vec![EvalPattern {
            name: "bad_regex".into(),
            substring: None,
            regex: Some("[invalid".into()),
            score_if_matched: 1.0,
            score_if_not_matched: 0.0,
            required: false,
        }]);

        let target = llm_target("anything");
        let score = strategy.evaluate(&target, &rule).await.unwrap();
        // Invalid regex → treated as non-match → score_if_not_matched wins
        assert_eq!(score.dimensions[0].score, 0.0);
    }
}
