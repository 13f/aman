use crate::models::{
    ChatMessageEntry, ChatSessionInfo, ChatSessionState,
    DlqEntry, MetricsSnapshot, PluginEntry, PluginHealthEntry, QueueDepth, RuntimeConfigInfo,
    RuntimeStatusInfo,
    SkillEntry, SoulInfo, WorkflowEntry,
};
use crate::state::AppState;
use config::ConfigLoader;
use kernel::event::{Event, EventType};
use persistence::DeadLetterQueue;
use runtime::{AgentRuntimeBuilder, RuntimePhase};
use std::time::Instant;
use tauri::State;

/// Record IPC command metrics from an optional runtime reference.
fn record_ipc(rt: Option<&runtime::AgentRuntime>, cmd: &str, ok: bool, dur_ms: f64) {
    if let Some(r) = rt {
        r.metrics().inc_ipc_command(cmd, if ok { "ok" } else { "error" });
        r.metrics().observe_ipc_duration(cmd, dur_ms);
    }
}

// ---------------------------------------------------------------------------
// Runtime lifecycle
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn get_runtime_status(state: State<'_, AppState>) -> Result<RuntimeStatusInfo, String> {
    let guard = state.runtime.lock().await;
    match guard.as_ref() {
        Some(rt) => Ok(RuntimeStatusInfo {
            phase: format!("{:?}", rt.phase()),
            ready: rt.is_ready(),
            live: rt.is_live(),
            running: rt.phase() != RuntimePhase::Phase0,
        }),
        None => Ok(RuntimeStatusInfo {
            phase: "stopped".to_owned(),
            ready: false,
            live: false,
            running: false,
        })
    }
}

#[tauri::command]
pub async fn get_runtime_config(state: State<'_, AppState>) -> Result<RuntimeConfigInfo, String> {
    let guard = state.runtime.lock().await;
    match guard.as_ref() {
        Some(rt) => Ok(RuntimeConfigInfo {
            runtime_dir: Some(rt.runtime_dir().display().to_string()),
            bind_addr: Some(rt.bind_addr().to_string()),
            has_api_token: rt.api_token().is_some(),
            risky_enabled: rt.risky_capabilities_enabled(),
            skills_dir: Some(rt.skills_dir().display().to_string()),
        }),
        None => Ok(RuntimeConfigInfo {
            runtime_dir: None,
            bind_addr: None,
            has_api_token: false,
            risky_enabled: false,
            skills_dir: None,
        }),
    }
}

#[tauri::command]
pub async fn start_runtime(
    state: State<'_, AppState>,
    config_path: Option<String>,
) -> Result<String, String> {
    let mut guard = state.runtime.lock().await;
    if guard.is_some() {
        return Err("Runtime already running".to_owned());
    }

    let path = config_path.unwrap_or_else(|| {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_owned());
        format!("{home}/.aman/config.yaml")
    });
    let path = std::path::Path::new(&path);

    // Auto-create default config file if missing
    if !path.exists() {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("Create config dir error: {e}"))?;
        }
        let default_config = r#"# Aman Agent Framework — default config
runtime:
  drain_timeout_sec: 30
  tool_timeout_sec: 60
"#;
        std::fs::write(path, default_config).map_err(|e| format!("Write default config error: {e}"))?;
    }

    let load_result =
        ConfigLoader::load(Some(path), None).map_err(|e| format!("Config load error: {e}"))?;
    let rt = AgentRuntimeBuilder::new(load_result.config)
        .build()
        .map_err(|e| format!("Runtime build error: {e}"))?;
    rt.start()
        .await
        .map_err(|e| format!("Runtime start error: {e}"))?;

    *guard = Some(rt);
    Ok("Runtime started".to_owned())
}

#[tauri::command]
pub async fn stop_runtime(state: State<'_, AppState>) -> Result<String, String> {
    let mut guard = state.runtime.lock().await;
    match guard.take() {
        Some(rt) => {
            rt.shutdown()
                .await
                .map_err(|e| format!("Shutdown error: {e}"))?;
            Ok("Runtime stopped".to_owned())
        }
        None => Err("No runtime running".to_owned()),
    }
}

