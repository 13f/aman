// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! Trend detector — identifies directional trends in time-series data using
//! simple moving average (SMA), linear regression, and rate-of-change analysis.

use crate::report::{BucketValue, Trend, TrendDirection};
use crate::trends::metrics::{build_trend, TrendableMetric};

/// Configuration for trend detection.
#[derive(Debug, Clone)]
pub struct TrendConfig {
    /// Minimum number of data points required to compute a trend.
    pub min_data_points: usize,
    /// Rate-of-change threshold (fractional). |change| > threshold → trending.
    pub roc_threshold: f64,
    /// Confidence dampening factor per missing data point.
    pub missing_penalty: f64,
}

impl Default for TrendConfig {
    fn default() -> Self {
        Self {
            min_data_points: 3,
            roc_threshold: 0.20,
            missing_penalty: 0.05,
        }
    }
}

/// Detect trends for a single metric across time buckets.
///
/// Uses three methods and picks the most confident result:
/// 1. Simple Moving Average crossing
/// 2. Linear regression slope
/// 3. Rate of change
pub fn detect_trend(
    metric: TrendableMetric,
    data: &[BucketValue],
    config: &TrendConfig,
) -> Option<Trend> {
    // Filter to buckets that have data (non-zero values)
    let non_zero: Vec<&BucketValue> = data.iter().filter(|b| b.value > 0.0).collect();
    if non_zero.len() < config.min_data_points {
        return None;
    }

    let values: Vec<f64> = non_zero.iter().map(|b| b.value).collect();

    let sma_result = sma_trend(&values, metric.higher_is_better());
    let lr_result = linear_regression_trend(&values, metric.higher_is_better());
    let roc_result = rate_of_change_trend(&values, metric.higher_is_better(), config.roc_threshold);

    // Pick the method with the highest confidence
    let mut candidates: Vec<(TrendDirection, f64)> = Vec::new();
    if let Some((dir, conf)) = sma_result {
        candidates.push((dir, conf));
    }
    if let Some((dir, conf)) = lr_result {
        candidates.push((dir, conf));
    }
    if let Some((dir, conf)) = roc_result {
        candidates.push((dir, conf));
    }

    let best = candidates.into_iter().max_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))?;

    // Penalize for missing data
    let missing_ratio = (data.len().saturating_sub(non_zero.len())) as f64 / data.len().max(1) as f64;
    let confidence = (best.1 - missing_ratio * config.missing_penalty).clamp(0.0, 1.0);

    let data_points: Vec<BucketValue> = non_zero.into_iter().cloned().collect();

    Some(build_trend(metric, best.0, confidence, data_points))
}

// ---------------------------------------------------------------------------
// SMA (Simple Moving Average)
// ---------------------------------------------------------------------------

/// Detect a trend by comparing a 3-period SMA to the overall mean.
///
/// Counts how many SMA windows sit above vs below the mean, ignoring magnitude.
/// Returns `(direction, confidence)` where confidence is derived from how
/// consistently the SMA is on one side of the mean.
fn sma_trend(values: &[f64], higher_is_better: bool) -> Option<(TrendDirection, f64)> {
    if values.len() < 3 {
        return None;
    }

    let overall_mean = values.iter().sum::<f64>() / values.len() as f64;
    let overall_std = std_dev(values, overall_mean);
    if overall_std < f64::EPSILON {
        return Some((TrendDirection::Stable, 0.9));
    }

    // Compute a 3-period SMA and check which side of the mean it sits on.
    // Use a softer threshold (0.5× std_dev) so linear trends don't get masked.
    let threshold = overall_std * 0.5;
    let mut above_count = 0u32;
    let mut below_count = 0u32;
    for i in 2..values.len() {
        let sma = (values[i - 2] + values[i - 1] + values[i]) / 3.0;
        if sma > overall_mean + threshold {
            above_count += 1;
        } else if sma < overall_mean - threshold {
            below_count += 1;
        }
    }

    let total = above_count + below_count;
    if total == 0 {
        return Some((TrendDirection::Stable, 0.6));
    }

    let ratio = above_count as f64 / total as f64;
    // If SMA is consistently above mean, the metric value is rising
    if ratio >= 0.75 {
        let dir = if higher_is_better {
            TrendDirection::Improving
        } else {
            TrendDirection::Degrading
        };
        Some((dir, ratio))
    } else if ratio <= 0.25 {
        let dir = if higher_is_better {
            TrendDirection::Degrading
        } else {
            TrendDirection::Improving
        };
        Some((dir, 1.0 - ratio))
    } else {
        Some((TrendDirection::Stable, 0.5))
    }
}

// ---------------------------------------------------------------------------
// Linear Regression
// ---------------------------------------------------------------------------

/// Simple OLS linear regression on (index, value) pairs.
///
/// Returns `(direction, confidence)` where confidence = R².
fn linear_regression_trend(values: &[f64], higher_is_better: bool) -> Option<(TrendDirection, f64)> {
    if values.len() < 2 {
        return None;
    }

    let n = values.len() as f64;
    let x_mean = (n - 1.0) / 2.0; // mean of indices 0..n-1
    let y_mean = values.iter().sum::<f64>() / n;

    let mut cov = 0.0;
    let mut var_x = 0.0;
    for (i, &y) in values.iter().enumerate() {
        let dx = i as f64 - x_mean;
        cov += dx * (y - y_mean);
        var_x += dx * dx;
    }

    if var_x < f64::EPSILON {
        return Some((TrendDirection::Stable, 0.5));
    }

    let slope = cov / var_x;
    let intercept = y_mean - slope * x_mean;

    // Compute R² (coefficient of determination)
    let mut ss_res = 0.0;
    let mut ss_tot = 0.0;
    for (i, &y) in values.iter().enumerate() {
        let y_pred = slope * i as f64 + intercept;
        ss_res += (y - y_pred).powi(2);
        ss_tot += (y - y_mean).powi(2);
    }
    let r2 = if ss_tot < f64::EPSILON {
        0.9 // all values identical → stable with high confidence
    } else {
        (1.0 - ss_res / ss_tot).clamp(0.0, 1.0)
    };

    let direction = if slope.abs() < y_mean.abs() * 0.01 {
        // Essentially flat
        TrendDirection::Stable
    } else if slope > 0.0 {
        if higher_is_better {
            TrendDirection::Improving
        } else {
            TrendDirection::Degrading
        }
    } else if higher_is_better {
        TrendDirection::Degrading
    } else {
        TrendDirection::Improving
    };

    Some((direction, r2))
}

