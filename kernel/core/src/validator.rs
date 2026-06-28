#![forbid(unsafe_code)]

// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! OutputValidator — LLM reply validation with fail_closed semantics (§8.2).
//!
//! Validates complete LLM replies (LLM_STREAM_DONE) for:
//!   1. Secret leakage (API keys, private keys, tokens)
//!   2. System prompt leakage
//!   3. Tool injection
//!
//! Fail-closed: validator error/timeout → all replies blocked.
//!
//! Trust-level aware: `TrustLevel::Trusted` inputs skip validation entirely.

use crate::types::TrustLevel;
use regex::Regex;
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

/// Category of a validation rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuleCategory {
    SecretLeak,
    SystemPromptLeak,
    ToolInjection,
}

/// How a rule matches: simple substring or compiled regex.
#[derive(Debug, Clone)]
enum MatchMode {
    /// Simple case-insensitive substring match.
    Substring(String),
    /// Compiled regex (case-insensitive). Stored as a pattern string for
    /// Debug/Clone and compiled on demand via LazyLock-like one-shot.
    Regex {
        pattern: String,
        compiled: Regex,
    },
}

impl MatchMode {
    fn matches(&self, text: &str) -> bool {
        match self {
            Self::Substring(pattern) => text.contains(pattern.as_str()),
            Self::Regex { compiled, .. } => compiled.is_match(text),
        }
    }
}

impl std::fmt::Display for MatchMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Substring(p) => write!(f, "substring:{p}"),
            Self::Regex { pattern, .. } => write!(f, "regex:{pattern}"),
        }
    }
}

/// A single validation rule.
#[derive(Debug, Clone)]
struct ValidationRule {
    name: String,
    mode: MatchMode,
    category: RuleCategory,
}

impl ValidationRule {
    fn matches(&self, lower: &str) -> bool {
        self.mode.matches(lower)
    }

    fn substring(name: &str, pattern: &str, category: RuleCategory) -> Self {
        Self {
            name: name.to_owned(),
            mode: MatchMode::Substring(pattern.to_lowercase()),
            category,
        }
    }

    fn regex(name: &str, pattern: &str, category: RuleCategory) -> Self {
        let compiled = Regex::new(&format!("(?i){pattern}")).expect("regex must compile");
        Self {
            name: name.to_owned(),
            mode: MatchMode::Regex {
                pattern: pattern.to_owned(),
                compiled,
            },
            category,
        }
    }
}

/// Audit record for a single output validation run.
#[derive(Debug, Clone)]
pub struct OutputAuditRecord {
    /// Timestamp of validation (milliseconds since epoch).
    pub validated_at_ms: u128,
    /// Trust level of the validated content.
    pub trust_level: TrustLevel,
    /// Length of the validated output in bytes.
    pub output_len: usize,
    /// Number of violations detected.
    pub violations_count: usize,
}

