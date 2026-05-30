// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

use async_trait::async_trait;
use kernel::AmanResult;
use messaging_core::sender::MessageSender;
use messaging_core::types::ChatTarget;

/// Sends messages to Discord via the HTTP API.
pub struct DiscordSender {
    bot_token: String,
    http: reqwest::Client,
}

impl DiscordSender {
    #[must_use]
    pub fn new(bot_token: impl Into<String>) -> Self {
        Self {
            bot_token: bot_token.into(),
            http: reqwest::Client::new(),
        }
    }
}

#[async_trait]
impl MessageSender for DiscordSender {
    async fn send_text(&self, target: &ChatTarget, text: &str) -> AmanResult<()> {
        let url = format!(
            "https://discord.com/api/v10/channels/{}/messages",
            target.chat_id
        );
        let payload = serde_json::json!({ "content": text });
        let resp = self
            .http
            .post(&url)
            .header("Authorization", format!("Bot {}", self.bot_token))
            .json(&payload)
            .send()
            .await
            .map_err(|e| kernel::Error::Unrecoverable {
                message: format!("discord send failed: {e}"),
            })?;
        if !resp.status().is_success() {
            return Err(kernel::Error::Unrecoverable {
                message: format!("discord api error: {}", resp.status()),
            });
        }
        Ok(())
    }
}
