#![forbid(unsafe_code)]
// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! JSON-RPC 2.0 server over stdin/stdout.
//!
//! Reads newline-delimited JSON-RPC requests from stdin, dispatches to
//! `AgentRuntime` methods, and writes JSON-RPC responses to stdout.
//! No authentication — the caller is a trusted local process.
//!
//! println! is intentional here: this is the JSON-RPC wire protocol, not
//! application logging. It must write structured JSON to stdout exactly.

#![allow(clippy::print_stdout)]

use super::agent_runtime::AgentRuntime;
use kernel::agent::AgentStatus;
use kernel::event::EventType;
use kernel::{AmanResult, Error};
use persistence::{DeadLetterQueue, DlqFilter};
use serde::Deserialize;
use serde_json::Value;
use std::sync::Arc;

// ── JSON-RPC 2.0 types ──

#[derive(Debug, Deserialize)]
struct Request {
    jsonrpc: String,
    #[serde(default)]
    id: Value,
    method: String,
    #[serde(default)]
    params: Option<Value>,
}

#[derive(Debug, serde::Serialize)]
struct Response {
    jsonrpc: &'static str,
    #[serde(skip_serializing_if = "value_is_null")]
    id: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<JsonRpcError>,
}

#[derive(Debug, serde::Serialize)]
struct JsonRpcError {
    code: i32,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    data: Option<Value>,
}

fn value_is_null(v: &Value) -> bool {
    v.is_null()
}

// ── Public API ──

/// Run the JSON-RPC 2.0 loop on stdin/stdout until EOF or shutdown.
pub async fn serve_stdio(runtime: Arc<AgentRuntime>) -> AmanResult<()> {
    use tokio::io::{AsyncBufReadExt, BufReader};

    let stdin = BufReader::new(tokio::io::stdin());
    let mut lines = stdin.lines();

    while let Ok(Some(line)) = lines.next_line().await {
        let line = line.trim().to_owned();
        if line.is_empty() {
            continue;
        }

        let req: Request = match serde_json::from_str::<Request>(&line) {
            Ok(r) => {
                if r.jsonrpc != "2.0" {
                    let resp = Response {
                        jsonrpc: "2.0",
                        id: r.id,
                        result: None,
                        error: Some(JsonRpcError {
                            code: -32600,
                            message: "Invalid Request: jsonrpc must be \"2.0\"".into(),
                            data: None,
                        }),
                    };
                    print_jsonrpc_response(&resp);
                    continue;
                }
                r
            }
            Err(e) => {
                let resp = Response {
                    jsonrpc: "2.0",
                    id: Value::Null,
                    result: None,
                    error: Some(JsonRpcError {
                        code: -32700,
                        message: format!("Parse error: {e}"),
                        data: None,
                    }),
                };
                print_jsonrpc_response(&resp);
                continue;
            }
        };

        let result = dispatch(&runtime, &req.method, req.params.as_ref()).await;
        let resp = match result {
            Ok(val) => Response {
                jsonrpc: "2.0",
                id: req.id,
                result: Some(val),
                error: None,
            },
            Err(e) => Response {
                jsonrpc: "2.0",
                id: req.id,
                result: None,
                error: Some(map_error(e)),
            },
        };
        print_jsonrpc_response(&resp);
    }

    Ok(())
}

// ── Error mapping ──

fn map_error(e: Error) -> JsonRpcError {
    let (code, message) = match &e {
        Error::NotFound { name } => (100, format!("Not found: {name}")),
        Error::AlreadyExists { name } => (101, format!("Already exists: {name}")),
        Error::InvalidStateTransition { message } => (102, message.clone()),
        Error::PermissionDenied { message } => (103, message.clone()),
        Error::ConfigInvalid { message } => (104, message.clone()),
        Error::BusFull | Error::BackpressureBlocked { .. } => {
            (105, "Bus full / backpressure".into())
        }
        Error::Timeout => (106, "Timeout".into()),
        _ => (-32603, e.to_string()),
    };
    JsonRpcError {
        code,
        message,
        data: None,
    }
}

