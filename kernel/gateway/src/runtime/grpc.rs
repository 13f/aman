#![forbid(unsafe_code)]
// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! gRPC server for the Aman service.
//!
//! Implements the `Aman` trait generated from `proto/aman.proto`.
//! Each RPC handler delegates to `AgentRuntime` — the same business logic
//! shared with HTTP and stdio JSON-RPC.

use super::agent_runtime::AgentRuntime;
use kernel::agent::AgentStatus;
use kernel::event::EventType;
use kernel::Error;
use persistence::{DeadLetterQueue, DlqFilter};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;
use tonic::{Request, Response, Status};

// Include the generated proto code.
pub mod aman_proto {
    tonic::include_proto!("aman");
}

use aman_proto::aman_server::Aman;
use aman_proto::*;

// ── Error mapping ──

fn map_error(e: Error) -> Status {
    match &e {
        Error::NotFound { name } => Status::not_found(format!("Not found: {name}")),
        Error::AlreadyExists { name } => Status::already_exists(format!("Already exists: {name}")),
        Error::InvalidStateTransition { message } => Status::failed_precondition(message.clone()),
        Error::PermissionDenied { message } => Status::permission_denied(message.clone()),
        Error::ConfigInvalid { message } => Status::invalid_argument(message.clone()),
        Error::BusFull | Error::BackpressureBlocked { .. } => {
            Status::resource_exhausted("Bus full / backpressure")
        }
        Error::Timeout => Status::deadline_exceeded("Timeout"),
        _ => Status::internal(e.to_string()),
    }
}

// ── Helper: JSON-encode a value into `bytes data` ──

fn json_bytes(val: &impl serde::Serialize) -> Vec<u8> {
    serde_json::to_vec(val).unwrap_or_default()
}

fn build_event(source: &str, event_type: &str, payload: serde_json::Value) -> kernel::event::Event {
    kernel::event::Event::new(source, EventType::from(event_type), payload)
}

// ── Server struct ──

pub struct AmanServiceImpl {
    runtime: Arc<AgentRuntime>,
}

impl AmanServiceImpl {
    pub fn new(runtime: Arc<AgentRuntime>) -> Self {
        Self { runtime }
    }
}

// ── Aman trait implementation ──

#[tonic::async_trait]
impl Aman for AmanServiceImpl {
    // -- Health --

    async fn health_live(&self, _req: Request<Empty>) -> Result<Response<HealthResponse>, Status> {
        let ok = self.runtime.is_live();
        Ok(Response::new(HealthResponse { ok }))
    }

    async fn health_ready(&self, _req: Request<Empty>) -> Result<Response<HealthResponse>, Status> {
        let ok = self.runtime.is_ready();
        Ok(Response::new(HealthResponse { ok }))
    }

    async fn health_llm(&self, _req: Request<Empty>) -> Result<Response<HealthResponse>, Status> {
        let ok = self.runtime.is_ready();
        Ok(Response::new(HealthResponse { ok }))
    }

    // -- Agent lifecycle --

    async fn agent_start(&self, _req: Request<Empty>) -> Result<Response<Empty>, Status> {
        self.runtime.start().await.map_err(map_error)?;
        Ok(Response::new(Empty {}))
    }

    async fn agent_shutdown(&self, _req: Request<Empty>) -> Result<Response<Empty>, Status> {
        // gRPC agent.shutdown: trigger-driven, not Ctrl+C. Fresh
        // un-cancelled token so all drain loops run to completion.
        let cancel = CancellationToken::new();
        self.runtime.shutdown(&cancel).await.map_err(map_error)?;
        Ok(Response::new(Empty {}))
    }

    // -- Agent management --

    async fn list_agents(&self, _req: Request<Empty>) -> Result<Response<AgentListResponse>, Status> {
        let agents = self.runtime.agent_registry().list().await;
        let entries: Vec<AgentEntry> = agents
            .into_iter()
            .map(|info| AgentEntry {
                key: info.descriptor.agent_id,
                display_name: info.descriptor.display_name,
                provider: info.descriptor.provider,
                model: info.descriptor.model,
                soul_summary: String::new(),
                session_count: 0,
                is_active: info.status == AgentStatus::Busy,
            })
            .collect();
        Ok(Response::new(AgentListResponse { agents: entries }))
    }

    async fn get_agent(&self, req: Request<GetAgentRequest>) -> Result<Response<AgentInstance>, Status> {
        let r = req.into_inner();
        let info = self
            .runtime
            .agent_registry()
            .get(&r.agent_id)
            .await
            .ok_or_else(|| Status::not_found(format!("agent: {}", r.agent_id)))?;
        Ok(Response::new(AgentInstance {
            agent_id: info.descriptor.agent_id,
            display_name: info.descriptor.display_name,
            provider: info.descriptor.provider,
            model: info.descriptor.model,
            status: format!("{:?}", info.status),
            enabled: info.descriptor.enabled,
            active_session_id: info.active_session_id.unwrap_or_default(),
        }))
    }

