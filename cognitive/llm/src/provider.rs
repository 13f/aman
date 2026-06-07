// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! LLM Provider abstraction — provider-agnostic chat completion interface.
//!
//! Moved here from `kernel::llm` as part of the cognitive engine decoupling.

use crate::react::{ChatMessage, ParsedToolCall, ToolDescriptor};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::sync::Arc;

/// Streaming event emitted during a streaming LLM response.
#[derive(Debug, Clone)]
pub enum StreamEvent {
    /// Stream has started.
    Start,
    /// A text chunk was received.
    Chunk(String),
    /// Stream completed with a finish reason ("stop", "length", "tool_calls").
    Done { finish_reason: String },
    /// An error occurred during streaming.
    Error(String),
}

/// Request to an LLM provider for a chat completion.
pub struct LlmChatRequest {
    pub model: String,
    pub system_prompt: String,
    pub messages: Vec<ChatMessage>,
    pub tools: Vec<ToolDescriptor>,
    pub max_output_tokens: u32,
    /// When set, the provider should request structured JSON output from the
    /// model (e.g. `response_format: { type: "json_object" }` for OpenAI).
    pub response_format: Option<String>,
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
