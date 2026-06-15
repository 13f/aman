//! Snapshot tests for `kernel::redactor::redact_sensitive_data`.
//!
//! These tests ensure the redactor consistently redacts known sensitive patterns.
//! Run with `cargo insta review` to accept new snapshots after intentional changes.

use kernel::redactor::redact_sensitive_data;

#[test]
fn redact_openai_api_key() {
    let input = "sk-proj-abc123def456ghi789jkl012mno345pqr678stu";
    let output = redact_sensitive_data(input);
    insta::assert_snapshot!(format!("input:  {input}\noutput: {output}"));
}

#[test]
fn redact_anthropic_api_key() {
    let input = "x-api-key: sk-ant-api03-abc123def456ghi789jkl012mno345pqr678stu901vwx";
    let output = redact_sensitive_data(input);
    insta::assert_snapshot!(format!("input:  {input}\noutput: {output}"));
}

#[test]
fn redact_bearer_jwt() {
    let input = "Bearer eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dozjgNryP4J3jVmNHl0w5N_XgL0n3I9PlFUP0THsR8g";
    let output = redact_sensitive_data(input);
    insta::assert_snapshot!(format!("input:  {input}\noutput: {output}"));
}

#[test]
fn redact_authorization_bearer() {
    let input = "Authorization: Bearer abc123def456ghi789jkl012mno345";
    let output = redact_sensitive_data(input);
    insta::assert_snapshot!(format!("input:  {input}\noutput: {output}"));
}

#[test]
fn redact_api_key_field() {
    let input = r#"{"api_key": "my-secret-api-key-12345", "user": "alice"}"#;
    let output = redact_sensitive_data(input);
    insta::assert_snapshot!(format!("input:  {input}\noutput: {output}"));
}

#[test]
fn redact_password_param() {
    let input = "password=super-secret-password-999&user=admin";
    let output = redact_sensitive_data(input);
    insta::assert_snapshot!(format!("input:  {input}\noutput: {output}"));
}

#[test]
fn redact_env_token() {
    let input = "AMAN_API_TOKEN=my-aman-token-value-here";
    let output = redact_sensitive_data(input);
    insta::assert_snapshot!(format!("input:  {input}\noutput: {output}"));
}

#[test]
fn no_redaction_for_clean_text() {
    let input = "Hello, how are you? This is a normal message.";
    let output = redact_sensitive_data(input);
    insta::assert_snapshot!(format!("input:  {input}\noutput: {output}"));
}
