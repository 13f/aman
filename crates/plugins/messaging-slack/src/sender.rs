// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

use async_trait::async_trait;
use kernel::AmanResult;
use messaging_core::sender::MessageSender;
use messaging_core::types::ChatTarget;

/// Build a proxy-aware reqwest client.
/// Reads the proxy URL from (in order): `ALL_PROXY`, `HTTPS_PROXY`, `https_proxy`.
#[must_use]
pub fn build_proxy_client() -> reqwest::Client {
    let mut builder = reqwest::Client::builder();
    if let Some(proxy_url) = detect_proxy_url() {
        if let Ok(proxy) = reqwest::Proxy::all(&proxy_url) {
            builder = builder.proxy(proxy);
        }
    }
    builder.build().expect("build slack reqwest client")
}

fn detect_proxy_url() -> Option<String> {
    std::env::var("ALL_PROXY")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| std::env::var("HTTPS_PROXY").ok().filter(|s| !s.is_empty()))
        .or_else(|| std::env::var("https_proxy").ok().filter(|s| !s.is_empty()))
}

/// Sends messages back to Slack via the Web API.
pub struct SlackSender {
    bot_token: String,
    http: reqwest::Client,
}

impl SlackSender {
    #[must_use]
    pub fn new(bot_token: impl Into<String>) -> Self {
        Self {
            bot_token: bot_token.into(),
            http: build_proxy_client(),
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
