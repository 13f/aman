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
    builder.build().expect("build matrix reqwest client")
}

fn detect_proxy_url() -> Option<String> {
    std::env::var("ALL_PROXY")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| std::env::var("HTTPS_PROXY").ok().filter(|s| !s.is_empty()))
        .or_else(|| std::env::var("https_proxy").ok().filter(|s| !s.is_empty()))
        .map(|url| {
            if url.starts_with("socks5://") && !url.starts_with("socks5h://") {
                url.replacen("socks5://", "socks5h://", 1)
            } else {
                url
            }
        })
}

/// Sends messages to Matrix via the Client-Server HTTP API.
pub struct MatrixSender {
    homeserver_url: String,
    access_token: String,
    http: reqwest::Client,
}

impl MatrixSender {
    #[must_use]
    pub fn new(homeserver_url: impl Into<String>, access_token: impl Into<String>) -> Self {
        Self {
            homeserver_url: homeserver_url.into(),
            access_token: access_token.into(),
            http: build_proxy_client(),
        }
    }
}

#[async_trait]
impl MessageSender for MatrixSender {
    async fn send_text(&self, target: &ChatTarget, text: &str) -> AmanResult<()> {
        let txn_id = uuid::Uuid::now_v7().to_string();
        let url = format!(
            "{}/_matrix/client/v3/rooms/{}/send/m.room.message/{}",
            self.homeserver_url.trim_end_matches('/'),
            target.chat_id,
            txn_id,
        );
        let payload = serde_json::json!({
            "msgtype": "m.text",
            "body": text,
        });
        let resp = self
            .http
            .put(&url)
            .bearer_auth(&self.access_token)
            .json(&payload)
            .send()
            .await
            .map_err(|e| kernel::Error::Unrecoverable {
                message: format!("matrix send failed: {e}"),
            })?;
        if !resp.status().is_success() {
            return Err(kernel::Error::Unrecoverable {
                message: format!("matrix api error: {}", resp.status()),
            });
        }
        Ok(())
    }
}
