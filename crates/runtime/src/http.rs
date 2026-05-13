use crate::agent_runtime::AgentRuntime;
use axum::extract::{Multipart, Path, State};
use tracing::instrument;
use axum::extract::Query;
use axum::http::{HeaderMap, StatusCode};
use axum::middleware;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use kernel::event::{Event, EventType};
use kernel::Error;
use persistence::{DeadLetterEntry, DeadLetterQueue, DlqFilter};
use plugin::PluginManifest;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use uuid::Uuid;

#[derive(Debug, Clone)]
pub struct HttpServerConfig {
    pub bind: SocketAddr,
}

pub struct HttpServerHandle {
    addr: SocketAddr,
    shutdown_tx: Option<oneshot::Sender<()>>,
}

impl HttpServerHandle {
    #[must_use]
    pub fn local_addr(&self) -> SocketAddr {
        self.addr
    }

    pub fn shutdown(mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
    }
}

pub async fn serve(runtime: Arc<AgentRuntime>, config: HttpServerConfig) -> kernel::AmanResult<HttpServerHandle> {
    let router = build_router(runtime);
    let listener = TcpListener::bind(config.bind).await?;
    let addr = listener.local_addr()?;
    let (tx, rx) = oneshot::channel::<()>();
    let server = axum::serve(listener, router).with_graceful_shutdown(async move {
        let _ = rx.await;
    });
    tokio::spawn(async move {
        let _ = server.await;
    });
    Ok(HttpServerHandle {
        addr,
        shutdown_tx: Some(tx),
    })
}

fn build_router(runtime: Arc<AgentRuntime>) -> Router {
    let control = Router::new()
        .route("/agent/start", post(agent_start))
        .route("/agent/shutdown", post(agent_shutdown))
        .route("/event-source/{id}/pause", post(source_pause))
        .route("/event-source/{id}/resume", post(source_resume))
        .route("/event-source/{id}/config", put(source_config))
        .route("/source/{id}/pause", post(source_pause))
        .route("/source/{id}/resume", post(source_resume))
        .route("/source/{id}/config", put(source_config))
        .route("/skills", get(skill_list))
        .route("/skills/search", get(skill_search))
        .route("/skill/{name}", get(skill_info))
        .route("/skill/{name}/enable", post(skill_enable))
        .route("/skill/{name}/disable", post(skill_disable))
        .route("/skill/{name}/versions", get(skill_versions))
        .route("/skill/{name}/rollback", post(skill_rollback))
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
        .route("/events/dump/{id}", get(event_dump))
        .route("/events/trace/{trace_id}", get(event_trace))
        .route("/dlq", get(dlq_list))
        .route("/dlq/{id}/retry", post(dlq_retry))
        .route("/dlq/{id}/discard", post(dlq_discard))
        .route("/config/set", post(config_set))
        .route("/audit-log", get(audit_log))
        .route_layer(middleware::from_fn_with_state(
            runtime.clone(),
            require_api_token,
        ));

    Router::new()
        .route("/health/live", get(health_live))
        .route("/health/ready", get(health_ready))
        .route("/health", get(health_ready))
        .route("/metrics", get(metrics))
        .merge(control)
        .with_state(runtime)
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

#[instrument(skip(runtime))]
async fn agent_start(State(runtime): State<Arc<AgentRuntime>>) -> Response {
    match runtime.start().await {
        Ok(()) => {
            runtime
                .audit()
                .record("api", "agent.start", "agent", "ok", "");
            StatusCode::OK.into_response()
        }
        Err(error) => {
            runtime
                .audit()
                .record("api", "agent.start", "agent", "error", error.to_string());
            error_response(error)
        }
    }
}

#[instrument(skip(runtime, headers))]
async fn agent_shutdown(State(runtime): State<Arc<AgentRuntime>>, headers: HeaderMap) -> Response {
    let operator = operator_from_headers(&headers).unwrap_or("api");
    if !require_confirmation(&headers) {
        runtime
            .audit()
            .record(operator, "agent.shutdown", "agent", "confirm_required", "");
        return (
            StatusCode::CONFLICT,
            Json(ErrorBody {
                message: "confirmation required".to_owned(),
            }),
        )
            .into_response();
    }
    match runtime.shutdown().await {
        Ok(()) => {
            runtime
                .audit()
                .record(operator, "agent.shutdown", "agent", "ok", "");
            StatusCode::OK.into_response()
        }
        Err(error) => {
            runtime
                .audit()
                .record(operator, "agent.shutdown", "agent", "error", error.to_string());
            error_response(error)
        }
    }
}

async fn source_pause(
    State(runtime): State<Arc<AgentRuntime>>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Response {
    let operator = operator_from_headers(&headers).unwrap_or("api");
    match runtime.sources().pause(&id).await {
        Ok(()) => {
            runtime
                .audit()
                .record(operator, "source.pause", format!("source:{id}"), "ok", "");
            StatusCode::OK.into_response()
        }
        Err(error) => {
            runtime.audit().record(
                operator,
                "source.pause",
                format!("source:{id}"),
                "error",
                error.to_string(),
            );
            error_response(error)
        }
    }
}

async fn source_resume(
    State(runtime): State<Arc<AgentRuntime>>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Response {
    let operator = operator_from_headers(&headers).unwrap_or("api");
    match runtime.sources().resume(&id).await {
        Ok(()) => {
            runtime
                .audit()
                .record(operator, "source.resume", format!("source:{id}"), "ok", "");
            StatusCode::OK.into_response()
        }
        Err(error) => {
            runtime.audit().record(
                operator,
                "source.resume",
                format!("source:{id}"),
                "error",
                error.to_string(),
            );
            error_response(error)
        }
    }
}

async fn source_config(
    State(runtime): State<Arc<AgentRuntime>>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(payload): Json<Value>,
) -> Response {
    let operator = operator_from_headers(&headers).unwrap_or("api");
    match runtime.sources().reconfigure(&id, payload).await {
        Ok(()) => {
            runtime
                .audit()
                .record(operator, "source.config", format!("source:{id}"), "ok", "");
            StatusCode::OK.into_response()
        }
        Err(error) => {
            runtime.audit().record(
                operator,
                "source.config",
                format!("source:{id}"),
                "error",
                error.to_string(),
            );
            error_response(error)
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
        return (
            StatusCode::NOT_FOUND,
            Json(ErrorBody {
                message: format!("skill not found: {name}"),
            }),
        )
            .into_response();
    };
    Json(item).into_response()
}

async fn skill_enable(
    State(runtime): State<Arc<AgentRuntime>>,
    Path(name): Path<String>,
    headers: HeaderMap,
) -> Response {
    let operator = operator_from_headers(&headers).unwrap_or("api");
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
            error_response(error)
        }
    }
}

async fn skill_disable(
    State(runtime): State<Arc<AgentRuntime>>,
    Path(name): Path<String>,
    headers: HeaderMap,
) -> Response {
    let operator = operator_from_headers(&headers).unwrap_or("api");
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
            error_response(error)
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
        Err(error) => return error_response(error),
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
    let operator = operator_from_headers(&headers).unwrap_or("api");
    if !require_confirmation(&headers) {
        runtime.audit().record(
            operator,
            "skill.rollback",
            format!("skill:{name}"),
            "confirm_required",
            "",
        );
        return (
            StatusCode::CONFLICT,
            Json(ErrorBody {
                message: "confirmation required".to_owned(),
            }),
        )
            .into_response();
    }

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
        return error_response(error);
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
        return (
            StatusCode::NOT_FOUND,
            Json(ErrorBody {
                message: format!("workflow not found: {name}"),
            }),
        )
            .into_response();
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
    let operator = operator_from_headers(&headers).unwrap_or("api");
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
            error_response(error)
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
        return (
            StatusCode::NOT_FOUND,
            Json(ErrorBody {
                message: format!("workflow instance not found: {id}"),
            }),
        )
            .into_response();
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
    let operator = operator_from_headers(&headers).unwrap_or("api");
    if !require_confirmation(&headers) {
        runtime.audit().record(
            operator,
            "workflow.retry",
            format!("workflow-instance:{id}"),
            "confirm_required",
            "",
        );
        return (
            StatusCode::CONFLICT,
            Json(ErrorBody {
                message: "confirmation required".to_owned(),
            }),
        )
            .into_response();
    }
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
            error_response(error)
        }
    }
}

