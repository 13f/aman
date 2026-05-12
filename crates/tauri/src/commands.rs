use crate::models::{
    DlqEntry, MetricsSnapshot, PluginEntry, PluginHealthEntry, QueueDepth, RuntimeConfigInfo,
    RuntimeStatusInfo,
    SkillEntry, SoulInfo, WorkflowEntry,
};
use crate::state::AppState;
use config::ConfigLoader;
use kernel::event::{Event, EventType};
use persistence::DeadLetterQueue;
use runtime::{AgentRuntimeBuilder, RuntimePhase};
use tauri::State;

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

    let load_result =
        ConfigLoader::load(Some(std::path::Path::new(&path)), None).map_err(|e| format!("Config load error: {e}"))?;
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
            enqueued_at_ms: e.enqueued_at.as_millis() as i64,
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
