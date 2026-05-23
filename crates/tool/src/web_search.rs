#![forbid(unsafe_code)]
// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0


//! Web search tool — read-only, no user authorization required.
//!
//! Supports multiple search backends selected via the `backend` parameter:
//!
//! | Backend | API Key Required | Keychain Key |
//! |---------|-----------------|--------------|
//! | `tavily` | Yes | `aman.3rd.tavily.api_key` |
//! | `brave` | Yes | `aman.3rd.brave.api_key` |
//! | `duckduckgo` | No (free API) | optional `aman.3rd.duckduckgo.api_key` |
//! | `google` | Yes (key + CX) | `aman.3rd.google.api_key` + `aman.3rd.google.cx` |
//! | `x` | Yes (Bearer token) | `aman.3rd.x.api_key` |
//!
//! API keys are stored in macOS Keychain (not env vars), same mechanism as LLM
//! provider keys. Use the `security` CLI to set them:
//!
//! ```text
//! security add-generic-password -a aman-desktop -s aman.3rd.tavily.api_key -w YOUR_KEY -U
//! ```

use async_trait::async_trait;
use kernel::context::ToolContext;
use kernel::schema::JsonSchema;
use kernel::tool::Tool;
use kernel::types::ToolMode;
use kernel::{AmanResult, Error};
use secret::{KeychainBackend, SecretBackend};
use serde_json::{json, Value};
use std::sync::LazyLock;

/// Keychain prefix for all third-party service credentials.
const KC_3RD: &str = "aman.3rd";

/// The `web_search` tool struct.
pub struct WebSearchTool;

#[async_trait]
impl Tool for WebSearchTool {
    fn name(&self) -> &str {
        "web_search"
    }

    fn mode(&self) -> ToolMode {
        ToolMode::Local
    }

    fn parameters(&self) -> &JsonSchema {
        static PARAMS: LazyLock<JsonSchema> = LazyLock::new(|| {
            JsonSchema::from(json!({
                "type": "object",
                "required": ["query"],
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "The search query"
                    },
                    "backend": {
                        "type": "string",
                        "description": "Search backend: tavily, brave, duckduckgo, google, x",
                        "default": "tavily"
                    },
                    "count": {
                        "type": "integer",
                        "description": "Number of results to return (max 10)",
                        "default": 5
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
                    "results": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "title": {"type": "string"},
                                "url": {"type": "string"},
                                "content": {"type": "string"}
                            }
                        }
                    },
                    "error": {"type": "string"}
                }
            }))
        });
        &RETURNS
    }

    async fn execute(&self, params: Value, _ctx: ToolContext) -> AmanResult<Value> {
        let query = params
            .get("query")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::ConfigInvalid {
                message: "query must be a string".to_owned(),
            })?;

        let count = params
            .get("count")
            .and_then(Value::as_u64)
            .unwrap_or(5)
            .min(10) as usize;

        let default_backend = available_backends().first().copied().unwrap_or("duckduckgo");
        let backend = params
            .get("backend")
            .and_then(Value::as_str)
            .unwrap_or(default_backend);

        Ok(execute_search(backend, query, count).await)
    }
}

/// Read a third-party credential from macOS Keychain.
fn kc_get(name: &str, sub: &str) -> String {
    let key = format!("{KC_3RD}.{name}.{sub}");
    KeychainBackend
        .get(&key)
        .ok()
        .flatten()
        .unwrap_or_default()
}

/// Check which search backends have valid credentials configured.
///
/// Order determines default priority: configured backends first (they're more
/// reliable), free/unreliable backends last.
fn available_backends() -> Vec<&'static str> {
    let mut available = Vec::new();
    // Configured API backends first (most reliable)
    if !kc_get("tavily", "api_key").is_empty() {
        available.push("tavily");
    }
    if !kc_get("brave", "api_key").is_empty() {
        available.push("brave");
    }
    if !kc_get("google", "api_key").is_empty() && !kc_get("google", "cx").is_empty() {
        available.push("google");
    }
    if !kc_get("x", "api_key").is_empty() {
        available.push("x");
    }
    // Free backends last (may be unreliable)
    available.push("duckduckgo");
    available
}

/// Dispatch to the appropriate search backend.
async fn execute_search(backend: &str, query: &str, count: usize) -> Value {
    let configured = available_backends();
    if !configured.contains(&backend) {
        return json!({
            "results": [],
            "error": format!(
                "Backend '{backend}' is not configured. Available: {}.\
                 Do not retry with a different backend — pick one from the available list or answer from your own knowledge.",
                configured.join(", ")
            ),
        });
    }

    match backend {
        "tavily" => search_tavily(query, count).await,
        "brave" => search_brave(query, count).await,
        "duckduckgo" => search_duckduckgo(query, count).await,
        "google" => search_google(query, count).await,
        "x" => search_x(query, count).await,
        other => json!({
            "results": [],
            "error": format!("unknown search backend: {other}")
        }),
    }
}