/// Print a JSON-RPC Response to stdout. If serialization fails, print a
/// hardcoded JSON-RPC internal-error object so the client always receives
/// valid JSON-RPC, never an empty line.
fn print_jsonrpc_response(resp: &Response) {
    match serde_json::to_string(resp) {
        Ok(json) => println!("{json}"),
        Err(e) => {
            // Fallback: a hardcoded, always-valid JSON-RPC error response.
            // This should never happen in practice (Response is always
            // serializable), but if it does, the client gets a valid
            // JSON-RPC error instead of a protocol-breaking empty line.
            println!(
                r#"{{"jsonrpc":"2.0","id":null,"error":{{"code":-32603,"message":"internal serialization error: {}"}}}}"#,
                e.to_string().replace('"', r#"\""#)
            );
        }
    }
}

// ── Dispatch ──

async fn dispatch(
    runtime: &AgentRuntime,
    method: &str,
    params: Option<&Value>,
) -> AmanResult<Value> {
    match method {
        // -- Health --
        "health.live" => health(runtime, "live").await,
        "health.ready" => health(runtime, "ready").await,
        "health.llm" => health(runtime, "llm").await,

        // -- Agent lifecycle --
        "agent.start" => {
            runtime.start().await?;
            Ok(Value::String("started".into()))
        }
        "agent.shutdown" => {
            runtime.shutdown().await?;
            Ok(Value::String("shutdown".into()))
        }

        // -- Agent management --
        "agent.list" => list_agents(runtime).await,
        "agent.get" => get_agent(runtime, params).await,
        "agent.set_status" => set_agent_status(runtime, params).await,

        // -- Metrics --
        "metrics.get" => get_metrics(runtime).await,

        // -- Audit --
        "audit.log" => audit_log(runtime, params).await,

        // -- Events --
        "event.inject" => inject_event(runtime, params).await,
        "event.push" => push_event(runtime, params).await,
        "event.dump" => dump_event(runtime, params).await,
        "event.recent" => recent_events(runtime, params).await,
        "event.trace" => trace_event(runtime, params).await,

        // -- DLQ --
        "dlq.list" => dlq_list(runtime, params).await,
        "dlq.depth" => dlq_depth(runtime).await,
        "dlq.retry" => dlq_retry(runtime, params).await,
        "dlq.discard" => dlq_discard(runtime, params).await,

        // -- Notifications --
        "notification.list" => notifications_list(runtime, params).await,
        "notification.unread_count" => notification_unread_count(runtime).await,
        "notification.dismiss" => notification_dismiss(runtime, params).await,
        "notification.ack" => notification_ack(runtime, params).await,
        "notification.dismiss_all" => {
            runtime.notifications().dismiss_all();
            Ok(Value::String("ok".into()))
        }

        // -- Config --
        "config.set" => config_set(runtime, params).await,

        // -- Runtime --
        "runtime.status" => runtime_status(runtime).await,
        "runtime.config" => runtime_config(runtime).await,

        // -- Skills --
        "skill.list" => skill_list(runtime).await,
        "skill.llm" => skill_llm(runtime).await,
        "skill.search" => skill_search(runtime, params).await,
        "skill.info" => skill_info(runtime, params).await,
        "skill.enable" => skill_enable_disable(runtime, params, true).await,
        "skill.disable" => skill_enable_disable(runtime, params, false).await,
        "skill.versions" => skill_versions(runtime, params).await,
        "skill.rollback" => skill_rollback(runtime, params).await,
        "skill.reload" => {
            let report = runtime.reload_skills_now()?;
            Ok(serde_json::json!({
                "inserted": report.inserted,
                "updated_same_version": report.updated_same_version,
                "updated_new_version": report.updated_new_version,
                "removed": report.removed,
                "changed": report.changed(),
            }))
        }

        // -- Workflows --
        "workflow.list" => workflow_list(runtime).await,
        "workflow.info" => workflow_info(runtime, params).await,
        "workflow.instance_list" => workflow_instance_list(runtime).await,
        "workflow.instance" => workflow_instance(runtime, params).await,
        "workflow.instance_create" => workflow_instance_create(runtime, params).await,
        "workflow.retry" => workflow_retry(runtime, params).await,
        "workflow.cancel" => workflow_cancel(runtime, params).await,

        // -- Plugins --
        "plugin.list" => plugin_list(runtime).await,
        "plugin.enable" => plugin_enable_disable(runtime, params, true).await,
        "plugin.disable" => plugin_enable_disable(runtime, params, false).await,
        "plugin.uninstall" => plugin_uninstall(runtime, params).await,
        "plugin.install" => plugin_install(runtime, params).await,

        // -- Cron --
        "cron.add" => cron_add(runtime, params).await,
        "cron.update" => cron_update(runtime, params).await,
        "cron.remove" => cron_remove(runtime, params).await,

        // -- Sources --
        "source.pause" => source_pause_resume(runtime, params, true).await,
        "source.resume" => source_pause_resume(runtime, params, false).await,
        "source.config" => source_config(runtime, params).await,

        // -- Chat Sessions --
        "chat.sessions" => chat_sessions(runtime, params).await,
        "chat.session_create" => chat_session_create(runtime, params).await,
        "chat.session_state" => chat_session_state(runtime, params).await,
        "chat.session_history" => chat_session_history(runtime, params).await,
        "chat.send" => chat_send(runtime, params).await,
        "chat.session_close" => chat_session_close(runtime, params).await,
        "chat.stop" => chat_stop(runtime, params).await,
        "chat.retry" => chat_retry(runtime, params).await,
        "chat.edit" => chat_edit(runtime, params).await,
        "chat.session_delete" => chat_session_delete(runtime, params).await,

        // -- Soul --
        "soul.info" => soul_info(runtime).await,
        "soul.raw" => soul_raw(runtime).await,
        "soul.update" => soul_update(runtime, params).await,

        // -- Capabilities --
        "capability.list" => capabilities_list(runtime).await,

        // -- Tools --
        "tool.execute" => tool_execute(runtime, params).await,
        "tool.auth_respond" => tool_auth_respond(runtime, params).await,

        // -- Plugin auth --
        "plugin.auth_respond" => plugin_auth_respond(runtime, params).await,

        _ => Err(Error::NotFound {
            name: format!("method: {method}"),
        }),
    }
}

// ── Param helpers ──

fn get_param_str(params: Option<&Value>, key: &str) -> Option<String> {
    params
        .and_then(|p| p.get(key))
        .and_then(|v| v.as_str())
        .map(String::from)
}

fn require_param_str(params: Option<&Value>, key: &str) -> AmanResult<String> {
    get_param_str(params, key).ok_or_else(|| Error::ConfigInvalid {
        message: format!("missing required param: {key}"),
    })
}

fn get_param_u64(params: Option<&Value>, key: &str) -> Option<u64> {
    params.and_then(|p| p.get(key)).and_then(|v| v.as_u64())
}

fn get_param_i64(params: Option<&Value>, key: &str) -> Option<i64> {
    params.and_then(|p| p.get(key)).and_then(|v| v.as_i64())
}

fn get_param_bool(params: Option<&Value>, key: &str) -> Option<bool> {
    params.and_then(|p| p.get(key)).and_then(|v| v.as_bool())
}

fn get_param_value(params: Option<&Value>, key: &str) -> Option<Value> {
    params.and_then(|p| p.get(key)).cloned()
}

/// Build an Event from required params, filling in defaults via builder-style mutations.
fn build_event(source: &str, event_type: &str, payload: Value) -> kernel::event::Event {
    kernel::event::Event::new(source, EventType::from(event_type), payload)
}

// ── Handler implementations ──

async fn health(runtime: &AgentRuntime, kind: &str) -> AmanResult<Value> {
    let ok = match kind {
        "live" => runtime.is_live(),
        "ready" => runtime.is_ready(),
        "llm" => runtime.is_ready(),
        _ => runtime.is_ready(),
    };
    Ok(serde_json::json!({ "ok": ok }))
}

async fn list_agents(runtime: &AgentRuntime) -> AmanResult<Value> {
    let agents: Vec<Value> = runtime
        .agent_registry()
        .list()
        .await
        .into_iter()
        .map(|info| {
            serde_json::json!({
                "agent_id": info.descriptor.agent_id,
                "display_name": info.descriptor.display_name,
                "provider": info.descriptor.provider,
                "model": info.descriptor.model,
                "status": format!("{:?}", info.status),
                "enabled": info.descriptor.enabled,
                "active_session_id": info.active_session_id,
            })
        })
        .collect();
    Ok(serde_json::json!({ "agents": agents }))
}

async fn get_agent(runtime: &AgentRuntime, params: Option<&Value>) -> AmanResult<Value> {
    let agent_id = require_param_str(params, "agent_id")?;
    let info = runtime.agent_registry().get(&agent_id).await.ok_or_else(|| {
        Error::NotFound {
            name: format!("agent: {agent_id}"),
        }
    })?;
    Ok(serde_json::json!({
        "agent_id": info.descriptor.agent_id,
        "display_name": info.descriptor.display_name,
        "provider": info.descriptor.provider,
        "model": info.descriptor.model,
        "status": format!("{:?}", info.status),
        "enabled": info.descriptor.enabled,
        "active_session_id": info.active_session_id,
    }))
}

async fn set_agent_status(runtime: &AgentRuntime, params: Option<&Value>) -> AmanResult<Value> {
    let agent_id = require_param_str(params, "agent_id")?;
    let status_str = require_param_str(params, "status")?;
    let new_status = match status_str.to_lowercase().as_str() {
        "idle" => AgentStatus::Idle,
        "busy" => AgentStatus::Busy,
        "disabled" => AgentStatus::Disabled,
        "error" => AgentStatus::Error,
        _ => {
            return Err(Error::ConfigInvalid {
                message: format!("invalid agent status: {status_str}"),
            })
        }
    };
    runtime.agent_registry().set_status(&agent_id, new_status).await?;
    Ok(serde_json::json!({ "ok": true }))
}

async fn get_metrics(runtime: &AgentRuntime) -> AmanResult<Value> {
    let bus = runtime.bus_metrics();
    let dlq_depth = runtime.dlq().depth();
    let inflight_p = runtime.inflight_pipelines();
    let inflight_s = runtime.inflight_skills();
    let plugin_states: Vec<(String, String)> = Vec::new();
    let session_count = 0usize;
    runtime.metrics().update_from(bus, dlq_depth, inflight_p, inflight_s, &plugin_states, session_count);
    let encoded = runtime.metrics().encode();
    Ok(serde_json::from_str(&encoded).unwrap_or(Value::Null))
}

async fn audit_log(runtime: &AgentRuntime, params: Option<&Value>) -> AmanResult<Value> {
    let action = get_param_str(params, "action");
    let operator = get_param_str(params, "operator");
    let since = get_param_i64(params, "since_ms");
    let until = get_param_i64(params, "until_ms");
    let offset = get_param_u64(params, "offset").unwrap_or(0) as usize;
    let limit = get_param_u64(params, "limit").unwrap_or(50) as usize;

    let records = runtime.audit().list(
        action.as_deref(),
        operator.as_deref(),
        since,
        until,
        offset,
        limit,
    );
    let items: Vec<Value> = records
        .into_iter()
        .map(|r| {
            serde_json::json!({
                "id": r.id,
                "timestamp_ms": r.timestamp_ms,
                "operator": r.operator,
                "action": r.action,
                "target": r.target,
                "outcome": r.outcome,
                "detail": r.detail,
            })
        })
        .collect();
    Ok(serde_json::json!({ "records": items }))
}

async fn inject_event(runtime: &AgentRuntime, params: Option<&Value>) -> AmanResult<Value> {
    let source = require_param_str(params, "source")?;
    let event_type = require_param_str(params, "event_type")?;
    let payload = get_param_value(params, "payload").unwrap_or(Value::Null);

    let event = build_event(&source, &event_type, payload);
    runtime.publish_event(event).await?;
    runtime.audit().record("cli", "event.inject", &source, "ok", "");
    Ok(serde_json::json!({ "ok": true }))
}

async fn push_event(runtime: &AgentRuntime, params: Option<&Value>) -> AmanResult<Value> {
    let source = require_param_str(params, "source")?;
    let event_type = require_param_str(params, "event_type")?;
    let payload = get_param_value(params, "payload").unwrap_or(Value::Null);

    let event = build_event(&source, &event_type, payload);
    // Note: agent routing, priority, and TTL are set at the workflow/skill level,
    // not as Event fields in this version.

    runtime.publish_event(event).await?;
    Ok(serde_json::json!({ "ok": true }))
}

async fn dump_event(runtime: &AgentRuntime, params: Option<&Value>) -> AmanResult<Value> {
    let id = require_param_str(params, "event_id")?;
    let evt = runtime.event_store().get(&id).ok_or_else(|| Error::NotFound {
        name: format!("event: {id}"),
    })?;
    Ok(serde_json::json!({
        "id": evt.id.to_string(),
        "event_type": format!("{:?}", evt.event_type),
        "payload": evt.payload,
        "timestamp": evt.timestamp.as_millis(),
        "trace_id": evt.metadata.trace_id.to_string(),
    }))
}

async fn recent_events(runtime: &AgentRuntime, params: Option<&Value>) -> AmanResult<Value> {
    let limit = get_param_u64(params, "limit").unwrap_or(20) as usize;
    let events: Vec<Value> = runtime
        .event_store()
        .recent(limit)
        .into_iter()
        .map(|evt| {
            serde_json::json!({
                "id": evt.id.to_string(),
                "event_type": format!("{:?}", evt.event_type),
                "timestamp": evt.timestamp.as_millis(),
                "trace_id": evt.metadata.trace_id.to_string(),
            })
        })
        .collect();
    Ok(serde_json::json!({ "events": events }))
}

async fn trace_event(runtime: &AgentRuntime, params: Option<&Value>) -> AmanResult<Value> {
    let trace_id = require_param_str(params, "trace_id")?;
    let chain: Vec<Value> = runtime
        .event_store()
        .trace(&trace_id)
        .into_iter()
        .map(|evt| {
            serde_json::json!({
                "id": evt.id.to_string(),
                "event_type": format!("{:?}", evt.event_type),
                "timestamp": evt.timestamp.as_millis(),
                "trace_id": evt.metadata.trace_id.to_string(),
            })
        })
        .collect();
    Ok(serde_json::json!({ "events": chain }))
}

// -- DLQ --

async fn dlq_list(runtime: &AgentRuntime, params: Option<&Value>) -> AmanResult<Value> {
    let filter = DlqFilter {
        reason: get_param_str(params, "reason"),
        source: get_param_str(params, "source"),
        event_type: get_param_str(params, "event_type"),
        limit: get_param_u64(params, "limit").map(|v| v as usize),
        offset: get_param_u64(params, "offset").unwrap_or(0) as usize,
    };
    let entries = runtime.dlq().list(filter)?;
    let items: Vec<Value> = entries
        .into_iter()
        .map(|e| {
            serde_json::json!({
                "id": e.id,
                "reason": e.reason,
                "source": e.event.source.to_string(),
                "event_type": format!("{:?}", e.event.event_type),
                "payload": e.event.payload,
                "enqueued_at": e.enqueued_at.as_millis(),
                "retry_count": e.retry_count,
            })
        })
        .collect();
    Ok(serde_json::json!({ "entries": items }))
}

async fn dlq_depth(runtime: &AgentRuntime) -> AmanResult<Value> {
    let depth = runtime.dlq().depth();
    Ok(serde_json::json!({ "depth": depth }))
}

async fn dlq_retry(runtime: &AgentRuntime, params: Option<&Value>) -> AmanResult<Value> {
    let id = require_param_str(params, "id")?;
    let operator = get_param_str(params, "operator").unwrap_or_else(|| "cli".into());
    let reason = get_param_str(params, "reason").unwrap_or_else(|| "manual retry".into());
    let _event = runtime.dlq().retry(&id, &operator, &reason)?;
    runtime.audit().record("cli", "dlq.retry", &id, "ok", "");
    Ok(serde_json::json!({ "ok": true }))
}

async fn dlq_discard(runtime: &AgentRuntime, params: Option<&Value>) -> AmanResult<Value> {
    let id = require_param_str(params, "id")?;
    let _ = runtime.dlq().discard(&id)?;
    runtime.audit().record("cli", "dlq.discard", &id, "ok", "");
    Ok(serde_json::json!({ "ok": true }))
}

// -- Notifications --

async fn notifications_list(runtime: &AgentRuntime, params: Option<&Value>) -> AmanResult<Value> {
    let limit = get_param_u64(params, "limit").unwrap_or(50) as usize;
    let offset = get_param_u64(params, "offset").unwrap_or(0) as usize;
    let ns = runtime.notifications().list(false, None, limit, offset);
    let items: Vec<Value> = ns
        .into_iter()
        .map(|n| {
            serde_json::json!({
                "id": n.id,
                "severity": format!("{:?}", n.severity),
                "title": n.title,
                "message": n.message,
                "dismissed": n.dismissed,
                "category": format!("{:?}", n.category),
                "created_at": n.created_at,
            })
        })
        .collect();
    Ok(serde_json::json!({ "notifications": items }))
}

async fn notification_unread_count(runtime: &AgentRuntime) -> AmanResult<Value> {
    let count = runtime.notifications().unread_count();
    Ok(serde_json::json!({ "count": count }))
}

async fn notification_dismiss(runtime: &AgentRuntime, params: Option<&Value>) -> AmanResult<Value> {
    let id = require_param_str(params, "id")?;
    runtime.notifications().dismiss(&id);
    Ok(serde_json::json!({ "ok": true }))
}

async fn notification_ack(runtime: &AgentRuntime, params: Option<&Value>) -> AmanResult<Value> {
    let id = require_param_str(params, "id")?;
    runtime.notifications().acknowledge(&id);
    Ok(serde_json::json!({ "ok": true }))
}

async fn config_set(runtime: &AgentRuntime, params: Option<&Value>) -> AmanResult<Value> {
    let fields = get_param_value(params, "changed_fields");
    let changed: Vec<String> = fields
        .and_then(|v| serde_json::from_value(v).ok())
        .unwrap_or_default();
    runtime.log_config_change("cli", &changed);
    Ok(serde_json::json!({ "ok": true }))
}

async fn runtime_status(runtime: &AgentRuntime) -> AmanResult<Value> {
    let status = runtime.status().await;
    Ok(serde_json::json!({
        "phase": format!("{:?}", status),
        "live": runtime.is_live(),
        "ready": runtime.is_ready(),
    }))
}

async fn runtime_config(runtime: &AgentRuntime) -> AmanResult<Value> {
    Ok(serde_json::json!({
        "bind_addr": runtime.bind_addr().to_string(),
        "has_token": runtime.api_token().is_some(),
        "phase": format!("{:?}", runtime.phase()),
        "live": runtime.is_live(),
        "ready": runtime.is_ready(),
    }))
}

// -- Skills --

async fn skill_list(runtime: &AgentRuntime) -> AmanResult<Value> {
    let items: Vec<Value> = runtime
        .skills()
        .list()
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
    Ok(serde_json::json!({ "items": items }))
}

async fn skill_llm(runtime: &AgentRuntime) -> AmanResult<Value> {
    let items: Vec<Value> = runtime
        .llm_skills()
        .into_iter()
        .map(|s| {
            serde_json::json!({
                "name": s.name,
                "description": s.description,
            })
        })
        .collect();
    Ok(serde_json::json!({ "items": items }))
}

async fn skill_search(runtime: &AgentRuntime, params: Option<&Value>) -> AmanResult<Value> {
    let q = get_param_str(params, "q").unwrap_or_default();
    let limit = get_param_u64(params, "limit").unwrap_or(10) as usize;
    let results = runtime.skill_search().search(&q, limit);
    let items: Vec<Value> = results
        .into_iter()
        .map(|s| {
            serde_json::json!({
                "name": s.name,
                "version": s.version,
                "score": s.score,
                "snippet": s.snippet,
                "matched_field": s.matched_field,
            })
        })
        .collect();
    Ok(serde_json::json!({ "items": items }))
}

async fn skill_info(runtime: &AgentRuntime, params: Option<&Value>) -> AmanResult<Value> {
    let name = require_param_str(params, "name")?;
    let snapshot = runtime.skills().snapshot(&name).ok_or_else(|| Error::NotFound {
        name: format!("skill: {name}"),
    })?;
    Ok(serde_json::json!({
        "name": snapshot.name,
        "version": snapshot.version,
        "description": snapshot.description,
        "enabled": snapshot.enabled,
        "triggers": snapshot.triggers.iter().map(|t| format!("{t:?}")).collect::<Vec<_>>(),
    }))
}

async fn skill_enable_disable(
    runtime: &AgentRuntime,
    params: Option<&Value>,
    enable: bool,
) -> AmanResult<Value> {
    let name = require_param_str(params, "name")?;
    if enable {
        runtime.skills().enable(&name)?;
    } else {
        runtime.skills().disable(&name)?;
    }
    Ok(serde_json::json!({ "ok": true }))
}

async fn skill_versions(runtime: &AgentRuntime, params: Option<&Value>) -> AmanResult<Value> {
    let name = require_param_str(params, "name")?;
    let versions: Vec<Value> = runtime
        .skill_versions()
        .history(&name)?
        .into_iter()
        .map(|v| {
            serde_json::json!({ "version": v.version, "created_at_ms": v.created_at_ms })
        })
        .collect();
    Ok(serde_json::json!({ "name": name, "versions": versions }))
}

async fn skill_rollback(runtime: &AgentRuntime, params: Option<&Value>) -> AmanResult<Value> {
    let name = require_param_str(params, "name")?;
    let version = require_param_str(params, "version")?;
    let dest = runtime.skills_dir().join(&name).join("SKILL.md");
    runtime.skill_versions().rollback(&name, &version, &dest)?;
    Ok(serde_json::json!({ "ok": true }))
}

// -- Workflows --

async fn workflow_list(runtime: &AgentRuntime) -> AmanResult<Value> {
    let names = runtime.workflow_engine().list_workflows();
    let wfs: Vec<Value> = names
        .into_iter()
        .filter_map(|name| {
            runtime.workflow_engine().get_workflow(&name).map(|w| {
                serde_json::json!({
                    "name": w.name,
                    "states": w.states.iter().map(|s| &s.name).collect::<Vec<_>>(),
                    "initial_state": w.initial_state,
                    "final_states": w.final_states,
                })
            })
        })
        .collect();
    Ok(serde_json::json!({ "workflows": wfs }))
}

async fn workflow_info(runtime: &AgentRuntime, params: Option<&Value>) -> AmanResult<Value> {
    let name = require_param_str(params, "name")?;
    let w = runtime.workflow_engine().get_workflow(&name).ok_or_else(|| {
        Error::NotFound {
            name: format!("workflow: {name}"),
        }
    })?;
    Ok(serde_json::json!({
        "name": w.name,
        "states": w.states.iter().map(|s| &s.name).collect::<Vec<_>>(),
        "initial_state": w.initial_state,
        "final_states": w.final_states,
    }))
}

async fn workflow_instance_list(runtime: &AgentRuntime) -> AmanResult<Value> {
    let instances: Vec<Value> = runtime
        .workflow_engine()
        .list_instances()
        .into_iter()
        .map(|i| {
            serde_json::json!({
                "id": i.id,
                "workflow_name": i.workflow_name,
                "current_state": i.current_state,
                "data": i.data,
                "total_retry_count": i.total_retry_count,
            })
        })
        .collect();
    Ok(serde_json::json!({ "instances": instances }))
}

async fn workflow_instance(runtime: &AgentRuntime, params: Option<&Value>) -> AmanResult<Value> {
    let id = require_param_str(params, "id")?;
    let i = runtime.workflow_engine().get_instance(&id).ok_or_else(|| {
        Error::NotFound {
            name: format!("workflow instance: {id}"),
        }
    })?;
    Ok(serde_json::json!({
        "id": i.id,
        "workflow_name": i.workflow_name,
        "current_state": i.current_state,
        "data": i.data,
        "total_retry_count": i.total_retry_count,
    }))
}

async fn workflow_instance_create(
    runtime: &AgentRuntime,
    params: Option<&Value>,
) -> AmanResult<Value> {
    let workflow_name = require_param_str(params, "workflow_name")?;
    let initial_data = get_param_value(params, "initial_data").unwrap_or(Value::Null);
    let instance = runtime.workflow_engine().create_instance(&workflow_name, initial_data)?;
    Ok(serde_json::json!({
        "id": instance.id,
        "workflow_name": instance.workflow_name,
        "current_state": instance.current_state,
        "data": instance.data,
    }))
}

async fn workflow_retry(runtime: &AgentRuntime, params: Option<&Value>) -> AmanResult<Value> {
    let id = require_param_str(params, "id")?;
    // Publish a retry event for the workflow instance
    let event = build_event("cli", "workflow:retry", serde_json::json!({
        "instance_id": id,
    }));
    let result = runtime.workflow_engine().handle_event(&id, event).await?;
    runtime.audit().record("cli", "workflow.retry", &id, "ok", "");
    Ok(serde_json::json!({
        "ok": true,
        "from_state": result.from_state,
        "to_state": result.to_state,
    }))
}

async fn workflow_cancel(runtime: &AgentRuntime, params: Option<&Value>) -> AmanResult<Value> {
    let id = require_param_str(params, "id")?;
    runtime.workflow_engine().delete_instance(&id)?;
    runtime.audit().record("cli", "workflow.cancel", &id, "ok", "");
    Ok(serde_json::json!({ "ok": true }))
}

// -- Plugins --

async fn plugin_list(runtime: &AgentRuntime) -> AmanResult<Value> {
    let loader = runtime.plugin_loader().await;
    let names = loader.loaded_plugins();
    let plugins: Vec<Value> = names
        .into_iter()
        .map(|name| {
            let state = loader.state_of(&name).map(|s| format!("{s:?}"));
            let health = loader.health_of(&name).map(|h| format!("{h:?}"));
            serde_json::json!({
                "name": name,
                "state": state,
                "health": health,
            })
        })
        .collect();
    drop(loader);
    Ok(serde_json::json!({ "plugins": plugins }))
}

async fn plugin_enable_disable(
    runtime: &AgentRuntime,
    params: Option<&Value>,
    enable: bool,
) -> AmanResult<Value> {
    let name = require_param_str(params, "name")?;
    if enable {
        runtime.enable_plugin(&name).await?;
    } else {
        runtime.disable_plugin(&name).await?;
    }
    Ok(serde_json::json!({ "ok": true }))
}

async fn plugin_uninstall(runtime: &AgentRuntime, params: Option<&Value>) -> AmanResult<Value> {
    let name = require_param_str(params, "name")?;
    runtime.uninstall_plugin(&name).await?;
    runtime.audit().record("cli", "plugin.uninstall", &name, "ok", "");
    Ok(serde_json::json!({ "ok": true }))
}

async fn plugin_install(runtime: &AgentRuntime, params: Option<&Value>) -> AmanResult<Value> {
    let data = get_param_str(params, "data").ok_or_else(|| Error::ConfigInvalid {
        message: "missing base64 data for plugin install".into(),
    })?;
    let bytes = base64_decode(&data)?;
    let result = runtime.plugin_installer().install_from_archive_bytes(&bytes)?;
    Ok(serde_json::json!({
        "name": result.manifest.name,
        "version": result.manifest.version.to_string(),
        "installed": true,
    }))
}

fn base64_decode(s: &str) -> AmanResult<Vec<u8>> {
    let alphabet = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut result = Vec::new();
    let mut buf = 0u32;
    let mut bits = 0u8;

    for ch in s.chars() {
        if ch == '=' {
            break;
        }
        if ch.is_whitespace() {
            continue;
        }
        let idx = alphabet.find(ch).ok_or_else(|| Error::ConfigInvalid {
            message: format!("invalid base64 character: {ch}"),
        })? as u32;
        buf = (buf << 6) | idx;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            result.push((buf >> bits) as u8);
            buf &= (1 << bits) - 1;
        }
    }
    Ok(result)
}

