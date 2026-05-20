#![forbid(unsafe_code)]
#![doc = "Tauri desktop integration library for Aman."]

pub mod agent_fs;
pub mod commands;
pub mod gateway_client;
pub mod models;
pub mod rate_limiter;
pub mod state;

use state::AppState;
use tauri::Emitter;
use tauri::Manager;
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem, Submenu};
use tokio::time::{interval, Duration};

/// Build and run the Tauri application.
///
/// Called from the binary entry point (`src-tauri/main.rs`).
#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let app_state = AppState::new();

    let gc_for_metrics = app_state.gateway_client.clone();
    let gc_for_events = app_state.gateway_client.clone();
    let gc_for_notifications = app_state.gateway_client.clone();

    // Create a Tokio runtime for background tasks. Must be created before
    // Tauri's event loop since `setup()` runs on the main thread which has
    // no Tokio context. Box::leak gives us a &'static Runtime so it survives
    // the setup closure returning.
    let rt = Box::new(tokio::runtime::Runtime::new().expect("create tokio runtime"));
    let rt: &'static tokio::runtime::Runtime = Box::leak(rt);

    tauri::Builder::default()
        .on_menu_event(|app, event| {
            match event.id().as_ref() {
                "reload_skills" => {
                    let _ = app.emit("menu:reload_skills", ());
                }
                "devtools" => {
                    if let Some(window) = app.get_webview_window("main") {
                        window.open_devtools();
                    }
                }
                _ => {}
            }
        })
        .manage(app_state)
        .invoke_handler(tauri::generate_handler![
            commands::get_runtime_status,
            commands::get_runtime_config,
            commands::start_runtime,
            commands::stop_runtime,
            commands::get_gateway_port,
            commands::get_metrics,
            commands::list_skills,
            commands::reload_skills,
            commands::enable_skill,
            commands::disable_skill,
            commands::inject_event,
            commands::get_debug_events,
            commands::get_event_trace,
            commands::get_workflow_instances,
            commands::get_workflow_def,
            commands::retry_workflow,
            commands::cancel_workflow,
            commands::get_soul_info,
            commands::preview_system_prompt,
            commands::update_soul,
            commands::get_soul_raw,
            commands::list_plugins,
            commands::enable_plugin,
            commands::disable_plugin,
            commands::list_dlq,
            commands::retry_dlq,
            commands::discard_dlq,
            commands::get_capabilities,
            commands::chat_send_message,
            commands::chat_stop_generation,
            commands::chat_session_list,
            commands::chat_session_list_db,
            commands::chat_session_create,
            commands::chat_session_branch,
            commands::chat_session_close,
            commands::chat_session_delete,
            commands::chat_session_history,
            commands::chat_session_state,
            commands::chat_retry_last,
            commands::chat_edit_message,
            commands::chat_trace_chain,
            // Multi-agent provider commands (P2)
            commands::list_providers,
            commands::create_provider,
            commands::update_provider,
            commands::delete_provider,
            commands::set_provider_api_key,
            commands::has_provider_api_key,
            // Multi-agent agent commands (P2)
            commands::list_agents,
            commands::create_agent,
            commands::update_agent,
            commands::delete_agent,
            commands::get_agent_soul,
            commands::select_agent,
            commands::get_active_agent,
            // Multi-agent config/status (P2)
            commands::get_aman_config,
            commands::has_any_provider,
            commands::has_any_agent,
            commands::get_default_model,
            // Tool authorization
            commands::show_tool_auth_dialog,
            // Third-party service keys
            commands::list_third_party_services,
            commands::set_third_party_key,
            commands::set_third_party_config,
            // Notifications
            commands::get_notifications,
            commands::get_notifications_unread_count,
            commands::notification_dismiss,
            commands::notification_ack,
            commands::notification_dismiss_all,
            // Agent runtime (M1)
            commands::list_runtime_agents,
            commands::get_runtime_agent,
            commands::set_runtime_agent_status,
        ])
        .setup(move |app: &mut tauri::App<tauri::Wry>| {
            // Build menu bar
            let handle = app.handle();
            let reload = MenuItem::with_id(handle, "reload_skills", "Reload Skills", true, Some("CmdOrCtrl+R"))?;
            let separator = PredefinedMenuItem::separator(handle)?;
            let quit = PredefinedMenuItem::quit(handle, Some("Quit Aman"))?;
            let file_menu = Submenu::with_items(handle, "File", true, &[&reload, &separator, &quit])?;
            let cut = PredefinedMenuItem::cut(handle, Some("Cut"))?;
            let copy = PredefinedMenuItem::copy(handle, Some("Copy"))?;
            let paste = PredefinedMenuItem::paste(handle, Some("Paste"))?;
            let select_all = PredefinedMenuItem::select_all(handle, Some("Select All"))?;
            let edit_sep = PredefinedMenuItem::separator(handle)?;
            let edit_menu = Submenu::with_items(
                handle, "Edit", true,
                &[&cut, &copy, &paste, &edit_sep, &select_all],
            )?;
            let about = PredefinedMenuItem::about(handle, Some("About Aman"), None)?;
            let devtools = MenuItem::with_id(handle, "devtools", "Toggle DevTools", true, Some("CmdOrCtrl+Shift+I"))?;
            let help_menu = Submenu::with_items(handle, "Help", true, &[&about, &devtools])?;
            let menu = Menu::with_items(handle, &[&file_menu, &edit_menu, &help_menu])?;
            app.set_menu(menu)?;

            let handle1 = app.handle().clone();
            let handle2 = app.handle().clone();
            let handle3 = app.handle().clone();

            // Background task: emit `metrics:updated` every 2 s.
            rt.spawn(async move {
                let mut tick = interval(Duration::from_secs(2));
                loop {
                    tick.tick().await;
                    let guard = gc_for_metrics.lock().await;
                    let snapshot = match guard.as_ref() {
                        Some(client) => {
                            match client.debug_metrics().await {
                                Ok(v) => {
                                    let plugin_health = v["plugin_health"].as_array()
                                        .map(|arr| {
                                            arr.iter().map(|item| crate::models::PluginHealthEntry {
                                                name: item["name"].as_str().unwrap_or("").to_owned(),
                                                status: item["status"].as_str().unwrap_or("").to_owned(),
                                            }).collect::<Vec<_>>()
                                        })
                                        .unwrap_or_default();

                                    Some(crate::models::MetricsSnapshot {
                                        queue_depth: crate::models::QueueDepth {
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
                                Err(_) => None,
                            }
                        }
                        None => None,
                    };
                    drop(guard);
                    if let Some(snapshot) = snapshot {
                        let payload = serde_json::to_value(&snapshot).unwrap_or_default();
                        let _ = handle1.emit("metrics:updated", payload);
                    }
                }
            });

            // Background task: emit `event:processed` every 1 s (poll EventStore via gateway).
            rt.spawn(async move {
                let mut tick = interval(Duration::from_secs(1));
                let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
                loop {
                    tick.tick().await;
                    let guard = gc_for_events.lock().await;
                    if let Some(client) = guard.as_ref() {
                        match client.recent_events(20).await {
                            Ok(v) => {
                                drop(guard);
                                if let Some(events) = v["events"].as_array() {
                                    // Events are newest-first from EventStore::recent().
                                    // Reverse to oldest-first so stream_start arrives before chunks.
                                    for event_val in events.iter().rev() {
                                        if let Some(id) = event_val["id"].as_str() {
                                            if seen.insert(id.to_owned()) {
                                                let _ = handle2.emit("event:processed", event_val.clone());
                                            }
                                        }
                                    }
                                }
                            }
                            Err(_) => { drop(guard); }
                        }
                    } else {
                        drop(guard);
                    }
                }
            });

            // Background task: emit `notification:updated` every 2 s (poll notification center).
            rt.spawn(async move {
                let mut tick = interval(Duration::from_secs(2));
                let mut previous_count: i64 = 0;
                loop {
                    tick.tick().await;
                    let guard = gc_for_notifications.lock().await;
                    if let Some(client) = guard.as_ref() {
                        match client.notifications_unread_count().await {
                            Ok(count) => {
                                drop(guard);
                                if count != previous_count {
                                    previous_count = count;
                                    let _ = handle3.emit(
                                        "notification:updated",
                                        serde_json::json!({ "unread_count": count }),
                                    );
                                }
                            }
                            Err(_) => { drop(guard); }
                        }
                    } else {
                        drop(guard);
                    }
                }
            });

            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
