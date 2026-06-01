// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MatrixConfig {
    #[serde(default)]
    pub enabled: bool,

    /// Matrix homeserver URL (e.g. `https://matrix.org`).
    pub homeserver_url: String,

    /// Matrix username / MXID.
    pub username: String,

    /// Login password or access token.
    /// Supports `$KEYCHAIN:aman.bot.matrix.access_token`.
    pub password: String,

    /// Device display name for this session.
    #[serde(default = "default_device_name")]
    pub device_name: String,

    #[serde(default = "default_agent")]
    pub default_agent: String,
}

fn default_device_name() -> String {
    "aman-agent".to_owned()
}

fn default_agent() -> String {
    "cortana".to_owned()
}

impl Default for MatrixConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            homeserver_url: String::new(),
            username: String::new(),
            password: String::new(),
            device_name: "aman-agent".to_owned(),
            default_agent: "cortana".to_owned(),
        }
    }
}
