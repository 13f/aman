// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscordConfig {
    #[serde(default)]
    pub enabled: bool,

    /// Discord bot token. Supports `$KEYCHAIN:aman.bot.discord.token`.
    pub bot_token: String,

    /// Optional list of allowed guild IDs. Empty = allow all.
    #[serde(default)]
    pub allowed_guild_ids: Vec<String>,

    #[serde(default = "default_agent")]
    pub default_agent: String,
}

fn default_agent() -> String {
    "cortana".to_owned()
}

impl Default for DiscordConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            bot_token: String::new(),
            allowed_guild_ids: Vec::new(),
            default_agent: "cortana".to_owned(),
        }
    }
}
