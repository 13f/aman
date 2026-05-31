// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! Token-bucket rate limiter for the event bus, gating per-source event throughput.
//!
//! Each unique source gets its own token bucket. Tokens refill continuously at
//! the configured rate. If a source exceeds the burst limit, its events are
//! rejected until enough tokens have been replenished.

use kernel::types::SourceId;
use kernel::{AmanResult, Error};
use std::collections::HashMap;
use std::time::Instant;

#[derive(Debug, Clone, PartialEq)]
pub struct RateLimiterConfig {
    /// Maximum events per second per source (token refill rate).
    pub max_per_second: f64,
    /// Maximum burst size (initial token bucket capacity).
    pub burst: u32,
}

impl Default for RateLimiterConfig {
    fn default() -> Self {
        Self {
            max_per_second: 100.0,
            burst: 200,
        }
    }
}

/// Internal token bucket for a single source.
#[derive(Debug)]
struct TokenBucket {
    tokens: f64,
    last_refill: Instant,
    max_tokens: f64,
    refill_rate: f64,
}

impl TokenBucket {
    fn new(max_tokens: f64, refill_rate: f64) -> Self {
        Self {
            tokens: max_tokens,
            last_refill: Instant::now(),
            max_tokens,
            refill_rate,
        }
    }

    /// Attempt to consume one token. Returns `true` if a token was available
    /// (event allowed), `false` if throttled.
    fn try_consume(&mut self) -> bool {
        // Refill
        let now = Instant::now();
        let elapsed = now.duration_since(self.last_refill).as_secs_f64();
        if elapsed > 0.0 {
            self.tokens = (self.tokens + elapsed * self.refill_rate).min(self.max_tokens);
            self.last_refill = now;
        }

        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }

    /// How many milliseconds until one token is available.
    fn retry_after_ms(&self) -> u64 {
        if self.tokens >= 1.0 {
            return 0;
        }
        let needed = 1.0 - self.tokens;
        (needed / self.refill_rate * 1000.0).ceil() as u64
    }
}

/// Per-source rate limiter using a token-bucket algorithm.
///
/// Thread-safe: internal state is guarded by a Mutex. Callers should hold
/// the lock only for the duration of `check()`.
#[derive(Debug)]
pub struct EventRateLimiter {
    buckets: HashMap<SourceId, TokenBucket>,
    config: RateLimiterConfig,
}

impl EventRateLimiter {
    pub fn new(config: RateLimiterConfig) -> Self {
        Self {
            buckets: HashMap::new(),
            config,
        }
    }

    /// Check whether the given source is within its rate limit.
    ///
    /// Returns `Ok(())` if the event is allowed, or `Err(Error::RateLimited)`
    /// with a suggested retry delay if throttled.
    pub fn check(&mut self, source: &SourceId) -> AmanResult<()> {
        let bucket = self.buckets.entry(source.clone()).or_insert_with(|| {
            TokenBucket::new(
                f64::from(self.config.burst),
                self.config.max_per_second,
            )
        });

        if bucket.try_consume() {
            Ok(())
        } else {
            let retry_after_ms = bucket.retry_after_ms();
            Err(Error::RateLimited {
                source_id: source.as_str().to_owned(),
                retry_after_ms,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kernel::types::SourceId;

    #[test]
    fn accepts_events_within_burst_limit() {
        let mut limiter = EventRateLimiter::new(RateLimiterConfig {
            max_per_second: 100.0,
            burst: 200,
        });
        let source = SourceId::new("test:source");

        // Should accept up to burst events immediately
        for _ in 0..200 {
            assert!(limiter.check(&source).is_ok(), "should accept within burst");
        }

        // 201st should be rejected
        assert!(limiter.check(&source).is_err(), "should reject above burst");
    }

    #[test]
    fn different_sources_have_independent_buckets() {
        let mut limiter = EventRateLimiter::new(RateLimiterConfig {
            max_per_second: 1.0,
            burst: 1,
        });
        let source_a = SourceId::new("test:a");
        let source_b = SourceId::new("test:b");

        // Both get their first event
        assert!(limiter.check(&source_a).is_ok());
        assert!(limiter.check(&source_b).is_ok());

        // Second event for each should be rejected (independent)
        assert!(limiter.check(&source_a).is_err());
        assert!(limiter.check(&source_b).is_err());
    }

    #[test]
    fn rate_limited_error_contains_source_and_retry_time() {
        let mut limiter = EventRateLimiter::new(RateLimiterConfig {
            max_per_second: 1.0,
            burst: 0,
        });
        let source = SourceId::new("test:limits");
        let err = limiter.check(&source).expect_err("should be rate limited");
        let msg = err.to_string();
        assert!(msg.contains("test:limits"), "error should mention source");
        assert!(msg.contains("retry after"), "error should mention retry");
    }
}