// -- Cron --

async fn cron_add(runtime: &AgentRuntime, params: Option<&Value>) -> AmanResult<Value> {
    let id = require_param_str(params, "id")?;
    let expression = require_param_str(params, "expression")?;
    runtime.add_cron_job(id, expression, "cli").await?;
    Ok(serde_json::json!({ "ok": true }))
}

async fn cron_update(runtime: &AgentRuntime, params: Option<&Value>) -> AmanResult<Value> {
    let id = require_param_str(params, "id")?;
    let patch = get_param_value(params, "patch").unwrap_or(Value::Null);
    runtime.update_cron_job(&id, patch, "cli").await?;
    Ok(serde_json::json!({ "ok": true }))
}

async fn cron_remove(runtime: &AgentRuntime, params: Option<&Value>) -> AmanResult<Value> {
    let id = require_param_str(params, "id")?;
    runtime.remove_cron_job(&id, "cli").await?;
    Ok(serde_json::json!({ "ok": true }))
}

// -- Sources --

async fn source_pause_resume(
    runtime: &AgentRuntime,
    params: Option<&Value>,
    pause: bool,
) -> AmanResult<Value> {
    let id = require_param_str(params, "id")?;
    let action = if pause { "pause" } else { "resume" };
    if pause {
        runtime.sources().pause(&id).await?;
    } else {
        runtime.sources().resume(&id).await?;
    }
    runtime.audit().record("cli", format!("source.{action}"), &id, "ok", "");
    Ok(serde_json::json!({ "ok": true }))
}

