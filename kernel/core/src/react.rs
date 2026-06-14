// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! ⚠️ DEPRECATED SHIM — kept only because `cognitive-llm` depends on this
//! crate. The source of truth for ReAct types now lives in
//! `cognitive_llm::react` (see `cognitive/llm/src/react.rs`). New code should
//! depend on `cognitive-llm` directly and use those types.
//!
//! Full migration requires extracting these types into a leaf crate with no
//! `kernel` dependency — see the note in `kernel/core/src/llm.rs` and the
//! P1 roadmap in `docs/code-review-20260614.md`.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Arc;

/// Role of a chat message in a conversation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ChatMessageRole {
    System,
    User,
    Assistant,
    Tool,
}

/// A single message in a chat conversation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: ChatMessageRole,
    pub content: String,
    /// Optional tool call ID (for Tool role messages).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    /// Optional tool name for Tool role messages.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
    /// Structured tool calls for assistant messages that invoked tools (OpenAI format).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<serde_json::Value>>,
    /// Reasoning/thinking content (e.g. DeepSeek `reasoning_content`).
    /// Must be echoed back to the API when present in the original response.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub reasoning_content: String,
}

impl ChatMessage {
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: ChatMessageRole::System,
            content: content.into(),
            tool_call_id: None,
            tool_name: None,
            tool_calls: None,
            reasoning_content: String::new(),
        }
    }

    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: ChatMessageRole::User,
            content: content.into(),
            tool_call_id: None,
            tool_name: None,
            tool_calls: None,
            reasoning_content: String::new(),
        }
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: ChatMessageRole::Assistant,
            content: content.into(),
            tool_call_id: None,
            tool_name: None,
            tool_calls: None,
            reasoning_content: String::new(),
        }
    }

    pub fn tool_result(tool_call_id: impl Into<String>, tool_name: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            role: ChatMessageRole::Tool,
            content: content.into(),
            tool_call_id: Some(tool_call_id.into()),
            tool_name: Some(tool_name.into()),
            tool_calls: None,
            reasoning_content: String::new(),
        }
    }
}

/// A parsed tool call extracted from the LLM response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParsedToolCall {
    pub id: String,
    pub tool_name: String,
    pub args: Value,
}

/// Result of executing a single tool call.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallResult {
    pub id: String,
    pub tool_name: String,
    pub success: bool,
    pub output: String,
    pub duration_ms: u64,
    /// When set, the tool spawned a detached process (PID) that is still
    /// running. The caller should wait for a `tool:completed` event before
    /// feeding this result to the LLM.
    pub pending_detach: Option<u32>,
}

/// Result of executing tool calls via `ReActEngine::execute_tools`.
#[derive(Debug, Clone)]
pub struct ToolExecutionResult {
    /// Chat messages representing the tool results (ready for LLM history).
    pub messages: Vec<ChatMessage>,
    /// When `block_on_detach = false` and a tool spawned a detached process,
    /// this holds the (pid, tool_call_id) so the caller can wait for
    /// completion later.
    pub pending_detach: Option<(u32, String)>,
}

/// Error types for the ReAct loop.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ReActError {
    /// LLM provider returned an error.
    LlmError(String),
    /// Tool execution failed.
    ToolError { tool_name: String, reason: String },
    /// Token budget exceeded.
    BudgetExceeded { used: u64, limit: u64 },
    /// ReAct loop was interrupted (e.g., user stop).
    Interrupted,
    /// Maximum ReAct turns reached.
    MaxTurnsReached { turns: u32 },
}

impl std::fmt::Display for ReActError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::LlmError(msg) => write!(f, "LLM error: {msg}"),
            Self::ToolError { tool_name, reason } => write!(f, "tool '{tool_name}' error: {reason}"),
            Self::BudgetExceeded { used, limit } => write!(f, "token budget exceeded: {used}/{limit}"),
            Self::Interrupted => write!(f, "ReAct loop interrupted"),
            Self::MaxTurnsReached { turns } => write!(f, "max turns ({turns}) reached"),
        }
    }
}

impl std::error::Error for ReActError {}

/// Result of one ReAct iteration.
#[derive(Debug, Clone)]
pub enum ReActTurn {
    /// LLM returned a text-only reply → loop ends.
    Finished {
        content: String,
        finish_reason: String,
    },
    /// LLM returned tool calls → loop continues.
    ToolCalls {
        /// Text content that accompanied the tool calls.
        content: String,
        /// The parsed tool calls.
        calls: Vec<ParsedToolCall>,
        /// Reasoning/thinking content from the LLM (e.g. DeepSeek reasoning_content).
        reasoning_content: String,
    },
    /// LLM call failed → loop terminates abnormally.
    Error(ReActError),
}

/// Token budget tracker.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenBudget {
    /// Tokens used so far in this session.
    pub used: u64,
    /// Maximum allowed tokens.
    pub limit: u64,
    /// Maximum output tokens per LLM call (sent as `max_tokens` parameter).
    pub max_output_tokens: u64,
}

impl TokenBudget {
    pub fn new(limit: u64) -> Self {
        Self { used: 0, limit, max_output_tokens: 0 }
    }

    /// Create with explicit max_output_tokens for the LLM `max_tokens` parameter.
    pub fn with_output_limit(limit: u64, max_output_tokens: u64) -> Self {
        Self { used: 0, limit, max_output_tokens }
    }

