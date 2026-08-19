// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! Anthropic LLM provider — implements [`LlmProvider`] for Anthropic's
//! `/v1/messages` API.
//!
//! Handles both streaming (SSE) and non-streaming chat completions.
//! Converts between the internal ChatMessage/ToolDescriptor formats and
//! Anthropic's native message/tool_use content blocks.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use futures_util::StreamExt;
use serde_json::{json, Value};

use crate::provider::{
    LlmChatRequest, LlmProvider, LlmResponse, ResponseFormat, StreamEvent, TokenUsage,
};
use crate::react::{ChatMessage, ParsedToolCall, ToolDescriptor};
use crate::shared::{self, SseParser};

const ANTHROPIC_VERSION: &str = "2023-06-01";
const DEFAULT_MAX_TOKENS: u32 = 4096;
const REQUEST_TIMEOUT_SECS: u64 = 120;
const STREAM_TIMEOUT_SECS: u64 = 600;

// ---------------------------------------------------------------------------
// LlmAnthropicProvider — implements the LlmProvider trait
// ---------------------------------------------------------------------------

/// Anthropic LLM provider for `/v1/messages`.
///
/// Handles both streaming (SSE) and non-streaming chat completions.
/// Converts internal message/tool formats to Anthropic's native content
/// block structure.
pub struct LlmAnthropicProvider {
    api_key: String,
    base_url: String,
    model: String,
    /// Extended thinking budget in tokens. `None` = disabled (default).
    /// When set, the model performs extended reasoning before responding.
    thinking_budget_tokens: Option<u32>,
}

impl LlmAnthropicProvider {
    /// Create a new Anthropic provider.
    ///
    /// `base_url` should be the API root, e.g. `"https://api.anthropic.com/v1"`.
    pub fn new(base_url: &str, api_key: &str, model: &str) -> Self {
        Self {
            api_key: api_key.to_owned(),
            base_url: base_url.trim_end_matches('/').to_owned(),
            model: model.to_owned(),
            thinking_budget_tokens: None,
        }
    }

    /// Enable extended thinking with the given token budget.
    ///
    /// Extended thinking requires at least 1024 tokens and the model
    /// must be a Claude 3.7+ thinking model (e.g. `claude-sonnet-4-6`).
    #[must_use]
    pub fn with_thinking(mut self, budget_tokens: u32) -> Self {
        self.thinking_budget_tokens = Some(budget_tokens.max(1024));
        self
    }

    // ── Message format conversion ──────────────────────────────────

    /// Convert a single [`ChatMessage`] to Anthropic's content-block format.
    fn message_to_anthropic(msg: &ChatMessage) -> Value {
        let role = match msg.role {
            crate::react::ChatMessageRole::System => "system",
            crate::react::ChatMessageRole::User => "user",
            crate::react::ChatMessageRole::Assistant => "assistant",
            crate::react::ChatMessageRole::Tool => "user", // tool results are user messages
        };

        let mut m = json!({"role": role});

        // Build content array (Anthropic uses content blocks, not plain text)
        let mut content_blocks: Vec<Value> = Vec::new();

        // If this is a tool result message
        if role == "user" && let Some(ref tool_call_id) = msg.tool_call_id {
            content_blocks.push(json!({
                "type": "tool_result",
                "tool_use_id": tool_call_id,
                "content": msg.content,
            }));
        } else if role == "assistant" && msg.tool_calls.is_some() {
            // Assistant message with tool calls — include both text and tool_use blocks
            if !msg.content.is_empty() {
                content_blocks.push(json!({
                    "type": "text",
                    "text": msg.content,
                }));
            }
            if let Some(ref calls) = msg.tool_calls {
                for tc in calls {
                    content_blocks.push(json!({
                        "type": "tool_use",
                        "id": tc["id"],
                        "name": tc["function"]["name"],
                        "input": tc["function"].get("arguments")
                            .and_then(|a| {
                                if a.is_string() {
                                    serde_json::from_str::<Value>(a.as_str().unwrap_or("{}")).ok()
                                } else {
                                    Some(a.clone())
                                }
                            })
                            .unwrap_or(json!({})),
                    }));
                }
            }
            if !msg.reasoning_content.is_empty() {
                content_blocks.push(json!({
                    "type": "thinking",
                    "thinking": msg.reasoning_content,
                }));
            }
        } else {
            // Simple text message (user or assistant without tools)
            content_blocks.push(json!({
                "type": "text",
                "text": msg.content,
            }));
        }

        m["content"] = json!(content_blocks);
        m
    }