    async fn set_agent_status(&self, req: Request<SetAgentStatusRequest>) -> Result<Response<Empty>, Status> {
        let r = req.into_inner();
        let new_status = match r.status.to_lowercase().as_str() {
            "idle" => AgentStatus::Idle,
            "busy" => AgentStatus::Busy,
            "disabled" => AgentStatus::Disabled,
            "error" => AgentStatus::Error,
            _ => return Err(Status::invalid_argument(format!("invalid status: {}", r.status))),
        };
        self.runtime
            .agent_registry()
            .set_status(&r.agent_id, new_status)
            .await
            .map_err(map_error)?;
        Ok(Response::new(Empty {}))
    }

    async fn reload_agent(&self, req: Request<ReloadAgentRequest>) -> Result<Response<Empty>, Status> {
        let r = req.into_inner();
        // reload_agent needs a config reference; skip for now — agents reload via config hot-reload
        let _ = r;
        Ok(Response::new(Empty {}))
    }

    // -- Metrics --

    async fn get_metrics(&self, _req: Request<Empty>) -> Result<Response<MetricsResponse>, Status> {
        let bus = self.runtime.bus_metrics();
        let dlq_depth = self.runtime.dlq().depth();
        let inflight_p = self.runtime.inflight_pipelines();
        let inflight_s = self.runtime.inflight_skills();
        let plugin_states: Vec<(String, String)> = Vec::new();
        self.runtime.metrics().update_from(
            bus,
            dlq_depth,
            inflight_p,
            inflight_s,
            &plugin_states,
            0,
        );
        let encoded = self.runtime.metrics().encode();
        Ok(Response::new(MetricsResponse {
            data: encoded.into_bytes(),
        }))
    }

    async fn debug_metrics(&self, _req: Request<Empty>) -> Result<Response<DebugMetricsResponse>, Status> {
        let bus = self.runtime.bus_metrics();
        let dlq_depth = self.runtime.dlq().depth();
        let instances = self.runtime.workflow_engine().list_instances();
        self.runtime.metrics().update_from(
            bus,
            dlq_depth,
            self.runtime.inflight_pipelines(),
            self.runtime.inflight_skills(),
            &[],
            0,
        );
        let _ = instances;
        let encoded = self.runtime.metrics().encode();
        Ok(Response::new(DebugMetricsResponse {
            data: encoded.into_bytes(),
        }))
    }

    // -- Audit --

    async fn audit_log(&self, req: Request<AuditLogRequest>) -> Result<Response<AuditLogResponse>, Status> {
        let r = req.into_inner();
        let records = self.runtime.audit().list(
            r.action.as_deref(),
            r.operator.as_deref(),
            r.since_ms,
            r.until_ms,
            r.offset.unwrap_or(0) as usize,
            r.limit.unwrap_or(50) as usize,
        );
        let data = json_bytes(&serde_json::json!({ "records": records }));
        Ok(Response::new(AuditLogResponse { data }))
    }

    // -- Events --

    async fn inject_event(&self, req: Request<InjectEventRequest>) -> Result<Response<InjectEventResponse>, Status> {
        let r = req.into_inner();
        let payload: serde_json::Value =
            serde_json::from_slice(&r.payload).unwrap_or(serde_json::Value::Null);
        let event = build_event(&r.source, &r.event_type, payload);
        self.runtime.publish_event(event).await.map_err(map_error)?;
        self.runtime.audit().record("grpc", "event.inject", &r.source, "ok", "");
        let data = json_bytes(&serde_json::json!({ "ok": true }));
        Ok(Response::new(InjectEventResponse { data }))
    }

    async fn push_event(&self, req: Request<PushEventRequest>) -> Result<Response<PushEventResponse>, Status> {
        let r = req.into_inner();
        let payload: serde_json::Value =
            serde_json::from_slice(&r.payload).unwrap_or(serde_json::Value::Null);
        let event = build_event(&r.source, &r.event_type, payload);
        self.runtime.publish_event(event).await.map_err(map_error)?;
        let data = json_bytes(&serde_json::json!({ "ok": true }));
        Ok(Response::new(PushEventResponse { data }))
    }

    async fn list_event_types(&self, _req: Request<Empty>) -> Result<Response<EventTypesResponse>, Status> {
        // EventStore doesn't expose known_types directly; return empty
        Ok(Response::new(EventTypesResponse { types: vec![] }))
    }

    async fn dump_event(&self, req: Request<DumpEventRequest>) -> Result<Response<Event>, Status> {
        let r = req.into_inner();
        let evt = self
            .runtime
            .event_store()
            .get(&r.event_id)
            .ok_or_else(|| Status::not_found(format!("event: {}", r.event_id)))?;
        Ok(Response::new(Event {
            event_id: evt.id.to_string(),
            event_type: format!("{:?}", evt.event_type),
            payload: serde_json::to_vec(&evt.payload).unwrap_or_default(),
            timestamp_ms: evt.timestamp.as_millis(),
            trace_id: evt.metadata.trace_id.to_string(),
        }))
    }

