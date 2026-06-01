// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! Configuration for the Slack messaging channel.

use serde::{Deserialize, Serialize};

/// Slack bot configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SlackConfig {
    /// Whether this channel is enabled. Default `false`.
    #[serde(default)]
    pub enabled: bool,

    /// Slack Bot User OAuth Token (`xoxb-...`).
    /// Supports `$KEYCHAIN:aman.bot.slack.bot_token`.
    pub bot_token: String,

    /// Slack App-Level Token for Socket Mode (`xapp-...`).
    /// Supports `$KEYCHAIN:aman.bot.slack.app_token`.
    #[serde(default)]
    pub app_token: String,

    /// Whether to use Socket Mode (websocket) instead of Events API (HTTP).
    #[serde(default = "default_socket_mode")]
    pub socket_mode: bool,

    /// Default agent to route to when no affinity exists.
    #[serde(default = "default_agent")]
    pub default_agent: String,
}

fn default_socket_mode() -> bool {
    true
}

fn default_agent() -> String {
    "cortana".to_owned()
}

impl Default for SlackConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            bot_token: String::new(),
            app_token: String::new(),
            socket_mode: true,
            default_agent: "cortana".to_owned(),
        }
    }
}
