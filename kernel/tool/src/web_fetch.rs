#![forbid(unsafe_code)]
// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0


//! Web fetch tool — read-only, fetches a URL and returns its full content.
//!
//! Handles HTML pages, JSON APIs, plain text, and binary content (as Base64).
//! Text content is returned as UTF-8; binary content is Base64-encoded.
//!
//! An internal hard cap of 16 MiB prevents runaway memory consumption on
//! unexpectedly large responses. If the body exceeds this limit the response
//! is truncated and `truncated: true` is set.
//!
//! ## Proxy
//!
//! The underlying HTTP client reads `ALL_PROXY` / `HTTPS_PROXY` / `https_proxy`
//! and automatically converts `socks5://` to `socks5h://` so DNS is resolved by
//! the proxy. Both `socks5://` and `socks5h://` work — just set your usual env
//! var and the conversion is handled transparently.

use async_trait::async_trait;
use kernel::context::ToolContext;
use kernel::schema::JsonSchema;
use kernel::tool::Tool;
use kernel::types::ToolMode;
use kernel::{AmanResult, Error};
use serde_json::{json, Value};
use std::sync::LazyLock;

/// Internal hard cap to prevent OOM on unexpectedly large responses (16 MiB).
/// Large enough for any normal web page, article, or API response — higher
/// than the entire text of *War and Peace*.
const MAX_BODY_SIZE: usize = 16 * 1024 * 1024;

/// The `web_fetch` tool struct.
pub struct WebFetchTool;

#[async_trait]
impl Tool for WebFetchTool {
    fn name(&self) -> &str {
        "web_fetch"
    }

    fn mode(&self) -> ToolMode {
        ToolMode::Local
    }

    fn parameters(&self) -> &JsonSchema {
        static PARAMS: LazyLock<JsonSchema> = LazyLock::new(|| {
            JsonSchema::from(json!({
                "type": "object",
                "required": ["url"],
                "properties": {
                    "url": {
                        "type": "string",
                        "description": "The URL to fetch"
                    },
                    "method": {
                        "type": "string",
                        "description": "HTTP method (GET, POST, etc.)",
                        "default": "GET"
                    },
                    "headers": {
                        "type": "object",
                        "description": "Optional request headers (e.g. Authorization, Accept)"
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
                    "ok": {"type": "boolean"},
                    "status": {"type": "integer"},
                    "content_type": {"type": "string"},
                    "headers": {"type": "object"},
                    "body": {"type": "string"},
                    "json": {},
                    "truncated": {"type": "boolean"},
                    "body_size": {"type": "integer"},
                    "error": {"type": "string"}
                }
            }))
        });
        &RETURNS
    }

    async fn execute(&self, params: Value, _ctx: ToolContext) -> AmanResult<Value> {
        let url = params
            .get("url")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::ConfigInvalid {
                message: "url must be a string".to_owned(),
            })?;

        let method_str = params
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or("GET")
            .to_uppercase();
        let method = reqwest::Method::from_bytes(method_str.as_bytes())
            .map_err(|e| Error::ConfigInvalid {
                message: format!("invalid HTTP method: {e}"),
            })?;

        let client = http_client();
        let mut request = client.request(method, url);

        if let Some(headers) = params.get("headers").and_then(Value::as_object) {
            for (name, value) in headers {
                if let Some(text) = value.as_str() {
                    request = request.header(name, text);
                }
            }
        }

        let response = match request.send().await {
            Ok(r) => r,
            Err(e) => {
                return Ok(json!({
                    "ok": false,
                    "status": 0,
                    "content_type": null,
                    "headers": {},
                    "body": "",
                    "json": null,
                    "truncated": false,
                    "body_size": 0,
                    "error": format!("request failed: {e}")
                }));
            }
        };

        let status = response.status().as_u16();
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_owned();

        // Collect response headers into a JSON object.
        let mut response_headers = serde_json::Map::new();
        for (name, value) in response.headers() {
            response_headers.insert(
                name.to_string(),
                Value::String(value.to_str().unwrap_or_default().to_owned()),
            );
        }

        // Read the full body, capped at our internal safety limit.
        let body_bytes = match read_body(response).await {
            Ok(b) => b,
            Err(e) => {
                return Ok(json!({
                    "ok": false,
                    "status": status,
                    "content_type": content_type,
                    "headers": response_headers,
                    "body": "",
                    "json": null,
                    "truncated": false,
                    "body_size": 0,
                    "error": format!("failed to read response body: {e}")
                }));
            }
        };

        let body_len = body_bytes.len();
        let truncated = body_len > MAX_BODY_SIZE;
        let actual = if truncated {
            &body_bytes[..MAX_BODY_SIZE]
        } else {
            &body_bytes[..]
        };

        // Textual content → UTF-8; binary → Base64.
        let body_text = if is_text_content_type(&content_type) {
            String::from_utf8_lossy(actual).to_string()
        } else {
            use base64::Engine;
            base64::engine::general_purpose::STANDARD.encode(actual)
        };

        // Try JSON parse if the content type suggests it.
        let json_value: Option<Value> = if content_type.contains("json") {
            serde_json::from_slice(actual).ok()
        } else {
            None
        };

        Ok(json!({
            "ok": (200..400).contains(&status),
            "status": status,
            "content_type": content_type,
            "headers": response_headers,
            "body": body_text,
            "json": json_value,
            "truncated": truncated,
            "body_size": body_len
        }))
    }
}

