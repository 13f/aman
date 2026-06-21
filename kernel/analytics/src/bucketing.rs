// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! Time-series bucketing — split a time range into equal-width intervals.

use crate::request::TimeRange;
use crate::report::BucketValue;

/// Strategy for choosing bucket width.
#[derive(Debug, Clone, Copy)]
pub enum BucketStrategy {
    /// One bucket per hour (suitable for ≤3 day windows).
    Hourly,
    /// One bucket per day (suitable for longer windows).
    Daily,
}

impl BucketStrategy {
    /// Choose a strategy appropriate for the given time range.
    #[must_use]
    pub fn for_range(range: &TimeRange) -> Self {
        let duration_ms = range.duration_ms();
        // 3 days in ms
        if duration_ms <= 259_200_000 {
            Self::Hourly
        } else {
            Self::Daily
        }
    }

    /// Width of each bucket in milliseconds.
    #[must_use]
    pub fn bucket_width_ms(&self) -> i64 {
        match self {
            Self::Hourly => 3_600_000, // 1 hour
            Self::Daily => 86_400_000, // 24 hours
        }
    }
}

/// Generate the sequence of bucket boundaries covering `range`.
///
/// Returns a vector of `BucketValue` with `value = 0.0` — callers fill in the
/// values by aggregating over each bucket's time window.
#[must_use]
pub fn generate_buckets(range: &TimeRange, strategy: BucketStrategy) -> Vec<BucketValue> {
    let width_ms = strategy.bucket_width_ms();
    if width_ms <= 0 {
        return Vec::new();
    }
    let mut buckets = Vec::new();
    let mut cursor = range.start_ms;
    while cursor <= range.end_ms {
        buckets.push(BucketValue {
            bucket_start_ms: cursor,
            value: 0.0,
        });
        // Avoid overflow on very large ranges
        cursor = cursor.saturating_add(width_ms);
        // Safety valve for degenerate inputs
        if buckets.len() > 10_000 {
            break;
        }
    }
    buckets
}

/// Bucket a series of (timestamp_ms, value) pairs into the given buckets.
///
/// Each data point is placed in the bucket whose window `[start, start+width)`
/// contains its timestamp. Values are **averaged** within each bucket.
pub fn bucket_values(data: &[(i64, f64)], width_ms: i64, buckets: &mut [BucketValue]) {
    if width_ms <= 0 || buckets.is_empty() {
        return;
    }
    // Reset all bucket values before accumulating
    for b in buckets.iter_mut() {
        b.value = 0.0;
    }
    // Accumulate sums and counts per bucket
    let mut sums: Vec<f64> = vec![0.0; buckets.len()];
    let mut counts: Vec<usize> = vec![0; buckets.len()];

    for &(ts, val) in data {
        // Find which bucket this ts falls into
        for (i, b) in buckets.iter().enumerate() {
            let bucket_end = b.bucket_start_ms.saturating_add(width_ms);
            if ts >= b.bucket_start_ms && ts < bucket_end {
                sums[i] += val;
                counts[i] += 1;
                break;
            }
        }
    }

    for (i, b) in buckets.iter_mut().enumerate() {
        if counts[i] > 0 {
            b.value = sums[i] / counts[i] as f64;
        }
    }
}

// ---------------------------------------------------------------------------
// tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strategy_hourly_for_short_range() {
        let range = TimeRange {
            start_ms: 0,
            end_ms: 86_400_000, // 1 day
        };
        assert!(matches!(BucketStrategy::for_range(&range), BucketStrategy::Hourly));
    }

    #[test]
    fn strategy_daily_for_long_range() {
        let range = TimeRange {
            start_ms: 0,
            end_ms: 864_000_000, // 10 days
        };
        assert!(matches!(BucketStrategy::for_range(&range), BucketStrategy::Daily));
    }

    #[test]
    fn generate_hourly_buckets() {
        let range = TimeRange {
            start_ms: 0,
            end_ms: 7_200_000, // 2 hours
        };
        let buckets = generate_buckets(&range, BucketStrategy::Hourly);
        assert_eq!(buckets.len(), 3); // 0:00, 1:00, 2:00
        assert_eq!(buckets[0].bucket_start_ms, 0);
        assert_eq!(buckets[1].bucket_start_ms, 3_600_000);
        assert_eq!(buckets[2].bucket_start_ms, 7_200_000);
    }

    #[test]
    fn bucket_values_average() {
        let mut buckets = vec![
            BucketValue { bucket_start_ms: 0, value: 0.0 },
            BucketValue { bucket_start_ms: 3_600_000, value: 0.0 },
        ];
        let data = vec![
            (1_800_000, 10.0),   // midpoint of first bucket
            (1_900_000, 20.0),   // also in first bucket → avg = 15
            (5_400_000, 100.0),  // midpoint of second bucket
        ];
        bucket_values(&data, 3_600_000, &mut buckets);
        assert!((buckets[0].value - 15.0).abs() < 0.001);
        assert!((buckets[1].value - 100.0).abs() < 0.001);
    }

    #[test]
    fn empty_data_leaves_zero() {
        let mut buckets = vec![
            BucketValue { bucket_start_ms: 0, value: 1.0 },
        ];
        bucket_values(&[], 3_600_000, &mut buckets);
        assert!((buckets[0].value - 0.0).abs() < 0.001);
    }
}
