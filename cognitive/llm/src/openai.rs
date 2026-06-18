// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! OpenAI-compatible LLM provider — implements `LlmProvider`.
//!
//! Moved here from `llm-provider-openai` plugin as part of the cognitive
//! engine consolidation. Handles both streaming (SSE) and non-streaming
//! chat completions.

use async_trait::async_trait;
use futures_util::StreamExt;
use kernel::context::ToolContext;
use kernel::schema::JsonSchema;
use kernel::tool::Tool;
use kernel::types::ToolMode;
use kernel::{AmanResult, Error};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::LazyLock;

use crate::provider::{LlmChatRequest, LlmProvider, LlmResponse, StreamEvent};
use crate::react::{ChatMessage, ParsedToolCall, ToolDescriptor};

const DEFAULT_MODEL: &str = "gpt-4o";
const DEFAULT_API_BASE: &str = "https://api.openai.com/v1";
const DEFAULT_TEMPERATURE: f64 = 0.7;
const DEFAULT_MAX_TOKENS: u32 = 4096;
const REQUEST_TIMEOUT_SECS: u64 = 60;
const STREAM_TIMEOUT_SECS: u64 = 600;

// ---------------------------------------------------------------------------
// LlmOpenaiProvider — implements the LlmProvider trait for OpenAI-compatible APIs
// ---------------------------------------------------------------------------

/// OpenAI-compatible LLM provider.
///
/// Handles both streaming (SSE) and non-streaming chat completions.
pub struct LlmOpenaiProvider {
    api_key: String,
    base_url: String,
}

impl LlmOpenaiProvider {
    pub fn new(api_key: String, base_url: String) -> Self {
        Self { api_key, base_url }
    }

    /// Convert internal ChatMessage to OpenAI API JSON format.
    fn message_to_openai(msg: &ChatMessage) -> Value {
        let role = format!("{:?}", msg.role).to_lowercase();
        let mut m = json!({
            "role": role,
            "content": msg.content,
        });
        if let Some(ref id) = msg.tool_call_id {
            m["tool_call_id"] = json!(id);
        }
        if let Some(ref calls) = msg.tool_calls {
            m["tool_calls"] = json!(calls);
        }
        if !msg.reasoning_content.is_empty() {
            m["reasoning_content"] = json!(msg.reasoning_content);
        }
        m
    }

    /// Convert tool descriptors to OpenAI tools format.
    fn tools_to_openai(tools: &[ToolDescriptor]) -> Vec<Value> {
        tools
            .iter()
            .map(|td| {
                json!({
                    "type": "function",
                    "function": {
                        "name": td.name,
                        "description": td.description,
                        "parameters": td.parameters,
                    }
                })
            })
            .collect()
    }

    /// Convert accumulated OpenAI tool call deltas into ParsedToolCall vec.
    fn parse_tool_calls(
        tool_call_acc: HashMap<usize, Value>,
    ) -> Vec<ParsedToolCall> {
        tool_call_acc
            .into_values()
            .filter_map(|tc| {
                let id = tc.get("id")?.as_str()?.to_owned();
                let name = tc.get("function")?.get("name")?.as_str()?.to_owned();
                // Accept arguments as either a JSON string (OpenAI spec) or a
                // pre-parsed JSON object (some non-OpenAI providers / local models).
                let args = match tc.get("function").and_then(|f| f.get("arguments")) {
                    Some(v) if v.is_string() => {
                        let s = v.as_str().unwrap_or("");
                        if s.is_empty() {
                            tracing::warn!(
                                tool_name = %name,
                                "tool call with empty arguments string — defaulting to empty object"
                            );
                        }
                        serde_json::from_str(s)
                            .unwrap_or_else(|e| {
                                tracing::warn!(
                                    tool_name = %name,
                                    error = %e,
                                    "failed to parse tool call arguments JSON — defaulting to empty object"
                                );
                                Value::Object(Default::default())
                            })
                    }
                    Some(v) if v.is_object() => {
                        // Pre-parsed JSON object — use directly.
                        if v.as_object().map(|o| o.is_empty()).unwrap_or(true) {
                            tracing::warn!(
                                tool_name = %name,
                                "tool call with empty arguments object"
                            );
                        }
                        v.clone()
                    }
                    _ => {
                        tracing::warn!(
                            tool_name = %name,
                            "tool call with missing or unexpected arguments type — defaulting to empty object"
                        );
                        Value::Object(Default::default())
                    }
                };
                Some(ParsedToolCall {
                    id,
                    tool_name: name,
                    args,
                })
            })
            .collect()
    }
}