// ---------------------------------------------------------------------------
// Metrics
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn get_metrics(state: State<'_, AppState>) -> Result<MetricsSnapshot, String> {
    let guard = state.runtime.lock().await;
    let rt = guard
        .as_ref()
        .ok_or_else(|| "No runtime running".to_owned())?;

    let bus = rt.bus_metrics();
    let dlq_depth = rt.dlq().depth();
    let loader = rt.plugin_loader().await;
    let plugin_health: Vec<PluginHealthEntry> = loader
        .loaded_plugins()
        .into_iter()
        .map(|name| {
            let status = match loader.state_of(&name) {
                Some(s) => format!("{s:?}"),
                None => "unknown".to_owned(),
            };
            PluginHealthEntry { name, status }
        })
        .collect();

    Ok(MetricsSnapshot {
        queue_depth: QueueDepth {
            high: bus.queue_depth.high as i64,
            normal: bus.queue_depth.normal as i64,
            low: bus.queue_depth.low as i64,
        },
        throughput: bus.throughput,
        discarded: bus.discarded_count,
        duplicate: bus.duplicate_count,
        subscription_count: bus.subscription_count as i64,
        retry_queue_depth: bus.retry_queue_depth as i64,
        dlq_depth,
        inflight_pipelines: rt.inflight_pipelines(),
        inflight_skills: rt.inflight_skills(),
        backpressure_level: format!("{:?}", bus.backpressure_level),
        plugin_health,
    })
}

// ---------------------------------------------------------------------------
// Skills
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn list_skills(state: State<'_, AppState>) -> Result<Vec<SkillEntry>, String> {
    let guard = state.runtime.lock().await;
    let rt = guard
        .as_ref()
        .ok_or_else(|| "No runtime running".to_owned())?;

    let items = rt.skills().list();
    Ok(items
        .into_iter()
        .map(|s| SkillEntry {
            name: s.name,
            version: s.version,
            description: s.description,
            enabled: s.enabled,
            concurrency: format!("{:?}", s.concurrency),
            triggers: s
                .triggers
                .into_iter()
                .map(|t| crate::models::TriggerInfo {
                    event_types: t.event_types.into_iter().map(|et| format!("{et:?}")).collect(),
                    sources: t.sources.into_iter().map(|s| s.to_string()).collect(),
                    priorities: t.priorities.into_iter().map(|p| format!("{p:?}")).collect(),
                    match_all: t.match_all,
                })
                .collect(),
        })
        .collect())
}

#[tauri::command]
pub async fn reload_skills(state: State<'_, AppState>) -> Result<String, String> {
    let guard = state.runtime.lock().await;
    let rt = guard
        .as_ref()
        .ok_or_else(|| "No runtime running".to_owned())?;
    rt.reload_skills_now().map_err(|e| e.to_string())?;
    Ok("Skills reloaded".to_owned())
}

#[tauri::command]
pub async fn enable_skill(state: State<'_, AppState>, name: String) -> Result<String, String> {
    let guard = state.runtime.lock().await;
    let rt = guard
        .as_ref()
        .ok_or_else(|| "No runtime running".to_owned())?;
    rt.skills().enable(&name).map_err(|e| e.to_string())?;
    Ok(format!("Skill '{name}' enabled"))
}

#[tauri::command]
pub async fn disable_skill(state: State<'_, AppState>, name: String) -> Result<String, String> {
    let guard = state.runtime.lock().await;
    let rt = guard
        .as_ref()
        .ok_or_else(|| "No runtime running".to_owned())?;
    rt.skills().disable(&name).map_err(|e| e.to_string())?;
    Ok(format!("Skill '{name}' disabled"))
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
    let guard = state.runtime.lock().await;
    let rt = guard
        .as_ref()
        .ok_or_else(|| "No runtime running".to_owned())?;
    let event = Event::new(source, EventType::from(event_type), payload);
    let id = event.id.to_string();
    rt.publish_event(event)
        .await
        .map_err(|e| e.to_string())?;
    Ok(id)
}