    async fn recent_events(&self, req: Request<RecentEventsRequest>) -> Result<Response<RecentEventsResponse>, Status> {
        let r = req.into_inner();
        let limit = r.limit.unwrap_or(20) as usize;
        let events: Vec<Event> = self
            .runtime
            .event_store()
            .recent(limit)
            .into_iter()
            .map(|evt| Event {
                event_id: evt.id.to_string(),
                event_type: format!("{:?}", evt.event_type),
                payload: serde_json::to_vec(&evt.payload).unwrap_or_default(),
                timestamp_ms: evt.timestamp.as_millis(),
                trace_id: evt.metadata.trace_id.to_string(),
            })
            .collect();
        Ok(Response::new(RecentEventsResponse { events }))
    }

    async fn event_trace(&self, req: Request<EventTraceRequest>) -> Result<Response<TraceResponse>, Status> {
        let r = req.into_inner();
        let events: Vec<Event> = self
            .runtime
            .event_store()
            .trace(&r.trace_id)
            .into_iter()
            .map(|evt| Event {
                event_id: evt.id.to_string(),
                event_type: format!("{:?}", evt.event_type),
                payload: serde_json::to_vec(&evt.payload).unwrap_or_default(),
                timestamp_ms: evt.timestamp.as_millis(),
                trace_id: evt.metadata.trace_id.to_string(),
            })
            .collect();
        Ok(Response::new(TraceResponse { events }))
    }

    // -- DLQ --

    async fn list_dlq(&self, req: Request<ListDlqRequest>) -> Result<Response<DlqListResponse>, Status> {
        let r = req.into_inner();
        let filter = DlqFilter {
            reason: r.reason,
            source: r.source,
            event_type: r.event_type,
            limit: r.limit.map(|v| v as usize),
            offset: r.offset.unwrap_or(0) as usize,
        };
        let items = self.runtime.dlq().list(filter).map_err(map_error)?;
        let entries: Vec<DlqEntry> = items
            .into_iter()
            .map(|e| DlqEntry {
                id: e.id,
                reason: e.reason,
                source: e.event.source.to_string(),
                event_type: format!("{:?}", e.event.event_type),
                payload: serde_json::to_vec(&e.event.payload).unwrap_or_default(),
                timestamp_ms: e.enqueued_at.as_millis(),
            })
            .collect();
        Ok(Response::new(DlqListResponse { entries }))
    }

    async fn dlq_depth(&self, _req: Request<Empty>) -> Result<Response<DlqDepthResponse>, Status> {
        let depth = self.runtime.dlq().depth() as u64;
        Ok(Response::new(DlqDepthResponse { depth }))
    }

    async fn retry_dlq(&self, req: Request<RetryDlqRequest>) -> Result<Response<Empty>, Status> {
        let r = req.into_inner();
        let reason = r.reason.unwrap_or_else(|| "manual retry".into());
        self.runtime.dlq().retry(&r.id, "grpc", &reason).map_err(map_error)?;
        self.runtime.audit().record("grpc", "dlq.retry", &r.id, "ok", "");
        Ok(Response::new(Empty {}))
    }

    async fn discard_dlq(&self, req: Request<DiscardDlqRequest>) -> Result<Response<Empty>, Status> {
        let r = req.into_inner();
        self.runtime.dlq().discard(&r.id).map_err(map_error)?;
        self.runtime.audit().record("grpc", "dlq.discard", &r.id, "ok", "");
        Ok(Response::new(Empty {}))
    }

    // -- Notifications --

    async fn list_notifications(&self, req: Request<ListNotificationsRequest>) -> Result<Response<ListNotificationsResponse>, Status> {
        let r = req.into_inner();
        let limit = r.limit.unwrap_or(50) as usize;
        let offset = r.offset.unwrap_or(0) as usize;
        let ns = self.runtime.notifications().list(false, None, limit, offset);
        let data = json_bytes(&serde_json::json!({ "notifications": ns }));
        Ok(Response::new(ListNotificationsResponse { data }))
    }

    async fn unread_notification_count(&self, _req: Request<Empty>) -> Result<Response<UnreadCountResponse>, Status> {
        let count = self.runtime.notifications().unread_count() as u64;
        Ok(Response::new(UnreadCountResponse { count }))
    }

    async fn dismiss_notification(&self, req: Request<DismissNotificationRequest>) -> Result<Response<Empty>, Status> {
        let r = req.into_inner();
        self.runtime.notifications().dismiss(&r.id);
        Ok(Response::new(Empty {}))
    }

    async fn ack_notification(&self, req: Request<AckNotificationRequest>) -> Result<Response<Empty>, Status> {
        let r = req.into_inner();
        self.runtime.notifications().acknowledge(&r.id);
        Ok(Response::new(Empty {}))
    }

    async fn dismiss_all_notifications(&self, _req: Request<Empty>) -> Result<Response<Empty>, Status> {
        self.runtime.notifications().dismiss_all();
        Ok(Response::new(Empty {}))
    }

    async fn test_notification(&self, req: Request<TestNotificationRequest>) -> Result<Response<Empty>, Status> {
        let r = req.into_inner();
        let level = r.level.unwrap_or_else(|| "info".into());
        let notif = notification::Notification::warning(
            notification::Category::Gateway,
            &level,
            &r.message,
        );
        self.runtime.notifications().push(notif);
        Ok(Response::new(Empty {}))
    }