    /// Convert [`ToolDescriptor`]s to Anthropic's tool format.
    fn tools_to_anthropic(tools: &[ToolDescriptor]) -> Vec<Value> {
        tools
            .iter()
            .map(|td| {
                json!({
                    "name": td.name,
                    "description": td.description,
                    "input_schema": td.parameters,
                })
            })
            .collect()
    }

    /// Parse Anthropic content blocks into text + tool calls.
    fn parse_content_blocks(content_blocks: &[Value]) -> (String, String, Vec<ParsedToolCall>) {
        let mut text = String::new();
        let mut reasoning = String::new();
        let mut tool_calls: Vec<ParsedToolCall> = Vec::new();

        for block in content_blocks {
            match block.get("type").and_then(|t| t.as_str()) {
                Some("text") => {
                    if let Some(t) = block.get("text").and_then(|v| v.as_str()) {
                        text.push_str(t);
                    }
                }
                Some("tool_use") => {
                    let id = block
                        .get("id")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_owned();
                    let name = block
                        .get("name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_owned();
                    let input = block.get("input").cloned().unwrap_or(json!({}));
                    tool_calls.push(ParsedToolCall {
                        id,
                        tool_name: name,
                        args: input,
                    });
                }
                Some("thinking") => {
                    if let Some(t) = block.get("thinking").and_then(|v| v.as_str()) {
                        reasoning.push_str(t);
                    }
                }
                _ => {}
            }
        }

        (text, reasoning, tool_calls)
    }

    /// Build the common request body for both streaming and non-streaming.
    fn build_request_body(
        &self,
        req: &LlmChatRequest,
        stream: bool,
    ) -> Value {
        // Convert system prompt to Anthropic's top-level "system" field
        let system = if req.system_prompt.is_empty() {
            None
        } else {
            Some(json!(req.system_prompt))
        };

        // Convert messages
        let messages: Vec<Value> = req
            .messages
            .iter()
            .map(Self::message_to_anthropic)
            .collect();

        let anthropic_tools = Self::tools_to_anthropic(&req.tools);

        let max_tokens = if req.max_output_tokens > 0 {
            req.max_output_tokens
        } else {
            DEFAULT_MAX_TOKENS
        };

        let mut body = json!({
            "model": self.model,
            "messages": messages,
            "max_tokens": max_tokens,
            "stream": stream,
        });

        if let Some(ref sys) = system {
            body["system"] = sys.clone();
        }

        if !anthropic_tools.is_empty() {
            body["tools"] = json!(anthropic_tools);
        }

        // Extended thinking
        if let Some(budget) = self.thinking_budget_tokens {
            body["thinking"] = json!({
                "type": "enabled",
                "budget_tokens": budget,
            });
            // When thinking is enabled, max_tokens must include the thinking budget
            body["max_tokens"] = json!(max_tokens + budget);
        }

        // Response format (Anthropic-specific)
        if let Some(ref fmt) = req.response_format {
            match fmt {
                ResponseFormat::JsonObject => {
                    // Anthropic doesn't have json_object mode — use system prompt hint
                    // (already handled by prompt pipeline)
                }
                ResponseFormat::JsonSchema { name, schema, .. } => {
                    body["tool_choice"] = json!({
                        "type": "tool",
                        "name": name,
                    });
                    // Add schema as a tool definition
                    let schema_tool = json!({
                        "name": name,
                        "description": format!("Respond using the {} schema", name),
                        "input_schema": schema,
                    });
                    if let Some(tools) = body.get_mut("tools").and_then(|t| t.as_array_mut()) {
                        tools.push(schema_tool);
                    }
                }
            }
        }

        body
    }
}

// ---------------------------------------------------------------------------
// LlmProvider implementation
// ---------------------------------------------------------------------------

#[async_trait]
impl LlmProvider for LlmAnthropicProvider {
    fn name(&self) -> &str {
        "anthropic"
    }

