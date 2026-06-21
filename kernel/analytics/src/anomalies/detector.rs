// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! Anomaly detector — statistical outlier (z-score) and spike detection on
//! time-series data.

use crate::report::{Anomaly, BucketValue, Severity};
use crate::trends::metrics::TrendableMetric;

/// Configuration for anomaly detection.
#[derive(Debug, Clone)]
pub struct AnomalyConfig {
    /// Z-score threshold for Warning severity.
    pub z_warning: f64,
    /// Z-score threshold for Critical severity.
    pub z_critical: f64,
    /// Spike multiplier: value > median * multiplier → spike.
    pub spike_multiplier: f64,
    /// Minimum data points required to compute statistics.
    pub min_data_points: usize,
}

impl Default for AnomalyConfig {
    fn default() -> Self {
        Self {
            z_warning: 2.5,
            z_critical: 3.5,
            spike_multiplier: 3.0,
            min_data_points: 4,
        }
    }
}

/// Run all anomaly detectors on a metric's data.
pub fn detect_anomalies(
    metric: TrendableMetric,
    data: &[BucketValue],
    config: &AnomalyConfig,
) -> Vec<Anomaly> {
    let non_zero: Vec<&BucketValue> = data.iter().filter(|b| b.value > 0.0).collect();
    if non_zero.len() < config.min_data_points {
        return Vec::new();
    }

    let mut anomalies = Vec::new();

    anomalies.extend(z_score_anomalies(metric, &non_zero, config));
    anomalies.extend(spike_anomalies(metric, &non_zero, config));

    anomalies
}

// ---------------------------------------------------------------------------
// Z-Score Detection
// ---------------------------------------------------------------------------

/// Detect anomalies where a bucket's value is statistically distant from the
/// window's own mean.
fn z_score_anomalies(
    metric: TrendableMetric,
    data: &[&BucketValue],
    config: &AnomalyConfig,
) -> Vec<Anomaly> {
    let values: Vec<f64> = data.iter().map(|b| b.value).collect();
    let n = values.len() as f64;
    let mean = values.iter().sum::<f64>() / n;

    let variance = if values.len() <= 1 {
        1.0 // avoid division by zero; every value gets z ≈ 0
    } else {
        values.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / (values.len() - 1) as f64
    };
    let std_dev = variance.sqrt();

    let mut anomalies = Vec::new();
    for bv in data.iter() {
        let z = if std_dev < f64::EPSILON {
            0.0
        } else {
            (bv.value - mean) / std_dev
        };

        let severity = if z.abs() >= config.z_critical {
            Severity::Critical
        } else if z.abs() >= config.z_warning {
            Severity::Warning
        } else {
            continue;
        };

        let bucket_label = format_bucket_label(bv.bucket_start_ms);
        anomalies.push(Anomaly {
            metric: metric.name().to_owned(),
            severity,
            detected_at_bucket: bucket_label,
            expected_value: mean,
            actual_value: bv.value,
            z_score: Some(z),
            detail: format!(
                "{}: value {:.2} is {:.1}σ {} the mean of {:.2} (z={:.2})",
                metric.name(),
                bv.value,
                z.abs(),
                if z > 0.0 { "above" } else { "below" },
                mean,
                z
            ),
        });
    }

    anomalies
}

// ---------------------------------------------------------------------------
// Spike Detection
// ---------------------------------------------------------------------------

/// Detect spikes: a bucket whose value exceeds `multiplier × median` of its
/// ±3 neighbors.
fn spike_anomalies(
    metric: TrendableMetric,
    data: &[&BucketValue],
    config: &AnomalyConfig,
) -> Vec<Anomaly> {
    if data.len() < 3 {
        return Vec::new();
    }

    let values: Vec<f64> = data.iter().map(|b| b.value).collect();
    let mut anomalies = Vec::new();

    let window_radius = 3;
    for (i, &bv) in data.iter().enumerate() {
        // Collect neighbor values (exclude self)
        let start = i.saturating_sub(window_radius);
        let end = (i + window_radius + 1).min(values.len());
        let neighbors: Vec<f64> = values[start..end]
            .iter()
            .enumerate()
            .filter(|(j, _)| start + j != i)
            .map(|(_, &v)| v)
            .collect();

        if neighbors.len() < 2 {
            continue;
        }

        let median = median(&neighbors);
        if median < f64::EPSILON {
            continue;
        }

        let ratio = bv.value / median;
        if ratio >= config.spike_multiplier {
            let bucket_label = format_bucket_label(bv.bucket_start_ms);
            anomalies.push(Anomaly {
                metric: metric.name().to_owned(),
                severity: if ratio >= config.spike_multiplier * 2.0 {
                    Severity::Critical
                } else {
                    Severity::Warning
                },
                detected_at_bucket: bucket_label,
                expected_value: median,
                actual_value: bv.value,
                z_score: None,
                detail: format!(
                    "{}: spike at {} — value {:.2} is {:.1}× the neighbor median of {:.2}",
                    metric.name(),
                    format_bucket_label(bv.bucket_start_ms),
                    bv.value,
                    ratio,
                    median
                ),
            });
        }
    }

    anomalies
}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