#[tauri::command]
pub async fn get_event_trace(
    state: State<'_, AppState>,
    trace_id: String,
) -> Result<serde_json::Value, String> {
    let guard = state.runtime.lock().await;
    let rt = guard
        .as_ref()
        .ok_or_else(|| "No runtime running".to_owned())?;
    let events = rt.event_store().trace(&trace_id);
    if events.is_empty() {
        return Err("Trace not found".to_owned());
    }
    serde_json::to_value(events).map_err(|e| e.to_string())
}

// ---------------------------------------------------------------------------
// Workflow
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn get_workflow_instances(
    state: State<'_, AppState>,
) -> Result<Vec<WorkflowEntry>, String> {
    let guard = state.runtime.lock().await;
    let rt = guard
        .as_ref()
        .ok_or_else(|| "No runtime running".to_owned())?;

    let instances = rt.workflow_engine().list_instances();
    Ok(instances
        .into_iter()
        .map(|inst| {
            let running = inst.last_active_state.is_none();
            WorkflowEntry {
                id: inst.id,
                workflow_name: inst.workflow_name,
                current_state: inst.current_state,
                status: if running { "running".to_owned() } else { inst.last_active_state.unwrap_or_default() },
            }
        })
        .collect())
}

#[tauri::command]
pub async fn retry_workflow(state: State<'_, AppState>, id: String) -> Result<String, String> {
    let guard = state.runtime.lock().await;
    let rt = guard
        .as_ref()
        .ok_or_else(|| "No runtime running".to_owned())?;
    let event = Event::new(
        "tauri:control",
        EventType::Custom("retry".to_owned()),
        serde_json::json!({}),
    );
    rt.workflow_engine()
        .handle_event(&id, event)
        .await
        .map_err(|e| e.to_string())?;
    Ok("Workflow retried".to_owned())
}

#[tauri::command]
pub async fn cancel_workflow(state: State<'_, AppState>, id: String) -> Result<String, String> {
    let guard = state.runtime.lock().await;
    let rt = guard
        .as_ref()
        .ok_or_else(|| "No runtime running".to_owned())?;
    let event = Event::new(
        "tauri:control",
        EventType::Custom("cancel".to_owned()),
        serde_json::json!({}),
    );
    rt.workflow_engine()
        .handle_event(&id, event)
        .await
        .map_err(|e| e.to_string())?;
    Ok("Workflow cancelled".to_owned())
}

#[tauri::command]
pub async fn get_workflow_def(
    state: State<'_, AppState>,
    name: String,
) -> Result<crate::models::WorkflowDefInfo, String> {
    let guard = state.runtime.lock().await;
    let rt = guard
        .as_ref()
        .ok_or_else(|| "No runtime running".to_owned())?;

    let def = rt
        .workflow_engine()
        .get_workflow(&name)
        .ok_or_else(|| format!("Workflow '{name}' not found"))?;

    Ok(crate::models::WorkflowDefInfo {
        name: def.name,
        states: def.states.into_iter().map(|s| s.name).collect(),
        initial_state: def.initial_state,
        final_states: def.final_states,
        error_state: def.error_state,
        transitions: def
            .transitions
            .into_iter()
            .map(|t| crate::models::TransitionInfo {
                from: match t.from {
                    workflow::TransitionFrom::Specific(s) => s,
                    workflow::TransitionFrom::Any => "__ANY__".to_owned(),
                },
                event: t.event,
                to: match t.to {
                    workflow::TransitionTo::Specific(s) => s,
                    workflow::TransitionTo::LastActiveState => "__LAST__".to_owned(),
                },
                guard: t.guard,
                has_action: t.action.is_some(),
            })
            .collect(),
        state_timeouts: def
            .state_timeouts
            .into_iter()
            .map(|st| crate::models::StateTimeoutInfo {
                state: st.state,
                timeout_ms: st.timeout_ms,
                on_timeout: match st.on_timeout {
                    workflow::TransitionTo::Specific(s) => s,
                    workflow::TransitionTo::LastActiveState => "__LAST__".to_owned(),
                },
            })
            .collect(),
    })
}

