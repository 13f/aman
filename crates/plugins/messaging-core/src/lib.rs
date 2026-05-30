// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

#![forbid(unsafe_code)]
#![doc = "Shared messaging abstractions for chat platform integration."]

pub mod registry;
pub mod router;
pub mod sender;
pub mod session;
pub mod types;

pub use registry::ChannelRegistry;
pub use router::StickyAgentRouter;
pub use sender::MessageSender;
pub use session::ChatSessionStore;
pub use types::{
    make_session_id, ChatTarget, IncomingChatMessage, OutgoingReply, PlatformKind, AGENT_ID_KEY,
    CHAT_TARGET_KEY, MESSAGE_RECEIVED_EVENT, REPLY_KEY, REPLY_READY_EVENT, SESSION_ID_KEY,
};
