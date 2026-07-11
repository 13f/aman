// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

use crate::gateway_client::GatewayClient;
use crate::state::AppState;
use crate::models::{
    ChatMessageEntry, ChatSessionInfo, ChatSessionState,
    DlqEntry, EmotionEntry, EmotionsConfig, FinanceCardEntry,
    MetricsSnapshot, PluginEntry, PluginHealthEntry, QueueDepth,
    RuntimeConfigInfo, RuntimeStatusInfo, SkillEntry, SoulInfo,
    WorkflowEntry,
};
use i18n::Translator;
use secret::{KeychainBackend, SecretBackend};
use serde::{Deserialize, Serialize};
use std::time::Instant;
use tauri::{Emitter, Manager, State};
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind};

/// Create a translator from the app state's locale.
fn translator(state: &State<'_, AppState>) -> Translator {
    Translator::new(state.locale)
}

/// Convenience: translate a key with key-value placeholder pairs.
fn t_with(t: &Translator, key: &'static str, pairs: &[(&str, &str)]) -> String {
    let map: std::collections::HashMap<&str, &str> = pairs.iter().copied().collect();
    t.translate_with(key, &map)
}

/// Helper to get the gateway client from state, failing with a clear message if disconnected.
async fn require_gateway(state: &State<'_, AppState>) -> Result<GatewayClient, String> {
    let guard = state.gateway_client.lock().await;
    let t = translator(state);
    guard
        .clone()
        .ok_or_else(|| t.translate("desktop.error.no_gateway").to_owned())
}

// ---------------------------------------------------------------------------
// Runtime lifecycle — gateway process management
// ---------------------------------------------------------------------------