    // -- Config --

    async fn set_config(&self, req: Request<SetConfigRequest>) -> Result<Response<Empty>, Status> {
        let r = req.into_inner();
        let changed: Vec<String> =
            serde_json::from_slice(&r.patch).unwrap_or_default();
        self.runtime.log_config_change("grpc", &changed);
        Ok(Response::new(Empty {}))
    }

    // -- Runtime --

    async fn runtime_status(&self, _req: Request<Empty>) -> Result<Response<RuntimeStatusResponse>, Status> {
        let status = self.runtime.status().await;
        Ok(Response::new(RuntimeStatusResponse {
            phase: format!("{:?}", status),
            live: self.runtime.is_live(),
            ready: self.runtime.is_ready(),
        }))
    }

    async fn runtime_config(&self, _req: Request<Empty>) -> Result<Response<RuntimeConfigResponse>, Status> {
        let data = json_bytes(&serde_json::json!({
            "bind_addr": self.runtime.bind_addr().to_string(),
            "has_token": self.runtime.api_token().is_some(),
            "phase": format!("{:?}", self.runtime.phase()),
            "live": self.runtime.is_live(),
            "ready": self.runtime.is_ready(),
        }));
        Ok(Response::new(RuntimeConfigResponse { data }))
    }

    // -- Skills --

    async fn list_skills(&self, _req: Request<Empty>) -> Result<Response<SkillListResponse>, Status> {
        let items: Vec<SkillSnapshot> = self
            .runtime
            .skills()
            .list()
            .into_iter()
            .map(|s| SkillSnapshot {
                name: s.name,
                version: s.version,
                description: s.description,
                enabled: s.enabled,
                path: String::new(),
                files: vec![],
            })
            .collect();
        Ok(Response::new(SkillListResponse { items }))
    }

    async fn llm_skills(&self, _req: Request<Empty>) -> Result<Response<LlmSkillsResponse>, Status> {
        let items: Vec<SkillSnapshot> = self
            .runtime
            .llm_skills()
            .into_iter()
            .map(|s| SkillSnapshot {
                name: s.name,
                version: String::new(),
                description: s.description,
                enabled: true,
                path: String::new(),
                files: vec![],
            })
            .collect();
        Ok(Response::new(LlmSkillsResponse { items }))
    }

    async fn search_skills(&self, req: Request<SearchSkillsRequest>) -> Result<Response<SearchSkillsResponse>, Status> {
        let r = req.into_inner();
        let limit = r.limit.unwrap_or(10) as usize;
        let results = self.runtime.skill_search().search(&r.q, limit);
        let items: Vec<SkillSnapshot> = results
            .into_iter()
            .map(|s| SkillSnapshot {
                name: s.name,
                version: s.version,
                description: s.snippet,
                enabled: true,
                path: String::new(),
                files: vec![],
            })
            .collect();
        Ok(Response::new(SearchSkillsResponse { items }))
    }

    async fn get_skill(&self, req: Request<GetSkillRequest>) -> Result<Response<SkillSnapshot>, Status> {
        let r = req.into_inner();
        let snapshot = self
            .runtime
            .skills()
            .snapshot(&r.name)
            .ok_or_else(|| Status::not_found(format!("skill: {}", r.name)))?;
        Ok(Response::new(SkillSnapshot {
            name: snapshot.name,
            version: snapshot.version,
            description: snapshot.description,
            enabled: snapshot.enabled,
            path: String::new(),
            files: vec![],
        }))
    }

    async fn get_skill_content(&self, req: Request<GetSkillContentRequest>) -> Result<Response<SkillContentResponse>, Status> {
        let r = req.into_inner();
        let content = self.runtime.read_skill(&r.name).unwrap_or_default();
        Ok(Response::new(SkillContentResponse {
            name: r.name,
            content: content.into_bytes(),
        }))
    }

    async fn enable_skill(&self, req: Request<EnableSkillRequest>) -> Result<Response<Empty>, Status> {
        let r = req.into_inner();
        self.runtime.skills().enable(&r.name).map_err(map_error)?;
        Ok(Response::new(Empty {}))
    }

    async fn disable_skill(&self, req: Request<DisableSkillRequest>) -> Result<Response<Empty>, Status> {
        let r = req.into_inner();
        self.runtime.skills().disable(&r.name).map_err(map_error)?;
        Ok(Response::new(Empty {}))
    }

    async fn get_skill_versions(&self, req: Request<GetSkillVersionsRequest>) -> Result<Response<SkillVersionsResponse>, Status> {
        let r = req.into_inner();
        let versions: Vec<SkillVersion> = self
            .runtime
            .skill_versions()
            .history(&r.name)
            .map_err(map_error)?
            .into_iter()
            .map(|v| SkillVersion {
                version: v.version,
                hash: v.created_at_ms.to_string(),
            })
            .collect();
        Ok(Response::new(SkillVersionsResponse {
            name: r.name,
            versions,
        }))
    }

