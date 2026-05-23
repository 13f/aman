// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

use std::collections::HashMap;
use std::sync::RwLock;
use std::time::{Duration, Instant};

/// Error returned by the rate limiter when the limit is exceeded.
#[derive(Debug, Clone)]
pub struct RateLimitError {
    pub retry_after_seconds: f64,
    pub message: String,
}

impl std::fmt::Display for RateLimitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

/// Sliding Window Log rate limiter (§4.5).
///
/// Maintains a sorted list of timestamps per key. On `allow()`, entries
/// outside the window are evicted, then the count is checked against
/// the limit. Returns `Ok` if under the limit, or `Err(RateLimitError)`
/// with `retry_after_seconds` set to when the oldest entry expires.
///
/// Thread-safe via `RwLock<HashMap>`. Not persisted across restarts.
pub struct SlidingWindowRateLimiter {
    window: Duration,
    max_requests: usize,
    logs: RwLock<HashMap<String, Vec<Instant>>>,
}

impl SlidingWindowRateLimiter {
    /// Create a new rate limiter with the given window and max requests per window.
    #[must_use]
    pub fn new(window: Duration, max_requests: usize) -> Self {
        Self {
            window,
            max_requests,
            logs: RwLock::new(HashMap::new()),
        }
    }

    /// Check if a request from `key` is allowed.
    ///
    /// Returns `Ok(())` if under the limit, or `Err` with `retry_after_seconds`
    /// indicating when the caller may retry.
    pub fn allow(&self, key: &str) -> Result<(), RateLimitError> {
        let now = Instant::now();
        let mut logs = self.logs.write().expect("rate limiter lock poisoned");

        let entries = logs.entry(key.to_owned()).or_insert_with(Vec::new);

        // Evict entries outside the window (they've expired).
        let cutoff = now - self.window;
        entries.retain(|t| *t > cutoff);

        if entries.len() < self.max_requests {
            entries.push(now);
            return Ok(());
        }

        // Rate limited — calculate when the oldest entry expires.
        let oldest = entries[0];
        let retry_after = oldest + self.window - now;
        let retry_after_secs = retry_after.as_secs_f64().max(0.0);

        Err(RateLimitError {
            retry_after_seconds: retry_after_secs,
            message: format!(
                "Rate limit exceeded. Try again in {:.0} seconds.",
                retry_after_secs.ceil()
            ),
        })
    }

    /// Remove all tracked entries for a key (e.g., when a session is closed).
    pub fn reset(&self, key: &str) {
        let mut logs = self.logs.write().expect("rate limiter lock poisoned");
        logs.remove(key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;

    #[test]
    fn allows_up_to_limit() {
        let limiter = SlidingWindowRateLimiter::new(Duration::from_secs(60), 3);
        assert!(limiter.allow("user_a").is_ok());
        assert!(limiter.allow("user_a").is_ok());
        assert!(limiter.allow("user_a").is_ok());
    }

    #[test]
    fn blocks_exceeding_limit() {
        let limiter = SlidingWindowRateLimiter::new(Duration::from_secs(60), 3);
        assert!(limiter.allow("user_a").is_ok());
        assert!(limiter.allow("user_a").is_ok());
        assert!(limiter.allow("user_a").is_ok());
        let err = limiter.allow("user_a").unwrap_err();
        assert!(err.retry_after_seconds > 0.0);
        assert!(err.message.contains("Rate limit exceeded"));
    }

    #[test]
    fn different_keys_independent() {
        let limiter = SlidingWindowRateLimiter::new(Duration::from_secs(60), 3);
        assert!(limiter.allow("user_a").is_ok());
        assert!(limiter.allow("user_a").is_ok());
        assert!(limiter.allow("user_a").is_ok());
        // user_b is unaffected
        assert!(limiter.allow("user_b").is_ok());
    }

    #[test]
    fn window_expires_after_duration() {
        let limiter = SlidingWindowRateLimiter::new(Duration::from_millis(50), 2);
        assert!(limiter.allow("user_a").is_ok());
        assert!(limiter.allow("user_a").is_ok());
        assert!(limiter.allow("user_a").is_err());
        thread::sleep(Duration::from_millis(60));
        // Window has slid: old entries expired
        assert!(limiter.allow("user_a").is_ok());
    }

    #[test]
    fn reset_clears_key() {
        let limiter = SlidingWindowRateLimiter::new(Duration::from_secs(60), 2);
        assert!(limiter.allow("user_a").is_ok());
        assert!(limiter.allow("user_a").is_ok());
        assert!(limiter.allow("user_a").is_err());
        limiter.reset("user_a");
        assert!(limiter.allow("user_a").is_ok());
    }
}