// ---------------------------------------------------------------------------
// SOUL
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn get_soul_info(state: State<'_, AppState>) -> Result<SoulInfo, String> {
    let guard = state.runtime.lock().await;
    let rt = guard
        .as_ref()
        .ok_or_else(|| "No runtime running".to_owned())?;

    match rt.soul_runtime() {
        Some(soul_rt) => {
            let soul = soul_rt.current_soul();
            let last = soul_rt.last_soul_changed_event();
            Ok(SoulInfo {
                current_soul: Some(soul.name.clone()),
                last_changed: last.map(|e| e.id.to_string()),
            })
        }
        None => Ok(SoulInfo {
            current_soul: None,
            last_changed: None,
        }),
    }
}

#[tauri::command]
pub async fn preview_system_prompt(state: State<'_, AppState>) -> Result<String, String> {
    let guard = state.runtime.lock().await;
    let rt = guard
        .as_ref()
        .ok_or_else(|| "No runtime running".to_owned())?;

    match rt.soul_runtime() {
        Some(soul_rt) => {
            let soul = soul_rt.current_soul();
            Ok(soul.to_system_prompt())
        }
        None => Err("No SOUL configured".to_owned()),
    }
}

#[tauri::command]
pub async fn update_soul(state: State<'_, AppState>, content: String) -> Result<String, String> {
    let guard = state.runtime.lock().await;
    let rt = guard
        .as_ref()
        .ok_or_else(|| "No runtime running".to_owned())?;
    rt.update_soul(&content)
        .await
        .map_err(|e| e.to_string())?;
    Ok("SOUL updated".to_owned())
}

#[tauri::command]
pub async fn get_soul_raw(state: State<'_, AppState>) -> Result<String, String> {
    let guard = state.runtime.lock().await;
    let rt = guard
        .as_ref()
        .ok_or_else(|| "No runtime running".to_owned())?;

    match rt.soul_runtime() {
        Some(soul_rt) => {
            let soul = soul_rt.current_soul();
            Ok(soul.raw.clone())
        }
        None => Err("No SOUL configured".to_owned()),
    }
}

// ---------------------------------------------------------------------------
// Plugins
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn list_plugins(state: State<'_, AppState>) -> Result<Vec<PluginEntry>, String> {
    let guard = state.runtime.lock().await;
    let rt = guard
        .as_ref()
        .ok_or_else(|| "No runtime running".to_owned())?;

    let loader = rt.plugin_loader().await;
    let names: Vec<String> = loader.loaded_plugins();
    let mut items: Vec<PluginEntry> = names
        .into_iter()
        .map(|name| {
            let pstate = loader.state_of(&name);
            PluginEntry {
                name: name.clone(),
                version: None,
                loaded: true,
                state: pstate.map(|s| format!("{s:?}")),
                enabled: !loader.is_unstable(&name),
            }
        })
        .collect();
    items.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(items)
}

#[tauri::command]
pub async fn get_capabilities(state: State<'_, AppState>) -> Result<Vec<crate::models::CapabilityEntry>, String> {
    let guard = state.runtime.lock().await;
    let rt = guard
        .as_ref()
        .ok_or_else(|| "No runtime running".to_owned())?;
    let entries = rt.get_capability_entries().await;
    let mut result: Vec<crate::models::CapabilityEntry> = entries
        .into_values()
        .flat_map(|entries| {
            entries.into_iter().map(|e| crate::models::CapabilityEntry {
                capability: e.capability,
                plugin: e.plugin,
                version: e.version,
                status: format!("{:?}", e.status),
            })
        })
        .collect();
    result.sort_by(|a, b| a.capability.cmp(&b.capability));
    Ok(result)
}

#[tauri::command]
pub async fn enable_plugin(state: State<'_, AppState>, name: String) -> Result<String, String> {
    let guard = state.runtime.lock().await;
    let rt = guard
        .as_ref()
        .ok_or_else(|| "No runtime running".to_owned())?;
    rt.enable_plugin(&name)
        .await
        .map_err(|e| e.to_string())?;
    Ok(format!("Plugin {name} enabled"))
}