    async fn rollback_skill(&self, req: Request<RollbackSkillRequest>) -> Result<Response<Empty>, Status> {
        let r = req.into_inner();
        let dest = self.runtime.skills_dir().join(&r.name).join("SKILL.md");
        self.runtime
            .skill_versions()
            .rollback(&r.name, &r.version, &dest)
            .map_err(map_error)?;
        Ok(Response::new(Empty {}))
    }

    async fn reload_skills(&self, req: Request<ReloadSkillsRequest>) -> Result<Response<Empty>, Status> {
        let _force = req.into_inner().force;
        self.runtime.reload_skills_now().map_err(map_error)?;
        Ok(Response::new(Empty {}))
    }

    // -- Workflows --

    async fn list_workflows(&self, _req: Request<Empty>) -> Result<Response<WorkflowListResponse>, Status> {
        let names = self.runtime.workflow_engine().list_workflows();
        let wfs: Vec<WorkflowDefinition> = names
            .into_iter()
            .filter_map(|name| {
                self.runtime.workflow_engine().get_workflow(&name).map(|w| {
                    WorkflowDefinition {
                        name: w.name,
                        description: String::new(),
                        states: w.states.into_iter().map(|s| s.name).collect(),
                        initial_state: w.initial_state,
                    }
                })
            })
            .collect();
        Ok(Response::new(WorkflowListResponse { workflows: wfs }))
    }

    async fn get_workflow(&self, req: Request<GetWorkflowRequest>) -> Result<Response<WorkflowDefinition>, Status> {
        let r = req.into_inner();
        let w = self
            .runtime
            .workflow_engine()
            .get_workflow(&r.name)
            .ok_or_else(|| Status::not_found(format!("workflow: {}", r.name)))?;
        Ok(Response::new(WorkflowDefinition {
            name: w.name,
            description: String::new(),
            states: w.states.into_iter().map(|s| s.name).collect(),
            initial_state: w.initial_state,
        }))
    }

    async fn create_workflow_instance(
        &self,
        req: Request<CreateWorkflowInstanceRequest>,
    ) -> Result<Response<WorkflowInstance>, Status> {
        let r = req.into_inner();
        let data: serde_json::Value = r
            .initial_data
            .as_deref()
            .and_then(|d| serde_json::from_slice(d).ok())
            .unwrap_or(serde_json::Value::Null);
        let instance = self
            .runtime
            .workflow_engine()
            .create_instance(&r.workflow_name, data)
            .map_err(map_error)?;
        Ok(Response::new(WorkflowInstance {
            id: instance.id,
            workflow_name: instance.workflow_name,
            state: instance.current_state,
            data: serde_json::to_vec(&instance.data).unwrap_or_default(),
            created_at: 0,
        }))
    }

    async fn list_workflow_instances(&self, _req: Request<Empty>) -> Result<Response<WorkflowInstanceListResponse>, Status> {
        let instances: Vec<WorkflowInstance> = self
            .runtime
            .workflow_engine()
            .list_instances()
            .into_iter()
            .map(|i| WorkflowInstance {
                id: i.id,
                workflow_name: i.workflow_name,
                state: i.current_state,
                data: serde_json::to_vec(&i.data).unwrap_or_default(),
                created_at: 0,
            })
            .collect();
        Ok(Response::new(WorkflowInstanceListResponse { instances }))
    }

    async fn get_workflow_instance(&self, req: Request<GetWorkflowInstanceRequest>) -> Result<Response<WorkflowInstance>, Status> {
        let r = req.into_inner();
        let i = self
            .runtime
            .workflow_engine()
            .get_instance(&r.id)
            .ok_or_else(|| Status::not_found(format!("workflow instance: {}", r.id)))?;
        Ok(Response::new(WorkflowInstance {
            id: i.id,
            workflow_name: i.workflow_name,
            state: i.current_state,
            data: serde_json::to_vec(&i.data).unwrap_or_default(),
            created_at: 0,
        }))
    }

    async fn retry_workflow_instance(
        &self,
        req: Request<RetryWorkflowInstanceRequest>,
    ) -> Result<Response<WorkflowTransitionResponse>, Status> {
        let r = req.into_inner();
        let event = build_event("grpc", "workflow:retry", serde_json::json!({
            "instance_id": r.id,
        }));
        let result = self
            .runtime
            .workflow_engine()
            .handle_event(&r.id, event)
            .await
            .map_err(map_error)?;
        self.runtime.audit().record("grpc", "workflow.retry", &r.id, "ok", "");
        Ok(Response::new(WorkflowTransitionResponse {
            ok: result.transitioned,
            new_state: Some(result.to_state),
        }))
    }

    async fn cancel_workflow_instance(
        &self,
        req: Request<CancelWorkflowInstanceRequest>,
    ) -> Result<Response<WorkflowTransitionResponse>, Status> {
        let r = req.into_inner();
        self.runtime
            .workflow_engine()
            .delete_instance(&r.id)
            .map_err(map_error)?;
        self.runtime.audit().record("grpc", "workflow.cancel", &r.id, "ok", "");
        Ok(Response::new(WorkflowTransitionResponse {
            ok: true,
            new_state: Some("cancelled".into()),
        }))
    }

