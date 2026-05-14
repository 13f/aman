use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// A deterministic clock that advances only when `tick()` is called.
/// Use in place of `SystemTime::now()` in tests for time-dependent logic
/// (rate limiting, timeouts, session expiry).
#[derive(Debug, Clone)]
pub struct DeterministicClock {
    elapsed: Duration,
}

impl DeterministicClock {
    pub fn new() -> Self {
        Self {
            elapsed: Duration::ZERO,
        }
    }

    /// Advance the clock by the given duration.
    pub fn tick(&mut self, dur: Duration) {
        self.elapsed += dur;
    }

    /// Reset the clock back to zero.
    pub fn reset(&mut self) {
        self.elapsed = Duration::ZERO;
    }

    /// Return the simulated wall clock time.
    /// Always returns a time relative to UNIX_EPOCH so callers
    /// can use it anywhere `SystemTime` is expected.
    pub fn now(&self) -> SystemTime {
        UNIX_EPOCH + self.elapsed
    }

    /// Return the elapsed duration since epoch.
    pub fn elapsed(&self) -> Duration {
        self.elapsed
    }
}

impl Default for DeterministicClock {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starts_at_epoch() {
        let clock = DeterministicClock::new();
        assert_eq!(clock.now(), UNIX_EPOCH);
    }

    #[test]
    fn tick_advances_time() {
        let mut clock = DeterministicClock::new();
        clock.tick(Duration::from_secs(60));
        assert_eq!(
            clock.now().duration_since(UNIX_EPOCH).unwrap(),
            Duration::from_secs(60)
        );
    }

    #[test]
    fn reset_returns_to_zero() {
        let mut clock = DeterministicClock::new();
        clock.tick(Duration::from_secs(120));
        clock.reset();
        assert_eq!(clock.elapsed(), Duration::ZERO);
    }

    #[test]
    fn multiple_ticks_accumulate() {
        let mut clock = DeterministicClock::new();
        clock.tick(Duration::from_secs(10));
        clock.tick(Duration::from_secs(20));
        clock.tick(Duration::from_secs(30));
        assert_eq!(clock.elapsed(), Duration::from_secs(60));
    }
}
