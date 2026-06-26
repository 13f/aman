// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! Shared utilities for LLM providers — SSE stream parsing, HTTP client
//! construction, and tool call argument parsing.
//!
//! Extracted from `openai.rs` so that Anthropic and future providers can
//! reuse the same SSE line buffer, argument parser, and HTTP builder.

use crate::react::ParsedToolCall;
use serde_json::Value;

// ---------------------------------------------------------------------------
// SSE stream parser
// ---------------------------------------------------------------------------

/// Generic SSE (Server-Sent Events) line parser.
///
/// Feed raw bytes via [`feed`] and drain complete SSE data lines via
/// [`drain_lines`]. Handles the `data: ...` prefix and `[DONE]` sentinel
/// used by both OpenAI and Anthropic streaming endpoints.
///
/// [`feed`]: SseParser::feed
/// [`drain_lines`]: SseParser::drain_lines
#[derive(Debug, Default)]
pub struct SseParser {
    buffer: String,
}

impl SseParser {
    /// Create a new empty SSE parser.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed a raw chunk of bytes into the parser.
    pub fn feed(&mut self, chunk: &[u8]) {
        self.buffer.push_str(&String::from_utf8_lossy(chunk));
    }

    /// Drain all complete SSE data lines from the buffer.
    ///
    /// Returns a vec of data payload strings (the content after `data: `).
    /// The special sentinel `[DONE]` halts parsing and is not included.
    /// Lines without a `data: ` prefix are silently skipped.
    pub fn drain_lines(&mut self) -> Vec<String> {
        let mut lines = Vec::new();
        while let Some(newline_pos) = self.buffer.find('\n') {
            let line = self.buffer[..newline_pos].to_string();
            self.buffer = self.buffer[newline_pos + 1..].to_string();

            if let Some(data) = line.strip_prefix("data: ") {
                let data = data.trim();
                if data == "[DONE]" {
                    break;
                }
                if !data.is_empty() {
                    lines.push(data.to_owned());
                }
            }
        }
        lines
    }
}

// ---------------------------------------------------------------------------
// HTTP client builder
// ---------------------------------------------------------------------------

/// Build a `reqwest::Client` with common settings for LLM API calls.
///
/// - No system proxy (direct connection to API endpoints)
/// - TLS via rustls
/// - Configurable timeout
///
/// # Errors
/// Returns a `String` error if the client builder fails (should not happen
/// with sensible parameters).
pub fn build_http_client(timeout_secs: u64) -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(timeout_secs))
        .no_proxy()
        .build()
        .map_err(|e| format!("build HTTP client: {e}"))
}

/// Build a `reqwest::Client` without the `no_proxy()` call — used for
/// streaming connections where `no_proxy()` may interfere with some
/// proxy configurations.
pub fn build_streaming_client(timeout_secs: u64) -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(timeout_secs))
        .build()
        .map_err(|e| format!("build streaming HTTP client: {e}"))
}

// ---------------------------------------------------------------------------
// Tool call argument parsing
// ---------------------------------------------------------------------------

/// Parse a tool call's arguments from a function object value.
///
/// Handles both formats:
/// - JSON string (`"arguments": "{\"key\": \"value\"}"`) — OpenAI spec
/// - Pre-parsed JSON object (`"arguments": {"key": "value"}`) — some local models
///
/// Returns a `ParsedToolCall` with the parsed args (or empty object on failure).
/// Logs warnings for unparseable arguments (never panics).
pub fn parse_tool_call(id: String, tool_name: String, function: &Value) -> ParsedToolCall {
    let args = parse_function_args(&tool_name, function);
    ParsedToolCall {
        id,
        tool_name,
        args,
    }
}