    fn base_url(&self) -> &str {
        &self.base_url
    }

    async fn chat_completion(
        &self,
        req: LlmChatRequest,
        cb: Option<Arc<dyn Fn(StreamEvent) + Send + Sync>>,
    ) -> Result<LlmResponse, String> {
        if let Some(cb) = cb {
            self.streaming_completion(req, cb).await
        } else {
            self.non_streaming_completion(req).await
        }
    }
}

// ---------------------------------------------------------------------------
// Non-streaming path
// ---------------------------------------------------------------------------

impl LlmAnthropicProvider {
    async fn non_streaming_completion(
        &self,
        req: LlmChatRequest,
    ) -> Result<LlmResponse, String> {
        let body = self.build_request_body(&req, false);

        let url = format!("{}/messages", self.base_url);
        let client = shared::build_http_client(REQUEST_TIMEOUT_SECS)?;

        let response = client
            .post(&url)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", ANTHROPIC_VERSION)
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("request failed: {e}"))?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            return Err(shared::api_error(status, &text));
        }

        let response_text = response
            .text()
            .await
            .map_err(|e| format!("read response: {e}"))?;

        let response_body: Value =
            serde_json::from_str(&response_text).map_err(|e| {
                format!(
                    "parse Anthropic response: {e} — first 500 chars: {}",
                    &response_text[..response_text.len().min(500)]
                )
            })?;

        let stop_reason = response_body
            .get("stop_reason")
            .and_then(|v| v.as_str())
            .unwrap_or("end_turn")
            .to_owned();

        let finish_reason = map_stop_reason(&stop_reason);

        let content_blocks = response_body
            .get("content")
            .and_then(|v| v.as_array())
            .map(|arr| arr.as_slice())
            .unwrap_or(&[]);

        let (content, reasoning_content, tool_calls) =
            Self::parse_content_blocks(content_blocks);

        // Anthropic's usage schema: { input_tokens, output_tokens }
        let usage = response_body.get("usage").and_then(|u| {
            let input = u.get("input_tokens")?.as_u64()?;
            let output = u.get("output_tokens")?.as_u64()?;
            Some(TokenUsage {
                prompt_tokens: input,
                completion_tokens: output,
                total_tokens: input + output,
            })
        });

