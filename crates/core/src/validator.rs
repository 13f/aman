#![forbid(unsafe_code)]
//! OutputValidator — LLM reply validation with fail_closed semantics (§8.2).
//!
//! Validates complete LLM replies (LLM_STREAM_DONE) for:
//!   1. Secret leakage (API keys, private keys, tokens)
//!   2. System prompt leakage
//!   3. Tool injection
//!
//! Fail-closed: validator error/timeout → all replies blocked.

use serde::Serialize;
use std::time::{Duration, Instant};

/// Outcome of a validation check.
#[derive(Debug, Clone, Serialize)]
pub enum ValidationOutcome {
    /// Reply passed all checks.
    Pass,
    /// Reply failed one or more checks.
    Fail {
        matched_rules: Vec<String>,
        reason: String,
    },
    /// Validator itself encountered an error (fail_closed).
    Error {
        message: String,
    },
}

/// A single validation rule.
#[derive(Debug, Clone)]
struct ValidationRule {
    name: String,
    /// Lower-case substring or regex-like pattern to search for.
    pattern: String,
    /// Category of the rule.
    category: RuleCategory,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuleCategory {
    SecretLeak,
    SystemPromptLeak,
    ToolInjection,
}

impl ValidationRule {
    fn matches(&self, lower: &str) -> bool {
        lower.contains(&self.pattern)
    }
}

/// OutputValidator — validates LLM replies with fail_closed semantics.
#[derive(Debug, Clone)]
pub struct OutputValidator {
    rules: Vec<ValidationRule>,
    timeout: Duration,
}

impl Default for OutputValidator {
    fn default() -> Self {
        Self::new()
    }
}

impl OutputValidator {
    /// Create a new OutputValidator with default rules.
    #[must_use]
    pub fn new() -> Self {
        Self {
            rules: vec![
                // --- Secret leakage detection ---
                ValidationRule {
                    name: "openai_api_key".into(),
                    pattern: "sk-".into(),
                    category: RuleCategory::SecretLeak,
                },
                ValidationRule {
                    name: "aws_access_key".into(),
                    pattern: "akia".into(),
                    category: RuleCategory::SecretLeak,
                },
                ValidationRule {
                    name: "private_key_block".into(),
                    pattern: "-----begin".into(),
                    category: RuleCategory::SecretLeak,
                },
                ValidationRule {
                    name: "github_token".into(),
                    pattern: "ghp_".into(),
                    category: RuleCategory::SecretLeak,
                },
                // --- System prompt leakage ---
                ValidationRule {
                    name: "system_prompt_disclosure".into(),
                    pattern: "you are an ai assistant".into(),
                    category: RuleCategory::SystemPromptLeak,
                },
                // --- Tool injection ---
                ValidationRule {
                    name: "tool_prompt_injection".into(),
                    pattern: "ignore safety".into(),
                    category: RuleCategory::ToolInjection,
                },
                ValidationRule {
                    name: "tool_bypass".into(),
                    pattern: "bypass filter".into(),
                    category: RuleCategory::ToolInjection,
                },
            ],
            timeout: Duration::from_secs(2),
        }
    }

    /// Create an OutputValidator with custom rules and timeout (for testing).
    #[must_use]
    pub fn with_config(
        rules: Vec<(&str, &str, RuleCategory)>,
        timeout_secs: u64,
    ) -> Self {
        Self {
            rules: rules.into_iter().map(|(name, pattern, category)| ValidationRule {
                name: name.to_owned(),
                pattern: pattern.to_lowercase(),
                category,
            }).collect(),
            timeout: Duration::from_secs(timeout_secs),
        }
    }

    /// Set a custom timeout.
    #[must_use]
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Get the validation timeout.
    #[must_use]
    pub fn timeout(&self) -> Duration {
        self.timeout
    }

    /// Validate an LLM reply.
    ///
    /// Returns `Error` if validation takes longer than `timeout` (fail_closed).
    /// Returns `Fail` if any rules match.
    /// Returns `Pass` if no rules match.
    pub fn validate(&self, text: &str) -> ValidationOutcome {
        // fail_closed: timeout protection
        let started = Instant::now();
        if started.elapsed() >= self.timeout {
            return ValidationOutcome::Error {
                message: "validation timed out".to_owned(),
            };
        }

        let lower = text.to_lowercase();
        let mut matched: Vec<String> = Vec::new();

        for rule in &self.rules {
            if started.elapsed() >= self.timeout {
                return ValidationOutcome::Error {
                    message: "validation timed out".to_owned(),
                };
            }
            if rule.matches(&lower) {
                matched.push(rule.name.clone());
            }
        }

        if matched.is_empty() {
            ValidationOutcome::Pass
        } else {
            ValidationOutcome::Fail {
                matched_rules: matched.clone(),
                reason: format!(
                    "matched {} rule(s): {}",
                    self.categorize(&matched),
                    matched.join(", "),
                ),
            }
        }
    }