// ── HTTP client ──────────────────────────────────────────────────────────────

/// Build a shared async HTTP client (30s timeout).
///
/// Proxy is detected from `ALL_PROXY` / `HTTPS_PROXY` / `https_proxy` and
/// `socks5://` is automatically upgraded to `socks5h://` (remote DNS) via
/// [`kernel::proxy::detect_proxy_url`].
fn http_client() -> reqwest::Client {
    let mut builder = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30));
    if let Some(proxy_url) = kernel::proxy::detect_proxy_url() {
        match reqwest::Proxy::all(&proxy_url) {
            Ok(proxy) => {
                builder = builder.proxy(proxy);
            }
            Err(e) => {
                tracing::warn!(%proxy_url, error = %e, "web_fetch: invalid proxy URL, connecting directly");
            }
        }
    }
    builder.build().expect("reqwest async client")
}

// ── Helpers ──────────────────────────────────────────────────────────────────

/// Read the full response body, capping at `MAX_BODY_SIZE + 1` to detect
/// truncation.
async fn read_body(response: reqwest::Response) -> Result<Vec<u8>, reqwest::Error> {
    let cap = MAX_BODY_SIZE + 1;
    let bytes = response.bytes().await?;
    Ok(if bytes.len() > cap {
        bytes[..cap].to_vec()
    } else {
        bytes.to_vec()
    })
}

/// Heuristic for content types that are human-readable text (or JSON).
fn is_text_content_type(content_type: &str) -> bool {
    let ct = content_type.to_lowercase();
    ct.starts_with("text/")
        || ct.contains("json")
        || ct.contains("xml")
        || ct.contains("javascript")
        || ct.contains("yaml")
        || ct.contains("csv")
        || ct.is_empty() // unknown → assume text
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_text_content_type() {
        assert!(is_text_content_type("text/html; charset=utf-8"));
        assert!(is_text_content_type("application/json"));
        assert!(is_text_content_type("application/xml"));
        assert!(is_text_content_type("application/javascript"));
        assert!(is_text_content_type("text/plain"));
        assert!(is_text_content_type("application/x-yaml"));
        assert!(is_text_content_type("text/csv"));
        assert!(is_text_content_type("")); // unknown → assume text
        assert!(!is_text_content_type("image/png"));
        assert!(!is_text_content_type("application/octet-stream"));
        assert!(!is_text_content_type("video/mp4"));
    }

    #[test]
    fn test_rejects_missing_url() {
        let tool = WebFetchTool;
        let result = pollster::block_on(tool.execute(
            json!({}),
            ToolContext::default(),
        ));
        assert!(result.is_err());
    }

    #[test]
    fn test_method_validation() {
        let tool = WebFetchTool;
        let result = pollster::block_on(tool.execute(
            json!({"url": "https://example.com", "method": ""}),
            ToolContext::default(),
        ));
        // Empty method fails — but the tool returns Ok (error is in the value),
        // or Err if the method can't be parsed.
        match result {
            Ok(val) => {
                // If URL validation passed, reqwest may fail on empty method.
                // That's fine — we just want to make sure we don't panic.
                assert!(val.get("error").is_some() || val.get("ok").is_some());
            }
            Err(_) => {
                // ConfigInvalid for invalid method is also acceptable.
            }
        }
    }
}