        Ok(LlmResponse {
            content,
            finish_reason,
            tool_calls,
            reasoning_content,
            usage,
        })
    }

    // ── Streaming path ─────────────────────────────────────────────

    async fn streaming_completion(
        &self,
        req: LlmChatRequest,
        cb: Arc<dyn Fn(StreamEvent) + Send + Sync>,
    ) -> Result<LlmResponse, String> {
        let body = self.build_request_body(&req, true);

        let url = format!("{}/messages", self.base_url);
        let client = shared::build_streaming_client(STREAM_TIMEOUT_SECS)?;

        let response = client
            .post(&url)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", ANTHROPIC_VERSION)
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("streaming request failed: {e}"))?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            return Err(shared::api_error(status, &text));
        }

        cb(StreamEvent::Start);

        let mut stream = response.bytes_stream();
        let mut sse_parser = SseParser::new();
        let mut full_text = String::new();
        let mut reasoning = String::new();
        let mut tool_use_acc: HashMap<usize, Value> = HashMap::new();
        let mut active_block_type: Option<String> = None;
        let mut active_block_index: usize = 0;
        let mut _text_block_count: usize = 0;
        let mut finish_reason = "end_turn".to_owned();
        let mut current_usage: Option<TokenUsage> = None;

        while let Some(chunk_result) = stream.next().await {
            let chunk = chunk_result.map_err(|e| format!("stream error: {e}"))?;
            sse_parser.feed(&chunk);

            for data in sse_parser.drain_lines() {
                let Ok(event) = serde_json::from_str::<Value>(&data) else {
                    continue;
                };

                let event_type = event
                    .get("type")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");

                match event_type {
                    "message_start" => {
                        // Message started — no action needed
                    }

                    "content_block_start" => {
                        let block = &event["content_block"];
                        let block_type = block
                            .get("type")
                            .and_then(|v| v.as_str())
                            .unwrap_or("");
                        active_block_type = Some(block_type.to_owned());

                        match block_type {
                            "text" => {
                                _text_block_count += 1;
                            }
                            "tool_use" => {
                                let idx = block
                                    .get("index")
                                    .and_then(|i| i.as_u64())
                                    .unwrap_or(0) as usize;
                                active_block_index = idx;
                                let name = block
                                    .get("name")
                                    .and_then(|n| n.as_str())
                                    .unwrap_or("");
                                let id = block
                                    .get("id")
                                    .and_then(|i| i.as_str())
                                    .unwrap_or("");
                                tool_use_acc
                                    .entry(idx)
                                    .or_insert_with(|| {
                                        json!({
                                            "id": id,
                                            "name": name,
                                            "input": {},
                                        })
                                    });
                            }
                            "thinking" => {
                                // Thinking block — accumulate into reasoning_content
                            }
                            _ => {}
                        }
                    }

                    "content_block_delta" => {
                        let delta = &event["delta"];
                        match delta.get("type").and_then(|v| v.as_str()) {
                            Some("text_delta") => {
                                if let Some(text) = delta
                                    .get("text")
                                    .and_then(|v| v.as_str())
                                {
                                    full_text.push_str(text);
                                    cb(StreamEvent::Chunk(text.to_owned()));
                                }
                            }
                            Some("input_json_delta") => {
                                if let (Some(partial), Some(entry)) = (
                                    delta.get("partial_json").and_then(|v| v.as_str()),
                                    tool_use_acc.get_mut(&active_block_index),
                                ) {
                                    let current = entry["input"]
                                        .as_str()
                                        .unwrap_or("");
                                    entry["input"] =
                                        json!(current.to_owned() + partial);
                                }
                            }
                            Some("thinking_delta") => {
                                if let Some(thinking) = delta
                                    .get("thinking")
                                    .and_then(|v| v.as_str())
                                {
                                    reasoning.push_str(thinking);
                                    cb(StreamEvent::Reasoning(thinking.to_owned()));
                                }
                            }
                            _ => {}
                        }
                    }

                    "content_block_stop" => {
                        // Parse completed tool_use input from JSON string
                        if active_block_type.as_deref() == Some("tool_use")
                            && let Some(entry) = tool_use_acc.get_mut(&active_block_index)
                            && let Some(input_str) = entry["input"].as_str()
                            && let Ok(parsed) = serde_json::from_str::<Value>(input_str)
                        {
                            entry["input"] = parsed;
                        }
                        active_block_type = None;
                    }

                    "message_delta" => {
                        if let Some(delta) = event.get("delta")
                            && let Some(sr) = delta
                                .get("stop_reason")
                                .and_then(|v| v.as_str())
                        {
                            finish_reason = sr.to_owned();
                        }
                        // Anthropic streams usage in the message_delta event.
                        if let Some(usage) = event.get("usage")
                            && let (Some(input), Some(output)) = (
                                usage.get("input_tokens").and_then(|v| v.as_u64()),
                                usage.get("output_tokens").and_then(|v| v.as_u64()),
                            )
                        {
                            current_usage = Some(TokenUsage {
                                prompt_tokens: input,
                                completion_tokens: output,
                                total_tokens: input + output,
                            });
                        }
                    }

                    "message_stop" => {
                        cb(StreamEvent::Done {
                            finish_reason: map_stop_reason(&finish_reason),
                        });
                    }

                    "ping" => {
                        // Anthropic sends periodic pings to keep connection alive
                    }

                    _ => {}
                }
            }
        }

        // Parse accumulated tool calls from streaming
        let tool_calls: Vec<ParsedToolCall> = tool_use_acc
            .into_values()
            .map(|tc| {
                let id = tc["id"].as_str().unwrap_or("").to_owned();
                let name = tc["name"].as_str().unwrap_or("").to_owned();
                let args = tc["input"].clone();
                ParsedToolCall {
                    id,
                    tool_name: name,
                    args,
                }
            })
            .collect();

        Ok(LlmResponse {
            content: full_text,
            finish_reason: map_stop_reason(&finish_reason),
            tool_calls,
            reasoning_content: reasoning,
            usage: current_usage,
        })
    }
}