#[tauri::command]
pub async fn disable_plugin(state: State<'_, AppState>, name: String) -> Result<String, String> {
    let guard = state.runtime.lock().await;
    let rt = guard
        .as_ref()
        .ok_or_else(|| "No runtime running".to_owned())?;
    rt.disable_plugin(&name)
        .await
        .map_err(|e| e.to_string())?;
    Ok(format!("Plugin {name} disabled"))
}

// ---------------------------------------------------------------------------
// DLQ
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn list_dlq(state: State<'_, AppState>) -> Result<Vec<DlqEntry>, String> {
    let guard = state.runtime.lock().await;
    let rt = guard
        .as_ref()
        .ok_or_else(|| "No runtime running".to_owned())?;

    let items = rt.dlq().list(Default::default()).map_err(|e| e.to_string())?;
    Ok(items
        .into_iter()
        .map(|e| DlqEntry {
            id: e.id,
            event_source: e.event.source.to_string(),
            event_type: format!("{:?}", e.event.event_type),
            reason: e.reason,
            retry_count: e.retry_count,
            enqueued_at_ms: e.enqueued_at.as_millis(),
        })
        .collect())
}

#[tauri::command]
pub async fn retry_dlq(state: State<'_, AppState>, id: String) -> Result<String, String> {
    let guard = state.runtime.lock().await;
    let rt = guard
        .as_ref()
        .ok_or_else(|| "No runtime running".to_owned())?;
    let event = rt
        .dlq()
        .retry(&id, "tauri", "manual retry from dashboard")
        .map_err(|e| e.to_string())?;
    rt.publish_event(event)
        .await
        .map_err(|e| e.to_string())?;
    Ok("DLQ entry retried".to_owned())
}

#[tauri::command]
pub async fn discard_dlq(state: State<'_, AppState>, id: String) -> Result<String, String> {
    let guard = state.runtime.lock().await;
    let rt = guard
        .as_ref()
        .ok_or_else(|| "No runtime running".to_owned())?;
    rt.dlq().discard(&id).map_err(|e| e.to_string())?;
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
    let _start = Instant::now();
    let guard = state.runtime.lock().await;
    let rt = guard
        .as_ref()
        .ok_or_else(|| "No runtime running".to_owned())?;

    if session_id.trim().is_empty() {
        return Err("Session ID cannot be empty".to_owned());
    }

    let event = kernel::event::Event::new(
        "chat-platform:tauri-desktop",
        kernel::event::EventType::Custom("session_stop".to_owned()),
        serde_json::json!({
            "session_id": &session_id,
            "requested_at": std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis())
                .unwrap_or(0),
        }),
    );
    rt.publish_event(event)
        .await
        .map_err(|e| e.to_string())?;

    let result = Ok(session_id);
    record_ipc(Some(rt), "chat:stop_generation", result.is_ok(), _start.elapsed().as_secs_f64() * 1000.0);
    result
}

#[tauri::command]
pub async fn chat_send_message(
    state: State<'_, AppState>,
    text: String,
    session_id: String,
) -> Result<String, String> {
    let _start = Instant::now();
    let result = (|| async {
        let guard = state.runtime.lock().await;
        let rt = guard
            .as_ref()
            .ok_or_else(|| "No runtime running".to_owned())?;

        // Check chat capability
        if !rt.has_capability("chat").await {
            return Err("Chat capability not available".to_owned());
        }

        // Validate message length (chat-source default max is 4096 chars)
        let len = text.chars().count();
        if len > 4096 {
            return Err(format!(
                "Message exceeds maximum length of 4096 characters (got {len})"
            ));
        }
        if text.trim().is_empty() {
            return Err("Message cannot be empty".to_owned());
        }
        if session_id.trim().is_empty() {
            return Err("Session ID cannot be empty".to_owned());
        }

        // Publish MESSAGE_RECEIVED event to the Event Bus
        let event = kernel::event::Event::new(
            "chat-platform:tauri-desktop",
            kernel::event::EventType::MessageReceived,
            serde_json::json!({
                "session_id": session_id,
                "text": text,
                "channel": "tauri_desktop",
                "message_id": uuid::Uuid::now_v7(),
                "client_timestamp": std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_millis())
                    .unwrap_or(0),
            }),
        );
        let event_id = event.id;
        rt.publish_event(event)
            .await
            .map_err(|e| e.to_string())?;

        Ok::<_, String>(event_id.to_string())
    })()
    .await;

    let _dur_ms = _start.elapsed().as_secs_f64() * 1000.0;
    if let Ok(guard) = state.runtime.try_lock() {
        if let Some(rt) = guard.as_ref() {
            record_ipc(Some(rt), "chat:send_message", result.is_ok(), _dur_ms);
        }
    }
    result
}

