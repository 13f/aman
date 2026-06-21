// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! HTTP handler and data-source bridge for the analytics engine.

use std::sync::Arc;

use analytics::data::trace_reader::{
    AnalyticsDataSource, AuditAnalyticsRecord, SessionAnalyticsRecord,
};
use analytics::{AnalyticsEngine, AnalyticsEngineImpl, AnalysisReport, AnalysisRequest};
use axum::extract::State;
use axum::Json;
use kernel::trace::TraceRecord;
use kernel::AmanResult;

use super::agent_runtime::AgentRuntime;

// ---------------------------------------------------------------------------
// Data source bridge — wraps AgentRuntime to implement AnalyticsDataSource
// ---------------------------------------------------------------------------

/// Bridges the gateway's data stores to the analytics crate's data-source
/// trait, avoiding a circular dependency.
pub struct GatewayAnalyticsDataSource {
    runtime: Arc<AgentRuntime>,
}

impl GatewayAnalyticsDataSource {
    #[must_use]
    pub fn new(runtime: Arc<AgentRuntime>) -> Self {
        Self { runtime }
    }
}

#[async_trait::async_trait]
impl AnalyticsDataSource for GatewayAnalyticsDataSource {
    async fn query_traces(
        &self,
        agent_id: &str,
        start_ms: i64,
        end_ms: i64,
        limit: usize,
    ) -> AmanResult<Vec<TraceRecord>> {
        match self.runtime.trace_store_for_agent(agent_id) {
            Some(store) => store.load_by_time_range(agent_id, start_ms, end_ms, limit).await,
            None => Ok(Vec::new()),
        }
    }

    async fn query_sessions(
        &self,
        start_ms: i64,
        _end_ms: i64,
    ) -> AmanResult<Vec<SessionAnalyticsRecord>> {
        let store = match self.runtime.session_store() {
            Some(s) => s,
            None => return Ok(Vec::new()),
        };
        let all = store.list_all()?;
        Ok(all
            .into_iter()
            .filter(|s| s.created_at >= start_ms && s.created_at <= _end_ms)
            .map(|s| SessionAnalyticsRecord {
                id: s.id,
                agent_id: s.agent_id,
                state: s.state,
                message_count: s.message_count,
                created_at: s.created_at,
                last_active_at: s.last_active_at,
                session_type: s.session_type,
            })
            .collect())
    }

    async fn query_audit(
        &self,
        start_ms: i64,
        end_ms: i64,
    ) -> AmanResult<Vec<AuditAnalyticsRecord>> {
        let audit = self.runtime.audit();
        Ok(audit
            .list(None, None, Some(start_ms), Some(end_ms), 0, usize::MAX)
            .into_iter()
            .map(|r| AuditAnalyticsRecord {
                timestamp_ms: r.timestamp_ms,
                action: r.action,
                operator: r.operator,
                outcome: r.outcome,
            })
            .collect())
    }

    async fn list_agents(&self) -> AmanResult<Vec<String>> {
        let instances = self.runtime.agent_registry().list().await;
        Ok(instances.into_iter().map(|i| i.descriptor.agent_id).collect())
    }
}

// ---------------------------------------------------------------------------
// HTTP handler
// ---------------------------------------------------------------------------

/// `POST /analytics/analyze`
///
/// Accepts an [`AnalysisRequest`] JSON body and returns an [`AnalysisReport`].
pub async fn analytics_analyze(
    State(runtime): State<Arc<AgentRuntime>>,
    Json(request): Json<AnalysisRequest>,
) -> Result<Json<AnalysisReport>, (axum::http::StatusCode, String)> {
    let data_source = Arc::new(GatewayAnalyticsDataSource::new(runtime));
    let engine = AnalyticsEngineImpl::new(data_source);

    match engine.analyze(request).await {
        Ok(report) => Ok(Json(report)),
        Err(e) => Err((
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            format!("Analytics error: {e}"),
        )),
    }
}
