#![forbid(unsafe_code)]
// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! gRPC client for the Aman service.
//!
//! Wraps a tonic `AmanClient` with convenience methods matching the CLI
//! command patterns. All methods delegate to the generated gRPC stubs.

use std::net::SocketAddr;
use tonic::transport::Channel;

// Include the generated proto code (same proto, generated in CLI's build.rs).
pub mod aman_proto {
    tonic::include_proto!("aman");
}

use aman_proto::aman_client::AmanClient;
use aman_proto::*;

/// Convenience wrapper around the generated `AmanClient`.
pub struct GrpcClient {
    inner: AmanClient<Channel>,
}

#[allow(dead_code)]
impl GrpcClient {
    /// Connect to the gRPC server at `addr`.
    pub async fn connect(addr: SocketAddr) -> Result<Self, tonic::transport::Error> {
        let uri = format!("http://{addr}")
            .parse()
            .expect("valid gRPC URI from socket addr");
        let channel = Channel::builder(uri).connect().await?;
        let inner = AmanClient::new(channel);
        Ok(Self { inner })
    }

    // -- Health --

    pub async fn health_live(&mut self) -> Result<bool, tonic::Status> {
        let resp = self.inner.health_live(Empty {}).await?;
        Ok(resp.into_inner().ok)
    }

    pub async fn health_ready(&mut self) -> Result<bool, tonic::Status> {
        let resp = self.inner.health_ready(Empty {}).await?;
        Ok(resp.into_inner().ok)
    }

    // -- Agent --

    pub async fn agent_start(&mut self) -> Result<(), tonic::Status> {
        self.inner.agent_start(Empty {}).await?;
        Ok(())
    }

    pub async fn agent_shutdown(&mut self) -> Result<(), tonic::Status> {
        self.inner.agent_shutdown(Empty {}).await?;
        Ok(())
    }

    pub async fn list_agents_json(&mut self) -> Result<String, tonic::Status> {
        let resp = self.inner.list_agents(Empty {}).await?;
        let agents: Vec<serde_json::Value> = resp
            .into_inner()
            .agents
            .into_iter()
            .map(|a| {
                serde_json::json!({
                    "key": a.key,
                    "display_name": a.display_name,
                    "provider": a.provider,
                    "model": a.model,
                    "is_active": a.is_active,
                })
            })
            .collect();
        Ok(serde_json::to_string(&agents).unwrap_or_default())
    }

    // -- Metrics --

    pub async fn get_metrics_json(&mut self) -> Result<String, tonic::Status> {
        let resp = self.inner.get_metrics(Empty {}).await?;
        Ok(String::from_utf8_lossy(&resp.into_inner().data).to_string())
    }

    // -- Audit --

    pub async fn audit_log_json(
        &mut self,
        action: Option<String>,
        operator: Option<String>,
        since_ms: Option<i64>,
        until_ms: Option<i64>,
        limit: Option<u32>,
        offset: Option<u32>,
    ) -> Result<String, tonic::Status> {
        let resp = self
            .inner
            .audit_log(AuditLogRequest {
                action,
                operator,
                since_ms,
                until_ms,
                limit,
                offset,
            })
            .await?;
        Ok(String::from_utf8_lossy(&resp.into_inner().data).to_string())
    }

    // -- Events --

    pub async fn inject_event_json(
        &mut self,
        source: String,
        event_type: String,
        payload: Vec<u8>,
    ) -> Result<String, tonic::Status> {
        let resp = self
            .inner
            .inject_event(InjectEventRequest {
                source,
                event_type,
                payload,
            })
            .await?;
        Ok(String::from_utf8_lossy(&resp.into_inner().data).to_string())
    }

