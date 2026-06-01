// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

use kernel::context::SourceContext;
use kernel::event::{Event, EventType};
use kernel::source::EventSource;
use kernel::types::{HealthStatus, SourceType};
use kernel::{AmanResult, Error};
use serde_json::Value;
use tokio::time::{interval, Duration, MissedTickBehavior};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TimerCatchUp {
    Skip,
}

impl TimerCatchUp {
    fn from_value(value: Option<&str>) -> Self {
        match value {
            Some("skip") | None => Self::Skip,
            Some(_) => Self::Skip,
        }
    }
}

pub struct TimerSource {
    id: String,
    interval_ms: u64,
    heartbeat: bool,
    catch_up: TimerCatchUp,
    ticker: Option<tokio::time::Interval>,
    paused: bool,
    initialized: bool,
}

impl TimerSource {
    #[must_use]
    pub fn new(id: impl Into<String>, interval_ms: u64, heartbeat: bool) -> Self {
        Self {
            id: id.into(),
            interval_ms,
            heartbeat,
            catch_up: TimerCatchUp::Skip,
            ticker: None,
            paused: false,
            initialized: false,
        }
    }

    fn build_interval(&self) -> tokio::time::Interval {
        let mut ticker = interval(Duration::from_millis(self.interval_ms.max(1)));
        if self.catch_up == TimerCatchUp::Skip {
            ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
        }
        ticker
    }

    fn parse_reconfigure(config: &Value) -> AmanResult<(Option<u64>, Option<bool>, Option<TimerCatchUp>)> {
        let interval_ms = match config.get("interval_ms").and_then(Value::as_u64) {
            Some(0) => {
                return Err(Error::config_invalid(
                    "`interval_ms` must be greater than zero",
                ));
            }
            value => value,
        };

        let heartbeat = config.get("heartbeat").and_then(Value::as_bool);
        let catch_up = config
            .get("catch_up")
            .and_then(Value::as_str)
            .map(|mode| TimerCatchUp::from_value(Some(mode)));

        Ok((interval_ms, heartbeat, catch_up))
    }
}

#[async_trait::async_trait]
impl EventSource for TimerSource {
    fn id(&self) -> &str {
        &self.id
    }

    fn source_type(&self) -> SourceType {
        SourceType::Timer
    }

    async fn init(&mut self, _ctx: SourceContext) -> AmanResult<()> {
        self.ticker = Some(self.build_interval());
        self.initialized = true;
        self.paused = false;
        Ok(())
    }

    async fn shutdown(&mut self) -> AmanResult<()> {
        self.paused = true;
        self.initialized = false;
        self.ticker = None;
        Ok(())
    }

    async fn poll(&mut self, _ctx: &SourceContext) -> AmanResult<Vec<Event>> {
        if !self.initialized || self.paused {
            return Ok(Vec::new());
        }

        if self.ticker.is_none() {
            self.ticker = Some(self.build_interval());
        }

        if let Some(ticker) = self.ticker.as_mut() {
            ticker.tick().await;
        }

        let event_type = if self.heartbeat {
            EventType::Heartbeat
        } else {
            EventType::TimerTick
        };
        let payload = serde_json::json!({
            "source_kind": "timer",
            "interval_ms": self.interval_ms,
            "heartbeat": self.heartbeat,
        });
        Ok(vec![Event::new(self.id.clone(), event_type, payload)])
    }

    fn health(&self) -> HealthStatus {
        if self.initialized {
            HealthStatus::Ok
        } else {
            HealthStatus::Degraded
        }
    }

    async fn pause(&mut self) -> AmanResult<()> {
        self.paused = true;
        Ok(())
    }

    async fn resume(&mut self) -> AmanResult<()> {
        if self.initialized {
            self.paused = false;
        }
        Ok(())
    }

    async fn reconfigure(&mut self, config: Value) -> AmanResult<()> {
        let (interval_ms, heartbeat, catch_up) = Self::parse_reconfigure(&config)?;

        if let Some(interval_ms) = interval_ms {
            self.interval_ms = interval_ms;
        }
        if let Some(heartbeat) = heartbeat {
            self.heartbeat = heartbeat;
        }
        if let Some(catch_up) = catch_up {
            self.catch_up = catch_up;
        }

        if self.initialized {
            self.ticker = Some(self.build_interval());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::TimerSource;
    use kernel::context::{BaseContext, SourceContext};
    use kernel::event::EventType;
    use kernel::source::EventSource;
    use kernel::types::TraceId;
    use tokio::time::{Duration, Instant};

    fn context() -> SourceContext {
        SourceContext {
            base: BaseContext::new(TraceId::new()),
            source_name: Some("timer:test".to_owned()),
        }
    }

    #[tokio::test]
    async fn interval_ticks_with_expected_precision() {
        let mut source = TimerSource::new("timer:test", 30, false);
        let ctx = context();
        source.init(ctx.clone()).await.expect("init");

        // `tokio::time::interval` first tick is immediate; measure the next one.
        let _ = source.poll(&ctx).await.expect("warm-up poll");
        let start = Instant::now();
        let first = source.poll(&ctx).await.expect("measured poll");
        let elapsed = start.elapsed();
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].event_type, EventType::TimerTick);
        assert!(
            elapsed >= Duration::from_millis(20),
            "tick should not fire immediately: {elapsed:?}"
        );
        assert!(
            elapsed <= Duration::from_millis(200),
            "tick should remain within a reasonable range: {elapsed:?}"
        );

        source.shutdown().await.expect("shutdown");
    }

    #[tokio::test]
    async fn reconfigure_changes_event_shape() {
        let mut source = TimerSource::new("timer:test", 20, false);
        let ctx = context();
        source.init(ctx.clone()).await.expect("init");

        source
            .reconfigure(serde_json::json!({
                "interval_ms": 10,
                "heartbeat": true,
                "catch_up": "skip"
            }))
            .await
            .expect("reconfigure");
        let event = source.poll(&ctx).await.expect("poll");
        assert_eq!(event[0].event_type, EventType::Heartbeat);
    }
}
