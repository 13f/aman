// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! [`TelegramSender`] — `MessageSender` implementation backed by teloxide.

use async_trait::async_trait;
use kernel::AmanResult;
use messaging_core::sender::MessageSender;
use messaging_core::types::ChatTarget;
use teloxide::prelude::*;
use teloxide::types::{ChatId, ParseMode, Recipient};

/// Sends messages back to Telegram via the Bot API.
pub struct TelegramSender {
    bot: Bot,
}

impl TelegramSender {
    #[must_use]
    pub fn new(bot_token: &str) -> Self {
        Self {
            bot: Bot::new(bot_token),
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
        let chat_id = parse_chat_id(&target.chat_id)
            .map_err(|e| kernel::Error::config_invalid(e))?;
        let reply_to: i32 = reply_to_message_id
            .parse()
            .map_err(|e| kernel::Error::config_invalid(format!("invalid message_id: {e}")))?;
        use teloxide::types::{MessageId, ReplyParameters};
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
}
