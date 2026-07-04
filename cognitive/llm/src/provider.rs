// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! LLM Provider abstraction — provider-agnostic chat completion interface.
//!
//! Moved here from `kernel::llm` as part of the cognitive engine decoupling.

use crate::react::{ChatMessage, ParsedToolCall, ToolDescriptor};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::sync::Arc;

// Re-export from the shared leaf crate.
pub use cognitive_react::StreamEvent;

/// Structured output format requested from the LLM.
#[derive(Debug, Clone)]
pub enum ResponseFormat {
    /// Request JSON object mode (`{ type: "json_object" }` for OpenAI).
    JsonObject,
    /// Request strict structured output with a JSON schema
    /// Sent as `{ type: "json_object" }` for universal provider compatibility;
    /// the schema is enforced via post-processing instead.
    JsonSchema {
        /// Schema name (must match `[a-zA-Z0-9_-]+` for OpenAI).
        name: String,
        /// JSON Schema value.
        schema: serde_json::Value,
        /// Whether to enforce strict mode (OpenAI: `strict: true`).
        strict: bool,
    },
}

/// Request to an LLM provider for a chat completion.
#[derive(Clone)]
pub struct LlmChatRequest {
    pub model: String,
    pub system_prompt: String,
    pub messages: Vec<ChatMessage>,
    pub tools: Vec<ToolDescriptor>,
    pub max_output_tokens: u32,
    /// When set, the provider should request structured JSON output from the
    /// model using the specified format.
    pub response_format: Option<ResponseFormat>,
}

/// Response from an LLM provider after a chat completion.
#[derive(Debug, Default)]
pub struct LlmResponse {
    pub content: String,
    pub finish_reason: String,
    pub tool_calls: Vec<ParsedToolCall>,
    pub reasoning_content: String,
}

/// Provider-agnostic LLM chat completion interface.
///
/// Each LLM backend (OpenAI, Anthropic, etc.) implements this trait.
/// The agent harness calls `chat_completion` with a request and optional
/// streaming callback, and receives a structured response.
#[async_trait]
pub trait LlmProvider: Send + Sync {
    /// Provider name identifier (e.g. "openai", "claude").
    fn name(&self) -> &str;

    /// Base URL of the LLM API endpoint (e.g. "https://api.openai.com/v1").
    fn base_url(&self) -> &str;

    /// Send a chat completion request to the LLM.
    ///
    /// When `cb` is `Some`, the implementation SHOULD stream the response
    /// via SSE/SSE-like protocol and invoke the callback for each event.
    /// When `cb` is `None`, the implementation returns the complete response.
    async fn chat_completion(
        &self,
        req: LlmChatRequest,
        cb: Option<Arc<dyn Fn(StreamEvent) + Send + Sync>>,
    ) -> Result<LlmResponse, String>;
}

/// Format [`ParsedToolCall`]s into the JSON format expected in
/// conversation history for the next LLM turn.
///
/// Produces the OpenAI `tool_calls` structure so providers that
/// follow that convention can echo tool calls back to the API.
pub fn format_tool_calls_for_history(calls: &[ParsedToolCall]) -> Vec<Value> {
    calls
        .iter()
        .map(|tc| {
            json!({
                "id": tc.id,
                "type": "function",
                "function": {
                    "name": tc.tool_name,
                    "arguments": serde_json::to_string(&tc.args).unwrap_or_default(),
                }
            })
        })
        .collect()
}
