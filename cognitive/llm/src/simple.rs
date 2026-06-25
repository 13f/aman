// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! Minimal OpenAI-compatible chat completion client.
//!
//! Provides a thin wrapper around HTTP-level chat completion calls.
//! Used by info-hub (article scoring), eval (LLM-as-Judge), and any
//! other internal consumer that needs simple prompt→response without
//! streaming, tool calls, or conversation history.
//!
//! Moved here from `llm-api` as part of the cognitive engine consolidation.
//! For full-featured LLM access (streaming, tools, multi-turn), use
//! the [`LlmProvider`](crate::provider::LlmProvider) trait instead.

use crate::provider::ResponseFormat;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tracing::warn;

// ── Config ─────────────────────────────────────────────────────────────

/// Configuration for an OpenAI-compatible chat completion endpoint.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmApiConfig {
    /// Base URL of the API (e.g. `https://api.openai.com/v1`).
    pub base_url: String,
    /// Optional API key (sent as `Authorization: Bearer {key}`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    /// Model ID to use (e.g. `gpt-4o`, `deepseek-v4-flash`).
    pub model: String,
}

// ── Provider ───────────────────────────────────────────────────────────

/// A reusable HTTP-level chat completion client.
///
/// Holds a shared `reqwest::Client` for connection reuse. Create one
/// instance per runtime and clone it (the `Client` is internally `Arc`-ed).
#[derive(Debug, Clone)]
pub struct SimpleLlmClient {
    client: reqwest::Client,
}

impl Default for SimpleLlmClient {
    fn default() -> Self {
        Self::new()
    }
}

impl SimpleLlmClient {
    /// Create a new client with a default HTTP client.
    #[must_use]
    pub fn new() -> Self {
        Self {
            client: reqwest::Client::new(),
        }
    }

    /// Create a client with a custom `reqwest::Client`.
    #[must_use]
    pub fn with_client(client: reqwest::Client) -> Self {
        Self { client }
    }

    /// One-shot chat completion: send a system + user prompt and return the
    /// text response.
    ///
    /// When `response_format` is `Some`, the request includes the appropriate
    /// `response_format` field (`json_object` or `json_schema`).
    ///
    /// # Errors
    ///
    /// Returns `Err(String)` on network errors, non-2xx responses, or
    /// unparseable response bodies.
    #[allow(clippy::too_many_arguments)]
    pub async fn chat_completion(
        &self,
        config: &LlmApiConfig,
        system_prompt: &str,
        user_prompt: &str,
        temperature: f64,
        max_tokens: u64,
        timeout_secs: u64,
        response_format: Option<&ResponseFormat>,
    ) -> Result<String, String> {
        let url = format!(
            "{}/chat/completions",
            config.base_url.trim_end_matches('/')
        );

        let mut body = serde_json::json!({
            "model": config.model,
            "messages": [
                {"role": "system", "content": system_prompt},
                {"role": "user", "content": user_prompt}
            ],
            "temperature": temperature,
            "max_tokens": max_tokens,
        });

        // DeepSeek API requires `max_completion_tokens` instead of `max_tokens`
        // for some models. Include both — the API ignores the one it doesn't
        // recognise.
        if let Some(obj) = body.as_object_mut() {
            obj.insert(
                "max_completion_tokens".into(),
                serde_json::json!(max_tokens),
            );
        }

        if let Some(fmt) = response_format
            && let Some(obj) = body.as_object_mut()
        {
            obj.insert("response_format".into(), response_format_json(fmt));
        }

        let client = if timeout_secs > 0 {
            reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(timeout_secs))
                .build()
                .map_err(|e| format!("client build: {e}"))?
        } else {
            self.client.clone()
        };

        let mut req = client.post(&url).json(&body);

        if let Some(key) = &config.api_key {
            req = req.header("Authorization", format!("Bearer {key}"));
        }

        let resp = req.send().await.map_err(|e| format!("request: {e}"))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(format!("API error ({status}): {text}"));
        }

        let data: Value =
            resp.json().await.map_err(|e| format!("parse: {e}"))?;

        let content = data
            .pointer("/choices/0/message/content")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        Ok(content.to_string())
    }

    /// Chat completion with exponential-backoff retries.
    #[allow(clippy::too_many_arguments)]
    pub async fn chat_completion_with_retries(
        &self,
        config: &LlmApiConfig,
        system_prompt: &str,
        user_prompt: &str,
        temperature: f64,
        max_tokens: u64,
        timeout_secs: u64,
        retries: u32,
        response_format: Option<&ResponseFormat>,
    ) -> Result<String, String> {
        let mut last_err = String::new();
        for attempt in 0..retries {
            if attempt > 0 {
                let delay = std::time::Duration::from_millis(
                    std::cmp::min(1000u64 * 2u64.pow(attempt - 1), 8000),
                );
                tokio::time::sleep(delay).await;
            }
            match self
                .chat_completion(
                    config,
                    system_prompt,
                    user_prompt,
                    temperature,
                    max_tokens,
                    timeout_secs,
                    response_format,
                )
                .await
            {
                Ok(text) => return Ok(text),
                Err(e) => {
                    warn!(attempt, %e, "llm-api call failed, retrying");
                    last_err = e;
                }
            }
        }
        Err(last_err)
    }
}