#[async_trait]
impl LlmProvider for LlmOpenaiProvider {
    fn name(&self) -> &str {
        "openai"
    }

    async fn chat_completion(
        &self,
        req: LlmChatRequest,
        cb: Option<Arc<dyn Fn(StreamEvent) + Send + Sync>>,
    ) -> Result<LlmResponse, String> {
        if let Some(cb) = &cb {
            self.streaming_chat_completion(req, Arc::clone(cb)).await
        } else {
            self.non_streaming_chat_completion(req).await
        }
    }
}

impl LlmOpenaiProvider {
    /// Streaming SSE path — emits events via callback.
    async fn streaming_chat_completion(
        &self,
        req: LlmChatRequest,
        cb: Arc<dyn Fn(StreamEvent) + Send + Sync>,
    ) -> Result<LlmResponse, String> {
        let mut request_messages: Vec<Value> = Vec::with_capacity(req.messages.len() + 1);
        request_messages.push(json!({"role": "system", "content": req.system_prompt}));
        for msg in &req.messages {
            request_messages.push(Self::message_to_openai(msg));
        }

        let openai_tools = Self::tools_to_openai(&req.tools);

        let mut request_body = json!({
            "model": req.model,
            "messages": request_messages,
            "stream": true,
            "temperature": DEFAULT_TEMPERATURE,
        });
        if req.max_output_tokens > 0 {
            request_body["max_tokens"] = json!(req.max_output_tokens);
        }
        if !openai_tools.is_empty() {
            request_body["tools"] = json!(openai_tools);
            request_body["tool_choice"] = json!("auto");
        }
        if let Some(ref fmt) = req.response_format
            && fmt == "json_object"
        {
            request_body["response_format"] = json!({"type": "json_object"});
        }

        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(STREAM_TIMEOUT_SECS))
            .build()
            .map_err(|e| format!("build client: {e}"))?;

        let url = format!("{}/chat/completions", self.base_url);

