// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! ReAct loop types — chat messages, tool calls, token budgets, and the
//! ReAct engine trait.
//!
//! Moved here from `kernel::react` as part of the cognitive engine decoupling.
//! `SoulSnapshot` is kept — it's the LLM-specific rendering of the agent's
//! SOUL into a system prompt string.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Arc;

// ── Chat Messages ─────────────────────────────────────────────────────

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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<serde_json::Value>>,
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

    pub fn tool_result(
        tool_call_id: impl Into<String>,
        tool_name: impl Into<String>,
        content: impl Into<String>,
    ) -> Self {
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

// ── Tool Calls ────────────────────────────────────────────────────────

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
    pub pending_detach: Option<u32>,
}

/// Result of executing tool calls via `ReActEngine::execute_tools`.
#[derive(Debug, Clone)]
pub struct ToolExecutionResult {
    pub messages: Vec<ChatMessage>,
    pub pending_detach: Option<(u32, String)>,
}

// ── ReAct Engine ──────────────────────────────────────────────────────

/// Error types for the ReAct loop.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ReActError {
    LlmError(String),
    ToolError { tool_name: String, reason: String },
    BudgetExceeded { used: u64, limit: u64 },
    Interrupted,
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
    Finished { content: String, finish_reason: String },
    ToolCalls { content: String, calls: Vec<ParsedToolCall>, reasoning_content: String },
    Error(ReActError),
}

// ── Token Budget ──────────────────────────────────────────────────────

/// Token budget tracker.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenBudget {
    pub used: u64,
    pub limit: u64,
    pub max_output_tokens: u64,
}

impl TokenBudget {
    pub fn new(limit: u64) -> Self {
        Self { used: 0, limit, max_output_tokens: 0 }
    }

    pub fn with_output_limit(limit: u64, max_output_tokens: u64) -> Self {
        Self { used: 0, limit, max_output_tokens }
    }

    pub fn is_exceeded(&self) -> bool {
        self.used >= self.limit
    }

    pub fn add(&mut self, tokens: u64) -> u64 {
        self.used = self.used.saturating_add(tokens);
        self.used
    }

    pub fn fraction_used(&self) -> f64 {
        if self.limit == 0 { 0.0 } else { (self.used as f64) / (self.limit as f64) }
    }
}

// ── Tool Descriptor ───────────────────────────────────────────────────

/// Describes a tool available to the agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDescriptor {
    pub name: String,
    pub description: String,
    pub parameters: Value,
}

// ── Soul Snapshot (LLM-specific rendering) ────────────────────────────

/// A snapshot of the agent's SOUL, rendered as a system prompt for the LLM.
///
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

// ── ReAct Context ─────────────────────────────────────────────────────

/// Context for a single ReAct loop execution.
#[derive(Clone)]
pub struct ReActContext {
    pub agent_id: String,
    pub session_id: String,
    pub turn: u32,
    pub max_turns: u32,
    pub soul_snapshot: SoulSnapshot,
    pub history: Vec<ChatMessage>,
    pub agent_tools: Vec<ToolDescriptor>,
    pub memory_context: Option<String>,
    pub token_budget: TokenBudget,
    pub model: String,
    pub stream_cb: Option<Arc<dyn Fn(crate::provider::StreamEvent) + Send + Sync>>,
    pub interrupt_flag: Option<Arc<kernel::interrupt::InterruptFlag>>,
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

// ── Tool Permission ───────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolPermission {
    pub tool_name: String,
    pub allowed_agent_ids: Vec<String>,
    pub deny_agent_ids: Vec<String>,
}

// ── ReAct Engine Trait ────────────────────────────────────────────────

/// The ReAct loop engine trait.
///
/// Implementations coordinate LLM calls, tool execution, and
/// result feedback in a think-act-observe iteration.
#[async_trait::async_trait]
pub trait ReActEngine: Send + Sync {
    async fn execute_turn(
        &self,
        ctx: &ReActContext,
        messages: Vec<ChatMessage>,
    ) -> Result<ReActTurn, ReActError>;

    async fn execute_tools(
        &self,
        ctx: &ReActContext,
        calls: &[ParsedToolCall],
        block_on_detach: bool,
    ) -> Result<ToolExecutionResult, ReActError>;
}
