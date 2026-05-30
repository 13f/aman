// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! Assertion-based evaluation strategy — structural checks on JSON content.

use async_trait::async_trait;
use kernel::AmanResult;
use regex::Regex;
use serde_json::Value;

use crate::rule::EvalRule;
use crate::score::{EvalScore, ScoredDimension};
use crate::strategy::{EvalAssertion, EvalStrategy, EvalStrategyType};
use crate::target::EvalTarget;

/// Assertion strategy: verify structural properties of JSON output.
pub struct AssertionStrategy;

#[async_trait]
impl EvalStrategy for AssertionStrategy {
    fn strategy_type(&self) -> &'static str {
        "assertion"
    }

    async fn evaluate(&self, target: &EvalTarget, rule: &EvalRule) -> AmanResult<EvalScore> {
        let EvalStrategyType::Assertion { assertions } = &rule.strategy else {
            return Err(kernel::Error::Unrecoverable {
                message: "expected Assertion strategy type".into(),
            });
        };

        // Get JSON content — for LlmOutput we try to parse the text as JSON,
        // for other variants we use the embedded Value directly.
        let json = extract_json(target);

        let mut dimensions = Vec::with_capacity(assertions.len());

        for assertion in assertions {
            let (name, ok, detail) = check_assertion(assertion, &json);
            dimensions.push(ScoredDimension {
                name,
                score: if ok { 1.0 } else { 0.0 },
                weight: 1.0,
                reason: Some(detail),
            });
        }

        let id = uuid::Uuid::now_v7().to_string();
        Ok(EvalScore::new(
            id,
            &rule.id,
            target.id(),
            "assertion",
            dimensions,
            rule.threshold,
        ))
    }
}

/// Extract a JSON value from the target.
fn extract_json(target: &EvalTarget) -> Value {
    match target {
        EvalTarget::ToolResult { output, .. }
        | EvalTarget::TaskResult { result: output, .. }
        | EvalTarget::PipelineResult { output, .. } => output.clone(),
        EvalTarget::Custom { content, .. } => content.clone(),
        EvalTarget::LlmOutput { content, .. } => {
            // Try to parse LLM output as JSON; fall back to wrapping as string
            serde_json::from_str(content).unwrap_or_else(|_| Value::String(content.clone()))
        }
    }
}

/// Resolve a JSON pointer path (e.g., "/status", "/data/name").
fn resolve_pointer<'a>(value: &'a Value, path: &str) -> Option<&'a Value> {
    if path.is_empty() || path == "/" {
        return Some(value);
    }
    // serde_json pointer requires leading "/" — ensure it
    let pointer = if path.starts_with('/') {
        path.to_owned()
    } else {
        format!("/{path}")
    };
    value.pointer(&pointer)
}

/// Check a single assertion and return (name, passed, detail).
fn check_assertion(assertion: &EvalAssertion, json: &Value) -> (String, bool, String) {
    match assertion {
        EvalAssertion::HasField { path } => {
            let found = resolve_pointer(json, path).is_some();
            (
                format!("has_field:{path}"),
                found,
                if found {
                    format!("field '{path}' exists")
                } else {
                    format!("field '{path}' is missing")
                },
            )
        }
        EvalAssertion::FieldEquals { path, value } => {
            match resolve_pointer(json, path) {
                Some(actual) => {
                    let ok = actual == value;
                    (
                        format!("field_equals:{path}"),
                        ok,
                        if ok {
                            format!("field '{path}' equals {value}")
                        } else {
                            format!("field '{path}' is {actual}, expected {value}")
                        },
                    )
                }
                None => (
                    format!("field_equals:{path}"),
                    false,
                    format!("field '{path}' not found"),
                ),
            }
        }
        EvalAssertion::FieldType { path, json_type } => {
            match resolve_pointer(json, path) {
                Some(actual) => {
                    let actual_type = json_type_of(actual);
                    let ok = actual_type == *json_type;
                    (
                        format!("field_type:{path}"),
                        ok,
                        if ok {
                            format!("field '{path}' is {json_type}")
                        } else {
                            format!("field '{path}' is {actual_type}, expected {json_type}")
                        },
                    )
                }
                None => (
                    format!("field_type:{path}"),
                    false,
                    format!("field '{path}' not found"),
                ),
            }
        }
        EvalAssertion::LengthBetween { path, min, max } => {
            match resolve_pointer(json, path) {
                Some(Value::String(s)) => {
                    let len = s.len();
                    let ok = min.is_none_or(|m| len >= m) && max.is_none_or(|m| len <= m);
                    (
                        format!("length_between:{path}"),
                        ok,
                        format!("field '{path}' length is {len} (min={min:?}, max={max:?})"),
                    )
                }
                Some(Value::Array(a)) => {
                    let len = a.len();
                    let ok = min.is_none_or(|m| len >= m) && max.is_none_or(|m| len <= m);
                    (
                        format!("length_between:{path}"),
                        ok,
                        format!("field '{path}' array length is {len} (min={min:?}, max={max:?})"),
                    )
                }
                Some(v) => (
                    format!("length_between:{path}"),
                    false,
                    format!(
                        "field '{path}' is {} (not a string or array)",
                        json_type_of(v)
                    ),
                ),
                None => (
                    format!("length_between:{path}"),
                    false,
                    format!("field '{path}' not found"),
                ),
            }
        }
        EvalAssertion::RegexMatch { path, pattern } => {
            match resolve_pointer(json, path) {
                Some(Value::String(s)) => match Regex::new(pattern) {
                    Ok(re) => {
                        let ok = re.is_match(s);
                        (
                            format!("regex_match:{path}"),
                            ok,
                            if ok {
                                format!("field '{path}' matches /{pattern}/")
                            } else {
                                format!("field '{path}' does not match /{pattern}/")
                            },
                        )
                    }
                    Err(e) => (
                        format!("regex_match:{path}"),
                        false,
                        format!("invalid regex /{pattern}/: {e}"),
                    ),
                },
                Some(v) => (
                    format!("regex_match:{path}"),
                    false,
                    format!(
                        "field '{path}' is {} (not a string)",
                        json_type_of(v)
                    ),
                ),
                None => (
                    format!("regex_match:{path}"),
                    false,
                    format!("field '{path}' not found"),
                ),
            }
        }
    }
}