fn median(sorted: &[f64]) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let mut copy: Vec<f64> = sorted.to_vec();
    copy.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let mid = copy.len() / 2;
    if copy.len().is_multiple_of(2) {
        (copy[mid - 1] + copy[mid]) / 2.0
    } else {
        copy[mid]
    }
}

/// Format a bucket start timestamp as an ISO-like label.
fn format_bucket_label(ts_ms: i64) -> String {
    let total_secs = ts_ms / 1000;
    let days = total_secs / 86_400;
    let remaining = total_secs % 86_400;
    let hours = remaining / 3_600;
    let minutes = (remaining % 3_600) / 60;

    // Days since UNIX epoch → approximate YYYY-MM-DD
    // This is a simplified conversion; for production use a proper date library.
    // Using the civil calendar approximation: 1970-01-01 + days
    let (y, m, d) = civil_from_days(days);

    if hours == 0 && minutes == 0 {
        format!("{y:04}-{m:02}-{d:02}")
    } else {
        format!("{y:04}-{m:02}-{d:02}T{hours:02}:{minutes:02}")
    }
}

/// Convert days since UNIX epoch to (year, month, day).
/// Algorithm from Howard Hinnant's civil_from_days.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u32;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

// ---------------------------------------------------------------------------
// tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn z_score_detects_outlier() {
        // 20 buckets at ~100, one extreme spike at 1000.
        // With 20 data points the outlier doesn't distort mean/σ as much,
        // so the z-score for the spike exceeds the 3.5 critical threshold.
        let data: Vec<BucketValue> = (0..20)
            .map(|i| {
                let v = if i == 10 { 1000.0 } else { 100.0 };
                BucketValue { bucket_start_ms: i * 3_600_000, value: v }
            })
            .collect();
        let refs: Vec<&BucketValue> = data.iter().collect();
        let anomalies = z_score_anomalies(TrendableMetric::TraceThroughput, &refs, &AnomalyConfig::default());
        assert!(!anomalies.is_empty());
        let critical: Vec<_> = anomalies.iter().filter(|a| a.severity == Severity::Critical).collect();
        assert!(!critical.is_empty());
    }

    #[test]
    fn z_score_no_anomalies_uniform() {
        let data: Vec<BucketValue> = (0..10)
            .map(|i| BucketValue { bucket_start_ms: i * 3_600_000, value: 100.0 })
            .collect();
        let refs: Vec<&BucketValue> = data.iter().collect();
        let anomalies = z_score_anomalies(TrendableMetric::TraceThroughput, &refs, &AnomalyConfig::default());
        // All identical → std_dev ≈ 0 → z ≈ 0 → no anomalies
        assert!(anomalies.is_empty());
    }

    #[test]
    fn spike_detection() {
        // Neighbors ~10, spike at 100
        let data: Vec<BucketValue> = vec![
            BucketValue { bucket_start_ms: 0, value: 10.0 },
            BucketValue { bucket_start_ms: 1, value: 11.0 },
            BucketValue { bucket_start_ms: 2, value: 9.0 },
            BucketValue { bucket_start_ms: 3, value: 100.0 }, // spike
            BucketValue { bucket_start_ms: 4, value: 10.0 },
            BucketValue { bucket_start_ms: 5, value: 12.0 },
            BucketValue { bucket_start_ms: 6, value: 11.0 },
        ];
        let refs: Vec<&BucketValue> = data.iter().collect();
        let anomalies = spike_anomalies(TrendableMetric::TraceThroughput, &refs, &AnomalyConfig::default());
        assert!(!anomalies.is_empty());
        assert_eq!(anomalies[0].detected_at_bucket, format_bucket_label(3));
    }

    #[test]
    fn median_odd() {
        assert!((median(&[1.0, 3.0, 2.0]) - 2.0).abs() < f64::EPSILON);
    }

    #[test]
    fn median_even() {
        assert!((median(&[1.0, 4.0, 2.0, 3.0]) - 2.5).abs() < f64::EPSILON);
    }

    #[test]
    fn median_empty() {
        assert!((median(&[]) - 0.0).abs() < f64::EPSILON);
    }
}