async fn workflow_cancel(
    State(runtime): State<Arc<AgentRuntime>>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Response {
    let operator = operator_from_headers(&headers).unwrap_or("api");
    if !require_confirmation(&headers) {
        runtime.audit().record(
            operator,
            "workflow.cancel",
            format!("workflow-instance:{id}"),
            "confirm_required",
            "",
        );
        return (
            StatusCode::CONFLICT,
            Json(ErrorBody {
                message: "confirmation required".to_owned(),
            }),
        )
            .into_response();
    }
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
            error_response(error)
        }
    }
}

async fn plugin_enable(
    State(runtime): State<Arc<AgentRuntime>>,
    Path(name): Path<String>,
    headers: HeaderMap,
) -> Response {
    let operator = operator_from_headers(&headers).unwrap_or("api");
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
            error_response(error)
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
    let operator = operator_from_headers(&headers).unwrap_or("api");
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
                return error_response(error);
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
    let operator = operator_from_headers(&headers).unwrap_or("api");
    if !require_confirmation(&headers) {
        runtime.audit().record(
            operator,
            "plugin.disable",
            format!("plugin:{name}"),
            "confirm_required",
            "",
        );
        return (
            StatusCode::CONFLICT,
            Json(ErrorBody {
                message: "confirmation required".to_owned(),
            }),
        )
            .into_response();
    }
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
            error_response(error)
        }
    }
}

async fn plugin_uninstall(
    State(runtime): State<Arc<AgentRuntime>>,
    Path(name): Path<String>,
    headers: HeaderMap,
) -> Response {
    let operator = operator_from_headers(&headers).unwrap_or("api");
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
            error_response(error)
        }
    }
}