async fn source_config(runtime: &AgentRuntime, params: Option<&Value>) -> AmanResult<Value> {
    let id = require_param_str(params, "id")?;
    let config = get_param_value(params, "config").unwrap_or(Value::Null);
    runtime.sources().reconfigure(&id, config).await?;
    Ok(serde_json::json!({ "ok": true }))
}

// -- Chat Sessions --

async fn chat_sessions(runtime: &AgentRuntime, params: Option<&Value>) -> AmanResult<Value> {
    let agent_key = get_param_str(params, "agent_key");
    let list = if let Some(store) = runtime.session_store() {
        store.list_all().unwrap_or_default()
    } else {
        Vec::new()
    };
    let sessions: Vec<Value> = list
        .into_iter()
        .filter(|s| {
            agent_key.as_ref().is_none_or(|ak| {
                s.session_type.as_str() == ak.as_str() || s.session_type == "persistent"
            })
        })
        .map(|s| {
            serde_json::json!({
                "id": s.id,
                "state": s.state,
                "message_count": s.message_count,
                "created_at": s.created_at,
                "last_active_at": s.last_active_at,
                "session_type": s.session_type,
            })
        })
        .collect();
    Ok(serde_json::json!({ "sessions": sessions }))
}

async fn chat_session_create(runtime: &AgentRuntime, params: Option<&Value>) -> AmanResult<Value> {
    let agent_key = get_param_str(params, "agent_key").unwrap_or_else(|| "default".to_owned());
    let session_id = uuid::Uuid::new_v4().to_string();
    let store = runtime.session_store_for_agent(&agent_key)
        .or_else(|| runtime.session_store());
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
            reflected_at: None,
            title: None,
        });
    }
    Ok(serde_json::json!({ "session_id": session_id }))
}

