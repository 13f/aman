// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! HTTP API routes for the team plugin.
//!
//! Architecture ref: docs/team-architect.md §13

use crate::scheduler::TeamScheduler;
use crate::store::{HumanDecision, TeamStore};
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use work::{WorkItem, WorkItemId};

/// Shared state for all team API handlers.
#[derive(Clone)]
pub struct TeamApiState {
    pub team_id: String,
    pub store: Arc<TeamStore>,
    pub scheduler: Arc<TeamScheduler>,
    /// If set, the handler can list agent info from the registry.
    pub agent_registry: Option<Arc<dyn crate::AgentRegistryAccess>>,
}

/// Build the axum Router for team endpoints.
pub fn team_api_routes(state: TeamApiState) -> Router {
    Router::new()
        // Work items
        .route("/team/{team_id}/tasks", get(list_tasks).post(create_task))
        .route("/team/{team_id}/tasks/{id}/assign", post(assign_task))
        .route("/team/{team_id}/tasks/{id}/complete", post(complete_task))
        // Safety gates
        .route("/team/{team_id}/safety/pending", get(pending_gates))
        .route("/team/{team_id}/safety/{id}/resolve", post(resolve_gate))
        // Context
        .route("/team/{team_id}/context", get(list_context))
        .route("/team/{team_id}/context/{id}", get(get_context))
        // Agent status
        .route("/team/{team_id}/agents", get(agent_status))
        .with_state(state)
}

// ---------------------------------------------------------------------------
// Request / Response types
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct CreateTaskRequest {
    title: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    priority: Option<String>,
}

#[derive(Debug, Serialize)]
struct TaskResponse {
    id: String,
    title: String,
    description: String,
    stage: String,
}

#[derive(Debug, Deserialize)]
struct CompleteTaskRequest {
    confidence: Option<f64>,
}

#[derive(Debug, Deserialize)]
struct ResolveGateRequest {
    decision: String, // "approved" or "denied"
    decided_by: String,
}