    pub async fn push_event_json(
        &mut self,
        source: String,
        event_type: String,
        payload: Vec<u8>,
        agent_id: Option<String>,
    ) -> Result<String, tonic::Status> {
        let resp = self
            .inner
            .push_event(PushEventRequest {
                source,
                event_type,
                payload,
                agent_id,
                priority: None,
                delivery: None,
                ttl_ms: None,
            })
            .await?;
        Ok(String::from_utf8_lossy(&resp.into_inner().data).to_string())
    }

    pub async fn event_types(&mut self) -> Result<Vec<String>, tonic::Status> {
        let resp = self.inner.list_event_types(Empty {}).await?;
        Ok(resp.into_inner().types)
    }

    pub async fn dump_event_json(&mut self, event_id: String) -> Result<String, tonic::Status> {
        let resp = self
            .inner
            .dump_event(DumpEventRequest { event_id })
            .await?;
        let e = resp.into_inner();
        Ok(serde_json::json!({
            "event_id": e.event_id,
            "event_type": e.event_type,
            "timestamp_ms": e.timestamp_ms,
            "trace_id": e.trace_id,
        })
        .to_string())
    }

    pub async fn recent_events_json(&mut self, limit: u32) -> Result<String, tonic::Status> {
        let resp = self
            .inner
            .recent_events(RecentEventsRequest {
                limit: Some(limit),
                source: None,
            })
            .await?;
        let events: Vec<serde_json::Value> = resp
            .into_inner()
            .events
            .into_iter()
            .map(|e| {
                serde_json::json!({
                    "event_id": e.event_id,
                    "event_type": e.event_type,
                    "timestamp_ms": e.timestamp_ms,
                    "trace_id": e.trace_id,
                })
            })
            .collect();
        Ok(serde_json::to_string(&events).unwrap_or_default())
    }

    pub async fn event_trace_json(&mut self, trace_id: String) -> Result<String, tonic::Status> {
        let resp = self
            .inner
            .event_trace(EventTraceRequest { trace_id })
            .await?;
        let events: Vec<serde_json::Value> = resp
            .into_inner()
            .events
            .into_iter()
            .map(|e| {
                serde_json::json!({
                    "event_id": e.event_id,
                    "event_type": e.event_type,
                    "timestamp_ms": e.timestamp_ms,
                    "trace_id": e.trace_id,
                })
            })
            .collect();
        Ok(serde_json::to_string(&events).unwrap_or_default())
    }

    // -- DLQ --

    pub async fn dlq_list_json(
        &mut self,
        reason: Option<String>,
        source: Option<String>,
        event_type: Option<String>,
        limit: Option<u32>,
        offset: Option<u32>,
    ) -> Result<String, tonic::Status> {
        let resp = self
            .inner
            .list_dlq(ListDlqRequest {
                reason,
                source,
                event_type,
                limit,
                offset,
            })
            .await?;
        let entries: Vec<serde_json::Value> = resp
            .into_inner()
            .entries
            .into_iter()
            .map(|e| {
                serde_json::json!({
                    "id": e.id,
                    "reason": e.reason,
                    "source": e.source,
                    "event_type": e.event_type,
                    "timestamp_ms": e.timestamp_ms,
                })
            })
            .collect();
        Ok(serde_json::to_string(&entries).unwrap_or_default())
    }

    pub async fn dlq_depth(&mut self) -> Result<u64, tonic::Status> {
        let resp = self.inner.dlq_depth(Empty {}).await?;
        Ok(resp.into_inner().depth)
    }

    pub async fn dlq_retry(&mut self, id: String, reason: Option<String>) -> Result<(), tonic::Status> {
        self.inner.retry_dlq(RetryDlqRequest { id, reason }).await?;
        Ok(())
    }

    pub async fn dlq_discard(&mut self, id: String) -> Result<(), tonic::Status> {
        self.inner.discard_dlq(DiscardDlqRequest { id }).await?;
        Ok(())
    }

    // -- Notifications --