async fn chat_session_state(runtime: &AgentRuntime, params: Option<&Value>) -> AmanResult<Value> {
    let session_id = require_param_str(params, "session_id")?;
    let state = runtime
        .workflow_engine()
        .get_instance(&session_id)
        .map(|i| {
            serde_json::json!({
                "id": i.id,
                "current_state": i.current_state,
                "data": i.data,
            })
        })
        .unwrap_or(serde_json::json!({ "id": session_id, "current_state": "unknown" }));
    Ok(state)
}

async fn chat_session_history(runtime: &AgentRuntime, params: Option<&Value>) -> AmanResult<Value> {
    let session_id = require_param_str(params, "session_id")?;
    let events = runtime
        .session_store()
        .map(|s| s.load_session_events(&session_id))
        .unwrap_or_default();
    Ok(serde_json::json!({ "session_id": session_id, "messages": events }))
}

async fn chat_send(runtime: &AgentRuntime, params: Option<&Value>) -> AmanResult<Value> {
    let session_id = require_param_str(params, "session_id")?;
    let text = require_param_str(params, "text")?;
    let trace_prev = get_param_str(params, "trace_prev");

    let event = build_event("chat", "MessageReceived", serde_json::json!({
        "text": text,
        "session_id": session_id,
        "trace_prev": trace_prev,
    }));
    runtime.publish_event(event).await?;
    Ok(serde_json::json!({ "ok": true, "session_id": session_id }))
}