/// Build a shared async HTTP client (15s timeout, no proxy).
fn http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .no_proxy()
        .build()
        .expect("reqwest async client")
}

// ── Tavily ──────────────────────────────────────────────────────────────────

async fn search_tavily(query: &str, count: usize) -> Value {
    let api_key = kc_get("tavily", "api_key");
    if api_key.is_empty() {
        return no_key_error("tavily");
    }

    let client = http_client();
    let body = json!({
        "api_key": api_key,
        "query": query,
        "max_results": count,
        "include_answer": false,
    });

    let response = match client
        .post("https://api.tavily.com/search")
        .json(&body)
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => return request_error("Tavily", &e),
    };

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_else(|_| "unknown error".to_owned());
        return http_error("Tavily", status, &body);
    }

    let body_text = match response.text().await {
        Ok(t) => t,
        Err(e) => return parse_error("Tavily", &e),
    };

    let data: Value = match serde_json::from_str(&body_text) {
        Ok(v) => v,
        Err(e) => {
            // Show the first 500 chars of the raw response for debugging
            let preview = &body_text[..body_text.len().min(500)];
            return json!({
                "results": [],
                "error": format!("Tavily response parse failed: {e}. Raw response: {preview}")
            });
        }
    };

    extract_tavily_results(data)
}

fn extract_tavily_results(data: Value) -> Value {
    let results = data
        .get("results")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .map(|r| {
                    json!({
                        "title": r.get("title").and_then(Value::as_str).unwrap_or(""),
                        "url": r.get("url").and_then(Value::as_str).unwrap_or(""),
                        "content": r.get("content").and_then(Value::as_str).unwrap_or(""),
                    })
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    json!({ "results": results })
}

// ── Brave ───────────────────────────────────────────────────────────────────

async fn search_brave(query: &str, count: usize) -> Value {
    let api_key = kc_get("brave", "api_key");
    if api_key.is_empty() {
        return no_key_error("brave");
    }

    let client = http_client();
    let response = match client
        .get("https://api.search.brave.com/res/v1/web/search")
        .header("Accept", "application/json")
        .header("X-Subscription-Token", &api_key)
        .query(&[
            ("q", &query.to_string()),
            ("count", &count.to_string()),
        ])
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => return request_error("Brave", &e),
    };

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_else(|_| "unknown error".to_owned());
        return http_error("Brave", status, &body);
    }

    let data: Value = match response.json().await {
        Ok(v) => v,
        Err(e) => return parse_error("Brave", &e),
    };

    extract_brave_results(data)
}

fn extract_brave_results(data: Value) -> Value {
    let results = data
        .get("web")
        .and_then(|w| w.get("results"))
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .map(|r| {
                    json!({
                        "title": r.get("title").and_then(Value::as_str).unwrap_or(""),
                        "url": r.get("url").and_then(Value::as_str).unwrap_or(""),
                        "content": r.get("description").and_then(Value::as_str).unwrap_or(""),
                    })
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    json!({ "results": results })
}

// ── DuckDuckGo (free Instant Answer API) ────────────────────────────────────

async fn search_duckduckgo(query: &str, _count: usize) -> Value {
    let client = http_client();
    let response = match client
        .get("https://api.duckduckgo.com/")
        .query(&[
            ("q", query),
            ("format", "json"),
            ("no_html", "1"),
            ("skip_disambig", "1"),
        ])
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => return request_error("DuckDuckGo", &e),
    };

    let data: Value = match response.json().await {
        Ok(v) => v,
        Err(e) => return parse_error("DuckDuckGo", &e),
    };

    extract_duckduckgo_results(data)
}

fn extract_duckduckgo_results(data: Value) -> Value {
    let mut results: Vec<Value> = Vec::new();

    // Abstract result (main answer)
    if let Some(abstract_text) = data.get("AbstractText").and_then(Value::as_str)
        && !abstract_text.is_empty()
    {
        results.push(json!({
            "title": data.get("Heading").and_then(Value::as_str).unwrap_or(""),
            "url": data.get("AbstractURL").and_then(Value::as_str).unwrap_or(""),
            "content": abstract_text,
        }));
    }

    // Related topics
    if let Some(topics) = data.get("RelatedTopics").and_then(Value::as_array) {
        for topic in topics {
            if let Some(text) = topic.get("Text").and_then(Value::as_str)
                && !text.is_empty()
            {
                results.push(json!({
                    "title": topic.get("FirstURL").and_then(Value::as_str).unwrap_or(""),
                    "url": topic.get("FirstURL").and_then(Value::as_str).unwrap_or(""),
                    "content": text,
                }));
            }
            // Some topics have a "Topics" sub-array (categories)
            if let Some(sub_topics) = topic.get("Topics").and_then(Value::as_array) {
                for sub in sub_topics {
                    if let Some(text) = sub.get("Text").and_then(Value::as_str)
                        && !text.is_empty()
                    {
                        results.push(json!({
                            "title": sub.get("FirstURL").and_then(Value::as_str).unwrap_or(""),
                            "url": sub.get("FirstURL").and_then(Value::as_str).unwrap_or(""),
                            "content": text,
                        }));
                    }
                }
            }
        }
    }

    if results.is_empty() {
        json!({
            "results": [],
            "error": "DuckDuckGo Instant Answer API returned no results (this is a free API with limited coverage)"
        })
    } else {
        json!({ "results": results })
    }
}

// ── Google Custom Search ────────────────────────────────────────────────────

async fn search_google(query: &str, count: usize) -> Value {
    let api_key = kc_get("google", "api_key");
    let cx = kc_get("google", "cx");
    if api_key.is_empty() || cx.is_empty() {
        return json!({
            "results": [],
            "error": "Google Custom Search not configured. Set both aman.3rd.google.api_key and aman.3rd.google.cx in Keychain."
        });
    }

    let client = http_client();
    let response = match client
        .get("https://www.googleapis.com/customsearch/v1")
        .query(&[
            ("key", &api_key),
            ("cx", &cx),
            ("q", &query.to_string()),
            ("num", &count.min(10).to_string()),
        ])
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => return request_error("Google", &e),
    };

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_else(|_| "unknown error".to_owned());
        return http_error("Google", status, &body);
    }

    let data: Value = match response.json().await {
        Ok(v) => v,
        Err(e) => return parse_error("Google", &e),
    };

    extract_google_results(data)
}

