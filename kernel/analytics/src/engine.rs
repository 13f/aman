// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! Analytics engine implementation — orchestrates data reading, trend
//! detection, and anomaly detection into a single [`AnalysisReport`].

use std::sync::Arc;

use async_trait::async_trait;
use kernel::AmanResult;
use kernel::types::Timestamp;

use crate::anomalies::detector::{detect_anomalies, AnomalyConfig};
use crate::anomalies::rules::{predefined_rules, run_threshold_rules};
use crate::bucketing::BucketStrategy;
use crate::data::session_reader::read_session_buckets;
use crate::data::trace_reader::{read_trace_buckets, AnalyticsDataSource};
use crate::report::{AnalysisReport, ReportSummary};
use crate::trends::detector::{detect_trend, TrendConfig};
use crate::trends::metrics::TrendableMetric;
use crate::{AnalyticsEngine, AnalysisRequest};

/// Concrete implementation of the [`AnalyticsEngine`] trait.
///
/// Consumes trace, session, and audit data via an [`AnalyticsDataSource`]
/// implementation (provided by the gateway or test harness).
pub struct AnalyticsEngineImpl {
    data_source: Arc<dyn AnalyticsDataSource>,
}

impl AnalyticsEngineImpl {
    /// Create a new engine backed by the given data source.
    #[must_use]
    pub fn new(data_source: Arc<dyn AnalyticsDataSource>) -> Self {
        Self { data_source }
    }
}

#[async_trait]
impl AnalyticsEngine for AnalyticsEngineImpl {
    async fn analyze(&self, request: AnalysisRequest) -> AmanResult<AnalysisReport> {
        let range = request.time_range;
        let strategy = BucketStrategy::for_range(&range);

        // Determine which agents to query
        let agents = if request.agent_filter.is_empty() {
            self.data_source.list_agents().await?
        } else {
            request.agent_filter.clone()
        };

        let mut all_trace_buckets: Vec<_> = Vec::new();
        let mut all_trace_metrics: Vec<_> = Vec::new();
        let mut total_traces: u64 = 0;
        let mut total_success: u64 = 0;
        let mut total_durations: Vec<u64> = Vec::new();

        // Phase 1: Read trace data per agent
        for agent_id in &agents {
            let (buckets, metrics) =
                read_trace_buckets(&self.data_source, agent_id, &range, strategy).await?;
            for m in &metrics {
                total_traces += m.trace_count;
                total_success += m.success_count;
                total_durations.extend(&m.durations);
            }
            all_trace_buckets = buckets; // last agent's buckets define the timeline
            all_trace_metrics = metrics;
        }

        // Phase 2: Read session data
        let (session_buckets, session_metrics) =
            read_session_buckets(&self.data_source, &range, strategy).await?;
        let total_sessions: u64 = session_metrics.iter().map(|m| m.session_count).sum();

        // Phase 3: Detect trends (if requested)
        let mut trends = Vec::new();
        if request.wants_trends() {
            let trend_config = TrendConfig::default();

            // Trace-derived metrics
            let trace_metric_values = extract_trace_metric_values(
                &all_trace_metrics, &all_trace_buckets, strategy,
            );
            for (metric, values) in &trace_metric_values {
                if let Some(trend) = detect_trend(*metric, values, &trend_config) {
                    trends.push(trend);
                }
            }

            // Session-derived metrics
            let session_metric_values = extract_session_metric_values(
                &session_metrics, &session_buckets, strategy,
            );
            for (metric, values) in &session_metric_values {
                if let Some(trend) = detect_trend(*metric, values, &trend_config) {
                    trends.push(trend);
                }
            }
        }

        // Phase 4: Detect anomalies (if requested)
        let mut all_anomalies = Vec::new();
        if request.wants_anomalies() {
            let anomaly_config = AnomalyConfig::default();

            // Statistical anomalies on trace metrics
            let trace_metric_values = extract_trace_metric_values(
                &all_trace_metrics, &all_trace_buckets, strategy,
            );
            for (metric, values) in &trace_metric_values {
                all_anomalies.extend(detect_anomalies(*metric, values, &anomaly_config));
            }

            // Statistical anomalies on session metrics
            let session_metric_values = extract_session_metric_values(
                &session_metrics, &session_buckets, strategy,
            );
            for (metric, values) in &session_metric_values {
                all_anomalies.extend(detect_anomalies(*metric, values, &anomaly_config));
            }

            // Threshold-based rule anomalies
            let rules = predefined_rules();
            all_anomalies.extend(run_threshold_rules(
                &rules,
                &all_trace_buckets,
                &all_trace_metrics,
                &session_metrics,
            ));
        }

        // Phase 5: Compute summary
        let overall_success_rate = if total_traces > 0 {
            total_success as f64 / total_traces as f64
        } else {
            0.0
        };
        let avg_duration_ms = if !total_durations.is_empty() {
            total_durations.iter().sum::<u64>() as f64 / total_durations.len() as f64
        } else {
            0.0
        };
        let p95_duration_ms = percentile(&total_durations, 0.95);

        let critical_count = all_anomalies
            .iter()
            .filter(|a| a.severity == crate::report::Severity::Critical)
            .count();

        let summary = ReportSummary {
            total_traces,
            total_agents: agents.len() as u64,
            total_sessions,
            overall_success_rate,
            avg_duration_ms,
            p95_duration_ms,
            trend_count: trends.len(),
            anomaly_count: all_anomalies.len(),
            critical_anomaly_count: critical_count,
        };

        Ok(AnalysisReport {
            time_range: range,
            generated_at_ms: Timestamp::now().as_millis(),
            trends,
            anomalies: all_anomalies,
            summary,
        })
    }
}

