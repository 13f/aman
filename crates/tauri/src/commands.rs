use crate::models::{
    ChatMessageEntry, ChatSessionInfo, ChatSessionState,
    DlqEntry, MetricsSnapshot, PluginEntry, PluginHealthEntry, QueueDepth, RuntimeConfigInfo,
    RuntimeStatusInfo,
    SkillEntry, SoulInfo, WorkflowEntry,
};
use crate::state::AppState;
use config::ConfigLoader;
use kernel::event::{Event, EventType};
use kernel::sanitizer::{InputSanitizer, SanitizeResult, content_hash};
use persistence::DeadLetterQueue;
use runtime::{AgentRuntimeBuilder, RuntimePhase};
use secret::{KeychainBackend, SecretBackend};
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
pub async fn get_debug_events(
    state: State<'_, AppState>,
    limit: Option<usize>,
) -> Result<Vec<serde_json::Value>, String> {
    let guard = state.runtime.lock().await;
    let rt = guard
        .as_ref()
        .ok_or_else(|| "No runtime running".to_owned())?;
    let events = rt.event_store().recent(limit.unwrap_or(50));
    Ok(events
        .into_iter()
        .filter_map(|e| serde_json::to_value(&e).ok())
        .collect())
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
    expected_version: Option<u64>,
    trace_prev: Option<String>,
) -> Result<String, String> {
    let _start = Instant::now();

    // --- Rate limiting check (user-level: 10 msg / 60s sliding window, §4.5) ---
    let user_key = session_id.clone();
    if let Err(err) = state.rate_limiter.allow(&user_key) {
        let dur_ms = _start.elapsed().as_secs_f64() * 1000.0;
        if let Ok(guard) = state.runtime.try_lock() {
            if let Some(rt) = guard.as_ref() {
                record_ipc(Some(rt), "chat:send_message", false, dur_ms);
            }
        }
        return Err(format!("429:{}", err.message));
    }

    let result = (|| async {
        let guard = state.runtime.lock().await;
        let rt = guard
            .as_ref()
            .ok_or_else(|| "No runtime running".to_owned())?;

        // Check chat capability
        if !rt.has_capability("chat").await {
            eprintln!("[diag] chat_send_message: no chat capability for session={session_id}");
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

        // --- Optimistic lock version check (§11.3) ---
        let instance = rt
            .workflow_engine()
            .get_instance(&session_id)
            .ok_or_else(|| format!("Session not found: {session_id}"))?;
        check_session_version(&instance, expected_version)?;
        drop(instance); // release borrow before publish

        // --- InputSanitizer (§8.1): three-tier sanitization ---
        let sanitizer = InputSanitizer::new();
        let sanitize_result = sanitizer.sanitize(&text);
        let (final_text, sanitized, original_text) = match &sanitize_result {
            SanitizeResult::Block { matched_patterns } => {
                rt.audit().record(
                    "system",
                    "input_sanitize.block",
                    "chat:send_message",
                    "blocked",
                    format!(
                        "session_id={session_id}, matched_patterns={}, original_content_hash={}",
                        matched_patterns.join(","),
                        content_hash(&text),
                    ),
                );
                return Err(format!(
                    "Message blocked by InputSanitizer: matched {}",
                    matched_patterns.join(", "),
                ));
            }
            SanitizeResult::ReplaceMessage { matched_patterns } => {
                rt.audit().record(
                    "system",
                    "input_sanitize.replace_message",
                    "chat:send_message",
                    "replaced",
                    format!(
                        "session_id={session_id}, matched_patterns={}, original_content_hash={}",
                        matched_patterns.join(","),
                        content_hash(&text),
                    ),
                );
                ("[redacted]".to_owned(), true, text.clone())
            }
            SanitizeResult::ReplaceToken { sanitized, matched_patterns } => {
                rt.audit().record(
                    "system",
                    "input_sanitize.replace_token",
                    "chat:send_message",
                    "replaced",
                    format!(
                        "session_id={session_id}, matched_patterns={}, original_content_hash={}, sanitized_length={}",
                        matched_patterns.join(","),
                        content_hash(&text),
                        sanitized.len(),
                    ),
                );
                (sanitized.clone(), true, text.clone())
            }
            SanitizeResult::PassThrough => (text.clone(), false, String::new()),
        };

        // Publish MESSAGE_RECEIVED event to the Event Bus (with sanitized text)
        let mut payload = serde_json::json!({
            "session_id": session_id,
            "text": final_text,
            "channel": "tauri_desktop",
            "message_id": uuid::Uuid::now_v7(),
            "client_timestamp": std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis())
                .unwrap_or(0),
        });
        if let Some(prev_trace) = &trace_prev {
            payload["trace_prev"] = serde_json::json!(prev_trace);
        }
        if sanitized {
            payload["original_text"] = serde_json::json!(original_text);
            payload["sanitized"] = serde_json::json!(true);
        }
        let event = kernel::event::Event::new(
            "chat-platform:tauri-desktop",
            kernel::event::EventType::MessageReceived,
            payload,
        );
        let event_id = event.id;
        eprintln!("[diag] chat_send_message: publishing event session={session_id} event_id={event_id}");
        rt.publish_event(event)
            .await
            .map_err(|e| {
                eprintln!("[diag] chat_send_message: publish failed session={session_id} error={e}");
                format!("Publish error: {e}")
            })?;
        eprintln!("[diag] chat_send_message: event published session={session_id} event_id={event_id}");

        // Update session version after successful publish.
        touch_session(rt, &session_id).await?;

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

/// Read the current optimistic lock version from a workflow instance's data.
fn read_session_version(instance: &workflow::WorkflowInstance) -> u64 {
    instance.data.get("version").and_then(|v| v.as_u64()).unwrap_or(0)
}

/// Check the expected version against the current version of a session.
///
/// If `expected_version` is `Some(v)`, returns an error if the stored version
/// does not match. Used for optimistic locking (§11.3).
fn check_session_version(instance: &workflow::WorkflowInstance, expected_version: Option<u64>) -> Result<(), String> {
    if let Some(expected) = expected_version {
        let current = read_session_version(instance);
        if current != expected {
            return Err(format!(
                "Session version conflict: expected {expected}, actual {current}"
            ));
        }
    }
    Ok(())
}

/// Helper: update session last_active_at and increment version in workflow data.
async fn touch_session(
    rt: &runtime::AgentRuntime,
    session_id: &str,
) -> Result<(), String> {
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);

    rt.workflow_engine()
        .update_instance_data(session_id, |data| {
            data["last_active_at"] = serde_json::json!(now_ms);
            // Increment optimistic lock version.
            let v = data.get("version").and_then(|v| v.as_u64()).unwrap_or(0);
            data["version"] = serde_json::json!(v + 1);
        })
        .map_err(|e| e.to_string())
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
            let session_type = inst.data.get("session_type").and_then(|v| v.as_str()).map(String::from);
            let parent_session_id = inst.data.get("parent_session_id").and_then(|v| v.as_str()).map(String::from);
            let branch_message_id = inst.data.get("branch_message_id").and_then(|v| v.as_str()).map(String::from);
            let version = inst.data.get("version").and_then(|v| v.as_u64()).unwrap_or(0);
            ChatSessionInfo {
                id: inst.id,
                state: inst.current_state,
                message_count: 0, // computed on request or cached
                created_at: created,
                last_active_at: last_active,
                session_type,
                parent_session_id,
                branch_message_id,
                version,
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
    session_type: Option<String>,
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

    let st = session_type.unwrap_or_else(|| "persistent".to_owned());

    let instance = rt
        .workflow_engine()
        .create_instance(
            "chat-session",
            serde_json::json!({
                "session_type": st,
                "version": 0,
                "created_at": now_ms,
                "last_active_at": now_ms,
            }),
        )
        .map_err(|e| e.to_string())?;
    let result = Ok(instance.id);
    record_ipc(Some(rt), "chat:session_create", result.is_ok(), _start.elapsed().as_secs_f64() * 1000.0);
    result
}

/// Create a branch session forked from a specific message in an existing session.
///
/// The new session inherits the source session's history up to (and including)
/// the given `message_id`, then diverges independently. The branch stores
/// references to the parent session and branch point for traceability.
///
/// Returns the new session ID.
#[tauri::command]
pub async fn chat_session_branch(
    state: State<'_, AppState>,
    session_id: String,
    message_id: String,
    session_type: Option<String>,
) -> Result<String, String> {
    let _start = Instant::now();
    let guard = state.runtime.lock().await;
    let rt = guard
        .as_ref()
        .ok_or_else(|| "No runtime running".to_owned())?;

    if session_id.trim().is_empty() {
        return Err("Session ID cannot be empty".to_owned());
    }
    if message_id.trim().is_empty() {
        return Err("Message ID cannot be empty".to_owned());
    }

    // Verify source session exists.
    let _parent = rt
        .workflow_engine()
        .get_instance(&session_id)
        .ok_or_else(|| format!("Source session not found: {session_id}"))?;

    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);

    let st = session_type.unwrap_or_else(|| "branch".to_owned());

    let instance = rt
        .workflow_engine()
        .create_instance(
            "chat-session",
            serde_json::json!({
                "session_type": st,
                "version": 0,
                "parent_session_id": session_id,
                "branch_message_id": message_id,
                "created_at": now_ms,
                "last_active_at": now_ms,
            }),
        )
        .map_err(|e| e.to_string())?;

    let result = Ok(instance.id);
    record_ipc(Some(rt), "chat:session_branch", result.is_ok(), _start.elapsed().as_secs_f64() * 1000.0);
    result
}

#[tauri::command]
pub async fn chat_session_close(
    state: State<'_, AppState>,
    session_id: String,
    expected_version: Option<u64>,
) -> Result<String, String> {
    let _start = Instant::now();
    let guard = state.runtime.lock().await;
    let rt = guard
        .as_ref()
        .ok_or_else(|| "No runtime running".to_owned())?;

    if session_id.trim().is_empty() {
        return Err("Session ID cannot be empty".to_owned());
    }

    // Optimistic lock version check (§11.3)
    {
        let instance = rt
            .workflow_engine()
            .get_instance(&session_id)
            .ok_or_else(|| format!("Session not found: {session_id}"))?;
        check_session_version(&instance, expected_version)?;
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

    touch_session(rt, &session_id).await?;

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

    let state_version = messages.len() as u64;
    let session_type = instance
        .data
        .get("session_type")
        .and_then(|v| v.as_str())
        .unwrap_or("persistent")
        .to_owned();
    let version = instance
        .data
        .get("version")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);

    let result = Ok(ChatSessionState {
        session_id: instance.id,
        state: instance.current_state,
        state_version,
        retry_count: instance.total_retry_count,
        messages,
        session_type,
        version,
    });
    record_ipc(Some(rt), "chat:session_state", result.is_ok(), _start.elapsed().as_secs_f64() * 1000.0);
    result
}

#[tauri::command]
pub async fn chat_retry_last(
    state: State<'_, AppState>,
    session_id: String,
    expected_version: Option<u64>,
) -> Result<String, String> {
    let _start = Instant::now();
    let guard = state.runtime.lock().await;
    let rt = guard
        .as_ref()
        .ok_or_else(|| "No runtime running".to_owned())?;

    if session_id.trim().is_empty() {
        return Err("Session ID cannot be empty".to_owned());
    }

    // Optimistic lock version check (§11.3)
    let prev_trace_id: Option<String>;
    {
        let instance = rt
            .workflow_engine()
            .get_instance(&session_id)
            .ok_or_else(|| format!("Session not found: {session_id}"))?;
        check_session_version(&instance, expected_version)?;

        // Find the last llm_reply_ready event for this session to get its trace_id.
        let recent = rt.event_store().recent(200);
        prev_trace_id = recent.iter()
            .filter(|e| {
                e.event_type.as_str() == "llm_reply_ready"
                    && e.payload.get("session_id")
                        .and_then(|v| v.as_str())
                        .is_some_and(|sid| sid == session_id)
            })
            .last()
            .map(|e| e.metadata.trace_id.to_string());
    }

    let event = kernel::event::Event::new(
        "tauri:control",
        kernel::event::EventType::Custom("RETRY_CMD".to_owned()),
        serde_json::json!({
            "session_id": &session_id,
            "trace_prev": prev_trace_id,
        }),
    );
    // Record audit log with trace chain info.
    rt.audit().record(
        "user",
        "chat:retry",
        &session_id,
        "ok",
        format!("trace_prev={:?}", prev_trace_id),
    );
    rt.workflow_engine()
        .handle_event(&session_id, event)
        .await
        .map_err(|e| e.to_string())?;

    touch_session(rt, &session_id).await?;

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
    expected_version: Option<u64>,
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

    // Optimistic lock version check (§11.3)
    let original_trace_id: Option<String>;
    {
        let instance = rt
            .workflow_engine()
            .get_instance(&session_id)
            .ok_or_else(|| format!("Session not found: {session_id}"))?;
        check_session_version(&instance, expected_version)?;

        // Look up the original message event to get its trace_id for trace_chain.
        original_trace_id = rt.event_store().get(&message_id)
            .map(|e| e.metadata.trace_id.to_string());
    }

    let event = kernel::event::Event::new(
        "tauri:control",
        kernel::event::EventType::Custom("MESSAGE_EDITED".to_owned()),
        serde_json::json!({
            "session_id": session_id,
            "message_id": message_id,
            "text": text,
            "trace_prev": original_trace_id,
            "edited_at": std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis())
                .unwrap_or(0),
        }),
    );
    let event_id = event.id;
    // Record audit log with trace chain info.
    rt.audit().record(
        "user",
        "chat:edit",
        &message_id,
        "ok",
        format!("session_id={}, trace_prev={:?}", session_id, original_trace_id),
    );
    rt.publish_event(event)
        .await
        .map_err(|e| e.to_string())?;

    touch_session(rt, &session_id).await?;

    let result = Ok(event_id.to_string());
    record_ipc(Some(rt), "chat:edit_message", result.is_ok(), _start.elapsed().as_secs_f64() * 1000.0);
    result
}

#[tauri::command]
pub async fn chat_validator_health(
    state: State<'_, AppState>,
) -> Result<serde_json::Value, String> {
    // Check if the LLM plugin's skill is registered (indicates validator is operational)
    let rt = {
        let guard = state.runtime.lock().await;
        guard.as_ref().map(std::sync::Arc::clone)
    };
    let rt = match rt {
        Some(rt) => rt,
        None => {
            return Ok(serde_json::json!({
                "ok": false,
                "reason": "no_runtime",
                "healthy": false,
            }));
        }
    };

    let has_skill = rt.plugin_loader().await.state_of("llm-plugin")
        .map(|s| s == plugin::PluginLifecycleState::Running)
        .unwrap_or(false);
    let (healthy, rule_count) = if has_skill {
        (true, 7) // default rule count
    } else {
        (false, 0)
    };

    Ok(serde_json::json!({
        "ok": healthy,
        "healthy": healthy,
        "rule_count": rule_count,
        "timeout_sec": 2,
        "fail_closed": true,
    }))
}

/// A single event in a trace chain response (§11.6).
#[derive(Debug, Clone, serde::Serialize)]
pub struct TraceChainEntry {
    pub event_id: String,
    pub event_type: String,
    pub trace_id: String,
    pub timestamp_ms: i64,
    pub session_id: String,
}

#[tauri::command]
pub async fn chat_trace_chain(
    state: State<'_, AppState>,
    trace_id: String,
) -> Result<Vec<TraceChainEntry>, String> {
    let guard = state.runtime.lock().await;
    let rt = guard
        .as_ref()
        .ok_or_else(|| "No runtime running".to_owned())?;

    if trace_id.trim().is_empty() {
        return Err("Trace ID cannot be empty".to_owned());
    }

    let events = rt.event_store().trace_chain(&trace_id);
    let entries: Vec<TraceChainEntry> = events
        .into_iter()
        .map(|e| TraceChainEntry {
            event_id: e.id.to_string(),
            event_type: e.event_type.as_str().to_owned(),
            trace_id: e.metadata.trace_id.to_string(),
            timestamp_ms: e.timestamp.as_millis(),
            session_id: e
                .payload
                .get("session_id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_owned(),
        })
        .collect();

    Ok(entries)
}

// ---------------------------------------------------------------------------
// Provider management (multi-agent P2) — no runtime required
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
            api_key: None,
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

/// Called once at app startup. API keys are stored in macOS Keychain,
/// no env var injection needed.
pub fn load_secrets_into_env() {}

// ---------------------------------------------------------------------------
// Agent management (multi-agent P2) — partially requires filesystem config
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn list_agents(
    state: State<'_, AppState>,
) -> Result<Vec<crate::models::AgentEntry>, String> {
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
            let session_count = agent_dir.exists().then(|| {
                let mut count = 0u64;
                if let Ok(entries) = std::fs::read_dir(&agent_dir) {
                    for entry in entries.flatten() {
                        if entry.file_type().map_or(false, |t| t.is_dir()) {
                            count += 1;
                        }
                    }
                }
                count
            }).unwrap_or(0);

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
        },
    );

    let path = default_config_path();
    aman_config.save(&path).map_err(|e| format!("保存配置失败: {e}"))?;

    Ok(format!("Agent '{key}' 已创建"))
}

#[tauri::command]
pub async fn update_agent(
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
        agent.provider = p;
    }
    if let Some(m) = model {
        agent.model = m;
    }
    if let Some(override_val) = system_prompt_override {
        agent.system_prompt_override = override_val;
    }

    let path = default_config_path();
    aman_config.save(&path).map_err(|e| format!("保存配置失败: {e}"))?;

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
// Config/status queries (multi-agent P2)
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

fn default_config_path() -> std::path::PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    std::path::PathBuf::from(home).join(".aman").join("config.yaml")
}
