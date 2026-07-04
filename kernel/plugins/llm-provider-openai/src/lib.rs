#![forbid(unsafe_code)]
#![doc = "OpenAI LLM Provider Tool — wraps OpenAI Chat Completion API as a Tool and LlmProvider trait."]

use async_trait::async_trait;
use futures_util::StreamExt;
use kernel::context::ToolContext;
use kernel::llm::{LlmChatRequest, LlmProvider, LlmResponse, ResponseFormat, StreamEvent};
use kernel::react::{ChatMessage, ParsedToolCall, ToolDescriptor};
use kernel::schema::JsonSchema;
use kernel::tool::Tool;
use kernel::types::ToolMode;
use kernel::{AmanResult, Error};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::LazyLock;

const DEFAULT_MODEL: &str = "gpt-4o";
const DEFAULT_API_BASE: &str = "https://api.openai.com/v1";
const DEFAULT_TEMPERATURE: f64 = 0.7;
const DEFAULT_MAX_TOKENS: u32 = 4096;
const REQUEST_TIMEOUT_SECS: u64 = 180;
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
                let args_str = tc
                    .get("function")?
                    .get("arguments")?
                    .as_str()?
                    .to_owned();
                let args: Value =
                    serde_json::from_str(&args_str).unwrap_or(Value::Object(Default::default()));
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

    fn base_url(&self) -> &str {
        &self.base_url
    }

    async fn chat_completion(
        &self,
        req: LlmChatRequest,
        cb: Option<Arc<dyn Fn(StreamEvent) + Send + Sync>>,
    ) -> Result<LlmResponse, Error> {
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
    ) -> Result<LlmResponse, Error> {
        // Build system + conversation messages
        let mut request_messages: Vec<Value> = Vec::with_capacity(req.messages.len() + 1);
        request_messages.push(json!({"role": "system", "content": req.system_prompt}));
        for msg in &req.messages {
            request_messages.push(Self::message_to_openai(msg));
        }

        // Convert tools
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
        if let Some(ref fmt) = req.response_format {
            match fmt {
                ResponseFormat::JsonObject => {
                    request_body["response_format"] = json!({"type": "json_object"});
                }
                ResponseFormat::JsonSchema { .. } => {
                    // Use json_object for universal provider compatibility
                    request_body["response_format"] = json!({"type": "json_object"});
                }
            }
        }

        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(STREAM_TIMEOUT_SECS))
            .no_proxy()
            .build()
            .map_err(|e| {
                Error::Io(std::io::Error::other(e.to_string()))
            })?;

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
            .map_err(|e| {
                Error::Io(std::io::Error::other(e.to_string()))
            })?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(Error::ConfigInvalid {
                message: format!("LLM API streaming error HTTP {status}: {body}"),
            });
        }

        let mut stream = response.bytes_stream();
        let mut full_content = String::new();
        let mut reasoning_content = String::new();
        let mut buffer = String::new();
        let mut finish_reason = "stop".to_owned();
        let mut tool_call_acc: HashMap<usize, Value> = HashMap::new();

        cb(StreamEvent::Start);

        while let Some(chunk_result) = stream.next().await {
            let chunk = chunk_result.map_err(|e| {
                Error::Io(std::io::Error::other(e.to_string()))
            })?;
            let chunk_str = String::from_utf8_lossy(&chunk);
            buffer.push_str(&chunk_str);

            // Process complete lines in buffer
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

                // Extract delta content (text)
                if let Some(delta) = choice.get("delta") {
                    if let Some(content) = delta.get("content").and_then(|c| c.as_str())
                        && !content.is_empty()
                    {
                        full_content.push_str(content);
                        cb(StreamEvent::Chunk(content.to_owned()));
                    }

                    // Capture reasoning_content (DeepSeek thinking mode)
                    if let Some(rc) = delta.get("reasoning_content").and_then(|c| c.as_str())
                        && !rc.is_empty()
                    {
                        reasoning_content.push_str(rc);
                    }

                    // Accumulate tool call deltas
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
                            if let Some(args) = tc
                                .get("function")
                                .and_then(|f| f.get("arguments"))
                                .and_then(|a| a.as_str())
                            {
                                let current = entry["function"]["arguments"]
                                    .as_str()
                                    .unwrap_or("")
                                    .to_owned();
                                entry["function"]["arguments"] = json!(current + args);
                            }
                        }
                    }
                }

                // Check finish_reason
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
    ) -> Result<LlmResponse, Error> {
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
        if let Some(ref fmt) = req.response_format {
            match fmt {
                ResponseFormat::JsonObject => {
                    body["response_format"] = json!({"type": "json_object"});
                }
                ResponseFormat::JsonSchema { .. } => {
                    // Use json_object for universal provider compatibility
                    body["response_format"] = json!({"type": "json_object"});
                }
            }
        }

        let url = format!("{}/chat/completions", self.base_url);
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(REQUEST_TIMEOUT_SECS))
            .no_proxy()
            .build()
            .map_err(|e| Error::Unrecoverable {
                message: format!("failed to build HTTP client: {e}"),
            })?;

        let mut last_error = String::new();
        for attempt in 0..3 {
            if attempt > 0 {
                tokio::time::sleep(std::time::Duration::from_secs(1 << attempt)).await;
            }

            let response = match client
                .post(&url)
                .header("Authorization", format!("Bearer {}", self.api_key))
                .header("Content-Type", "application/json")
                .header("HTTP-Referer", "https://github.com/13f/aman")
                .header("X-Title", "aman")
                .json(&body)
                .send()
                .await
            {
                Ok(r) => r,
                Err(e) => {
                    last_error = format!("LLM API request failed: {e}");
                    continue;
                }
            };

            if !response.status().is_success() {
                let status = response.status();
                if status.is_client_error() {
                    // 4xx — don't retry, these are permanent errors
                    let text = response.text().await.unwrap_or_default();
                    return Err(Error::ConfigInvalid {
                        message: format!("LLM API error HTTP {status}: {text}"),
                    });
                }
                // 5xx — retryable
                last_error = format!("LLM API error HTTP {status}");
                continue;
            }

            let response_text = match response.text().await {
                Ok(t) => t,
                Err(e) => {
                    last_error = format!("failed to read LLM response body: {e}");
                    continue;
                }
            };

            match serde_json::from_str::<Value>(&response_text) {
                Ok(v) => {
                    let choice = &v["choices"][0];
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
                                    let args_str = tc["function"]["arguments"].as_str()?.to_owned();
                                    let args: Value = serde_json::from_str(&args_str)
                                        .unwrap_or(Value::Object(Default::default()));
                                    Some(ParsedToolCall { id, tool_name: name, args })
                                })
                                .collect()
                        })
                        .unwrap_or_default();
                    return Ok(LlmResponse {
                        content,
                        finish_reason,
                        tool_calls,
                        reasoning_content,
                    });
                }
                Err(e) => {
                    last_error = format!(
                        "failed to parse LLM response: {e} — raw body (first 500 chars): {}",
                        &response_text[..response_text.len().min(500)]
                    );
                    // Only retry parse errors if body looks incomplete/truncated
                    if !response_text.trim().starts_with('{') {
                        continue;
                    }
                    return Err(Error::ConfigInvalid { message: last_error });
                }
            }
        }

        Err(Error::Unrecoverable {
            message: format!("llm_chat failed after 3 attempts: {last_error}"),
        })
    }
}