fn json_type_of(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rule::EvalRule;
    use crate::strategy::EvalAssertion;
    use crate::target::EvalTarget;

    fn json_target(json: Value) -> EvalTarget {
        EvalTarget::ToolResult {
            tool_name: "test".into(),
            input: Value::Null,
            output: json,
        }
    }

    fn make_rule(assertions: Vec<EvalAssertion>) -> EvalRule {
        EvalRule {
            id: "test".into(),
            name: "Test".into(),
            description: None,
            strategy: EvalStrategyType::Assertion { assertions },
            threshold: 0.8,
            enabled: true,
            tags: vec![],
            applies_to: vec![],
        }
    }

    #[tokio::test]
    async fn has_field_pass() {
        let strategy = AssertionStrategy;
        let rule = make_rule(vec![EvalAssertion::HasField {
            path: "/status".into(),
        }]);
        let target = json_target(serde_json::json!({"status": "ok"}));
        let score = strategy.evaluate(&target, &rule).await.unwrap();
        assert_eq!(score.dimensions[0].score, 1.0);
    }

    #[tokio::test]
    async fn has_field_fail() {
        let strategy = AssertionStrategy;
        let rule = make_rule(vec![EvalAssertion::HasField {
            path: "/missing".into(),
        }]);
        let target = json_target(serde_json::json!({"status": "ok"}));
        let score = strategy.evaluate(&target, &rule).await.unwrap();
        assert_eq!(score.dimensions[0].score, 0.0);
    }

    #[tokio::test]
    async fn field_equals() {
        let strategy = AssertionStrategy;
        let rule = make_rule(vec![EvalAssertion::FieldEquals {
            path: "/status".into(),
            value: serde_json::json!("ok"),
        }]);
        let target = json_target(serde_json::json!({"status": "ok"}));
        let score = strategy.evaluate(&target, &rule).await.unwrap();
        assert_eq!(score.dimensions[0].score, 1.0);
    }

    #[tokio::test]
    async fn field_type_check() {
        let strategy = AssertionStrategy;
        let rule = make_rule(vec![EvalAssertion::FieldType {
            path: "/count".into(),
            json_type: "number".into(),
        }]);
        let target = json_target(serde_json::json!({"count": 42}));
        let score = strategy.evaluate(&target, &rule).await.unwrap();
        assert_eq!(score.dimensions[0].score, 1.0);
    }

    #[tokio::test]
    async fn length_between_string() {
        let strategy = AssertionStrategy;
        let rule = make_rule(vec![EvalAssertion::LengthBetween {
            path: "/name".into(),
            min: Some(3),
            max: Some(10),
        }]);
        let target = json_target(serde_json::json!({"name": "Alice"}));
        let score = strategy.evaluate(&target, &rule).await.unwrap();
        assert_eq!(score.dimensions[0].score, 1.0);
    }

    #[tokio::test]
    async fn regex_match_pass() {
        let strategy = AssertionStrategy;
        let rule = make_rule(vec![EvalAssertion::RegexMatch {
            path: "/email".into(),
            pattern: r"@".into(),
        }]);
        let target = json_target(serde_json::json!({"email": "alice@example.com"}));
        let score = strategy.evaluate(&target, &rule).await.unwrap();
        assert_eq!(score.dimensions[0].score, 1.0);
    }

    #[tokio::test]
    async fn multiple_assertions_partial_pass() {
        let strategy = AssertionStrategy;
        let rule = make_rule(vec![
            EvalAssertion::HasField {
                path: "/status".into(),
            },
            EvalAssertion::HasField {
                path: "/missing".into(),
            },
        ]);
        let target = json_target(serde_json::json!({"status": "ok"}));
        let score = strategy.evaluate(&target, &rule).await.unwrap();
        assert_eq!(score.dimensions.len(), 2);
        assert_eq!(score.aggregate_score, 0.5); // 1/2 passed
    }
}
