// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! Configuration for the Telegram messaging channel.

use serde::{Deserialize, Serialize};

/// Telegram bot configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TelegramConfig {
    /// Whether this channel is enabled. Default `false`.
    #[serde(default)]
    pub enabled: bool,

    /// Telegram Bot API token (from @BotFather).
    /// Supports `$KEYCHAIN:aman.bot.telegram.token` and `$ENV:TELEGRAM_BOT_TOKEN`.
    pub bot_token: String,

    /// Bot username without `@` (e.g. "aman_agent_bot").
    #[serde(default)]
    pub bot_username: String,

    /// Optional list of allowed chat IDs. Empty = allow all.
    #[serde(default)]
    pub allowed_chat_ids: Vec<i64>,

    /// Default agent to route to when no affinity exists.
    #[serde(default = "default_agent")]
    pub default_agent: String,
}

fn default_agent() -> String {
    "cortana".to_owned()
}

impl Default for TelegramConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            bot_token: String::new(),
            bot_username: String::new(),
            allowed_chat_ids: Vec::new(),
            default_agent: "cortana".to_owned(),
        }
    }
}