// ── Helpers ─────────────────────────────────────────────────────────

/// Map Anthropic stop_reason to OpenAI-compatible finish_reason.
fn map_stop_reason(stop_reason: &str) -> String {
    match stop_reason {
        "end_turn" => "stop".to_owned(),
        "max_tokens" => "length".to_owned(),
        "tool_use" => "tool_calls".to_owned(),
        "stop_sequence" => "stop".to_owned(),
        other => other.to_owned(),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::react::ChatMessageRole;

    #[test]
    fn provider_name_is_anthropic() {
        let provider = LlmAnthropicProvider::new(
            "https://api.anthropic.com/v1",
            "test-key",
            "claude-sonnet-4-6",
        );
        assert_eq!(provider.name(), "anthropic");
    }

    #[test]
    fn user_message_to_anthropic() {
        let msg = ChatMessage {
            role: ChatMessageRole::User,
            content: "Hello".to_owned(),
            tool_call_id: None,
            tool_name: None,
            tool_calls: None,
            reasoning_content: String::new(),
        };
        let value = LlmAnthropicProvider::message_to_anthropic(&msg);
        assert_eq!(value["role"], "user");
        assert_eq!(value["content"][0]["type"], "text");
        assert_eq!(value["content"][0]["text"], "Hello");
    }

    #[test]
    fn assistant_message_with_tool_calls() {
        let msg = ChatMessage {
            role: ChatMessageRole::Assistant,
            content: "Let me search.".to_owned(),
            tool_calls: Some(vec![json!({
                "id": "toolu_01",
                "type": "function",
                "function": {
                    "name": "search",
                    "arguments": "{\"query\":\"hello\"}"
                }
            })]),
            tool_call_id: None,
            tool_name: None,
            reasoning_content: String::new(),
        };
        let value = LlmAnthropicProvider::message_to_anthropic(&msg);
        let blocks = value["content"].as_array().unwrap();
        assert_eq!(blocks.len(), 2, "should have text + tool_use blocks");
        assert_eq!(blocks[0]["type"], "text");
        assert_eq!(blocks[1]["type"], "tool_use");
        assert_eq!(blocks[1]["name"], "search");
    }

    #[test]
    fn tool_result_message() {
        let msg = ChatMessage {
            role: ChatMessageRole::Tool,
            content: "result data".to_owned(),
            tool_call_id: Some("toolu_01".to_owned()),
            tool_name: None,
            tool_calls: None,
            reasoning_content: String::new(),
        };
        let value = LlmAnthropicProvider::message_to_anthropic(&msg);
        assert_eq!(value["role"], "user");
        assert_eq!(value["content"][0]["type"], "tool_result");
        assert_eq!(value["content"][0]["tool_use_id"], "toolu_01");
    }

    #[test]
    fn tools_to_anthropic_format() {
        let tools = vec![ToolDescriptor {
            name: "search".to_owned(),
            description: "Search the web".to_owned(),
            parameters: json!({"type": "object", "properties": {"query": {"type": "string"}}}),
        }];
        let result = LlmAnthropicProvider::tools_to_anthropic(&tools);
        assert_eq!(result[0]["name"], "search");
        assert_eq!(result[0]["input_schema"]["type"], "object");
        // Anthropic format uses "input_schema", not "parameters"
        assert!(result[0].get("parameters").is_none());
    }

    #[test]
    fn parse_content_blocks_text_only() {
        let blocks = vec![
            json!({"type": "text", "text": "Hello"}),
            json!({"type": "text", "text": " world"}),
        ];
        let (text, reasoning, tool_calls) =
            LlmAnthropicProvider::parse_content_blocks(&blocks);
        assert_eq!(text, "Hello world");
        assert!(reasoning.is_empty());
        assert!(tool_calls.is_empty());
    }

    #[test]
    fn parse_content_blocks_with_tool_use() {
        let blocks = vec![
            json!({"type": "text", "text": "Let me check."}),
            json!({
                "type": "tool_use",
                "id": "toolu_01",
                "name": "search",
                "input": {"query": "rust docs"}
            }),
        ];
        let (text, _, tool_calls) =
            LlmAnthropicProvider::parse_content_blocks(&blocks);
        assert_eq!(text, "Let me check.");
        assert_eq!(tool_calls.len(), 1);
        assert_eq!(tool_calls[0].tool_name, "search");
        assert_eq!(tool_calls[0].args, json!({"query": "rust docs"}));
    }

    #[test]
    fn parse_content_blocks_with_thinking() {
        let blocks = vec![
            json!({"type": "thinking", "thinking": "I should search first."}),
            json!({"type": "text", "text": "Here is the answer."}),
        ];
        let (text, reasoning, _) =
            LlmAnthropicProvider::parse_content_blocks(&blocks);
        assert_eq!(text, "Here is the answer.");
        assert_eq!(reasoning, "I should search first.");
    }

    #[test]
    fn map_stop_reason_mappings() {
        assert_eq!(map_stop_reason("end_turn"), "stop");
        assert_eq!(map_stop_reason("max_tokens"), "length");
        assert_eq!(map_stop_reason("tool_use"), "tool_calls");
        assert_eq!(map_stop_reason("stop_sequence"), "stop");
    }

    #[test]
    fn with_thinking_enables_extended_reasoning() {
        let provider = LlmAnthropicProvider::new(
            "https://api.anthropic.com/v1",
            "key",
            "claude-sonnet-4-6",
        )
        .with_thinking(4096);
        assert_eq!(provider.thinking_budget_tokens, Some(4096));
    }

    #[test]
    fn with_thinking_clamps_to_minimum() {
        let provider = LlmAnthropicProvider::new(
            "https://api.anthropic.com/v1",
            "key",
            "claude-sonnet-4-6",
        )
        .with_thinking(512); // below 1024 minimum
        assert_eq!(provider.thinking_budget_tokens, Some(1024));
    }

    #[test]
    fn build_request_body_includes_thinking() {
        let provider = LlmAnthropicProvider::new(
            "https://api.anthropic.com/v1",
            "key",
            "claude-sonnet-4-6",
        )
        .with_thinking(2048);

        let req = LlmChatRequest {
            model: "claude-sonnet-4-6".into(),
            system_prompt: "You are helpful.".into(),
            messages: vec![],
            tools: vec![],
            max_output_tokens: 4096,
            response_format: None,
        };

        let body = provider.build_request_body(&req, false);
        assert_eq!(body["thinking"]["type"], "enabled");
        assert_eq!(body["thinking"]["budget_tokens"], 2048);
        // max_tokens should be output + thinking budget
        assert_eq!(body["max_tokens"], 4096 + 2048);
    }
}
