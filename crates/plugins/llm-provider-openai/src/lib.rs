#![forbid(unsafe_code)]
#![doc = "OpenAI LLM Provider Tool — wraps OpenAI Chat Completion API as a Tool."]

use async_trait::async_trait;
use kernel::context::ToolContext;
use kernel::schema::JsonSchema;
use kernel::tool::Tool;
use kernel::types::ToolMode;
use kernel::{AmanResult, Error};
use serde_json::{json, Value};
use std::sync::LazyLock;

const DEFAULT_MODEL: &str = "gpt-4o";
const DEFAULT_API_BASE: &str = "https://api.openai.com/v1";
const DEFAULT_TEMPERATURE: f64 = 0.7;
const DEFAULT_MAX_TOKENS: u32 = 4096;
const REQUEST_TIMEOUT_SECS: u64 = 60;

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
}
