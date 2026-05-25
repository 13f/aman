// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

use crate::gateway_client::GatewayClient;
use crate::state::AppState;
use crate::models::{
    ChatMessageEntry, ChatSessionInfo, ChatSessionState,
    DlqEntry, FinanceCardEntry, MetricsSnapshot, PluginEntry,
    PluginHealthEntry, QueueDepth, RuntimeConfigInfo,
    RuntimeStatusInfo, SkillEntry, SoulInfo, WorkflowEntry,
};
use secret::{KeychainBackend, SecretBackend};
use serde::Serialize;
use std::time::Instant;
use tauri::State;

/// Helper to get the gateway client from state, failing with a clear message if disconnected.
async fn require_gateway(state: &State<'_, AppState>) -> Result<GatewayClient, String> {
    let guard = state.gateway_client.lock().await;
    guard
        .clone()
        .ok_or_else(|| "Gateway not connected. Start the gateway daemon first.".to_owned())
}

// ---------------------------------------------------------------------------
// Runtime lifecycle — gateway process management
// ---------------------------------------------------------------------------

/// Parse runtime status JSON into RuntimeStatusInfo.
fn parse_runtime_status(v: &serde_json::Value) -> RuntimeStatusInfo {
    RuntimeStatusInfo {
        phase: v["phase"].as_str().map(|s| format!("Phase{s}")).unwrap_or_else(|| "stopped".to_owned()),
        ready: v["ready"].as_bool().unwrap_or(false),
        live: v["live"].as_bool().unwrap_or(false),
        running: v["phase"].as_u64().map(|p| p > 0).unwrap_or(false),
    }
}

#[tauri::command]
pub async fn get_runtime_status(state: State<'_, AppState>) -> Result<RuntimeStatusInfo, String> {
    let client = require_gateway(&state).await?;
    let v = client.runtime_status().await?;
    Ok(parse_runtime_status(&v))
}

/// Try to connect to an already-running gateway without spawning a new process.
///
/// Reads the configured port from config, pings the health endpoint, and
/// stores the client in app state on success.
#[tauri::command]
pub async fn try_connect_gateway(state: State<'_, AppState>) -> Result<RuntimeStatusInfo, String> {
    // Skip if already connected
    {
        let guard = state.gateway_client.lock().await;
        if guard.is_some() {
            return Err("Already connected to a gateway".to_owned());
        }
    }

    let port = get_gateway_port().await?;
    let base_url = format!("http://127.0.0.1:{port}");
    let client = GatewayClient::new(&base_url);

    client.health().await.map_err(|e| format!("Gateway not reachable at {base_url}: {e}"))?;

    let v = client.runtime_status().await?;
    let status = parse_runtime_status(&v);

    {
        let mut guard = state.gateway_client.lock().await;
        *guard = Some(client);
    }

    Ok(status)
}

#[tauri::command]
pub async fn get_runtime_config(state: State<'_, AppState>) -> Result<RuntimeConfigInfo, String> {
    let client = require_gateway(&state).await?;
    let v = client.runtime_config().await?;
    Ok(RuntimeConfigInfo {
        runtime_dir: v["runtime_dir"].as_str().map(String::from),
        bind_addr: v["bind_addr"].as_str().map(String::from),
        has_api_token: v["api_token_configured"].as_bool().unwrap_or(false),
        risky_enabled: v["risky_capabilities_enabled"].as_bool().unwrap_or(false),
        skills_dir: v["skills_dir"].as_str().map(String::from),
    })
}

/// Find the workspace root by searching upward for a Cargo.toml containing `[workspace]`.
fn find_workspace_root() -> Result<std::path::PathBuf, String> {
    let manifest_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    for ancestor in manifest_dir.ancestors() {
        let candidate = ancestor.join("Cargo.toml");
        if candidate.exists() {
            let content = std::fs::read_to_string(&candidate)
                .map_err(|e| format!("read {candidate:?}: {e}"))?;
            if content.contains("[workspace]") {
                return Ok(ancestor.to_owned());
            }
        }
    }
    Err("Cannot find workspace root (no Cargo.toml with [workspace] found)".to_owned())
}

#[tauri::command]
pub async fn start_runtime(
    state: State<'_, AppState>,
    gateway_url: String,
) -> Result<String, String> {
    // Check not already connected
    {
        let guard = state.gateway_client.lock().await;
        if guard.is_some() {
            return Err("Already connected to a gateway".to_owned());
        }
    }

    let project_root = find_workspace_root()?;

    // Spawn `cargo run --bin gateway` from the workspace root
    let mut child = tokio::process::Command::new("cargo")
        .args(["run", "--bin", "gateway"])
        .current_dir(&project_root)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| format!("Failed to spawn gateway process: {e}"))?;

    // Poll health endpoint until the gateway is ready (up to 120 s)
    let client = GatewayClient::new(&gateway_url);
    let max_retries = 120u32;
    let mut last_err = String::new();

    for _ in 0..max_retries {
        // Detect premature exit (e.g. cargo build failure)
        if let Ok(Some(status)) = child.try_wait() {
            use tokio::io::AsyncReadExt;
            let stderr = match child.stderr.take() {
                Some(mut s) => {
                    let mut buf = String::new();
                    let _ = s.read_to_string(&mut buf).await;
                    buf
                }
                None => String::new(),
            };
            return Err(format!(
                "Gateway process exited with status {status}:\n{stderr}"
            ));
        }

        match client.health().await {
            Ok(()) => {
                let mut client_guard = state.gateway_client.lock().await;
                *client_guard = Some(client);
                let mut proc_guard = state.gateway_process.lock().await;
                *proc_guard = Some(child);
                return Ok(format!("Gateway started at {gateway_url}"));
            }
            Err(e) => {
                last_err = e;
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            }
        }
    }

    // Timeout — kill the process and report failure
    let _ = child.kill().await;
    let _ = child.wait().await;
    Err(format!(
        "Gateway failed to become healthy within {max_retries}s: {last_err}"
    ))
}

#[tauri::command]
pub async fn stop_runtime(state: State<'_, AppState>) -> Result<String, String> {
    // Best-effort call to the gateway's shutdown endpoint
    let base_url = {
        let guard = state.gateway_client.lock().await;
        guard.as_ref().map(|c| c.base_url.clone())
    };

    if let Some(ref url) = base_url {
        let http_client = reqwest::Client::builder()
            .no_proxy()
            .build()
            .map_err(|e| format!("create http client: {e}"))?;
        let _ = http_client
            .post(format!("{url}/agent/shutdown"))
            .send()
            .await;
    }

    // Clear the client from state
    {
        let mut guard = state.gateway_client.lock().await;
        *guard = None;
    }

    // Kill the child process if we own it
    let mut proc_guard = state.gateway_process.lock().await;
    if let Some(mut child) = proc_guard.take() {
        let _ = child.kill().await;
        let _ = child.wait().await;
        Ok("Gateway stopped".to_owned())
    } else {
        Ok("Disconnected from gateway".to_owned())
    }
}

#[tauri::command]
pub async fn get_gateway_port() -> Result<u16, String> {
    let cfg = config::AmanConfig::from_default_path()
        .map_err(|e| format!("load config: {e}"))?;
    Ok(cfg.runtime.gateway.port)
}

