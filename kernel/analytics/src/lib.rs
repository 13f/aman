// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

#![forbid(unsafe_code)]
#![doc = "Analytics engine — trend detection, anomaly alerting, and time-series analysis on agent operational data."]

pub mod request;
pub mod report;
pub mod bucketing;

pub mod data;
pub mod trends;
pub mod anomalies;
mod engine;

pub use engine::AnalyticsEngineImpl;
pub use request::{AnalysisRequest, AnalysisType, TimeRange};
pub use report::{AnalysisReport, Anomaly, BucketValue, ReportSummary, Severity, Trend, TrendDirection};

use async_trait::async_trait;
use kernel::AmanResult;

// ---------------------------------------------------------------------------
// AnalyticsEngine trait
// ---------------------------------------------------------------------------

/// The analytics engine consumes existing operational data (traces, sessions,
/// audit logs) and produces structured analysis reports.
///
/// # Default time range
///
/// When [`AnalysisRequest::time_range`] is not explicitly set, it defaults to
/// *today* — midnight local time to the current instant.
///
/// # Example
///
/// ```ignore
/// use analytics::{AnalyticsEngine, AnalyticsEngineImpl, AnalysisRequest};
///
/// let engine = AnalyticsEngineImpl::new(trace_store, session_store, audit_logger);
/// let report = engine.analyze(AnalysisRequest::default()).await?;
/// println!("{} trends, {} anomalies", report.trends.len(), report.anomalies.len());
/// ```
#[async_trait]
pub trait AnalyticsEngine: Send + Sync {
    /// Run analysis and return a structured report.
    async fn analyze(&self, request: AnalysisRequest) -> AmanResult<AnalysisReport>;
}
