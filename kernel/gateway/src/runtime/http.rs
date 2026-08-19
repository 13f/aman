// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

use super::agent_runtime::AgentRuntime;
use super::session_store;
use super::AuditLogger;
use axum::extract::{Multipart, Path, State};
use tracing::instrument;
use axum::extract::Query;
use axum::http::{HeaderMap, StatusCode};
use axum::middleware;
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post, put};
use axum::{Json, Router};
use kernel::agent::AgentStatus;
use kernel::agent::AgentSystemState;
use kernel::context::{BaseContext, ToolContext};
use kernel::event::{Event, EventType};
use kernel::sanitizer::{content_hash, InputSanitizer, SanitizeResult};
use kernel::types::TraceId;
use kernel::Error;
use kernel::security::{ApprovedCapabilities, CapabilitySet};
use notification::{Notification as NotificationModel, Severity};
use persistence::{DeadLetterEntry, DeadLetterQueue, DlqFilter};
use plugin::{PluginCandidate, PluginManifest};
use serde::{Deserialize, Serialize};
use rand::Rng;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::net::SocketAddr;
use tokio_util::sync::CancellationToken;
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use uuid::Uuid;

/// Default operator name when the `x-aman-operator` header is absent.
const DEFAULT_OPERATOR: &str = "api";

#[derive(Debug, Clone)]
pub struct HttpServerConfig {
    pub bind: SocketAddr,
}

pub struct HttpServerHandle {
    addr: SocketAddr,
    shutdown_tx: Option<oneshot::Sender<()>>,
    /// Server task handle. Kept alive so `shutdown()` can wait for the
    /// graceful shutdown to actually finish (in-flight requests served,
    /// connections closed) before the process exits. Without this, the
    /// `/agent/shutdown` HTTP handler could be aborted mid-flight by the
    /// process exiting, and the desktop would never get a response.
    server_join: Option<tokio::task::JoinHandle<()>>,
}

impl HttpServerHandle {
    #[must_use]
    pub fn local_addr(&self) -> SocketAddr {
        self.addr
    }

    /// Trigger graceful shutdown and wait for the server task to finish.
    ///
    /// This blocks until all in-flight requests (including the
    /// `/agent/shutdown` request that triggers gateway teardown) have
    /// been responded to and the listening socket is closed. Returning
    /// early would let `main()` exit the process while the handler is
    /// still running, so the desktop would never receive the HTTP
    /// response and would have to fall back to a 60s timeout + SIGKILL.
    pub async fn shutdown(mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
        if let Some(join) = self.server_join.take() {
            let _ = join.await;
        }
    }
}

pub async fn serve(runtime: Arc<AgentRuntime>, config: HttpServerConfig) -> kernel::AmanResult<HttpServerHandle> {
    super::sse::start_sse_tasks(&runtime).await;
    // Emit plugin approval requests that were deferred during startup.
    // SSE tasks are now running, so the desktop client can receive them.
    runtime.emit_pending_plugin_approvals().await;
    // Collect plugin routes BEFORE build_router — uses the async lock safely.
    let plugin_routes = {
        let loader = runtime.plugin_loader().await;
        loader.collect_routes()
    };
    let router = build_router(runtime, plugin_routes);
    let listener = TcpListener::bind(config.bind).await?;
    let addr = listener.local_addr()?;
    let (tx, rx) = oneshot::channel::<()>();
    let server = axum::serve(listener, router).with_graceful_shutdown(async move {
        let _ = rx.await;
    });
    let server_join = tokio::spawn(async move {
        let _ = server.await;
    });
    Ok(HttpServerHandle {
        addr,
        shutdown_tx: Some(tx),
        server_join: Some(server_join),
    })
}

fn build_router(runtime: Arc<AgentRuntime>, plugin_routes: Vec<axum::Router<()>>) -> Router {
    let control = Router::new()
        .route("/agent/start", post(agent_start))
        .route("/agent/shutdown", post(agent_shutdown))
        .route("/event-source/{id}/pause", post(source_pause))
        .route("/event-source/{id}/resume", post(source_resume))
        .route("/event-source/{id}/config", put(source_config))
        .route("/source/{id}/pause", post(source_pause))
        .route("/source/{id}/resume", post(source_resume))
        .route("/source/{id}/config", put(source_config))
        .route("/im-channel/{platform}/{instance}/reload", post(im_channel_reload))
        .route("/skills", get(skill_list))
        .route("/llm-skills", get(llm_skills_list))
        .route("/skills/search", get(skill_search))
        .route("/skill/{name}", get(skill_info))
        .route("/skill/{name}/content", get(skill_content))
        .route("/skill/{name}/enable", post(skill_enable))
        .route("/skill/{name}/disable", post(skill_disable))
        .route("/skill/{name}/versions", get(skill_versions))
        .route("/skill/{name}/rollback", post(skill_rollback))
        .route("/skills/reload", post(skills_reload))
        .route("/workflows", get(workflow_list))
        .route("/workflow/{name}", get(workflow_info))
        .route("/workflow/{name}/create", post(workflow_create))
        .route("/workflow-instances", get(workflow_instances))
        .route("/workflow-instance/{id}", get(workflow_instance))
        .route("/workflow-instance/{id}/retry", post(workflow_retry))
        .route("/workflow-instance/{id}/cancel", post(workflow_cancel))
        .route("/plugins", get(plugin_list))
        .route("/plugin/{name}/enable", post(plugin_enable))
        .route("/plugin/{name}/disable", post(plugin_disable))
        .route("/plugin/{name}/uninstall", post(plugin_uninstall))
        .route("/plugin/install", post(plugin_install))
        .route("/cron/add", post(cron_add))
        .route("/cron/{id}/update", post(cron_update))
        .route("/cron/{id}/remove", post(cron_remove))
        .route("/inject-event", post(inject_event))
        .route("/events/push", post(push_event))
        .route("/events/types", get(events_types))
        .route("/events/dump/{id}", get(event_dump))
        .route("/events/recent", get(events_recent))
        .route("/events/trace/{trace_id}", get(event_trace))
        .route("/dlq", get(dlq_list))
        .route("/dlq/{id}/retry", post(dlq_retry))
        .route("/dlq/{id}/discard", post(dlq_discard))
        .route("/notifications", get(notifications_list))
        .route("/notifications/unread-count", get(notifications_unread_count))
        .route("/notifications/{id}/dismiss", post(notification_dismiss))
        .route("/notifications/{id}/ack", post(notification_ack))
        .route("/notifications/dismiss-all", post(notifications_dismiss_all))
        .route("/notifications/test", post(notifications_test))
        .route("/notifications/send", post(notifications_send))
        .route("/config/set", post(config_set))
        .route("/audit-log", get(audit_log))
        .route("/runtime/status", get(runtime_status))
        .route("/runtime/config", get(runtime_config))
        .route("/chat/sessions", get(chat_sessions))
        .route("/chat/session/create", post(chat_session_create))
        .route("/chat/session/{id}/state", get(chat_session_state))
        .route("/chat/session/{id}/history", get(chat_session_history))
        .route("/chat/session/{id}/send", post(chat_session_send))
        .route("/chat/session/{id}/close", post(chat_session_close))
        .route("/chat/session/{id}/stop", post(chat_session_stop))
        .route("/chat/session/{id}/kill", post(chat_session_kill))
        .route("/chat/session/{id}/retry", post(chat_session_retry))
        .route("/chat/session/{id}/edit", post(chat_session_edit))
        .route("/chat/session/{id}", delete(chat_session_delete))
        .route("/soul/info", get(soul_info))
        .route("/soul/raw", get(soul_raw))
        .route("/soul/update", post(soul_update))
        .route("/system-prompt", get(get_system_prompt))
        .route("/capabilities", get(capability_list))
        .route("/dlq/depth", get(dlq_depth))
        .route("/debug/metrics", get(debug_metrics))
        .route("/tool-auth/respond", post(tool_auth_respond))
        .route("/plugin-auth/respond", post(plugin_auth_respond))
        .route("/plugin-auth/pending", get(plugin_auth_pending))
        .route("/tools/{name}/execute", post(tool_execute))
        .route("/explore/start", post(explore_start))
        .route("/idle-run", post(idle_run))
        .route("/agents", get(agent_list))
        .route("/agent/{agent_id}", get(agent_get))
        .route("/agent/{agent_id}/status", post(agent_set_status))
        .route("/agent/{agent_id}/reload", post(agent_reload))
        .route("/agent/{agent_id}/idle/start", post(agent_idle_start))
        .route("/agent/{agent_id}/idle/stop", post(agent_idle_stop))
        .route("/agents/idle/start", post(agents_idle_start_all))
        .route("/agents/idle-availability", get(agents_idle_availability))
        .route("/analytics/analyze", post(super::analytics_handler::analytics_analyze))
        // MCP endpoints (per-agent)
        .route("/mcp/agent/{agent_key}/servers", get(mcp_list_servers))
        .route("/mcp/agent/{agent_key}/server/{name}/connect", post(mcp_connect_server))
        .route("/mcp/agent/{agent_key}/server/{name}/disconnect", post(mcp_disconnect_server))
        .route("/mcp/agent/{agent_key}/reload", post(mcp_reload))
        .route_layer(middleware::from_fn_with_state(
            runtime.clone(),
            require_api_token,
        ));

    let mut app = Router::new()
        .route("/health/live", get(health_live))
        .route("/health/ready", get(health_ready))
        .route("/health", get(health_ready))
        .route("/health/llm", get(health_llm))
        .route("/metrics", get(metrics))
        .route("/ui/pages", get(ui_plugin_pages))
        .route("/events/stream", get(super::sse::sse_stream_handler))
        .merge(control);

    // Merge plugin-contributed routes under /api/v1
    if !plugin_routes.is_empty() {
        let merged = plugin_routes
            .into_iter()
            .reduce(|acc, r| acc.merge(r))
            .unwrap();
        app = app.nest_service("/api/v1", merged);
    }

    app.with_state(runtime)
}

#[derive(Serialize)]
struct UiPageEntry {
    id: String,
    label: String,
}

async fn ui_plugin_pages(State(runtime): State<Arc<AgentRuntime>>) -> Json<Vec<UiPageEntry>> {
    let pages = runtime
        .plugin_manifests()
        .iter()
        .filter_map(|m| m.ui.as_ref())
        .flat_map(|ui| {
            ui.pages.iter().map(|page_id| UiPageEntry {
                id: page_id.clone(),
                label: match page_id.as_str() {
                    "team" => "Team".into(),
                    "startup" => "Startup".into(),
                    other => other.to_string(),
                },
            })
        })
        .collect();
    Json(pages)
}

async fn health_live(State(runtime): State<Arc<AgentRuntime>>) -> impl IntoResponse {
    if runtime.is_live() {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    }
}

async fn health_ready(State(runtime): State<Arc<AgentRuntime>>) -> impl IntoResponse {
    if runtime.is_ready() {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    }
}

async fn health_llm(State(runtime): State<Arc<AgentRuntime>>) -> impl IntoResponse {
    // LLM provider is considered healthy if a tool whose name starts with
    // "llm_" is registered in the runtime's tool registry.
    let tools = runtime.tools();
    let has_provider = ["llm_openai", "llm_provider_openai"]
        .iter()
        .any(|name| tools.get(name).is_some());
    if runtime.is_ready() && has_provider {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    }
}

#[instrument(skip(runtime))]
async fn agent_start(State(runtime): State<Arc<AgentRuntime>>) -> Response {
    match runtime.start().await {
        Ok(()) => {
            runtime
                .audit()
                .record(DEFAULT_OPERATOR, "agent.start", "agent", "ok", "");
            StatusCode::OK.into_response()
        }
        Err(error) => {
            runtime
                .audit()
                .record(DEFAULT_OPERATOR, "agent.start", "agent", "error", error.to_string());
            ApiError::from(error).into_response()
        }
    }
}

#[instrument(skip(runtime, headers))]
async fn agent_shutdown(State(runtime): State<Arc<AgentRuntime>>, headers: HeaderMap) -> Response {
    let operator = operator_from_headers(&headers).unwrap_or(DEFAULT_OPERATOR);
    if let Some(response) = guard_confirmation(&runtime.audit(), operator, "agent.shutdown", "agent", &headers) { return response; }
    // HTTP agent.shutdown: per-agent druntime-wide shutdown. This path
    // is not driven by a Ctrl+C, so pass a fresh, un-cancelled token —
    // all drain loops run to their natural completion.
    let cancel = CancellationToken::new();
    with_audit(&runtime.audit(), operator, "agent.shutdown", "agent", runtime.shutdown(&cancel).await)
}

