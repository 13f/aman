// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! Analysis request types — time range, analysis selection, agent filtering.

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// TimeRange
// ---------------------------------------------------------------------------

/// A time window for analysis, specified in milliseconds since UNIX epoch.
///
/// Both bounds are inclusive. Use [`TimeRange::today`] for the default
/// "midnight local time → now" window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimeRange {
    /// Inclusive start of the window (UNIX ms).
    pub start_ms: i64,
    /// Inclusive end of the window (UNIX ms).
    pub end_ms: i64,
}

impl TimeRange {
    /// Create a time range covering today from 00:00:00 local time to now.
    ///
    /// Falls back to the last 24 hours if the local time offset cannot be
    /// determined.
    #[must_use]
    pub fn today() -> Self {
        let now_ms = now_millis();
        let midnight_ms = midnight_local(now_ms);
        Self {
            start_ms: midnight_ms,
            end_ms: now_ms,
        }
    }

    /// Validate that `start_ms <= end_ms` and both are positive.
    pub fn validate(&self) -> Result<(), String> {
        if self.start_ms <= 0 {
            return Err("start_ms must be positive".into());
        }
        if self.end_ms <= 0 {
            return Err("end_ms must be positive".into());
        }
        if self.start_ms > self.end_ms {
            return Err(format!(
                "start_ms ({}) must be <= end_ms ({})",
                self.start_ms, self.end_ms
            ));
        }
        Ok(())
    }

    /// Duration of the window in milliseconds.
    #[must_use]
    pub fn duration_ms(&self) -> i64 {
        (self.end_ms - self.start_ms).max(0)
    }
}

// ---------------------------------------------------------------------------
// AnalysisType
// ---------------------------------------------------------------------------

/// What kind of analysis to perform.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnalysisType {
    /// Detect directional trends in operational metrics.
    Trends,
    /// Detect statistical anomalies and spikes.
    Anomalies,
}

// ---------------------------------------------------------------------------
// AnalysisRequest
// ---------------------------------------------------------------------------

/// A request to the analytics engine.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnalysisRequest {
    /// Time window to analyze. Defaults to today if omitted.
    #[serde(default = "TimeRange::today")]
    pub time_range: TimeRange,

    /// Which agents to include. Empty = all agents.
    #[serde(default)]
    pub agent_filter: Vec<String>,

    /// Which analyses to run. Empty = all types.
    #[serde(default)]
    pub analyses: Vec<AnalysisType>,
}

impl Default for AnalysisRequest {
    fn default() -> Self {
        Self {
            time_range: TimeRange::today(),
            agent_filter: Vec::new(),
            analyses: Vec::new(),
        }
    }
}

impl AnalysisRequest {
    /// Should trend analysis be performed?
    #[must_use]
    pub fn wants_trends(&self) -> bool {
        self.analyses.is_empty() || self.analyses.contains(&AnalysisType::Trends)
    }

    /// Should anomaly detection be performed?
    #[must_use]
    pub fn wants_anomalies(&self) -> bool {
        self.analyses.is_empty() || self.analyses.contains(&AnalysisType::Anomalies)
    }
}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

/// Current time in milliseconds since UNIX epoch.
fn now_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Compute the local midnight (00:00:00) timestamp for the day containing
/// `now_ms`.
///
/// Uses UTC (start of current UTC day). Local timezone support can be added
/// as a follow-up by pulling in `chrono` or reading the TZ environment variable.
fn midnight_local(now_ms: i64) -> i64 {
    let day_ms = 86_400_000i64;
    let days_since_epoch = now_ms / day_ms;
    days_since_epoch * day_ms
}

// ---------------------------------------------------------------------------
// tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn time_range_today() {
        let tr = TimeRange::today();
        assert!(tr.start_ms > 0);
        assert!(tr.end_ms > 0);
        assert!(tr.start_ms <= tr.end_ms);
        // Window should be less than 24h (today only)
        assert!(tr.duration_ms() <= 86_400_000);
    }

    #[test]
    fn time_range_validate_ok() {
        let tr = TimeRange {
            start_ms: 1000,
            end_ms: 2000,
        };
        assert!(tr.validate().is_ok());
    }

    #[test]
    fn time_range_validate_reversed() {
        let tr = TimeRange {
            start_ms: 2000,
            end_ms: 1000,
        };
        assert!(tr.validate().is_err());
    }

    #[test]
    fn time_range_duration() {
        let tr = TimeRange {
            start_ms: 1000,
            end_ms: 5000,
        };
        assert_eq!(tr.duration_ms(), 4000);
    }

    #[test]
    fn default_request_is_today() {
        let req = AnalysisRequest::default();
        let tr = req.time_range;
        assert!(tr.start_ms <= tr.end_ms);
        assert!(tr.duration_ms() <= 86_400_000);
    }

    #[test]
    fn wants_all_when_empty() {
        let req = AnalysisRequest::default();
        assert!(req.wants_trends());
        assert!(req.wants_anomalies());
    }

    #[test]
    fn wants_specific() {
        let req = AnalysisRequest {
            analyses: vec![AnalysisType::Trends],
            ..Default::default()
        };
        assert!(req.wants_trends());
        assert!(!req.wants_anomalies());
    }
}
