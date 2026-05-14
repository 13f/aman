#![forbid(unsafe_code)]
#![doc = "Tauri desktop integration library for Aman."]

pub mod commands;
pub mod models;
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

    // Clone the inner runtime handle for background tasks before moving
    // app_state into Tauri's managed state.
    let rt_state_for_metrics = app_state.runtime.clone();
    let rt_state_for_events = app_state.runtime.clone();

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
            commands::get_metrics,
            commands::list_skills,
            commands::reload_skills,
            commands::enable_skill,
            commands::disable_skill,
            commands::inject_event,
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
        ])
        .setup(move |app: &mut tauri::App<tauri::Wry>| {
            // Build menu bar
            let handle = app.handle();
            let reload = MenuItem::with_id(handle, "reload_skills", "Reload Skills", true, Some("CmdOrCtrl+R"))?;
            let separator = PredefinedMenuItem::separator(handle)?;
            let quit = PredefinedMenuItem::quit(handle, Some("Quit Aman"))?;
            let file_menu = Submenu::with_items(handle, "File", true, &[&reload, &separator, &quit])?;
            let about = PredefinedMenuItem::about(handle, Some("About Aman"), None)?;
            let devtools = MenuItem::with_id(handle, "devtools", "Toggle DevTools", true, Some("CmdOrCtrl+Shift+I"))?;
            let help_menu = Submenu::with_items(handle, "Help", true, &[&about, &devtools])?;
            let menu = Menu::with_items(handle, &[&file_menu, &help_menu])?;
            app.set_menu(menu)?;

            let handle1 = app.handle().clone();
            let handle2 = app.handle().clone();

            // Background task: emit `metrics:updated` every 2 s.
            rt.spawn(async move {
                let mut tick = interval(Duration::from_secs(2));
                loop {
                    tick.tick().await;
                    let guard = rt_state_for_metrics.lock().await;
                    let snapshot = match guard.as_ref() {
                        Some(rt) => {
                            let bus = rt.bus_metrics();
                            let dlq_depth = rt.dlq().depth();
                            let loader = rt.plugin_loader().await;
                            let plugin_health: Vec<crate::models::PluginHealthEntry> = loader
                                .loaded_plugins()
                                .into_iter()
                                .map(|name| {
                                    let status = match loader.state_of(&name) {
                                        Some(s) => format!("{s:?}"),
                                        None => "unknown".to_owned(),
                                    };
                                    crate::models::PluginHealthEntry { name, status }
                                })
                                .collect();
                            Some(crate::models::MetricsSnapshot {
                                queue_depth: crate::models::QueueDepth {
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
                        None => None,
                    };
                    drop(guard);
                    if let Some(snapshot) = snapshot {
                        let payload = serde_json::to_value(&snapshot).unwrap_or_default();
                        let _ = handle1.emit("metrics:updated", payload);
                    }
                }
            });

            // Background task: emit `event:processed` every 1 s (poll EventStore).
            rt.spawn(async move {
                let mut tick = interval(Duration::from_secs(1));
                let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
                loop {
                    tick.tick().await;
                    let guard = rt_state_for_events.lock().await;
                    if let Some(rt) = guard.as_ref() {
                        let events = rt.event_store().recent(20);
                        drop(guard);
                        for event in events {
                            if seen.insert(event.id.to_string()) {
                                let payload = serde_json::to_value(&event).unwrap_or_default();
                                let _ = handle2.emit("event:processed", payload);
                            }
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