    pub async fn list_notifications_json(&mut self, limit: u32, offset: u32) -> Result<String, tonic::Status> {
        let resp = self
            .inner
            .list_notifications(ListNotificationsRequest {
                limit: Some(limit),
                offset: Some(offset),
            })
            .await?;
        Ok(String::from_utf8_lossy(&resp.into_inner().data).to_string())
    }

    pub async fn unread_count(&mut self) -> Result<u64, tonic::Status> {
        let resp = self.inner.unread_notification_count(Empty {}).await?;
        Ok(resp.into_inner().count)
    }

    pub async fn dismiss_notification(&mut self, id: String) -> Result<(), tonic::Status> {
        self.inner
            .dismiss_notification(DismissNotificationRequest { id })
            .await?;
        Ok(())
    }

    pub async fn dismiss_all_notifications(&mut self) -> Result<(), tonic::Status> {
        self.inner.dismiss_all_notifications(Empty {}).await?;
        Ok(())
    }

    // -- Runtime --

    pub async fn runtime_status_json(&mut self) -> Result<String, tonic::Status> {
        let resp = self.inner.runtime_status(Empty {}).await?;
        let inner = resp.into_inner();
        Ok(serde_json::json!({
            "phase": inner.phase,
            "live": inner.live,
            "ready": inner.ready,
        })
        .to_string())
    }

    pub async fn runtime_config_json(&mut self) -> Result<String, tonic::Status> {
        let resp = self.inner.runtime_config(Empty {}).await?;
        Ok(String::from_utf8_lossy(&resp.into_inner().data).to_string())
    }

    // -- Skills --

    pub async fn list_skills_json(&mut self) -> Result<String, tonic::Status> {
        let resp = self.inner.list_skills(Empty {}).await?;
        let items: Vec<serde_json::Value> = resp
            .into_inner()
            .items
            .into_iter()
            .map(|s| {
                serde_json::json!({
                    "name": s.name,
                    "version": s.version,
                    "description": s.description,
                    "enabled": s.enabled,
                })
            })
            .collect();
        Ok(serde_json::to_string(&items).unwrap_or_default())
    }

    pub async fn search_skills_json(&mut self, q: String, limit: u32) -> Result<String, tonic::Status> {
        let resp = self
            .inner
            .search_skills(SearchSkillsRequest {
                q,
                limit: Some(limit),
            })
            .await?;
        let items: Vec<serde_json::Value> = resp
            .into_inner()
            .items
            .into_iter()
            .map(|s| {
                serde_json::json!({
                    "name": s.name,
                    "version": s.version,
                    "description": s.description,
                    "enabled": s.enabled,
                })
            })
            .collect();
        Ok(serde_json::to_string(&items).unwrap_or_default())
    }

    pub async fn get_skill_json(&mut self, name: String) -> Result<String, tonic::Status> {
        let resp = self.inner.get_skill(GetSkillRequest { name }).await?;
        let s = resp.into_inner();
        Ok(serde_json::json!({
            "name": s.name,
            "version": s.version,
            "description": s.description,
            "enabled": s.enabled,
        })
        .to_string())
    }

    pub async fn enable_skill(&mut self, name: String) -> Result<(), tonic::Status> {
        self.inner.enable_skill(EnableSkillRequest { name }).await?;
        Ok(())
    }

    pub async fn disable_skill(&mut self, name: String) -> Result<(), tonic::Status> {
        self.inner.disable_skill(DisableSkillRequest { name }).await?;
        Ok(())
    }

    pub async fn rollback_skill(&mut self, name: String, version: String) -> Result<(), tonic::Status> {
        self.inner
            .rollback_skill(RollbackSkillRequest { name, version })
            .await?;
        Ok(())
    }

    // -- Workflows --

    pub async fn list_workflows_json(&mut self) -> Result<String, tonic::Status> {
        let resp = self.inner.list_workflows(Empty {}).await?;
        let wfs: Vec<serde_json::Value> = resp
            .into_inner()
            .workflows
            .into_iter()
            .map(|w| {
                serde_json::json!({
                    "name": w.name,
                    "states": w.states,
                    "initial_state": w.initial_state,
                })
            })
            .collect();
        Ok(serde_json::to_string(&wfs).unwrap_or_default())
    }