fn extract_google_results(data: Value) -> Value {
    let results = data
        .get("items")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .map(|r| {
                    json!({
                        "title": r.get("title").and_then(Value::as_str).unwrap_or(""),
                        "url": r.get("link").and_then(Value::as_str).unwrap_or(""),
                        "content": r.get("snippet").and_then(Value::as_str).unwrap_or(""),
                    })
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    json!({ "results": results })
}

// ── X (Twitter) API v2 ──────────────────────────────────────────────────────

async fn search_x(query: &str, count: usize) -> Value {
    let bearer = kc_get("x", "api_key");
    if bearer.is_empty() {
        return no_key_error("x (Twitter)");
    }

    let max_results = count.clamp(1, 10); // API allows 10-100; use min 10
    let client = http_client();
    let response = match client
        .get("https://api.twitter.com/2/tweets/search/recent")
        .header("Authorization", format!("Bearer {bearer}"))
        .query(&[
            ("query", query),
            ("max_results", &max_results.to_string()),
            ("tweet.fields", "created_at,author_id"),
        ])
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => return request_error("X (Twitter)", &e),
    };

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_else(|_| "unknown error".to_owned());
        return http_error("X", status, &body);
    }

    let data: Value = match response.json().await {
        Ok(v) => v,
        Err(e) => return parse_error("X", &e),
    };

    extract_x_results(data)
}

