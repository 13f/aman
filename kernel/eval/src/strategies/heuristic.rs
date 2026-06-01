// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! Heuristic evaluation strategy — extract quantifiable signals and combine them.

use async_trait::async_trait;
use kernel::AmanResult;
use regex::Regex;
use serde_json::Value;
use std::collections::HashMap;

use crate::rule::EvalRule;
use crate::score::{EvalScore, ScoredDimension};
use crate::strategy::{
    EvalStrategy, EvalStrategyType, HeuristicExtractor, HeuristicFactor,
};
use crate::target::EvalTarget;

/// Heuristic strategy: extract signals and compute weighted scores.
pub struct HeuristicStrategy;

#[async_trait]
impl EvalStrategy for HeuristicStrategy {
    fn strategy_type(&self) -> &'static str {
        "heuristic"
    }

    async fn evaluate(&self, target: &EvalTarget, rule: &EvalRule) -> AmanResult<EvalScore> {
        let EvalStrategyType::Heuristic {
            factors,
            dimension_scores,
        } = &rule.strategy
        else {
            return Err(kernel::Error::Unrecoverable {
                message: "expected Heuristic strategy type".into(),
            });
        };

        let text = target.text_content().unwrap_or("");
        let json = extract_json_value(target);

        // Step 1: Extract raw factor values (0.0–1.0)
        let mut factor_values: HashMap<String, f64> = HashMap::with_capacity(factors.len());
        for factor in factors {
            let value = extract_factor(factor, text, &json);
            factor_values.insert(factor.name.clone(), value);
        }

        // Step 2: Map factors to dimensions
        let mut dimensions = Vec::with_capacity(dimension_scores.len());
        for mapping in dimension_scores {
            let mut weighted_sum = 0.0;
            let mut total_weight = 0.0;
            for fname in &mapping.factor_names {
                if let Some(&value) = factor_values.get(fname) {
                    // Find the factor weight
                    let fweight = factors
                        .iter()
                        .find(|f| &f.name == fname)
                        .map_or(1.0, |f| f.weight);
                    weighted_sum += value * fweight;
                    total_weight += fweight;
                }
            }
            let score = if total_weight > 0.0 {
                (weighted_sum / total_weight).clamp(0.0, 1.0)
            } else {
                1.0
            };
            dimensions.push(ScoredDimension {
                name: mapping.dimension_name.clone(),
                score,
                weight: mapping.weight,
                reason: None,
            });
        }

        // If no dimension mappings, fall back to one dimension per factor
        if dimensions.is_empty() {
            for factor in factors {
                if let Some(&value) = factor_values.get(&factor.name) {
                    dimensions.push(ScoredDimension {
                        name: factor.name.clone(),
                        score: value,
                        weight: factor.weight,
                        reason: None,
                    });
                }
            }
        }

        let id = uuid::Uuid::now_v7().to_string();
        Ok(EvalScore::new(
            id,
            &rule.id,
            target.id(),
            "heuristic",
            dimensions,
            rule.threshold,
        ))
    }
}

/// Extract a JSON value from the target for JSON-aware extractors.
fn extract_json_value(target: &EvalTarget) -> Value {
    match target {
        EvalTarget::ToolResult { output, .. }
        | EvalTarget::TaskResult { result: output, .. }
        | EvalTarget::PipelineResult { output, .. }
        | EvalTarget::Custom { content: output, .. } => output.clone(),
        EvalTarget::LlmOutput { content, .. } => {
            serde_json::from_str(content).unwrap_or(Value::Null)
        }
    }
}