    // -- Plugins --

    async fn list_plugins(&self, req: Request<ListPluginsRequest>) -> Result<Response<PluginListResponse>, Status> {
        let kind_filter = req.into_inner().kind;
        let loader = self.runtime.plugin_loader().await;
        let names = loader.loaded_plugins();
        let plugins: Vec<PluginEntry> = names
            .into_iter()
            .map(|name| {
                let state = loader.state_of(&name).map(|s| format!("{s:?}"));
                let _kind = String::new();
                PluginEntry {
                    name,
                    version: String::new(),
                    enabled: state.as_deref() == Some("Enabled"),
                    kind: _kind,
                    capabilities: vec![],
                }
            })
            .collect();
        drop(loader);
        let _ = kind_filter;
        Ok(Response::new(PluginListResponse { plugins }))
    }

    async fn enable_plugin(&self, req: Request<EnablePluginRequest>) -> Result<Response<Empty>, Status> {
        let r = req.into_inner();
        self.runtime.enable_plugin(&r.name).await.map_err(map_error)?;
        Ok(Response::new(Empty {}))
    }

    async fn disable_plugin(&self, req: Request<DisablePluginRequest>) -> Result<Response<Empty>, Status> {
        let r = req.into_inner();
        self.runtime.disable_plugin(&r.name).await.map_err(map_error)?;
        Ok(Response::new(Empty {}))
    }

    async fn uninstall_plugin(&self, req: Request<UninstallPluginRequest>) -> Result<Response<Empty>, Status> {
        let r = req.into_inner();
        self.runtime.uninstall_plugin(&r.name).await.map_err(map_error)?;
        self.runtime.audit().record("grpc", "plugin.uninstall", &r.name, "ok", "");
        Ok(Response::new(Empty {}))
    }

    async fn install_plugin(&self, req: Request<InstallPluginRequest>) -> Result<Response<InstallPluginResponse>, Status> {
        let r = req.into_inner();
        let result = self
            .runtime
            .plugin_installer()
            .install_from_archive_bytes(&r.data)
            .map_err(map_error)?;
        let data = json_bytes(&serde_json::json!({
            "name": result.manifest.name,
            "version": result.manifest.version.to_string(),
            "installed": true,
        }));
        Ok(Response::new(InstallPluginResponse { data }))
    }

    // -- Cron --

    async fn add_cron_job(&self, req: Request<AddCronJobRequest>) -> Result<Response<Empty>, Status> {
        let r = req.into_inner();
        self.runtime
            .add_cron_job(r.id, r.expression, &r.agent_key, "grpc")
            .await
            .map_err(map_error)?;
        Ok(Response::new(Empty {}))
    }

    async fn update_cron_job(&self, req: Request<UpdateCronJobRequest>) -> Result<Response<Empty>, Status> {
        let r = req.into_inner();
        let patch: serde_json::Value =
            serde_json::from_slice(&r.patch).unwrap_or(serde_json::Value::Null);
        self.runtime
            .update_cron_job(&r.id, patch, &r.agent_key, "grpc")
            .await
            .map_err(map_error)?;
        Ok(Response::new(Empty {}))
    }

    async fn remove_cron_job(&self, req: Request<RemoveCronJobRequest>) -> Result<Response<Empty>, Status> {
        let r = req.into_inner();
        self.runtime
            .remove_cron_job(&r.id, &r.agent_key, "grpc")
            .await
            .map_err(map_error)?;
        Ok(Response::new(Empty {}))
    }

    // -- Sources --

    async fn pause_source(&self, req: Request<PauseSourceRequest>) -> Result<Response<Empty>, Status> {
        let r = req.into_inner();
        self.runtime.sources().pause(&r.id).await.map_err(map_error)?;
        self.runtime.audit().record("grpc", "source.pause", &r.id, "ok", "");
        Ok(Response::new(Empty {}))
    }

    async fn resume_source(&self, req: Request<ResumeSourceRequest>) -> Result<Response<Empty>, Status> {
        let r = req.into_inner();
        self.runtime.sources().resume(&r.id).await.map_err(map_error)?;
        self.runtime.audit().record("grpc", "source.resume", &r.id, "ok", "");
        Ok(Response::new(Empty {}))
    }

    async fn set_source_config(&self, req: Request<SetSourceConfigRequest>) -> Result<Response<Empty>, Status> {
        let r = req.into_inner();
        let config: serde_json::Value =
            serde_json::from_slice(&r.config).unwrap_or(serde_json::Value::Null);
        self.runtime.sources().reconfigure(&r.id, config).await.map_err(map_error)?;
        Ok(Response::new(Empty {}))
    }

    // -- Chat Sessions --

    async fn list_chat_sessions(&self, _req: Request<Empty>) -> Result<Response<ChatSessionListResponse>, Status> {
        let list = self
            .runtime
            .session_store()
            .map(|s| s.list_all().unwrap_or_default())
            .unwrap_or_default();
        let sessions: Vec<ChatSessionEntry> = list
            .into_iter()
            .map(|s| ChatSessionEntry {
                id: s.id,
                title: String::new(),
                message_count: s.message_count as u64,
                state: s.state,
                created_at: s.created_at,
                last_active_at: s.last_active_at,
            })
            .collect();
        Ok(Response::new(ChatSessionListResponse { sessions }))
    }