async fn chat_session_close(runtime: &AgentRuntime, params: Option<&Value>) -> AmanResult<Value> {
    let session_id = require_param_str(params, "session_id")?;
    if let Some(store) = runtime.find_session_store(&session_id) {
        let _ = store.upsert(&super::session_store::SessionRecord {
            id: session_id.clone(),
            agent_id: String::new(),
            state: "closed".into(),
            message_count: 0,
            created_at: 0,
            last_active_at: 0,
            session_type: "persistent".into(),
            reflected_at: None,
            title: None,
        });
    }
    Ok(serde_json::json!({ "ok": true }))
}

async fn chat_stop(runtime: &AgentRuntime, params: Option<&Value>) -> AmanResult<Value> {
    let session_id = require_param_str(params, "session_id")?;
    let event = build_event("chat", "stop_generation", serde_json::json!({
        "session_id": session_id,
    }));
    runtime.publish_event(event).await?;
    Ok(serde_json::json!({ "ok": true }))
}

async fn chat_retry(runtime: &AgentRuntime, params: Option<&Value>) -> AmanResult<Value> {
    let session_id = require_param_str(params, "session_id")?;
    let event = build_event("chat", "retry_last", serde_json::json!({
        "session_id": session_id,
    }));
    runtime.publish_event(event).await?;
    Ok(serde_json::json!({ "ok": true }))
}

