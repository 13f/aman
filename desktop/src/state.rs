// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::process::Child;
use std::time::Duration;

use crate::gateway_client::GatewayClient;
use crate::rate_limiter::SlidingWindowRateLimiter;
use i18n::Locale;

pub struct AppState {
    pub gateway_client: Arc<Mutex<Option<GatewayClient>>>,
    /// Handle to the spawned gateway child process.
    pub gateway_process: Arc<Mutex<Option<Child>>>,
    pub rate_limiter: SlidingWindowRateLimiter,
    /// The currently active agent key (for multi-agent mode, P2+).
    pub active_agent_key: Arc<Mutex<Option<String>>>,
    /// Current UI locale loaded from config.
    pub locale: Locale,
}

impl AppState {
    #[must_use]
    pub fn new() -> Self {
        let locale = config::ConfigLoader::load(None, None)
            .map(|r| r.config.ui.locale)
            .unwrap_or_default();
        Self {
            gateway_client: Arc::new(Mutex::new(None)),
            gateway_process: Arc::new(Mutex::new(None)),
            // User-level: 10 messages per 60-second sliding window (§4.5)
            rate_limiter: SlidingWindowRateLimiter::new(Duration::from_secs(60), 10),
            active_agent_key: Arc::new(Mutex::new(None)),
            locale,
        }
    }
}

impl Default for AppState {
    fn default() -> Self {
        Self::new()
    }
}