    async fn create_chat_session(
        &self,
        req: Request<CreateChatSessionRequest>,
    ) -> Result<Response<ChatSessionCreatedResponse>, Status> {
        let agent_key = req.into_inner().agent_key.unwrap_or_else(|| "default".to_owned());
        let session_id = uuid::Uuid::new_v4().to_string();
        let store = self.runtime.session_store_for_agent(&agent_key)
            .or_else(|| self.runtime.session_store());
        if let Some(store) = store {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as i64;
            let _ = store.upsert(&super::session_store::SessionRecord {
                id: session_id.clone(),
                agent_id: agent_key.clone(),
                state: "active".into(),
                message_count: 0,
                created_at: now,
                last_active_at: now,
                session_type: "persistent".into(),
                title: None,
                reflected_at: None,
            });
        }
        Ok(Response::new(ChatSessionCreatedResponse { session_id }))
    }

    async fn get_chat_session_state(
        &self,
        req: Request<GetChatSessionStateRequest>,
    ) -> Result<Response<ChatSessionStateResponse>, Status> {
        let r = req.into_inner();
        let state = self
            .runtime
            .workflow_engine()
            .get_instance(&r.session_id)
            .map(|i| {
                serde_json::json!({
                    "id": i.id,
                    "current_state": i.current_state,
                    "data": i.data,
                })
            })
            .unwrap_or(serde_json::json!({ "id": r.session_id, "current_state": "unknown" }));
        Ok(Response::new(ChatSessionStateResponse {
            data: json_bytes(&state),
        }))
    }

    async fn get_chat_session_history(
        &self,
        req: Request<GetChatSessionHistoryRequest>,
    ) -> Result<Response<ChatSessionHistoryResponse>, Status> {
        let r = req.into_inner();
        let events = self
            .runtime
            .session_store()
            .map(|s| s.load_session_events(&r.session_id))
            .unwrap_or_default();
        let data = json_bytes(&serde_json::json!({
            "session_id": r.session_id,
            "messages": events,
        }));
        Ok(Response::new(ChatSessionHistoryResponse { data }))
    }

    async fn send_chat_message(
        &self,
        req: Request<SendChatMessageRequest>,
    ) -> Result<Response<SendChatMessageResponse>, Status> {
        let r = req.into_inner();
        let event = build_event("chat", "MessageReceived", serde_json::json!({
            "text": r.text,
            "session_id": r.session_id,
            "trace_prev": r.trace_prev,
        }));
        self.runtime.publish_event(event).await.map_err(map_error)?;
        let data = json_bytes(&serde_json::json!({ "ok": true, "session_id": r.session_id }));
        Ok(Response::new(SendChatMessageResponse { data }))
    }

    async fn close_chat_session(&self, req: Request<CloseChatSessionRequest>) -> Result<Response<Empty>, Status> {
        let r = req.into_inner();
        if let Some(store) = self.runtime.find_session_store(&r.session_id) {
            let _ = store.upsert(&super::session_store::SessionRecord {
                id: r.session_id.clone(),
                agent_id: String::new(),
                state: "closed".into(),
                message_count: 0,
                created_at: 0,
                last_active_at: 0,
                session_type: "persistent".into(),
                title: None,
                reflected_at: None,
            });
        }
        Ok(Response::new(Empty {}))
    }

    async fn stop_chat_session(&self, req: Request<StopChatSessionRequest>) -> Result<Response<Empty>, Status> {
        let r = req.into_inner();
        let event = build_event("chat", "stop_generation", serde_json::json!({
            "session_id": r.session_id,
        }));
        self.runtime.publish_event(event).await.map_err(map_error)?;
        Ok(Response::new(Empty {}))
    }

    async fn retry_chat_session(&self, req: Request<RetryChatSessionRequest>) -> Result<Response<Empty>, Status> {
        let r = req.into_inner();
        let event = build_event("chat", "retry_last", serde_json::json!({
            "session_id": r.session_id,
        }));
        self.runtime.publish_event(event).await.map_err(map_error)?;
        let _ = r.expected_version;
        Ok(Response::new(Empty {}))
    }

    async fn edit_chat_message(&self, req: Request<EditChatMessageRequest>) -> Result<Response<Empty>, Status> {
        let r = req.into_inner();
        let event = build_event("chat", "edit_message", serde_json::json!({
            "session_id": r.session_id,
            "message_id": r.message_id,
            "text": r.text,
        }));
        self.runtime.publish_event(event).await.map_err(map_error)?;
        Ok(Response::new(Empty {}))
    }

    async fn delete_chat_session(&self, req: Request<DeleteChatSessionRequest>) -> Result<Response<Empty>, Status> {
        let r = req.into_inner();
        if let Some(store) = self.runtime.session_store() {
            let _ = store.delete(&r.session_id);
        }
        let _ = self.runtime.workflow_engine().delete_instance(&r.session_id);
        self.runtime.audit().record("grpc", "chat.session.delete", &r.session_id, "ok", "");
        Ok(Response::new(Empty {}))
    }