// ---------------------------------------------------------------------------
// Metric extraction helpers
// ---------------------------------------------------------------------------

/// Extract per-metric time-series from trace bucket metrics.
fn extract_trace_metric_values(
    metrics: &[crate::data::trace_reader::TraceBucketMetrics],
    buckets: &[crate::report::BucketValue],
    strategy: BucketStrategy,
) -> Vec<(TrendableMetric, Vec<crate::report::BucketValue>)> {
    let mut result = Vec::new();
    let _width_ms = strategy.bucket_width_ms();

    for metric in TrendableMetric::trace_metrics() {
        let values: Vec<crate::report::BucketValue> = metrics
            .iter()
            .enumerate()
            .map(|(i, m)| {
                let bucket_start_ms = buckets.get(i).map(|b| b.bucket_start_ms).unwrap_or(0);
                let value = match metric {
                    TrendableMetric::TraceThroughput => m.trace_count as f64,
                    TrendableMetric::TraceSuccessRate => {
                        if m.trace_count > 0 {
                            m.success_count as f64 / m.trace_count as f64
                        } else {
                            0.0
                        }
                    }
                    TrendableMetric::TraceErrorRate => {
                        if m.trace_count > 0 {
                            m.error_count as f64 / m.trace_count as f64
                        } else {
                            0.0
                        }
                    }
                    TrendableMetric::TraceAvgDuration => {
                        if !m.durations.is_empty() {
                            m.duration_sum_ms / m.durations.len() as f64
                        } else {
                            0.0
                        }
                    }
                    TrendableMetric::TraceP95Duration => percentile(&m.durations, 0.95),
                    TrendableMetric::ToolFailureRate => {
                        if m.tool_call_count > 0 {
                            m.failed_tool_call_count as f64 / m.tool_call_count as f64
                        } else {
                            0.0
                        }
                    }
                    TrendableMetric::ToolAvgLatency => {
                        if !m.tool_latencies.is_empty() {
                            m.tool_latency_sum_ms / m.tool_latencies.len() as f64
                        } else {
                            0.0
                        }
                    }
                    _ => 0.0,
                };
                crate::report::BucketValue {
                    bucket_start_ms,
                    value,
                }
            })
            .collect();
        result.push((*metric, values));
    }
    result
}

fn extract_session_metric_values(
    metrics: &[crate::data::session_reader::SessionBucketMetrics],
    buckets: &[crate::report::BucketValue],
    _strategy: BucketStrategy,
) -> Vec<(TrendableMetric, Vec<crate::report::BucketValue>)> {
    let mut result = Vec::new();

    for metric in TrendableMetric::session_metrics() {
        let values: Vec<crate::report::BucketValue> = metrics
            .iter()
            .enumerate()
            .map(|(i, m)| {
                let bucket_start_ms = buckets.get(i).map(|b| b.bucket_start_ms).unwrap_or(0);
                let value = match metric {
                    TrendableMetric::SessionCount => m.session_count as f64,
                    TrendableMetric::SessionAvgMessages => {
                        if m.session_count > 0 {
                            m.total_messages as f64 / m.session_count as f64
                        } else {
                            0.0
                        }
                    }
                    _ => 0.0,
                };
                crate::report::BucketValue {
                    bucket_start_ms,
                    value,
                }
            })
            .collect();
        result.push((*metric, values));
    }
    result
}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

/// Compute a percentile from a slice of values.
fn percentile(values: &[u64], p: f64) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    let mut sorted: Vec<u64> = values.to_vec();
    sorted.sort_unstable();
    let idx = ((sorted.len() - 1) as f64 * p).round() as usize;
    sorted[idx.min(sorted.len() - 1)] as f64
}