/// Parse the arguments field from a function object.
///
/// Accepts arguments as either a JSON string (OpenAI spec) or a pre-parsed
/// JSON object (some non-OpenAI providers / local models).
fn parse_function_args(tool_name: &str, function: &Value) -> Value {
    match function.get("arguments") {
        Some(v) if v.is_string() => {
            let s = v.as_str().unwrap_or("");
            if s.is_empty() {
                tracing::warn!(
                    tool_name,
                    "tool call with empty arguments string — defaulting to empty object"
                );
            }
            serde_json::from_str(s).unwrap_or_else(|e| {
                tracing::warn!(
                    tool_name,
                    error = %e,
                    "failed to parse tool call arguments JSON — defaulting to empty object"
                );
                Value::Object(Default::default())
            })
        }
        Some(v) if v.is_object() => {
            if v.as_object().map(|o| o.is_empty()).unwrap_or(true) {
                tracing::warn!(
                    tool_name,
                    "tool call with empty arguments object"
                );
            }
            v.clone()
        }
        _ => {
            tracing::warn!(
                tool_name,
                "tool call with missing or unexpected arguments type — defaulting to empty object"
            );
            Value::Object(Default::default())
        }
    }
}

// ---------------------------------------------------------------------------
// Error formatting
// ---------------------------------------------------------------------------

/// Format an API error from HTTP status + response body.
pub fn api_error(status: reqwest::StatusCode, body: &str) -> String {
    let truncated: String = body.chars().take(500).collect();
    format!("LLM API error HTTP {status}: {truncated}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // ── SseParser ────────────────────────────────────────────────

    #[test]
    fn sse_parser_single_line() {
        let mut parser = SseParser::new();
        parser.feed(b"data: {\"key\":\"val\"}\n");
        let lines = parser.drain_lines();
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0], "{\"key\":\"val\"}");
    }

    #[test]
    fn sse_parser_multiple_lines() {
        let mut parser = SseParser::new();
        parser.feed(b"data: line1\ndata: line2\n");
        let lines = parser.drain_lines();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0], "line1");
        assert_eq!(lines[1], "line2");
    }

    #[test]
    fn sse_parser_ignores_non_data_lines() {
        let mut parser = SseParser::new();
        parser.feed(b"event: message_start\ndata: payload\nevent: done\n");
        let lines = parser.drain_lines();
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0], "payload");
    }

    #[test]
    fn sse_parser_done_sentinel_stops() {
        let mut parser = SseParser::new();
        parser.feed(b"data: line1\ndata: [DONE]\ndata: should_not_appear\n");
        let lines = parser.drain_lines();
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0], "line1");
    }

    #[test]
    fn sse_parser_partial_chunk() {
        let mut parser = SseParser::new();
        parser.feed(b"data: first\n");
        parser.feed(b"data: second\n");
        let lines = parser.drain_lines();
        assert_eq!(lines.len(), 2);
    }

    #[test]
    fn sse_parser_incomplete_line_held() {
        let mut parser = SseParser::new();
        parser.feed(b"data: incompl");
        let lines = parser.drain_lines();
        assert!(lines.is_empty()); // no newline yet
        parser.feed(b"ete\n");
        let lines = parser.drain_lines();
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0], "incomplete");
    }

    // ── parse_tool_call ──────────────────────────────────────────

    #[test]
    fn parse_tool_args_json_string() {
        let func = json!({"name": "greet", "arguments": "{\"name\": \"Alice\"}"});
        let tc = parse_tool_call("id1".into(), "greet".into(), &func);
        assert_eq!(tc.id, "id1");
        assert_eq!(tc.tool_name, "greet");
        assert_eq!(tc.args, json!({"name": "Alice"}));
    }

    #[test]
    fn parse_tool_args_pre_parsed_object() {
        let func = json!({"name": "search", "arguments": {"query": "hello"}});
        let tc = parse_tool_call("id2".into(), "search".into(), &func);
        assert_eq!(tc.args, json!({"query": "hello"}));
    }

    #[test]
    fn parse_tool_args_empty_string_defaults_to_empty_object() {
        let func = json!({"name": "t", "arguments": ""});
        let tc = parse_tool_call("id3".into(), "t".into(), &func);
        assert_eq!(tc.args, json!({}));
    }

    #[test]
    fn parse_tool_args_invalid_json_defaults_to_empty_object() {
        let func = json!({"name": "t", "arguments": "not json"});
        let tc = parse_tool_call("id4".into(), "t".into(), &func);
        assert_eq!(tc.args, json!({}));
    }
}