// ── Response Format Helper ──────────────────────────────────────────────

/// Convert a [`ResponseFormat`] to the JSON value sent in the request body.
pub fn response_format_json(fmt: &ResponseFormat) -> Value {
    match fmt {
        ResponseFormat::JsonObject => serde_json::json!({"type": "json_object"}),
        // json_object is universally supported (OpenAI, DeepSeek, etc.);
        // json_schema is OpenAI-only. Schema enforcement happens in
        // post-processing via parse_json_response + caller validation.
        ResponseFormat::JsonSchema { .. } => {
            serde_json::json!({"type": "json_object"})
        }
    }
}

// ── JSON Utilities ─────────────────────────────────────────────────────

/// Robust JSON extraction from LLM output.
///
/// Handles common LLM formatting quirks:
/// - Markdown code fences (```json ... ```)
/// - Smart/curly quotes replaced with ASCII equivalents
/// - Trailing ``` mid-stream
/// - Unmatched braces (repaired)
/// - Nested object extraction via brace matching
pub fn parse_json_response<T: serde::de::DeserializeOwned>(
    text: &str,
) -> Result<T, String> {
    let mut json_text = text
        .replace(['\u{201C}', '\u{201D}'], "\u{FF02}") // " → ＂
        .replace(['\u{2018}', '\u{2019}'], "\u{FF07}") // ' → ＇
        .trim()
        .to_string();

    // Strip markdown code fences
    if json_text.starts_with("```") {
        if let Some(rest) = json_text.strip_prefix("```json") {
            json_text = rest.to_string();
        } else if let Some(rest) = json_text.strip_prefix("```") {
            json_text = rest.to_string();
        }
        if let Some(end) = json_text.rfind("```") {
            json_text = json_text[..end].to_string();
        }
        json_text = json_text.trim().to_string();
    }

    // Strip trailing ``` mid-stream
    if let Some(pos) = json_text.find("\n```") {
        json_text = json_text[..pos].trim().to_string();
    }

    // Extract JSON object by brace matching
    if let Some(first_brace) = json_text.find('{') {
        let chars: Vec<char> = json_text.chars().collect();
        let mut depth = 0i32;
        let mut in_string = false;
        let mut end = None;
        for (i, &ch) in chars.iter().enumerate().skip(first_brace) {
            if in_string {
                if ch == '\\' { continue; }
                if ch == '"' { in_string = false; }
            } else {
                match ch {
                    '"' => in_string = true,
                    '{' => depth += 1,
                    '}' => {
                        depth -= 1;
                        if depth == 0 {
                            end = Some(i);
                            break;
                        }
                    }
                    _ => {}
                }
            }
        }
        if let Some(e) = end {
            json_text = chars[first_brace..=e].iter().collect();
        } else {
            json_text = chars[first_brace..].iter().collect();
        }
    }

    // Try parse, then repair if needed
    if let Ok(v) = serde_json::from_str::<T>(&json_text) {
        return Ok(v);
    }

    // Repair: close unmatched braces
    let mut repaired = json_text.clone();
    let quotes = repaired.matches('"').count();
    if !quotes.is_multiple_of(2) {
        repaired.push('"');
    }
    let mut stack: Vec<char> = Vec::new();
    let mut in_str = false;
    for ch in repaired.chars() {
        if in_str {
            if ch == '\\' { continue; }
            if ch == '"' { in_str = false; }
        } else {
            match ch {
                '"' => in_str = true,
                '{' | '[' => stack.push(ch),
                '}' if stack.last() == Some(&'{') => { stack.pop(); }
                ']' if stack.last() == Some(&'[') => { stack.pop(); }
                _ => {}
            }
        }
    }
    for ch in stack.iter().rev() {
        repaired.push(if *ch == '{' { '}' } else { ']' });
    }

    serde_json::from_str::<T>(&repaired)
        .map_err(|e| format!("JSON parse after repair: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    #[derive(Debug, Deserialize, PartialEq)]
    struct TestScores {
        scores: std::collections::HashMap<String, f64>,
    }

    #[test]
    fn parse_plain_json() {
        let input = r#"{"scores": {"correctness": 0.8}}"#;
        let result: TestScores = parse_json_response(input).unwrap();
        assert!((result.scores["correctness"] - 0.8).abs() < 0.001);
    }

    #[test]
    fn parse_fenced_json() {
        let input = "```json\n{\"scores\": {\"correctness\": 0.9}}\n```";
        let result: TestScores = parse_json_response(input).unwrap();
        assert!((result.scores["correctness"] - 0.9).abs() < 0.001);
    }

    #[test]
    fn parse_generic_fence() {
        let input = "```\n{\"scores\": {\"correctness\": 0.7}}\n```";
        let result: TestScores = parse_json_response(input).unwrap();
        assert!((result.scores["correctness"] - 0.7).abs() < 0.001);
    }

    #[test]
    fn parse_with_smart_quotes() {
        let input = "{\"scores\": {\"correctness\": 0.5}}";
        let result: TestScores = parse_json_response(input).unwrap();
        assert!((result.scores["correctness"] - 0.5).abs() < 0.001);
    }
}
