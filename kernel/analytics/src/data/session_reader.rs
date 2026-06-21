// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! Session data reader — extracts session-level time-series from session records.

use std::sync::Arc;

use kernel::AmanResult;

use crate::bucketing::{generate_buckets, BucketStrategy};
use crate::data::trace_reader::AnalyticsDataSource;
use crate::request::TimeRange;
use crate::report::BucketValue;

/// Per-bucket aggregate metrics computed from session data.
#[derive(Debug, Clone, Default)]
pub struct SessionBucketMetrics {
    /// Number of sessions created in this bucket.
    pub session_count: u64,
    /// Sum of message counts across sessions created in this bucket.
    pub total_messages: u64,
    /// Sum of session durations (last_active_at - created_at) for sessions
    /// that ended within this bucket.
    pub duration_sum_ms: f64,
    /// Number of sessions that have gone inactive (for inactivity ratio).
    pub inactive_count: u64,
}

/// Load session records from the data source, bucket them by `created_at`,
/// and compute per-bucket metrics.
pub async fn read_session_buckets(
    data_source: &Arc<dyn AnalyticsDataSource>,
    range: &TimeRange,
    strategy: BucketStrategy,
) -> AmanResult<(Vec<BucketValue>, Vec<SessionBucketMetrics>)> {
    let buckets = generate_buckets(range, strategy);
    let width_ms = strategy.bucket_width_ms();
    let mut metrics: Vec<SessionBucketMetrics> = (0..buckets.len())
        .map(|_| SessionBucketMetrics::default())
        .collect();

    let sessions = data_source
        .query_sessions(range.start_ms, range.end_ms)
        .await?;

    if sessions.is_empty() {
        return Ok((buckets, metrics));
    }

    for session in &sessions {
        let Some(idx) = bucket_index(session.created_at, &buckets, width_ms) else {
            continue;
        };

        let m = &mut metrics[idx];
        m.session_count += 1;
        m.total_messages += session.message_count.max(0) as u64;
        let duration = (session.last_active_at - session.created_at).max(0);
        m.duration_sum_ms += duration as f64;

        // Consider a session "inactive" if it has been >24h since last activity
        // and it's not currently within the last bucket
        let last_bucket_start = buckets.last().map(|b| b.bucket_start_ms).unwrap_or(0);
        if session.last_active_at < last_bucket_start.saturating_sub(86_400_000) {
            m.inactive_count += 1;
        }
    }

    Ok((buckets, metrics))
}

/// Find the bucket index for a timestamp.
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
