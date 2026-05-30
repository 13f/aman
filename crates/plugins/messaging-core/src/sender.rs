// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! [`MessageSender`] trait — abstract interface for sending messages
//! through a chat platform.

use crate::types::ChatTarget;
use async_trait::async_trait;
use kernel::AmanResult;

/// Platform-agnostic interface for sending messages back to a chat channel.
///
/// Each platform crate (Telegram, Slack, etc.) provides its own implementation
/// that wraps the platform SDK client.
#[async_trait]
pub trait MessageSender: Send + Sync {
    /// Send a plain-text message to a chat.
    async fn send_text(&self, target: &ChatTarget, text: &str) -> AmanResult<()>;

    /// Send a markdown-formatted message, if the platform supports rich
    /// formatting.  Falls back to `send_text` by default.
    async fn send_markdown(&self, target: &ChatTarget, text: &str) -> AmanResult<()> {
        self.send_text(target, text).await
    }

    /// Send a reply threaded to a specific inbound message.
    async fn send_reply(
        &self,
        target: &ChatTarget,
        _reply_to_message_id: &str,
        text: &str,
    ) -> AmanResult<()> {
        self.send_text(target, text).await
    }

    /// Indicate a typing / "bot is thinking …" indicator, if supported.
    async fn send_typing(&self, _target: &ChatTarget) -> AmanResult<()> {
        Ok(())
    }
}