/// Extract a single heuristic factor value (0.0–1.0).
fn extract_factor(factor: &HeuristicFactor, text: &str, json: &Value) -> f64 {
    match &factor.extractor {
        HeuristicExtractor::CountMatches { pattern, is_regex } => {
            if *is_regex {
                match Regex::new(pattern) {
                    Ok(re) => {
                        let count = re.find_iter(text).count();
                        // Normalize: 0 matches → 0.0, 1+ matches → scales up
                        (count as f64 / 5.0).min(1.0)
                    }
                    Err(_) => 0.0,
                }
            } else {
                let lower = text.to_lowercase();
                let pat = pattern.to_lowercase();
                let count = lower.matches(&pat).count();
                (count as f64 / 5.0).min(1.0)
            }
        }
        HeuristicExtractor::ContentLength { min, max } => {
            let len = text.len();
            match (min, max) {
                (Some(min_v), Some(max_v)) => {
                    let min_v = *min_v;
                    let max_v = *max_v;
                    if len < min_v {
                        0.0
                    } else if len > max_v {
                        // Penalize exceeding max but not to zero
                        0.5
                    } else {
                        // Linear scale between min and max
                        let range = (max_v - min_v).max(1);
                        ((len - min_v) as f64 / range as f64).clamp(0.0, 1.0)
                    }
                }
                (Some(min_v), None) => {
                    let min_v = *min_v;
                    if len >= min_v { 1.0 } else { len as f64 / min_v as f64 }
                }
                (None, Some(max_v)) => {
                    let max_v = *max_v;
                    if len <= max_v { 1.0 } else { max_v as f64 / len as f64 }
                }
                (None, None) => 1.0,
            }
        }
        HeuristicExtractor::HasKeys { keys } => {
            if let Value::Object(obj) = json {
                let found = keys.iter().filter(|k| obj.contains_key(*k)).count();
                if keys.is_empty() {
                    1.0
                } else {
                    found as f64 / keys.len() as f64
                }
            } else {
                0.0
            }
        }
        HeuristicExtractor::KeywordDensity { keywords } => {
            if keywords.is_empty() || text.is_empty() {
                return 1.0;
            }
            let lower = text.to_lowercase();
            let tokens: Vec<&str> = lower.split_whitespace().collect();
            let total = tokens.len().max(1);
            let kw_count = tokens
                .iter()
                .filter(|t| keywords.iter().any(|kw| t.contains(&kw.to_lowercase())))
                .count();
            // Density normalized: 0% → 0.0, 50%+ → 1.0
            (kw_count as f64 / total as f64 * 2.0).min(1.0)
        }
        HeuristicExtractor::Custom { name: _name } => {
            // Custom extractors are registered externally; default to 1.0
            // The engine can replace this with a real implementation via
            // the strategy's custom extractor registry (future feature).
            tracing::debug!("custom extractor not registered, defaulting to 1.0");
            1.0
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rule::EvalRule;
    use crate::strategy::{
        HeuristicDimensionMapping, HeuristicExtractor, HeuristicFactor,
    };
    use crate::target::EvalTarget;

    fn llm_target(content: &str) -> EvalTarget {
        EvalTarget::LlmOutput {
            content: content.into(),
            model: None,
            turn: 1,
            query: None,
        }
    }

    #[tokio::test]
    async fn content_length_in_range() {
        let strategy = HeuristicStrategy;
        let rule = EvalRule {
            id: "test".into(),
            name: "Test".into(),
            description: None,
            strategy: EvalStrategyType::Heuristic {
                factors: vec![HeuristicFactor {
                    name: "length".into(),
                    weight: 1.0,
                    extractor: HeuristicExtractor::ContentLength {
                        min: Some(10),
                        max: Some(100),
                    },
                }],
                dimension_scores: vec![HeuristicDimensionMapping {
                    dimension_name: "quality".into(),
                    weight: 1.0,
                    factor_names: vec!["length".into()],
                }],
            },
            threshold: 0.5,
            enabled: true,
            tags: vec![],
            applies_to: vec![],
        };

        let target = llm_target("A reasonably long response that should be within range.");
        let score = strategy.evaluate(&target, &rule).await.unwrap();
        assert!(score.aggregate_score > 0.0);
    }

    #[tokio::test]
    async fn keyword_density() {
        let strategy = HeuristicStrategy;
        let rule = EvalRule {
            id: "test".into(),
            name: "Test".into(),
            description: None,
            strategy: EvalStrategyType::Heuristic {
                factors: vec![HeuristicFactor {
                    name: "tech_keywords".into(),
                    weight: 1.0,
                    extractor: HeuristicExtractor::KeywordDensity {
                        keywords: vec!["error".into(), "solution".into(), "fix".into()],
                    },
                }],
                dimension_scores: vec![HeuristicDimensionMapping {
                    dimension_name: "relevance".into(),
                    weight: 1.0,
                    factor_names: vec!["tech_keywords".into()],
                }],
            },
            threshold: 0.5,
            enabled: true,
            tags: vec![],
            applies_to: vec![],
        };

        let target = llm_target("The error occurred. The solution is to fix the config.");
        let score = strategy.evaluate(&target, &rule).await.unwrap();
        assert!(score.aggregate_score > 0.0);
    }

    #[tokio::test]
    async fn count_matches() {
        let strategy = HeuristicStrategy;
        let rule = EvalRule {
            id: "test".into(),
            name: "Test".into(),
            description: None,
            strategy: EvalStrategyType::Heuristic {
                factors: vec![HeuristicFactor {
                    name: "code_blocks".into(),
                    weight: 1.0,
                    extractor: HeuristicExtractor::CountMatches {
                        pattern: "```".into(),
                        is_regex: false,
                    },
                }],
                dimension_scores: vec![HeuristicDimensionMapping {
                    dimension_name: "structure".into(),
                    weight: 1.0,
                    factor_names: vec!["code_blocks".into()],
                }],
            },
            threshold: 0.5,
            enabled: true,
            tags: vec![],
            applies_to: vec![],
        };

        let target = llm_target("Here is code:\n```\nfn main() {}\n```\nAnd more:\n```\nlet x = 1;\n```");
        let score = strategy.evaluate(&target, &rule).await.unwrap();
        assert!(score.aggregate_score > 0.5); // 4 occurrences of ```
    }

    #[tokio::test]
    async fn has_keys() {
        let strategy = HeuristicStrategy;
        let rule = EvalRule {
            id: "test".into(),
            name: "Test".into(),
            description: None,
            strategy: EvalStrategyType::Heuristic {
                factors: vec![HeuristicFactor {
                    name: "required_keys".into(),
                    weight: 1.0,
                    extractor: HeuristicExtractor::HasKeys {
                        keys: vec!["status".into(), "data".into()],
                    },
                }],
                dimension_scores: vec![HeuristicDimensionMapping {
                    dimension_name: "completeness".into(),
                    weight: 1.0,
                    factor_names: vec!["required_keys".into()],
                }],
            },
            threshold: 0.7,
            enabled: true,
            tags: vec![],
            applies_to: vec![],
        };

        let target = EvalTarget::ToolResult {
            tool_name: "test".into(),
            input: Value::Null,
            output: serde_json::json!({"status": "ok", "data": [1, 2]}),
        };
        let score = strategy.evaluate(&target, &rule).await.unwrap();
        assert_eq!(score.dimensions[0].score, 1.0); // both keys present
    }
}
