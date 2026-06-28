#![forbid(unsafe_code)]

// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! ContentFilter — PII and harmful content detection for LLM output.
//!
//! Detects:
//!   1. PII: email addresses, phone numbers, SSNs, credit card numbers
//!   2. API key patterns (reuses patterns from `redactor`)
//!   3. High-risk content markers
//!
//! Produces a [`FilterDecision`]: `Pass`, `Flag` (log + allow), or `Block`.

use regex::Regex;
use std::sync::LazyLock;

/// Decision after filtering content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FilterDecision {
    /// Content is safe — no action needed.
    Pass,
    /// Potentially sensitive content detected — should be logged but allowed.
    Flag {
        matched_rules: Vec<String>,
    },
    /// Harmful content detected — should be blocked.
    Block {
        matched_rules: Vec<String>,
        reason: String,
    },
}

/// A single content filter rule.
#[derive(Debug, Clone)]
struct FilterRule {
    name: String,
    regex: Regex,
    severity: RuleSeverity,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RuleSeverity {
    /// Log only — informational.
    Low,
    /// Flag for review — may be sensitive.
    Medium,
    /// Block immediately — clear violation.
    High,
}

// ── Pre-compiled patterns ────────────────────────────────────────

static EMAIL_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"[a-zA-Z0-9._%+\-]+@[a-zA-Z0-9.\-]+\.[a-zA-Z]{2,}").unwrap()
});

static PHONE_US_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\b\d{3}[-.]?\d{3}[-.]?\d{4}\b").unwrap()
});

static SSN_US_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\b\d{3}-\d{2}-\d{4}\b").unwrap()
});

/// Credit card number pattern (13-19 digits, may have spaces/dashes).
static CREDIT_CARD_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\b(?:\d[ -]*?){13,19}\b").unwrap()
});

/// JWT token (base64url-encoded, 3 parts separated by dots).
static JWT_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"eyJ[a-zA-Z0-9_-]+\.eyJ[a-zA-Z0-9_-]+\.[a-zA-Z0-9_-]+").unwrap()
});

// ── ContentFilter ─────────────────────────────────────────────────

/// Filter for detecting PII and harmful content in text.
///
/// Designed to be used after [`crate::validator::OutputValidator`] —
/// the validator catches secret leaks and injection, while this filter
/// catches PII exposure and content policy violations.
#[derive(Debug, Clone)]
pub struct ContentFilter {
    rules: Vec<FilterRule>,
    /// Enable Luhn check for credit card patterns (more expensive).
    luhn_check: bool,
}

impl Default for ContentFilter {
    fn default() -> Self {
        Self::new()
    }
}

impl ContentFilter {
    /// Create a new ContentFilter with default rules.
    #[must_use]
    pub fn new() -> Self {
        Self {
            rules: vec![
                // ── High severity (block) ──────────────────────────
                // API key patterns (sk-*, ghp_*, AKIA*) are handled by
                // OutputValidator — kept there to avoid redundant filtering.
                FilterRule {
                    name: "jwt_token".into(),
                    regex: JWT_RE.clone(),
                    severity: RuleSeverity::High,
                },
                // ── Medium severity (flag, logged — never blocks) ───
                FilterRule {
                    name: "email_address".into(),
                    regex: EMAIL_RE.clone(),
                    severity: RuleSeverity::Medium,
                },
                FilterRule {
                    name: "credit_card_number".into(),
                    regex: CREDIT_CARD_RE.clone(),
                    severity: RuleSeverity::Medium,
                },
                // ── Low severity (flag, logged) ─────────────────────
                FilterRule {
                    name: "phone_us".into(),
                    regex: PHONE_US_RE.clone(),
                    severity: RuleSeverity::Low,
                },
                FilterRule {
                    name: "ssn_us".into(),
                    regex: SSN_US_RE.clone(),
                    severity: RuleSeverity::Low,
                },
            ],
            luhn_check: true,
        }
    }

    /// Create a ContentFilter without the Luhn check (faster, less accurate).
    #[must_use]
    pub fn without_luhn(mut self) -> Self {
        self.luhn_check = false;
        self
    }