async fn chat_edit(runtime: &AgentRuntime, params: Option<&Value>) -> AmanResult<Value> {
    let session_id = require_param_str(params, "session_id")?;
    let text = require_param_str(params, "text")?;
    let event = build_event("chat", "edit_message", serde_json::json!({
        "session_id": session_id,
        "text": text,
    }));
    runtime.publish_event(event).await?;
    Ok(serde_json::json!({ "ok": true }))
}

async fn chat_session_delete(runtime: &AgentRuntime, params: Option<&Value>) -> AmanResult<Value> {
    let session_id = require_param_str(params, "session_id")?;
    if let Some(store) = runtime.session_store() {
        let _ = store.delete(&session_id);
    }
    let _ = runtime.workflow_engine().delete_instance(&session_id);
    runtime.audit().record("cli", "chat.session.delete", &session_id, "ok", "");
    Ok(serde_json::json!({ "ok": true }))
}

// -- Soul --

async fn soul_info(runtime: &AgentRuntime) -> AmanResult<Value> {
    if let Some(sr) = runtime.soul_runtime() {
        let current = sr.current_soul();
        Ok(serde_json::json!({
            "name": current.name,
            "identity": current.identity,
        }))
    } else {
        Ok(serde_json::json!({ "name": null }))
    }
}

