// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! Audit log reader — extracts operational event patterns from audit records.

use std::collections::BTreeMap;
use std::sync::Arc;

use kernel::AmanResult;

use crate::data::trace_reader::AnalyticsDataSource;
use crate::request::TimeRange;

/// Summary of audit activity within a time window.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct AuditSummary {
    /// Total audit records found.
    pub total_records: usize,
    /// Counts grouped by action (e.g. "skill.enable", "config.set").
    pub action_counts: BTreeMap<String, usize>,
    /// Counts grouped by outcome (e.g. "success", "failure").
    pub outcome_counts: BTreeMap<String, usize>,
    /// Number of distinct operators.
    pub distinct_operators: usize,
    /// Number of failure outcomes.
    pub failure_count: usize,
}

/// Query audit records and produce a summary.
pub async fn read_audit_summary(
    data_source: &Arc<dyn AnalyticsDataSource>,
    range: &TimeRange,
) -> AmanResult<AuditSummary> {
    let records = data_source
        .query_audit(range.start_ms, range.end_ms)
        .await?;

    let mut summary = AuditSummary {
        total_records: records.len(),
        ..Default::default()
    };

    let mut operators = std::collections::BTreeSet::new();
    for r in &records {
        *summary.action_counts.entry(r.action.clone()).or_insert(0) += 1;
        *summary.outcome_counts.entry(r.outcome.clone()).or_insert(0) += 1;
        operators.insert(r.operator.clone());
        if r.outcome.eq_ignore_ascii_case("failure")
            || r.outcome.eq_ignore_ascii_case("error")
        {
            summary.failure_count += 1;
        }
    }
    summary.distinct_operators = operators.len();

    Ok(summary)
}
