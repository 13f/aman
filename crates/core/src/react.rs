use serde::{Deserialize, Serialize};
use serde_json::Value;

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
}

impl ChatMessage {
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: ChatMessageRole::System,
            content: content.into(),
            tool_call_id: None,
            tool_name: None,
        }
    }

    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: ChatMessageRole::User,
            content: content.into(),
            tool_call_id: None,
            tool_name: None,
        }
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: ChatMessageRole::Assistant,
            content: content.into(),
            tool_call_id: None,
            tool_name: None,
        }
    }

    pub fn tool_result(tool_call_id: impl Into<String>, tool_name: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            role: ChatMessageRole::Tool,
            content: content.into(),
            tool_call_id: Some(tool_call_id.into()),
            tool_name: Some(tool_name.into()),
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
    ToolCalls(Vec<ParsedToolCall>),
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
}

impl TokenBudget {
    pub fn new(limit: u64) -> Self {
        Self { used: 0, limit }
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
#[derive(Debug, Clone)]
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
}

impl ReActContext {
    /// Create a new ReActContext with default budget.
    pub fn new(
        agent_id: impl Into<String>,
        session_id: impl Into<String>,
        soul_snapshot: SoulSnapshot,
        history: Vec<ChatMessage>,
        agent_tools: Vec<ToolDescriptor>,
        max_turns: u32,
        token_limit: u64,
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
            token_budget: TokenBudget::new(token_limit),
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

    /// Execute tool calls and return result messages.
    async fn execute_tools(
        &self,
        ctx: &ReActContext,
        calls: &[ParsedToolCall],
    ) -> Result<Vec<ChatMessage>, ReActError>;
}
