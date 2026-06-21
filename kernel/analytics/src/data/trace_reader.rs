// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! Trace data reader — queries trace data via [`AnalyticsDataSource`] and
//! buckets results into time-series windows.

use async_trait::async_trait;
use kernel::trace::{TraceOutcome, TraceRecord};
use kernel::AmanResult;
use std::sync::Arc;

use crate::bucketing::{generate_buckets, BucketStrategy};
use crate::request::TimeRange;
use crate::report::BucketValue;

/// Per-bucket aggregate metrics computed from trace data.
#[derive(Debug, Clone, Default)]
pub struct TraceBucketMetrics {
    /// Total number of traces in this bucket.
    pub trace_count: u64,
    /// Number of successful traces.
    pub success_count: u64,
    /// Number of failed traces.
    pub failure_count: u64,
    /// Number of partial traces.
    pub partial_count: u64,
    /// Total errors across all traces in this bucket.
    pub error_count: u64,
    /// Total tool calls across all traces.
    pub tool_call_count: u64,
    /// Number of failed tool calls.
    pub failed_tool_call_count: u64,
    /// Sum of trace durations (for computing mean).
    pub duration_sum_ms: f64,
    /// All trace durations in this bucket (for percentile computation).
    pub durations: Vec<u64>,
    /// Sum of per-tool-call durations.
    pub tool_latency_sum_ms: f64,
    /// All tool call durations (for percentile computation).
    pub tool_latencies: Vec<u64>,
}

/// Load traces via the data source, bucket them, and compute per-bucket metrics.
///
/// Returns one [`TraceBucketMetrics`] per bucket, aligned with `buckets`.
pub async fn read_trace_buckets(
    data_source: &Arc<dyn AnalyticsDataSource>,
    agent_id: &str,
    range: &TimeRange,
    strategy: BucketStrategy,
) -> AmanResult<(Vec<BucketValue>, Vec<TraceBucketMetrics>)> {
    let buckets = generate_buckets(range, strategy);
    let width_ms = strategy.bucket_width_ms();
    let mut metrics: Vec<TraceBucketMetrics> = (0..buckets.len())
        .map(|_| TraceBucketMetrics::default())
        .collect();

    // Query traces in the time range
    let traces = data_source
        .query_traces(agent_id, range.start_ms, range.end_ms, usize::MAX)
        .await?;

    if traces.is_empty() {
        return Ok((buckets, metrics));
    }

    for trace in &traces {
        // Find the right bucket
        let Some(idx) = bucket_index(trace.started_at_ms, &buckets, width_ms) else {
            continue;
        };

        let m = &mut metrics[idx];
        m.trace_count += 1;
        match trace.outcome {
            TraceOutcome::Success => m.success_count += 1,
            TraceOutcome::Failure => m.failure_count += 1,
            TraceOutcome::Partial => m.partial_count += 1,
            TraceOutcome::Cancelled => { /* still counts as a trace */ }
        }
        m.error_count += trace.errors.len() as u64;
        m.tool_call_count += trace.tool_calls.len() as u64;
        m.failed_tool_call_count += trace
            .tool_calls
            .iter()
            .filter(|tc| !tc.success)
            .count() as u64;
        m.duration_sum_ms += trace.duration_ms as f64;
        m.durations.push(trace.duration_ms);
        for tc in &trace.tool_calls {
            m.tool_latency_sum_ms += tc.duration_ms as f64;
            m.tool_latencies.push(tc.duration_ms);
        }
    }

    Ok((buckets, metrics))
}

/// Find the bucket index for a timestamp. Returns `None` if the timestamp
/// falls outside all buckets.
fn bucket_index(ts_ms: i64, buckets: &[BucketValue], width_ms: i64) -> Option<usize> {
    if buckets.is_empty() || width_ms <= 0 {
        return None;
    }
    for (i, b) in buckets.iter().enumerate() {
        let bucket_end = b.bucket_start_ms.saturating_add(width_ms);
        if ts_ms >= b.bucket_start_ms && ts_ms < bucket_end {
            return Some(i);
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Async data source trait
// ---------------------------------------------------------------------------

/// Lightweight session record used by analytics (avoids depending on gateway).
#[derive(Debug, Clone)]
pub struct SessionAnalyticsRecord {
    pub id: String,
    pub agent_id: String,
    pub state: String,
    pub message_count: i64,
    pub created_at: i64,
    pub last_active_at: i64,
    pub session_type: String,
}

/// Lightweight audit record used by analytics.
#[derive(Debug, Clone)]
pub struct AuditAnalyticsRecord {
    pub timestamp_ms: i64,
    pub action: String,
    pub operator: String,
    pub outcome: String,
}

/// Abstraction over the data stores that analytics reads from.
///
/// Implemented by the gateway (or test harness) to avoid circular
/// dependencies between the analytics crate and the gateway crate.
#[async_trait]
pub trait AnalyticsDataSource: Send + Sync {
    /// Query trace records for an agent within a time range.
    async fn query_traces(
        &self,
        agent_id: &str,
        start_ms: i64,
        end_ms: i64,
        limit: usize,
    ) -> AmanResult<Vec<TraceRecord>>;

    /// Query session records within a time range (by `created_at`).
    async fn query_sessions(
        &self,
        start_ms: i64,
        end_ms: i64,
    ) -> AmanResult<Vec<SessionAnalyticsRecord>>;

    /// Query audit records within a time range.
    async fn query_audit(
        &self,
        start_ms: i64,
        end_ms: i64,
    ) -> AmanResult<Vec<AuditAnalyticsRecord>>;

    /// List all known agent IDs.
    async fn list_agents(&self) -> AmanResult<Vec<String>>;
}

// ---------------------------------------------------------------------------
// tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bucket_index_matches() {
        let buckets = vec![
            BucketValue { bucket_start_ms: 0, value: 0.0 },
            BucketValue { bucket_start_ms: 3_600_000, value: 0.0 },
        ];
        assert_eq!(bucket_index(1_800_000, &buckets, 3_600_000), Some(0));
        assert_eq!(bucket_index(5_400_000, &buckets, 3_600_000), Some(1));
        assert_eq!(bucket_index(7_200_000, &buckets, 3_600_000), None);
    }
}
