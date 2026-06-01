// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! Decision types — outputs from the cognitive engine to the event bus.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A decision is the cognitive engine's response to observations.
///
/// The gateway converts decisions into events and publishes them to the
/// event bus. The engine does not interact with the event bus directly.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Decision {
    /// Unique identifier for this decision.
    pub id: String,
    /// The session this decision belongs to.
    pub session_id: String,
    /// What action to take.
    pub kind: DecisionKind,
    /// Optional metadata (engine-specific hints, confidence scores, etc.).
    pub metadata: Value,
}

/// The kind of action the engine wants to take.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum DecisionKind {
    /// Send a text reply to the user.
    Reply {
        text: String,
        /// Whether this is a final reply or an intermediate streaming chunk.
        is_final: bool,
    },
    /// Execute a tool or set of tools.
    CallTools {
        calls: Vec<ToolCallRequest>,
        /// If true, the gateway should wait for tool completion before
        /// sending more observations.
        block_on_completion: bool,
    },
    /// Delegate work to another agent.
    Delegate {
        target_agent_id: String,
        task: String,
    },
    /// Wait for a specific event type before continuing (pause cognition).
    WaitFor {
        event_types: Vec<String>,
        timeout_ms: Option<u64>,
    },
    /// Store something in memory.
    Remember {
        key: String,
        content: String,
        importance: f64,
    },
    /// The engine has no action to take (idle).
    NoOp,
}

/// A request to execute a specific tool.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallRequest {
    /// Unique ID for tracking this call.
    pub id: String,
    /// The tool name to invoke.
    pub tool_name: String,
    /// Tool arguments as a JSON value.
    pub args: Value,
    /// If true, detach the tool process (run in background).
    pub detach: bool,
}

impl Decision {
    /// Create a final text reply.
    pub fn reply(
        id: impl Into<String>,
        session_id: impl Into<String>,
        text: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            session_id: session_id.into(),
            kind: DecisionKind::Reply {
                text: text.into(),
                is_final: true,
            },
            metadata: Value::Null,
        }
    }

    /// Create a streaming chunk reply (not final).
    pub fn reply_chunk(
        id: impl Into<String>,
        session_id: impl Into<String>,
        text: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            session_id: session_id.into(),
            kind: DecisionKind::Reply {
                text: text.into(),
                is_final: false,
            },
            metadata: Value::Null,
        }
    }

    /// Create a tool call decision.
    pub fn call_tools(
        id: impl Into<String>,
        session_id: impl Into<String>,
        calls: Vec<ToolCallRequest>,
    ) -> Self {
        Self {
            id: id.into(),
            session_id: session_id.into(),
            kind: DecisionKind::CallTools {
                calls,
                block_on_completion: true,
            },
            metadata: Value::Null,
        }
    }

    /// Create a no-op decision.
    pub fn noop(id: impl Into<String>, session_id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            session_id: session_id.into(),
            kind: DecisionKind::NoOp,
            metadata: Value::Null,
        }
    }
}