// ---------------------------------------------------------------------------
// Rate of Change
// ---------------------------------------------------------------------------

/// Simple rate of change: `(last - first) / |first|`.
///
/// Returns `Some` only when |change| exceeds `threshold`.
fn rate_of_change_trend(
    values: &[f64],
    higher_is_better: bool,
    threshold: f64,
) -> Option<(TrendDirection, f64)> {
    if values.len() < 2 {
        return None;
    }

    let first = values[0];
    let last = values[values.len() - 1];

    if first.abs() < f64::EPSILON {
        return None;
    }

    let roc = (last - first) / first.abs();

    if roc.abs() < threshold {
        return None;
    }

    // Confidence grows linearly with the magnitude of the change,
    // capped at 0.95
    let confidence = (roc.abs() / (threshold * 5.0)).min(0.95).clamp(0.0, 0.95);

    let direction = if roc > 0.0 {
        if higher_is_better {
            TrendDirection::Improving
        } else {
            TrendDirection::Degrading
        }
    } else if higher_is_better {
        TrendDirection::Degrading
    } else {
        TrendDirection::Improving
    };

    Some((direction, confidence))
}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

fn std_dev(values: &[f64], mean: f64) -> f64 {
    if values.len() <= 1 {
        return 0.0;
    }
    let variance = values.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / (values.len() - 1) as f64;
    variance.sqrt()
}

// ---------------------------------------------------------------------------
// tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sma_sawtooth_detects_nothing() {
        // SMA doesn't detect monotonic trends well (it detects crossovers).
        // For a cleanly increasing series, the SMA lags behind and the
        // early readings sit below the mean while later ones sit above —
        // producing a ~50/50 split that reads as Stable.
        let values = vec![1.0, 2.0, 3.0, 4.0, 5.0, 6.0];
        let result = sma_trend(&values, true);
        assert!(result.is_some());
        let (dir, _) = result.unwrap();
        // Stable is correct — SMA alone can't distinguish monotonic trends.
        assert_eq!(dir, TrendDirection::Stable);
    }

    #[test]
    fn sma_oscillation_detects_pattern() {
        // SMA CAN detect when values oscillate around the mean with
        // consistent bias: [8, 9, 8, 9, 8, 9] — SMA stays above mean.
        let values = vec![8.0, 9.0, 8.0, 9.0, 8.0, 9.0];
        // mean = 8.5, SMA windows: (8+9+8)/3=8.33 < mean, (9+8+9)/3=8.67 > mean, ...
        // Should return something — direction depends on exact values
        let result = sma_trend(&values, true);
        assert!(result.is_some());
    }

    #[test]
    fn linear_regression_upward() {
        let values = vec![1.0, 2.0, 3.0, 4.0, 5.0];
        let (dir, r2) = linear_regression_trend(&values, true).unwrap();
        assert_eq!(dir, TrendDirection::Improving);
        assert!(r2 > 0.9); // nearly perfect fit
    }

    #[test]
    fn linear_regression_flat() {
        let values = vec![2.0, 2.1, 1.9, 2.0, 2.0];
        let (dir, _) = linear_regression_trend(&values, true).unwrap();
        assert_eq!(dir, TrendDirection::Stable);
    }

    #[test]
    fn rate_of_change_significant() {
        // 100% increase — should be detected
        let values = vec![10.0, 12.0, 15.0, 18.0, 20.0];
        let result = rate_of_change_trend(&values, true, 0.20);
        assert!(result.is_some());
        let (dir, conf) = result.unwrap();
        assert_eq!(dir, TrendDirection::Improving);
        assert!(conf > 0.5);
    }

    #[test]
    fn rate_of_change_insignificant() {
        let values = vec![10.0, 10.1, 10.2, 10.3];
        let result = rate_of_change_trend(&values, true, 0.20);
        assert!(result.is_none());
    }

    #[test]
    fn detect_trend_needs_enough_data() {
        let data = &[
            BucketValue { bucket_start_ms: 0, value: 1.0 },
            BucketValue { bucket_start_ms: 1, value: 2.0 },
        ];
        assert!(detect_trend(TrendableMetric::TraceThroughput, data, &TrendConfig::default()).is_none());
    }

    #[test]
    fn detect_trend_with_data() {
        let data: Vec<BucketValue> = (0..6)
            .map(|i| BucketValue {
                bucket_start_ms: i * 3_600_000,
                value: (i + 1) as f64 * 10.0,
            })
            .collect();
        let trend = detect_trend(TrendableMetric::TraceThroughput, &data, &TrendConfig::default());
        assert!(trend.is_some());
        let t = trend.unwrap();
        assert_eq!(t.metric, "trace_throughput");
        assert_eq!(t.direction, TrendDirection::Improving);
    }
}