    /// Check if the budget has been exceeded.
    pub fn is_exceeded(&self) -> bool {
        self.used >= self.limit
    }

    /// Add token usage, returning the new total.
    pub fn add(&mut self, tokens: u64) -> u64 {
        self.used = self.used.saturating_add(tokens);
        self.used
    }

    /// Fraction of budget used (0.0 – 1.0).
    pub fn fraction_used(&self) -> f64 {
        if self.limit == 0 {
            return 0.0;
        }
        (self.used as f64) / (self.limit as f64)
    }
}

/// Describes a tool available to the agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDescriptor {
    pub name: String,
    pub description: String,
    /// JSON schema for the tool parameters.
    pub parameters: Value,
}

/// A snapshot of the agent's SOUL at the time of message processing.
/// Keeps the prompt stable throughout the ReAct loop.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SoulSnapshot {
    pub name: String,
    pub system_prompt: String,
    pub boundaries: Vec<String>,
}

impl SoulSnapshot {
    pub fn new(name: impl Into<String>, system_prompt: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            system_prompt: system_prompt.into(),
            boundaries: vec![],
        }
    }
}

/// Context for a single ReAct loop execution.
#[derive(Clone)]
pub struct ReActContext {
    pub agent_id: String,
    pub session_id: String,
    /// Current turn number (0-based).
    pub turn: u32,
    /// Maximum ReAct turns allowed.
    pub max_turns: u32,
    /// Snapshot of the agent's SOUL at the start of processing.
    pub soul_snapshot: SoulSnapshot,
    /// Conversation history.
    pub history: Vec<ChatMessage>,
    /// Tools available to this agent.
    pub agent_tools: Vec<ToolDescriptor>,
    /// Optional retrieved memory context.
    pub memory_context: Option<String>,
    /// Token budget tracker.
    pub token_budget: TokenBudget,
    /// The LLM model name for this context.
    pub model: String,
    /// Optional streaming callback (T2.4).
    ///
    /// When set, the ReAct engine will call this with `StreamEvent::Chunk`
    /// as each delta arrives from the LLM, enabling real-time output.
    pub stream_cb: Option<Arc<dyn Fn(StreamEvent) + Send + Sync>>,
    /// Optional interrupt flag, set by the harness to cancel long-running
    /// operations (e.g. detached process execution).
    pub interrupt_flag: Option<Arc<crate::interrupt::InterruptFlag>>,
}

impl std::fmt::Debug for ReActContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReActContext")
            .field("agent_id", &self.agent_id)
            .field("session_id", &self.session_id)
            .field("turn", &self.turn)
            .field("max_turns", &self.max_turns)
            .field("soul_snapshot", &self.soul_snapshot)
            .field("history", &self.history)
            .field("agent_tools", &self.agent_tools)
            .field("memory_context", &self.memory_context)
            .field("token_budget", &self.token_budget)
            .field("model", &self.model)
            .field("stream_cb", &self.stream_cb.as_ref().map(|_| "Some(cb)"))
            .finish()
    }
}

impl ReActContext {
    /// Create a new ReActContext with default budget.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        agent_id: impl Into<String>,
        session_id: impl Into<String>,
        soul_snapshot: SoulSnapshot,
        history: Vec<ChatMessage>,
        agent_tools: Vec<ToolDescriptor>,
        model: impl Into<String>,
        max_turns: u32,
        token_limit: u64,
        max_output_tokens: u64,
    ) -> Self {
        Self {
            agent_id: agent_id.into(),
            session_id: session_id.into(),
            turn: 0,
            max_turns,
            soul_snapshot,
            history,
            agent_tools,
            memory_context: None,
            token_budget: TokenBudget::with_output_limit(token_limit, max_output_tokens),
            model: model.into(),
            stream_cb: None,
            interrupt_flag: None,
        }
    }
}

/// Tool permission entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolPermission {
    pub tool_name: String,
    pub allowed_agent_ids: Vec<String>,
    pub deny_agent_ids: Vec<String>,
}

/// Streaming event emitted during a streaming LLM response (T2.4).
///
/// The agent harness creates a callback that forwards these events
/// to the event bus as `agent:reply_chunk` etc.
pub use crate::llm::StreamEvent;

/// The ReAct loop engine trait.
///
/// Implementations coordinate LLM calls, tool execution, and
/// result feedback in a think-act-observe iteration.
#[async_trait::async_trait]
pub trait ReActEngine: Send + Sync {
    /// Execute one ReAct iteration: send messages to the LLM
    /// and return the parsed result.
    async fn execute_turn(
        &self,
        ctx: &ReActContext,
        messages: Vec<ChatMessage>,
    ) -> Result<ReActTurn, ReActError>;

    /// Execute tool calls and return result messages plus optional
    /// detach-tracking info.
    ///
    /// When `block_on_detach` is `false`, detached processes are spawned and
    /// their spawn result is returned immediately.  The caller is responsible
    /// for waiting for the real completion (via the `tool:completed` event
    /// from `tool:detached`) before feeding the results to the LLM.
    async fn execute_tools(
        &self,
        ctx: &ReActContext,
        calls: &[ParsedToolCall],
        block_on_detach: bool,
    ) -> Result<ToolExecutionResult, ReActError>;
}
