// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! [`TelegramSender`] — `MessageSender` implementation backed by teloxide.

use async_trait::async_trait;
use kernel::AmanResult;
use messaging_core::sender::{MessageSender, StreamHandle};
use messaging_core::types::ChatTarget;
use teloxide::prelude::*;
use teloxide::types::{ChatId, MessageId, ParseMode, Recipient};

/// Build a proxy-aware reqwest client for Telegram API calls.
/// Reads the proxy URL from (in order): `ALL_PROXY`, `HTTPS_PROXY`, `https_proxy`.
#[must_use]
pub fn build_telegram_client() -> reqwest::Client {
    let mut builder = reqwest::Client::builder();
    if let Some(proxy_url) = kernel::proxy::detect_proxy_url() {
        match reqwest::Proxy::all(&proxy_url) {
            Ok(proxy) => {
                tracing::info!(%proxy_url, "telegram: using proxy");
                builder = builder.proxy(proxy);
            }
            Err(e) => {
                tracing::warn!(%proxy_url, error = %e, "telegram: invalid proxy URL");
            }
        }
    } else {
        tracing::info!("telegram: no proxy configured (ALL_PROXY/HTTPS_PROXY not set)");
    }
    builder.build().expect("build telegram reqwest client")
}

/// Build a teloxide [`Bot`] using the proxy-aware client.
#[must_use]
pub fn build_telegram_bot(bot_token: &str) -> Bot {
    Bot::with_client(bot_token, build_telegram_client())
}

/// Sends messages back to Telegram via the Bot API.
pub struct TelegramSender {
    bot: Bot,
}

impl TelegramSender {
    #[must_use]
    pub fn new(bot_token: &str) -> Self {
        Self {
            bot: build_telegram_bot(bot_token),
        }
    }

    /// Return a clone of the inner [`Bot`] for use in dispatcher handlers.
    #[must_use]
    pub fn bot(&self) -> Bot {
        self.bot.clone()
    }
}

fn parse_chat_id(chat_id: &str) -> Result<ChatId, String> {
    let val: i64 = chat_id
        .parse()
        .map_err(|e| format!("invalid Telegram chat_id '{chat_id}': {e}"))?;
    Ok(ChatId(val))
}

#[async_trait]
impl MessageSender for TelegramSender {
    async fn send_text(&self, target: &ChatTarget, text: &str) -> AmanResult<()> {
        let chat_id = parse_chat_id(&target.chat_id)
            .map_err(|e| kernel::Error::config_invalid(e))?;
        self.bot
            .send_message(Recipient::Id(chat_id), text)
            .await
            .map_err(|e| kernel::Error::Unrecoverable {
                message: format!("telegram send failed: {e}"),
            })?;
        Ok(())
    }

    async fn send_markdown(&self, target: &ChatTarget, text: &str) -> AmanResult<()> {
        let chat_id = parse_chat_id(&target.chat_id)
            .map_err(|e| kernel::Error::config_invalid(e))?;
        self.bot
            .send_message(Recipient::Id(chat_id), text)
            .parse_mode(ParseMode::MarkdownV2)
            .await
            .map_err(|e| kernel::Error::Unrecoverable {
                message: format!("telegram send markdown failed: {e}"),
            })?;
        Ok(())
    }

    async fn send_reply(
        &self,
        target: &ChatTarget,
        reply_to_message_id: &str,
        text: &str,
    ) -> AmanResult<()> {
        use teloxide::types::{MessageId, ReplyParameters};
        let chat_id = parse_chat_id(&target.chat_id)
            .map_err(|e| kernel::Error::config_invalid(e))?;
        let reply_to: i32 = reply_to_message_id
            .parse()
            .map_err(|e| kernel::Error::config_invalid(format!("invalid message_id: {e}")))?;
        let req = self
            .bot
            .send_message(Recipient::Id(chat_id), text)
            .reply_parameters(ReplyParameters::new(MessageId(reply_to)));
        req.await
            .map_err(|e| kernel::Error::Unrecoverable {
                message: format!("telegram reply failed: {e}"),
            })?;
        Ok(())
    }

    async fn send_typing(&self, target: &ChatTarget) -> AmanResult<()> {
        use teloxide::types::ChatAction;
        let chat_id = parse_chat_id(&target.chat_id)
            .map_err(|e| kernel::Error::config_invalid(e))?;
        self.bot
            .send_chat_action(Recipient::Id(chat_id), ChatAction::Typing)
            .await
            .map_err(|e| kernel::Error::Unrecoverable {
                message: format!("telegram typing indicator failed: {e}"),
            })?;
        Ok(())
    }

    // ── Streaming reply support ─────────────────────────────────────────

    async fn begin_stream(&self, target: &ChatTarget) -> AmanResult<StreamHandle> {
        let chat_id = parse_chat_id(&target.chat_id)
            .map_err(|e| kernel::Error::config_invalid(e))?;
        // Send a minimal placeholder so the user sees immediate feedback.
        // Plain-text only — no parse_mode, to avoid unclosed-marker errors.
        let msg = self
            .bot
            .send_message(Recipient::Id(chat_id), "⏳ …")
            .await
            .map_err(|e| kernel::Error::Unrecoverable {
                message: format!("telegram begin_stream (placeholder) failed: {e}"),
            })?;
        Ok(msg.id.0.to_string())
    }

    async fn update_stream(
        &self,
        target: &ChatTarget,
        handle: &str,
        text: &str,
        finalize: bool,
    ) -> AmanResult<()> {
        let chat_id = parse_chat_id(&target.chat_id)
            .map_err(|e| kernel::Error::config_invalid(e))?;
        let msg_id: i32 = handle
            .parse()
            .map_err(|e| kernel::Error::config_invalid(format!("invalid stream handle: {e}")))?;

        let mut req = self
            .bot
            .edit_message_text(Recipient::Id(chat_id), MessageId(msg_id), text);

        // Only apply MarkdownV2 on the final edit — during streaming the
        // accumulated text may contain unclosed tokens (e.g. **bold, ```fence)
        // that would cause a Telegram 400 parse error.
        if finalize {
            req = req.parse_mode(ParseMode::MarkdownV2);
        }

        req.await.map_err(|e| kernel::Error::Unrecoverable {
            message: format!("telegram update_stream (edit) failed: {e}"),
        })?;
        Ok(())
    }

    async fn cancel_stream(&self, target: &ChatTarget, handle: &str) -> AmanResult<()> {
        let chat_id = parse_chat_id(&target.chat_id)
            .map_err(|e| kernel::Error::config_invalid(e))?;
        let msg_id: i32 = handle
            .parse()
            .map_err(|e| kernel::Error::config_invalid(format!("invalid stream handle: {e}")))?;
        // Best-effort — don't fail if the message was already deleted.
        if let Err(e) = self
            .bot
            .delete_message(Recipient::Id(chat_id), MessageId(msg_id))
            .await
        {
            tracing::warn!(
                error = %e,
                chat_id = %target.chat_id,
                msg_id = %msg_id,
                "telegram cancel_stream (delete) failed, ignoring"
            );
        }
        Ok(())
    }
}