#[tauri::command]
pub async fn chat_session_list(
    state: State<'_, AppState>,
) -> Result<Vec<ChatSessionInfo>, String> {
    let _start = Instant::now();
    let guard = state.runtime.lock().await;
    let rt = guard
        .as_ref()
        .ok_or_else(|| "No runtime running".to_owned())?;

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);

    let instances = rt.workflow_engine().list_instances();
    let sessions: Vec<ChatSessionInfo> = instances
        .into_iter()
        .filter(|inst| inst.workflow_name == "chat-session")
        .map(|inst| {
            let created = inst
                .data
                .get("created_at")
                .and_then(|v| v.as_i64())
                .unwrap_or(now);
            let last_active = inst
                .data
                .get("last_active_at")
                .and_then(|v| v.as_i64());
            ChatSessionInfo {
                id: inst.id,
                state: inst.current_state,
                message_count: 0, // computed on request or cached
                created_at: created,
                last_active_at: last_active,
            }
        })
        .collect();
    let result = Ok(sessions);
    record_ipc(Some(rt), "chat:session_list", result.is_ok(), _start.elapsed().as_secs_f64() * 1000.0);
    result
}

#[tauri::command]
pub async fn chat_session_create(
    state: State<'_, AppState>,
) -> Result<String, String> {
    let _start = Instant::now();
    let guard = state.runtime.lock().await;
    let rt = guard
        .as_ref()
        .ok_or_else(|| "No runtime running".to_owned())?;

    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);

    let instance = rt
        .workflow_engine()
        .create_instance(
            "chat-session",
            serde_json::json!({
                "created_at": now_ms,
                "last_active_at": now_ms,
            }),
        )
        .map_err(|e| e.to_string())?;
    let result = Ok(instance.id);
    record_ipc(Some(rt), "chat:session_create", result.is_ok(), _start.elapsed().as_secs_f64() * 1000.0);
    result
}

#[tauri::command]
pub async fn chat_session_close(
    state: State<'_, AppState>,
    session_id: String,
) -> Result<String, String> {
    let _start = Instant::now();
    let guard = state.runtime.lock().await;
    let rt = guard
        .as_ref()
        .ok_or_else(|| "No runtime running".to_owned())?;

    if session_id.trim().is_empty() {
        return Err("Session ID cannot be empty".to_owned());
    }

    let event = kernel::event::Event::new(
        "tauri:control",
        kernel::event::EventType::Custom("SESSION_CLOSE_CMD".to_owned()),
        serde_json::json!({ "session_id": &session_id }),
    );
    rt.workflow_engine()
        .handle_event(&session_id, event)
        .await
        .map_err(|e| e.to_string())?;

    let result = Ok(session_id);
    record_ipc(Some(rt), "chat:session_close", result.is_ok(), _start.elapsed().as_secs_f64() * 1000.0);
    result
}

#[tauri::command]
pub async fn chat_session_history(
    state: State<'_, AppState>,
    session_id: String,
    limit: Option<usize>,
) -> Result<Vec<ChatMessageEntry>, String> {
    let _start = Instant::now();
    let guard = state.runtime.lock().await;
    let rt = guard
        .as_ref()
        .ok_or_else(|| "No runtime running".to_owned())?;

    if session_id.trim().is_empty() {
        return Err("Session ID cannot be empty".to_owned());
    }

    let max = limit.unwrap_or(200).min(1000);
    let events = rt.event_store().recent(max);
    let mut messages: Vec<ChatMessageEntry> = events
        .into_iter()
        .filter(|e| {
            e.payload
                .get("session_id")
                .and_then(|v| v.as_str())
                .is_some_and(|sid| sid == session_id)
        })
        .map(|e| ChatMessageEntry {
            id: e.id.to_string(),
            event_type: e.event_type.as_str().to_owned(),
            payload: e.payload,
            timestamp: e.timestamp.as_millis(),
            trace_id: e.metadata.trace_id.to_string(),
        })
        .collect();
    messages.reverse(); // chronological order
    let result = Ok(messages);
    record_ipc(Some(rt), "chat:session_history", result.is_ok(), _start.elapsed().as_secs_f64() * 1000.0);
    result
}

