use crate::react::{ChatMessage, ParsedToolCall, ToolDescriptor};
use crate::Error;
use async_trait::async_trait;
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
    ) -> Result<LlmResponse, Error>;
}