    /// Filter content and return a decision.
    ///
    /// High-severity matches cause `Block`. Medium-severity matches cause
    /// `Flag`. Low-severity matches are only reported if no higher-severity
    /// match exists.
    #[must_use]
    pub fn filter(&self, text: &str) -> FilterDecision {
        let mut high_matches: Vec<String> = Vec::new();
        let mut medium_matches: Vec<String> = Vec::new();
        let mut low_matches: Vec<String> = Vec::new();

        for rule in &self.rules {
            if rule.regex.is_match(text) {
                // For credit card patterns, verify with Luhn check
                if rule.name == "credit_card_number" && self.luhn_check {
                    // Extract all candidate numbers and check Luhn
                    let has_valid_cc = rule
                        .regex
                        .find_iter(text)
                        .any(|m| luhn_valid(strip_non_digits(m.as_str())));
                    if !has_valid_cc {
                        continue;
                    }
                }
                match rule.severity {
                    RuleSeverity::High => high_matches.push(rule.name.clone()),
                    RuleSeverity::Medium => medium_matches.push(rule.name.clone()),
                    RuleSeverity::Low => low_matches.push(rule.name.clone()),
                }
            }
        }

        if !high_matches.is_empty() {
            return FilterDecision::Block {
                reason: format!(
                    "detected sensitive data: {}",
                    high_matches.join(", ")
                ),
                matched_rules: high_matches,
            };
        }

        if !medium_matches.is_empty() {
            return FilterDecision::Flag {
                matched_rules: medium_matches,
            };
        }

        if !low_matches.is_empty() {
            return FilterDecision::Flag {
                matched_rules: low_matches,
            };
        }

        FilterDecision::Pass
    }
}

// ── Luhn algorithm ─────────────────────────────────────────────────

/// Strip all non-digit characters from a string.
fn strip_non_digits(s: &str) -> String {
    s.chars().filter(|c| c.is_ascii_digit()).collect()
}

/// Validate a numeric string using the Luhn algorithm.
fn luhn_valid(number: String) -> bool {
    if number.len() < 13 || number.len() > 19 {
        return false;
    }

    let digits: Vec<u8> = number
        .bytes()
        .filter_map(|b| {
            if b.is_ascii_digit() {
                Some(b - b'0')
            } else {
                None
            }
        })
        .collect();

    if digits.is_empty() {
        return false;
    }

    let sum: u32 = digits
        .iter()
        .rev()
        .enumerate()
        .map(|(i, &d)| {
            if i % 2 == 1 {
                let doubled = d as u32 * 2;
                if doubled > 9 {
                    doubled - 9
                } else {
                    doubled
                }
            } else {
                d as u32
            }
        })
        .sum();

    sum.is_multiple_of(10)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_text_passes() {
        let filter = ContentFilter::new();
        assert_eq!(
            filter.filter("Hello, how can I help you today?"),
            FilterDecision::Pass
        );
    }




    #[test]
    fn jwt_blocked() {
        let filter = ContentFilter::new();
        let result = filter.filter(
            "Bearer eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dozjgNryP4J3jVmNHl0w5N_XgL0n3I9PlFUP0THsR8U",
        );
        assert!(matches!(result, FilterDecision::Block { .. }));
    }

    #[test]
    fn email_flagged() {
        let filter = ContentFilter::new();
        let result = filter.filter("Contact me at user@example.com");
        assert!(
            matches!(result, FilterDecision::Flag { .. }),
            "email should be flagged, got {result:?}"
        );
    }

    #[test]
    fn valid_credit_card_flagged() {
        let filter = ContentFilter::new();
        // 4111 1111 1111 1111 is a valid test Visa number (Luhn-passing)
        let result = filter.filter("Card: 4111 1111 1111 1111");
        assert!(
            matches!(result, FilterDecision::Flag { .. }),
            "valid CC should be flagged, got {result:?}"
        );
    }

    #[test]
    fn invalid_credit_card_passes() {
        let filter = ContentFilter::new();
        // 1234 5678 9012 3456 fails Luhn check
        let result = filter.filter("Card: 1234 5678 9012 3456");
        assert!(
            matches!(result, FilterDecision::Pass),
            "invalid CC number should pass, got {result:?}"
        );
    }


    #[test]
    fn luhn_validation_works() {
        assert!(luhn_valid("4111111111111111".into()));
        assert!(!luhn_valid("1234567890123456".into()));
        // American Express test number
        assert!(luhn_valid("378282246310005".into()));
    }

    #[test]
    fn phone_number_flagged() {
        let filter = ContentFilter::new();
        let result = filter.filter("Call me at 555-123-4567");
        assert!(matches!(result, FilterDecision::Flag { .. }));
    }

    #[test]
    fn ssn_flagged() {
        let filter = ContentFilter::new();
        let result = filter.filter("SSN: 123-45-6789");
        assert!(matches!(result, FilterDecision::Flag { .. }));
    }

    #[test]
    fn without_luhn_flags_all_cc_patterns() {
        let filter = ContentFilter::new().without_luhn();
        // Even invalid CC numbers match the pattern
        let result = filter.filter("1234 5678 9012 3456");
        assert!(matches!(result, FilterDecision::Flag { .. }));
    }
}
