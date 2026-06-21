// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! Analysis report types — the structured output of an analytics run.

use serde::{Deserialize, Serialize};

use crate::request::TimeRange;

// ---------------------------------------------------------------------------
// Trend
// ---------------------------------------------------------------------------

/// Direction of a detected trend.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrendDirection {
    /// Metric is getting better (e.g. lower latency, higher success rate).
    Improving,
    /// Metric is getting worse (e.g. higher error rate, longer durations).
    Degrading,
    /// No significant directional change detected.
    Stable,
}

/// A single detected trend on a metric.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Trend {
    /// Metric name (e.g. "trace_success_rate", "avg_tool_latency_ms").
    pub metric: String,

    /// Which direction the metric is trending.
    pub direction: TrendDirection,

    /// Confidence in the trend direction (0.0–1.0). Derived from R² or
    /// the consistency of the moving average crossing.
    pub confidence: f64,

    /// Human-readable description of the trend.
    pub detail: String,

    /// The raw bucketed time-series data backing this trend.
    /// Useful for rendering sparklines or charts.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub data_points: Vec<BucketValue>,
}

// ---------------------------------------------------------------------------
// Anomaly
// ---------------------------------------------------------------------------

/// Severity of a detected anomaly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    /// Informational — minor deviation, worth noting.
    Info,
    /// Warning — significant deviation, needs attention.
    Warning,
    /// Critical — extreme deviation, immediate action recommended.
    Critical,
}

/// A single detected anomaly.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Anomaly {
    /// Which metric exhibited the anomaly.
    pub metric: String,

    /// How severe the anomaly is.
    pub severity: Severity,

    /// Human-readable label for the time bucket where the anomaly occurred.
    /// Format depends on bucket size (e.g. "2026-06-21T14:00" for hourly).
    pub detected_at_bucket: String,

    /// The expected (baseline) value.
    pub expected_value: f64,

    /// The actual observed value.
    pub actual_value: f64,

    /// Z-score of the deviation. `None` for spike/threshold detections.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub z_score: Option<f64>,

    /// Human-readable description.
    pub detail: String,
}

// ---------------------------------------------------------------------------
// BucketValue
// ---------------------------------------------------------------------------

/// A single data point in a time-series bucket.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BucketValue {
    /// Start of the bucket (UNIX ms).
    pub bucket_start_ms: i64,
    /// Computed value for this bucket.
    pub value: f64,
}

// ---------------------------------------------------------------------------
// ReportSummary
// ---------------------------------------------------------------------------

/// High-level summary of the analysis report.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ReportSummary {
    /// Total number of traces analyzed.
    pub total_traces: u64,

    /// Total number of agents included.
    pub total_agents: u64,

    /// Number of sessions within the time window.
    pub total_sessions: u64,

    /// Overall trace success rate (0.0–1.0).
    pub overall_success_rate: f64,

    /// Average trace duration in milliseconds.
    pub avg_duration_ms: f64,

    /// P95 trace duration in milliseconds.
    pub p95_duration_ms: f64,

    /// Number of trends detected.
    pub trend_count: usize,

    /// Number of anomalies detected.
    pub anomaly_count: usize,

    /// Number of critical anomalies.
    pub critical_anomaly_count: usize,
}

// ---------------------------------------------------------------------------
// AnalysisReport
// ---------------------------------------------------------------------------

/// The complete result of an analytics run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnalysisReport {
    /// The time range that was analyzed.
    pub time_range: TimeRange,

    /// When this report was generated (UNIX ms).
    pub generated_at_ms: i64,

    /// Detected trends (empty if trend analysis was not requested).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub trends: Vec<Trend>,

    /// Detected anomalies (empty if anomaly analysis was not requested).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub anomalies: Vec<Anomaly>,

    /// High-level summary across all analyses.
    pub summary: ReportSummary,
}
