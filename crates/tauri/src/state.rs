use runtime::AgentRuntime;
use std::sync::Arc;
use tokio::sync::Mutex;
use std::time::Duration;

use crate::rate_limiter::SlidingWindowRateLimiter;

pub struct AppState {
    pub runtime: Arc<Mutex<Option<Arc<AgentRuntime>>>>,
    pub rate_limiter: SlidingWindowRateLimiter,
    /// The currently active agent key (for multi-agent mode, P2+).
    pub active_agent_key: Arc<Mutex<Option<String>>>,
}

impl AppState {
    #[must_use]
    pub fn new() -> Self {
        Self {
            runtime: Arc::new(Mutex::new(None)),
            // User-level: 10 messages per 60-second sliding window (§4.5)
            rate_limiter: SlidingWindowRateLimiter::new(Duration::from_secs(60), 10),
            active_agent_key: Arc::new(Mutex::new(None)),
        }
    }
}

impl Default for AppState {
    fn default() -> Self {
        Self::new()
    }
}
