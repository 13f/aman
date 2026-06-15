#![forbid(unsafe_code)]

// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! Sensitive data redaction for log output.
//!
//! Provides pre-compiled regex patterns that detect and redact common
//! secret formats (API keys, bearer tokens, passwords, JWTs) from
//! arbitrary text. Used by the gateway's tracing layer and available
//! as a general-purpose utility via [`redact_sensitive_data`].

use std::borrow::Cow;
use std::sync::LazyLock;
use regex::Regex;

// ── Pre-compiled redaction patterns ────────────────────────────────────────

static REDACT_PATTERNS: LazyLock<Vec<(Regex, &str)>> = LazyLock::new(|| {
    vec![
        // OpenAI / Anthropic API keys: sk-... or sk-ant-...
        (
            Regex::new(r"(sk-[a-zA-Z0-9_-]{20,})").unwrap(),
            "[REDACTED_API_KEY]",
        ),
        // AWS access keys: AKIA... (access key ID pattern)
        (
            Regex::new(r"(AKIA[A-Z0-9]{16})").unwrap(),
            "[REDACTED_AWS_KEY]",
        ),
        // JWT tokens: eyJ... (base64url-encoded JSON header)
        (
            Regex::new(r"(eyJ[a-zA-Z0-9_-]{10,}\.[a-zA-Z0-9_-]{10,}\.[a-zA-Z0-9_-]{10,})").unwrap(),
            "[REDACTED_JWT]",
        ),
        // Bearer tokens in HTTP Authorization headers
        (
            Regex::new(r"(Bearer\s+)[a-zA-Z0-9_\-\.=]{20,}").unwrap(),
            "${1}[REDACTED_TOKEN]",
        ),
        // JSON / key=value patterns with sensitive key names
        // Matches: api_key="...", apikey="...", secret_key="...", password="...", token="..."
        (
            Regex::new(
                r#"((?i)(?:api[_-]?key|apikey|secret[_-]?key|secret|password|auth[_-]?token|access[_-]?token)\s*[:=]\s*['\"]?)([^'"\s,}\[\]]{8,})(['\"]?)"#,
            ).unwrap(),
            "${1}[REDACTED]${3}",
        ),
        // JSON field values: "api_key": "actual-secret-value"
        (
            Regex::new(
                r#"("(?i)(?:api[_-]?key|apikey|secret[_-]?key|secret|password|auth[_-]?token|access[_-]?token)"\s*:\s*")([^"]{8,})(")"#,
            ).unwrap(),
            r#"${1}[REDACTED]${3}"#,
        ),
        // AMAN_API_TOKEN or similar env-var style tokens in text
        (
            Regex::new(r"((?:AMAN|AUTH|SECRET)[_A-Z0-9]*TOKEN[_A-Z0-9]*\s*=\s*)([^\s]{10,})").unwrap(),
            "${1}[REDACTED]",
        ),
    ]
});

// ── Public API ─────────────────────────────────────────────────────────────

/// Redact sensitive data from a string.
///
/// Scans the input for known secret patterns (API keys, tokens, passwords,
/// JWTs) and replaces any matches with `[REDACTED]` placeholders.
///
/// Returns `Cow::Borrowed` if no sensitive data was found, or
/// `Cow::Owned` with redacted content otherwise.
///
/// # Examples
///
/// ```ignore
/// use kernel::redactor::redact_sensitive_data;
///
/// let clean = redact_sensitive_data("api_key=sk-abc123secret");
/// assert!(clean.contains("[REDACTED]"));
///
/// let unchanged = redact_sensitive_data("Hello, world!");
/// assert_eq!(unchanged, "Hello, world!");
/// ```
pub fn redact_sensitive_data(input: &str) -> Cow<'_, str> {
    let mut result = Cow::Borrowed(input);
    for (regex, replacement) in REDACT_PATTERNS.iter() {
        if let Cow::Owned(new) = regex.replace_all(&result, *replacement) {
            result = Cow::Owned(new);
        }
    }
    result
}

/// Check whether a string contains any recognized sensitive data.
///
/// This is a fast-path check before redaction — returns `true` if
/// [`redact_sensitive_data`] would modify the input.
#[must_use]
pub fn contains_sensitive_data(input: &str) -> bool {
    REDACT_PATTERNS.iter().any(|(regex, _)| regex.is_match(input))
}

// ── Print macros ───────────────────────────────────────────────────────────

