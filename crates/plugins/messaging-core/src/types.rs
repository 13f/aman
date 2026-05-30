// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! Shared types for messaging channel integration.
//!
//! These types are platform-agnostic and used by all messaging platform crates.

use serde::{Deserialize, Serialize};

/// Identifies the messaging platform.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlatformKind {
    Telegram,
    Slack,
    Discord,
    Matrix,
}

impl PlatformKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Telegram => "telegram",
            Self::Slack => "slack",
            Self::Discord => "discord",
            Self::Matrix => "matrix",
        }
    }
}

/// A target for sending a reply message through a chat platform.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatTarget {
    /// Platform identifier (e.g., "telegram", "slack").
    pub platform: PlatformKind,
    /// Platform-specific chat/room/channel ID.
    pub chat_id: String,
    /// Source ID registered in SourceRegistry (e.g., "chat:telegram:mybot").
    pub source_id: String,
    /// Optional thread timestamp or thread ID for threading support.
    pub thread_id: Option<String>,
}

/// Inbound message received from a chat platform, before routing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IncomingChatMessage {
    pub platform: PlatformKind,
    pub chat_id: String,
    pub source_id: String,
    pub message_id: String,
    pub text: String,
    pub user_id: Option<String>,
    pub user_display_name: Option<String>,
    pub thread_id: Option<String>,
}

/// Reply to be sent back through a chat platform.
#[derive(Debug, Clone)]
pub struct OutgoingReply {
    pub target: ChatTarget,
    pub text: String,
}

/// Built-in `session_id` payload key used to correlate chat messages with
/// agent reply events.  The session ID is deterministic:
/// `chat:{platform}:{chat_id}`.
pub const SESSION_ID_KEY: &str = "session_id";

/// Payload key for the serialised [`ChatTarget`] within an inbound event.
/// The `ChatIngestionHandler` reads this key to store routing information
/// in the [`ChatSessionStore`](super::ChatSessionStore).
pub const CHAT_TARGET_KEY: &str = "_chat_target";

/// Payload key for the resolved agent ID injected by the
/// [`StickyAgentRouter`](super::StickyAgentRouter).
pub const AGENT_ID_KEY: &str = "agent_id";

/// Reply text key within ``agent:reply_ready`` events.
pub const REPLY_KEY: &str = "reply";

/// Standard event type emitted when a chat message arrives.
pub const MESSAGE_RECEIVED_EVENT: &str = "message_received";

/// Standard event type for agent replies ready to be sent back.
pub const REPLY_READY_EVENT: &str = "agent:reply_ready";

/// Construct the deterministic session ID for a chat platform + chat ID pair.
#[must_use]
pub fn make_session_id(platform: PlatformKind, chat_id: &str) -> String {
    format!("chat:{}:{}", platform.as_str(), chat_id)
}
