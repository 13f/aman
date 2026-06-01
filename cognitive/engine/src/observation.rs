// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! Observation types — inputs from the event bus to the cognitive engine.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// An observation is a structured input delivered to the cognitive engine.
///
/// It represents something the agent should be aware of: a user message,
/// a completed tool execution, a timer firing, a world state change, etc.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Observation {
    /// Unique identifier for this observation.
    pub id: String,
    /// The session this observation belongs to.
    pub session_id: String,
    /// The type and content of the observation.
    pub payload: ObservationPayload,
    /// Priority hint (0 = low, 100 = critical).
    pub priority: u8,
}

/// The type and content of an observation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ObservationPayload {
    /// A user sent a text message.
    UserMessage {
        text: String,
    },
    /// A tool execution completed (result from a prior `Decision::CallTool`).
    ToolCompleted {
        tool_call_id: String,
        tool_name: String,
        output: String,
        success: bool,
        duration_ms: u64,
    },
    /// A detached process completed (used for background tool execution).
    DetachedCompleted {
        pid: u32,
        tool_call_id: String,
        output: String,
        success: bool,
    },
    /// A timer or cron trigger fired.
    TimerFired {
        source_id: String,
        cron_id: Option<String>,
    },
    /// A system event (internal notification, queue drained, etc.).
    SystemEvent {
        event_type: String,
        data: Value,
    },
    /// A world state change notification.
    WorldStateChange {
        diff: Value,
    },
}

impl Observation {
    /// Create a user message observation.
    pub fn user_message(id: impl Into<String>, session_id: impl Into<String>, text: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            session_id: session_id.into(),
            payload: ObservationPayload::UserMessage { text: text.into() },
            priority: 50,
        }
    }

    /// Create a tool completion observation.
    pub fn tool_completed(
        id: impl Into<String>,
        session_id: impl Into<String>,
        tool_call_id: impl Into<String>,
        tool_name: impl Into<String>,
        output: impl Into<String>,
        success: bool,
        duration_ms: u64,
    ) -> Self {
        Self {
            id: id.into(),
            session_id: session_id.into(),
            payload: ObservationPayload::ToolCompleted {
                tool_call_id: tool_call_id.into(),
                tool_name: tool_name.into(),
                output: output.into(),
                success,
                duration_ms,
            },
            priority: 50,
        }
    }

    /// Create a system event observation.
    pub fn system_event(
        id: impl Into<String>,
        session_id: impl Into<String>,
        event_type: impl Into<String>,
        data: Value,
    ) -> Self {
        Self {
            id: id.into(),
            session_id: session_id.into(),
            payload: ObservationPayload::SystemEvent {
                event_type: event_type.into(),
                data,
            },
            priority: 30,
        }
    }
}