/// OutputValidator — validates LLM replies with fail_closed semantics.
///
/// Trusted content bypasses validation entirely. Untrusted and sandboxed
/// content is checked against all configured rules.
#[derive(Debug, Clone)]
pub struct OutputValidator {
    rules: Vec<ValidationRule>,
    timeout: Duration,
    /// Accumulated audit records. Call `drain_audit_log()` to consume.
    audit_log: Vec<OutputAuditRecord>,
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
                // --- Secret leakage detection (substring) ---
                ValidationRule::substring(
                    "openai_api_key", "sk-", RuleCategory::SecretLeak,
                ),
                ValidationRule::substring(
                    "aws_access_key", "akia", RuleCategory::SecretLeak,
                ),
                ValidationRule::substring(
                    "private_key_block", "-----begin", RuleCategory::SecretLeak,
                ),
                ValidationRule::substring(
                    "github_token", "ghp_", RuleCategory::SecretLeak,
                ),
                // --- Secret leakage detection (regex — from secret crate) ---
                ValidationRule::regex(
                    "private_key_pem",
                    r"-----BEGIN\s+PRIVATE\s+KEY-----",
                    RuleCategory::SecretLeak,
                ),
                // --- System prompt leakage ---
                ValidationRule::substring(
                    "system_prompt_disclosure",
                    "you are an ai assistant",
                    RuleCategory::SystemPromptLeak,
                ),
                ValidationRule::regex(
                    "system_prompt_mention",
                    r"(?:my|our|the|your|agent(?:\x27s)?)\s+system\s+prompt\s+(?:is|says|contains|tells|reads)",
                    RuleCategory::SystemPromptLeak,
                ),
                // --- Tool injection (substring) ---
                ValidationRule::substring(
                    "tool_prompt_injection",
                    "ignore safety",
                    RuleCategory::ToolInjection,
                ),
                ValidationRule::substring(
                    "tool_bypass", "bypass filter", RuleCategory::ToolInjection,
                ),
                // --- Tool injection (regex — from secret crate) ---
                ValidationRule::regex(
                    "shell_command_exec",
                    r"execute\s+shell\s+command",
                    RuleCategory::ToolInjection,
                ),
                // --- JWT token detection (from ContentFilter) ---
                ValidationRule::regex(
                    "jwt_token",
                    r"eyJ[a-zA-Z0-9_-]+\.[a-zA-Z0-9_-]+\.[a-zA-Z0-9_-]+",
                    RuleCategory::SecretLeak,
                ),
            ],
            timeout: Duration::from_secs(2),
            audit_log: Vec::new(),
        }
    }

    /// Create an OutputValidator with custom substring rules and timeout.
    #[must_use]
    pub fn with_config(
        rules: Vec<(&str, &str, RuleCategory)>,
        timeout_secs: u64,
    ) -> Self {
        Self {
            rules: rules
                .into_iter()
                .map(|(name, pattern, category)| {
                    ValidationRule::substring(name, pattern, category)
                })
                .collect(),
            timeout: Duration::from_secs(timeout_secs),
            audit_log: Vec::new(),
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
    /// Trusted content bypasses validation entirely (returns `Pass`).
    /// Untrusted and sandboxed content is checked against all rules.
    ///
    /// Returns `Error` if validation takes longer than `timeout` (fail_closed).
    /// Returns `Fail` if any rules match.
    /// Returns `Pass` if no rules match.
    pub fn validate(&mut self, text: &str, trust_level: TrustLevel) -> ValidationOutcome {
        // Trusted inputs bypass validation
        if trust_level == TrustLevel::Trusted {
            return ValidationOutcome::Pass;
        }

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
                // Record audit before returning error
                self.audit_log.push(OutputAuditRecord {
                    validated_at_ms: now_ms(),
                    trust_level,
                    output_len: text.len(),
                    violations_count: matched.len(),
                });
                return ValidationOutcome::Error {
                    message: "validation timed out".to_owned(),
                };
            }
            if rule.matches(&lower) {
                matched.push(rule.name.clone());
            }
        }

        // Record audit
        self.audit_log.push(OutputAuditRecord {
            validated_at_ms: now_ms(),
            trust_level,
            output_len: text.len(),
            violations_count: matched.len(),
        });

        if matched.is_empty() {
            ValidationOutcome::Pass
        } else {
            ValidationOutcome::Fail {
                reason: format!(
                    "matched {} rule(s): {}",
                    self.categorize(&matched),
                    matched.join(", "),
                ),
                matched_rules: matched,
            }
        }
    }

    /// Drain accumulated audit records.
    #[must_use]
    pub fn drain_audit_log(&mut self) -> Vec<OutputAuditRecord> {
        std::mem::take(&mut self.audit_log)
    }

    /// Check if the validator itself is healthy (can operate without error).
    #[must_use]
    pub fn is_healthy(&self) -> bool {
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

fn now_ms() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis())
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Existing core tests (unchanged) ──────────────────────────

    #[test]
    fn normal_reply_passes() {
        let mut v = OutputValidator::new();
        let result = v.validate("Hello! How can I help you today?", TrustLevel::Untrusted);
        assert!(matches!(result, ValidationOutcome::Pass));
    }

    #[test]
    fn trusted_input_bypasses_validation() {
        let mut v = OutputValidator::new();
        let result = v.validate("sk-proj-leaked-key", TrustLevel::Trusted);
        assert!(matches!(result, ValidationOutcome::Pass));
    }

    #[test]
    fn trusted_input_does_not_create_audit_record() {
        let mut v = OutputValidator::new();
        let _ = v.validate("sk-proj-leaked-key", TrustLevel::Trusted);
        assert!(v.drain_audit_log().is_empty());
    }

    #[test]
    fn openai_api_key_detected() {
        let mut v = OutputValidator::new();
        let result = v.validate("My key is sk-proj-abc123def456", TrustLevel::Untrusted);
        match result {
            ValidationOutcome::Fail { matched_rules, .. } => {
                assert!(matched_rules.contains(&"openai_api_key".into()));
            }
            _ => panic!("expected Fail, got {result:?}"),
        }
    }

    #[test]
    fn private_key_block_detected() {
        let mut v = OutputValidator::new();
        let result = v
            .validate("Here is my key:\n-----BEGIN RSA PRIVATE KEY-----", TrustLevel::Untrusted);
        match result {
            ValidationOutcome::Fail { matched_rules, .. } => {
                assert!(matched_rules.contains(&"private_key_block".into()));
            }
            _ => panic!("expected Fail, got {result:?}"),
        }
    }

    #[test]
    fn system_prompt_leak_detected() {
        let mut v = OutputValidator::new();
        let result = v.validate(
            "You are an AI assistant created by...",
            TrustLevel::Untrusted,
        );
        match result {
            ValidationOutcome::Fail { matched_rules, .. } => {
                assert!(matched_rules.contains(&"system_prompt_disclosure".into()));
            }
            _ => panic!("expected Fail, got {result:?}"),
        }
    }

    #[test]
    fn tool_bypass_detected() {
        let mut v = OutputValidator::new();
        let result = v.validate("You can bypass filter by using...", TrustLevel::Untrusted);
        match result {
            ValidationOutcome::Fail { matched_rules, .. } => {
                assert!(matched_rules.contains(&"tool_bypass".into()));
            }
            _ => panic!("expected Fail, got {result:?}"),
        }
    }

    #[test]
    fn case_insensitive_detection() {
        let mut v = OutputValidator::new();
        let result = v.validate("SK-PROJ-ABC123", TrustLevel::Untrusted);
        match result {
            ValidationOutcome::Fail { matched_rules, .. } => {
                assert!(matched_rules.contains(&"openai_api_key".into()));
            }
            _ => panic!("expected Fail, got {result:?}"),
        }
    }

    #[test]
    fn custom_rules_override_defaults() {
        let mut v = OutputValidator::with_config(
            vec![("custom_leak", "secret123", RuleCategory::SecretLeak)],
            5,
        );
        assert!(matches!(
            v.validate("my secret123", TrustLevel::Untrusted),
            ValidationOutcome::Fail { .. }
        ));
        assert!(matches!(
            v.validate("hello", TrustLevel::Untrusted),
            ValidationOutcome::Pass
        ));
    }

    #[test]
    fn timeout_triggers_error() {
        // Use a zero timeout to simulate fail_closed on timeout
        let mut v = OutputValidator::with_config(vec![], 0);
        let result = v.validate("some long text", TrustLevel::Untrusted);
        assert!(matches!(result, ValidationOutcome::Error { .. }));
    }

    #[test]
    fn multiple_rules_can_match() {
        let mut v = OutputValidator::new();
        let result = v.validate("sk-proj-abc and ghp_token123", TrustLevel::Untrusted);
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
    fn rule_count_matches() {
        let v = OutputValidator::new();
        assert_eq!(v.rule_count(), 11);
    }

    // ── Migrated tests from secret::OutputValidator ──────────────

    #[test]
    fn regex_private_key_pem_rejected() {
        let mut validator = OutputValidator::new();
        let result = validator.validate(
            "-----BEGIN PRIVATE KEY-----\nabc",
            TrustLevel::Untrusted,
        );
        assert!(
            matches!(result, ValidationOutcome::Fail { .. }),
            "PEM private key marker should be rejected, got {result:?}"
        );
        assert_eq!(validator.drain_audit_log().len(), 1);
    }

    #[test]
    fn regex_system_prompt_mention_detected() {
        let mut validator = OutputValidator::new();
        let result = validator.validate("my system prompt is to be helpful", TrustLevel::Untrusted);
        assert!(
            matches!(result, ValidationOutcome::Fail { .. }),
            "system prompt mention should be detected, got {result:?}"
        );
    }

    #[test]
    fn system_prompt_as_topic_not_flagged() {
        let mut validator = OutputValidator::new();
        let result = validator.validate("you need a system prompt to define agent behavior", TrustLevel::Untrusted);
        assert!(
            matches!(result, ValidationOutcome::Pass),
            "discussing system prompt as a concept should pass, got {result:?}"
        );
    }

    #[test]
    fn regex_shell_command_exec_detected() {
        let mut validator = OutputValidator::new();
        let result = validator.validate(
            "please execute shell command for me",
            TrustLevel::Untrusted,
        );
        assert!(
            matches!(result, ValidationOutcome::Fail { .. }),
            "shell command injection should be detected, got {result:?}"
        );
    }

    #[test]
    fn jwt_token_detected() {
        let mut validator = OutputValidator::new();
        let result = validator.validate(
            "Bearer eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dozjgNryP4J3jVmNHl0w5N_XgL0n3I9PlFUP0THsR8U",
            TrustLevel::Untrusted,
        );
        assert!(
            matches!(result, ValidationOutcome::Fail { .. }),
            "JWT token should be detected, got {result:?}"
        );
    }

    #[test]
    fn audit_log_accumulates_records() {
        let mut validator = OutputValidator::new();
        let _ = validator.validate("hello", TrustLevel::Untrusted);
        let _ = validator.validate("sk-leaked-key", TrustLevel::Untrusted);
        let records = validator.drain_audit_log();
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].violations_count, 0);
        assert!(records[1].violations_count > 0);
    }

    #[test]
    fn sandboxed_level_validates_same_as_untrusted() {
        let mut validator = OutputValidator::new();
        let result = validator.validate("sk-proj-leaked", TrustLevel::Sandboxed);
        assert!(matches!(result, ValidationOutcome::Fail { .. }));
    }
}