// ---------------------------------------------------------------------------
// LlmOpenaiTool — wraps the OpenAI API as a Tool for health check / fallback
// ---------------------------------------------------------------------------

/// Tool that calls the OpenAI Chat Completion API.
///
/// Accepts conversation messages, model config, and optional tool definitions.
/// Returns the assistant response including content, tool_calls, and usage.
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

        // Build request body.
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
        let response_body: Value = match response.json().await {
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

            // Rate limit detection.
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

        // Parse successful response.
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
///
/// Input (simple): `[{ name, description, parameters }]`
/// Output (OpenAI): `[{ type: "function", function: { name, description, parameters } }]`
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

#[cfg(test)]
mod tests {
    use super::*;
    use kernel::context::BaseContext;
    use kernel::types::TraceId;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;

    fn tool_context() -> ToolContext {
        ToolContext {
            base: BaseContext::new(TraceId::new()),
            tool_name: Some("llm_openai".to_owned()),
            working_directory: None,
        }
    }

    /// Start a mock OpenAI HTTP server and return its address.
    /// The server echoes back the assistant reply from the request content.
    fn start_mock_server(
        response_json: &'static str,
    ) -> (std::net::SocketAddr, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock server");
        let addr = listener.local_addr().expect("local addr");
        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            let mut buf = [0_u8; 8192];
            let _n = stream.read(&mut buf).expect("read request");

            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                response_json.len(),
                response_json
            );
            stream
                .write_all(response.as_bytes())
                .expect("write response");
        });
        (addr, handle)
    }

    fn start_error_server(
        status: u16,
        response_json: &'static str,
    ) -> (std::net::SocketAddr, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind error server");
        let addr = listener.local_addr().expect("local addr");
        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            let mut buf = [0_u8; 8192];
            let _n = stream.read(&mut buf).expect("read request");

            let response = format!(
                "HTTP/1.1 {} {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                status,
                if status == 401 { "Unauthorized" } else if status == 429 { "Too Many Requests" } else { "Error" },
                response_json.len(),
                response_json
            );
            stream
                .write_all(response.as_bytes())
                .expect("write response");
        });
        (addr, handle)
    }

    #[tokio::test]
    async fn returns_assistant_reply() {
        let response = r#"{
            "id": "chatcmpl-abc123",
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": "Hello! How can I help you today?"
                },
                "finish_reason": "stop"
            }],
            "usage": {
                "prompt_tokens": 12,
                "completion_tokens": 8,
                "total_tokens": 20
            }
        }"#;

        let (addr, server) = start_mock_server(response);
        let tool = LlmOpenaiTool;
        let params = json!({
            "messages": [{"role": "user", "content": "Hello"}],
            "api_key": "test-key",
            "api_base": format!("http://{addr}"),
            "model": "gpt-4o",
        });

        let result = tool.execute(params, tool_context()).await.unwrap();
        assert_eq!(
            result["content"].as_str(),
            Some("Hello! How can I help you today?"),
        );
        assert_eq!(result["finish_reason"].as_str(), Some("stop"));
        assert_eq!(result["usage"]["total_tokens"].as_u64(), Some(20));
        assert_eq!(result["status_code"].as_u64(), Some(200));
        server.join().unwrap();
    }

    #[tokio::test]
    async fn returns_error_on_invalid_api_key() {
        let error_response = r#"{
            "error": {
                "message": "Incorrect API key provided",
                "type": "authentication_error",
                "code": "invalid_api_key"
            }
        }"#;

        let (addr, server) = start_error_server(401, error_response);
        let tool = LlmOpenaiTool;
        let params = json!({
            "messages": [{"role": "user", "content": "Hello"}],
            "api_key": "bad-key",
            "api_base": format!("http://{addr}"),
        });

        let result = tool.execute(params, tool_context()).await.unwrap();
        assert_eq!(result["finish_reason"].as_str(), Some("error"));
        assert!(result["error"].as_str().unwrap_or("").contains("Incorrect API key"));
        assert_eq!(result["error_type"].as_str(), Some("authentication_error"));
        assert_eq!(result["status_code"].as_u64(), Some(401));
        server.join().unwrap();
    }

    #[tokio::test]
    async fn returns_error_on_rate_limit() {
        let error_response = r#"{
            "error": {
                "message": "Rate limit exceeded",
                "type": "rate_limit_exceeded",
                "code": "rate_limited",
                "retry_after_seconds": 30
            }
        }"#;

        let (addr, server) = start_error_server(429, error_response);
        let tool = LlmOpenaiTool;
        let params = json!({
            "messages": [{"role": "user", "content": "Hello"}],
            "api_key": "key",
            "api_base": format!("http://{addr}"),
        });

        let result = tool.execute(params, tool_context()).await.unwrap();
        assert_eq!(result["finish_reason"].as_str(), Some("error"));
        assert_eq!(result["error_type"].as_str(), Some("rate_limit_exceeded"));
        assert_eq!(result["retry_after_seconds"].as_u64(), Some(30));
        assert_eq!(result["status_code"].as_u64(), Some(429));
        server.join().unwrap();
    }

    #[tokio::test]
    async fn requires_messages_field() {
        let tool = LlmOpenaiTool;
        let params = json!({
            "api_key": "test-key",
        });

        let err = tool
            .execute(params, tool_context())
            .await
            .expect_err("missing messages");
        assert!(matches!(err, Error::ConfigInvalid { .. }));
        assert!(err.to_string().contains("messages"));
    }

    #[tokio::test]
    async fn requires_api_key_or_env() {
        let tool = LlmOpenaiTool;
        let params = json!({
            "messages": [{"role": "user", "content": "hi"}],
            "api_base": "http://localhost:9999",
        });

        // No api_key and no env var should fail.
        let err = tool
            .execute(params, tool_context())
            .await
            .expect_err("missing api_key");
        assert!(matches!(err, Error::ConfigInvalid { .. }));
        assert!(err.to_string().contains("api_key"));
    }

    #[tokio::test]
    async fn handles_connection_refused() {
        let tool = LlmOpenaiTool;
        let params = json!({
            "messages": [{"role": "user", "content": "hi"}],
            "api_key": "test-key",
            "api_base": "http://127.0.0.1:19199", // hopefully nothing listening
        });

        let result = tool.execute(params, tool_context()).await.unwrap();
        assert_eq!(result["finish_reason"].as_str(), Some("error"));
        assert_eq!(result["error_type"].as_str(), Some("connection_error"));
        assert_eq!(result["status_code"].as_u64(), Some(0));
    }

    #[tokio::test]
    async fn formats_tools_correctly() {
        let response = r#"{
            "id": "chatcmpl-tc",
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": "Let me check the weather.",
                    "tool_calls": [{
                        "id": "call_1",
                        "type": "function",
                        "function": {
                            "name": "get_weather",
                            "arguments": "{\"city\": \"London\"}"
                        }
                    }]
                },
                "finish_reason": "tool_calls"
            }],
            "usage": {
                "prompt_tokens": 30,
                "completion_tokens": 15,
                "total_tokens": 45
            }
        }"#;

        let (addr, server) = start_mock_server(response);
        let tool = LlmOpenaiTool;
        let params = json!({
            "messages": [{"role": "user", "content": "What's the weather in London?"}],
            "api_key": "test-key",
            "api_base": format!("http://{addr}"),
            "tools": [
                {
                    "name": "get_weather",
                    "description": "Get weather for a city",
                    "parameters": {"type": "object"}
                }
            ]
        });

        let result = tool.execute(params, tool_context()).await.unwrap();
        assert_eq!(result["finish_reason"].as_str(), Some("tool_calls"));
        let calls = result["tool_calls"].as_array().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0]["name"].as_str(), Some("get_weather"));
        assert!(calls[0]["arguments"].as_str().unwrap().contains("London"));
        assert_eq!(result["usage"]["total_tokens"].as_u64(), Some(45));
        server.join().unwrap();
    }

    #[tokio::test]
    async fn metadata_is_correct() {
        let tool = LlmOpenaiTool;
        assert_eq!(tool.name(), "llm_openai");
        assert_eq!(tool.mode(), ToolMode::Local);
    }

    #[tokio::test]
    async fn provider_non_streaming() {
        let response = r#"{
            "id": "chatcmpl-p1",
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": "Hello from provider!"
                },
                "finish_reason": "stop"
            }],
            "usage": {"prompt_tokens": 5, "completion_tokens": 3, "total_tokens": 8}
        }"#;

        let (addr, _server) = start_mock_server(response);
        let provider = LlmOpenaiProvider::new(
            "test-key".into(),
            format!("http://{addr}"),
        );

        let req = LlmChatRequest {
            model: "gpt-4o".into(),
            system_prompt: "You are a helpful assistant.".into(),
            messages: vec![ChatMessage::user("Hello")],
            tools: vec![],
            max_output_tokens: 0,
            response_format: None,
        };

        let result = provider.chat_completion(req, None).await.unwrap();
        assert_eq!(result.content, "Hello from provider!");
        assert_eq!(result.finish_reason, "stop");
    }

    #[tokio::test]
    async fn provider_streaming() {
        // SSE response simulating a streaming completion
        let sse_body = "data: {\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\"},\"finish_reason\":null}]}\n\
                        data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Hello\"},\"finish_reason\":null}]}\n\
                        data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\" world\"},\"finish_reason\":null}]}\n\
                        data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\
                        data: [DONE]\n";

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("local addr");
        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            let mut buf = [0_u8; 8192];
            let _n = stream.read(&mut buf).expect("read");
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                sse_body.len(),
                sse_body
            );
            stream.write_all(response.as_bytes()).expect("write");
        });

        let provider = LlmOpenaiProvider::new("test-key".into(), format!("http://{addr}"));

        let received = std::sync::Mutex::new(Vec::new());
        let cb: Arc<dyn Fn(StreamEvent) + Send + Sync> = Arc::new(move |evt| {
            received.lock().unwrap().push(evt);
        });

        let req = LlmChatRequest {
            model: "gpt-4o".into(),
            system_prompt: "Be helpful.".into(),
            messages: vec![ChatMessage::user("Hi")],
            tools: vec![],
            max_output_tokens: 0,
            response_format: None,
        };

        let result = provider.chat_completion(req, Some(cb)).await.unwrap();
        assert_eq!(result.content, "Hello world");
        assert_eq!(result.finish_reason, "stop");
        handle.join().unwrap();
    }
}