        let response = client
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .header("HTTP-Referer", "https://github.com/13f/aman")
            .header("X-Title", "aman")
            .json(&request_body)
            .send()
            .await
            .map_err(|e| format!("request failed: {e}"))?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(format!("LLM API streaming error HTTP {status}: {body}"));
        }

        let mut stream = response.bytes_stream();
        let mut full_content = String::new();
        let mut reasoning_content = String::new();
        let mut buffer = String::new();
        let mut finish_reason = "stop".to_owned();
        let mut tool_call_acc: HashMap<usize, Value> = HashMap::new();

        cb(StreamEvent::Start);

        while let Some(chunk_result) = stream.next().await {
            let chunk = chunk_result.map_err(|e| format!("stream error: {e}"))?;
            let chunk_str = String::from_utf8_lossy(&chunk);
            buffer.push_str(&chunk_str);

            while let Some(newline_pos) = buffer.find('\n') {
                let line = buffer[..newline_pos].to_string();
                buffer = buffer[newline_pos + 1..].to_string();

                if !line.starts_with("data: ") {
                    continue;
                }
                let data = line[6..].trim();
                if data == "[DONE]" {
                    break;
                }
                let Ok(sse) = serde_json::from_str::<Value>(data) else {
                    continue;
                };
                let Some(choices) = sse.get("choices").and_then(|c| c.as_array()) else {
                    continue;
                };
                let Some(choice) = choices.first() else {
                    continue;
                };

                if let Some(delta) = choice.get("delta") {
                    if let Some(content) = delta.get("content").and_then(|c| c.as_str())
                        && !content.is_empty()
                    {
                        full_content.push_str(content);
                        cb(StreamEvent::Chunk(content.to_owned()));
                    }

                    if let Some(rc) = delta.get("reasoning_content").and_then(|c| c.as_str())
                        && !rc.is_empty()
                    {
                        reasoning_content.push_str(rc);
                    }

                    if let Some(tc_arr) = delta.get("tool_calls").and_then(|t| t.as_array()) {
                        for tc in tc_arr {
                            let idx = tc
                                .get("index")
                                .and_then(|i| i.as_u64())
                                .unwrap_or(0) as usize;
                            let entry = tool_call_acc.entry(idx).or_insert_with(|| {
                                json!({
                                    "id": null,
                                    "type": "function",
                                    "function": {"name": null, "arguments": ""}
                                })
                            });
                            if let Some(id) = tc.get("id").and_then(|i| i.as_str()) {
                                entry["id"] = json!(id);
                            }
                            if let Some(name) = tc
                                .get("function")
                                .and_then(|f| f.get("name"))
                                .and_then(|n| n.as_str())
                            {
                                entry["function"]["name"] = json!(name);
                            }
                            // Arguments may arrive as a string fragment (OpenAI
                                // spec) or as a pre-parsed JSON object (some local
                                // models / non-OpenAI providers). String fragments
                                // are concatenated across deltas; a JSON object
                                // overwrites any prior value (last-write-wins).
                                if let Some(args) = tc
                                    .get("function")
                                    .and_then(|f| f.get("arguments"))
                                {
                                    if let Some(fragment) = args.as_str() {
                                        let current = entry["function"]["arguments"]
                                            .as_str()
                                            .unwrap_or("")
                                            .to_owned();
                                        entry["function"]["arguments"] =
                                            json!(current + fragment);
                                    } else if args.is_object() {
                                        entry["function"]["arguments"] = args.clone();
                                    }
                                }
                        }
                    }
                }

                if let Some(reason) = choice.get("finish_reason").and_then(|r| r.as_str())
                    && !reason.is_empty()
                    && reason != "null"
                {
                    finish_reason = reason.to_owned();
                    cb(StreamEvent::Done {
                        finish_reason: finish_reason.clone(),
                    });
                }
            }
        }

        let tool_calls = Self::parse_tool_calls(tool_call_acc);

        Ok(LlmResponse {
            content: full_content,
            finish_reason,
            tool_calls,
            reasoning_content,
        })
    }

    /// Non-streaming path — returns the complete response.
    async fn non_streaming_chat_completion(
        &self,
        req: LlmChatRequest,
    ) -> Result<LlmResponse, String> {
        let mut request_messages: Vec<Value> = Vec::with_capacity(req.messages.len() + 1);
        request_messages.push(json!({"role": "system", "content": req.system_prompt}));
        for msg in &req.messages {
            request_messages.push(Self::message_to_openai(msg));
        }

        let openai_tools = Self::tools_to_openai(&req.tools);

        let mut body = json!({
            "model": req.model,
            "messages": request_messages,
            "temperature": DEFAULT_TEMPERATURE,
        });
        if req.max_output_tokens > 0 {
            body["max_tokens"] = json!(req.max_output_tokens);
        }
        if !openai_tools.is_empty() {
            body["tools"] = json!(openai_tools);
        }
        if let Some(ref fmt) = req.response_format
            && fmt == "json_object"
        {
            body["response_format"] = json!({"type": "json_object"});
        }

        let url = format!("{}/chat/completions", self.base_url);
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(REQUEST_TIMEOUT_SECS))
            .no_proxy()
            .build()
            .map_err(|e| format!("build client: {e}"))?;

        let response = client
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .header("HTTP-Referer", "https://github.com/13f/aman")
            .header("X-Title", "aman")
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("request failed: {e}"))?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            return Err(format!("LLM API error HTTP {status}: {text}"));
        }

        let response_text = response.text().await.map_err(|e| format!("read: {e}"))?;
        let response_body: Value = serde_json::from_str(&response_text).map_err(|e| {
            format!(
                "parse: {e} — raw body (first 500 chars): {}",
                &response_text[..response_text.len().min(500)]
            )
        })?;

        let choice = &response_body["choices"][0];
        let message = &choice["message"];
        let finish_reason = choice["finish_reason"]
            .as_str()
            .unwrap_or("stop")
            .to_owned();

        let content = message
            .get("content")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();

        let reasoning_content = message
            .get("reasoning_content")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();

        let tool_calls: Vec<ParsedToolCall> = message
            .get("tool_calls")
            .and_then(Value::as_array)
            .map(|arr| {
                arr.iter()
                    .filter_map(|tc| {
                        let id = tc.get("id")?.as_str()?.to_owned();
                        let name = tc["function"]["name"].as_str()?.to_owned();
                        // Accept arguments as either a JSON string (OpenAI spec) or a
                        // pre-parsed JSON object (some non-OpenAI providers / local models).
                        let args = match tc["function"].get("arguments") {
                            Some(v) if v.is_string() => {
                                let s = v.as_str().unwrap_or("");
                                if s.is_empty() {
                                    tracing::warn!(
                                        tool_name = %name,
                                        "non-streaming tool call with empty arguments string — defaulting to empty object"
                                    );
                                }
                                serde_json::from_str(s)
                                    .unwrap_or_else(|e| {
                                        tracing::warn!(
                                            tool_name = %name,
                                            error = %e,
                                            "non-streaming: failed to parse tool call arguments JSON — defaulting to empty object"
                                        );
                                        Value::Object(Default::default())
                                    })
                            }
                            Some(v) if v.is_object() => {
                                if v.as_object().map(|o| o.is_empty()).unwrap_or(true) {
                                    tracing::warn!(
                                        tool_name = %name,
                                        "non-streaming tool call with empty arguments object"
                                    );
                                }
                                v.clone()
                            }
                            _ => {
                                tracing::warn!(
                                    tool_name = %name,
                                    "non-streaming tool call with missing or unexpected arguments type — defaulting to empty object"
                                );
                                Value::Object(Default::default())
                            }
                        };
                        Some(ParsedToolCall {
                            id,
                            tool_name: name,
                            args,
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();

        Ok(LlmResponse {
            content,
            finish_reason,
            tool_calls,
            reasoning_content,
        })
    }
}

// ---------------------------------------------------------------------------
// LlmOpenaiTool — wraps the OpenAI API as a Tool for health check / fallback
// ---------------------------------------------------------------------------

/// Tool that calls the OpenAI Chat Completion API.
pub struct LlmOpenaiTool;

#[async_trait]
impl Tool for LlmOpenaiTool {
    fn name(&self) -> &str {
        "llm_openai"
    }

    fn mode(&self) -> ToolMode {
        ToolMode::Local
    }

    fn parameters(&self) -> &JsonSchema {
        static PARAMS: LazyLock<JsonSchema> = LazyLock::new(|| {
            JsonSchema::from(json!({
                "type": "object",
                "required": ["messages"],
                "properties": {
                    "messages": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "required": ["role", "content"],
                            "properties": {
                                "role": {
                                    "type": "string",
                                    "enum": ["system", "user", "assistant"]
                                },
                                "content": {"type": "string"}
                            }
                        }
                    },
                    "model": {
                        "type": "string",
                        "default": DEFAULT_MODEL
                    },
                    "temperature": {
                        "type": "number",
                        "default": DEFAULT_TEMPERATURE
                    },
                    "max_tokens": {
                        "type": "integer",
                        "default": DEFAULT_MAX_TOKENS
                    },
                    "api_key": {"type": "string"},
                    "api_base": {
                        "type": "string",
                        "default": DEFAULT_API_BASE
                    },
                    "tools": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "required": ["name", "description"],
                            "properties": {
                                "name": {"type": "string"},
                                "description": {"type": "string"},
                                "parameters": {"type": "object"}
                            }
                        }
                    }
                }
            }))
        });
        &PARAMS
    }

    fn returns(&self) -> &JsonSchema {
        static RETURNS: LazyLock<JsonSchema> = LazyLock::new(|| {
            JsonSchema::from(json!({
                "type": "object",
                "properties": {
                    "content": {"type": "string"},
                    "finish_reason": {"type": "string"},
                    "tool_calls": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "name": {"type": "string"},
                                "arguments": {"type": "string"}
                            }
                        }
                    },
                    "usage": {
                        "type": "object",
                        "properties": {
                            "prompt_tokens": {"type": "integer"},
                            "completion_tokens": {"type": "integer"},
                            "total_tokens": {"type": "integer"}
                        }
                    },
                    "error": {"type": "string"},
                    "error_type": {"type": "string"},
                    "status_code": {"type": "integer"}
                }
            }))
        });
        &RETURNS
    }

    async fn execute(&self, params: Value, _ctx: ToolContext) -> AmanResult<Value> {
        let messages = params.get("messages").ok_or_else(|| Error::ConfigInvalid {
            message: "messages field is required".to_owned(),
        })?;

        let model = params
            .get("model")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .unwrap_or(DEFAULT_MODEL);

        let api_key = match params.get("api_key").and_then(Value::as_str) {
            Some(key) if !key.is_empty() => key.to_owned(),
            _ => std::env::var("OPENAI_API_KEY").map_err(|_| Error::ConfigInvalid {
                message: "api_key parameter or OPENAI_API_KEY env var is required".to_owned(),
            })?,
        };

        let api_base = params
            .get("api_base")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .unwrap_or(DEFAULT_API_BASE)
            .trim_end_matches('/')
            .to_owned();

        let temperature = params
            .get("temperature")
            .and_then(Value::as_f64)
            .unwrap_or(DEFAULT_TEMPERATURE);

        let max_tokens = params
            .get("max_tokens")
            .and_then(Value::as_u64)
            .map(|v| v.min(1_000_000) as u32)
            .unwrap_or(DEFAULT_MAX_TOKENS);

        let tools = params.get("tools");

        let mut body = json!({
            "model": model,
            "messages": messages,
            "temperature": temperature,
            "max_tokens": max_tokens,
        });

        if let Some(tools_val) = tools
            && !tools_val.is_null()
            && tools_val.as_array().is_some_and(|a| !a.is_empty())
        {
            body["tools"] = format_tools_for_openai(tools_val);
        }

        let url = format!("{api_base}/chat/completions");
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(REQUEST_TIMEOUT_SECS))
            .no_proxy()
            .build()
            .map_err(|e| Error::Unrecoverable {
                message: format!("failed to build HTTP client: {e}"),
            })?;

        let response = match client
            .post(&url)
            .header("Authorization", format!("Bearer {api_key}"))
            .header("Content-Type", "application/json")
            .header("HTTP-Referer", "https://github.com/13f/aman")
            .header("X-Title", "aman")
            .json(&body)
            .send()
            .await
        {
            Ok(resp) => resp,
            Err(e) => {
                let error_msg = e.to_string();
                return if e.is_timeout() {
                    Ok(json!({
                        "content": "",
                        "finish_reason": "error",
                        "error": format!("OpenAI request timed out after {REQUEST_TIMEOUT_SECS}s: {error_msg}"),
                        "error_type": "timeout",
                        "status_code": 0
                    }))
                } else if e.is_connect() {
                    Ok(json!({
                        "content": "",
                        "finish_reason": "error",
                        "error": format!("connection failed to {url}: {error_msg}"),
                        "error_type": "connection_error",
                        "status_code": 0
                    }))
                } else {
                    Ok(json!({
                        "content": "",
                        "finish_reason": "error",
                        "error": format!("OpenAI API request failed: {error_msg}"),
                        "error_type": "request_failed",
                        "status_code": 0
                    }))
                };
            }
        };

        let status = response.status().as_u16();
        let raw_text = match response.text().await {
            Ok(t) => t,
            Err(e) => {
                return Ok(serde_json::json!({
                    "error": format!("failed to read response: {e}"),
                    "error_type": "parse_failed",
                    "status_code": status
                }))
            }
        };
        let response_body: Value = match serde_json::from_str(&raw_text) {
            Ok(body) => body,
            Err(e) => {
                return Ok(json!({
                    "content": "",
                    "finish_reason": "error",
                    "error": format!("failed to parse OpenAI response: {e}"),
                    "error_type": "parse_error",
                    "status_code": status,
                }));
            }
        };

        if !(200..=299).contains(&status) {
            let error_msg = response_body
                .get("error")
                .and_then(|e| e.get("message"))
                .and_then(Value::as_str)
                .unwrap_or("unknown API error");

            let error_type = response_body
                .get("error")
                .and_then(|e| e.get("type"))
                .and_then(Value::as_str)
                .unwrap_or("api_error");

            let is_rate_limit = status == 429 || error_type == "rate_limit_exceeded";
            let retry_after = if is_rate_limit {
                response_body
                    .get("error")
                    .and_then(|e| e.get("retry_after_seconds"))
                    .or_else(|| response_body.get("retry_after_seconds"))
                    .and_then(Value::as_u64)
                    .unwrap_or(0)
            } else {
                0
            };

            let mut result = json!({
                "content": "",
                "finish_reason": "error",
                "error": error_msg,
                "error_type": error_type,
                "status_code": status,
            });
            if retry_after > 0 {
                result["retry_after_seconds"] = json!(retry_after);
            }
            return Ok(result);
        }

        let choice = &response_body["choices"][0];
        let message = &choice["message"];
        let finish_reason = choice["finish_reason"]
            .as_str()
            .unwrap_or("stop")
            .to_owned();

        let content = message
            .get("content")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();

        let tool_calls: Vec<Value> = message
            .get("tool_calls")
            .and_then(Value::as_array)
            .map(|arr| {
                arr.iter()
                    .map(|tc| {
                        json!({
                            "name": tc["function"]["name"].as_str().unwrap_or(""),
                            "arguments": tc["function"]["arguments"].as_str().unwrap_or("{}")
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();

        let usage = response_body.get("usage").cloned().unwrap_or_default();

        Ok(json!({
            "content": content,
            "finish_reason": finish_reason,
            "tool_calls": tool_calls,
            "usage": usage,
            "status_code": status,
        }))
    }
}

/// Convert simplified tool definitions to OpenAI tool format.
fn format_tools_for_openai(tools: &Value) -> Value {
    let Some(arr) = tools.as_array() else {
        return tools.clone();
    };
    Value::Array(
        arr.iter()
            .map(|t| {
                json!({
                    "type": "function",
                    "function": {
                        "name": t["name"],
                        "description": t.get("description").and_then(Value::as_str).unwrap_or(""),
                        "parameters": t.get("parameters").cloned().unwrap_or(json!({"type": "object"}))
                    }
                })
            })
            .collect(),
    )
}