#[tauri::command]
pub async fn chat_session_state(
    state: State<'_, AppState>,
    session_id: String,
) -> Result<ChatSessionState, String> {
    let _start = Instant::now();
    let guard = state.runtime.lock().await;
    let rt = guard
        .as_ref()
        .ok_or_else(|| "No runtime running".to_owned())?;

    if session_id.trim().is_empty() {
        return Err("Session ID cannot be empty".to_owned());
    }

    let instance = rt
        .workflow_engine()
        .get_instance(&session_id)
        .ok_or_else(|| format!("Session not found: {session_id}"))?;

    let events = rt.event_store().recent(200);
    let mut messages: Vec<ChatMessageEntry> = events
        .into_iter()
        .filter(|e| {
            e.payload
                .get("session_id")
                .and_then(|v| v.as_str())
                .is_some_and(|sid| sid == session_id)
        })
        .map(|e| ChatMessageEntry {
            id: e.id.to_string(),
            event_type: e.event_type.as_str().to_owned(),
            payload: e.payload,
            timestamp: e.timestamp.as_millis(),
            trace_id: e.metadata.trace_id.to_string(),
        })
        .collect();
    messages.reverse();

    let result = Ok(ChatSessionState {
        session_id: instance.id,
        state: instance.current_state,
        retry_count: instance.total_retry_count,
        messages,
    });
    record_ipc(Some(rt), "chat:session_state", result.is_ok(), _start.elapsed().as_secs_f64() * 1000.0);
    result
}

#[tauri::command]
pub async fn chat_retry_last(
    state: State<'_, AppState>,
    session_id: String,
) -> Result<String, String> {
    let _start = Instant::now();
    let guard = state.runtime.lock().await;
    let rt = guard
        .as_ref()
        .ok_or_else(|| "No runtime running".to_owned())?;

    if session_id.trim().is_empty() {
        return Err("Session ID cannot be empty".to_owned());
    }

    let event = kernel::event::Event::new(
        "tauri:control",
        kernel::event::EventType::Custom("RETRY_CMD".to_owned()),
        serde_json::json!({ "session_id": &session_id }),
    );
    rt.workflow_engine()
        .handle_event(&session_id, event)
        .await
        .map_err(|e| e.to_string())?;

    let result = Ok(session_id);
    record_ipc(Some(rt), "chat:retry_last", result.is_ok(), _start.elapsed().as_secs_f64() * 1000.0);
    result
}

#[tauri::command]
pub async fn chat_edit_message(
    state: State<'_, AppState>,
    session_id: String,
    message_id: String,
    text: String,
) -> Result<String, String> {
    let _start = Instant::now();
    let guard = state.runtime.lock().await;
    let rt = guard
        .as_ref()
        .ok_or_else(|| "No runtime running".to_owned())?;

    if session_id.trim().is_empty() {
        return Err("Session ID cannot be empty".to_owned());
    }
    if text.trim().is_empty() {
        return Err("Edited message cannot be empty".to_owned());
    }

    let event = kernel::event::Event::new(
        "tauri:control",
        kernel::event::EventType::Custom("MESSAGE_EDITED".to_owned()),
        serde_json::json!({
            "session_id": session_id,
            "message_id": message_id,
            "text": text,
            "edited_at": std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis())
                .unwrap_or(0),
        }),
    );
    let event_id = event.id;
    rt.publish_event(event)
        .await
        .map_err(|e| e.to_string())?;

    let result = Ok(event_id.to_string());
    record_ipc(Some(rt), "chat:edit_message", result.is_ok(), _start.elapsed().as_secs_f64() * 1000.0);
    result
}
