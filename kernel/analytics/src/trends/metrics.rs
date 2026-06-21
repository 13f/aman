// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! Metric definitions — which metrics the analytics engine can track.

use crate::report::{BucketValue, Trend, TrendDirection};

/// A metric that can be trended over time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TrendableMetric {
    // Trace-derived
    /// Number of traces per bucket.
    TraceThroughput,
    /// Success rate (successes / total).
    TraceSuccessRate,
    /// Error rate (traces with errors / total).
    TraceErrorRate,
    /// Average trace duration.
    TraceAvgDuration,
    /// 95th percentile trace duration.
    TraceP95Duration,
    /// Tool failure rate (failed tool calls / total tool calls).
    ToolFailureRate,
    /// Average per-tool-call latency.
    ToolAvgLatency,
    // Session-derived
    /// Number of sessions created per bucket.
    SessionCount,
    /// Average messages per session.
    SessionAvgMessages,
}

impl TrendableMetric {
    /// Human-readable metric name for reports.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::TraceThroughput => "trace_throughput",
            Self::TraceSuccessRate => "trace_success_rate",
            Self::TraceErrorRate => "trace_error_rate",
            Self::TraceAvgDuration => "trace_avg_duration_ms",
            Self::TraceP95Duration => "trace_p95_duration_ms",
            Self::ToolFailureRate => "tool_failure_rate",
            Self::ToolAvgLatency => "tool_avg_latency_ms",
            Self::SessionCount => "session_count",
            Self::SessionAvgMessages => "session_avg_message_count",
        }
    }

    /// Whether a higher value is "better" for this metric.
    /// Used to determine [`TrendDirection`].
    #[must_use]
    pub fn higher_is_better(self) -> bool {
        match self {
            Self::TraceThroughput | Self::TraceSuccessRate | Self::SessionCount => true,
            Self::TraceErrorRate
            | Self::TraceAvgDuration
            | Self::TraceP95Duration
            | Self::ToolFailureRate
            | Self::ToolAvgLatency
            | Self::SessionAvgMessages => false,
        }
    }

    /// All trace-derived metrics.
    #[must_use]
    pub fn trace_metrics() -> &'static [Self] {
        &[
            Self::TraceThroughput,
            Self::TraceSuccessRate,
            Self::TraceErrorRate,
            Self::TraceAvgDuration,
            Self::TraceP95Duration,
            Self::ToolFailureRate,
            Self::ToolAvgLatency,
        ]
    }

    /// All session-derived metrics.
    #[must_use]
    pub fn session_metrics() -> &'static [Self] {
        &[Self::SessionCount, Self::SessionAvgMessages]
    }

    /// All available metrics.
    #[must_use]
    pub fn all() -> &'static [Self] {
        &[
            Self::TraceThroughput,
            Self::TraceSuccessRate,
            Self::TraceErrorRate,
            Self::TraceAvgDuration,
            Self::TraceP95Duration,
            Self::ToolFailureRate,
            Self::ToolAvgLatency,
            Self::SessionCount,
            Self::SessionAvgMessages,
        ]
    }
}

/// Build a [`Trend`] from a metric, direction, confidence, and data.
pub fn build_trend(
    metric: TrendableMetric,
    direction: TrendDirection,
    confidence: f64,
    data_points: Vec<BucketValue>,
) -> Trend {
    let detail = trend_detail(metric, direction, confidence);
    Trend {
        metric: metric.name().to_owned(),
        direction,
        confidence,
        detail,
        data_points,
    }
}

fn trend_detail(metric: TrendableMetric, direction: TrendDirection, confidence: f64) -> String {
    let confidence_pct = (confidence * 100.0) as u32;
    let dir_word = match direction {
        TrendDirection::Improving => "improving",
        TrendDirection::Degrading => "degrading",
        TrendDirection::Stable => "stable",
    };
    format!(
        "{} is {} ({}% confidence)",
        metric.name(),
        dir_word,
        confidence_pct
    )
}