async fn soul_raw(runtime: &AgentRuntime) -> AmanResult<Value> {
    let content = runtime
        .soul_runtime()
        .map(|sr| sr.current_soul().raw.clone())
        .unwrap_or_default();
    Ok(serde_json::json!({ "content": content }))
}

async fn soul_update(runtime: &AgentRuntime, params: Option<&Value>) -> AmanResult<Value> {
    let content = require_param_str(params, "content")?;
    runtime.update_soul(&content).await?;
    Ok(serde_json::json!({ "ok": true }))
}

// -- Capabilities --

async fn capabilities_list(runtime: &AgentRuntime) -> AmanResult<Value> {
    let caps = runtime.get_capabilities().await;
    let items: Vec<Value> = caps
        .into_iter()
        .map(|c| serde_json::json!({ "capability": c }))
        .collect();
    Ok(serde_json::json!({ "capabilities": items }))
}

// -- Tools --

async fn tool_execute(runtime: &AgentRuntime, params: Option<&Value>) -> AmanResult<Value> {
    let name = require_param_str(params, "name")?;
    let arguments = get_param_value(params, "arguments").unwrap_or(Value::Null);
    let tool = runtime.tools().get(&name).ok_or_else(|| Error::NotFound {
        name: format!("tool: {name}"),
    })?;
    let ctx = kernel::context::ToolContext::default();
    let result = tool.execute(arguments, ctx).await?;
    Ok(serde_json::json!({ "result": result }))
}

async fn tool_auth_respond(runtime: &AgentRuntime, params: Option<&Value>) -> AmanResult<Value> {
    let auth_id = require_param_str(params, "auth_id")?;
    let approved = get_param_bool(params, "approved").unwrap_or(true);
    runtime.auth_registry().resolve(&auth_id, approved);
    Ok(serde_json::json!({ "ok": true }))
}

async fn plugin_auth_respond(runtime: &AgentRuntime, params: Option<&Value>) -> AmanResult<Value> {
    let plugin_name = require_param_str(params, "plugin_name")?;
    let approved = get_param_bool(params, "approved").unwrap_or(true);

    runtime.plugin_approval_registry().resolve(&plugin_name, approved);

    if approved {
        let candidate = runtime.take_pending_plugin_candidate(&plugin_name).await;
        match candidate {
            Some((candidate, approved_caps)) => {
                // Persist approval with BLAKE3 signature
                if let Some(cache) = runtime.approval_cache() {
                    let now_ms: u64 = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_millis() as u64;
                    let mut caps = kernel::security::ApprovedCapabilities {
                        plugin_version: candidate.manifest.version.to_string(),
                        capabilities: approved_caps,
                        approved_at_ms: now_ms,
                        approved_by: "user".to_owned(),
                        signature: String::new(),
                    };
                    cache.save(&plugin_name, &mut caps).map_err(|e| Error::Unrecoverable {
                        message: format!("failed to save approval: {e}"),
                    })?;
                }
                // Load the approved plugin
                let mut loader = runtime.plugin_loader().await;
                loader.load_plugin(candidate).await?;
                Ok(serde_json::json!({ "ok": true, "loaded": true }))
            }
            None => Err(Error::NotFound {
                name: format!("pending plugin approval for '{plugin_name}'"),
            }),
        }
    } else {
        runtime.remove_pending_plugin_candidate(&plugin_name).await;
        Ok(serde_json::json!({ "ok": true, "denied": true }))
    }
}
