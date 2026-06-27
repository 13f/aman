// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! Cron event source — fires `CronTick` events on a cron schedule.
//!
//! `CronSource` is a thin [`EventSource`] registered with [`SourceRegistry`].
//! The registryʼs background poll loop drives it uniformly alongside
//! `TimerSource`, `WebhookSource`, etc. — no separate cron daemon needed.

use chrono::{DateTime, Utc};
use chrono_tz::Tz;
use cron::Schedule;
use kernel::context::SourceContext;
use kernel::event::{Event, EventType};
use kernel::source::EventSource;
use kernel::types::{HealthStatus, SourceType};
use kernel::{AmanResult, Error};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::str::FromStr;

pub struct CronSource {
    id: String,
    expression: String,
    schedule: Schedule,
    timezone: Tz,
    next_run_at: Option<DateTime<Tz>>,
    initialized: bool,
    paused: bool,
}

// ── Persistence types ──────────────────────────────────────────────

/// Serializable configuration for one cron job.
///
/// Persisted to `~/.aman/agents/{agent_key}/cron/jobs.json`.  Follows the
/// same shape as Hermesʼ `~/.hermes/cron/jobs.json` entries but keeps only
/// the fields that aman actually needs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CronJobConfig {
    /// Unique job identifier (also the `SourceRegistry` key).
    pub id: String,
    /// Human-readable label (defaults to `id` when absent).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Cron expression — 5 or 6 whitespace-separated fields.
    pub expression: String,
    /// IANA timezone name, e.g. `"Asia/Shanghai"`.  Defaults to `"UTC"`.
    #[serde(default = "default_timezone")]
    pub timezone: String,
    /// Whether this job should be auto-started on gateway restart.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// ISO-8601 creation timestamp.
    pub created_at: String,
    /// Last update timestamp (ISO-8601).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
    /// Most recent run timestamp (ISO-8601).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_run_at: Option<String>,
    /// Status of the last run: `"ok"` or `"error"`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_status: Option<String>,
    /// Error message from the last run (if any).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
}

/// Top-level JSON file persisted to
/// `~/.aman/agents/{agent_key}/cron/jobs.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CronJobsFile {
    pub jobs: Vec<CronJobConfig>,
    pub updated_at: String,
}

fn default_timezone() -> String {
    "UTC".to_owned()
}

fn default_true() -> bool {
    true
}

impl CronJobConfig {
    /// Create a new config with sensible defaults.
    ///
    /// `created_at` is set to now; `enabled` defaults to `true`;
    /// `timezone` defaults to `"UTC"`.
    #[must_use]
    pub fn new(id: impl Into<String>, expression: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            name: None,
            expression: expression.into(),
            timezone: "UTC".to_owned(),
            enabled: true,
            created_at: Utc::now().to_rfc3339(),
            updated_at: None,
            last_run_at: None,
            last_status: None,
            last_error: None,
        }
    }
}

impl CronSource {
    pub fn new(id: impl Into<String>, expression: impl Into<String>) -> AmanResult<Self> {
        let expression = expression.into();
        let normalized = normalize_expression(&expression)?;
        let schedule = Schedule::from_str(&normalized).map_err(|error| Error::ConfigInvalid {
            message: format!("invalid cron expression `{expression}`: {error}"),
        })?;
        Ok(Self {
            id: id.into(),
            expression,
            schedule,
            timezone: chrono_tz::UTC,
            next_run_at: None,
            initialized: false,
            paused: false,
        })
    }

    fn next_after(&self, from: DateTime<Tz>) -> Option<DateTime<Tz>> {
        self.schedule
            .after(&from)
            .next()
            .map(|next| next.with_timezone(&self.timezone))
    }

    fn build_event(&self, scheduled_at: DateTime<Tz>) -> Event {
        Event::new(
            self.id.clone(),
            EventType::CronTick,
            serde_json::json!({
                "cron_expression": self.expression,
                "scheduled_at": scheduled_at.to_rfc3339(),
                "timezone": self.timezone.name(),
            }),
        )
    }

    /// When the next tick is expected (RFC 3339, if known).
    #[must_use]
    pub fn next_run_rfc3339(&self) -> Option<String> {
        self.next_run_at.map(|next| next.to_rfc3339())
    }
}

fn normalize_expression(raw: &str) -> AmanResult<String> {
    let fields = raw.split_whitespace().collect::<Vec<_>>();
    match fields.len() {
        5 => Ok(format!("0 {raw}")),
        6 => Ok(raw.to_owned()),
        _ => Err(Error::ConfigInvalid {
            message: format!(
                "cron expression must have 5 or 6 fields, got {}",
                fields.len()
            ),
        }),
    }
}

#[async_trait::async_trait]
impl EventSource for CronSource {
    fn id(&self) -> &str {
        &self.id
    }

    fn source_type(&self) -> SourceType {
        SourceType::Timer
    }

    async fn init(&mut self, _ctx: SourceContext) -> AmanResult<()> {
        if self.initialized {
            return Ok(());
        }
        let now = Utc::now().with_timezone(&self.timezone);
        self.next_run_at = self.next_after(now);
        self.initialized = true;
        self.paused = false;
        Ok(())
    }

    async fn shutdown(&mut self) -> AmanResult<()> {
        self.initialized = false;
        self.paused = true;
        self.next_run_at = None;
        Ok(())
    }