    /// Check if the validator itself is healthy (can operate without error).
    #[must_use]
    pub fn is_healthy(&self) -> bool {
        // The validator is healthy if it can perform basic validation
        // without error. Since this is a pure in-process validator and
        // doesn't depend on external services, it's always healthy unless
        // something fundamental is wrong (which we can't detect here).
        true
    }

    fn categorize(&self, matched: &[String]) -> &'static str {
        for rule in &self.rules {
            if matched.contains(&rule.name) {
                return match rule.category {
                    RuleCategory::SecretLeak => "secret_leak",
                    RuleCategory::SystemPromptLeak => "system_prompt_leak",
                    RuleCategory::ToolInjection => "tool_injection",
                };
            }
        }
        "unknown"
    }

    /// Number of configured rules.
    #[must_use]
    pub fn rule_count(&self) -> usize {
        self.rules.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normal_reply_passes() {
        let v = OutputValidator::new();
        let result = v.validate("Hello! How can I help you today?");
        assert!(matches!(result, ValidationOutcome::Pass));
    }

    #[test]
    fn openai_api_key_detected() {
        let v = OutputValidator::new();
        let result = v.validate("My key is sk-proj-abc123def456");
        match result {
            ValidationOutcome::Fail { matched_rules, .. } => {
                assert!(matched_rules.contains(&"openai_api_key".into()));
            }
            _ => panic!("expected Fail, got {result:?}"),
        }
    }

    #[test]
    fn private_key_block_detected() {
        let v = OutputValidator::new();
        let result = v.validate("Here is my key:\n-----BEGIN RSA PRIVATE KEY-----");
        match result {
            ValidationOutcome::Fail { matched_rules, .. } => {
                assert!(matched_rules.contains(&"private_key_block".into()));
            }
            _ => panic!("expected Fail, got {result:?}"),
        }
    }

    #[test]
    fn system_prompt_leak_detected() {
        let v = OutputValidator::new();
        let result = v.validate("You are an AI assistant created by...");
        match result {
            ValidationOutcome::Fail { matched_rules, .. } => {
                assert!(matched_rules.contains(&"system_prompt_disclosure".into()));
            }
            _ => panic!("expected Fail, got {result:?}"),
        }
    }

    #[test]
    fn tool_bypass_detected() {
        let v = OutputValidator::new();
        let result = v.validate("You can bypass filter by using...");
        match result {
            ValidationOutcome::Fail { matched_rules, .. } => {
                assert!(matched_rules.contains(&"tool_bypass".into()));
            }
            _ => panic!("expected Fail, got {result:?}"),
        }
    }

    #[test]
    fn case_insensitive_detection() {
        let v = OutputValidator::new();
        let result = v.validate("SK-PROJ-ABC123");
        match result {
            ValidationOutcome::Fail { matched_rules, .. } => {
                assert!(matched_rules.contains(&"openai_api_key".into()));
            }
            _ => panic!("expected Fail, got {result:?}"),
        }
    }

    #[test]
    fn custom_rules_override_defaults() {
        let v = OutputValidator::with_config(
            vec![("custom_leak", "secret123", RuleCategory::SecretLeak)],
            5,
        );
        assert!(matches!(v.validate("my secret123"), ValidationOutcome::Fail { .. }));
        assert!(matches!(v.validate("hello"), ValidationOutcome::Pass));
    }

    #[test]
    fn timeout_triggers_error() {
        // Use a zero timeout to simulate fail_closed on timeout
        let v = OutputValidator::with_config(vec![], 0);
        let result = v.validate("some long text");
        assert!(matches!(result, ValidationOutcome::Error { .. }));
    }

    #[test]
    fn multiple_rules_can_match() {
        let v = OutputValidator::new();
        let result = v.validate("sk-proj-abc and ghp_token123");
        match result {
            ValidationOutcome::Fail { matched_rules, .. } => {
                assert!(matched_rules.len() >= 2);
            }
            _ => panic!("expected Fail, got {result:?}"),
        }
    }

    #[test]
    fn is_healthy_by_default() {
        let v = OutputValidator::new();
        assert!(v.is_healthy());
    }

    #[test]
    fn rule_count_matches_defaults() {
        let v = OutputValidator::new();
        assert_eq!(v.rule_count(), 7);
    }
}