#[derive(Debug, Serialize)]
pub struct AgentStatusResponse {
    agent_id: String,
    queue_length: usize,
    queue_max: usize,
    capabilities: Vec<String>,
    autonomy: String,
    allowed_stages: Vec<String>,
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// POST /team/{team_id}/tasks — create a work item
async fn create_task(
    Path(team_id): Path<String>,
    State(state): State<TeamApiState>,
    Json(req): Json<CreateTaskRequest>,
) -> impl IntoResponse {
    if team_id != state.team_id {
        return (StatusCode::NOT_FOUND, "team not found").into_response();
    }

    let item = WorkItem {
        id: WorkItemId::new(),
        title: req.title.clone(),
        description: req.description,
        steps: None,
        priority: match req.priority.as_deref() {
            Some("high") | Some("critical") => work::Priority::High,
            Some("low") => work::Priority::Low,
            _ => work::Priority::Normal,
        },
        timeout: None,
        context: Default::default(),
        notify_on_complete: true,
        created_at: kernel::types::Timestamp::now(),
    };

    let id_str = item.id.to_string();
    let resp = TaskResponse {
        id: id_str.clone(),
        title: req.title,
        description: String::new(),
        stage: String::new(),
    };

    (StatusCode::CREATED, Json(resp)).into_response()
}

/// GET /team/{team_id}/tasks — list work items (stub)
async fn list_tasks(
    Path(team_id): Path<String>,
    State(state): State<TeamApiState>,
) -> impl IntoResponse {
    if team_id != state.team_id {
        return (StatusCode::NOT_FOUND, "team not found").into_response();
    }
    // In a full implementation this would query the WorkflowEngine/StateStore.
    // For now, return empty list (work items live in the WorkflowEngine).
    Json(Vec::<TaskResponse>::new()).into_response()
}

/// POST /team/{team_id}/tasks/{id}/assign — dispatch a work item
async fn assign_task(
    Path((team_id, task_id)): Path<(String, String)>,
    State(state): State<TeamApiState>,
) -> impl IntoResponse {
    if team_id != state.team_id {
        return (StatusCode::NOT_FOUND, "team not found").into_response();
    }

    // In a full implementation, this would:
    // 1. Look up the work item by id from WorkflowEngine/StateStore
    // 2. Determine the current stage
    // 3. Call scheduler.dispatch()
    // For now, return a 501 Not Implemented with guidance.
    (
        StatusCode::NOT_IMPLEMENTED,
        format!("assign task {task_id}: manual dispatch via scheduler not yet wired — use stage auto_assign"),
    )
        .into_response()
}

/// POST /team/{team_id}/tasks/{id}/complete — mark work item complete
async fn complete_task(
    Path((team_id, task_id)): Path<(String, String)>,
    State(state): State<TeamApiState>,
    Json(req): Json<CompleteTaskRequest>,
) -> impl IntoResponse {
    if team_id != state.team_id {
        return (StatusCode::NOT_FOUND, "team not found").into_response();
    }

    // Check confidence-based safety gate
    if let Some(confidence) = req.confidence {
        // This would use the SafetyGateHandler — for now, informational
        if confidence < 0.7 {
            return (
                StatusCode::OK,
                Json(serde_json::json!({
                    "status": "pending_approval",
                    "reason": "confidence_below_threshold",
                    "task_id": task_id
                })),
            )
                .into_response();
        }
    }

    (
        StatusCode::OK,
        Json(serde_json::json!({
            "status": "completed",
            "task_id": task_id
        })),
    )
        .into_response()
}

/// GET /team/{team_id}/safety/pending — list pending safety gates
async fn pending_gates(
    Path(team_id): Path<String>,
    State(state): State<TeamApiState>,
) -> impl IntoResponse {
    if team_id != state.team_id {
        return (StatusCode::NOT_FOUND, "team not found").into_response();
    }
    match state.store.pending_safety_logs() {
        Ok(entries) => Json(entries).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
    }
}

/// POST /team/{team_id}/safety/{id}/resolve — approve/deny a safety gate
async fn resolve_gate(
    Path((team_id, id)): Path<(String, i64)>,
    State(state): State<TeamApiState>,
    Json(req): Json<ResolveGateRequest>,
) -> impl IntoResponse {
    if team_id != state.team_id {
        return (StatusCode::NOT_FOUND, "team not found").into_response();
    }
    let decision = match req.decision.as_str() {
        "approved" => HumanDecision::Approved,
        "denied" => HumanDecision::Denied,
        other => return (StatusCode::BAD_REQUEST, format!("invalid decision: {other}")).into_response(),
    };
    match state.store.resolve_safety_log(id, decision, &req.decided_by) {
        Ok(()) => (
            StatusCode::OK,
            Json(serde_json::json!({"status": "resolved", "id": id})),
        )
            .into_response(),
        Err(e) => (StatusCode::NOT_FOUND, e).into_response(),
    }
}

/// GET /team/{team_id}/context — list context documents
async fn list_context(
    Path(team_id): Path<String>,
    State(state): State<TeamApiState>,
) -> impl IntoResponse {
    if team_id != state.team_id {
        return (StatusCode::NOT_FOUND, "team not found").into_response();
    }
    match state.store.list_context(None) {
        Ok(entries) => Json(entries).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
    }
}

/// GET /team/{team_id}/context/{id} — get a context document
async fn get_context(
    Path((team_id, id)): Path<(String, i64)>,
    State(state): State<TeamApiState>,
) -> impl IntoResponse {
    if team_id != state.team_id {
        return (StatusCode::NOT_FOUND, "team not found").into_response();
    }
    match state.store.get_context(id) {
        Ok(Some(entry)) => Json(entry).into_response(),
        Ok(None) => (StatusCode::NOT_FOUND, "context not found").into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
    }
}

/// GET /team/{team_id}/agents — agent status summary
async fn agent_status(
    Path(team_id): Path<String>,
    State(state): State<TeamApiState>,
) -> impl IntoResponse {
    if team_id != state.team_id {
        return (StatusCode::NOT_FOUND, "team not found").into_response();
    }

    // Return empty if no registry accessor configured (subprocess or standalone mode)
    let agents = match &state.agent_registry {
        Some(reg) => reg.list_agent_summaries(),
        None => Vec::new(),
    };

    Json(agents).into_response()
}