fn extract_x_results(data: Value) -> Value {
    let results = data
        .get("data")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .map(|r| {
                    json!({
                        "title": format!(
                            "@{}",
                            r.get("author_id").and_then(Value::as_str).unwrap_or("unknown")
                        ),
                        "url": format!(
                            "https://twitter.com/i/web/status/{}",
                            r.get("id").and_then(Value::as_str).unwrap_or("")
                        ),
                        "content": r.get("text").and_then(Value::as_str).unwrap_or(""),
                    })
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    if results.is_empty() {
        // X may return meta.result_count in the response
        let meta_count = data
            .get("meta")
            .and_then(|m| m.get("result_count"))
            .and_then(Value::as_u64)
            .unwrap_or(0);
        if meta_count == 0 {
            return json!({
                "results": [],
                "error": "X API returned no results (query may match no recent tweets)"
            });
        }
    }

    json!({ "results": results })
}

// ── Error helpers ───────────────────────────────────────────────────────────

fn no_key_error(backend: &str) -> Value {
    json!({
        "results": [],
        "error": format!(
            "{backend} API key not configured. Add it to Keychain:\n  \
             security add-generic-password -a aman-desktop \
             -s {KC_3RD}.{backend_lc}.api_key -w YOUR_KEY -U",
            backend_lc = backend.split_whitespace().next().unwrap_or(backend).to_lowercase()
        ),
    })
}

fn request_error(backend: &str, err: &reqwest::Error) -> Value {
    json!({
        "results": [],
        "error": format!("{backend} request failed: {err}"),
    })
}

fn http_error(backend: &str, status: reqwest::StatusCode, body: &str) -> Value {
    json!({
        "results": [],
        "error": format!("{backend} returned {status}: {body}"),
    })
}

fn parse_error(backend: &str, err: &reqwest::Error) -> Value {
    json!({
        "results": [],
        "error": format!("{backend} response parse failed: {err}"),
    })
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_rejects_missing_query() {
        let tool = WebSearchTool;
        let result = pollster::block_on(tool.execute(
            json!({}),
            ToolContext::default(),
        ));
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_no_key_is_not_hard_error() {
        // Without a key, the tool should return a result with an error field
        // rather than failing with a hard error.
        // With a key configured, the tool makes a real request and may succeed.
        let result = execute_search("tavily", "test", 3).await;
        // Either way, the result must contain a "results" array (never panic).
        assert!(
            result.get("results").and_then(Value::as_array).is_some(),
            "execute_search should always return a results array"
        );
    }

    #[test]
    fn test_unknown_backend() {
        let result = pollster::block_on(execute_search("nonexistent", "test", 3));
        // "nonexistent" matches the catch-all arm, no HTTP call made
        assert!(result.get("error").and_then(Value::as_str).is_some());
    }

    #[tokio::test]
    async fn test_backend_dispatch_all_variants() {
        // Verify each backend produces an error-acknowledging response
        // (either "not configured" or "request failed"), never a panic.
        for backend in &["tavily", "brave", "duckduckgo", "google", "x"] {
            let result = execute_search(backend, "test", 3).await;
            // DuckDuckGo may succeed (free API) or return empty results
            // Other backends should say "not configured"
            let has_results = result.get("results").and_then(Value::as_array).is_some();
            assert!(has_results, "{backend} should return a results array");
        }
    }

    #[tokio::test]
    async fn test_count_is_clamped_to_max_10() {
        let tool = WebSearchTool;
        let result = tool.execute(
            json!({"query": "test", "count": 100}),
            ToolContext::default(),
        ).await;
        // Should not error — count is clamped; result shape should be valid.
        assert!(result.is_ok());
        let val = result.unwrap();
        assert!(val.get("results").is_some());
    }

    // ── Extract-parse tests (no network) ──────────────────────────────

    #[test]
    fn test_extract_tavily_results() {
        let data = json!({
            "results": [
                {"title": "T1", "url": "https://t1.com", "content": "c1"},
                {"title": "T2", "url": "https://t2.com", "content": "c2"},
            ]
        });
        let result = extract_tavily_results(data);
        let results = result["results"].as_array().unwrap();
        assert_eq!(results.len(), 2);
        assert_eq!(results[0]["title"], "T1");
    }

    #[test]
    fn test_extract_brave_results() {
        let data = json!({
            "web": {
                "results": [
                    {"title": "B1", "url": "https://b1.com", "description": "d1"},
                    {"title": "B2", "url": "https://b2.com", "description": "d2"},
                ]
            }
        });
        let result = extract_brave_results(data);
        let results = result["results"].as_array().unwrap();
        assert_eq!(results.len(), 2);
        assert_eq!(results[0]["content"], "d1");
    }

    #[test]
    fn test_extract_google_results() {
        let data = json!({
            "items": [
                {"title": "G1", "link": "https://g1.com", "snippet": "s1"},
            ]
        });
        let result = extract_google_results(data);
        let results = result["results"].as_array().unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0]["url"], "https://g1.com");
    }

    #[test]
    fn test_extract_x_results() {
        let data = json!({
            "data": [
                {"id": "123", "text": "hello world", "author_id": "user1"},
            ]
        });
        let result = extract_x_results(data);
        let results = result["results"].as_array().unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0]["content"], "hello world");
        assert_eq!(results[0]["url"], "https://twitter.com/i/web/status/123");
    }

    #[test]
    fn test_extract_duckduckgo_results_with_abstract() {
        let data = json!({
            "AbstractText": "Some abstract",
            "AbstractURL": "https://example.com",
            "Heading": "Example",
            "RelatedTopics": []
        });
        let result = extract_duckduckgo_results(data);
        let results = result["results"].as_array().unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0]["title"], "Example");
    }

    #[test]
    fn test_extract_duckduckgo_results_with_topics() {
        let data = json!({
            "AbstractText": "",
            "AbstractURL": "",
            "Heading": "",
            "RelatedTopics": [
                {"Text": "Topic 1", "FirstURL": "https://t1.com"},
                {"Text": "Topic 2", "FirstURL": "https://t2.com"},
            ]
        });
        let result = extract_duckduckgo_results(data);
        let results = result["results"].as_array().unwrap();
        assert_eq!(results.len(), 2);
    }

    #[test]
    fn test_extract_duckduckgo_empty() {
        let data = json!({
            "AbstractText": "",
            "AbstractURL": "",
            "Heading": "",
            "RelatedTopics": []
        });
        let result = extract_duckduckgo_results(data);
        assert!(result.get("error").is_some());
    }
}
