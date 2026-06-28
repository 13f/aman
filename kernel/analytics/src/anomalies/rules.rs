// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! Rule-based anomaly detection — predefined threshold rules that fire when
//! operational metrics cross known-dangerous boundaries.

use crate::data::trace_reader::TraceBucketMetrics;
use crate::data::session_reader::SessionBucketMetrics;
use crate::report::{Anomaly, BucketValue, Severity};

/// A threshold-based anomaly rule.
#[derive(Debug, Clone)]
pub struct ThresholdRule {
    /// Human-readable rule name.
    pub name: &'static str,
    /// Metric name for the report.
    pub metric: &'static str,
    /// Severity when triggered.
    pub severity: Severity,
    /// The check function: returns `Some(detail)` when the rule fires.
    /// `(trace_metrics, session_metrics, bucket_label)`
    pub check: fn(&TraceBucketMetrics, &SessionBucketMetrics, &str) -> Option<String>,
}

/// Predefined threshold rules.
#[must_use]
pub fn predefined_rules() -> Vec<ThresholdRule> {
    vec![
        ThresholdRule {
            name: "error_rate_critical",
            metric: "trace_error_rate",
            severity: Severity::Critical,
            check: |t, _s, label| {
                let total = t.success_count + t.failure_count + t.partial_count;
                if total == 0 {
                    return None;
                }
                let error_rate = (t.error_count as f64) / (total as f64);
                if error_rate > 0.50 {
                    Some(format!(
                        "Critical error rate at {label}: {:.1}% of traces contain errors",
                        error_rate * 100.0
                    ))
                } else {
                    None
                }
            },
        },
        ThresholdRule {
            name: "trace_failure_spike",
            metric: "trace_success_rate",
            severity: Severity::Warning,
            check: |t, _s, label| {
                let total = t.trace_count;
                if total == 0 {
                    return None;
                }
                let failure_rate = t.failure_count as f64 / total as f64;
                if failure_rate > 0.30 {
                    Some(format!(
                        "High failure rate at {label}: {:.1}% of traces failed",
                        failure_rate * 100.0
                    ))
                } else {
                    None
                }
            },
        },
        ThresholdRule {
            name: "tool_failure_high",
            metric: "tool_failure_rate",
            severity: Severity::Warning,
            check: |t, _s, label| {
                if t.tool_call_count == 0 {
                    return None;
                }
                let rate = t.failed_tool_call_count as f64 / t.tool_call_count as f64;
                if rate > 0.25 {
                    Some(format!(
                        "Elevated tool failure rate at {label}: {:.1}% of tool calls failed",
                        rate * 100.0
                    ))
                } else {
                    None
                }
            },
        },
        ThresholdRule {
            name: "zero_throughput",
            metric: "trace_throughput",
            severity: Severity::Warning,
            check: |t, s, label| {
                if t.trace_count == 0 && s.session_count == 0 {
                    Some(format!("Zero activity at {label}: no traces or sessions"))
                } else {
                    None
                }
            },
        },
    ]
}

/// Run all threshold rules against each bucket's metrics.
pub fn run_threshold_rules(
    rules: &[ThresholdRule],
    buckets: &[BucketValue],
    trace_metrics: &[TraceBucketMetrics],
    session_metrics: &[SessionBucketMetrics],
) -> Vec<Anomaly> {
    let mut anomalies = Vec::new();

    for (i, bucket) in buckets.iter().enumerate() {
        let tm = trace_metrics.get(i);
        let sm = session_metrics.get(i);
        let label = format_bucket_label(bucket.bucket_start_ms);

        let default_tm = TraceBucketMetrics::default();
        let default_sm = SessionBucketMetrics::default();
        let tm = tm.unwrap_or(&default_tm);
        let sm = sm.unwrap_or(&default_sm);

        for rule in rules {
            if let Some(detail) = (rule.check)(tm, sm, &label) {
                anomalies.push(Anomaly {
                    metric: rule.metric.to_owned(),
                    severity: rule.severity,
                    detected_at_bucket: label.clone(),
                    expected_value: 0.0,
                    actual_value: 0.0,
                    z_score: None,
                    detail,
                });
            }
        }
    }

    anomalies
}

fn format_bucket_label(ts_ms: i64) -> String {
    bucket_label(ts_ms)
}

fn bucket_label(ts_ms: i64) -> String {
    // Same algorithm as anomalies/detector.rs
    let total_secs = ts_ms / 1000;
    let days = total_secs / 86_400;
    let remaining = total_secs % 86_400;
    let hours = remaining / 3_600;
    let minutes = (remaining % 3_600) / 60;

    let (y, m, d) = civil_from_days(days);

    if hours == 0 && minutes == 0 {
        format!("{y:04}-{m:02}-{d:02}")
    } else {
        format!("{y:04}-{m:02}-{d:02}T{hours:02}:{minutes:02}")
    }
}

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
    use crate::data::trace_reader::TraceBucketMetrics;
    use crate::data::session_reader::SessionBucketMetrics;

    #[test]
    fn error_rate_critical_fires() {
        let tm = TraceBucketMetrics {
            trace_count: 10,
            failure_count: 8,
            error_count: 20,
            ..Default::default()
        };
        let sm = SessionBucketMetrics::default();
        let rule = &predefined_rules()[0];
        let result = (rule.check)(&tm, &sm, "test");
        assert!(result.is_some());
        assert!(result.unwrap().contains("errors"));
    }

    #[test]
    fn error_rate_normal_silent() {
        let tm = TraceBucketMetrics {
            trace_count: 100,
            success_count: 95,
            failure_count: 5,
            error_count: 2,
            ..Default::default()
        };
        let sm = SessionBucketMetrics::default();
        let rule = &predefined_rules()[0];
        assert!((rule.check)(&tm, &sm, "test").is_none());
    }

    #[test]
    fn zero_throughput_fires() {
        let tm = TraceBucketMetrics::default();
        let sm = SessionBucketMetrics::default();
        let rule = &predefined_rules()[3];
        assert!((rule.check)(&tm, &sm, "test").is_some());
    }
}
