// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! [`MessageSender`] trait — abstract interface for sending messages
//! through a chat platform.

use crate::types::ChatTarget;
use async_trait::async_trait;
use kernel::AmanResult;

/// Opaque handle returned by [`MessageSender::begin_stream`].
/// Platform-specific: on Telegram this is the placeholder message ID.
pub type StreamHandle = String;

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

    // ── Streaming reply support ─────────────────────────────────────────

    /// Begin a streaming reply.
    ///
    /// Sends a placeholder message (e.g. "⏳ …") so the user sees
    /// immediate feedback.  Returns an opaque [`StreamHandle`] that is
    /// passed to subsequent [`update_stream`] calls.
    ///
    /// Default: no-op, returns an empty handle.
    async fn begin_stream(&self, _target: &ChatTarget) -> AmanResult<StreamHandle> {
        Ok(String::new())
    }

    /// Update (or finalise) a streaming reply.
    ///
    /// * `handle` — the opaque handle from [`begin_stream`].
    /// * `text` — the **full accumulated** text so far.
    /// * `finalize` — when `true` this is the last edit; platforms SHOULD
    ///   apply rich formatting (Markdown, HTML) on the final update.
    ///
    /// **Plain-text during streaming:** implementations MUST send `text` as
    /// plain text when `finalize` is `false` to avoid parse errors from
    /// unclosed markup tokens (e.g. `**bold`, `````fences```).
    ///
    /// Default: no-op for non-final updates; falls back to [`send_text`]
    /// for the final update.
    async fn update_stream(
        &self,
        target: &ChatTarget,
        handle: &str,
        text: &str,
        finalize: bool,
    ) -> AmanResult<()> {
        if finalize {
            self.send_text(target, text).await?;
        }
        let _ = (target, handle, text);
        Ok(())
    }

    /// Cancel / delete an in-progress streaming message.
    ///
    /// Called when the stream errors out so the placeholder doesn't
    /// linger.  Default: no-op (most platforms auto-expire typing
    /// indicators).
    async fn cancel_stream(&self, _target: &ChatTarget, _handle: &str) -> AmanResult<()> {
        Ok(())
    }
}