/// Parse runtime status JSON into RuntimeStatusInfo.
fn parse_runtime_status(v: &serde_json::Value) -> RuntimeStatusInfo {
    let phase_num = v["phase"].as_u64().unwrap_or(0);
    RuntimeStatusInfo {
        phase: format!("Phase{phase_num}"),
        ready: v["ready"].as_bool().unwrap_or(false),
        live: v["live"].as_bool().unwrap_or(false),
        running: phase_num > 0,
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
    let t = translator(&state);
    {
        let guard = state.gateway_client.lock().await;
        if guard.is_some() {
            return Err(t.translate("desktop.error.already_connected").to_owned());
        }
    }

    let port = get_gateway_port().await?;
    let base_url = format!("http://127.0.0.1:{port}");
    let client = GatewayClient::new(&base_url);

    client.health().await.map_err(|e| {
        let mut args = std::collections::HashMap::new();
        args.insert("url", base_url.as_str());
        format!("{}: {e}", t.translate_with("desktop.error.gateway_unreachable", &args))
    })?;

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

/// Resolve the path to the gateway binary (`aman`).
///
/// Search order (first match wins):
/// 1. Homebrew-installed `aman` — `brew --prefix aman` then `<prefix>/bin/aman`
/// 2. User data directory — `~/.aman/bin/aman`
/// 3. Alongside the Tauri app executable — `<app_exe_dir>/aman`
fn gateway_bin_path() -> Result<std::path::PathBuf, String> {
    // ── Tier 1: Homebrew ──────────────────────────────────────────────
    if let Ok(output) = std::process::Command::new("brew")
        .args(["--prefix", "aman"])
        .output()
        && output.status.success() {
            let prefix = String::from_utf8_lossy(&output.stdout).trim().to_owned();
            let bin = std::path::PathBuf::from(&prefix).join("bin").join("aman");
            if bin.exists() {
                tracing::info!(path = %bin.display(), "found gateway via brew");
                return Ok(bin);
            }
        }

    // ── Tier 2: User data directory ───────────────────────────────────
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    let user_bin = std::path::PathBuf::from(&home).join(".aman").join("bin").join("aman");
    if user_bin.exists() {
        tracing::info!(path = %user_bin.display(), "found gateway in user data dir");
        return Ok(user_bin);
    }

    // ── Tier 3: Alongside the app executable ──────────────────────────
    if let Ok(exe) = std::env::current_exe()
        && let Some(exe_dir) = exe.parent() {
            let sibling = exe_dir.join("aman");
            if sibling.exists() {
                tracing::info!(path = %sibling.display(), "found gateway alongside app");
                return Ok(sibling);
            }
        }

    Err("Gateway binary 'aman' not found.\n\n\
        Search order:\n  \
        1. brew-installed: brew install aman\n  \
        2. user data dir: ~/.aman/bin/aman\n  \
        3. alongside the aman desktop app\n\n\
        Build and install it first:\n  \
        cargo build --release -p gateway\n  \
        mkdir -p ~/.aman/bin\n  \
        cp target/release/aman ~/.aman/bin/aman".to_string())
}

#[tauri::command]
pub async fn start_runtime(
    state: State<'_, AppState>,
    gateway_url: String,
) -> Result<String, String> {
    let t = translator(&state);
    // Check not already connected
    {
        let guard = state.gateway_client.lock().await;
        if guard.is_some() {
            return Err(t.translate("desktop.error.already_connected").to_owned());
        }
    }

    // If a gateway is already running (e.g. started manually via CLI), just
    // connect to it without spawning a second process.
    let client = GatewayClient::new(&gateway_url);
    if client.health().await.is_ok() {
        let mut guard = state.gateway_client.lock().await;
        *guard = Some(client);
        // gateway_process stays None — we don't own this process, so we
        // won't kill it on shutdown.
        let mut args = std::collections::HashMap::new();
        args.insert("url", gateway_url.as_str());
        return Ok(t.translate_with("desktop.info.gateway_connected", &args));
    }

    let bin_path = gateway_bin_path()?;

    // Spawn the installed gateway binary
    let mut child = tokio::process::Command::new(&bin_path)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| {
            let mut args = std::collections::HashMap::new();
            let path_str = bin_path.display().to_string();
            args.insert("path", path_str.as_str());
            format!("{}: {e}", t.translate_with("desktop.error.spawn_failed", &args))
        })?;

    // Poll health endpoint until the gateway is ready (up to 120 s)
    let max_retries = 120u32;
    let mut last_err = String::new();

    for _ in 0..max_retries {
        // Detect premature exit
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
                let mut args = std::collections::HashMap::new();
                args.insert("url", gateway_url.as_str());
                return Ok(t.translate_with("desktop.info.gateway_started", &args));
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
    let secs_str = max_retries.to_string();
    let mut args = std::collections::HashMap::new();
    args.insert("secs", secs_str.as_str());
    Err(format!(
        "{}: {last_err}",
        t.translate_with("desktop.error.startup_timeout", &args)
    ))
}

#[tauri::command]
pub async fn stop_runtime(state: State<'_, AppState>) -> Result<String, String> {
    let t = translator(&state);

    // If the CloseRequested shutdown sequence is already in progress,
    // skip to avoid a redundant / duplicate shutdown POST.
    if crate::SHUTTING_DOWN.load(std::sync::atomic::Ordering::Acquire) {
        return Ok(t.translate(i18n::key::DESKTOP_INFO_GATEWAY_DISCONNECTED).to_owned());
    }

    // Best-effort call to the gateway's shutdown endpoint.
    // Timeout = drain_timeout_sec * 2, mirroring the gateway's own
    // shutdown machinery.
    let secs = {
        let guard = state.gateway_client.lock().await;
        if let Some(ref client) = *guard {
            client
                .runtime_config()
                .await
                .ok()
                .and_then(|v| v.get("drain_timeout_sec").and_then(|d| d.as_u64()))
                .unwrap_or(30)
        } else {
            30
        }
    };
    let shutdown_timeout = std::time::Duration::from_secs(secs.saturating_mul(2));

    let base_url = {
        let guard = state.gateway_client.lock().await;
        guard.as_ref().map(|c| c.base_url.clone())
    };

    if let Some(ref url) = base_url {
        let http_client = reqwest::Client::builder()
            .no_proxy()
            .timeout(shutdown_timeout)
            .build()
            .map_err(|e| format!("create http client: {e}"))?;
        let _ = http_client
            .post(format!("{url}/agent/shutdown"))
            .header("x-aman-confirm", "yes")
            .send()
            .await;
    }

    // Clear the client from state
    {
        let mut guard = state.gateway_client.lock().await;
        *guard = None;
    }

    // Kill the child process with graceful escalation
    let child = {
        let mut proc_guard = state.gateway_process.lock().await;
        proc_guard.take()
    };
    if let Some(child) = child {
        crate::commands::escalate_kill(child).await;
        Ok(t.translate(i18n::key::DESKTOP_INFO_GATEWAY_STOPPED).to_owned())
    } else {
        Ok(t.translate(i18n::key::DESKTOP_INFO_GATEWAY_DISCONNECTED).to_owned())
    }
}

/// Send SIGTERM, wait a grace period, then escalate to SIGKILL.
///
/// This avoids the old behavior of unconditional SIGKILL, which bypassed
/// all Drop impls in the gateway process (tracing flush, crossterm TUI
/// cleanup) and froze the terminal in raw mode.
pub(crate) async fn escalate_kill(mut child: tokio::process::Child) {
    // Step 1: SIGTERM — lets the gateway run agenverse.shutdown().
    let pid = child.id();
    #[cfg(unix)]
    {
        // Use the shell's kill command for portability (no extra deps).
        let _ = tokio::process::Command::new("kill")
            .arg("-TERM")
            .arg(pid.map(|p| p.to_string()).unwrap_or_default())
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
    }
    #[cfg(not(unix))]
    {
        // tokio's Child::kill is SIGKILL on Windows — but we only get
        // here after transport.send has already unblocked, so the
        // gateway has had its chance. Fall through to hard kill below.
    }

    // Step 2: wait up to 3 s for the gateway to exit.
    let grace = std::time::Duration::from_secs(3);
    let exited = tokio::time::timeout(grace, child.wait())
        .await
        .is_ok();

    // Step 3: hard kill if it didn't exit in time.
    if !exited {
        let pid_str = pid.map(|p| p.to_string()).unwrap_or_default();
        #[cfg(unix)]
        {
            let _ = tokio::process::Command::new("kill")
                .arg("-KILL")
                .arg(&pid_str)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn();
        }
        #[cfg(not(unix))]
        {
            let _ = child.kill().await;
        }
        let _ = child.wait().await;
    }
}

/// Frontend response to the shutdown busy-agent confirmation dialog.
///
/// Called by the frontend after the user confirms or cancels the
/// "agents are still busy" prompt shown during window close.
#[tauri::command]
pub fn respond_shutdown(confirmed: bool) -> Result<(), String> {
    let tx = {
        let mut guard = crate::SHUTDOWN_CONFIRM_TX
            .lock()
            .map_err(|e| format!("lock: {e}"))?;
        guard.take()
    };
    if let Some(tx) = tx {
        let _ = tx.send(confirmed);
        Ok(())
    } else {
        Err("no pending shutdown confirmation".to_owned())
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
    let t = translator(&state);
    let client = require_gateway(&state).await?;
    client.reload_skills().await?;
    Ok(t.translate(i18n::key::DESKTOP_INFO_SKILLS_RELOADED).to_owned())
}

#[tauri::command]
pub async fn enable_skill(state: State<'_, AppState>, name: String) -> Result<String, String> {
    let t = translator(&state);
    let client = require_gateway(&state).await?;
    client.enable_skill(&name).await?;
    Ok(t_with(&t, i18n::key::DESKTOP_INFO_SKILL_ENABLED, [("name", name.as_str())].as_slice()))
}

#[tauri::command]
pub async fn disable_skill(state: State<'_, AppState>, name: String) -> Result<String, String> {
    let t = translator(&state);
    let client = require_gateway(&state).await?;
    client.disable_skill(&name).await?;
    Ok(t_with(&t, i18n::key::DESKTOP_INFO_SKILL_DISABLED, [("name", name.as_str())].as_slice()))
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
    let t = translator(&state);
    let client = require_gateway(&state).await?;
    client.retry_workflow(&id).await?;
    Ok(t.translate(i18n::key::DESKTOP_INFO_WORKFLOW_RETRIED).to_owned())
}

#[tauri::command]
pub async fn cancel_workflow(state: State<'_, AppState>, id: String) -> Result<String, String> {
    let t = translator(&state);
    let client = require_gateway(&state).await?;
    client.cancel_workflow(&id).await?;
    Ok(t.translate(i18n::key::DESKTOP_INFO_WORKFLOW_CANCELLED).to_owned())
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
    client.get_system_prompt().await
}

#[tauri::command]
pub async fn update_soul(state: State<'_, AppState>, content: String) -> Result<String, String> {
    let t = translator(&state);
    let client = require_gateway(&state).await?;
    client.update_soul(&content).await?;
    Ok(t.translate(i18n::key::DESKTOP_INFO_SOUL_UPDATED).to_owned())
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
    let t = translator(&state);
    let client = require_gateway(&state).await?;
    client.enable_plugin(&name).await?;
    Ok(t_with(
        &t,
        i18n::key::DESKTOP_INFO_PLUGIN_ENABLED,
        [("name", name.as_str())].as_slice(),
    ))
}

#[tauri::command]
pub async fn disable_plugin(state: State<'_, AppState>, name: String) -> Result<String, String> {
    let t = translator(&state);
    let client = require_gateway(&state).await?;
    client.disable_plugin(&name).await?;
    Ok(t_with(
        &t,
        i18n::key::DESKTOP_INFO_PLUGIN_DISABLED,
        [("name", name.as_str())].as_slice(),
    ))
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
    result.sort_by_key(|a| a.capability.clone());
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
    let t = translator(&state);
    let client = require_gateway(&state).await?;
    client.retry_dlq(&id).await?;
    Ok(t.translate(i18n::key::DESKTOP_INFO_DLQ_RETRIED).to_owned())
}

#[tauri::command]
pub async fn discard_dlq(state: State<'_, AppState>, id: String) -> Result<String, String> {
    let t = translator(&state);
    let client = require_gateway(&state).await?;
    client.discard_dlq(&id).await?;
    Ok(t.translate(i18n::key::DESKTOP_INFO_DLQ_DISCARDED).to_owned())
}

// ---------------------------------------------------------------------------
// Chat commands
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn chat_stop_generation(
    state: State<'_, AppState>,
    session_id: String,
) -> Result<String, String> {
    let t = translator(&state);
    if session_id.trim().is_empty() {
        return Err(t.translate(i18n::key::DESKTOP_ERROR_SESSION_ID_EMPTY).to_owned());
    }
    let client = require_gateway(&state).await?;
    client.chat_stop_generation(&session_id).await?;
    Ok(session_id)
}

#[tauri::command]
pub async fn chat_kill_session(
    state: State<'_, AppState>,
    session_id: String,
) -> Result<String, String> {
    let t = translator(&state);
    if session_id.trim().is_empty() {
        return Err(t.translate(i18n::key::DESKTOP_ERROR_SESSION_ID_EMPTY).to_owned());
    }
    let client = require_gateway(&state).await?;
    client.chat_kill_session(&session_id).await?;
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
    let t = translator(&state);
    let _start = Instant::now();

    // --- Rate limiting check (user-level: 10 msg / 60s sliding window, §4.5) ---
    if let Err(err) = state.rate_limiter.allow(&session_id) {
        return Err(format!("429:{}", err.message));
    }

    // Validate message length
    let len = text.chars().count();
    if len > 4096 {
        let len_str = len.to_string();
        return Err(t_with(
            &t,
            i18n::key::DESKTOP_ERROR_MESSAGE_TOO_LONG,
            [("max", "4096"), ("len", len_str.as_str())].as_slice(),
        ));
    }
    if text.trim().is_empty() {
        return Err(t.translate(i18n::key::DESKTOP_ERROR_MESSAGE_EMPTY).to_owned());
    }
    if session_id.trim().is_empty() {
        return Err(t.translate(i18n::key::DESKTOP_ERROR_SESSION_ID_EMPTY).to_owned());
    }

    let client = require_gateway(&state).await?;
    client.chat_send_message(&session_id, &text, expected_version).await
}

#[tauri::command]
pub async fn chat_session_list(
    state: State<'_, AppState>,
    agent_key: Option<String>,
) -> Result<Vec<ChatSessionInfo>, String> {
    let client = require_gateway(&state).await?;
    let v = client.chat_sessions(agent_key.as_deref()).await?;
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
            agent_id: item["agent_id"].as_str().unwrap_or("").to_owned(),
            // Gateway API path: no direct JSONL access, so only mark
            // zero-message sessions as deletable here. The local-DB path
            // (chat_session_list_db) applies the full low-value check.
            deletable: item["message_count"].as_u64().unwrap_or(0) == 0,
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

    /// Concatenate all agent reply text for a session's JSONL, searching
    /// across all agent directories. Mirrors the gateway's
    /// `SessionStore::load_session_events` + reply extraction used by
    /// `delete_stale_low_value_sessions`.
    fn jsonl_agent_reply_text(agents_root: &std::path::Path, session_id: &str) -> String {
        let entries = match std::fs::read_dir(agents_root) {
            Ok(e) => e,
            Err(_) => return String::new(),
        };
        let mut all = String::new();
        for entry in entries.flatten() {
            if !entry.file_type().is_ok_and(|t| t.is_dir()) {
                continue;
            }
            let path = entry.path().join("sessions").join(format!("{session_id}.jsonl"));
            let content = match std::fs::read_to_string(&path) {
                Ok(c) => c,
                Err(_) => continue,
            };
            for line in content.lines() {
                let Ok(evt) = serde_json::from_str::<serde_json::Value>(line) else { continue };
                let Some(et) = evt["event_type"].as_str() else { continue };
                if !et.contains("reply_ready") && et != "llm_reply_ready" {
                    continue;
                }
                let payload = &evt["payload"];
                if let Some(reply) = payload["reply"]
                    .as_str()
                    .or_else(|| payload["full_text"].as_str())
                {
                    all.push_str(reply);
                    all.push('\n');
                }
            }
        }
        all
    }

    /// Returns true if concatenated agent reply text matches patterns that
    /// indicate a session produced no useful content. Mirrors the gateway's
    /// `SessionStore::is_low_value_reply` so the UI's "deletable" flag uses
    /// the same keyword/regex rules as the automated sleep-phase cleanup.
    fn is_low_value_reply(reply_text: &str) -> bool {
        let normalized = reply_text.to_lowercase();

        // ── Category 1: Idle / no-work signals ──
        let idle_signals = [
            "agent is idle",
            "no work items",
            "nothing to do",
            "no tasks assigned",
            "no pending work",
            "currently idle",
            "没有工作项目",
            "无任务分配",
            "agent idle",
            "no active work",
        ];
        if idle_signals.iter().any(|s| normalized.contains(s)) {
            return true;
        }

        // ── Category 2: No-result batch/compute tasks ──
        let no_result_signals = [
            "no match found",
            "no collision found",
            "no results",
            "result: ❌",
            "nothing found",
            "0 matches",
            "no match",
            "未找到匹配",
            "无碰撞",
            "no wallet was cracked",
        ];
        let has_no_result = no_result_signals.iter().any(|s| normalized.contains(s));
        if !has_no_result {
            return false;
        }

        // Require corroborating signals that this was a batch compute task,
        // not a legitimate search that happened to find nothing.
        let batch_signals = [
            "keys checked",
            "workers",
            "duration",
            "elapsed",
            "average rate",
            "search space",
            "keys/sec",
            "probability",
            "expected time",
        ];
        let batch_hits = batch_signals
            .iter()
            .filter(|s| normalized.contains(*s))
            .count();

        // At least 3 batch signals + a no-result signal → low-value batch job
        batch_hits >= 3
    }

    let db = rusqlite::Connection::open(&db_path)
        .map_err(|e| format!("open sessions.db: {e}"))?;
    // Enable WAL mode so concurrent reads aren't blocked by gateway writes.
    let _ = db.execute_batch("PRAGMA journal_mode=WAL;");

    let mut stmt = db.prepare(
        "SELECT id, state, message_count, created_at, last_active_at, session_type, title, agent_id
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
            session_type: row.get(5)?,
            title: row.get::<_, Option<String>>(6)?,
            agent_id: row.get::<_, String>(7).unwrap_or_default(),
            parent_session_id: None,
            branch_message_id: None,
            version: 0,
            deletable: false,
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
        // Use DB title if set, otherwise derive from first user message in JSONL.
        if item.title.is_none() {
            item.title = jsonl_session_title(&agents_root, &item.id);
        }
        // A session is deletable if: it has zero messages, OR its agent replies
        // match low-value patterns (idle signals, no-result batch jobs, …), OR
        // a `session:marker` event with `data.deletable=true` is present
        // (written by the LLM `session` tool when it produced no useful output).
        item.deletable =
            item.message_count == 0 ||
            jsonl_has_deletable_marker(&agents_root, &item.id) ||
            is_low_value_reply(&jsonl_agent_reply_text(&agents_root, &item.id));
        items.push(item);
    }
    Ok(items)
}

/// True if any `session:marker` event in the session JSONL carries
/// `data.deletable=true`. Mirrors the gateway
/// `delete_stale_low_value_sessions` marker detection so the UI and the
/// sleep cleanup agree on what's deletable.
fn jsonl_has_deletable_marker(agents_root: &std::path::Path, session_id: &str) -> bool {
    let entries = match std::fs::read_dir(agents_root) {
        Ok(e) => e,
        Err(_) => return false,
    };
    for entry in entries.flatten() {
        if !entry.file_type().is_ok_and(|t| t.is_dir()) {
            continue;
        }
        let path = entry
            .path()
            .join("sessions")
            .join(format!("{session_id}.jsonl"));
        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(_) => continue,
        };
        for line in content.lines() {
            let Ok(evt) = serde_json::from_str::<serde_json::Value>(line) else { continue };
            if !evt["event_type"]
                .as_str()
                .map_or(false, |et| et.contains("session:marker"))
            {
                continue;
            }
            if evt["payload"]["data"]["deletable"].as_bool() == Some(true) {
                return true;
            }
        }
    }
    false
}

#[tauri::command]
pub async fn chat_session_create(
    state: State<'_, AppState>,
    agent_key: Option<String>,
    session_type: Option<String>,
) -> Result<String, String> {
    let client = require_gateway(&state).await?;
    client.chat_session_create(agent_key.as_deref(), session_type.as_deref()).await
}

#[tauri::command]
pub async fn chat_session_rename(
    agent_key: Option<String>,
    session_id: String,
    title: String,
) -> Result<(), String> {
    let home = std::env::var("HOME").map_err(|_| "HOME not set".to_owned())?;
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
    let db_path = std::path::PathBuf::from(&home)
        .join(".aman")
        .join("agents")
        .join(&agent_key)
        .join("sessions.db");
    let db = rusqlite::Connection::open(&db_path)
        .map_err(|e| format!("open sessions.db: {e}"))?;
    let title_val: Option<&str> = if title.is_empty() { None } else { Some(&title) };
    db.execute(
        "UPDATE sessions SET title = ?1 WHERE id = ?2",
        rusqlite::params![title_val, session_id],
    )
    .map_err(|e| format!("update session title: {e}"))?;
    Ok(())
}

#[tauri::command]
pub async fn explore_start(
    state: State<'_, AppState>,
    agent_key: Option<String>,
) -> Result<serde_json::Value, String> {
    let client = require_gateway(&state).await?;
    client.explore_start(agent_key.as_deref()).await
}

/// Trigger an idle-run action for a specific tag (work, study, fun).
#[tauri::command]
pub async fn idle_run(
    state: State<'_, AppState>,
    tag: String,
    agent_key: Option<String>,
    background: Option<bool>,
) -> Result<serde_json::Value, String> {
    let client = require_gateway(&state).await?;
    client.idle_run(&tag, agent_key.as_deref(), background.unwrap_or(false)).await
}

/// Fetch per-agent work/study/fun idle-run button availability.
#[tauri::command]
pub async fn list_idle_availability(
    state: State<'_, AppState>,
) -> Result<serde_json::Value, String> {
    let client = require_gateway(&state).await?;
    client.list_idle_availability().await
}

/// Create a branch session forked from a specific message in an existing session.
#[tauri::command]
pub async fn chat_session_branch(
    state: State<'_, AppState>,
    session_id: String,
    message_id: String,
    agent_key: Option<String>,
    session_type: Option<String>,
) -> Result<String, String> {
    let t = translator(&state);
    if session_id.trim().is_empty() {
        return Err(t.translate(i18n::key::DESKTOP_ERROR_SESSION_ID_EMPTY).to_owned());
    }
    if message_id.trim().is_empty() {
        return Err(t.translate(i18n::key::DESKTOP_ERROR_MESSAGE_ID_EMPTY).to_owned());
    }

    let client = require_gateway(&state).await?;
    client.chat_session_create_branch(&session_id, &message_id, agent_key.as_deref(), session_type.as_deref()).await
}

#[tauri::command]
pub async fn chat_session_close(
    state: State<'_, AppState>,
    session_id: String,
    _expected_version: Option<u64>,
) -> Result<String, String> {
    let t = translator(&state);
    if session_id.trim().is_empty() {
        return Err(t.translate(i18n::key::DESKTOP_ERROR_SESSION_ID_EMPTY).to_owned());
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
    let t = translator(&state);
    if session_id.trim().is_empty() {
        return Err(t.translate(i18n::key::DESKTOP_ERROR_SESSION_ID_EMPTY).to_owned());
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
    let t = translator(&state);
    if session_id.trim().is_empty() {
        return Err(t.translate(i18n::key::DESKTOP_ERROR_SESSION_ID_EMPTY).to_owned());
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
    let t = translator(&state);
    if session_id.trim().is_empty() {
        return Err(t.translate(i18n::key::DESKTOP_ERROR_SESSION_ID_EMPTY).to_owned());
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
        return Err(
            i18n::Translator::default()
                .translate(i18n::key::DESKTOP_ERROR_SESSION_ID_EMPTY)
                .to_owned(),
        );
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
    let t = translator(&state);
    if session_id.trim().is_empty() {
        return Err(t.translate(i18n::key::DESKTOP_ERROR_SESSION_ID_EMPTY).to_owned());
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
    let t = translator(&state);
    if session_id.trim().is_empty() {
        return Err(t.translate(i18n::key::DESKTOP_ERROR_SESSION_ID_EMPTY).to_owned());
    }
    if text.trim().is_empty() {
        return Err(t.translate(i18n::key::DESKTOP_ERROR_MESSAGE_EDITED_EMPTY).to_owned());
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
    let t = translator(&state);
    if trace_id.trim().is_empty() {
        return Err(t.translate(i18n::key::DESKTOP_ERROR_TRACE_ID_EMPTY).to_owned());
    }
    let client = require_gateway(&state).await?;
    client.event_trace(&trace_id).await
}

// ---------------------------------------------------------------------------
// Tool authorization (native OS dialog)
// ---------------------------------------------------------------------------

/// Show a native OS dialog for tool authorization and POST the user's
/// decision to the gateway's `/tool-auth/respond` endpoint.
///
/// Uses `tauri-plugin-dialog` which produces a real OS-native dialog
/// (outside the webview) on macOS, Windows, and Linux — it cannot be
/// bypassed by AI-driven UI manipulation.
#[tauri::command]
pub async fn show_tool_auth_dialog(
    app_handle: tauri::AppHandle,
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

    let message = format!(
        "Tool \"{tool_name}\" wants to execute with the following arguments:\n\n{arguments_summary}\n\nAllow this operation?"
    );

    let (tx, rx) = tokio::sync::oneshot::channel();
    app_handle
        .dialog()
        .message(message)
        .title("aman — Tool Authorization")
        .kind(MessageDialogKind::Warning)
        .buttons(MessageDialogButtons::OkCancelCustom(
            "Allow".into(),
            "Deny".into(),
        ))
        .show(move |confirmed| {
            let _ = tx.send(confirmed);
        });

    let approved = rx.await.unwrap_or(false); // default to Deny on error

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
        Ok(if approved { "allow".to_owned() } else { "deny".to_owned() })
    } else {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        Err(format!("Auth respond failed ({status}): {body}"))
    }
}

// ---------------------------------------------------------------------------
// Plugin capability authorization
// ---------------------------------------------------------------------------

/// Show a native OS dialog requesting the user's approval for a plugin's
/// requested capabilities. POSTs the decision back to the gateway's
/// `/plugin-auth/respond` endpoint, which persists the approval with a
/// BLAKE3 keyed-hash signature and dynamically loads the plugin.
#[tauri::command]
pub async fn show_plugin_auth_dialog(
    app_handle: tauri::AppHandle,
    state: State<'_, AppState>,
    plugin_name: String,
    version: String,
    capabilities_summary: String,
) -> Result<String, String> {
    let base_url = {
        let guard = state.gateway_client.lock().await;
        guard
            .as_ref()
            .map(|c| c.base_url.clone())
            .ok_or_else(|| "Gateway not connected".to_owned())?
    };

    let message = format!(
        "Plugin \"{plugin_name}\" (v{version}) requests the following capabilities:\n\n{capabilities_summary}\n\nAllow this plugin to use these capabilities?"
    );

    let (tx, rx) = tokio::sync::oneshot::channel();
    app_handle
        .dialog()
        .message(message)
        .title("aman — Plugin Authorization")
        .kind(MessageDialogKind::Warning)
        .buttons(MessageDialogButtons::OkCancelCustom(
            "Allow".into(),
            "Deny".into(),
        ))
        .show(move |confirmed| {
            let _ = tx.send(confirmed);
        });

    let approved = rx.await.unwrap_or(false); // default to Deny on error

    let client = reqwest::Client::builder()
        .no_proxy()
        .build()
        .map_err(|e| format!("Failed to create HTTP client: {e}"))?;

    let resp = client
        .post(format!("{base_url}/plugin-auth/respond"))
        .json(&serde_json::json!({
            "plugin_name": plugin_name,
            "approved": approved,
        }))
        .send()
        .await
        .map_err(|e| format!("Failed to send plugin auth response: {e}"))?;

    if resp.status().is_success() {
        Ok(if approved { "allow".to_owned() } else { "deny".to_owned() })
    } else {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        Err(format!("Plugin auth respond failed ({status}): {body}"))
    }
}

// ---------------------------------------------------------------------------
// Generic confirmation dialog (for iframes like Team, plugin pages, etc.)
// ---------------------------------------------------------------------------

/// Show a native OS confirmation dialog with a custom title, message,
/// and confirm/cancel button labels. Returns `true` if the user clicked
/// the confirm button, `false` otherwise.
///
/// This is designed to be called from iframe content via `postMessage`
/// (see `App.svelte` for the bridge). The iframe posts a message with
/// `{type: "aman:confirm", ...}` and the bridge invokes this command,
/// then posts the boolean result back.
#[tauri::command]
pub async fn show_confirm_dialog(
    app_handle: tauri::AppHandle,
    title: String,
    message: String,
    confirm_label: Option<String>,
    cancel_label: Option<String>,
) -> Result<bool, String> {
    let confirm = confirm_label.unwrap_or_else(|| "Confirm".into());
    let cancel = cancel_label.unwrap_or_else(|| "Cancel".into());

    let (tx, rx) = tokio::sync::oneshot::channel();
    app_handle
        .dialog()
        .message(message)
        .title(title)
        .kind(MessageDialogKind::Warning)
        .buttons(MessageDialogButtons::OkCancelCustom(confirm, cancel))
        .show(move |confirmed| {
            let _ = tx.send(confirmed);
        });

    Ok(rx.await.unwrap_or(false))
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
// IM Channel key management (multi-instance)
// ---------------------------------------------------------------------------
//
// Keychain key format: aman.bot.{platform}.{instance}.{field}
//   Default instance: aman.bot.telegram.default.token
//   Custom instance:  aman.bot.telegram.work.token
//
// To discover instances, we scan for tokens matching
// `aman.bot.{platform}.*.token` and extract the instance name.

/// Default instance name used when the user doesn't create custom instances.
const DEFAULT_INSTANCE: &str = "default";

/// Known IM channel platforms with their field prototypes.
/// Keychain keys are constructed dynamically: `aman.bot.{id}.{instance}.{field_key}`
#[allow(clippy::type_complexity)] // Static table type is inherently nested.
static IM_CHANNEL_PLATFORMS: &[(&str, &str, &[(&str, &str)])] = &[
    ("telegram", "Telegram", &[
        ("token", "Bot Token"),
        ("username", "Bot Username"),
        ("allowed_chat_ids", "Allowed Chat IDs (comma-separated, empty = allow all)"),
    ]),
    ("slack", "Slack", &[
        ("bot_token", "Bot User OAuth Token (xoxb-...)"),
        ("app_token", "App-Level Token (xapp-...)"),
    ]),
    ("discord", "Discord", &[
        ("token", "Bot Token"),
    ]),
    ("matrix", "Matrix", &[
        ("homeserver_url", "Homeserver URL"),
        ("username", "Username / MXID"),
        ("access_token", "Access Token / Password"),
    ]),
];

#[derive(Serialize, Clone)]
pub struct ImChannelField {
    pub key: String,
    pub label: String,
    pub configured: bool,
}

#[derive(Serialize, Clone)]
pub struct ImChannelInstance {
    pub name: String,
    pub fields: Vec<ImChannelField>,
}

#[derive(Serialize, Clone)]
pub struct ImChannel {
    pub id: String,
    pub display_name: String,
    pub instances: Vec<ImChannelInstance>,
}

/// Build a keychain key for a given platform/instance/field.
fn im_keychain_key(platform: &str, instance: &str, field: &str) -> String {
    format!("aman.bot.{platform}.{instance}.{field}")
}

/// Scan the keychain for all configured instances of a platform by looking
/// for token entries matching `aman.bot.{platform}.*.token` (or the
/// platform's first field if it has no field named "token").
fn discover_instances(backend: &KeychainBackend, platform: &str, fields: &[(&str, &str)]) -> Vec<String> {
    // The first field is the primary key used for discovery.
    let primary_field = fields[0].0;
    // We can't enumerate keychain entries, so we always include "default"
    // and let users add custom instances manually.
    let mut instances = vec![DEFAULT_INSTANCE.to_owned()];

    // Check if the default instance is actually configured.
    let default_configured = backend
        .get(&im_keychain_key(platform, DEFAULT_INSTANCE, primary_field))
        .ok()
        .flatten()
        .is_some();

    if default_configured {
        // Also check for common custom instances.
        // In a full implementation, we'd enumerate keychain entries.
        // For now, we scan known custom suffixes the user might have created.
        for suffix in &["work", "personal", "trading"] {
            let key = im_keychain_key(platform, suffix, primary_field);
            if backend.get(&key).ok().flatten().is_some() {
                instances.push(suffix.to_string());
            }
        }
    }

    instances
}

#[tauri::command]
pub async fn list_im_channels() -> Result<Vec<ImChannel>, String> {
    let backend = KeychainBackend;
    let mut channels: Vec<ImChannel> = Vec::new();
    for (id, display_name, fields) in IM_CHANNEL_PLATFORMS {
        let instances = discover_instances(&backend, id, fields);
        let channel_instances: Vec<ImChannelInstance> = instances
            .into_iter()
            .map(|inst_name| {
                let instance_fields: Vec<ImChannelField> = fields
                    .iter()
                    .map(|(key, label)| {
                        let kc_key = im_keychain_key(id, &inst_name, key);
                        ImChannelField {
                            key: key.to_string(),
                            label: label.to_string(),
                            configured: backend.get(&kc_key).ok().flatten().is_some(),
                        }
                    })
                    .collect();
                ImChannelInstance {
                    name: inst_name,
                    fields: instance_fields,
                }
            })
            .collect();
        channels.push(ImChannel {
            id: id.to_string(),
            display_name: display_name.to_string(),
            instances: channel_instances,
        });
    }
    Ok(channels)
}

#[tauri::command]
pub async fn save_im_channel(
    platform: String,
    instance: Option<String>,
    field_key: String,
    value: String,
) -> Result<String, String> {
    let instance = instance.unwrap_or_else(|| DEFAULT_INSTANCE.to_owned());
    let platform_def = IM_CHANNEL_PLATFORMS
        .iter()
        .find(|(id, _, _)| *id == platform)
        .ok_or_else(|| format!("Unknown platform: {platform}"))?;
    let field_def = platform_def
        .2
        .iter()
        .find(|(key, _)| *key == field_key)
        .ok_or_else(|| format!("Unknown field '{field_key}' for platform '{platform}'"))?;
    let keychain_key = im_keychain_key(&platform, &instance, field_def.0);
    let backend = KeychainBackend;
    backend
        .set(&keychain_key, &value)
        .map_err(|e| format!("Failed to save to Keychain: {e}"))?;
    Ok(format!("{platform}.{instance}.{field_key} saved to Keychain"))
}

#[tauri::command]
pub async fn delete_im_channel_field(
    platform: String,
    instance: Option<String>,
    field_key: String,
) -> Result<String, String> {
    let instance = instance.unwrap_or_else(|| DEFAULT_INSTANCE.to_owned());
    let platform_def = IM_CHANNEL_PLATFORMS
        .iter()
        .find(|(id, _, _)| *id == platform)
        .ok_or_else(|| format!("Unknown platform: {platform}"))?;
    let field_def = platform_def
        .2
        .iter()
        .find(|(key, _)| *key == field_key)
        .ok_or_else(|| format!("Unknown field '{field_key}' for platform '{platform}'"))?;
    let keychain_key = im_keychain_key(&platform, &instance, field_def.0);
    let backend = KeychainBackend;
    backend
        .set(&keychain_key, "")
        .map_err(|e| format!("Failed to delete from Keychain: {e}"))?;
    Ok(format!("{platform}.{instance}.{field_key} removed"))
}

/// Test connection to a configured IM channel. Returns the bot/account display
/// name on success, or an error message on failure.
#[tauri::command]
pub async fn test_im_channel(
    platform: String,
    instance: Option<String>,
) -> Result<String, String> {
    let instance = instance.unwrap_or_else(|| "default".to_owned());
    let backend = KeychainBackend;

    match platform.as_str() {
        "telegram" => {
            let token_key = format!("aman.bot.telegram.{instance}.token");
            let token = backend
                .get(&token_key)
                .map_err(|e| format!("Keychain error: {e}"))?
                .ok_or_else(|| "No bot token configured for this instance".to_owned())?;

            let url = format!("https://api.telegram.org/bot{token}/getMe");
            let resp = reqwest::get(&url)
                .await
                .map_err(|e| format!("Connection failed: {e}"))?;
            let body: serde_json::Value = resp
                .json()
                .await
                .map_err(|e| format!("Invalid response: {e}"))?;

            if body["ok"].as_bool().unwrap_or(false) {
                let username = body["result"]["username"]
                    .as_str()
                    .unwrap_or("unknown");
                let first_name = body["result"]["first_name"]
                    .as_str()
                    .unwrap_or("");
                return Ok(format!("@{username} — {first_name}"));
            }

            let desc = body["description"]
                .as_str()
                .unwrap_or("Unknown error");
            Err(format!("Telegram API error: {desc}"))
        }
        "slack" => {
            let token_key = format!("aman.bot.slack.{instance}.bot_token");
            let token = backend
                .get(&token_key)
                .map_err(|e| format!("Keychain error: {e}"))?
                .ok_or_else(|| "No bot token configured".to_owned())?;

            let client = reqwest::Client::new();
            let resp = client
                .post("https://slack.com/api/auth.test")
                .bearer_auth(&token)
                .send()
                .await
                .map_err(|e| format!("Connection failed: {e}"))?;
            let body: serde_json::Value = resp
                .json()
                .await
                .map_err(|e| format!("Invalid response: {e}"))?;

            if body["ok"].as_bool().unwrap_or(false) {
                let user = body["user"].as_str().unwrap_or("unknown");
                let team = body["team"].as_str().unwrap_or("unknown");
                return Ok(format!("{user} @ {team}"));
            }
            Err(format!("Slack API error: {}", body["error"].as_str().unwrap_or("unknown")))
        }
        "discord" => {
            let token_key = format!("aman.bot.discord.{instance}.token");
            let token = backend
                .get(&token_key)
                .map_err(|e| format!("Keychain error: {e}"))?
                .ok_or_else(|| "No bot token configured".to_owned())?;

            let client = reqwest::Client::new();
            let resp = client
                .get("https://discord.com/api/v10/users/@me")
                .header("Authorization", format!("Bot {token}"))
                .send()
                .await
                .map_err(|e| format!("Connection failed: {e}"))?;
            let body: serde_json::Value = resp
                .json()
                .await
                .map_err(|e| format!("Invalid response: {e}"))?;

            if let Some(username) = body["username"].as_str() {
                let discrim = body["discriminator"].as_str().unwrap_or("0");
                return Ok(format!("{username}#{discrim}"));
            }
            Err(format!("Discord API error: {}", body["message"].as_str().unwrap_or("unknown")))
        }
        "matrix" => {
            let hs_key = format!("aman.bot.matrix.{instance}.homeserver_url");
            let token_key = format!("aman.bot.matrix.{instance}.access_token");
            let hs_url = backend
                .get(&hs_key)
                .map_err(|e| format!("Keychain error: {e}"))?
                .ok_or_else(|| "No homeserver URL configured".to_owned())?;
            let token = backend
                .get(&token_key)
                .map_err(|e| format!("Keychain error: {e}"))?
                .ok_or_else(|| "No access token configured".to_owned())?;

            let client = reqwest::Client::new();
            let url = format!(
                "{}/_matrix/client/v3/account/whoami",
                hs_url.trim_end_matches('/')
            );
            let resp = client
                .get(&url)
                .bearer_auth(&token)
                .send()
                .await
                .map_err(|e| format!("Connection failed: {e}"))?;
            let body: serde_json::Value = resp
                .json()
                .await
                .map_err(|e| format!("Invalid response: {e}"))?;

            if let Some(user_id) = body["user_id"].as_str() {
                return Ok(user_id.to_owned());
            }
            Err(format!("Matrix API error: {}", body["error"].as_str().unwrap_or("unknown")))
        }
        _ => Err(format!("Unknown platform: {platform}")),
    }
}

/// Reload an IM channel source from keychain without restarting the gateway.
///
/// Uses a raw TCP stream (no reqwest, no curl) to bypass any proxy
/// interference. Sends a minimal HTTP/1.1 POST request directly to
/// the gateway's localhost port.
#[tauri::command]
pub async fn reload_im_channel(
    _state: State<'_, AppState>,
    platform: String,
    instance: Option<String>,
) -> Result<String, String> {
    let instance = instance.unwrap_or_else(|| "default".to_owned());
    let port = get_gateway_port().await?;
    let path = format!("/im-channel/{}/{}/reload", platform, instance);

    // Build a minimal HTTP/1.1 POST request.
    let request = format!(
        "POST {} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nConnection: close\r\nContent-Length: 0\r\n\r\n",
        path, port
    );

    // Connect directly via TCP — bypasses ALL proxy settings.
    let addr = format!("127.0.0.1:{port}");
    let mut stream = tokio::net::TcpStream::connect(&addr)
        .await
        .map_err(|e| format!("TCP connect failed: {e}"))?;

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    stream
        .write_all(request.as_bytes())
        .await
        .map_err(|e| format!("Write failed: {e}"))?;

    let mut response = Vec::new();
    stream
        .read_to_end(&mut response)
        .await
        .map_err(|e| format!("Read failed: {e}"))?;

    let response_str = String::from_utf8_lossy(&response);
    if response_str.contains("200 OK") {
        Ok(format!("{platform}/{instance} reloaded"))
    } else {
        let first_line = response_str.lines().next().unwrap_or("no response");
        Err(format!("Reload failed: {first_line}"))
    }
}

/// Delete an entire instance (all fields) for a given platform.
#[tauri::command]
pub async fn delete_im_channel_instance(
    platform: String,
    instance: String,
) -> Result<String, String> {
    if instance == DEFAULT_INSTANCE {
        return Err("Cannot delete the default instance — clear its fields instead.".to_owned());
    }
    let platform_def = IM_CHANNEL_PLATFORMS
        .iter()
        .find(|(id, _, _)| *id == platform)
        .ok_or_else(|| format!("Unknown platform: {platform}"))?;
    let backend = KeychainBackend;
    for (key, _label) in platform_def.2 {
        let kc_key = im_keychain_key(&platform, &instance, key);
        let _ = backend.set(&kc_key, "");
    }
    Ok(format!("{platform}.{instance} removed"))
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
/// Respects secrets_mode: in "env" mode, only checks env vars (no Keychain).
fn provider_has_api_key(key: &str) -> bool {
    let use_keyring = config::AmanConfig::from_default_path()
        .map(|cfg| cfg.runtime.security.secrets_mode.use_keyring())
        .unwrap_or(true); // default to keyring if config can't be read

    if use_keyring {
        // Keychain is the primary store
        let backend = KeychainBackend;
        if let Ok(Some(_)) = backend.get(&format!("aman.providers.{key}.api_key")) {
            return true;
        }
    }
    // Env var (primary source in env mode, fallback in keyring mode)
    let env_var = format!("AMAN_PROVIDER_{}_API_KEY", provider_env_key(key));
    std::env::var(env_var).is_ok()
}

#[tauri::command]
pub async fn list_providers(state: State<'_, AppState>) -> Result<Vec<crate::models::ProviderEntry>, String> {
    let t = translator(&state);
    let config = config::AmanConfig::from_default_path()
        .map_err(|e| t_with(&t, "desktop.error.config_read", &[("detail", &e.to_string())]))?;

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
    entries.sort_by_key(|a| a.key.clone());
    Ok(entries)
}

#[tauri::command]
pub async fn create_provider(
    state: State<'_, AppState>,key: String,
    display_name: String,
    base_url: String,
) -> Result<String, String> {
    let t = translator(&state);
    if !config::is_valid_identifier(&key) {
        return Err(t.translate("desktop.error.provider_key_invalid").to_owned());
    }

    let mut aman_config = config::AmanConfig::from_default_path()
        .map_err(|e| t_with(&t, "desktop.error.config_read", &[("detail", &e.to_string())]))?;

    if aman_config.providers.contains_key(&key) {
        return Err(t_with(&t, "desktop.error.provider_exists", &[("key", &key)]));
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
    aman_config.save(&path).map_err(|e| t_with(&t, "desktop.error.config_save", &[("detail", &e.to_string())]))?;

    Ok(t_with(&t, "desktop.info.provider_created", &[("key", &key)]))
}

#[tauri::command]
pub async fn update_provider(
    state: State<'_, AppState>,key: String,
    display_name: Option<String>,
    base_url: Option<String>,
) -> Result<String, String> {
    let t = translator(&state);
    let mut aman_config = config::AmanConfig::from_default_path()
        .map_err(|e| t_with(&t, "desktop.error.config_read", &[("detail", &e.to_string())]))?;

    let provider = aman_config
        .providers
        .get_mut(&key)
        .ok_or_else(|| t_with(&t, "desktop.error.provider_not_found", &[("key", &key)]))?;

    if let Some(name) = display_name {
        provider.display_name = name;
    }
    if let Some(url) = base_url {
        provider.base_url = url;
    }

    let path = default_config_path();
    aman_config.save(&path).map_err(|e| t_with(&t, "desktop.error.config_save", &[("detail", &e.to_string())]))?;

    Ok(t_with(&t, "desktop.info.provider_updated", &[("key", &key)]))
}

#[tauri::command]
pub async fn delete_provider(state: State<'_, AppState>, key: String) -> Result<String, String> {
    let t = translator(&state);
    let mut aman_config = config::AmanConfig::from_default_path()
        .map_err(|e| t_with(&t, "desktop.error.config_read", &[("detail", &e.to_string())]))?;

    if !aman_config.providers.contains_key(&key) {
        return Err(t_with(&t, "desktop.error.provider_not_found", &[("key", &key)]));
    }

    // Check that no agent references this provider.
    let agents_using: Vec<String> = aman_config
        .agents
        .iter()
        .filter(|(_, a)| a.provider == key)
        .map(|(k, _)| k.clone())
        .collect();
    if !agents_using.is_empty() {
        return Err(t_with(&t, "desktop.error.provider_in_use", &[("key", &key), ("agents", &agents_using.join(", "))]));
    }

    aman_config.providers.remove(&key);

    let path = default_config_path();
    aman_config.save(&path).map_err(|e| t_with(&t, "desktop.error.config_save", &[("detail", &e.to_string())]))?;

    Ok(t_with(&t, "desktop.info.provider_deleted", &[("key", &key)]))
}

#[tauri::command]
pub async fn set_provider_api_key(state: State<'_, AppState>, key: String, api_key: String) -> Result<String, String> {
    let t = translator(&state);
    // Validate provider exists
    let aman_config = config::AmanConfig::from_default_path()
        .map_err(|e| t_with(&t, "desktop.error.config_read", &[("detail", &e.to_string())]))?;
    if !aman_config.providers.contains_key(&key) {
        return Err(t_with(&t, "desktop.error.provider_not_found", &[("key", &key)]));
    }

    let backend = KeychainBackend;
    backend
        .set(&format!("aman.providers.{key}.api_key"), &api_key)
        .map_err(|e| t_with(&t, "desktop.error.keychain_save", &[("detail", &e.to_string())]))?;

    Ok(t_with(&t, "desktop.info.provider_api_key_saved", &[("key", &key), ("backend", "macOS Keychain")]))
}

#[tauri::command]
pub async fn has_provider_api_key(state: State<'_, AppState>, key: String) -> Result<bool, String> {
    let t = translator(&state);
    // Validate provider exists
    let aman_config = config::AmanConfig::from_default_path()
        .map_err(|e| t_with(&t, "desktop.error.config_read", &[("detail", &e.to_string())]))?;
    if !aman_config.providers.contains_key(&key) {
        return Err(t_with(&t, "desktop.error.provider_not_found", &[("key", &key)]));
    }

    Ok(provider_has_api_key(&key))
}

/// Retrieve the API key for a provider.
/// Respects secrets_mode: in "env" mode, only checks env vars (no Keychain).
fn provider_get_api_key(key: &str) -> Option<String> {
    let use_keyring = config::AmanConfig::from_default_path()
        .map(|cfg| cfg.runtime.security.secrets_mode.use_keyring())
        .unwrap_or(true); // default to keyring if config can't be read

    if use_keyring {
        let backend = KeychainBackend;
        if let Ok(Some(val)) = backend.get(&format!("aman.providers.{key}.api_key")) {
            return Some(val);
        }
    }
    let env_var = format!("AMAN_PROVIDER_{}_API_KEY", provider_env_key(key));
    std::env::var(env_var).ok()
}

#[derive(Debug, Deserialize)]
struct OpenAIModelListResponse {
    data: Vec<OpenAIModelEntry>,
}

#[derive(Debug, Deserialize)]
struct OpenAIModelEntry {
    id: String,
}

#[tauri::command]
pub async fn list_provider_models(
    state: State<'_, AppState>,provider_key: String,
) -> Result<Vec<crate::models::ModelEntry>, String> {
    let t = translator(&state);
    let aman_config = config::AmanConfig::from_default_path()
        .map_err(|e| t_with(&t, "desktop.error.config_read", &[("detail", &e.to_string())]))?;

    let provider = aman_config
        .providers
        .get(&provider_key)
        .ok_or_else(|| t_with(&t, "desktop.error.provider_not_found", &[("key", &provider_key)]))?;

    let secrets_mode = aman_config.runtime.security.secrets_mode;

    // In env mode, skip the remote API call — providers/models are configured
    // statically in config files (no Keychain access needed).
    if !secrets_mode.use_keyring() {
        let mut models: Vec<crate::models::ModelEntry> = provider
            .models
            .iter()
            .map(|m| crate::models::ModelEntry {
                id: m.id.clone(),
                model_id: m.model_id.clone(),
            })
            .collect();
        models.sort_by_key(|a| a.id.clone());
        return Ok(models);
    }

    // Try fetching from the provider's /v1/models endpoint.
    if let Some(api_key) = provider_get_api_key(&provider_key) {
        let url = format!("{}/v1/models", provider.base_url.trim_end_matches('/'));
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .build()
            .map_err(|e| t_with(&t, "desktop.error.http_client", &[("detail", &e.to_string())]))?;

        match client
            .get(&url)
            .header("Authorization", format!("Bearer {api_key}"))
            .send()
            .await
        {
            Ok(resp) => {
                if let Ok(body) = resp.json::<OpenAIModelListResponse>().await
                    && !body.data.is_empty() {
                        let mut models: Vec<crate::models::ModelEntry> = body
                            .data
                            .into_iter()
                            .map(|m| crate::models::ModelEntry {
                                id: m.id.clone(),
                                model_id: m.id,
                            })
                            .collect();
                        models.sort_by_key(|a| a.id.clone());
                        return Ok(models);
                    }
            }
            Err(_) => { /* fall through to config fallback */ }
        }
    }

    // Fallback: use statically configured models from config.
    let mut models: Vec<crate::models::ModelEntry> = provider
        .models
        .iter()
        .map(|m| crate::models::ModelEntry {
            id: m.id.clone(),
            model_id: m.model_id.clone(),
        })
        .collect();
    models.sort_by_key(|a| a.id.clone());
    Ok(models)
}


// ---------------------------------------------------------------------------
// Agent management (multi-agent P2) — LOCAL filesystem operations
// ---------------------------------------------------------------------------

/// Check whether config.yaml already has any agents registered, and skip
/// filesystem sync entirely if it does. This replaces the old per-directory
/// iteration with a single bulk check: once local agents exist, no more
/// auto-syncing from predefined or filesystem discovery.
fn sync_filesystem_agents_to_config(t: &Translator) -> Result<(), String> {
    let agents_dir = crate::agent_fs::agents_dir();
    if !agents_dir.exists() {
        return Ok(());
    }

    let aman_config = config::AmanConfig::from_default_path()
        .map_err(|e| t_with(t, "desktop.error.config_read", &[("detail", &e.to_string())]))?;

    // If config already has any agents, skip entirely — no per-agent checking.
    if !aman_config.agents.is_empty() {
        return Ok(());
    }

    // No agents in config yet — seed_builtin_agents (gateway side) handles
    // first-run setup. Nothing to discover here.
    Ok(())
}

#[tauri::command]
pub async fn list_agents(
    state: State<'_, AppState>,
) -> Result<Vec<crate::models::AgentEntry>, String> {
    let t = translator(&state);
    // Discover any agents manually copied into ~/.aman/agents/ before listing.
    sync_filesystem_agents_to_config(&t)?;

    let aman_config = config::AmanConfig::from_default_path()
        .map_err(|e| t_with(&t, "desktop.error.config_read", &[("detail", &e.to_string())]))?;

    let active_key = state.active_agent_key.lock().await.clone();

    let mut entries: Vec<crate::models::AgentEntry> = aman_config
        .agents
        .into_iter()
        .map(|(key, agent)| {
            let summary = crate::agent_fs::soul_summary(&key);
            // Count session JSONL files on disk (approximate, P4 will use sessions.db).
            let agent_dir = crate::agent_fs::agents_dir().join(&key).join("sessions");
            let session_count = if agent_dir.exists() {
                let mut count = 0u64;
                if let Ok(entries) = std::fs::read_dir(&agent_dir) {
                    for entry in entries.flatten() {
                        if entry.file_type().is_ok_and(|t| t.is_file()) {
                            count += 1;
                        }
                    }
                }
                count
            } else { 0 };

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
    entries.sort_by_key(|a| a.key.clone());
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
    let t = translator(&state);
    if !config::is_valid_identifier(&key) {
        return Err(t.translate("desktop.error.agent_key_invalid").to_owned());
    }

    // Validate provider exists.
    let mut aman_config = config::AmanConfig::from_default_path()
        .map_err(|e| t_with(&t, "desktop.error.config_read", &[("detail", &e.to_string())]))?;
    if !aman_config.providers.contains_key(&provider) {
        return Err(t_with(&t, "desktop.error.provider_not_found_create_first", &[("provider", &provider)]));
    }
    if aman_config.agents.contains_key(&key) {
        return Err(t_with(&t, "desktop.error.agent_exists", &[("key", &key)]));
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
            capabilities: Vec::new(),
            queue_max_size: 5,
        },
    );

    let path = default_config_path();
    aman_config.save(&path).map_err(|e| t_with(&t, "desktop.error.config_save", &[("detail", &e.to_string())]))?;

    // Notify the gateway runtime to reload this agent so the idle system picks it up.
    if let Ok(client) = require_gateway(&state).await {
        let _ = client.reload_agent(&key).await;
    }

    Ok(t_with(&t, "desktop.info.agent_created", &[("key", &key)]))
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
    let t = translator(&state);
    let mut aman_config = config::AmanConfig::from_default_path()
        .map_err(|e| t_with(&t, "desktop.error.config_read", &[("detail", &e.to_string())]))?;

    let agent = aman_config
        .agents
        .get_mut(&key)
        .ok_or_else(|| t_with(&t, "desktop.error.agent_not_found", &[("key", &key)]))?;

    if let Some(name) = display_name {
        agent.display_name = name;
    }
    if let Some(p) = provider {
        if !aman_config.providers.contains_key(&p) {
            return Err(t_with(&t, "desktop.error.provider_not_found", &[("key", &p)]));
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
    aman_config.save(&path).map_err(|e| t_with(&t, "desktop.error.config_save", &[("detail", &e.to_string())]))?;

    // Notify the gateway runtime to reload this agent so the idle system picks it up.
    if let Ok(client) = require_gateway(&state).await {
        let _ = client.reload_agent(&key).await;
    }

    // Write soul content separately if provided.
    if let Some(content) = soul_content {
        crate::agent_fs::write_soul(&key, &content)?;
    }

    Ok(t_with(&t, "desktop.info.agent_updated", &[("key", &key)]))
}

#[tauri::command]
pub async fn delete_agent(
    state: State<'_, AppState>,key: String,
) -> Result<String, String> {
    let t = translator(&state);
    let mut aman_config = config::AmanConfig::from_default_path()
        .map_err(|e| t_with(&t, "desktop.error.config_read", &[("detail", &e.to_string())]))?;

    if !aman_config.agents.contains_key(&key) {
        return Err(t_with(&t, "desktop.error.agent_not_found", &[("key", &key)]));
    }

    // Remove from config.
    aman_config.agents.remove(&key);

    let path = default_config_path();
    aman_config.save(&path).map_err(|e| t_with(&t, "desktop.error.config_save", &[("detail", &e.to_string())]))?;

    // Remove filesystem directory.
    let _ = crate::agent_fs::remove_agent_dir(&key);

    Ok(t_with(&t, "desktop.info.agent_deleted", &[("key", &key)]))
}

#[tauri::command]
pub async fn get_agent_soul(key: String) -> Result<String, String> {
    crate::agent_fs::read_soul(&key)
}

/// Read an agent's emotions configuration from `~/.aman/agents/{key}/emotions/`.
///
/// Returns `Ok(None)` when the emotions directory doesn't exist, `data.json`
/// can't be read, or any referenced image file is missing — the frontend
/// should fall back to the default emoji display.
///
/// Returns `Ok(Some(config))` when `data.json` is valid and all referenced
/// image files exist on disk.  Each [`EmotionEntry`] includes a base64-encoded
/// `data_url` so the frontend can render `<img>` tags directly with no
/// additional IPC round-trips.
#[tauri::command]
pub async fn get_agent_emotions(key: String) -> Result<Option<EmotionsConfig>, String> {
    use base64::Engine;
    let dir = crate::agent_fs::emotions_dir(&key);
    if !dir.exists() {
        return Ok(None);
    }

    let data_path = dir.join("data.json");
    let raw = match std::fs::read_to_string(&data_path) {
        Ok(r) => r,
        Err(_) => return Ok(None),
    };

    // Parse the JSON — we only need img_ext and the items array.
    let parsed: serde_json::Value =
        serde_json::from_str(&raw).map_err(|e| format!("emotions data.json parse error: {e}"))?;

    let img_ext = parsed["img_ext"]
        .as_str()
        .unwrap_or("png")
        .to_owned();

    let mime = match img_ext.as_str() {
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        _ => "image/png",
    };

    let raw_items = parsed["items"]
        .as_array()
        .cloned()
        .unwrap_or_default();

    if raw_items.is_empty() {
        return Ok(None);
    }

    let mut items: Vec<EmotionEntry> = Vec::with_capacity(raw_items.len());

    for item in &raw_items {
        let emotion_id = item["id"].as_str().unwrap_or("").to_owned();
        if emotion_id.is_empty() {
            return Ok(None); // malformed entry
        }

        let img_path = dir.join(format!("{}.{}", emotion_id, img_ext));
        let img_bytes = match std::fs::read(&img_path) {
            Ok(b) => b,
            Err(_) => {
                tracing::warn!(
                    "emotion image missing for agent '{key}': {}",
                    img_path.display()
                );
                return Ok(None);
            }
        };

        let b64 = base64::engine::general_purpose::STANDARD.encode(&img_bytes);
        let data_url = format!("data:{};base64,{}", mime, b64);

        items.push(EmotionEntry {
            id: emotion_id,
            tags: item["tags"]
                .as_array()
                .map(|t| {
                    t.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default(),
            description: item["description"]
                .as_str()
                .unwrap_or("")
                .to_owned(),
            data_url,
        });
    }

    Ok(Some(EmotionsConfig { img_ext, items }))
}

#[tauri::command]
pub async fn select_agent(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    key: String,
) -> Result<String, String> {
    let t = translator(&state);
    // Validate agent exists in config.
    let aman_config = config::AmanConfig::from_default_path()
        .map_err(|e| t_with(&t, "desktop.error.config_read", &[("detail", &e.to_string())]))?;
    if !aman_config.agents.contains_key(&key) {
        return Err(t_with(&t, "desktop.error.agent_not_found", &[("key", &key)]));
    }

    // Block selection of agents without a configured provider.
    if let Some(entry) = aman_config.agents.get(&key)
        && entry.provider.is_empty() {
            return Err(t_with(&t, "desktop.error.agent_no_provider", &[("key", &key)]));
        }

    let mut active = state.active_agent_key.lock().await;
    *active = Some(key.clone());
    drop(active);

    // Notify the UI so sidebar widgets can refresh.
    let _ = app.emit("agent:selected", serde_json::json!({ "key": key }));

    Ok(t_with(&t, "desktop.info.agent_activated", &[("key", &key)]))
}

#[tauri::command]
pub async fn get_active_agent(
    state: State<'_, AppState>,
) -> Result<Option<crate::models::AgentEntry>, String> {
    let t = translator(&state);
    let active_key = state.active_agent_key.lock().await.clone();
    let Some(key) = active_key else {
        return Ok(None);
    };

    let aman_config = config::AmanConfig::from_default_path()
        .map_err(|e| t_with(&t, "desktop.error.config_read", &[("detail", &e.to_string())]))?;

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
pub async fn get_aman_config(state: State<'_, AppState>) -> Result<config::AmanConfig, String> {
    let t = translator(&state);
    config::AmanConfig::from_default_path()
        .map_err(|e| t_with(&t, "desktop.error.config_read", &[("detail", &e.to_string())]))
}

#[tauri::command]
pub async fn get_secrets_mode(state: State<'_, AppState>) -> Result<String, String> {
    let t = translator(&state);
    let cfg = config::AmanConfig::from_default_path()
        .map_err(|e| t_with(&t, "desktop.error.config_read", &[("detail", &e.to_string())]))?;
    Ok(serde_json::to_string(&cfg.runtime.security.secrets_mode)
        .unwrap_or_else(|_| "\"env\"".to_owned()))
}

/// Return whether MCP (Model Context Protocol) server integration is enabled.
#[tauri::command]
pub async fn get_mcp_enabled(state: State<'_, AppState>) -> Result<bool, String> {
    let t = translator(&state);
    let cfg = config::AmanConfig::from_default_path()
        .map_err(|e| t_with(&t, "desktop.error.config_read", &[("detail", &e.to_string())]))?;
    Ok(cfg.runtime.mcp.enabled)
}

/// Return the current UI locale as `{ code: "en", display: "English" }`.
#[tauri::command]
pub async fn get_locale(state: State<'_, AppState>) -> Result<serde_json::Value, String> {
    let locale = state.locale;
    Ok(serde_json::json!({
        "code": locale.code(),
        "display": locale.display_name(),
    }))
}

/// Return the current UI visual style (`"frosted-glass"` | `"aurora"`).
#[tauri::command]
pub async fn get_ui_style(state: State<'_, AppState>) -> Result<String, String> {
    Ok(state.ui_style.to_string())
}

/// Return the current agents page viewer mode (`"grid"` | `"aoa-realm"`).
#[tauri::command]
pub async fn get_agents_viewer(state: State<'_, AppState>) -> Result<String, String> {
    Ok(state.agents_viewer.to_string())
}

#[tauri::command]
pub async fn has_any_provider(state: State<'_, AppState>) -> Result<bool, String> {
    let t = translator(&state);
    let config = config::AmanConfig::from_default_path()
        .map_err(|e| t_with(&t, "desktop.error.config_read", &[("detail", &e.to_string())]))?;
    Ok(!config.providers.is_empty())
}

#[tauri::command]
pub async fn has_any_agent(state: State<'_, AppState>) -> Result<bool, String> {
    let t = translator(&state);
    let config = config::AmanConfig::from_default_path()
        .map_err(|e| t_with(&t, "desktop.error.config_read", &[("detail", &e.to_string())]))?;
    Ok(!config.agents.is_empty())
}

#[tauri::command]
pub async fn get_default_model(state: State<'_, AppState>) -> Result<Option<config::DefaultModelConfig>, String> {
    let t = translator(&state);
    let config = config::AmanConfig::from_default_path()
        .map_err(|e| t_with(&t, "desktop.error.config_read", &[("detail", &e.to_string())]))?;
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
            system_state: item["system_state"].as_str().unwrap_or("idle").to_owned(),
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
        system_state: v["system_state"].as_str().unwrap_or("idle").to_owned(),
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
    tokio::task::spawn_blocking(move || {
        crate::code_agents::launch_code_agent(&command)
    })
    .await
    .map_err(|e| format!("Thread panicked: {e}"))?
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

// ---------------------------------------------------------------------------
// Plugin UI pages
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn get_plugin_pages(state: State<'_, AppState>) -> Result<Vec<serde_json::Value>, String> {
    let client = require_gateway(&state).await?;
    let resp = client.plugin_pages().await?;
    let pages = resp
        .as_array()
        .cloned()
        .unwrap_or_default();
    Ok(pages)
}

fn default_config_path() -> std::path::PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    std::path::PathBuf::from(home).join(".aman").join("config.yaml")
}

// ── MCP Server commands ────────────────────────────────────────────

/// List all MCP server definitions (global + per-agent), merged with runtime
/// connection status from the gateway (if running).
#[tauri::command]
pub async fn list_mcp_servers(
    state: tauri::State<'_, crate::state::AppState>,
) -> Result<Vec<crate::models::McpServerEntry>, String> {
    // 1. Load all server definitions from JSON files.
    let mut entries = crate::mcp_servers_fs::load_all_mcp_servers()?;

    // 2. If gateway is running, query runtime status for each agent.
    let guard = state.gateway_client.lock().await;
    if let Some(client) = guard.as_ref() {
        // Collect runtime status from gateway per agent.
        let mut seen_agents = std::collections::HashSet::new();
        // Map of (source, name) -> (connected, tool_count, error)
        let mut runtime_status: std::collections::HashMap<(String, String), (bool, usize, Option<String>)> = std::collections::HashMap::new();

        for entry in &entries {
            if entry.source != "global" && seen_agents.insert(entry.source.clone())
                && let Ok(statuses) = client.mcp_list_servers(&entry.source).await {
                    for status in &statuses {
                        if let Some(name) = status.get("name").and_then(|v| v.as_str()) {
                            let connected = status.get("connected")
                                .and_then(|v| v.as_bool())
                                .unwrap_or(false);
                            let tool_count = status.get("tool_count")
                                .and_then(|v| v.as_u64())
                                .unwrap_or(0) as usize;
                            let error = status.get("error")
                                .and_then(|v| v.as_str())
                                .map(|s| s.to_string());
                            runtime_status.insert(
                                (entry.source.clone(), name.to_string()),
                                (connected, tool_count, error),
                            );
                        }
                    }
                }
        }

        // Apply runtime status to entries.
        for e in &mut entries {
            if let Some((connected, tool_count, error)) =
                runtime_status.get(&(e.source.clone(), e.name.clone()))
            {
                e.connected = *connected;
                e.tool_count = *tool_count;
                e.error = error.clone();
            }
        }
    }

    Ok(entries)
}

/// Create a new MCP server definition (global or per-agent).
#[tauri::command]
#[allow(clippy::too_many_arguments)] // Tauri command mirrors the MCP config fields.
pub async fn create_mcp_server(
    state: State<'_, AppState>,
    name: String,
    transport: String,
    command: Option<String>,
    args: Vec<String>,
    url: Option<String>,
    env: std::collections::BTreeMap<String, String>,
    headers: std::collections::BTreeMap<String, String>,
    auto_connect: bool,
    agent_key: Option<String>,
) -> Result<String, String> {
    let t = translator(&state);
    if name.trim().is_empty() {
        return Err(t.translate("desktop.error.mcp_name_empty").to_string());
    }

    let config = crate::mcp_servers_fs::McpServerConfig {
        name: name.trim().to_string(),
        transport,
        command,
        args,
        url,
        env,
        headers,
        auto_connect,
    };

    crate::mcp_servers_fs::add_mcp_server(config, agent_key.as_deref())?;

    Ok(t_with(&t, "desktop.info.mcp_created", &[("name", name.trim())]))
}

/// Delete an MCP server definition.
#[tauri::command]
pub async fn delete_mcp_server(
    state: State<'_, AppState>,name: String,
    agent_key: Option<String>,
) -> Result<String, String> {
    let t = translator(&state);
    crate::mcp_servers_fs::remove_mcp_server(&name, agent_key.as_deref())?;
    Ok(t_with(&t, "desktop.info.mcp_deleted", &[("name", &name)]))
}

/// Connect an agent to an MCP server via the gateway.
#[tauri::command]
pub async fn connect_mcp_server(
    state: tauri::State<'_, crate::state::AppState>,
    agent_key: String,
    name: String,
) -> Result<String, String> {
    let t = translator(&state);
    let guard = state.gateway_client.lock().await;
    let client = guard.as_ref()
        .ok_or_else(|| t.translate("desktop.error.gateway_not_running").to_string())?;
    client.mcp_connect_server(&agent_key, &name).await
}

/// Disconnect an agent from an MCP server via the gateway.
#[tauri::command]
pub async fn disconnect_mcp_server(
    state: tauri::State<'_, crate::state::AppState>,
    agent_key: String,
    name: String,
) -> Result<String, String> {
    let t = translator(&state);
    let guard = state.gateway_client.lock().await;
    let client = guard.as_ref()
        .ok_or_else(|| t.translate("desktop.error.gateway_not_running").to_string())?;
    client.mcp_disconnect_server(&agent_key, &name).await
}

/// List all agent keys (for UI dropdown).
#[tauri::command]
pub async fn list_agent_keys() -> Result<Vec<String>, String> {
    Ok(crate::agent_fs::list_agent_dirs())
}

// ---------------------------------------------------------------------------
// Agent windows — multi-window management
// ---------------------------------------------------------------------------

/// Sanitize an agent key for use as a Tauri window label.
/// Labels must match `^[a-zA-Z0-9-_:]+$`; replace anything else with `-`.
fn sanitize_label(key: &str) -> String {
    key.chars()
        .map(|c| if c.is_alphanumeric() || c == '-' || c == '_' || c == ':' { c } else { '-' })
        .collect()
}

/// Open a per-agent window, or focus it if it already exists.
///
/// Each agent gets a single OS window (`agent-{key}`) with a transparent
/// background and a two-column layout (avatar | tabs).  Repeated calls with
/// the same key bring the existing window to front instead of opening a
/// duplicate.
#[tauri::command]
pub async fn open_or_focus_agent_window(
    app: tauri::AppHandle,
    agent_key: String,
    display_name: String,
) -> Result<(), String> {
    let label = format!("agent-{}", sanitize_label(&agent_key));

    // If the window already exists, focus it (unminimize + bring to front).
    if let Some(window) = app.get_webview_window(&label) {
        tracing::info!(window = %label, "focusing existing agent window");
        window.unminimize().map_err(|e| format!("unminimize failed: {e}"))?;
        window.set_focus().map_err(|e| format!("set_focus failed: {e}"))?;
        return Ok(());
    }

    tracing::info!(window = %label, agent = %agent_key, "creating agent window");

    // The window loads the default frontend (index.html). The agent key is
    // encoded in the window label (`agent-{key}`) and read by the frontend
    // via `getCurrentWindow().label` — no custom URL construction needed
    // (Tauri's `WebviewUrl` enum is not publicly constructible).
    // NOTE: We deliberately do NOT use `transparent(true)` or apply
    // window_vibrancy here.  Vibrancy requires main-thread access, but
    // Tauri commands run on a tokio worker thread.  dispatch_sync from a
    // tokio thread deadlocks against Tauri's main run loop, and
    // dispatch_async runs *after* the window is already visible (black
    // title bar flash).  The main window works because its vibrancy is
    // applied in `setup()` which runs on the main thread.
    //
    // Instead we let macOS draw its standard dark-mode title bar
    // (unified toolbar appearance) and match our content background to
    // it so the transition is seamless.
    tauri::webview::WebviewWindowBuilder::new(
        &app,
        &label,
        tauri::WebviewUrl::App("index.html".into()),
    )
    .title(format!("{display_name}"))
    .inner_size(880.0, 620.0)
    .min_inner_size(600.0, 420.0)
    .decorations(true)
    .center()
    .build()
    .map_err(|e| format!("failed to create window: {e}"))?;

    tracing::info!(window = %label, "agent window created");
    Ok(())
}

/// Close a per-agent window (used by the agent page's own close button).
#[tauri::command]
pub async fn close_agent_window(
    app: tauri::AppHandle,
    agent_key: String,
) -> Result<(), String> {
    let label = format!("agent-{}", sanitize_label(&agent_key));
    if let Some(window) = app.get_webview_window(&label) {
        window.close().map_err(|e| format!("close failed: {e}"))?;
    }
    Ok(())
}