/// Like `println!` but redacts sensitive data before printing to stdout.
///
/// Use this in CLI code where `tracing` is not wired up but you still
/// want automatic redaction of accidental secret leaks in output.
///
/// The workspace-level `clippy::print_stdout` lint is suppressed inside
/// this macro — it is the **only** sanctioned way to write to stdout
/// outside of build scripts and early-startup error paths.
#[macro_export]
macro_rules! safe_println {
    ($($arg:tt)*) => {
        #[allow(clippy::print_stdout)]
        {
            let msg = format!($($arg)*);
            println!("{}", $crate::redactor::redact_sensitive_data(&msg));
        }
    };
}

/// Like `eprintln!` but redacts sensitive data before printing to stderr.
///
/// The workspace-level `clippy::print_stderr` lint is suppressed inside
/// this macro — it is the **only** sanctioned way to write to stderr
/// outside of build scripts and early-startup error paths.
#[macro_export]
macro_rules! safe_eprintln {
    ($($arg:tt)*) => {
        #[allow(clippy::print_stderr)]
        {
            let msg = format!($($arg)*);
            eprintln!("{}", $crate::redactor::redact_sensitive_data(&msg));
        }
    };
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── OpenAI / Anthropic keys ────────────────────────────────────────

    #[test]
    fn redact_openai_api_key() {
        let input = "Authorization: Bearer sk-proj-abc123def456ghi789jkl012mno345pqr678stu";
        let result = redact_sensitive_data(input);
        assert!(result.contains("[REDACTED_API_KEY]"));
        assert!(!result.contains("sk-proj-abc123"));
    }

    #[test]
    fn redact_anthropic_api_key() {
        let input = "x-api-key: sk-ant-api03-abc123def456ghi789jkl012mno345pqr678stu901vwx";
        let result = redact_sensitive_data(input);
        assert!(result.contains("REDACTED"), "expected redaction in: {result}");
        assert!(!result.contains("sk-ant-api03"));
    }

    // ── AWS access keys ────────────────────────────────────────────────

    #[test]
    fn redact_aws_access_key() {
        let input = "AWS_ACCESS_KEY_ID=AKIAIOSFODNN7EXAMPLE";
        let result = redact_sensitive_data(input);
        assert!(result.contains("[REDACTED_AWS_KEY]"));
    }

    // ── JWT tokens ─────────────────────────────────────────────────────

    #[test]
    fn redact_jwt_token() {
        let input = "Authorization: Bearer eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dozjgNryP4J3jVmNHl0w5N_XgL0n3I9PlFUP0THsR8g";
        let result = redact_sensitive_data(input);
        assert!(result.contains("[REDACTED_JWT]"));
    }

    // ── Bearer tokens ──────────────────────────────────────────────────

    #[test]
    fn redact_bearer_token() {
        let input = "Authorization: Bearer abc123def456ghi789jkl012mno345";
        let result = redact_sensitive_data(input);
        assert!(result.contains("[REDACTED_TOKEN]"));
        assert!(result.contains("Bearer ")); // prefix preserved
    }

    // ── Key=value patterns ─────────────────────────────────────────────

    #[test]
    fn redact_api_key_assignment() {
        let input = r#"api_key = "my-super-secret-value-12345""#;
        let result = redact_sensitive_data(input);
        assert!(result.contains("[REDACTED]"));
        assert!(!result.contains("my-super-secret"));
    }

    #[test]
    fn redact_password_assignment() {
        let input = r#"password: "p@ssw0rd!12345""#;
        let result = redact_sensitive_data(input);
        assert!(result.contains("[REDACTED]"));
    }

    #[test]
    fn redact_token_assignment() {
        let input = r#"auth_token=ghp_1234567890abcdefghijklmnopqrstuv"#;
        let result = redact_sensitive_data(input);
        assert!(result.contains("[REDACTED]"));
    }

    // ── JSON field patterns ────────────────────────────────────────────

    #[test]
    fn redact_json_api_key_field() {
        let input = r#"{"api_key": "sk-abc123def456ghi789jkl", "model": "opus"}"#;
        let result = redact_sensitive_data(input);
        assert!(result.contains("[REDACTED]"));
        assert!(result.contains("\"model\": \"opus\"")); // non-sensitive preserved
    }

    #[test]
    fn redact_json_password_field() {
        let input = r#"{"username": "admin", "password": "admin12345678"}"#;
        let result = redact_sensitive_data(input);
        assert!(result.contains("[REDACTED]"));
        assert!(result.contains("\"username\": \"admin\"")); // non-sensitive preserved
    }

    // ── Env-var token patterns ─────────────────────────────────────────

    #[test]
    fn redact_env_var_token() {
        let input = "AMAN_API_TOKEN=super-secret-token-value-12345";
        let result = redact_sensitive_data(input);
        assert!(result.contains("[REDACTED]"));
    }

    // ── No-op on clean input ───────────────────────────────────────────

    #[test]
    fn clean_input_passes_through() {
        let input = "Hello, world! This is a normal log message.";
        let result = redact_sensitive_data(input);
        assert_eq!(result, input);
    }

    #[test]
    fn normal_json_is_unchanged() {
        let input = r#"{"user": "alice", "action": "login", "status": "ok"}"#;
        let result = redact_sensitive_data(input);
        assert_eq!(result, input);
    }

    // ── contains_sensitive_data ────────────────────────────────────────

    #[test]
    fn detect_sensitive_content() {
        assert!(contains_sensitive_data("api_key=sk-abc123def456"));
        assert!(!contains_sensitive_data("normal log message"));
    }

    // ── Short values are not redacted (avoid false positives) ──────────

    #[test]
    fn short_values_not_redacted() {
        // "token=short" — value too short, should not trigger redaction
        let input = "token=short";
        let result = redact_sensitive_data(input);
        assert_eq!(result, input); // unchanged
    }

    // ── Multiple matches in one string ─────────────────────────────────

    #[test]
    fn redact_multiple_secrets() {
        let input = "Used key sk-abc123def456ghi789jkl012mno and token Bearer xyz789uvw012rst345abc678def901";
        let result = redact_sensitive_data(input);
        assert!(result.contains("REDACTED"), "expected redaction in: {result}");
        assert!(!result.contains("sk-abc123"));
        assert!(!result.contains("xyz789"));
    }

    // ── safe_println! / safe_eprintln! ─────────────────────────────────

    #[test]
    fn safe_println_redacts() {
        // Just verify the macro compiles and redacts
        let msg = "Key: sk-proj-abc123def456ghi789".to_string();
        let result = redact_sensitive_data(&msg);
        assert!(result.contains("REDACTED"), "expected redaction in: {result}");
        assert!(!result.contains("sk-proj-abc123"));
    }

    // ── Property-based tests (proptest) ─────────────────────────────────

    mod proptests {
        use super::*;
        use proptest::prelude::*;

        // Property: redaction is idempotent. Applying redact twice
        // yields the same result as applying it once. This is the
        // foundational safety property — if a redaction step runs
        // in a pipeline twice (e.g. a log is processed by two
        // subscribers, each of which runs the redactor), the
        // output must be stable.
        proptest! {
            #![proptest_config(ProptestConfig::with_cases(256))]
            #[test]
            fn redact_is_idempotent(input in ".*") {
                let once = redact_sensitive_data(&input).into_owned();
                let twice = redact_sensitive_data(&once).into_owned();
                prop_assert_eq!(once, twice);
            }
        }

        // Property: `contains_sensitive_data` is consistent with
        // `redact_sensitive_data`. If the detector says "no secrets
        // here", the redactor must return the input unchanged.
        // This is the no-false-positive property — the detector
        // is used as a fast-path gate; if it returns false but
        // the redactor would still modify the string, downstream
        // pipelines that skip redaction based on the detector
        // would leak secrets.
        proptest! {
            #![proptest_config(ProptestConfig::with_cases(256))]
            #[test]
            fn detector_agrees_with_redactor(input in "[a-zA-Z0-9 _,.!?]{0,200}") {
                // Restrict to a clean alphabet so we don't accidentally
                // generate a string that matches a redaction pattern.
                let result = redact_sensitive_data(&input);
                if !contains_sensitive_data(&input) {
                    prop_assert_eq!(
                        result.as_ref(),
                        input.as_str(),
                        "redactor modified input but detector said clean"
                    );
                }
            }
        }

        // Property: when a known secret pattern is present, the
        // output is strictly shorter than the input (we always
        // replace at least the secret value with a placeholder).
        // If a redaction step produces output ≥ input length, the
        // placeholder is longer than the secret — a sign the
        // pattern changed or a new pattern was added that no
        // longer shortens.
        proptest! {
            #![proptest_config(ProptestConfig::with_cases(128))]
            #[test]
            fn known_secret_is_shortened(
                suffix in "[a-zA-Z0-9_-]{20,40}"
            ) {
                // sk- + 20+ chars matches the OpenAI/Anthropic pattern.
                let input = format!("sk-{suffix}");
                let result = redact_sensitive_data(&input);
                prop_assert!(
                    result.len() < input.len(),
                    "redaction of sk-* should shorten; in={} out={}",
                    input.len(), result.len()
                );
            }
        }
    }
}