async fn source_pause(
    State(runtime): State<Arc<AgentRuntime>>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Response {
    let operator = operator_from_headers(&headers).unwrap_or(DEFAULT_OPERATOR);
    let resource = format!("source:{id}");
    with_audit(&runtime.audit(), operator, "source.pause", &resource, runtime.sources().pause(&id).await)
}

async fn source_resume(
    State(runtime): State<Arc<AgentRuntime>>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Response {
    let operator = operator_from_headers(&headers).unwrap_or(DEFAULT_OPERATOR);
    let resource = format!("source:{id}");
    with_audit(&runtime.audit(), operator, "source.resume", &resource, runtime.sources().resume(&id).await)
}

async fn source_config(
    State(runtime): State<Arc<AgentRuntime>>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(payload): Json<Value>,
) -> Response {
    let operator = operator_from_headers(&headers).unwrap_or(DEFAULT_OPERATOR);
    let resource = format!("source:{id}");
    with_audit(&runtime.audit(), operator, "source.config", &resource, runtime.sources().reconfigure(&id, payload).await)
}

async fn im_channel_reload(
    State(runtime): State<Arc<AgentRuntime>>,
    Path((platform, instance)): Path<(String, String)>,
    headers: HeaderMap,
) -> Response {
    let operator = operator_from_headers(&headers).unwrap_or(DEFAULT_OPERATOR);
    match runtime.reload_im_channel_source(&platform, &instance).await {
        Ok(()) => {
            runtime.audit().record(
                operator,
                "im_channel.reload",
                format!("{platform}/{instance}"),
                "ok",
                "",
            );
            StatusCode::OK.into_response()
        }
        Err(error) => {
            runtime.audit().record(
                operator,
                "im_channel.reload",
                format!("{platform}/{instance}"),
                "error",
                error.to_string(),
            );
            ApiError::from(error).into_response()
        }
    }
}

#[derive(Debug, Serialize)]
struct SkillListResponse {
    items: Vec<skill::SkillSnapshot>,
}

async fn skill_list(State(runtime): State<Arc<AgentRuntime>>) -> Response {
    Json(SkillListResponse {
        items: runtime.skills().list(),
    })
    .into_response()
}

#[derive(Debug, Serialize)]
struct LlmSkillItem {
    name: String,
    description: String,
    category: String,
    triggers: Vec<String>,
}

#[derive(Debug, Serialize)]
struct LlmSkillsResponse {
    items: Vec<LlmSkillItem>,
}

async fn llm_skills_list(State(runtime): State<Arc<AgentRuntime>>) -> Response {
    let items = runtime
        .llm_skills()
        .into_iter()
        .map(|s| LlmSkillItem {
            name: s.name,
            description: s.description,
            category: s.category,
            triggers: s.triggers,
        })
        .collect();
    Json(LlmSkillsResponse { items }).into_response()
}

#[derive(Debug, Deserialize)]
struct SkillSearchParams {
    q: Option<String>,
    limit: Option<usize>,
}

#[derive(Debug, Serialize)]
struct SkillSearchMatch {
    name: String,
    version: String,
    score: u32,
    snippet: String,
    matched_field: String,
}

#[derive(Debug, Serialize)]
struct SkillSearchResponse {
    items: Vec<SkillSearchMatch>,
}

async fn skill_search(
    State(runtime): State<Arc<AgentRuntime>>,
    Query(params): Query<SkillSearchParams>,
) -> Response {
    let q = params.q.unwrap_or_default();
    let limit = params.limit.unwrap_or(10);
    let matches = runtime
        .skill_search()
        .search(&q, limit)
        .into_iter()
        .map(|item| SkillSearchMatch {
            name: item.name,
            version: item.version,
            score: item.score,
            snippet: item.snippet,
            matched_field: item.matched_field,
        })
        .collect::<Vec<_>>();
    Json(SkillSearchResponse { items: matches }).into_response()
}

async fn skill_info(State(runtime): State<Arc<AgentRuntime>>, Path(name): Path<String>) -> Response {
    let Some(item) = runtime.skills().snapshot(&name) else {
        return ApiError::not_found(format!("skill not found: {name}"),).into_response();
    };
    Json(item).into_response()
}

#[derive(Debug, Serialize)]
struct SkillContentResponse {
    name: String,
    content: String,
}

async fn skill_content(
    State(runtime): State<Arc<AgentRuntime>>,
    Path(name): Path<String>,
) -> Response {
    match runtime.read_skill(&name) {
        Some(content) => Json(SkillContentResponse { name, content }).into_response(),
        None => ApiError::not_found(format!("skill not found: {name}"),).into_response(),
    }
}

async fn skill_enable(
    State(runtime): State<Arc<AgentRuntime>>,
    Path(name): Path<String>,
    headers: HeaderMap,
) -> Response {
    let operator = operator_from_headers(&headers).unwrap_or(DEFAULT_OPERATOR);
    match runtime.skills().enable(&name) {
        Ok(()) => {
            runtime.audit().record(
                operator,
                "skill.enable",
                format!("skill:{name}"),
                "ok",
                "",
            );
            StatusCode::OK.into_response()
        }
        Err(error) => {
            runtime.audit().record(
                operator,
                "skill.enable",
                format!("skill:{name}"),
                "error",
                error.to_string(),
            );
            ApiError::from(error).into_response()
        }
    }
}

async fn skill_disable(
    State(runtime): State<Arc<AgentRuntime>>,
    Path(name): Path<String>,
    headers: HeaderMap,
) -> Response {
    let operator = operator_from_headers(&headers).unwrap_or(DEFAULT_OPERATOR);
    match runtime.skills().disable(&name) {
        Ok(()) => {
            runtime.audit().record(
                operator,
                "skill.disable",
                format!("skill:{name}"),
                "ok",
                "",
            );
            StatusCode::OK.into_response()
        }
        Err(error) => {
            runtime.audit().record(
                operator,
                "skill.disable",
                format!("skill:{name}"),
                "error",
                error.to_string(),
            );
            ApiError::from(error).into_response()
        }
    }
}

#[derive(Debug, Serialize)]
struct SkillVersionItem {
    version: String,
    created_at_ms: u64,
}

#[derive(Debug, Serialize)]
struct SkillVersionsResponse {
    items: Vec<SkillVersionItem>,
}

async fn skill_versions(
    State(runtime): State<Arc<AgentRuntime>>,
    Path(name): Path<String>,
) -> Response {
    let history = match runtime.skill_versions().history(&name) {
        Ok(items) => items,
        Err(error) => return ApiError::from(error).into_response(),
    };
    Json(SkillVersionsResponse {
        items: history
            .into_iter()
            .map(|item| SkillVersionItem {
                version: item.version,
                created_at_ms: item.created_at_ms,
            })
            .collect(),
    })
    .into_response()
}

#[derive(Debug, Deserialize)]
struct SkillRollbackRequest {
    version: String,
}

async fn skill_rollback(
    State(runtime): State<Arc<AgentRuntime>>,
    Path(name): Path<String>,
    headers: HeaderMap,
    Json(payload): Json<SkillRollbackRequest>,
) -> Response {
    let operator = operator_from_headers(&headers).unwrap_or(DEFAULT_OPERATOR);
    if let Some(response) = guard_confirmation(&runtime.audit(), operator, "skill.rollback", &format!("skill:{name}"), &headers) { return response; }

    let destination = runtime
        .skills_dir()
        .join(format!("{}.yaml", sanitize_skill_file_name(&name)));
    if let Err(error) = runtime
        .skill_versions()
        .rollback(&name, &payload.version, &destination)
    {
        runtime.audit().record(
            operator,
            "skill.rollback",
            format!("skill:{name}"),
            "error",
            error.to_string(),
        );
        return ApiError::from(error).into_response();
    }

    let _ = runtime.reload_skills_now();
    runtime.audit().record(
        operator,
        "skill.rollback",
        format!("skill:{name}"),
        "ok",
        payload.version,
    );
    StatusCode::OK.into_response()
}

#[derive(Debug, Serialize)]
struct WorkflowListResponse {
    items: Vec<String>,
}

async fn workflow_list(State(runtime): State<Arc<AgentRuntime>>) -> Response {
    Json(WorkflowListResponse {
        items: runtime.workflow_engine().list_workflows(),
    })
    .into_response()
}

async fn workflow_info(
    State(runtime): State<Arc<AgentRuntime>>,
    Path(name): Path<String>,
) -> Response {
    let Some(workflow) = runtime.workflow_engine().get_workflow(&name) else {
        return ApiError::not_found(format!("workflow not found: {name}"),).into_response();
    };
    Json(workflow).into_response()
}

#[derive(Debug, Deserialize)]
struct WorkflowCreateRequest {
    #[serde(default)]
    data: Value,
}

async fn workflow_create(
    State(runtime): State<Arc<AgentRuntime>>,
    Path(name): Path<String>,
    headers: HeaderMap,
    Json(payload): Json<WorkflowCreateRequest>,
) -> Response {
    let operator = operator_from_headers(&headers).unwrap_or(DEFAULT_OPERATOR);
    match runtime.workflow_engine().create_instance(&name, payload.data) {
        Ok(instance) => {
            runtime.audit().record(
                operator,
                "workflow.create",
                format!("workflow:{name}"),
                "ok",
                instance.id.clone(),
            );
            Json(instance).into_response()
        }
        Err(error) => {
            runtime.audit().record(
                operator,
                "workflow.create",
                format!("workflow:{name}"),
                "error",
                error.to_string(),
            );
            ApiError::from(error).into_response()
        }
    }
}

#[derive(Debug, Serialize)]
struct WorkflowInstancesResponse {
    items: Vec<workflow::WorkflowInstance>,
}

async fn workflow_instances(State(runtime): State<Arc<AgentRuntime>>) -> Response {
    Json(WorkflowInstancesResponse {
        items: runtime.workflow_engine().list_instances(),
    })
    .into_response()
}

async fn workflow_instance(
    State(runtime): State<Arc<AgentRuntime>>,
    Path(id): Path<String>,
) -> Response {
    let Some(instance) = runtime.workflow_engine().get_instance(&id) else {
        return ApiError::not_found(format!("workflow instance not found: {id}"),).into_response();
    };
    Json(instance).into_response()
}

#[derive(Debug, Serialize)]
struct WorkflowTransitionResponse {
    instance_id: String,
    from_state: String,
    to_state: String,
    transitioned: bool,
    reason: String,
}

async fn workflow_retry(
    State(runtime): State<Arc<AgentRuntime>>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Response {
    let operator = operator_from_headers(&headers).unwrap_or(DEFAULT_OPERATOR);
    if let Some(response) = guard_confirmation(&runtime.audit(), operator, "workflow.retry", &format!("workflow-instance:{id}"), &headers) { return response; }
    let event = Event::new(
        "workflow:control",
        EventType::Custom("retry".to_owned()),
        json!({ "operator": operator }),
    );
    match runtime.workflow_engine().handle_event(&id, event).await {
        Ok(result) => {
            runtime.audit().record(
                operator,
                "workflow.retry",
                format!("workflow-instance:{id}"),
                "ok",
                "",
            );
            Json(WorkflowTransitionResponse {
                instance_id: result.instance_id,
                from_state: result.from_state,
                to_state: result.to_state,
                transitioned: result.transitioned,
                reason: format!("{:?}", result.reason),
            })
            .into_response()
        }
        Err(error) => {
            runtime.audit().record(
                operator,
                "workflow.retry",
                format!("workflow-instance:{id}"),
                "error",
                error.to_string(),
            );
            ApiError::from(error).into_response()
        }
    }
}

async fn workflow_cancel(
    State(runtime): State<Arc<AgentRuntime>>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Response {
    let operator = operator_from_headers(&headers).unwrap_or(DEFAULT_OPERATOR);
    if let Some(response) = guard_confirmation(&runtime.audit(), operator, "workflow.cancel", &format!("workflow-instance:{id}"), &headers) { return response; }
    let event = Event::new(
        "workflow:control",
        EventType::Custom("cancel".to_owned()),
        json!({ "operator": operator }),
    );
    match runtime.workflow_engine().handle_event(&id, event).await {
        Ok(result) => {
            runtime.audit().record(
                operator,
                "workflow.cancel",
                format!("workflow-instance:{id}"),
                "ok",
                "",
            );
            Json(WorkflowTransitionResponse {
                instance_id: result.instance_id,
                from_state: result.from_state,
                to_state: result.to_state,
                transitioned: result.transitioned,
                reason: format!("{:?}", result.reason),
            })
            .into_response()
        }
        Err(error) => {
            runtime.audit().record(
                operator,
                "workflow.cancel",
                format!("workflow-instance:{id}"),
                "error",
                error.to_string(),
            );
            ApiError::from(error).into_response()
        }
    }
}

async fn plugin_enable(
    State(runtime): State<Arc<AgentRuntime>>,
    Path(name): Path<String>,
    headers: HeaderMap,
) -> Response {
    let operator = operator_from_headers(&headers).unwrap_or(DEFAULT_OPERATOR);
    match runtime.enable_plugin(&name).await {
        Ok(()) => {
            runtime.audit().record(
                operator,
                "plugin.enable",
                format!("plugin:{name}"),
                "ok",
                "",
            );
            StatusCode::OK.into_response()
        }
        Err(error) => {
            runtime.audit().record(
                operator,
                "plugin.enable",
                format!("plugin:{name}"),
                "error",
                error.to_string(),
            );
            ApiError::from(error).into_response()
        }
    }
}

#[derive(Debug, Clone, Serialize)]
struct PluginListResponse {
    items: Vec<PluginListItem>,
}

#[derive(Debug, Clone, Serialize)]
struct PluginListItem {
    name: String,
    version: Option<String>,
    installed: bool,
    loaded: bool,
    state: Option<plugin::PluginLifecycleState>,
    unstable: bool,
    manifest_path: Option<String>,
}

async fn plugin_list(State(runtime): State<Arc<AgentRuntime>>, headers: HeaderMap) -> Response {
    let operator = operator_from_headers(&headers).unwrap_or(DEFAULT_OPERATOR);
    let plugins_root = runtime.runtime_dir().join("plugins");

    let mut installed = BTreeMap::<String, (String, String)>::new();
    for path in plugin::parse_plugin_manifests(&plugins_root) {
        let manifest = match PluginManifest::from_file(std::path::Path::new(&path)) {
            Ok(manifest) => manifest,
            Err(error) => {
                runtime.audit().record(
                    operator,
                    "plugin.list",
                    "plugins",
                    "error",
                    error.to_string(),
                );
                return ApiError::from(error).into_response();
            }
        };
        installed.insert(
            manifest.name,
            (manifest.version.to_string(), path.to_owned()),
        );
    }

    let mut loaded = BTreeMap::<String, (plugin::PluginLifecycleState, bool)>::new();
    let loader = runtime.plugin_loader().await;
    for name in loader.loaded_plugins() {
        if let Some(state) = loader.state_of(&name) {
            let unstable = loader.is_unstable(&name);
            loaded.insert(name, (state, unstable));
        }
    }

    let mut names = BTreeSet::new();
    names.extend(installed.keys().cloned());
    names.extend(loaded.keys().cloned());

    let mut items = Vec::new();
    for name in names {
        let installed_info = installed.get(&name);
        let loaded_info = loaded.get(&name);
        items.push(PluginListItem {
            name: name.clone(),
            version: installed_info.map(|(v, _)| v.clone()),
            installed: installed_info.is_some(),
            loaded: loaded_info.is_some(),
            state: loaded_info.map(|(state, _)| *state),
            unstable: loaded_info.map(|(_, u)| *u).unwrap_or(false),
            manifest_path: installed_info.map(|(_, p)| p.clone()),
        });
    }

    runtime
        .audit()
        .record(operator, "plugin.list", "plugins", "ok", "");

    (StatusCode::OK, Json(PluginListResponse { items })).into_response()
}

async fn plugin_disable(
    State(runtime): State<Arc<AgentRuntime>>,
    Path(name): Path<String>,
    headers: HeaderMap,
) -> Response {
    let operator = operator_from_headers(&headers).unwrap_or(DEFAULT_OPERATOR);
    if let Some(response) = guard_confirmation(&runtime.audit(), operator, "plugin.disable", &format!("plugin:{name}"), &headers) { return response; }
    match runtime.disable_plugin(&name).await {
        Ok(()) => {
            runtime.audit().record(
                operator,
                "plugin.disable",
                format!("plugin:{name}"),
                "ok",
                "",
            );
            StatusCode::OK.into_response()
        }
        Err(error) => {
            runtime.audit().record(
                operator,
                "plugin.disable",
                format!("plugin:{name}"),
                "error",
                error.to_string(),
            );
            ApiError::from(error).into_response()
        }
    }
}

async fn plugin_uninstall(
    State(runtime): State<Arc<AgentRuntime>>,
    Path(name): Path<String>,
    headers: HeaderMap,
) -> Response {
    let operator = operator_from_headers(&headers).unwrap_or(DEFAULT_OPERATOR);
    match runtime.uninstall_plugin(&name).await {
        Ok(()) => {
            runtime.audit().record(
                operator,
                "plugin.uninstall",
                format!("plugin:{name}"),
                "ok",
                "",
            );
            (StatusCode::OK, Json(OkResponse { ok: true })).into_response()
        }
        Err(error) => {
            runtime.audit().record(
                operator,
                "plugin.uninstall",
                format!("plugin:{name}"),
                "error",
                error.to_string(),
            );
            ApiError::from(error).into_response()
        }
    }
}

async fn plugin_install(
    State(runtime): State<Arc<AgentRuntime>>,
    headers: HeaderMap,
    mut multipart: Multipart,
) -> Response {
    let operator = operator_from_headers(&headers).unwrap_or(DEFAULT_OPERATOR);
    let mut archive_bytes = None;
    loop {
        match multipart.next_field().await {
            Ok(Some(field)) => {
                let is_plugin_field = field.name() == Some("plugin");
                let filename_match = field
                    .file_name()
                    .map(|name| name.ends_with(".tar.gz"))
                    .unwrap_or(false);
                if is_plugin_field || filename_match {
                    match field.bytes().await {
                        Ok(bytes) => {
                            archive_bytes = Some(bytes.to_vec());
                            break;
                        }
                        Err(error) => {
                            return ApiError::bad_request(format!("failed to read multipart field: {error}"),).into_response();
                        }
                    }
                }
            }
            Ok(None) => break,
            Err(error) => {
                return ApiError::bad_request(format!("invalid multipart payload: {error}"),).into_response();
            }
        }
    }

    let Some(archive_bytes) = archive_bytes else {
        return ApiError::bad_request("multipart must contain `plugin` file field".to_owned(),).into_response();
    };

    let installer = runtime.plugin_installer();
    let install_result =
        tokio::task::spawn_blocking(move || installer.install_from_archive_bytes(&archive_bytes))
            .await;
    match install_result {
        Ok(Ok(installed)) => {
            runtime.audit().record(
                operator,
                "plugin.install",
                format!("plugin:{}", installed.manifest.name),
                "ok",
                installed.install_dir.display().to_string(),
            );
            (
            StatusCode::OK,
            Json(InstallPluginResponse {
                plugin_name: installed.manifest.name,
                version: installed.manifest.version.to_string(),
                install_dir: installed.install_dir.display().to_string(),
            }),
            )
                .into_response()
        }
        Ok(Err(error)) => {
            runtime.audit().record(operator, "plugin.install", "plugin", "error", error.to_string());
            ApiError::from(error).into_response()
        }
        Err(error) => ApiError::internal(format!("install task join error: {error}"),).into_response(),
    }
}

#[derive(Debug, Deserialize)]
struct CronAddRequest {
    id: String,
    expression: String,
    #[serde(default)]
    agent_key: String,
}

async fn cron_add(
    State(runtime): State<Arc<AgentRuntime>>,
    headers: HeaderMap,
    Json(req): Json<CronAddRequest>,
) -> Response {
    let caller = operator_from_headers(&headers).unwrap_or(DEFAULT_OPERATOR);
    match runtime.add_cron_job(req.id, req.expression, &req.agent_key, caller).await {
        Ok(()) => {
            runtime
                .audit()
                .record(caller, "cron.add", "cron", "ok", "");
            (StatusCode::OK, Json(OkResponse { ok: true })).into_response()
        }
        Err(error) => {
            runtime
                .audit()
                .record(caller, "cron.add", "cron", "error", error.to_string());
            ApiError::from(error).into_response()
        }
    }
}

async fn cron_update(
    State(runtime): State<Arc<AgentRuntime>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(payload): Json<Value>,
) -> Response {
    let caller = operator_from_headers(&headers).unwrap_or(DEFAULT_OPERATOR);
    let agent_key = payload
        .get("agent_key")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_owned();
    match runtime.update_cron_job(&id, payload, &agent_key, caller).await {
        Ok(()) => {
            runtime
                .audit()
                .record(caller, "cron.update", format!("cron:{id}"), "ok", "");
            (StatusCode::OK, Json(OkResponse { ok: true })).into_response()
        }
        Err(error) => {
            runtime.audit().record(
                caller,
                "cron.update",
                format!("cron:{id}"),
                "error",
                error.to_string(),
            );
            ApiError::from(error).into_response()
        }
    }
}

#[derive(Deserialize)]
struct CronRemoveRequest {
    #[serde(default)]
    agent_key: String,
}

async fn cron_remove(
    State(runtime): State<Arc<AgentRuntime>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(req): Json<CronRemoveRequest>,
) -> Response {
    let caller = operator_from_headers(&headers).unwrap_or(DEFAULT_OPERATOR);
    match runtime.remove_cron_job(&id, &req.agent_key, caller).await {
        Ok(()) => {
            runtime
                .audit()
                .record(caller, "cron.remove", format!("cron:{id}"), "ok", "");
            (StatusCode::OK, Json(OkResponse { ok: true })).into_response()
        }
        Err(error) => {
            runtime.audit().record(
                caller,
                "cron.remove",
                format!("cron:{id}"),
                "error",
                error.to_string(),
            );
            ApiError::from(error).into_response()
        }
    }
}

#[instrument(skip(runtime, headers), fields(source = %req.source, event_type = %req.event_type))]
async fn inject_event(
    State(runtime): State<Arc<AgentRuntime>>,
    headers: HeaderMap,
    Json(req): Json<InjectEventRequest>,
) -> Response {
    let operator = operator_from_headers(&headers).unwrap_or(DEFAULT_OPERATOR);
    if !runtime.risky_capabilities_enabled() {
        runtime.audit().record(
            operator,
            "event.inject",
            "debug",
            "forbidden",
            "risky_capabilities_enabled=false",
        );
        return StatusCode::FORBIDDEN.into_response();
    }
    let event = Event::new(req.source, EventType::from(req.event_type), req.payload);
    let id = event.id.to_string();
    match runtime.publish_event(event).await {
        Ok(()) => {
            runtime
                .audit()
                .record(operator, "event.inject", "debug", "ok", &id);
            (StatusCode::OK, Json(InjectEventResponse { id })).into_response()
        }
        Err(error) => {
            runtime.audit().record(
                operator,
                "event.inject",
                "debug",
                "error",
                error.to_string(),
            );
            ApiError::from(error).into_response()
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
struct InjectEventRequest {
    source: String,
    event_type: String,
    payload: Value,
}

#[derive(Debug, Clone, Serialize)]
struct InjectEventResponse {
    id: String,
}

// ── External event push ────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize)]
struct PushEventRequest {
    source: String,
    event_type: String,
    payload: Value,
    #[serde(default)]
    agent_id: Option<String>,
    #[serde(default)]
    priority: Option<String>,
    #[serde(default)]
    delivery: Option<String>,
    #[serde(default)]
    ttl_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize)]
struct PushEventResponse {
    id: String,
    event_type: String,
    target: String,
}

#[instrument(skip(runtime, headers), fields(source = %req.source, event_type = %req.event_type))]
async fn push_event(
    State(runtime): State<Arc<AgentRuntime>>,
    headers: HeaderMap,
    Json(req): Json<PushEventRequest>,
) -> Response {
    let operator = operator_from_headers(&headers).unwrap_or(DEFAULT_OPERATOR);

    if req.source.trim().is_empty() {
        return ApiError::bad_request("source cannot be empty").into_response();
    }

    let event_type = EventType::from(req.event_type.clone());
    let mut event = Event::new(req.source.clone(), event_type, req.payload);

    if let Some(ref p) = req.priority
        && let Ok(priority) = serde_json::from_value(Value::String(p.clone())) {
            event.priority = priority;
        }
    if let Some(ref d) = req.delivery
        && let Ok(delivery) = serde_json::from_value(Value::String(d.clone())) {
            event.delivery = delivery;
        }
    if let Some(ttl) = req.ttl_ms {
        event.metadata.ttl_ms = Some(ttl);
    }

    let id = event.id.to_string();
    let target = match &req.agent_id {
        Some(agent_id) => {
            match runtime.publish_event_to_agent(agent_id, event).await {
                Ok(()) => format!("agent:{agent_id}"),
                Err(error) => {
                    runtime.audit().record(
                        operator,
                        "event.push",
                        "error",
                        "error",
                        error.to_string(),
                    );
                    return ApiError::from(error).into_response();
                }
            }
        }
        None => {
            match runtime.publish_event(event).await {
                Ok(()) => "global".to_owned(),
                Err(error) => {
                    runtime.audit().record(
                        operator,
                        "event.push",
                        "error",
                        "error",
                        error.to_string(),
                    );
                    return ApiError::from(error).into_response();
                }
            }
        }
    };

    runtime
        .audit()
        .record(operator, "event.push", &target, "ok", &id);

    (
        StatusCode::OK,
        Json(PushEventResponse {
            id,
            event_type: req.event_type,
            target,
        }),
    )
        .into_response()
}

async fn events_types() -> Response {
    let known_types: &[&str] = &[
        "file_created",
        "file_changed",
        "file_deleted",
        "cron_tick",
        "timer_tick",
        "heartbeat",
        "message_received",
        "webhook_received",
        "system_signal",
        "workflow_state_changed",
        "skill_loaded",
        "skill_reloaded",
        "config_changed",
        "secret_rotated",
        "injection_detected",
        "idle",
        "system.queue_drained",
        "agent:message",
    ];
    (
        StatusCode::OK,
        Json(json!({
            "known_types": known_types,
            "custom": "Any string not in known_types produces EventType::Custom(value)",
        })),
    )
        .into_response()
}

#[derive(Debug, Clone, Deserialize)]
struct DlqListQuery {
    reason: Option<String>,
    source: Option<String>,
    event_type: Option<String>,
    limit: Option<usize>,
    offset: Option<usize>,
}

async fn dlq_list(
    State(runtime): State<Arc<AgentRuntime>>,
    axum::extract::Query(query): axum::extract::Query<DlqListQuery>,
) -> Response {
    let filter = DlqFilter {
        reason: query.reason,
        source: query.source,
        event_type: query.event_type,
        limit: query.limit,
        offset: query.offset.unwrap_or(0),
    };
    match runtime.dlq().list(filter) {
        Ok(items) => (
            StatusCode::OK,
            Json(DlqListResponse {
                items: items
                    .into_iter()
                    .map(DlqEntryResponse::from)
                    .collect::<Vec<_>>(),
            }),
        )
            .into_response(),
        Err(error) => ApiError::from(error).into_response(),
    }
}

#[derive(Debug, Clone, Serialize)]
struct DlqListResponse {
    items: Vec<DlqEntryResponse>,
}

#[derive(Debug, Clone, Serialize)]
struct DlqEntryResponse {
    id: String,
    event: Event,
    reason: String,
    retry_count: u32,
    original_retry_count: u32,
    enqueued_at_ms: i64,
    expires_at_ms: i64,
}

impl From<DeadLetterEntry> for DlqEntryResponse {
    fn from(value: DeadLetterEntry) -> Self {
        Self {
            id: value.id,
            event: value.event,
            reason: value.reason,
            retry_count: value.retry_count,
            original_retry_count: value.original_retry_count,
            enqueued_at_ms: value.enqueued_at.as_millis(),
            expires_at_ms: value.expires_at.as_millis(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
struct DlqRetryRequest {
    reason: Option<String>,
}

async fn dlq_retry(
    State(runtime): State<Arc<AgentRuntime>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(payload): Json<DlqRetryRequest>,
) -> Response {
    let operator = operator_from_headers(&headers).unwrap_or(DEFAULT_OPERATOR);
    if let Some(response) = guard_confirmation(&runtime.audit(), operator, "dlq.retry", &format!("dlq:{id}"), &headers) { return response; }
    let reason = payload.reason.unwrap_or_else(|| "manual retry".to_owned());
    let event = match runtime.dlq().retry(&id, operator, reason) {
        Ok(event) => event,
        Err(error) => {
            runtime.audit().record(
                operator,
                "dlq.retry",
                format!("dlq:{id}"),
                "error",
                error.to_string(),
            );
            return ApiError::from(error).into_response();
        }
    };
    match runtime.publish_event(event).await {
        Ok(()) => {
            runtime.audit().record(
                operator,
                "dlq.retry",
                format!("dlq:{id}"),
                "ok",
                "",
            );
            (StatusCode::OK, Json(OkResponse { ok: true })).into_response()
        }
        Err(error) => {
            runtime.audit().record(
                operator,
                "dlq.retry",
                format!("dlq:{id}"),
                "error",
                error.to_string(),
            );
            ApiError::from(error).into_response()
        }
    }
}

async fn dlq_discard(
    State(runtime): State<Arc<AgentRuntime>>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Response {
    let operator = operator_from_headers(&headers).unwrap_or(DEFAULT_OPERATOR);
    match runtime.dlq().discard(&id) {
        Ok(entry) => {
            runtime.audit().record(
                operator,
                "dlq.discard",
                format!("dlq:{id}"),
                "ok",
                "",
            );
            (StatusCode::OK, Json(DlqEntryResponse::from(entry))).into_response()
        }
        Err(error) => {
            runtime.audit().record(
                operator,
                "dlq.discard",
                format!("dlq:{id}"),
                "error",
                error.to_string(),
            );
            ApiError::from(error).into_response()
        }
    }
}

// ── Notification endpoints ───────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize)]
struct NotificationsQuery {
    active_only: Option<bool>,
    severity: Option<String>,
    limit: Option<usize>,
    offset: Option<usize>,
}

#[derive(Debug, Clone, Serialize)]
struct NotificationResponse {
    id: String,
    severity: String,
    category: String,
    created_at: i64,
    title: String,
    message: String,
    dismissed: bool,
    dismissible: bool,
    action_label: Option<String>,
    action_route: Option<String>,
    event_id: Option<String>,
    source: Option<String>,
}

impl From<NotificationModel> for NotificationResponse {
    fn from(n: NotificationModel) -> Self {
        Self {
            id: n.id,
            severity: serde_json::to_value(n.severity)
                .ok()
                .and_then(|v| v.as_str().map(str::to_owned))
                .unwrap_or_default(),
            category: serde_json::to_value(&n.category)
                .ok()
                .and_then(|v| v.as_str().map(str::to_owned))
                .unwrap_or_default(),
            created_at: n.created_at,
            title: n.title,
            message: n.message,
            dismissed: n.dismissed,
            dismissible: n.dismissible,
            action_label: n.action_label,
            action_route: n.action_route,
            event_id: n.event_id,
            source: n.source,
        }
    }
}

#[instrument(skip(runtime, query))]
async fn notifications_list(
    State(runtime): State<Arc<AgentRuntime>>,
    axum::extract::Query(query): axum::extract::Query<NotificationsQuery>,
) -> Response {
    let severity = query.severity.as_deref().and_then(|s| match s {
        "critical" => Some(Severity::Critical),
        "warning" => Some(Severity::Warning),
        _ => None,
    });
    let items = runtime
        .notifications()
        .list(
            query.active_only.unwrap_or(true),
            severity,
            query.limit.unwrap_or(50).min(500),
            query.offset.unwrap_or(0),
        )
        .into_iter()
        .map(NotificationResponse::from)
        .collect::<Vec<_>>();
    (StatusCode::OK, Json(items)).into_response()
}

#[derive(Debug, Clone, Serialize)]
struct UnreadCountResponse {
    count: usize,
}

async fn notifications_unread_count(
    State(runtime): State<Arc<AgentRuntime>>,
) -> Response {
    let count = runtime.notifications().unread_count();
    (StatusCode::OK, Json(UnreadCountResponse { count })).into_response()
}

async fn notification_dismiss(
    State(runtime): State<Arc<AgentRuntime>>,
    Path(id): Path<String>,
) -> Response {
    if runtime.notifications().dismiss(&id) {
        StatusCode::OK.into_response()
    } else {
        (StatusCode::NOT_FOUND, "notification not found or not dismissible").into_response()
    }
}

async fn notification_ack(
    State(runtime): State<Arc<AgentRuntime>>,
    Path(id): Path<String>,
) -> Response {
    if runtime.notifications().acknowledge(&id) {
        StatusCode::OK.into_response()
    } else {
        (StatusCode::NOT_FOUND, "notification not found").into_response()
    }
}

async fn notifications_dismiss_all(
    State(runtime): State<Arc<AgentRuntime>>,
) -> Response {
    runtime.notifications().dismiss_all();
    StatusCode::OK.into_response()
}

/// Dev-only: push a test notification. Only available when no API token is set
/// (i.e. running in development mode).
#[derive(Debug, Clone, Deserialize)]
struct TestNotificationRequest {
    severity: Option<String>,
    title: Option<String>,
    message: Option<String>,
    action_label: Option<String>,
    action_route: Option<String>,
}

async fn notifications_test(
    State(runtime): State<Arc<AgentRuntime>>,
    Json(req): Json<TestNotificationRequest>,
) -> Response {
    if runtime.api_token().is_some() {
        return StatusCode::FORBIDDEN.into_response();
    }

    let severity = req.severity.as_deref().unwrap_or("warning");
    let title = req.title.unwrap_or_else(|| "测试通知".into());
    let message = req.message.unwrap_or_else(|| "这是一条测试通知".into());

    let mut n = match severity {
        "critical" => notification::Notification::critical(
            notification::Category::Gateway, title, message,
        ),
        _ => notification::Notification::warning(
            notification::Category::Gateway, title, message,
        ),
    };

    if let Some(label) = req.action_label
        && let Some(route) = req.action_route {
            n = n.with_action(label, route);
        }

    runtime.notifications().push(n);
    StatusCode::OK.into_response()
}

#[derive(Debug, Deserialize)]
struct SendNotificationRequest {
    title: Option<String>,
    message: String,
    #[serde(default = "default_notif_severity")]
    severity: String,
    #[serde(default = "default_notif_category")]
    category: String,
    action_label: Option<String>,
    action_route: Option<String>,
}
fn default_notif_severity() -> String { "info".into() }
fn default_notif_category() -> String { "plugin".into() }

async fn notifications_send(
    State(runtime): State<Arc<AgentRuntime>>,
    Json(req): Json<SendNotificationRequest>,
) -> Response {
    let title = req.title.unwrap_or_else(|| req.message.chars().take(50).collect());
    let sev = match req.severity.as_str() {
        "critical" => notification::Severity::Critical,
        "warning" => notification::Severity::Warning,
        _ => notification::Severity::Info,
    };
    let cat = match req.category.as_str() {
        "plugin" => notification::Category::Plugin,
        "idle" => notification::Category::Idle,
        "security" => notification::Category::Security,
        "workflow" => notification::Category::Workflow,
        "llm" => notification::Category::Llm,
        "skill" => notification::Category::Skill,
        _ => notification::Category::Plugin,
    };
    let n = notification::Notification {
        id: uuid::Uuid::now_v7().to_string(),
        severity: sev,
        category: cat,
        title,
        message: req.message,
        dismissed: false,
        dismissible: true,
        created_at: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as i64,
        event_id: None,
        source: Some("plugin:startup".into()),
        action_label: req.action_label,
        action_route: req.action_route,
    };
    runtime.notifications().push(n);
    StatusCode::OK.into_response()
}

#[instrument(skip(runtime))]
async fn metrics(State(runtime): State<Arc<AgentRuntime>>) -> Response {
    let bus = runtime.bus_metrics();
    let dlq_depth = runtime.dlq().depth();
    let plugin_states = {
        let loader = runtime.plugin_loader().await;
        loader
            .loaded_plugins()
            .into_iter()
            .map(|name| {
                let state = match loader.state_of(&name) {
                    Some(s) => format!("{s:?}"),
                    None => "unknown".to_owned(),
                };
                (name, state)
            })
            .collect::<Vec<_>>()
    };
    let sessions = runtime.workflow_engine().list_instances();
    let active_sessions = sessions
        .iter()
        .filter(|inst| inst.workflow_name == "message-session" && inst.current_state != "CLOSED")
        .count();
    runtime.metrics().update_from(
        bus,
        dlq_depth,
        runtime.inflight_pipelines(),
        runtime.inflight_skills(),
        &plugin_states,
        active_sessions,
    );
    let body = runtime.metrics().encode();
    (
        StatusCode::OK,
        [(axum::http::header::CONTENT_TYPE, "text/plain; version=0.0.4")],
        body,
    )
        .into_response()
}

#[derive(Debug, Deserialize)]
struct ConfigSetRequest {
    changed_fields: Vec<String>,
}

#[instrument(skip(runtime, headers), fields(fields = ?req.changed_fields))]
async fn config_set(
    State(runtime): State<Arc<AgentRuntime>>,
    headers: HeaderMap,
    Json(req): Json<ConfigSetRequest>,
) -> Response {
    let operator = operator_from_headers(&headers).unwrap_or(DEFAULT_OPERATOR);
    if let Some(response) = guard_confirmation(&runtime.audit(), operator, "config.set", "config", &headers) { return response; }
    runtime.log_config_change(operator, &req.changed_fields);
    (StatusCode::OK, Json(OkResponse { ok: true })).into_response()
}

#[instrument(skip(runtime), fields(event_id = %id))]
async fn event_dump(
    State(runtime): State<Arc<AgentRuntime>>,
    Path(id): Path<String>,
) -> Response {
    match runtime.event_store().get(&id) {
        Some(event) => (StatusCode::OK, Json(event)).into_response(),
        None => ApiError::not_found(format!("event not found: {id}"),).into_response(),
    }
}

#[derive(Debug, Clone, Serialize)]
struct TraceResponse {
    trace_id: String,
    events: Vec<Event>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cycle_detected: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cycle_path: Option<Vec<String>>,
}

fn detect_trace_cycle(events: &[Event]) -> (bool, Vec<String>) {
    use std::collections::HashSet;
    let by_id: std::collections::HashMap<Uuid, &Event> =
        events.iter().map(|e| (e.id, e)).collect();
    for start in events.iter() {
        let mut visited = HashSet::new();
        let mut path = Vec::new();
        let mut current = Some(start.id);
        while let Some(id) = current {
            if !visited.insert(id) {
                return (true, path.iter().map(|u: &Uuid| u.to_string()).collect());
            }
            path.push(id);
            current = by_id
                .get(&id)
                .and_then(|e| e.metadata.parent_event_id);
        }
    }
    (false, Vec::new())
}

#[instrument(skip(runtime), fields(trace_id = %trace_id))]
async fn event_trace(
    State(runtime): State<Arc<AgentRuntime>>,
    Path(trace_id): Path<String>,
) -> Response {
    let events = runtime.event_store().trace(&trace_id);
    if events.is_empty() {
        return ApiError::not_found(format!("trace not found: {trace_id}"),).into_response();
    }
    let (cycle_detected, cycle_path) = detect_trace_cycle(&events);
    (
        StatusCode::OK,
        Json(TraceResponse {
            trace_id,
            events,
            cycle_detected: if cycle_detected { Some(true) } else { None },
            cycle_path: if cycle_path.is_empty() { None } else { Some(cycle_path) },
        }),
    )
        .into_response()
}

#[derive(Debug, Clone, Deserialize)]
struct AuditLogQuery {
    action: Option<String>,
    operator: Option<String>,
    since_ms: Option<i64>,
    until_ms: Option<i64>,
    limit: Option<usize>,
    offset: Option<usize>,
}

#[instrument(skip(runtime, query))]
async fn audit_log(
    State(runtime): State<Arc<AgentRuntime>>,
    axum::extract::Query(query): axum::extract::Query<AuditLogQuery>,
) -> Response {
    let items = runtime.audit().list(
        query.action.as_deref(),
        query.operator.as_deref(),
        query.since_ms,
        query.until_ms,
        query.offset.unwrap_or(0),
        query.limit.unwrap_or(200).min(1_000),
    );
    (StatusCode::OK, Json(items)).into_response()
}

async fn require_api_token(
    State(runtime): State<Arc<AgentRuntime>>,
    request: axum::http::Request<axum::body::Body>,
    next: middleware::Next,
) -> Response {
    let Some(expected) = runtime.api_token() else {
        return next.run(request).await;
    };

    let headers = request.headers();
    let provided = parse_bearer(headers).or_else(|| {
        headers
            .get("x-aman-token")
            .and_then(|h| h.to_str().ok())
            .map(str::to_owned)
    });
    if provided.as_deref() != Some(expected) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    next.run(request).await
}

// ── Runtime status ───────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
struct RuntimeStatusResponse {
    phase: u8,
    ready: bool,
    live: bool,
}

async fn runtime_status(State(runtime): State<Arc<AgentRuntime>>) -> Response {
    Json(RuntimeStatusResponse {
        phase: runtime.phase() as u8,
        ready: runtime.is_ready(),
        live: runtime.is_live(),
    })
    .into_response()
}

#[derive(Debug, Clone, Serialize)]
struct RuntimeConfigResponse {
    bind_addr: String,
    runtime_dir: String,
    skills_dir: String,
    api_token_configured: bool,
    risky_capabilities_enabled: bool,
    /// Drain timeout (seconds) — the desktop uses `drain_timeout_sec * 2`
    /// as its graceful-shutdown POST timeout so it doesn't SIGKILL a
    /// gateway that is merely slow to drain.
    drain_timeout_sec: u64,
}

async fn runtime_config(State(runtime): State<Arc<AgentRuntime>>) -> Response {
    Json(RuntimeConfigResponse {
        bind_addr: runtime.bind_addr().to_string(),
        runtime_dir: runtime.runtime_dir().display().to_string(),
        skills_dir: runtime.skills_dir().display().to_string(),
        api_token_configured: runtime.api_token().is_some(),
        risky_capabilities_enabled: runtime.risky_capabilities_enabled(),
        drain_timeout_sec: runtime.runtime_cfg().drain_timeout_sec,
    })
    .into_response()
}

// ── Soul endpoints ───────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
struct SoulInfoResponse {
    name: String,
    last_changed_at: Option<i64>,
}

async fn soul_info(State(runtime): State<Arc<AgentRuntime>>) -> Response {
    let Some(soul) = runtime.soul_runtime() else {
        return ApiError::not_found("no soul configured".to_owned(),).into_response();
    };
    let current = soul.current_soul();
    let changed = soul.last_soul_changed_event();
    Json(SoulInfoResponse {
        name: current.name.clone(),
        last_changed_at: changed.as_ref().map(|e| e.timestamp.as_millis()),
    })
    .into_response()
}

#[derive(Debug, Clone, Serialize)]
struct SoulRawResponse {
    raw: String,
}

async fn soul_raw(State(runtime): State<Arc<AgentRuntime>>) -> Response {
    let Some(soul) = runtime.soul_runtime() else {
        return ApiError::not_found("no soul configured".to_owned(),).into_response();
    };
    Json(SoulRawResponse {
        raw: soul.current_soul().raw.clone(),
    })
    .into_response()
}

#[derive(Debug, Clone, Deserialize)]
struct SoulUpdateRequest {
    content: String,
}

async fn soul_update(
    State(runtime): State<Arc<AgentRuntime>>,
    headers: HeaderMap,
    Json(req): Json<SoulUpdateRequest>,
) -> Response {
    let operator = operator_from_headers(&headers).unwrap_or(DEFAULT_OPERATOR);
    match runtime.update_soul(&req.content).await {
        Ok(()) => {
            runtime
                .audit()
                .record(operator, "soul.update", "soul", "ok", "");
            (StatusCode::OK, Json(OkResponse { ok: true })).into_response()
        }
        Err(error) => {
            runtime.audit().record(
                operator,
                "soul.update",
                "soul",
                "error",
                error.to_string(),
            );
            ApiError::from(error).into_response()
        }
    }
}

// ── Capabilities ─────────────────────────────────────────────────────────

async fn capability_list(State(runtime): State<Arc<AgentRuntime>>) -> Response {
    let entries = runtime.get_capability_entries().await;
    (StatusCode::OK, Json(json!(entries))).into_response()
}

// ── DLQ depth ────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
struct DlqDepthResponse {
    depth: usize,
}

async fn tool_auth_respond(
    State(runtime): State<Arc<AgentRuntime>>,
    Json(body): Json<ToolAuthRespondBody>,
) -> Response {
    let resolved = runtime
        .auth_registry()
        .resolve(&body.auth_id, body.approved);
    if resolved {
        (StatusCode::OK, Json(OkResponse { ok: true })).into_response()
    } else {
        ApiError::not_found("auth_id not found or already expired").into_response()
    }
}

// ── Plugin capability approval ───────────────────────────────────────────

/// List all pending plugin capability approval requests.
/// GET /plugin-auth/pending
///
/// Returns a JSON array of objects with `plugin_name`, `version`,
/// `capabilities_summary`, and `capabilities`. An empty array means
/// no plugins are awaiting approval.
async fn plugin_auth_pending(
    State(runtime): State<Arc<AgentRuntime>>,
) -> Response {
    let pending: Vec<crate::runtime::agent_runtime::PendingApprovalInfo> =
        runtime.pending_plugin_approvals_list().await;
    (StatusCode::OK, Json(pending)).into_response()
}

async fn plugin_auth_respond(
    State(runtime): State<Arc<AgentRuntime>>,
    Json(body): Json<PluginAuthRespondBody>,
) -> Response {
    let plugin_name = &body.plugin_name;

    // Resolve the oneshot (unblocks any waiter, though the desktop flow
    // is fire-and-forget so there may not be one).
    runtime
        .plugin_approval_registry()
        .resolve(plugin_name, body.approved);

    if body.approved {
        // Take the pending candidate
        let candidate: Option<(PluginCandidate, CapabilitySet)> =
            runtime.take_pending_plugin_candidate(plugin_name).await;

        match candidate {
            Some((candidate, approved_caps)) => {
                // Persist the approval with a BLAKE3 keyed-hash signature
                if let Some(cache) = runtime.approval_cache() {
                    let now_ms = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_millis() as u64;
                    let mut caps = ApprovedCapabilities {
                        plugin_version: plugin::plugin_manifest_version(&candidate).to_string(),
                        capabilities: approved_caps.clone(),
                        approved_at_ms: now_ms,
                        approved_by: "user".to_owned(),
                        signature: String::new(),
                    };
                    if let Err(e) = cache.save(plugin_name, &mut caps) {
                        tracing::error!(
                            plugin = %plugin_name,
                            error = %e,
                            "failed to persist plugin capability approval"
                        );
                        return ApiError::internal(format!("failed to save approval: {e}")).into_response();
                    }
                    tracing::info!(
                        plugin = %plugin_name,
                        "plugin capability approval persisted with BLAKE3 signature"
                    );
                }

                // Dynamically load the approved plugin
                let mut loader = runtime.plugin_loader().await;
                match loader.load_plugin(candidate).await {
                    Ok(()) => {
                        tracing::info!(
                            plugin = %plugin_name,
                            "plugin loaded after user capability approval"
                        );
                        (StatusCode::OK, Json(OkResponse { ok: true })).into_response()
                    }
                    Err(e) => {
                        tracing::error!(
                            plugin = %plugin_name,
                            error = %e,
                            "failed to load plugin after approval"
                        );
                        ApiError::internal(format!("failed to load plugin: {e}")).into_response()
                    }
                }
            }
            None => {
                tracing::warn!(
                    plugin = %plugin_name,
                    "plugin_auth_respond: no pending candidate found"
                );
                ApiError::not_found("no pending approval found for plugin").into_response()
            }
        }
    } else {
        // User denied — remove from pending, don't load
        let removed: bool = runtime.remove_pending_plugin_candidate(plugin_name).await;
        tracing::info!(
            plugin = %plugin_name,
            removed,
            "plugin capability approval denied by user"
        );
        (StatusCode::OK, Json(OkResponse { ok: true })).into_response()
    }
}

async fn tool_execute(
    State(runtime): State<Arc<AgentRuntime>>,
    Path(name): Path<String>,
    Json(params): Json<Value>,
) -> Response {
    let tools = runtime.tools();
    let tool = match tools.get(&name) {
        Some(t) => t,
        None => {
            return ApiError::not_found(format!("tool not found: {name}")).into_response();
        }
    };
    let ctx = ToolContext {
        base: BaseContext::new(TraceId::default()),
        tool_name: Some(name.clone()),
        working_directory: None,
    };
    let started = std::time::Instant::now();
    match tool.execute(params, ctx).await {
        Ok(output) => Json(json!({
            "tool": name,
            "duration_ms": started.elapsed().as_millis(),
            "output": output,
        })).into_response(),
        Err(e) => ApiError::bad_request(e.to_string()).into_response(),
    }
}

// ── Explore endpoint ───────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize)]
struct ExploreStartRequest {
    #[serde(default)]
    agent_key: Option<String>,
}

async fn publish_explore_reply(runtime: &Arc<AgentRuntime>, session_id: &str, agent_id: &str, text: &str) {
    let event = Event::new(
        "explore:pipeline",
        EventType::Custom("agent:reply_ready".to_owned()),
        json!({
            "agent_id": agent_id,
            "session_id": session_id,
            "reply": text,
            "turns_processed": 0,
        }),
    );
    // Publish to global bus for persistence (JSONL) and frontend polling.
    let _ = runtime.publish_event(event.clone()).await;
    // Also publish to agent's local bus so AgentIdleManager sees the
    // busy→empty transition and produces Idle events → Reflection during sleep.
    let _ = runtime.publish_event_to_agent(agent_id, event).await;
}

async fn explore_start(
    State(runtime): State<Arc<AgentRuntime>>,
    Json(payload): Json<Value>,
) -> Response {
    let req = ExploreStartRequest {
        agent_key: payload.get("agent_key").and_then(|v| v.as_str()).map(String::from),
    };

    // Resolve agent key
    let agent_id = match req.agent_key.or_else(|| {
        config::AmanConfig::from_default_path()
            .ok()
            .and_then(|c| c.agents.into_keys().next())
    }) {
        Some(k) => k,
        None => {
            return ApiError::bad_request("no agent configured — specify agent_key or add an agent to config.yaml").into_response();
        }
    };

    // Read info-hub config and pick a random source
    let aman_cfg = match config::AmanConfig::from_default_path() {
        Ok(c) => c,
        Err(e) => {
            return ApiError::internal(format!("failed to read config: {e}")).into_response();
        }
    };

    let info_hub_config: info_hub::config::InfoHubConfig = aman_cfg
        .info_hub
        .as_ref()
        .and_then(|v| serde_json::from_value(v.clone()).ok())
        .unwrap_or_default();

    let sources = info_hub_config.sources;
    if sources.is_empty() {
        return ApiError::bad_request("no info-hub data sources configured").into_response();
    }

    let mut rng = rand::thread_rng();
    let source_name = sources[rng.gen_range(0..sources.len())].name().to_string();

    // Create session
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let data = json!({
        "session_type": "persistent",
        "version": 0,
        "created_at": now_ms,
        "last_active_at": now_ms,
    });

    let instance = match runtime.workflow_engine().create_instance("message-session", data) {
        Ok(i) => i,
        Err(e) => {
            return ApiError::internal(format!("failed to create session: {e}")).into_response();
        }
    };
    let session_id = instance.id.clone();

    // Persist session
    if let Some(store) = runtime.session_store_for_agent(&agent_id) {
        let _ = store.upsert(&session_store::SessionRecord {
            id: session_id.clone(),
            agent_id: agent_id.clone(),
            state: instance.current_state.clone(),
            message_count: 0,
            created_at: now_ms as i64,
            last_active_at: now_ms as i64,
            session_type: "persistent".to_owned(),
            reflected_at: None,
            title: None,
        });
    }

    // Spawn exploration pipeline in background.
    // Progress events are published during execution and polled by the frontend.
    let rt = runtime.clone();
    let sid = session_id.clone();
    let aid = agent_id.clone();
    let src = source_name.clone();
    tokio::spawn(async move {
        explore_pipeline(rt, sid, aid, src).await;
    });

    (
        StatusCode::OK,
        Json(json!({
            "session_id": session_id,
            "source": source_name,
        })),
    )
        .into_response()
}

async fn explore_pipeline(
    runtime: Arc<AgentRuntime>,
    session_id: String,
    agent_id: String,
    source_name: String,
) {
    // Guard: sync session message_count on all exit paths so reflection
    // can pick up the session even when explore ends early on error.
    let sync_session = |sid: &str| {
        if let Some(store) = runtime.session_store() {
            let now_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as i64;
            let _ = store.sync_message_count(sid, now_ms);
        }
    };

    // Publish initial message
    publish_explore_reply(
        &runtime,
        &session_id,
        &agent_id,
        &format!(
            "🔍 **Exploration Started**\n\nRandomly selected data source: **{source_name}**\nSearching for latest items…"
        ),
    )
    .await;

    // Phase 2: info_search
    let tools = runtime.tools();
    let info_search = match tools.get("info_search") {
        Some(t) => t,
        None => {
            publish_explore_reply(&runtime, &session_id, &agent_id, "❌ **Error**: info_search tool not available").await;
            sync_session(&session_id);
            return;
        }
    };

    let ctx = ToolContext {
        base: BaseContext::new(TraceId::new()),
        tool_name: Some("info_search".to_string()),
        working_directory: None,
    };

    let search_params = json!({
        "query": "",
        "limit": 20,
        "sources": [source_name],
    });

    let search_output = match info_search.execute(search_params, ctx.clone()).await {
        Ok(v) => v,
        Err(e) => {
            publish_explore_reply(&runtime, &session_id, &agent_id, &format!("❌ **Search failed**: {e}")).await;
            sync_session(&session_id);
            return;
        }
    };

    let items: Vec<Value> = search_output.as_array().cloned().unwrap_or_default();
    let items_found = items.len();

    if items_found == 0 {
        publish_explore_reply(
            &runtime,
            &session_id,
            &agent_id,
            &format!("📋 **No items found** from **{source_name}**.\n\n✨ Exploration complete (no results)."),
        ).await;
        sync_session(&session_id);
        return;
    }

    // Build title list
    let mut title_list = format!("📋 **Found {items_found} items from {source_name}**\n\n");
    for (i, item) in items.iter().enumerate() {
        let title = item.get("title").and_then(|v| v.as_str()).unwrap_or("(untitled)");
        let url = item.get("url").and_then(|v| v.as_str()).unwrap_or("");
        if url.is_empty() {
            title_list.push_str(&format!("{}. {title}\n", i + 1));
        } else {
            title_list.push_str(&format!("{}. [{title}]({url})\n", i + 1));
        }
    }
    publish_explore_reply(&runtime, &session_id, &agent_id, &title_list).await;

    // Build ArticleInput list for tagging
    let mut articles: Vec<info_hub::ai::ArticleInput> = items
        .iter()
        .enumerate()
        .map(|(i, item)| info_hub::ai::ArticleInput {
            index: i,
            title: item.get("title").and_then(|v| v.as_str()).unwrap_or("").to_string(),
            description: item.get("summary").and_then(|v| v.as_str()).unwrap_or("").to_string(),
            source_name: item.get("source").and_then(|v| v.as_str()).unwrap_or(&source_name).to_string(),
            link: item.get("url").and_then(|v| v.as_str()).unwrap_or("").to_string(),
            category: String::new(),
            keywords: vec![],
            relevance: 0,
            quality: 0,
            timeliness: 0,
        })
        .collect();

    // Phase 3: info_tag_articles
    let tag_tool = tools.get("info_tag_articles");
    if tag_tool.is_none() {
        publish_explore_reply(&runtime, &session_id, &agent_id, "⚠️ Tagging tool not available, skipping…").await;
    }

    if let Some(tag_tool) = tag_tool {
        publish_explore_reply(&runtime, &session_id, &agent_id, &format!("🏷 **Tagging {items_found} articles…**")).await;
        let tag_params = json!({
            "articles": articles.iter().map(|a| json!({
                "index": a.index,
                "title": a.title,
                "description": a.description,
                "source_name": a.source_name,
                "link": a.link,
            })).collect::<Vec<_>>(),
        });
        match tag_tool.execute(tag_params, ctx.clone()).await {
            Ok(v) => {
                let results: Vec<info_hub::ai::TagResult> = v
                    .get("results")
                    .and_then(|r: &Value| serde_json::from_value(r.clone()).ok())
                    .unwrap_or_default();
                // Merge tags into articles
                for tag in &results {
                    if tag.index < articles.len() {
                        articles[tag.index].category = tag.category.clone();
                        articles[tag.index].keywords = tag.keywords.clone();
                    }
                }
                let cat_summary: Vec<String> = results.iter().map(|t: &info_hub::ai::TagResult| t.category.clone()).filter(|c: &String| !c.is_empty()).collect();
                publish_explore_reply(
                    &runtime,
                    &session_id,
                    &agent_id,
                    &format!("✅ **Tagged {} articles**\nCategories: {}", results.len(), cat_summary.join(", ")),
                ).await;
            }
            Err(e) => {
                publish_explore_reply(&runtime, &session_id, &agent_id, &format!("⚠️ Tagging failed: {e}")).await;
            }
        }
    }

    // Phase 4: info_score_articles
    let score_tool = tools.get("info_score_articles");
    if score_tool.is_none() {
        publish_explore_reply(&runtime, &session_id, &agent_id, "⚠️ Scoring tool not available, skipping…").await;
    }

    if let Some(score_tool) = score_tool {
        publish_explore_reply(&runtime, &session_id, &agent_id, &format!("📊 **Scoring {items_found} articles…**")).await;
        let score_params = json!({
            "articles": articles.iter().map(|a| {
                let mut j = json!({
                    "index": a.index,
                    "title": a.title,
                    "description": a.description,
                    "source_name": a.source_name,
                    "link": a.link,
                });
                if !a.category.is_empty() {
                    j["category"] = json!(a.category);
                    j["keywords"] = json!(a.keywords);
                }
                j
            }).collect::<Vec<_>>(),
        });
        match score_tool.execute(score_params, ctx.clone()).await {
            Ok(v) => {
                let results: Vec<info_hub::ai::ScoreResult> = v
                    .get("results")
                    .and_then(|r: &Value| serde_json::from_value(r.clone()).ok())
                    .unwrap_or_default();
                for sc in &results {
                    if sc.index < articles.len() {
                        articles[sc.index].relevance = sc.relevance;
                        articles[sc.index].quality = sc.quality;
                        articles[sc.index].timeliness = sc.timeliness;
                    }
                }
                let high_count = articles.iter().filter(|a: &&info_hub::ai::ArticleInput| a.total_score() > 20).count();
                publish_explore_reply(
                    &runtime,
                    &session_id,
                    &agent_id,
                    &format!(
                        "✅ **Scored {} articles**\nScore range: {}-{} / 30\n**{} articles above 20/30**",
                        results.len(),
                        articles.iter().map(|a: &info_hub::ai::ArticleInput| a.total_score()).min().unwrap_or(0),
                        articles.iter().map(|a: &info_hub::ai::ArticleInput| a.total_score()).max().unwrap_or(0),
                        high_count,
                    ),
                ).await;
            }
            Err(e) => {
                publish_explore_reply(&runtime, &session_id, &agent_id, &format!("⚠️ Scoring failed: {e}")).await;
            }
        }
    }

    // Phase 5: info_summarize_articles for high-scoring items
    let high_score_articles: Vec<&info_hub::ai::ArticleInput> = articles
        .iter()
        .filter(|a| a.total_score() > 20)
        .collect();

    let items_summarized = high_score_articles.len();

    if items_summarized > 0 {
        let summarize_tool = match tools.get("info_summarize_articles") {
            Some(t) => t,
            None => {
                publish_explore_reply(&runtime, &session_id, &agent_id, "⚠️ Summarization tool not available").await;
                sync_session(&session_id);
                return;
            }
        };

        publish_explore_reply(
            &runtime,
            &session_id,
            &agent_id,
            &format!("📝 **Summarizing {items_summarized} high-score articles…**"),
        ).await;

        let summary_articles: Vec<Value> = high_score_articles
            .iter()
            .map(|a| {
                json!({
                    "index": a.index,
                    "title": a.title,
                    "description": a.description,
                    "source_name": a.source_name,
                    "link": a.link,
                    "relevance": a.relevance,
                    "quality": a.quality,
                    "timeliness": a.timeliness,
                })
            })
            .collect();

        let summary_params = json!({
            "articles": summary_articles,
            "lang": "zh",
            "min_score": 1,
        });

        match summarize_tool.execute(summary_params, ctx.clone()).await {
            Ok(v) => {
                let summary_results: Vec<info_hub::ai::SummaryResult> = v
                    .get("results")
                    .and_then(|r: &Value| serde_json::from_value(r.clone()).ok())
                    .unwrap_or_default();

                for sr in &summary_results {
                    let a = high_score_articles
                        .iter()
                        .find(|a| a.index == sr.index)
                        .copied()
                        .unwrap_or(high_score_articles[0]);

                    let tags_display = if a.category.is_empty() && a.keywords.is_empty() {
                        String::from("—")
                    } else {
                        let mut parts = vec![];
                        if !a.category.is_empty() {
                            parts.push(a.category.clone());
                        }
                        parts.extend(a.keywords.clone());
                        parts.join(" · ")
                    };

                    let msg = if a.link.is_empty() {
                        format!(
                            "📝 **{title}**\n🏷 {tags}\n📊 Score: {total}/30 (R:{rel} Q:{qual} T:{time})\n\n💡 {summary}",
                            title = sr.title_zh,
                            tags = tags_display,
                            total = a.total_score(),
                            rel = a.relevance,
                            qual = a.quality,
                            time = a.timeliness,
                            summary = sr.summary,
                        )
                    } else {
                        format!(
                            "📝 **[{title}]({url})**\n🏷 {tags}\n📊 Score: {total}/30 (R:{rel} Q:{qual} T:{time})\n\n💡 {summary}",
                            title = sr.title_zh,
                            url = a.link,
                            tags = tags_display,
                            total = a.total_score(),
                            rel = a.relevance,
                            qual = a.quality,
                            time = a.timeliness,
                            summary = sr.summary,
                        )
                    };
                    publish_explore_reply(&runtime, &session_id, &agent_id, &msg).await;
                }
            }
            Err(e) => {
                publish_explore_reply(&runtime, &session_id, &agent_id, &format!("⚠️ Summarization failed: {e}")).await;
            }
        }
    }

    // Phase 6: complete
    publish_explore_reply(
        &runtime,
        &session_id,
        &agent_id,
        &format!(
            "✨ **Exploration complete!**\n\n{items_found} items found from **{source_name}**\n{items_summarized} articles summarized (scored > 20/30)\n\n_Reflection will process this session during the next sleep cycle._"
        ),
    ).await;

    // Guard: ensure reflection can pick up the session.
    sync_session(&session_id);
}

async fn dlq_depth(State(runtime): State<Arc<AgentRuntime>>) -> Response {
    Json(DlqDepthResponse {
        depth: runtime.dlq().depth(),
    })
    .into_response()
}

// ── Chat endpoints ───────────────────────────────────────────────────────

async fn chat_session_create(
    State(runtime): State<Arc<AgentRuntime>>,
    headers: HeaderMap,
    Json(payload): Json<Value>,
) -> Response {
    let operator = operator_from_headers(&headers).unwrap_or(DEFAULT_OPERATOR);
    let session_type = payload
        .get("session_type")
        .and_then(|v| v.as_str())
        .unwrap_or("persistent");
    let agent_id = payload
        .get("agent_id")
        .and_then(|v| v.as_str())
        .unwrap_or("aman");

    match runtime.session_manager().create_session(operator, agent_id, session_type).await {
        Ok(id) => (StatusCode::OK, Json(json!({ "id": id }))).into_response(),
        Err(error) => ApiError::from(error).into_response(),
    }
}

#[derive(Debug, Clone, Serialize)]
struct ChatSessionItem {
    id: String,
    session_type: String,
    state: String,
    created_at: u64,
    last_active_at: u64,
    version: u64,
    message_count: u64,
    agent_id: String,
}

#[derive(Debug, Deserialize)]
struct ChatSessionsQuery {
    #[serde(default)]
    agent_id: Option<String>,
}

async fn chat_sessions(
    State(runtime): State<Arc<AgentRuntime>>,
    axum::extract::Query(query): axum::extract::Query<ChatSessionsQuery>,
) -> Response {
    let instances = runtime.workflow_engine().list_instances();
    let mut items: Vec<ChatSessionItem> = instances
        .into_iter()
        .filter(|inst| {
            inst.workflow_name == "message-session"
                && query.agent_id.as_ref().is_none_or(|aid| {
                    inst.data.get("agent_id")
                        .and_then(|v| v.as_str())
                        .is_some_and(|id| id == aid)
                })
        })
        .map(|inst| {
            let session_type = inst
                .data
                .get("session_type")
                .and_then(|v| v.as_str())
                .unwrap_or("persistent")
                .to_owned();
            let created_at = inst
                .data
                .get("created_at")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let last_active_at = inst
                .data
                .get("last_active_at")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let version = inst
                .data
                .get("version")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let message_count = inst
                .data
                .get("message_count")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let agent_id = inst
                .data
                .get("agent_id")
                .and_then(|v| v.as_str())
                .unwrap_or("aman")
                .to_owned();
            ChatSessionItem {
                id: inst.id,
                session_type,
                state: inst.current_state,
                created_at,
                last_active_at,
                version,
                message_count,
                agent_id,
            }
        })
        .collect();

    // Fall back to SQLite when no instances are in memory (e.g. after restart).
    if items.is_empty() {
        let stores = runtime.agent_registry().all_session_stores().await;
        for store in &stores {
            if let Ok(records) = store.list_all() {
                for rec in records {
                    // Respect agent_id filter when reading from stores.
                    if query.agent_id.as_ref().is_some_and(|aid| rec.agent_id != *aid) {
                        continue;
                    }
                    items.push(ChatSessionItem {
                        id: rec.id,
                        session_type: rec.session_type,
                        state: rec.state,
                        created_at: rec.created_at as u64,
                        last_active_at: rec.last_active_at as u64,
                        version: 0,
                        message_count: rec.message_count as u64,
                        agent_id: rec.agent_id,
                    });
                }
            }
        }
    }

    items.sort_by_key(|b| std::cmp::Reverse(b.last_active_at));
    (StatusCode::OK, Json(json!({ "items": items }))).into_response()
}

async fn chat_session_delete(
    State(runtime): State<Arc<AgentRuntime>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    let operator = operator_from_headers(&headers).unwrap_or(DEFAULT_OPERATOR);
    // Delete from persistent store first. Track whether the store
    // actually found the session — the WorkflowEngine may not know about
    // it (e.g. after a gateway restart), so the store is authoritative
    // for determining "not found."
    let store_deleted = runtime
        .find_session_store(&id)
        .map(|store| store.delete(&id).unwrap_or(0) > 0)
        .unwrap_or(false);
    let engine_deleted = match runtime.workflow_engine().delete_instance(&id) {
        Ok(deleted) => deleted,
        Err(error) => {
            runtime.audit().record(
                operator,
                "chat.session.delete",
                format!("session:{id}"),
                "error",
                error.to_string(),
            );
            return ApiError::from(error).into_response();
        }
    };

    if engine_deleted || store_deleted {
        runtime.audit().record(
            operator,
            "chat.session.delete",
            format!("session:{id}"),
            "ok",
            "",
        );
        (StatusCode::OK, Json(json!({ "ok": true }))).into_response()
    } else {
        runtime.audit().record(
            operator,
            "chat.session.delete",
            format!("session:{id}"),
            "not_found",
            "",
        );
        ApiError::not_found(format!("session not found: {id}"),).into_response()
    }
}

#[derive(Debug, Clone, Serialize)]
struct ChatSessionStateResponse {
    id: String,
    state: String,
    version: u64,
    messages: Vec<Value>,
}

async fn chat_session_state(
    State(runtime): State<Arc<AgentRuntime>>,
    Path(id): Path<String>,
) -> Response {
    // Try to get the session from the WorkflowEngine (in-memory, running sessions).
    let (state, version, messages) =
        if let Some(instance) = runtime.workflow_engine().get_instance(&id) {
            let version = instance
                .data
                .get("version")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let messages = runtime
                .event_store()
                .recent(2000)
                .into_iter()
                .filter(|e| {
                    e.payload
                        .get("session_id")
                        .and_then(|v| v.as_str())
                        .is_some_and(|sid| sid == id)
                })
                .map(|e| {
                    json!({
                        "event_id": e.id.to_string(),
                        "event_type": format!("{:?}", e.event_type),
                        "source": e.source,
                        "timestamp_ms": e.timestamp.as_millis(),
                        "payload": e.payload,
                    })
                })
                .collect::<Vec<_>>();
            (instance.current_state, version, messages)
        } else if let Some(store) = runtime.find_session_store(&id) {
            // Not in memory — try to restore from persisted JSONL.
            let stored = store.load_session_events(&id);
            let version = stored.len() as u64;
            ("closed".to_owned(), version, stored)
        } else {
            return ApiError::not_found(format!("session not found: {id}"),).into_response();
        };

    Json(ChatSessionStateResponse {
        id,
        state,
        version,
        messages,
    })
    .into_response()
}

async fn chat_session_history(
    State(runtime): State<Arc<AgentRuntime>>,
    Path(id): Path<String>,
) -> Response {
    let messages = {
        let from_store = runtime
            .event_store()
            .recent(2000)
            .into_iter()
            .filter(|e| {
                e.payload
                    .get("session_id")
                    .and_then(|v| v.as_str())
                    .is_some_and(|sid| sid == id)
            })
            .map(|e| {
                json!({
                    "event_id": e.id.to_string(),
                    "event_type": format!("{:?}", e.event_type),
                    "source": e.source,
                    "timestamp_ms": e.timestamp.as_millis(),
                    "payload": e.payload,
                })
            })
            .collect::<Vec<_>>();

        if !from_store.is_empty() {
            from_store
        } else if let Some(store) = runtime.find_session_store(&id) {
            store.load_session_events(&id)
        } else {
            from_store
        }
    };

    (StatusCode::OK, Json(json!({ "messages": messages }))).into_response()
}

#[derive(Debug, Clone, Deserialize)]
struct ChatSendRequest {
    text: String,
    #[serde(default)]
    expected_version: Option<u64>,
}

async fn chat_session_send(
    State(runtime): State<Arc<AgentRuntime>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(req): Json<ChatSendRequest>,
) -> Response {
    let operator = operator_from_headers(&headers).unwrap_or(DEFAULT_OPERATOR);

    // Validate session exists and check optimistic lock version.
    let instance = match runtime.workflow_engine().get_instance(&id) {
        Some(inst) => inst,
        None => {
            // Session not in memory — try to restore from persisted JSONL.
            match runtime.restore_chat_session(&id).await {
                Some(()) => runtime.workflow_engine().get_instance(&id).expect("just restored"),
                None => {
                    return ApiError::not_found(format!("session not found: {id}"),).into_response();
                }
            }
        }
    };
    let current_ver = instance
        .data
        .get("version")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    if let Some(expected) = req.expected_version
        && current_ver != expected {
            return ApiError::conflict(format!(
                        "version conflict: expected {}, got {}",
                        expected, current_ver
                    ),).into_response();
        }

    // Sanitize input (substring-based, 3-tier).
    let sanitizer = InputSanitizer::new();
    let text = match sanitizer.sanitize(&req.text) {
        SanitizeResult::Block { matched_patterns } => {
            runtime.audit().record(
                operator,
                "chat.send_message",
                format!("session:{id}"),
                "blocked",
                format!("matched:{}", matched_patterns.join(",")),
            );
            return ApiError::bad_request(format!("Message blocked: matched {}", matched_patterns.join(", ")),).into_response();
        }
        SanitizeResult::ReplaceMessage { matched_patterns } => {
            runtime.audit().record(
                operator,
                "chat.send_message",
                format!("session:{id}"),
                "sanitized_replace",
                format!("matched:{}", matched_patterns.join(",")),
            );
            "[message blocked by content policy]".to_owned()
        }
        SanitizeResult::ReplaceToken {
            sanitized,
            matched_patterns,
        } => {
            runtime.audit().record(
                operator,
                "chat.send_message",
                format!("session:{id}"),
                "sanitized_token",
                format!(
                    "matched:{},sanitized_len:{}",
                    matched_patterns.join(","),
                    sanitized.len()
                ),
            );
            sanitized
        }
        SanitizeResult::PassThrough => req.text.clone(),
    };

    // Second pass: regex-based injection detection (from secret::InputSanitizer).
    // Catches patterns the substring sanitizer may miss (e.g. "ignore  all\nprevious").
    let injection_detector = kernel::sanitizer::InjectionDetector::new();
    if let Some(warning) = injection_detector.detect_injection(&text) {
        runtime.audit().record(
            operator,
            "chat.send_message",
            format!("session:{id}"),
            "injection_detected",
            format!("pattern:{}", warning.message),
        );
        tracing::warn!(
            session_id = %id,
            pattern = %warning.pattern,
            message = %warning.message,
            "prompt injection detected by regex detector"
        );
    }

    // NOTE: session resumption does NOT depend on pattern-matching the user's
    // text (e.g. "继续"). `process_message_v2` restores the full session
    // history from the persisted JSONL whenever the in-memory history is empty
    // (after a timeout, max-turns abort, or gateway restart), and hands that
    // history plus this message straight to the LLM.

    // Detect slash-command skill invocation (e.g. "/btc-bottom-model should I buy?").
    // When a skill is invoked directly by the user, load the full SKILL.md body and
    // inject it into the message so the LLM can follow the methodology immediately
    // without a separate skill_view tool call.
    // Phase 3: Python self-module bridge for command parsing.
    let self_bridge = runtime.self_bridge().clone();
    let maybe_skill = self_bridge.parse_skill_command(&text);
    let (effective_text, skill_context) = match maybe_skill {
        Some((skill_name, user_input)) => {
            match skill::execution::prepare_skill_execution(
                &skill_name,
                &user_input,
                &runtime.llm_skills(),
            ) {
                Some(exec) => {
                    let ctx = Some(json!({
                        "skill_name": exec.skill_name,
                        "user_input": exec.user_input,
                        "augmented_message": exec.augmented_message,
                    }));
                    (exec.augmented_message, ctx)
                }
                None => (text.clone(), None),
            }
        }
        None => (text.clone(), None),
    };

    // Mark agent as actively chatting — will be reset by idle system when bus empties.
    let chat_agent_id = instance
        .data
        .get("agent_id")
        .and_then(|v| v.as_str())
        .unwrap_or("aman");
    runtime.agent_registry().set_system_state(chat_agent_id, AgentSystemState::Chatting).await;

    // Build the complete system prompt once per session: soul + skills + tools + date.
    // Cached via SessionManager so LLM prompt caching stays effective across turns.
    // No per-turn reassembly — execute_turn() reads the cached prompt directly.
    let combined_prompt = {
        let llm_skills = runtime.llm_skills();
        let self_bridge = runtime.self_bridge().clone();
        let agent_registry = runtime.agent_registry();
        let tool_registry = runtime.tools();
        let agent_id = chat_agent_id.to_owned();
        // Build tool descriptors once — reused by both the soul and fallback paths.
        let tool_descriptors: Vec<kernel::react::ToolDescriptor> = tool_registry
            .list_tools()
            .into_iter()
            .filter(|name| !name.starts_with("llm_") && !name.starts_with("llm_provider_"))
            .filter(|name| pollster::block_on(agent_registry.tool_allowed(&agent_id, name)))
            .filter_map(|name| tool_registry.get(&name))
            .map(|tool| kernel::react::ToolDescriptor {
                name: tool.name().to_owned(),
                description: tool.description().to_owned(),
                parameters: serde_json::to_value(tool.parameters()).unwrap_or_default(),
            })
            .collect();
        // Per-agent soul: load from ~/.aman/agents/<agent_id>/SOUL.md
        let per_agent_soul_path = super::agent_seed::aman_data_dir()
            .join("agents")
            .join(&agent_id)
            .join("SOUL.md");
        let per_agent_soul = if per_agent_soul_path.exists() {
            std::fs::read_to_string(&per_agent_soul_path).ok()
        } else {
            None
        };
        runtime.session_manager().get_system_prompt(&id, || {
            // Prefer per-agent soul file, fall back to global soul_runtime (--soul flag).
            let soul_raw: String = if let Some(s) = per_agent_soul {
                s
            } else if let Some(sr) = runtime.soul_runtime() {
                sr.current_soul().raw.clone()
            } else {
                // No soul available at all — fall back to a minimal prompt.
                tracing::warn!(
                    session_id = %id,
                    agent_id = %agent_id,
                    "no soul file found — using fallback system prompt"
                );
                let skills_json = serde_json::to_value(&*llm_skills).unwrap_or_default();
                let skills_prompt = self_bridge.build_skills_prompt(&skills_json).unwrap_or_default();
                return super::self_bridge::build_system_prompt_fallback(
                    "You are a helpful assistant.",
                    &skills_prompt,
                    &tool_descriptors,
                );
            };
            let skills_json = serde_json::to_value(&*llm_skills).unwrap_or_default();
            let tools_json = serde_json::to_value(&tool_descriptors).unwrap_or_default();

            // Python-first: unified system_prompt.py
            let current_dir = std::env::current_dir().ok();
            let prompt_ctx = super::self_bridge::SystemPromptContext {
                claude_md_content: None,  // TODO: discover CLAUDE.md from cwd
                cwd: current_dir.as_ref().and_then(|p| p.to_str()),
                platform: "desktop",
                model: None,
                provider: None,
            };
            if let Some(prompt) = self_bridge.build_full_system_prompt(
                &soul_raw,
                &skills_json,
                &tools_json,
                None,  // memory is retrieved per-turn
                &prompt_ctx,
            ) {
                return prompt;
            }

            // Rust fallback when Python is unavailable
            let soul_prompt = self_bridge
                .build_soul_prompt(&soul_raw)
                .unwrap_or_else(|| soul_raw.clone());
            let skills_prompt = self_bridge
                .build_skills_prompt(&skills_json)
                .unwrap_or_default();
            super::self_bridge::build_system_prompt_fallback(
                &soul_prompt,
                &skills_prompt,
                &tool_descriptors,
            )
        })
    };

    // Build event payload with optional skill context.
    let mut payload = json!({
        "session_id": id,
        "agent_id": chat_agent_id,
        "text": effective_text,
        "sender": operator,
        "source": "tauri-desktop",
        "soul_system_prompt": combined_prompt,
    });
    if let Some(sc) = skill_context {
        payload["skill_context"] = sc;
    }

    // Do NOT transition the workflow here. The session workflow
    // (ACTIVE→PROCESSING on MESSAGE_RECEIVED) is owned centrally by the
    // `MessageReceivedHandler` in agent_runtime.rs, which fires when this
    // event is delivered from the bus. Transitioning here too causes a
    // deterministic InvalidStateTransition on every message: the first
    // transition (here) wins, then the handler's identical transition fails
    // because the session is already PROCESSING.

    // Publish the message event.
    let event = Event::new(
        "gateway:http",
        EventType::MessageReceived,
        payload,
    );
    let event_id = event.id.to_string();
    if let Err(error) = runtime.publish_event(event).await {
        runtime.audit().record(
            operator,
            "chat.send_message",
            format!("session:{id}"),
            "error",
            error.to_string(),
        );
        return ApiError::from(error).into_response();
    }

    // Touch session (update timestamp + version).
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let _ = runtime.workflow_engine().update_instance_data(&id, |data| {
        data["last_active_at"] = json!(now_ms);
        let v = data.get("version").and_then(|v| v.as_u64()).unwrap_or(0);
        data["version"] = json!(v + 1);
        let mc = data.get("message_count").and_then(|v| v.as_u64()).unwrap_or(0);
        data["message_count"] = json!(mc + 1);
    });

    // Persist to SQLite store so the session list in the frontend shows
    // the correct message count even while the session is still open.
    let store = runtime.agent_registry().get_session_store(chat_agent_id).await
        .or_else(|| runtime.find_session_store(&id));
    if let Some(store) = store
        && let Some(inst) = runtime.workflow_engine().get_instance(&id) {
            let session_type = inst.data.get("session_type")
                .and_then(|v| v.as_str()).unwrap_or("persistent");
            let created_at = inst.data.get("created_at")
                .and_then(|v| v.as_i64()).unwrap_or(0);
            let message_count = inst.data.get("message_count")
                .and_then(|v| v.as_i64()).unwrap_or(0);
            let _ = store.upsert(&session_store::SessionRecord {
                id: inst.id,
                agent_id: chat_agent_id.to_owned(),
                state: inst.current_state,
                message_count,
                created_at,
                last_active_at: now_ms as i64,
                session_type: session_type.to_owned(),
                reflected_at: None,
            title: None,
            });
        }

    runtime.audit().record(
        operator,
        "chat.send_message",
        format!("session:{id}"),
        "ok",
        format!("text_hash:{}", content_hash(&text)),
    );
    (StatusCode::OK, Json(json!({ "ok": true, "event_id": event_id }))).into_response()
}

#[derive(Debug, Clone, Deserialize)]
struct ChatSessionActionRequest {
    #[serde(default)]
    reason: Option<String>,
}

async fn chat_session_close(
    State(runtime): State<Arc<AgentRuntime>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(req): Json<ChatSessionActionRequest>,
) -> Response {
    let operator = operator_from_headers(&headers).unwrap_or(DEFAULT_OPERATOR);
    let event = Event::new(
        "chat:control",
        EventType::Custom("SESSION_CLOSE_CMD".to_owned()),
        json!({
            "session_id": id,
            "operator": operator,
            "reason": req.reason,
        }),
    );
    match runtime.workflow_engine().handle_event(&id, event).await {
        Ok(_result) => {
            runtime.audit().record(
                operator,
                "chat.session.close",
                format!("session:{id}"),
                "ok",
                "",
            );
            let _ = runtime.workflow_engine().update_instance_data(&id, |data| {
                let now_ms = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis();
                data["last_active_at"] = json!(now_ms);
                let v = data.get("version").and_then(|v| v.as_u64()).unwrap_or(0);
                data["version"] = json!(v + 1);
            });
            let _ = runtime.publish_event(Event::new(
                "session:control",
                EventType::Custom("session:closed".to_owned()),
                json!({
                    "session_id": id,
                    "operator": operator,
                }),
            )).await;

            // Persist session record to the SQLite store.
            if let Some(store) = runtime.find_session_store(&id)
                && let Some(inst) = runtime.workflow_engine().get_instance(&id) {
                    let session_type = inst.data.get("session_type")
                        .and_then(|v| v.as_str()).unwrap_or("persistent");
                    let created_at = inst.data.get("created_at")
                        .and_then(|v| v.as_i64()).unwrap_or(0);
                    let last_active_at = inst.data.get("last_active_at")
                        .and_then(|v| v.as_i64()).unwrap_or(0);
                    let message_count = inst.data.get("message_count")
                        .and_then(|v| v.as_i64()).unwrap_or(0);
                    let _ = store.upsert(&session_store::SessionRecord {
                        id: inst.id,
                        agent_id: String::new(),
                        state: inst.current_state,
                        message_count,
                        created_at,
                        last_active_at,
                        session_type: session_type.to_owned(),
                        reflected_at: None,
            title: None,
                    });
                }

            (StatusCode::OK, Json(json!({ "ok": true }))).into_response()
        }
        Err(error) => {
            runtime.audit().record(
                operator,
                "chat.session.close",
                format!("session:{id}"),
                "error",
                error.to_string(),
            );
            ApiError::from(error).into_response()
        }
    }
}

async fn chat_session_stop(
    State(runtime): State<Arc<AgentRuntime>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    let operator = operator_from_headers(&headers).unwrap_or(DEFAULT_OPERATOR);
    let event = Event::new(
        "chat:control",
        EventType::Custom("STOP_GENERATION".to_owned()),
        json!({
            "session_id": id,
            "operator": operator,
        }),
    );
    if let Err(error) = runtime.publish_event(event).await {
        return ApiError::from(error).into_response();
    }
    (StatusCode::OK, Json(json!({ "ok": true }))).into_response()
}

async fn chat_session_kill(
    State(runtime): State<Arc<AgentRuntime>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    let operator = operator_from_headers(&headers).unwrap_or(DEFAULT_OPERATOR);
    match runtime.kill_session(&id, operator).await {
        Ok(true) => (
            StatusCode::OK,
            Json(json!({ "ok": true, "status": "killed" })),
        )
            .into_response(),
        Ok(false) => (
            StatusCode::OK,
            Json(json!({ "ok": true, "status": "no_task" })),
        )
            .into_response(),
        Err(e) => ApiError::from(e).into_response(),
    }
}

async fn chat_session_retry(
    State(runtime): State<Arc<AgentRuntime>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    let operator = operator_from_headers(&headers).unwrap_or(DEFAULT_OPERATOR);
    let event = Event::new(
        "chat:control",
        EventType::Custom("RETRY_CMD".to_owned()),
        json!({
            "session_id": id,
            "operator": operator,
        }),
    );
    match runtime.workflow_engine().handle_event(&id, event).await {
        Ok(_result) => {
            runtime.audit().record(
                operator,
                "chat.session.retry",
                format!("session:{id}"),
                "ok",
                "",
            );
            let _ = runtime.workflow_engine().update_instance_data(&id, |data| {
                let now_ms = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis();
                data["last_active_at"] = json!(now_ms);
                let v = data.get("version").and_then(|v| v.as_u64()).unwrap_or(0);
                data["version"] = json!(v + 1);
            });
            (StatusCode::OK, Json(json!({ "ok": true }))).into_response()
        }
        Err(error) => {
            runtime.audit().record(
                operator,
                "chat.session.retry",
                format!("session:{id}"),
                "error",
                error.to_string(),
            );
            ApiError::from(error).into_response()
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
struct ChatEditRequest {
    message_event_id: String,
    new_text: String,
    #[serde(default)]
    expected_version: Option<u64>,
}

async fn chat_session_edit(
    State(runtime): State<Arc<AgentRuntime>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(req): Json<ChatEditRequest>,
) -> Response {
    let operator = operator_from_headers(&headers).unwrap_or(DEFAULT_OPERATOR);

    // Validate session.
    let instance = match runtime.workflow_engine().get_instance(&id) {
        Some(inst) => inst,
        None => {
            return ApiError::not_found(format!("session not found: {id}"),).into_response();
        }
    };
    let current_ver = instance
        .data
        .get("version")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    if let Some(expected) = req.expected_version
        && current_ver != expected {
            return ApiError::conflict(format!(
                        "version conflict: expected {}, got {}",
                        expected, current_ver
                    ),).into_response();
        }

    // Verify the message exists in event store.
    if runtime.event_store().get(&req.message_event_id).is_none() {
        return ApiError::not_found(format!("message not found: {}", req.message_event_id),).into_response();
    }

    let event = Event::new(
        "chat:control",
        EventType::Custom("MESSAGE_EDITED".to_owned()),
        json!({
            "session_id": id,
            "message_event_id": req.message_event_id,
            "new_text": req.new_text,
            "operator": operator,
        }),
    );
    if let Err(error) = runtime.publish_event(event).await {
        runtime.audit().record(
            operator,
            "chat.session.edit",
            format!("session:{id}"),
            "error",
            error.to_string(),
        );
        return ApiError::from(error).into_response();
    }
    let _ = runtime.workflow_engine().update_instance_data(&id, |data| {
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        data["last_active_at"] = json!(now_ms);
        let v = data.get("version").and_then(|v| v.as_u64()).unwrap_or(0);
        data["version"] = json!(v + 1);
    });
    runtime.audit().record(
        operator,
        "chat.session.edit",
        format!("session:{id}"),
        "ok",
        "",
    );
    (StatusCode::OK, Json(json!({ "ok": true }))).into_response()
}

// ── Skills reload ──────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
struct SkillsReloadResponse {
    ok: bool,
}

async fn skills_reload(
    State(runtime): State<Arc<AgentRuntime>>,
    headers: HeaderMap,
) -> Response {
    let operator = operator_from_headers(&headers).unwrap_or(DEFAULT_OPERATOR);
    match runtime.reload_skills_now() {
        Ok(_report) => {
            runtime.audit().record(
                operator,
                "skills.reload",
                "skills",
                "ok",
                "",
            );
            (StatusCode::OK, Json(SkillsReloadResponse { ok: true })).into_response()
        }
        Err(error) => {
            runtime.audit().record(
                operator,
                "skills.reload",
                "skills",
                "error",
                error.to_string(),
            );
            ApiError::from(error).into_response()
        }
    }
}

// ── Events recent ──────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize)]
struct EventsRecentQuery {
    limit: Option<usize>,
}

async fn events_recent(
    State(runtime): State<Arc<AgentRuntime>>,
    Query(query): Query<EventsRecentQuery>,
) -> Response {
    let limit = query.limit.unwrap_or(50).min(1000);
    let events = runtime.event_store().recent(limit);
    (StatusCode::OK, Json(json!({ "events": events }))).into_response()
}

// ── Soul system prompt ─────────────────────────────────────────────────

async fn get_system_prompt(State(runtime): State<Arc<AgentRuntime>>) -> Response {
    let Some(soul) = runtime.soul_runtime() else {
        return ApiError::not_found("no soul configured".to_owned(),).into_response();
    };
    let soul = soul.current_soul();
    let skills_json = serde_json::to_value(&*runtime.llm_skills()).unwrap_or_default();
    let tools_json = serde_json::json!([]);
    let prompt = runtime.self_bridge()
        .build_full_system_prompt(
            &soul.raw, &skills_json, &tools_json, None,
            &super::self_bridge::SystemPromptContext {
                claude_md_content: None,
                cwd: std::env::current_dir().ok().as_ref().and_then(|p| p.to_str()),
                platform: "desktop", model: None, provider: None,
            },
        )
        .unwrap_or_else(|| {
            let soul_prompt = runtime.self_bridge()
                .build_soul_prompt(&soul.raw)
                .unwrap_or_else(|| soul.raw.clone());
            let skills_prompt = runtime.self_bridge()
                .build_skills_prompt(&skills_json)
                .unwrap_or_default();
            super::self_bridge::build_system_prompt_fallback(&soul_prompt, &skills_prompt, &[])
        });
    (StatusCode::OK, Json(json!({ "system_prompt": prompt }))).into_response()
}

// ── Debug metrics ──────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
struct DebugMetricsResponse {
    queue_depth_high: usize,
    queue_depth_normal: usize,
    queue_depth_low: usize,
    retry_queue_depth: usize,
    throughput: u64,
    discarded_count: u64,
    duplicate_count: u64,
    subscription_count: usize,
    backpressure_level: String,
    dlq_depth: usize,
    inflight_pipelines: usize,
    inflight_skills: usize,
    plugin_health: Vec<PluginHealthItem>,
}

#[derive(Debug, Clone, Serialize)]
struct PluginHealthItem {
    name: String,
    status: String,
}

async fn debug_metrics(State(runtime): State<Arc<AgentRuntime>>) -> Response {
    let bus = runtime.bus_metrics();
    let dlq_depth = runtime.dlq().depth();
    let loader = runtime.plugin_loader().await;
    let plugin_health: Vec<PluginHealthItem> = loader
        .loaded_plugins()
        .into_iter()
        .map(|name| {
            let status = match loader.state_of(&name) {
                Some(s) => format!("{s:?}"),
                None => "unknown".to_owned(),
            };
            PluginHealthItem { name, status }
        })
        .collect();

    Json(DebugMetricsResponse {
        queue_depth_high: bus.queue_depth.high,
        queue_depth_normal: bus.queue_depth.normal,
        queue_depth_low: bus.queue_depth.low,
        retry_queue_depth: bus.retry_queue_depth,
        throughput: bus.throughput,
        discarded_count: bus.discarded_count,
        duplicate_count: bus.duplicate_count,
        subscription_count: bus.subscription_count,
        backpressure_level: format!("{:?}", bus.backpressure_level),
        dlq_depth,
        inflight_pipelines: runtime.inflight_pipelines(),
        inflight_skills: runtime.inflight_skills(),
        plugin_health,
    })
    .into_response()
}

fn parse_bearer(headers: &HeaderMap) -> Option<String> {
    let value = headers.get(axum::http::header::AUTHORIZATION)?.to_str().ok()?;
    let value = value.trim();
    let token = value.strip_prefix("Bearer ")?;
    Some(token.to_owned())
}

fn operator_from_headers(headers: &HeaderMap) -> Option<&str> {
    headers
        .get("x-aman-operator")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn sanitize_skill_file_name(name: &str) -> String {
    name.chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect()
}

fn require_confirmation(headers: &HeaderMap) -> bool {
    headers
        .get("x-aman-confirm")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.trim().eq_ignore_ascii_case("yes"))
}

/// Guard: if the `x-aman-confirm: yes` header is absent, record an audit
/// entry and return `Some(response)`.  Handlers use `if let Some(r) = ... { return r; }`
/// to short-circuit.
fn guard_confirmation(
    audit: &AuditLogger,
    operator: &str,
    action: &str,
    resource: &str,
    headers: &HeaderMap,
) -> Option<Response> {
    if !require_confirmation(headers) {
        audit.record(operator, action, resource, "confirm_required", "");
        return Some(ApiError::conflict("confirmation required").into_response());
    }
    None
}

/// Wrap a `Result<(), Error>` with audit logging.
///
/// On success, records an `"ok"` outcome and returns `StatusCode::OK`.
/// On error, records an `"error"` outcome with the error string and converts
/// the error to the standard `ApiError` JSON response.
fn with_audit(
    audit: &AuditLogger,
    operator: &str,
    action: &str,
    resource: &str,
    result: Result<(), Error>,
) -> Response {
    match result {
        Ok(()) => {
            audit.record(operator, action, resource, "ok", "");
            StatusCode::OK.into_response()
        }
        Err(error) => {
            let msg = error.to_string();
            audit.record(operator, action, resource, "error", &msg);
            ApiError::from(error).into_response()
        }
    }
}

/// Unified API error response.
///
/// All HTTP handlers in this file produce errors in this shape so
/// clients see a consistent `{"error": "<message>"}` body with an
/// appropriate status code. The previous code mixed three styles
/// — `error_response()` returning `ErrorBody { message }`, inline
/// `(StatusCode, Json(ErrorBody{...}))` tuples, and bare
/// `(StatusCode, Json(json!({"error":...})))` — and used two
/// different field names (`message` vs `error`) for the body.
///
/// `IntoResponse` makes `ApiError` usable directly as a handler
/// return type: `Result<Json<T>, ApiError>` is the axum-idiomatic
/// shape and the `?` operator flows through unchanged.
#[derive(Debug)]
pub enum ApiError {
    BadRequest(String),
    NotFound(String),
    Conflict(String),
    Forbidden(String),
    Unprocessable(String),
    Internal(String),
}

impl ApiError {
    pub fn bad_request(msg: impl Into<String>) -> Self {
        Self::BadRequest(msg.into())
    }
    pub fn not_found(msg: impl Into<String>) -> Self {
        Self::NotFound(msg.into())
    }
    pub fn conflict(msg: impl Into<String>) -> Self {
        Self::Conflict(msg.into())
    }
    pub fn forbidden(msg: impl Into<String>) -> Self {
        Self::Forbidden(msg.into())
    }
    pub fn unprocessable(msg: impl Into<String>) -> Self {
        Self::Unprocessable(msg.into())
    }
    pub fn internal(msg: impl Into<String>) -> Self {
        Self::Internal(msg.into())
    }
}

impl From<kernel::Error> for ApiError {
    fn from(error: kernel::Error) -> Self {
        match error {
            Error::NotFound { name } => Self::not_found(format!("{name} not found")),
            Error::AlreadyExists { .. } | Error::InvalidStateTransition { .. } => {
                Self::conflict(error.to_string())
            }
            Error::PermissionDenied { .. } => Self::forbidden(error.to_string()),
            Error::ConfigInvalid { .. } => Self::bad_request(error.to_string()),
            Error::Unrecoverable { .. } => Self::unprocessable(error.to_string()),
            _ => Self::internal(error.to_string()),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, message) = match self {
            Self::BadRequest(m) => (StatusCode::BAD_REQUEST, m),
            Self::NotFound(m) => (StatusCode::NOT_FOUND, m),
            Self::Conflict(m) => (StatusCode::CONFLICT, m),
            Self::Forbidden(m) => (StatusCode::FORBIDDEN, m),
            Self::Unprocessable(m) => (StatusCode::UNPROCESSABLE_ENTITY, m),
            Self::Internal(m) => (StatusCode::INTERNAL_SERVER_ERROR, m),
        };
        (status, Json(json!({"error": message}))).into_response()
    }
}

#[derive(Debug, Clone, Serialize)]
struct OkResponse {
    ok: bool,
}

#[derive(Debug, Clone, Deserialize)]
struct ToolAuthRespondBody {
    auth_id: String,
    approved: bool,
}

#[derive(Debug, Clone, Deserialize)]
struct PluginAuthRespondBody {
    plugin_name: String,
    approved: bool,
}

#[derive(Debug, Clone, Serialize)]
struct InstallPluginResponse {
    plugin_name: String,
    version: String,
    install_dir: String,
}

// ── Agent management ──────────────────────────────────────────────────

async fn agent_list(State(runtime): State<Arc<AgentRuntime>>) -> Response {
    Json(runtime.agent_registry().list().await).into_response()
}

async fn agent_get(
    State(runtime): State<Arc<AgentRuntime>>,
    Path(agent_id): Path<String>,
) -> Response {
    match runtime.agent_registry().get(&agent_id).await {
        Some(instance) => Json(instance).into_response(),
        None => ApiError::not_found(format!("agent not found: {agent_id}"),).into_response(),
    }
}

#[derive(Debug, Deserialize)]
struct AgentSetStatusBody {
    status: AgentStatus,
}

async fn agent_set_status(
    State(runtime): State<Arc<AgentRuntime>>,
    Path(agent_id): Path<String>,
    Json(body): Json<AgentSetStatusBody>,
) -> Response {
    match runtime.agent_registry().set_status(&agent_id, body.status).await {
        Ok(()) => StatusCode::OK.into_response(),
        Err(e) => ApiError::bad_request(e.to_string()).into_response(),
    }
}

async fn agent_reload(
    State(runtime): State<Arc<AgentRuntime>>,
    Path(agent_id): Path<String>,
) -> Response {
    let config = match config::AmanConfig::from_default_path() {
        Ok(c) => c,
        Err(e) => {
            return ApiError::internal(format!("failed to read config: {e}"),).into_response();
        }
    };
    match runtime
        .agent_registry()
        .reload_agent(&config, &agent_id, runtime.agenverse().era_arc())
        .await
    {
        Ok(()) => Json(json!({ "ok": true, "agent_id": agent_id })).into_response(),
        Err(e) => ApiError::bad_request(e.to_string()).into_response(),
    }
}

// ── Idle system start/stop (UI focus-driven) ──────────────────────────

/// Start the idle system for a single agent.
///
/// Sets AgentSystemState → Idle and starts the idle loop.
/// Returns error if agent is Busy (has active session).
async fn agent_idle_start(
    State(runtime): State<Arc<AgentRuntime>>,
    Path(agent_id): Path<String>,
) -> Response {
    // 门控：只有非 Busy 状态才允许启动 idle。
    if let Some(instance) = runtime.agent_registry().get(&agent_id).await
        && instance.status == AgentStatus::Busy
    {
        return ApiError::bad_request(format!(
            "agent {agent_id} is busy (has active session)"
        ))
        .into_response();
    }

    // 设置状态为 Idle。
    runtime
        .agent_registry()
        .set_system_state(&agent_id, AgentSystemState::Idle)
        .await;

    // 启动 idle loop。
    if let Some(manager) = runtime.agent_registry().get_idle_manager(&agent_id).await {
        manager.start().await;
        tracing::info!(agent = %agent_id, "idle system started via UI");
    }

    Json(json!({ "ok": true, "agent_id": agent_id, "state": "idle" })).into_response()
}

/// Stop the idle system for a single agent.
///
/// Stops the idle loop and sets AgentSystemState → Ready.
async fn agent_idle_stop(
    State(runtime): State<Arc<AgentRuntime>>,
    Path(agent_id): Path<String>,
) -> Response {
    if let Some(manager) = runtime.agent_registry().get_idle_manager(&agent_id).await {
        manager.stop().await;
        tracing::info!(agent = %agent_id, "idle system stopped via UI");
    }

    // 停止后设为 Ready。
    runtime
        .agent_registry()
        .set_system_state(&agent_id, AgentSystemState::Ready)
        .await;

    Json(json!({ "ok": true, "agent_id": agent_id, "state": "ready" })).into_response()
}

/// Start idle system for all agents (used when main window loses focus).
///
/// Only starts agents that are not Busy and not already in Idle state.
async fn agents_idle_start_all(State(runtime): State<Arc<AgentRuntime>>) -> Response {
    let instances = runtime.agent_registry().list().await;
    let mut started = 0;
    for instance in instances {
        let agent_id = instance.descriptor.agent_id;
        if instance.status == AgentStatus::Busy {
            continue;
        }
        if instance.system_state == AgentSystemState::Idle {
            continue;
        }
        runtime
            .agent_registry()
            .set_system_state(&agent_id, AgentSystemState::Idle)
            .await;
        if let Some(manager) = runtime.agent_registry().get_idle_manager(&agent_id).await {
            manager.start().await;
            started += 1;
        }
    }
    tracing::info!(started, "idle systems started for all agents via UI");
    Json(json!({ "ok": true, "started": started })).into_response()
}

// ── Idle-run availability endpoint ───────────────────────────────────────────

#[derive(Serialize)]
struct AgentAvailability {
    work: bool,
    study: bool,
    fun: bool,
    prize: bool,
}

/// Return per-agent work/study/fun button availability.
///
/// One-step check per tag: does a skill with both the `idle_run` tag and the
/// requested tag exist? (global, all tags)
///
/// Work is intentionally NOT gated on a non-empty WorkSystem queue. The
/// kanban-worker / startup-worker skills are discovery skills — they query the
/// kanban board themselves to find assigned items (and correctly report idle
/// if nothing is assigned). Gating the button on queued items would create a
/// chicken-and-egg deadlock: you couldn't click Work to discover work, because
/// the button would stay disabled until work was already queued. Skill
/// existence is the only prerequisite, identical to study/fun/prize.
async fn agents_idle_availability(
    State(runtime): State<Arc<AgentRuntime>>,
) -> Response {
    // -- Step 1 (global): check for skills with idle_run + each requested tag --

    let idle_run_skills: Vec<_> = runtime
        .skill_search()
        .search_by_tag("idle_run");

    let has_work_skills = idle_run_skills.iter().any(|s| s.tags.iter().any(|t| t == "work"));
    let has_study_skills = idle_run_skills.iter().any(|s| s.tags.iter().any(|t| t == "study"));
    let has_fun_skills = idle_run_skills.iter().any(|s| s.tags.iter().any(|t| t == "fun"));
    let has_prize_skills = idle_run_skills.iter().any(|s| s.tags.iter().any(|t| t == "prize"));

    // All four tags: enabled iff the matching idle_run skill exists. The skill
    // itself handles empty-queue / not-assigned cases.

    let agents = runtime.agent_registry().list().await;

    let mut availabilities = BTreeMap::new();

    for agent in &agents {
        let agent_id = &agent.descriptor.agent_id;

        let work = has_work_skills;
        let study = has_study_skills;
        let fun = has_fun_skills;
        let prize = has_prize_skills;

        availabilities.insert(
            agent_id.clone(),
            AgentAvailability { work, study, fun, prize },
        );
    }

    Json(json!({"agents": availabilities})).into_response()
}

// ── Idle-run endpoint ───────────────────────────────────────────────────────

async fn idle_run(
    State(runtime): State<Arc<AgentRuntime>>,
    Json(payload): Json<Value>,
) -> Response {
    let tag = payload
        .get("tag")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_lowercase();

    if tag.is_empty() {
        return ApiError::bad_request("missing 'tag' field").into_response();
    }

    // Resolve agent
    let agent_id = match payload
        .get("agent_key")
        .and_then(|v| v.as_str())
        .map(String::from)
        .or_else(|| {
            config::AmanConfig::from_default_path()
                .ok()
                .and_then(|c| c.agents.into_keys().next())
        }) {
        Some(k) => k,
        None => {
            return ApiError::bad_request("no agent configured — specify agent_key or add an agent to config.yaml").into_response();
        }
    };

    // Find skills with both idle_run tag and the requested tag
    let candidates: Vec<_> = runtime
        .skill_search()
        .search_by_tag("idle_run")
        .into_iter()
        .filter(|s| s.tags.contains(&tag))
        .collect();

    if candidates.is_empty() {
        return ApiError::not_found("执行失败，还没有实装有关的技能").into_response();
    }

    // Pick a random skill (scope RNG so it's dropped before any await)
    let (skill_name, prompt_idx) = {
        let mut rng = rand::thread_rng();
        let idx = rng.gen_range(0..candidates.len());
        let name = candidates[idx].name.clone();
        let pidx = rng.gen_range(0..100usize);
        (name, pidx)
    };

    let Some(skill) = runtime.skills().get(&skill_name) else {
        return ApiError::not_found("执行失败，还没有实装有关的技能").into_response();
    };

    // Pick an idle_prompt (no RNG needed — use the pre-rolled index)
    let idle_prompt = runtime
        .skills()
        .idle_prompts(&skill_name)
        .map(|prompts| {
            let i = prompt_idx % prompts.len();
            prompts[i].replace("{agent_id}", &agent_id)
        });

    let text = match idle_prompt {
        Some(prompt) => {
            let body = runtime.skills().skill_body(&skill_name);
            match body {
                Some(b) => format!(
                    "[IDLE ACTION] {prompt}\n\n\
                     --- SKILL METHODOLOGY ---\n\
                     {b}\n\
                     --- END SKILL ---\n\n\
                     Execute the action above using the skill's methodology. \
                     Do not skip or abbreviate any prescribed stage."
                ),
                None => format!(
                    "[IDLE ACTION] {prompt}\n\n\
                     Execute the action above using your available tools and \
                     knowledge. Be thorough and complete the task."
                ),
            }
        }
        None => {
            format!(
                "[IDLE ACTION] Execute the skill \"{skill_name}\": {}.\n\
                 Use your available tools and follow your standard methodology.",
                skill.description()
            )
        }
    };

    // Determine session mode: background vs foreground, work item vs regular
    let background = payload
        .get("background")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let project_key = payload
        .get("project_key")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty());
    let work_id = payload
        .get("work_id")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty());

    let session_type = if background { "background" } else { "persistent" };

    // Create or resume session
    let session_id: String = if let (Some(pk), Some(wid)) = (project_key, work_id) {
        // Work item session: deterministic ID so the agent can resume
        // ("断点续传") across multiple idle-run invocations.
        let sid = super::session::work_session::work_session_id(&agent_id, pk, wid);
        if let Err(e) = runtime
            .session_manager()
            .ensure_session(&sid, &agent_id, session_type)
            .await
        {
            return ApiError::internal(format!("failed to ensure work session: {e}")).into_response();
        }

        // Resume: load previous history, apply compression if needed
        if let Some(store) = runtime.session_store_for_agent(&agent_id) {
            let _ = super::session::work_session::resume_work_session(
                &runtime.agent_harness(),
                &store,
                &sid,
                0, // max_history_tokens: 0 = no compression, just restore
            )
            .await;
        }

        sid
    } else {
        // Regular idle run (background or foreground): create a new session
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        let data = json!({
            "session_type": session_type,
            "version": 0,
            "created_at": now_ms,
            "last_active_at": now_ms,
        });

        let instance = match runtime
            .workflow_engine()
            .create_instance("message-session", data)
        {
            Ok(i) => i,
            Err(e) => {
                return ApiError::internal(format!("failed to create session: {e}")).into_response();
            }
        };
        let sid = instance.id.clone();

        // Persist session
        if let Some(store) = runtime.session_store_for_agent(&agent_id) {
            let _ = store.upsert(&session_store::SessionRecord {
                id: sid.clone(),
                agent_id: agent_id.clone(),
                state: instance.current_state.clone(),
                message_count: 0,
                created_at: now_ms as i64,
                last_active_at: now_ms as i64,
                session_type: session_type.to_owned(),
                reflected_at: None,
                title: None,
            });
        }

        sid
    };

    // Set system state so the UI reflects what the agent is doing.
    // Mirror the tag→state mapping from idle::manager::dispatch_one.
    let ss = match tag.as_str() {
        "work" => AgentSystemState::Working,
        "study" => AgentSystemState::Studying,
        "prize" => AgentSystemState::Prize,
        "internet" | "entertainment" | "fun" => AgentSystemState::DailyLife,
        _ => AgentSystemState::Waiting,
    };
    runtime.agent_registry().set_system_state(&agent_id, ss).await;

    // Publish MessageReceived event so the agent harness picks it up
    let event = Event::new(
        "idle.manual",
        EventType::MessageReceived,
        json!({
            "session_id": session_id,
            "agent_id": agent_id,
            "text": text,
            "skill_name": skill_name,
            "tag": tag,
            "session_type": session_type,
            "background": background,
        }),
    );
    if let Err(e) = runtime.publish_event(event).await {
        return ApiError::internal(format!("failed to publish event: {e}")).into_response();
    }

    (
        StatusCode::OK,
        Json(json!({
            "session_id": session_id,
            "skill_name": skill_name,
            "tag": tag,
            "background": background,
        })),
    )
        .into_response()
}

// ── MCP handlers ───────────────────────────────────────────────────

async fn mcp_list_servers(
    State(runtime): State<Arc<AgentRuntime>>,
    Path(agent_key): Path<String>,
) -> Result<Json<Vec<mcp_client::McpServerStatus>>, ApiError> {
    let manager = runtime
        .agent_registry()
        .get_mcp_manager(&agent_key)
        .await
        .ok_or_else(|| {
            ApiError::not_found(format!("MCP manager not found for agent '{agent_key}'"))
        })?;

    let statuses = manager.list_servers().await;
    Ok(Json(statuses))
}

async fn mcp_connect_server(
    State(runtime): State<Arc<AgentRuntime>>,
    Path((agent_key, name)): Path<(String, String)>,
) -> Result<Json<Value>, ApiError> {
    let manager = runtime
        .agent_registry()
        .get_mcp_manager(&agent_key)
        .await
        .ok_or_else(|| {
            ApiError::not_found(format!("MCP manager not found for agent '{agent_key}'"))
        })?;

    let merged = mcp_client::McpClientManager::load_merged_config(&agent_key)
        .map_err(ApiError::internal)?;

    let config = merged.iter().find(|c| c.name == name).cloned().ok_or_else(|| {
        ApiError::not_found(format!("MCP server '{name}' not found in config"))
    })?;

    manager
        .connect(&config)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;

    Ok(Json(json!({"ok": true, "server": name, "agent": agent_key})))
}

async fn mcp_disconnect_server(
    State(runtime): State<Arc<AgentRuntime>>,
    Path((agent_key, name)): Path<(String, String)>,
) -> Result<Json<Value>, ApiError> {
    let manager = runtime
        .agent_registry()
        .get_mcp_manager(&agent_key)
        .await
        .ok_or_else(|| {
            ApiError::not_found(format!("MCP manager not found for agent '{agent_key}'"))
        })?;

    manager
        .disconnect(&name)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;

    Ok(Json(json!({"ok": true, "server": name, "agent": agent_key})))
}

async fn mcp_reload(
    State(runtime): State<Arc<AgentRuntime>>,
    Path(agent_key): Path<String>,
) -> Result<Json<Value>, ApiError> {
    runtime
        .agent_registry()
        .reload_mcp_for_agent(&agent_key)
        .await
        .map_err(ApiError::internal)?;

    Ok(Json(json!({"ok": true, "agent": agent_key})))
}

#[cfg(test)]
mod tests {
    use super::{guard_confirmation, operator_from_headers, parse_bearer, require_confirmation, with_audit};
    use super::AuditLogger;
    use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode};
    use kernel::Error;

    fn header_map(name: &str, value: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(
            name.parse::<HeaderName>().unwrap(),
            HeaderValue::from_str(value).unwrap(),
        );
        headers
    }

    #[test]
    fn parse_bearer_extracts_token() {
        let headers = header_map("authorization", "Bearer secret-token");
        assert_eq!(parse_bearer(&headers), Some("secret-token".to_owned()));
    }

    #[test]
    fn parse_bearer_rejects_missing_header() {
        let headers = HeaderMap::new();
        assert_eq!(parse_bearer(&headers), None);
    }

    #[test]
    fn parse_bearer_rejects_malformed_value() {
        let headers = header_map("authorization", "Basic secret-token");
        assert_eq!(parse_bearer(&headers), None);
    }

    #[test]
    fn operator_from_headers_reads_and_trims_custom_header() {
        let headers = header_map("x-aman-operator", "  alice  ");
        assert_eq!(operator_from_headers(&headers), Some("alice"));
    }

    #[test]
    fn operator_from_headers_returns_none_for_empty_header() {
        let headers = header_map("x-aman-operator", "   ");
        assert_eq!(operator_from_headers(&headers), None);
    }

    #[test]
    fn operator_from_headers_returns_none_when_missing() {
        let headers = HeaderMap::new();
        assert_eq!(operator_from_headers(&headers), None);
    }

    #[test]
    fn require_confirmation_matches_yes() {
        assert!(require_confirmation(&header_map("x-aman-confirm", "yes")));
        assert!(require_confirmation(&header_map("x-aman-confirm", "YES")));
    }

    #[test]
    fn require_confirmation_rejects_other_values() {
        assert!(!require_confirmation(&header_map("x-aman-confirm", "no")));
        assert!(!require_confirmation(&header_map("x-aman-confirm", "true")));
        assert!(!require_confirmation(&HeaderMap::new()));
    }

    #[test]
    fn guard_confirmation_short_circuits_when_header_absent() {
        let audit = AuditLogger::new(10);
        let response = guard_confirmation(
            &audit,
            "api",
            "test.action",
            "test.resource",
            &HeaderMap::new(),
        )
        .expect("guard should reject");

        assert_eq!(response.status(), StatusCode::CONFLICT);
        let records = audit.list(Some("test.action"), None, None, None, 0, 10);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].outcome, "confirm_required");
    }

    #[test]
    fn guard_confirmation_passes_when_header_present() {
        let audit = AuditLogger::new(10);
        assert!(guard_confirmation(
            &audit,
            "api",
            "test.action",
            "test.resource",
            &header_map("x-aman-confirm", "yes"),
        )
        .is_none());
        assert!(audit.list(Some("test.action"), None, None, None, 0, 10).is_empty());
    }

    #[test]
    fn with_audit_records_ok_and_returns_200() {
        let audit = AuditLogger::new(10);
        let response = with_audit(
            &audit,
            "api",
            "test.action",
            "test.resource",
            Ok(()),
        );

        assert_eq!(response.status(), StatusCode::OK);
        let records = audit.list(Some("test.action"), None, None, None, 0, 10);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].outcome, "ok");
    }

    #[test]
    fn with_audit_records_error_and_maps_status_code() {
        let audit = AuditLogger::new(10);
        let response = with_audit(
            &audit,
            "api",
            "test.action",
            "test.resource",
            Err(Error::NotFound {
                name: "missing".to_owned(),
            }),
        );

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        let records = audit.list(Some("test.action"), None, None, None, 0, 10);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].outcome, "error");
        assert!(records[0].detail.contains("missing"));
    }
}