    async fn poll(&mut self, _ctx: &SourceContext) -> AmanResult<Vec<Event>> {
        if !self.initialized || self.paused {
            return Ok(Vec::new());
        }

        let now = Utc::now().with_timezone(&self.timezone);

        if self.next_run_at.is_none() {
            self.next_run_at = self.next_after(now);
        }

        // Collect all due ticks, then advance past them.
        let mut due = Vec::new();
        while let Some(next) = self.next_run_at {
            if next > now {
                break;
            }
            due.push(next);
            self.next_run_at = self.next_after(next);
        }

        // Emit only the latest tick (skip catch-up).  For most agent
        // use-cases firing every missed tick on restart is undesirable.
        let events: Vec<Event> = due
            .last()
            .map(|scheduled_at| self.build_event(*scheduled_at))
            .into_iter()
            .collect();

        Ok(events)
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
        if let Some(expression) = config.get("expression").and_then(Value::as_str) {
            let normalized = normalize_expression(expression)?;
            self.schedule = Schedule::from_str(&normalized).map_err(|error| {
                Error::ConfigInvalid {
                    message: format!("invalid cron expression `{expression}`: {error}"),
                }
            })?;
            self.expression = expression.to_owned();
            self.next_run_at = self.next_after(Utc::now().with_timezone(&self.timezone));
        }
        if let Some(timezone) = config.get("timezone").and_then(Value::as_str) {
            self.timezone =
                timezone
                    .parse::<Tz>()
                    .map_err(|error| Error::ConfigInvalid {
                        message: format!("invalid timezone `{timezone}`: {error}"),
                    })?;
            self.next_run_at = self.next_after(Utc::now().with_timezone(&self.timezone));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::CronSource;
    use kernel::context::{BaseContext, SourceContext};
    use kernel::event::EventType;
    use kernel::source::EventSource;
    use kernel::types::TraceId;

    fn context() -> SourceContext {
        SourceContext {
            base: BaseContext::new(TraceId::new()),
            source_name: Some("cron:test".to_owned()),
        }
    }

    #[tokio::test]
    async fn accepts_five_field_expression() {
        let mut source = CronSource::new("cron:five", "*/5 * * * *").expect("create source");
        source.init(context()).await.expect("init");
    }

    #[tokio::test]
    async fn accepts_six_field_expression() {
        let mut source =
            CronSource::new("cron:six", "*/10 * * * * *").expect("create source");
        source.init(context()).await.expect("init");
    }

    #[tokio::test]
    async fn emits_tick_with_timezone_context() {
        let mut source =
            CronSource::new("cron:tz", "*/1 * * * * *").expect("create source");
        source
            .reconfigure(serde_json::json!({ "timezone": "Asia/Shanghai" }))
            .await
            .expect("reconfigure");
        source.init(context()).await.expect("init");

        // A per-second cron fires within a short window.
        tokio::time::sleep(tokio::time::Duration::from_millis(1100)).await;
        let events = source.poll(&context()).await.expect("poll");
        assert!(!events.is_empty(), "expected at least one CronTick");
        assert_eq!(events[0].event_type, EventType::CronTick);
        assert_eq!(
            events[0].payload.get("timezone"),
            Some(&serde_json::Value::String("Asia/Shanghai".to_owned()))
        );
    }

    #[tokio::test]
    async fn poll_skips_when_paused() {
        let mut source =
            CronSource::new("cron:paused", "*/1 * * * * *").expect("create source");
        source.init(context()).await.expect("init");
        source.pause().await.expect("pause");
        let events = source.poll(&context()).await.expect("poll");
        assert!(events.is_empty(), "paused source should emit nothing");
    }

    #[tokio::test]
    async fn shutdown_clears_state() {
        let mut source =
            CronSource::new("cron:shutdown", "*/5 * * * *").expect("create source");
        source.init(context()).await.expect("init");
        assert!(source.next_run_at.is_some());
        source.shutdown().await.expect("shutdown");
        assert!(!source.initialized);
        assert!(source.paused);
        assert!(source.next_run_at.is_none());
    }

    #[tokio::test]
    async fn reconfigure_changes_expression_and_resets_next_run() {
        // Use a 5-field expression that normalizes to "sec 0, field 2=*"
        // and a fixed-minute expression so next_run_at can never coincide.
        let mut source =
            CronSource::new("cron:reconfig", "5 * * * *").expect("create source");
        source.init(context()).await.expect("init");
        let first = source.next_run_rfc3339().expect("next run after init");

        source
            .reconfigure(serde_json::json!({ "expression": "10 * * * *" }))
            .await
            .expect("reconfigure");
        let second = source.next_run_rfc3339().expect("next run after reconfigure");
        assert_eq!(source.expression, "10 * * * *");
        // `5 * * * *` fires at xx:05 each hour, `10 * * * *` at xx:10.
        // The two times can never be equal regardless of wall-clock.
        assert!(
            first != second,
            "reconfigure should recalculate next run"
        );
    }

    #[tokio::test]
    async fn poll_returns_empty_before_due_time() {
        // A cron that fires once a day — poll immediately after init should
        // return nothing.
        let mut source =
            CronSource::new("cron:future", "0 0 1 1 *").expect("create source");
        source.init(context()).await.expect("init");
        let events = source.poll(&context()).await.expect("poll");
        assert!(events.is_empty(), "daily cron should not fire immediately");
    }
}