// ---------------------------------------------------------------------------
// Metrics
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn get_metrics(state: State<'_, AppState>) -> Result<MetricsSnapshot, String> {
    let client = require_gateway(&state).await?;
    let v = client.debug_metrics().await?;

    let plugin_health = v["plugin_health"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .map(|item| PluginHealthEntry {
                    name: item["name"].as_str().unwrap_or("").to_owned(),
                    status: item["status"].as_str().unwrap_or("").to_owned(),
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    Ok(MetricsSnapshot {
        queue_depth: QueueDepth {
            high: v["queue_depth_high"].as_i64().unwrap_or(0),
            normal: v["queue_depth_normal"].as_i64().unwrap_or(0),
            low: v["queue_depth_low"].as_i64().unwrap_or(0),
        },
        throughput: v["throughput"].as_u64().unwrap_or(0),
        discarded: v["discarded_count"].as_u64().unwrap_or(0),
        duplicate: v["duplicate_count"].as_u64().unwrap_or(0),
        subscription_count: v["subscription_count"].as_i64().unwrap_or(0),
        retry_queue_depth: v["retry_queue_depth"].as_i64().unwrap_or(0),
        dlq_depth: v["dlq_depth"].as_u64().unwrap_or(0) as usize,
        inflight_pipelines: v["inflight_pipelines"].as_u64().unwrap_or(0) as usize,
        inflight_skills: v["inflight_skills"].as_u64().unwrap_or(0) as usize,
        backpressure_level: v["backpressure_level"].as_str().unwrap_or("Normal").to_owned(),
        plugin_health,
    })
}

// ---------------------------------------------------------------------------
// Skills
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn list_skills(state: State<'_, AppState>) -> Result<Vec<SkillEntry>, String> {
    let client = require_gateway(&state).await?;
    let v = client.list_skills().await?;
    let items = v["items"].as_array().map(|arr| {
        arr.iter().map(|item| SkillEntry {
            name: item["name"].as_str().unwrap_or("").to_owned(),
            version: item["version"].as_str().unwrap_or("").to_owned(),
            description: item["description"].as_str().unwrap_or("").to_owned(),
            enabled: item["enabled"].as_bool().unwrap_or(false),
            triggers: vec![],
            concurrency: item["concurrency"].as_str().map(|s| format!("{s:?}")).unwrap_or_default(),
        }).collect::<Vec<_>>()
    }).unwrap_or_default();
    Ok(items)
}

#[tauri::command]
pub async fn list_llm_skills(state: State<'_, AppState>) -> Result<serde_json::Value, String> {
    let client = require_gateway(&state).await?;
    client.list_llm_skills().await
}

#[tauri::command]
pub async fn reload_skills(state: State<'_, AppState>) -> Result<String, String> {
    let client = require_gateway(&state).await?;
    client.reload_skills().await?;
    Ok("Skills reloaded".to_owned())
}

#[tauri::command]
pub async fn enable_skill(state: State<'_, AppState>, name: String) -> Result<String, String> {
    let client = require_gateway(&state).await?;
    client.enable_skill(&name).await?;
    Ok(format!("Skill '{name}' enabled"))
}

#[tauri::command]
pub async fn disable_skill(state: State<'_, AppState>, name: String) -> Result<String, String> {
    let client = require_gateway(&state).await?;
    client.disable_skill(&name).await?;
    Ok(format!("Skill '{name}' disabled"))
}

#[tauri::command]
pub async fn search_skills(
    state: State<'_, AppState>,
    query: String,
    limit: Option<usize>,
) -> Result<serde_json::Value, String> {
    let client = require_gateway(&state).await?;
    client.search_skills(&query, limit.unwrap_or(10)).await
}

#[tauri::command]
pub async fn read_skill_content(
    state: State<'_, AppState>,
    name: String,
) -> Result<serde_json::Value, String> {
    let client = require_gateway(&state).await?;
    client.read_skill_content(&name).await
}

// ---------------------------------------------------------------------------
// Events
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn inject_event(
    state: State<'_, AppState>,
    source: String,
    event_type: String,
    payload: serde_json::Value,
) -> Result<String, String> {
    let client = require_gateway(&state).await?;
    client.inject_event(&source, &event_type, payload).await
}

#[tauri::command]
pub async fn get_debug_events(
    state: State<'_, AppState>,
    limit: Option<usize>,
) -> Result<Vec<serde_json::Value>, String> {
    let client = require_gateway(&state).await?;
    let v = client.recent_events(limit.unwrap_or(50)).await?;
    Ok(v["events"].as_array().cloned().unwrap_or_default())
}

#[tauri::command]
pub async fn get_event_trace(
    state: State<'_, AppState>,
    trace_id: String,
) -> Result<serde_json::Value, String> {
    let client = require_gateway(&state).await?;
    client.event_trace(&trace_id).await
}

// ---------------------------------------------------------------------------
// Workflow
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn get_workflow_instances(
    state: State<'_, AppState>,
) -> Result<Vec<WorkflowEntry>, String> {
    let client = require_gateway(&state).await?;
    let v = client.workflow_instances().await?;
    let items = v["items"].as_array().map(|arr| {
        arr.iter().map(|item| {
            let current_state = item["current_state"].as_str().unwrap_or("");
            let running = item["last_active_state"].is_null();
            WorkflowEntry {
                id: item["id"].as_str().unwrap_or("").to_owned(),
                workflow_name: item["workflow_name"].as_str().unwrap_or("").to_owned(),
                current_state: current_state.to_owned(),
                status: if running { "running".to_owned() } else {
                    item["last_active_state"].as_str().unwrap_or("").to_owned()
                },
            }
        }).collect::<Vec<_>>()
    }).unwrap_or_default();
    Ok(items)
}

#[tauri::command]
pub async fn retry_workflow(state: State<'_, AppState>, id: String) -> Result<String, String> {
    let client = require_gateway(&state).await?;
    client.retry_workflow(&id).await?;
    Ok("Workflow retried".to_owned())
}

#[tauri::command]
pub async fn cancel_workflow(state: State<'_, AppState>, id: String) -> Result<String, String> {
    let client = require_gateway(&state).await?;
    client.cancel_workflow(&id).await?;
    Ok("Workflow cancelled".to_owned())
}

#[tauri::command]
pub async fn get_workflow_def(
    state: State<'_, AppState>,
    name: String,
) -> Result<crate::models::WorkflowDefInfo, String> {
    let client = require_gateway(&state).await?;
    let v = client.workflow_def(&name).await?;

    let states = v["states"].as_array()
        .map(|a| a.iter().filter_map(|s| s.as_str().map(String::from)).collect::<Vec<_>>())
        .unwrap_or_default();
    let transitions = v["transitions"].as_array()
        .map(|a| a.iter().map(|t| crate::models::TransitionInfo {
            from: t["from"].as_str().unwrap_or("").to_owned(),
            event: t["event"].as_str().unwrap_or("").to_owned(),
            to: t["to"].as_str().unwrap_or("").to_owned(),
            guard: t["guard"].as_str().map(String::from),
            has_action: t["has_action"].as_bool().unwrap_or(false),
        }).collect::<Vec<_>>())
        .unwrap_or_default();
    let state_timeouts = v["state_timeouts"].as_array()
        .map(|a| a.iter().map(|st| crate::models::StateTimeoutInfo {
            state: st["state"].as_str().unwrap_or("").to_owned(),
            timeout_ms: st["timeout_ms"].as_u64().unwrap_or(0),
            on_timeout: st["on_timeout"].as_str().unwrap_or("").to_owned(),
        }).collect::<Vec<_>>())
        .unwrap_or_default();

    Ok(crate::models::WorkflowDefInfo {
        name: v["name"].as_str().unwrap_or("").to_owned(),
        states,
        initial_state: v["initial_state"].as_str().unwrap_or("").to_owned(),
        final_states: v["final_states"].as_array()
            .map(|a| a.iter().filter_map(|s| s.as_str().map(String::from)).collect())
            .unwrap_or_default(),
        error_state: v["error_state"].as_str().unwrap_or("").to_owned(),
        transitions,
        state_timeouts,
    })
}

// ---------------------------------------------------------------------------
// SOUL
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn get_soul_info(state: State<'_, AppState>) -> Result<SoulInfo, String> {
    let client = require_gateway(&state).await?;
    let v = client.soul_info().await?;
    Ok(SoulInfo {
        current_soul: v.get("name").and_then(|n| n.as_str()).map(String::from),
        last_changed: v.get("last_changed_at").and_then(|t| t.as_i64().map(|ts| ts.to_string())),
    })
}

#[tauri::command]
pub async fn preview_system_prompt(state: State<'_, AppState>) -> Result<String, String> {
    let client = require_gateway(&state).await?;
    client.soul_system_prompt().await
}

#[tauri::command]
pub async fn update_soul(state: State<'_, AppState>, content: String) -> Result<String, String> {
    let client = require_gateway(&state).await?;
    client.update_soul(&content).await?;
    Ok("SOUL updated".to_owned())
}

#[tauri::command]
pub async fn get_soul_raw(state: State<'_, AppState>) -> Result<String, String> {
    let client = require_gateway(&state).await?;
    client.soul_raw().await
}

// ---------------------------------------------------------------------------
// Plugins
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn list_plugins(state: State<'_, AppState>) -> Result<Vec<PluginEntry>, String> {
    let client = require_gateway(&state).await?;
    let v = client.list_plugins().await?;
    let items = v["items"].as_array().map(|arr| {
        arr.iter().map(|item| PluginEntry {
            name: item["name"].as_str().unwrap_or("").to_owned(),
            version: item["version"].as_str().map(String::from),
            loaded: item["loaded"].as_bool().unwrap_or(false),
            state: item["state"].as_str().map(String::from),
            enabled: !item["unstable"].as_bool().unwrap_or(false),
        }).collect::<Vec<_>>()
    }).unwrap_or_default();
    Ok(items)
}

#[tauri::command]
pub async fn enable_plugin(state: State<'_, AppState>, name: String) -> Result<String, String> {
    let client = require_gateway(&state).await?;
    client.enable_plugin(&name).await?;
    Ok(format!("Plugin {name} enabled"))
}

#[tauri::command]
pub async fn disable_plugin(state: State<'_, AppState>, name: String) -> Result<String, String> {
    let client = require_gateway(&state).await?;
    client.disable_plugin(&name).await?;
    Ok(format!("Plugin {name} disabled"))
}

// ---------------------------------------------------------------------------
// Capabilities
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn get_capabilities(state: State<'_, AppState>) -> Result<Vec<crate::models::CapabilityEntry>, String> {
    let client = require_gateway(&state).await?;
    let v = client.capabilities().await?;

    let mut result: Vec<crate::models::CapabilityEntry> = v.as_object()
        .map(|obj| {
            obj.iter().flat_map(|(_key, entries)| {
                entries.as_array().map(|arr| {
                    arr.iter().map(|e| crate::models::CapabilityEntry {
                        capability: e["capability"].as_str().unwrap_or("").to_owned(),
                        plugin: e["plugin"].as_str().unwrap_or("").to_owned(),
                        version: e["version"].as_str().unwrap_or("").to_owned(),
                        status: e["status"].as_str().unwrap_or("").to_owned(),
                    }).collect::<Vec<_>>()
                }).unwrap_or_default()
            }).collect::<Vec<_>>()
        })
        .unwrap_or_default();
    result.sort_by(|a, b| a.capability.cmp(&b.capability));
    Ok(result)
}

// ---------------------------------------------------------------------------
// DLQ
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn list_dlq(state: State<'_, AppState>) -> Result<Vec<DlqEntry>, String> {
    let client = require_gateway(&state).await?;
    let v = client.list_dlq().await?;
    let items = v["items"].as_array().map(|arr| {
        arr.iter().map(|item| DlqEntry {
            id: item["id"].as_str().unwrap_or("").to_owned(),
            event_source: item["event"]["source"].as_str().unwrap_or("").to_owned(),
            event_type: item["event"]["event_type"].as_str().map(|s| {
                // event_type may be an object with "Custom" variant
                s.to_owned()
            }).unwrap_or_default(),
            reason: item["reason"].as_str().unwrap_or("").to_owned(),
            retry_count: item["retry_count"].as_u64().unwrap_or(0) as u32,
            enqueued_at_ms: item["enqueued_at_ms"].as_i64().unwrap_or(0),
        }).collect::<Vec<_>>()
    }).unwrap_or_default();
    Ok(items)
}

#[tauri::command]
pub async fn retry_dlq(state: State<'_, AppState>, id: String) -> Result<String, String> {
    let client = require_gateway(&state).await?;
    client.retry_dlq(&id).await?;
    Ok("DLQ entry retried".to_owned())
}

#[tauri::command]
pub async fn discard_dlq(state: State<'_, AppState>, id: String) -> Result<String, String> {
    let client = require_gateway(&state).await?;
    client.discard_dlq(&id).await?;
    Ok("DLQ entry discarded".to_owned())
}

// ---------------------------------------------------------------------------
// Chat commands
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn chat_stop_generation(
    state: State<'_, AppState>,
    session_id: String,
) -> Result<String, String> {
    if session_id.trim().is_empty() {
        return Err("Session ID cannot be empty".to_owned());
    }
    let client = require_gateway(&state).await?;
    client.chat_stop_generation(&session_id).await?;
    Ok(session_id)
}

#[tauri::command]
pub async fn chat_send_message(
    state: State<'_, AppState>,
    text: String,
    session_id: String,
    expected_version: Option<u64>,
    #[allow(unused)]
    trace_prev: Option<String>,
) -> Result<String, String> {
    let _start = Instant::now();

    // --- Rate limiting check (user-level: 10 msg / 60s sliding window, §4.5) ---
    if let Err(err) = state.rate_limiter.allow(&session_id) {
        return Err(format!("429:{}", err.message));
    }

    // Validate message length
    let len = text.chars().count();
    if len > 4096 {
        return Err(format!("Message exceeds maximum length of 4096 characters (got {len})"));
    }
    if text.trim().is_empty() {
        return Err("Message cannot be empty".to_owned());
    }
    if session_id.trim().is_empty() {
        return Err("Session ID cannot be empty".to_owned());
    }

    let client = require_gateway(&state).await?;
    client.chat_send_message(&session_id, &text, expected_version).await
}

#[tauri::command]
pub async fn chat_session_list(
    state: State<'_, AppState>,
) -> Result<Vec<ChatSessionInfo>, String> {
    let client = require_gateway(&state).await?;
    let v = client.chat_sessions().await?;
    let items = v["items"].as_array().map(|arr| {
        arr.iter().map(|item| ChatSessionInfo {
            id: item["id"].as_str().unwrap_or("").to_owned(),
            state: item["state"].as_str().unwrap_or("").to_owned(),
            message_count: item["message_count"].as_u64().unwrap_or(0) as usize,
            created_at: item["created_at"].as_i64().unwrap_or(0),
            last_active_at: item["last_active_at"].as_i64(),
            title: item["title"].as_str().map(String::from),
            session_type: item["session_type"].as_str().map(String::from),
            parent_session_id: item["parent_session_id"].as_str().map(String::from),
            branch_message_id: item["branch_message_id"].as_str().map(String::from),
            version: item["version"].as_u64().unwrap_or(0),
        }).collect::<Vec<_>>()
    }).unwrap_or_default();
    Ok(items)
}

/// Read sessions directly from the local SQLite DB, bypassing the gateway.
///
/// Falls back to `chat_session_list` (gateway API) when the DB doesn't
/// exist or can't be opened — for example when the gateway was never
/// started and no sessions have been persisted yet.
#[tauri::command]
pub async fn chat_session_list_db(
    agent_key: Option<String>,
) -> Result<Vec<ChatSessionInfo>, String> {
    let home = std::env::var("HOME").map_err(|_| "HOME not set".to_owned())?;
    // Use the provided agent key, or fall back to the first agent in config.
    let agent_key = match agent_key {
        Some(k) => k,
        None => {
            let aman_cfg = config::AmanConfig::from_default_path()
                .map_err(|e| format!("load config: {e}"))?;
            aman_cfg.agents.keys().next()
                .ok_or_else(|| "no agents configured".to_owned())?
                .clone()
        }
    };
    let agent_dir = std::path::PathBuf::from(&home)
        .join(".aman")
        .join("agents")
        .join(&agent_key);
    let db_path = agent_dir.join("sessions.db");
    let agents_root = agent_dir.parent().map(|p| p.to_path_buf()).unwrap_or_else(|| agent_dir.clone());

    // Helper: count lines in a session's JSONL file, searching across ALL
    // agent directories (the gateway writes to the first agent's dir).
    fn jsonl_message_count(agents_root: &std::path::Path, session_id: &str) -> usize {
        let entries = match std::fs::read_dir(agents_root) {
            Ok(e) => e,
            Err(_) => return 0,
        };
        for entry in entries.flatten() {
            if !entry.file_type().is_ok_and(|t| t.is_dir()) {
                continue;
            }
            let path = entry.path().join("sessions").join(format!("{session_id}.jsonl"));
            if path.exists()
                && let Ok(s) = std::fs::read_to_string(&path) {
                    let count = s.lines().count();
                    if count > 0 {
                        return count;
                    }
                }
        }
        0
    }

    /// Extract a short title from the first user message in a session JSONL,
    /// searching across all agent directories.
    fn jsonl_session_title(agents_root: &std::path::Path, session_id: &str) -> Option<String> {
        let entries = std::fs::read_dir(agents_root).ok()?;
        for entry in entries.flatten() {
            if !entry.file_type().is_ok_and(|t| t.is_dir()) {
                continue;
            }
            let path = entry.path().join("sessions").join(format!("{session_id}.jsonl"));
            let content = std::fs::read_to_string(&path).ok()?;
            for line in content.lines() {
                if let Ok(evt) = serde_json::from_str::<serde_json::Value>(line) {
                    let et = evt["event_type"].as_str().unwrap_or("");
                    if et.contains("MessageReceived") {
                        let text = evt["payload"]["text"].as_str().unwrap_or("").trim().to_string();
                        if !text.is_empty() {
                            if text.chars().count() <= 40 {
                                return Some(text);
                            }
                            let truncated: String = text.chars().take(40).collect();
                            return Some(format!("{truncated}…"));
                        }
                    }
                }
            }
        }
        None
    }

    let db = rusqlite::Connection::open(&db_path)
        .map_err(|e| format!("open sessions.db: {e}"))?;

    let mut stmt = db.prepare(
        "SELECT id, state, message_count, created_at, last_active_at, session_type
         FROM sessions ORDER BY last_active_at DESC",
    )
    .map_err(|e| format!("query sessions.db: {e}"))?;

    let rows = stmt.query_map([], |row| {
        Ok(ChatSessionInfo {
            id: row.get(0)?,
            state: row.get(1)?,
            message_count: row.get::<_, i64>(2)? as usize,
            created_at: row.get(3)?,
            last_active_at: Some(row.get(4)?),
            title: None,
            session_type: row.get(5)?,
            parent_session_id: None,
            branch_message_id: None,
            version: 0,
        })
    })
    .map_err(|e| format!("read sessions.db: {e}"))?;

    let mut items = Vec::new();
    for row in rows {
        let mut item = row.map_err(|e| format!("session row: {e}"))?;
        // The DB message_count may be stale (only the first agent's DB is
        // updated by the gateway).  Use the JSONL line count as the
        // authoritative count for this per-agent session list.
        let jsonl_count = jsonl_message_count(&agents_root, &item.id);
        if jsonl_count > 0 {
            item.message_count = jsonl_count;
        }
        // Extract a title from the first user message in the JSONL.
        item.title = jsonl_session_title(&agents_root, &item.id);
        items.push(item);
    }
    Ok(items)
}

#[tauri::command]
pub async fn chat_session_create(
    state: State<'_, AppState>,
    session_type: Option<String>,
) -> Result<String, String> {
    let client = require_gateway(&state).await?;
    client.chat_session_create(session_type.as_deref()).await
}

#[tauri::command]
pub async fn explore_start(
    state: State<'_, AppState>,
    agent_key: Option<String>,
) -> Result<serde_json::Value, String> {
    let client = require_gateway(&state).await?;
    client.explore_start(agent_key.as_deref()).await
}

/// Create a branch session forked from a specific message in an existing session.
#[tauri::command]
pub async fn chat_session_branch(
    state: State<'_, AppState>,
    session_id: String,
    message_id: String,
    session_type: Option<String>,
) -> Result<String, String> {
    if session_id.trim().is_empty() {
        return Err("Session ID cannot be empty".to_owned());
    }
    if message_id.trim().is_empty() {
        return Err("Message ID cannot be empty".to_owned());
    }

    let client = require_gateway(&state).await?;
    client.chat_session_create_branch(&session_id, &message_id, session_type.as_deref()).await
}

#[tauri::command]
pub async fn chat_session_close(
    state: State<'_, AppState>,
    session_id: String,
    _expected_version: Option<u64>,
) -> Result<String, String> {
    if session_id.trim().is_empty() {
        return Err("Session ID cannot be empty".to_owned());
    }

    let client = require_gateway(&state).await?;
    client.chat_close_session(&session_id).await?;
    Ok(session_id)
}

#[tauri::command]
pub async fn chat_session_delete(
    state: State<'_, AppState>,
    session_id: String,
) -> Result<(), String> {
    if session_id.trim().is_empty() {
        return Err("Session ID cannot be empty".to_owned());
    }
    let client = require_gateway(&state).await?;
    client.chat_delete_session(&session_id).await
}

#[tauri::command]
pub async fn chat_session_history(
    state: State<'_, AppState>,
    session_id: String,
    limit: Option<usize>,
) -> Result<Vec<ChatMessageEntry>, String> {
    if session_id.trim().is_empty() {
        return Err("Session ID cannot be empty".to_owned());
    }

    let client = require_gateway(&state).await?;
    let v = client.chat_session_history(&session_id, limit).await?;
    let messages = v["messages"].as_array().map(|arr| {
        arr.iter().map(|e| ChatMessageEntry {
            id: e["event_id"].as_str().unwrap_or("").to_owned(),
            event_type: e["event_type"].as_str().unwrap_or("").to_owned(),
            payload: e["payload"].clone(),
            timestamp: e["timestamp_ms"].as_i64().unwrap_or(0),
            trace_id: String::new(),
        }).collect::<Vec<_>>()
    }).unwrap_or_default();
    Ok(messages)
}

#[tauri::command]
pub async fn chat_session_state(
    state: State<'_, AppState>,
    session_id: String,
) -> Result<ChatSessionState, String> {
    if session_id.trim().is_empty() {
        return Err("Session ID cannot be empty".to_owned());
    }

    let client = require_gateway(&state).await?;
    let v = client.chat_session_state(&session_id).await?;

    let messages = v["messages"].as_array().map(|arr| {
        arr.iter().map(|e| ChatMessageEntry {
            id: e["event_id"].as_str().unwrap_or("").to_owned(),
            event_type: e["event_type"].as_str().unwrap_or("").to_owned(),
            payload: e["payload"].clone(),
            timestamp: e["timestamp_ms"].as_i64().unwrap_or(0),
            trace_id: String::new(),
        }).collect::<Vec<_>>()
    }).unwrap_or_default();

    Ok(ChatSessionState {
        session_id: v["id"].as_str().unwrap_or(&session_id).to_owned(),
        state: v["state"].as_str().unwrap_or("").to_owned(),
        state_version: messages.len() as u64,
        retry_count: 0,
        messages,
        session_type: v["session_type"].as_str().unwrap_or("persistent").to_owned(),
        version: v["version"].as_u64().unwrap_or(0),
    })
}

/// Read session state directly from the per-agent sessions directory,
/// bypassing the gateway's single-agent session store.
#[tauri::command]
pub async fn chat_session_state_local(
    agent_key: String,
    session_id: String,
) -> Result<ChatSessionState, String> {
    if session_id.trim().is_empty() {
        return Err("Session ID cannot be empty".to_owned());
    }
    let home = std::env::var("HOME").map_err(|_| "HOME not set".to_owned())?;
    let sessions_dir = std::path::PathBuf::from(&home)
        .join(".aman")
        .join("agents")
        .join(&agent_key)
        .join("sessions");
    let jsonl_path = sessions_dir.join(format!("{session_id}.jsonl"));

    let messages: Vec<ChatMessageEntry> = if jsonl_path.exists() {
        let content = std::fs::read_to_string(&jsonl_path)
            .map_err(|e| format!("read JSONL: {e}"))?;
        content
            .lines()
            .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
            .map(|e| ChatMessageEntry {
                id: e["event_id"].as_str().unwrap_or("").to_owned(),
                event_type: e["event_type"].as_str().unwrap_or("").to_owned(),
                payload: e["payload"].clone(),
                timestamp: e["timestamp_ms"].as_i64().unwrap_or(0),
                trace_id: String::new(),
            })
            .collect()
    } else {
        Vec::new()
    };

    let msg_count = messages.len() as u64;
    Ok(ChatSessionState {
        session_id,
        state: "closed".to_owned(),
        state_version: msg_count,
        retry_count: 0,
        messages,
        session_type: "persistent".to_owned(),
        version: msg_count,
    })
}

#[tauri::command]
pub async fn chat_retry_last(
    state: State<'_, AppState>,
    session_id: String,
    _expected_version: Option<u64>,
) -> Result<String, String> {
    if session_id.trim().is_empty() {
        return Err("Session ID cannot be empty".to_owned());
    }

    let client = require_gateway(&state).await?;
    client.chat_retry(&session_id).await?;
    Ok(session_id)
}

#[tauri::command]
pub async fn chat_edit_message(
    state: State<'_, AppState>,
    session_id: String,
    message_id: String,
    text: String,
    expected_version: Option<u64>,
) -> Result<String, String> {
    if session_id.trim().is_empty() {
        return Err("Session ID cannot be empty".to_owned());
    }
    if text.trim().is_empty() {
        return Err("Edited message cannot be empty".to_owned());
    }

    let client = require_gateway(&state).await?;
    client.chat_edit_message(&session_id, &message_id, &text, expected_version).await?;
    Ok(message_id)
}

#[tauri::command]
pub async fn chat_trace_chain(
    state: State<'_, AppState>,
    trace_id: String,
) -> Result<serde_json::Value, String> {
    if trace_id.trim().is_empty() {
        return Err("Trace ID cannot be empty".to_owned());
    }
    let client = require_gateway(&state).await?;
    client.event_trace(&trace_id).await
}

// ---------------------------------------------------------------------------
// Tool authorization (native macOS dialog)
// ---------------------------------------------------------------------------

/// Show a native macOS dialog for tool authorization and POST the user's
/// decision to the gateway's `/tool-auth/respond` endpoint.
///
/// This runs as a native shell command (`osascript`) outside the webview, so
/// it cannot be bypassed by AI-driven UI manipulation.
#[tauri::command]
pub async fn show_tool_auth_dialog(
    state: State<'_, AppState>,
    auth_id: String,
    tool_name: String,
    arguments_summary: String,
) -> Result<String, String> {
    let base_url = {
        let guard = state.gateway_client.lock().await;
        guard
            .as_ref()
            .map(|c| c.base_url.clone())
            .ok_or_else(|| "Gateway not connected".to_owned())?
    };

    let dialog_result = show_native_auth_dialog(&tool_name, &arguments_summary)?;

    let approved = dialog_result == "allow";
    let client = reqwest::Client::builder()
        .no_proxy()
        .build()
        .map_err(|e| format!("Failed to create HTTP client: {e}"))?;

    let resp = client
        .post(format!("{base_url}/tool-auth/respond"))
        .json(&serde_json::json!({
            "auth_id": auth_id,
            "approved": approved,
        }))
        .send()
        .await
        .map_err(|e| format!("Failed to send auth response: {e}"))?;

    if resp.status().is_success() {
        Ok(dialog_result)
    } else {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        Err(format!("Auth respond failed ({status}): {body}"))
    }
}

/// Run a native macOS dialog via `osascript` and return "allow" or "deny".
fn show_native_auth_dialog(tool_name: &str, arguments_summary: &str) -> Result<String, String> {
    // Escape for AppleScript string literals: backslash, quote, and newlines.
    let escaped_tool = tool_name.replace('\\', "\\\\").replace('"', "\\\"");
    let escaped_args = arguments_summary
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', " ");

    let script = format!(
        r#"display dialog "Tool "{escaped_tool}" wants to execute with the following arguments:

{escaped_args}

Allow this operation?" buttons {{"Deny", "Allow"}} default button "Allow" cancel button "Deny" with title "aman — Tool Authorization" with icon caution"#
    );

    let output = std::process::Command::new("osascript")
        .arg("-e")
        .arg(&script)
        .output()
        .map_err(|e| format!("Failed to run osascript: {e}"))?;

    if output.status.success() {
        let stdout = String::from_utf8_lossy(&output.stdout);
        if stdout.contains("Allow") || stdout.contains("button returned:Allow") {
            Ok("allow".to_owned())
        } else {
            Ok("deny".to_owned())
        }
    } else {
        // User pressed cancel or Esc
        Ok("deny".to_owned())
    }
}

// ---------------------------------------------------------------------------
// Third-party service key management
// ---------------------------------------------------------------------------

static THIRD_PARTY_SERVICES: &[(&str, &str, bool, &[&str])] = &[
    ("tavily", "Tavily Search API", true, &["search"]),
    ("brave", "Brave Search API", true, &["search"]),
    ("duckduckgo", "DuckDuckGo Instant Answer", false, &["search"]),
    ("google", "Google Custom Search", true, &["search"]),
    ("x", "X (Twitter) API v2", true, &["search"]),
];

#[derive(Serialize)]
pub struct ThirdPartyService {
    pub id: String,
    pub display_name: String,
    pub requires_key: bool,
    pub has_key: bool,
    pub has_cx: bool,
    pub tags: Vec<String>,
    pub disabled: bool,
}

#[tauri::command]
pub async fn list_third_party_services() -> Result<Vec<ThirdPartyService>, String> {
    let backend = KeychainBackend;
    let mut services: Vec<ThirdPartyService> = Vec::new();
    for (id, display_name, requires_key, tags) in THIRD_PARTY_SERVICES {
        let api_key = format!("aman.3rd.{id}.api_key");
        let cx = format!("aman.3rd.{id}.cx");
        services.push(ThirdPartyService {
            id: id.to_string(),
            display_name: display_name.to_string(),
            requires_key: *requires_key,
            has_key: backend.get(&api_key).ok().flatten().is_some(),
            has_cx: backend.get(&cx).ok().flatten().is_some(),
            tags: tags.iter().map(|t| t.to_string()).collect(),
            disabled: false,
        });
    }
    Ok(services)
}

#[tauri::command]
pub async fn set_third_party_key(service: String, api_key: String) -> Result<String, String> {
    let backend = KeychainBackend;
    let key = format!("aman.3rd.{service}.api_key");
    backend
        .set(&key, &api_key)
        .map_err(|e| format!("Failed to save to Keychain: {e}"))?;
    Ok(format!("{service} API key saved to Keychain"))
}

#[tauri::command]
pub async fn set_third_party_config(
    service: String,
    sub_key: String,
    value: String,
) -> Result<String, String> {
    let backend = KeychainBackend;
    let key = format!("aman.3rd.{service}.{sub_key}");
    backend
        .set(&key, &value)
        .map_err(|e| format!("Failed to save to Keychain: {e}"))?;
    Ok(format!("{service}.{sub_key} saved to Keychain"))
}

// ---------------------------------------------------------------------------
// Provider management (multi-agent P2) — no runtime required (LOCAL)
// ---------------------------------------------------------------------------

/// Normalize a provider key into an env-var-compatible uppercase identifier.
fn provider_env_key(key: &str) -> String {
    key.to_ascii_uppercase()
        .chars()
        .map(|c| if c.is_alphanumeric() || c == '_' { c } else { '_' })
        .collect()
}

/// Check whether a provider's API key is available.
/// Checks Keychain first, then env var as fallback.
fn provider_has_api_key(key: &str) -> bool {
    // Keychain is the primary store
    let backend = KeychainBackend;
    if let Ok(Some(_)) = backend.get(&format!("aman.providers.{key}.api_key")) {
        return true;
    }
    // Env var fallback (runtime override)
    let env_var = format!("AMAN_PROVIDER_{}_API_KEY", provider_env_key(key));
    std::env::var(env_var).is_ok()
}

#[tauri::command]
pub async fn list_providers() -> Result<Vec<crate::models::ProviderEntry>, String> {
    let config = config::AmanConfig::from_default_path()
        .map_err(|e| format!("读取配置失败: {e}"))?;

    let mut entries: Vec<crate::models::ProviderEntry> = config
        .providers
        .into_iter()
        .map(|(key, p)| {
            let has_key = provider_has_api_key(&key);
            crate::models::ProviderEntry {
                key,
                display_name: p.display_name,
                base_url: p.base_url,
                has_api_key: has_key,
            }
        })
        .collect();
    entries.sort_by(|a, b| a.key.cmp(&b.key));
    Ok(entries)
}

#[tauri::command]
pub async fn create_provider(
    key: String,
    display_name: String,
    base_url: String,
) -> Result<String, String> {
    if !config::is_valid_identifier(&key) {
        return Err("Provider key 只能包含英文字母、数字、下划线、短横线".to_owned());
    }

    let mut aman_config = config::AmanConfig::from_default_path()
        .map_err(|e| format!("读取配置失败: {e}"))?;

    if aman_config.providers.contains_key(&key) {
        return Err(format!("Provider '{key}' 已存在"));
    }

    aman_config.providers.insert(
        key.clone(),
        config::ProviderConfig {
            display_name,
            base_url,
            api_type: Some("openai".to_owned()),
            api_key: None,
            models: Vec::new(),
        },
    );

    let path = default_config_path();
    aman_config.save(&path).map_err(|e| format!("保存配置失败: {e}"))?;

    Ok(format!("Provider '{key}' 已创建"))
}

#[tauri::command]
pub async fn update_provider(
    key: String,
    display_name: Option<String>,
    base_url: Option<String>,
) -> Result<String, String> {
    let mut aman_config = config::AmanConfig::from_default_path()
        .map_err(|e| format!("读取配置失败: {e}"))?;

    let provider = aman_config
        .providers
        .get_mut(&key)
        .ok_or_else(|| format!("Provider '{key}' 不存在"))?;

    if let Some(name) = display_name {
        provider.display_name = name;
    }
    if let Some(url) = base_url {
        provider.base_url = url;
    }

    let path = default_config_path();
    aman_config.save(&path).map_err(|e| format!("保存配置失败: {e}"))?;

    Ok(format!("Provider '{key}' 已更新"))
}

#[tauri::command]
pub async fn delete_provider(key: String) -> Result<String, String> {
    let mut aman_config = config::AmanConfig::from_default_path()
        .map_err(|e| format!("读取配置失败: {e}"))?;

    if !aman_config.providers.contains_key(&key) {
        return Err(format!("Provider '{key}' 不存在"));
    }

    // Check that no agent references this provider.
    let agents_using: Vec<String> = aman_config
        .agents
        .iter()
        .filter(|(_, a)| a.provider == key)
        .map(|(k, _)| k.clone())
        .collect();
    if !agents_using.is_empty() {
        return Err(format!(
            "Provider '{key}' 被以下 Agent 引用，无法删除: {}",
            agents_using.join(", "),
        ));
    }

    aman_config.providers.remove(&key);

    let path = default_config_path();
    aman_config.save(&path).map_err(|e| format!("保存配置失败: {e}"))?;

    Ok(format!("Provider '{key}' 已删除"))
}

#[tauri::command]
pub async fn set_provider_api_key(key: String, api_key: String) -> Result<String, String> {
    // Validate provider exists
    let aman_config = config::AmanConfig::from_default_path()
        .map_err(|e| format!("读取配置失败: {e}"))?;
    if !aman_config.providers.contains_key(&key) {
        return Err(format!("Provider '{key}' 不存在"));
    }

    let backend = KeychainBackend;
    backend
        .set(&format!("aman.providers.{key}.api_key"), &api_key)
        .map_err(|e| format!("保存到 Keychain 失败: {e}"))?;

    Ok(format!("Provider '{key}' API Key 已保存到 macOS Keychain"))
}

#[tauri::command]
pub async fn has_provider_api_key(key: String) -> Result<bool, String> {
    // Validate provider exists
    let aman_config = config::AmanConfig::from_default_path()
        .map_err(|e| format!("读取配置失败: {e}"))?;
    if !aman_config.providers.contains_key(&key) {
        return Err(format!("Provider '{key}' 不存在"));
    }

    Ok(provider_has_api_key(&key))
}


// ---------------------------------------------------------------------------
// Agent management (multi-agent P2) — LOCAL filesystem operations
// ---------------------------------------------------------------------------

/// Scan `~/.aman/agents/` for subdirectories containing `SOUL.md` that are
/// not yet in config.yaml, and auto-register them with empty provider (disabled).
fn sync_filesystem_agents_to_config() -> Result<(), String> {
    let agents_dir = crate::agent_fs::agents_dir();
    if !agents_dir.exists() {
        return Ok(());
    }

    let config_path = default_config_path();
    let mut aman_config = config::AmanConfig::from_default_path()
        .map_err(|e| format!("读取配置失败: {e}"))?;

    let entries = std::fs::read_dir(&agents_dir).map_err(|e| format!("读取agents目录失败: {e}"))?;
    let mut changed = false;

    for entry in entries.flatten() {
        if !entry.file_type().is_ok_and(|t| t.is_dir()) {
            continue;
        }
        let key = entry.file_name().to_string_lossy().to_string();
        if !config::is_valid_identifier(&key) {
            continue;
        }
        if aman_config.agents.contains_key(&key) {
            continue;
        }
        if !entry.path().join("SOUL.md").exists() {
            continue;
        }

        let display_name = crate::agent_fs::soul_summary(&key)
            .lines()
            .next()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
            .unwrap_or_else(|| key.clone());

        aman_config.agents.insert(
            key.clone(),
            config::AgentEntryConfig {
                display_name,
                provider: String::new(),
                model: String::new(),
                system_prompt_override: None,
                enabled: false,
                tools: None,
                skills: None,
                event_bus: None,
            },
        );
        changed = true;
    }

    if changed {
        aman_config.save(&config_path).map_err(|e| format!("保存配置失败: {e}"))?;
    }
    Ok(())
}

#[tauri::command]
pub async fn list_agents(
    state: State<'_, AppState>,
) -> Result<Vec<crate::models::AgentEntry>, String> {
    // Discover any agents manually copied into ~/.aman/agents/ before listing.
    sync_filesystem_agents_to_config()?;

    let aman_config = config::AmanConfig::from_default_path()
        .map_err(|e| format!("读取配置失败: {e}"))?;

    let active_key = state.active_agent_key.lock().await.clone();

    let mut entries: Vec<crate::models::AgentEntry> = aman_config
        .agents
        .into_iter()
        .map(|(key, agent)| {
            let summary = crate::agent_fs::soul_summary(&key);
            // Count session directories on disk (approximate, P4 will use sessions.db).
            let agent_dir = crate::agent_fs::agents_dir().join(&key).join("sessions");
            let session_count = if agent_dir.exists() { {
                let mut count = 0u64;
                if let Ok(entries) = std::fs::read_dir(&agent_dir) {
                    for entry in entries.flatten() {
                        if entry.file_type().is_ok_and(|t| t.is_dir()) {
                            count += 1;
                        }
                    }
                }
                count
            } } else { 0 };

            let is_active = active_key.as_deref() == Some(&key);
            crate::models::AgentEntry {
                key,
                display_name: agent.display_name,
                provider: agent.provider,
                model: agent.model,
                soul_summary: summary,
                session_count,
                is_active,
            }
        })
        .collect();
    entries.sort_by(|a, b| a.key.cmp(&b.key));
    Ok(entries)
}

#[tauri::command]
pub async fn create_agent(
    state: State<'_, AppState>,
    key: String,
    display_name: String,
    provider: String,
    model: String,
    soul_content: String,
) -> Result<String, String> {
    if !config::is_valid_identifier(&key) {
        return Err("Agent key 只能包含英文字母、数字、下划线、短横线".to_owned());
    }

    // Validate provider exists.
    let mut aman_config = config::AmanConfig::from_default_path()
        .map_err(|e| format!("读取配置失败: {e}"))?;
    if !aman_config.providers.contains_key(&provider) {
        return Err(format!("Provider '{provider}' 不存在，请先创建 Provider"));
    }
    if aman_config.agents.contains_key(&key) {
        return Err(format!("Agent '{key}' 已存在"));
    }

    // Create filesystem structure with {name} substituted.
    let soul_content = soul_content.replace("{name}", &display_name);
    crate::agent_fs::init_agent_dir(&key, &soul_content)?;

    // Update config.
    aman_config.agents.insert(
        key.clone(),
        config::AgentEntryConfig {
            display_name,
            provider,
            model,
            system_prompt_override: None,
            enabled: true,
            tools: None,
            skills: None,
            event_bus: None,
        },
    );

    let path = default_config_path();
    aman_config.save(&path).map_err(|e| format!("保存配置失败: {e}"))?;

    // Notify the gateway runtime to reload this agent so the idle system picks it up.
    if let Ok(client) = require_gateway(&state).await {
        let _ = client.reload_agent(&key).await;
    }

    Ok(format!("Agent '{key}' 已创建"))
}

#[tauri::command]
pub async fn update_agent(
    state: State<'_, AppState>,
    key: String,
    display_name: Option<String>,
    provider: Option<String>,
    model: Option<String>,
    soul_content: Option<String>,
    system_prompt_override: Option<Option<String>>,
) -> Result<String, String> {
    let mut aman_config = config::AmanConfig::from_default_path()
        .map_err(|e| format!("读取配置失败: {e}"))?;

    let agent = aman_config
        .agents
        .get_mut(&key)
        .ok_or_else(|| format!("Agent '{key}' 不存在"))?;

    if let Some(name) = display_name {
        agent.display_name = name;
    }
    if let Some(p) = provider {
        if !aman_config.providers.contains_key(&p) {
            return Err(format!("Provider '{p}' 不存在"));
        }
        // Auto-enable the agent when a provider is first configured.
        let was_unconfigured = agent.provider.is_empty();
        agent.provider = p;
        if was_unconfigured {
            agent.enabled = true;
        }
    }
    if let Some(m) = model {
        agent.model = m;
    }
    if let Some(override_val) = system_prompt_override {
        agent.system_prompt_override = override_val;
    }

    let path = default_config_path();
    aman_config.save(&path).map_err(|e| format!("保存配置失败: {e}"))?;

    // Notify the gateway runtime to reload this agent so the idle system picks it up.
    if let Ok(client) = require_gateway(&state).await {
        let _ = client.reload_agent(&key).await;
    }

    // Write soul content separately if provided.
    if let Some(content) = soul_content {
        crate::agent_fs::write_soul(&key, &content)?;
    }

    Ok(format!("Agent '{key}' 已更新"))
}

#[tauri::command]
pub async fn delete_agent(
    key: String,
) -> Result<String, String> {
    let mut aman_config = config::AmanConfig::from_default_path()
        .map_err(|e| format!("读取配置失败: {e}"))?;

    if !aman_config.agents.contains_key(&key) {
        return Err(format!("Agent '{key}' 不存在"));
    }

    // Remove from config.
    aman_config.agents.remove(&key);

    let path = default_config_path();
    aman_config.save(&path).map_err(|e| format!("保存配置失败: {e}"))?;

    // Remove filesystem directory.
    let _ = crate::agent_fs::remove_agent_dir(&key);

    Ok(format!("Agent '{key}' 已删除"))
}

#[tauri::command]
pub async fn get_agent_soul(key: String) -> Result<String, String> {
    crate::agent_fs::read_soul(&key)
}

#[tauri::command]
pub async fn select_agent(
    state: State<'_, AppState>,
    key: String,
) -> Result<String, String> {
    // Validate agent exists in config.
    let aman_config = config::AmanConfig::from_default_path()
        .map_err(|e| format!("读取配置失败: {e}"))?;
    if !aman_config.agents.contains_key(&key) {
        return Err(format!("Agent '{key}' 不存在"));
    }

    // Block selection of agents without a configured provider.
    if let Some(entry) = aman_config.agents.get(&key)
        && entry.provider.is_empty() {
            return Err(format!(
                "Agent '{key}' 尚未配置 Provider，请先在 Agents 页面配置。"
            ));
        }

    let mut active = state.active_agent_key.lock().await;
    *active = Some(key.clone());
    Ok(format!("Agent '{key}' 已激活"))
}

#[tauri::command]
pub async fn get_active_agent(
    state: State<'_, AppState>,
) -> Result<Option<crate::models::AgentEntry>, String> {
    let active_key = state.active_agent_key.lock().await.clone();
    let Some(key) = active_key else {
        return Ok(None);
    };

    let aman_config = config::AmanConfig::from_default_path()
        .map_err(|e| format!("读取配置失败: {e}"))?;

    match aman_config.agents.get(&key) {
        Some(agent) => {
            let summary = crate::agent_fs::soul_summary(&key);
            Ok(Some(crate::models::AgentEntry {
                key,
                display_name: agent.display_name.clone(),
                provider: agent.provider.clone(),
                model: agent.model.clone(),
                soul_summary: summary,
                session_count: 0,
                is_active: true,
            }))
        }
        None => {
            // Config may have been removed while agent was selected.
            let mut active = state.active_agent_key.lock().await;
            *active = None;
            Ok(None)
        }
    }
}

// ---------------------------------------------------------------------------
// Config/status queries (multi-agent P2) — LOCAL
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn get_aman_config() -> Result<config::AmanConfig, String> {
    config::AmanConfig::from_default_path()
        .map_err(|e| format!("读取配置失败: {e}"))
}

#[tauri::command]
pub async fn has_any_provider() -> Result<bool, String> {
    let config = config::AmanConfig::from_default_path()
        .map_err(|e| format!("读取配置失败: {e}"))?;
    Ok(!config.providers.is_empty())
}

#[tauri::command]
pub async fn has_any_agent() -> Result<bool, String> {
    let config = config::AmanConfig::from_default_path()
        .map_err(|e| format!("读取配置失败: {e}"))?;
    Ok(!config.agents.is_empty())
}

#[tauri::command]
pub async fn get_default_model() -> Result<Option<config::DefaultModelConfig>, String> {
    let config = config::AmanConfig::from_default_path()
        .map_err(|e| format!("读取配置失败: {e}"))?;
    Ok(config.model)
}

// ── Notifications ────────────────────────────────────────────────────

#[tauri::command]
pub async fn get_notifications(state: State<'_, AppState>, active_only: bool, severity: Option<String>) -> Result<Vec<crate::models::NotificationEntry>, String> {
    let client = require_gateway(&state).await?;
    let v = client.notifications(active_only, severity.as_deref(), 100).await?;
    let items: Vec<crate::models::NotificationEntry> = serde_json::from_value(v).map_err(|e| format!("notifications decode: {e}"))?;
    Ok(items)
}

#[tauri::command]
pub async fn get_notifications_unread_count(state: State<'_, AppState>) -> Result<crate::models::UnreadCount, String> {
    let client = require_gateway(&state).await?;
    let count = client.notifications_unread_count().await?;
    Ok(crate::models::UnreadCount { count: count as usize })
}

#[tauri::command]
pub async fn notification_dismiss(state: State<'_, AppState>, id: String) -> Result<(), String> {
    let client = require_gateway(&state).await?;
    client.notification_dismiss(&id).await
}

#[tauri::command]
pub async fn notification_ack(state: State<'_, AppState>, id: String) -> Result<(), String> {
    let client = require_gateway(&state).await?;
    client.notification_ack(&id).await
}

#[tauri::command]
pub async fn notification_dismiss_all(state: State<'_, AppState>) -> Result<(), String> {
    let client = require_gateway(&state).await?;
    client.notification_dismiss_all().await
}

// ── Agent runtime (T1.4) — gateway RPC ──────────────────────────────

#[tauri::command]
pub async fn list_runtime_agents(
    state: State<'_, AppState>,
) -> Result<Vec<crate::models::AgentInstanceInfo>, String> {
    let client = require_gateway(&state).await?;
    let v = client.list_agents().await?;
    let agents: Vec<crate::models::AgentInstanceInfo> = v
        .as_array()
        .ok_or_else(|| "invalid agents response".to_owned())?
        .iter()
        .map(|item| crate::models::AgentInstanceInfo {
            agent_id: item["agent_id"].as_str().unwrap_or("").to_owned(),
            display_name: item["display_name"].as_str().unwrap_or("").to_owned(),
            provider: item["provider"].as_str().unwrap_or("").to_owned(),
            model: item["model"].as_str().unwrap_or("").to_owned(),
            status: item["status"].as_str().unwrap_or("").to_owned(),
            enabled: item["descriptor"]["enabled"].as_bool().unwrap_or(false),
            active_session_id: item["active_session_id"].as_str().map(String::from),
        })
        .collect();
    Ok(agents)
}

#[tauri::command]
pub async fn get_runtime_agent(
    state: State<'_, AppState>,
    agent_id: String,
) -> Result<crate::models::AgentInstanceInfo, String> {
    let client = require_gateway(&state).await?;
    let v = client.get_agent(&agent_id).await?;
    Ok(crate::models::AgentInstanceInfo {
        agent_id: v["agent_id"].as_str().unwrap_or("").to_owned(),
        display_name: v["display_name"].as_str().unwrap_or("").to_owned(),
        provider: v["provider"].as_str().unwrap_or("").to_owned(),
        model: v["model"].as_str().unwrap_or("").to_owned(),
        status: v["status"].as_str().unwrap_or("").to_owned(),
        enabled: v["descriptor"]["enabled"].as_bool().unwrap_or(false),
        active_session_id: v["active_session_id"].as_str().map(String::from),
    })
}

#[tauri::command]
pub async fn set_runtime_agent_status(
    state: State<'_, AppState>,
    agent_id: String,
    status: String,
) -> Result<(), String> {
    let client = require_gateway(&state).await?;
    client.set_agent_status(&agent_id, &status).await
}

// ---------------------------------------------------------------------------
// Code Agents — external CLI tools (Claude Code, Codex, OpenCode, Gemini)
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn list_code_agents() -> Result<Vec<crate::models::CodeAgentEntry>, String> {
    crate::code_agents::load_code_agents()
}

#[tauri::command]
pub async fn launch_code_agent(command: String) -> Result<(), String> {
    crate::code_agents::launch_code_agent(&command)
}

// ---------------------------------------------------------------------------
// Finance Cards — Home page skill cards
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn list_finance_cards() -> Result<Vec<FinanceCardEntry>, String> {
    crate::finance_cards::load_finance_cards()
}

#[tauri::command]
pub async fn add_finance_card(
    skill_name: String,
    title: String,
    subtitle: String,
    icon: String,
) -> Result<(), String> {
    crate::finance_cards::add_finance_card(&skill_name, &title, &subtitle, &icon)
}

#[tauri::command]
pub async fn remove_finance_card(skill_name: String) -> Result<(), String> {
    crate::finance_cards::remove_finance_card(&skill_name)
}

fn default_config_path() -> std::path::PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    std::path::PathBuf::from(home).join(".aman").join("config.yaml")
}