    // -- Soul --

    async fn get_soul_info(&self, _req: Request<Empty>) -> Result<Response<SoulInfoResponse>, Status> {
        let info = self.runtime.soul_runtime().map(|sr| {
            let current = sr.current_soul();
            serde_json::json!({
                "name": current.name,
                "identity": current.identity,
            })
        });
        Ok(Response::new(SoulInfoResponse {
            data: json_bytes(&info.unwrap_or(serde_json::json!({ "name": null }))),
        }))
    }

    async fn get_soul_raw(&self, _req: Request<Empty>) -> Result<Response<SoulRawResponse>, Status> {
        let content = self
            .runtime
            .soul_runtime()
            .map(|sr| sr.current_soul().raw.clone())
            .unwrap_or_default();
        Ok(Response::new(SoulRawResponse {
            data: json_bytes(&serde_json::json!({ "content": content })),
        }))
    }

    async fn update_soul(&self, req: Request<UpdateSoulRequest>) -> Result<Response<Empty>, Status> {
        let r = req.into_inner();
        self.runtime.update_soul(&r.name_or_path).await.map_err(map_error)?;
        Ok(Response::new(Empty {}))
    }

    async fn get_soul_system_prompt(&self, _req: Request<Empty>) -> Result<Response<SoulSystemPromptResponse>, Status> {
        let prompt = self
            .runtime
            .soul_runtime()
            .map(|sr| {
                let soul = sr.current_soul();
                let skills_json = serde_json::to_value(&*self.runtime.llm_skills()).unwrap_or_default();
                let tools_json = serde_json::json!([]);
                self.runtime.self_bridge()
                    .build_full_system_prompt(
                        &soul.raw, &skills_json, &tools_json, None,
                        &super::self_bridge::SystemPromptContext {
                            claude_md_content: None,
                            cwd: std::env::current_dir().ok().as_ref().and_then(|p| p.to_str()),
                            platform: "desktop", model: None, provider: None,
                        },
                    )
                    .unwrap_or_else(|| soul.raw.clone())
            })
            .unwrap_or_default();
        Ok(Response::new(SoulSystemPromptResponse {
            data: json_bytes(&serde_json::json!({ "system_prompt": prompt })),
        }))
    }

    // -- Capabilities --

    async fn list_capabilities(&self, _req: Request<Empty>) -> Result<Response<CapabilityListResponse>, Status> {
        let caps = self.runtime.get_capabilities().await;
        let capabilities: Vec<CapabilityEntry> = caps
            .into_iter()
            .map(|c| CapabilityEntry { capability: c })
            .collect();
        Ok(Response::new(CapabilityListResponse { capabilities }))
    }

    // -- Tools --

    async fn execute_tool(&self, req: Request<ExecuteToolRequest>) -> Result<Response<ExecuteToolResponse>, Status> {
        let r = req.into_inner();
        let arguments: serde_json::Value =
            serde_json::from_slice(&r.arguments).unwrap_or(serde_json::Value::Null);
        let tool = self.runtime.tools().get(&r.name).ok_or_else(|| {
            Status::not_found(format!("tool: {}", r.name))
        })?;
        let ctx = kernel::context::ToolContext::default();
        let result = tool.execute(arguments, ctx).await.map_err(map_error)?;
        Ok(Response::new(ExecuteToolResponse {
            data: json_bytes(&serde_json::json!({ "result": result })),
        }))
    }

    async fn tool_auth_respond(&self, req: Request<ToolAuthRespondRequest>) -> Result<Response<Empty>, Status> {
        let r = req.into_inner();
        let approved = r.response.to_lowercase() == "approve" || r.response == "yes";
        self.runtime.auth_registry().resolve(&r.auth_id, approved);
        Ok(Response::new(Empty {}))
    }
}

// ── Server launch ──

use std::net::SocketAddr;
use tonic::transport::Server;

/// Start the gRPC server on the given address, sharing the runtime.
/// Returns a handle whose `shutdown` method can be used to stop it gracefully.
pub async fn serve_grpc(
    runtime: Arc<AgentRuntime>,
    addr: SocketAddr,
) -> Result<GrpcServerHandle, tonic::transport::Error> {
    let svc = AmanServiceImpl::new(runtime);

    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();

    let server = Server::builder()
        .add_service(aman_proto::aman_server::AmanServer::new(svc))
        .serve_with_shutdown(addr, async {
            let _ = shutdown_rx.await;
        });

    let handle = GrpcServerHandle {
        shutdown: Some(shutdown_tx),
        local_addr: addr,
    };

    tokio::spawn(async move {
        if let Err(e) = server.await {
            tracing::error!(%e, "gRPC server exited with error");
        }
    });

    Ok(handle)
}

pub struct GrpcServerHandle {
    shutdown: Option<tokio::sync::oneshot::Sender<()>>,
    pub local_addr: SocketAddr,
}

impl GrpcServerHandle {
    pub fn shutdown(mut self) {
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
    }
}