    pub async fn list_workflow_instances_json(&mut self) -> Result<String, tonic::Status> {
        let resp = self.inner.list_workflow_instances(Empty {}).await?;
        let instances: Vec<serde_json::Value> = resp
            .into_inner()
            .instances
            .into_iter()
            .map(|i| {
                serde_json::json!({
                    "id": i.id,
                    "workflow_name": i.workflow_name,
                    "state": i.state,
                })
            })
            .collect();
        Ok(serde_json::to_string(&instances).unwrap_or_default())
    }

    pub async fn get_workflow_instance_json(&mut self, id: String) -> Result<String, tonic::Status> {
        let resp = self
            .inner
            .get_workflow_instance(GetWorkflowInstanceRequest { id })
            .await?;
        let i = resp.into_inner();
        Ok(serde_json::json!({
            "id": i.id,
            "workflow_name": i.workflow_name,
            "state": i.state,
        })
        .to_string())
    }

    pub async fn retry_workflow(&mut self, id: String) -> Result<(), tonic::Status> {
        self.inner
            .retry_workflow_instance(RetryWorkflowInstanceRequest {
                id,
                reason: None,
            })
            .await?;
        Ok(())
    }

    pub async fn cancel_workflow(&mut self, id: String) -> Result<(), tonic::Status> {
        self.inner
            .cancel_workflow_instance(CancelWorkflowInstanceRequest {
                id,
                reason: None,
            })
            .await?;
        Ok(())
    }

    // -- Plugins --

    pub async fn list_plugins_json(&mut self) -> Result<String, tonic::Status> {
        let resp = self
            .inner
            .list_plugins(ListPluginsRequest { kind: None })
            .await?;
        let plugins: Vec<serde_json::Value> = resp
            .into_inner()
            .plugins
            .into_iter()
            .map(|p| {
                serde_json::json!({
                    "name": p.name,
                    "version": p.version,
                    "enabled": p.enabled,
                    "capabilities": p.capabilities,
                })
            })
            .collect();
        Ok(serde_json::to_string(&plugins).unwrap_or_default())
    }

    pub async fn enable_plugin(&mut self, name: String) -> Result<(), tonic::Status> {
        self.inner.enable_plugin(EnablePluginRequest { name }).await?;
        Ok(())
    }

    pub async fn disable_plugin(&mut self, name: String) -> Result<(), tonic::Status> {
        self.inner.disable_plugin(DisablePluginRequest { name }).await?;
        Ok(())
    }

    pub async fn uninstall_plugin(&mut self, name: String) -> Result<(), tonic::Status> {
        self.inner
            .uninstall_plugin(UninstallPluginRequest { name })
            .await?;
        Ok(())
    }

    pub async fn install_plugin_json(&mut self, data: Vec<u8>) -> Result<String, tonic::Status> {
        let resp = self
            .inner
            .install_plugin(InstallPluginRequest {
                file_name: "plugin.tar.gz".into(),
                data,
            })
            .await?;
        Ok(String::from_utf8_lossy(&resp.into_inner().data).to_string())
    }

    // -- Cron --

    pub async fn add_cron(&mut self, id: String, expression: String) -> Result<(), tonic::Status> {
        self.inner
            .add_cron_job(AddCronJobRequest { id, expression })
            .await?;
        Ok(())
    }

    pub async fn update_cron(&mut self, id: String, patch: Vec<u8>) -> Result<(), tonic::Status> {
        self.inner
            .update_cron_job(UpdateCronJobRequest { id, patch })
            .await?;
        Ok(())
    }

