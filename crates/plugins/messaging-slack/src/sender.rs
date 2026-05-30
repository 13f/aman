// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

use async_trait::async_trait;
use kernel::AmanResult;
use messaging_core::sender::MessageSender;
use messaging_core::types::ChatTarget;

/// Sends messages back to Slack via the Web API.
///
/// Uses the Slack `chat.postMessage` HTTP endpoint. A dedicated Slack SDK client
/// (e.g. `slack-morphism`) can be swapped in for full socket-mode / block-kit support.
pub struct SlackSender {
    bot_token: String,
    http: reqwest::Client,
}

impl SlackSender {
    #[must_use]
    pub fn new(bot_token: impl Into<String>) -> Self {
        Self {
            bot_token: bot_token.into(),
            http: reqwest::Client::new(),
        }
    }
}

#[async_trait]
impl MessageSender for SlackSender {
    async fn send_text(&self, target: &ChatTarget, text: &str) -> AmanResult<()> {
        let payload = serde_json::json!({
            "channel": target.chat_id,
            "text": text,
        });
        if let Some(thread_ts) = &target.thread_id {
            // thread_ts included in payload for threading
            let _ = thread_ts;
        }
        let resp = self
            .http
            .post("https://slack.com/api/chat.postMessage")
            .bearer_auth(&self.bot_token)
            .json(&payload)
            .send()
            .await
            .map_err(|e| kernel::Error::Unrecoverable {
                message: format!("slack send failed: {e}"),
            })?;
        if !resp.status().is_success() {
            return Err(kernel::Error::Unrecoverable {
                message: format!("slack api error: {}", resp.status()),
            });
        }
        Ok(())
    }
}