async fn plugin_install(
    State(runtime): State<Arc<AgentRuntime>>,
    headers: HeaderMap,
    mut multipart: Multipart,
) -> Response {
    let operator = operator_from_headers(&headers).unwrap_or("api");
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
                            return (
                                StatusCode::BAD_REQUEST,
                                Json(ErrorBody {
                                    message: format!("failed to read multipart field: {error}"),
                                }),
                            )
                                .into_response();
                        }
                    }
                }
            }
            Ok(None) => break,
            Err(error) => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(ErrorBody {
                        message: format!("invalid multipart payload: {error}"),
                    }),
                )
                    .into_response();
            }
        }
    }

    let Some(archive_bytes) = archive_bytes else {
        return (
            StatusCode::BAD_REQUEST,
            Json(ErrorBody {
                message: "multipart must contain `plugin` file field".to_owned(),
            }),
        )
            .into_response();
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
            error_response(error)
        }
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorBody {
                message: format!("install task join error: {error}"),
            }),
        )
            .into_response(),
    }
}

#[derive(Debug, Deserialize)]
struct CronAddRequest {
    id: String,
    expression: String,
}

async fn cron_add(
    State(runtime): State<Arc<AgentRuntime>>,
    headers: HeaderMap,
    Json(req): Json<CronAddRequest>,
) -> Response {
    let caller = operator_from_headers(&headers).unwrap_or("api");
    match runtime.add_cron_job(req.id, req.expression, caller).await {
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
            error_response(error)
        }
    }
}

async fn cron_update(
    State(runtime): State<Arc<AgentRuntime>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(payload): Json<Value>,
) -> Response {
    let caller = operator_from_headers(&headers).unwrap_or("api");
    match runtime.update_cron_job(&id, payload, caller).await {
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
            error_response(error)
        }
    }
}

async fn cron_remove(
    State(runtime): State<Arc<AgentRuntime>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    let caller = operator_from_headers(&headers).unwrap_or("api");
    match runtime.remove_cron_job(&id, caller).await {
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
            error_response(error)
        }
    }
}

#[instrument(skip(runtime, headers), fields(source = %req.source, event_type = %req.event_type))]
async fn inject_event(
    State(runtime): State<Arc<AgentRuntime>>,
    headers: HeaderMap,
    Json(req): Json<InjectEventRequest>,
) -> Response {
    let operator = operator_from_headers(&headers).unwrap_or("api");
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
            error_response(error)
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
        Err(error) => error_response(error),
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
    let operator = operator_from_headers(&headers).unwrap_or("api");
    if !require_confirmation(&headers) {
        runtime.audit().record(
            operator,
            "dlq.retry",
            format!("dlq:{id}"),
            "confirm_required",
            "",
        );
        return (
            StatusCode::CONFLICT,
            Json(ErrorBody {
                message: "confirmation required".to_owned(),
            }),
        )
            .into_response();
    }
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
            return error_response(error);
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
            error_response(error)
        }
    }
}

async fn dlq_discard(
    State(runtime): State<Arc<AgentRuntime>>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Response {
    let operator = operator_from_headers(&headers).unwrap_or("api");
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
            error_response(error)
        }
    }
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
    runtime.metrics().update_from(
        bus,
        dlq_depth,
        runtime.inflight_pipelines(),
        runtime.inflight_skills(),
        &plugin_states,
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
    let operator = operator_from_headers(&headers).unwrap_or("api");
    if !require_confirmation(&headers) {
        runtime.audit().record(
            operator,
            "config.set",
            "config",
            "confirm_required",
            "",
        );
        return (
            StatusCode::CONFLICT,
            Json(ErrorBody {
                message: "confirmation required".to_owned(),
            }),
        )
            .into_response();
    }
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
        None => (
            StatusCode::NOT_FOUND,
            Json(ErrorBody {
                message: format!("event not found: {id}"),
            }),
        )
            .into_response(),
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
        return (
            StatusCode::NOT_FOUND,
            Json(ErrorBody {
                message: format!("trace not found: {trace_id}"),
            }),
        )
            .into_response();
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

fn error_response(error: kernel::Error) -> Response {
    let status = match &error {
        Error::NotFound { .. } => StatusCode::NOT_FOUND,
        Error::AlreadyExists { .. } => StatusCode::CONFLICT,
        Error::InvalidStateTransition { .. } => StatusCode::CONFLICT,
        Error::PermissionDenied { .. } => StatusCode::FORBIDDEN,
        Error::ConfigInvalid { .. } => StatusCode::BAD_REQUEST,
        Error::Unrecoverable { .. } => StatusCode::UNPROCESSABLE_ENTITY,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    };
    (status, Json(ErrorBody::from(error))).into_response()
}

#[derive(Debug, Clone, Serialize)]
struct OkResponse {
    ok: bool,
}

#[derive(Debug, Clone, Serialize)]
struct InstallPluginResponse {
    plugin_name: String,
    version: String,
    install_dir: String,
}

#[derive(Debug, Clone, Serialize)]
struct ErrorBody {
    message: String,
}

impl From<kernel::Error> for ErrorBody {
    fn from(error: kernel::Error) -> Self {
        Self {
            message: error.to_string(),
        }
    }
}