    pub async fn remove_cron(&mut self, id: String) -> Result<(), tonic::Status> {
        self.inner
            .remove_cron_job(RemoveCronJobRequest { id })
            .await?;
        Ok(())
    }

    // -- Sources --

    pub async fn pause_source(&mut self, id: String) -> Result<(), tonic::Status> {
        self.inner.pause_source(PauseSourceRequest { id }).await?;
        Ok(())
    }

    pub async fn resume_source(&mut self, id: String) -> Result<(), tonic::Status> {
        self.inner.resume_source(ResumeSourceRequest { id }).await?;
        Ok(())
    }

    pub async fn source_config(&mut self, id: String, config: Vec<u8>) -> Result<(), tonic::Status> {
        self.inner
            .set_source_config(SetSourceConfigRequest { id, config })
            .await?;
        Ok(())
    }

    // -- Chat --

    pub async fn list_chat_sessions_json(&mut self) -> Result<String, tonic::Status> {
        let resp = self.inner.list_chat_sessions(Empty {}).await?;
        let sessions: Vec<serde_json::Value> = resp
            .into_inner()
            .sessions
            .into_iter()
            .map(|s| {
                serde_json::json!({
                    "id": s.id,
                    "title": s.title,
                    "message_count": s.message_count,
                    "state": s.state,
                    "created_at": s.created_at,
                    "last_active_at": s.last_active_at,
                })
            })
            .collect();
        Ok(serde_json::to_string(&sessions).unwrap_or_default())
    }

    pub async fn create_chat_session_json(&mut self, agent_key: Option<String>) -> Result<String, tonic::Status> {
        let resp = self
            .inner
            .create_chat_session(CreateChatSessionRequest { agent_key })
            .await?;
        Ok(serde_json::json!({ "session_id": resp.into_inner().session_id }).to_string())
    }

    pub async fn send_chat_message_json(&mut self, session_id: String, text: String) -> Result<String, tonic::Status> {
        let resp = self
            .inner
            .send_chat_message(SendChatMessageRequest {
                session_id,
                text,
                trace_prev: None,
            })
            .await?;
        Ok(String::from_utf8_lossy(&resp.into_inner().data).to_string())
    }

    pub async fn delete_chat_session(&mut self, session_id: String) -> Result<(), tonic::Status> {
        self.inner
            .delete_chat_session(DeleteChatSessionRequest { session_id })
            .await?;
        Ok(())
    }

    // -- Soul --

    pub async fn soul_info_json(&mut self) -> Result<String, tonic::Status> {
        let resp = self.inner.get_soul_info(Empty {}).await?;
        Ok(String::from_utf8_lossy(&resp.into_inner().data).to_string())
    }

    pub async fn soul_raw_json(&mut self) -> Result<String, tonic::Status> {
        let resp = self.inner.get_soul_raw(Empty {}).await?;
        Ok(String::from_utf8_lossy(&resp.into_inner().data).to_string())
    }

    pub async fn update_soul(&mut self, name_or_path: String) -> Result<(), tonic::Status> {
        self.inner
            .update_soul(UpdateSoulRequest { name_or_path })
            .await?;
        Ok(())
    }

    // -- Capabilities --

    pub async fn list_capabilities_json(&mut self) -> Result<String, tonic::Status> {
        let resp = self.inner.list_capabilities(Empty {}).await?;
        let caps: Vec<serde_json::Value> = resp
            .into_inner()
            .capabilities
            .into_iter()
            .map(|c| serde_json::json!({ "capability": c.capability }))
            .collect();
        Ok(serde_json::to_string(&caps).unwrap_or_default())
    }

    // -- Tools --

    pub async fn execute_tool_json(&mut self, name: String, arguments: Vec<u8>) -> Result<String, tonic::Status> {
        let resp = self
            .inner
            .execute_tool(ExecuteToolRequest { name, arguments })
            .await?;
        Ok(String::from_utf8_lossy(&resp.into_inner().data).to_string())
    }
}
