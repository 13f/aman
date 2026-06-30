#![forbid(unsafe_code)]
#![doc = "Tauri desktop integration library for aman."]
// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0


pub mod agent_fs;
pub mod code_agents;
pub mod commands;
pub mod finance_cards;
pub mod gateway_client;
pub mod mcp_servers_fs;
pub mod models;
pub mod rate_limiter;
pub mod sse_client;
pub mod state;
pub mod tts;

use i18n::Translator;
use state::AppState;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use tauri::Emitter;
use tauri::Manager;
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem, Submenu};
use tokio::sync::oneshot;

/// Guard to prevent re-entrant `CloseRequested` from `window.close()` after
/// the gateway shutdown sequence has already started.
static SHUTTING_DOWN: AtomicBool = AtomicBool::new(false);

/// Channel used to wait for the frontend's shutdown confirmation when
/// busy agents are detected during window close.
static SHUTDOWN_CONFIRM_TX: Mutex<Option<oneshot::Sender<bool>>> = Mutex::new(None);

/// Set to `true` during shutdown to signal the SSE listener that it should
/// stop reconnecting and exit gracefully.
static SSE_SHOULD_STOP: AtomicBool = AtomicBool::new(false);

/// Build and run the Tauri application.
///
/// Called from the binary entry point (`src-tauri/main.rs`).
#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let app_state = AppState::new();

    let shutdown_gc = app_state.gateway_client.clone();
    let shutdown_gp = app_state.gateway_process.clone();
    let sse_gc = app_state.gateway_client.clone();

    // Create a Tokio runtime for background tasks. Must be created before
    // Tauri's event loop since `setup()` runs on the main thread which has
    // no Tokio context. Box::leak gives us a &'static Runtime so it survives
    // the setup closure returning.
    let rt = Box::new(tokio::runtime::Runtime::new().expect("create tokio runtime"));
    let rt: &'static tokio::runtime::Runtime = Box::leak(rt);

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
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
        .on_window_event(move |window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                // Guard: prevent re-entrant close from the background task's
                // `window.close()` call. SHUTTING_DOWN is set at the start so
                // the guard also prevents a second close attempt while the
                // first is still in progress.
                if SHUTTING_DOWN.load(Ordering::SeqCst) {
                    return;
                }
                SHUTTING_DOWN.store(true, Ordering::SeqCst);
                api.prevent_close();

                let gc = shutdown_gc.clone();
                let gp = shutdown_gp.clone();
                let handle = window.app_handle().clone();

                // All async shutdown work runs in a background tokio task so
                // the main thread (Cocoa event loop) stays responsive.
                rt.spawn(async move {
                    // 1. Fast path: no gateway client → close immediately.
                    let has_gateway = {
                        let guard = gc.lock().await;
                        guard.is_some()
                    };
                    if !has_gateway {
                        tracing::info!("no gateway client — closing window");
                        SHUTTING_DOWN.store(false, Ordering::SeqCst);
                        let _ = handle
                            .get_webview_window("main")
                            .map(|w| w.close());
                        return;
                    }

                    // 2. Check whether we own a child process (needed later
                    //    for cleanup).
                    let owns_gateway = {
                        let guard = gp.lock().await;
                        guard.is_some()
                    };

                    // 3. Query the gateway for busy agents. If any agents are
                    //    currently processing, ask the frontend to confirm.
                    let busy_agents: Vec<serde_json::Value> = {
                        let guard = gc.lock().await;
                        if let Some(ref client) = *guard {
                            match client.list_agents().await {
                                Ok(v) => v
                                    .as_array()
                                    .map(|a| {
                                        a.iter()
                                            .filter(|item| {
                                                item.get("status")
                                                    .and_then(|s| s.as_str())
                                                    == Some("Busy")
                                            })
                                            .cloned()
                                            .collect()
                                    })
                                    .unwrap_or_default(),
                                Err(_) => Vec::new(),
                            }
                        } else {
                            Vec::new()
                        }
                    };

                    if !busy_agents.is_empty() {
                        tracing::info!(
                            count = busy_agents.len(),
                            agents = ?busy_agents.iter()
                                .map(|a| a.get("descriptor").and_then(|d| d.get("agent_id")).and_then(|v| v.as_str()).unwrap_or("?"))
                                .collect::<Vec<_>>(),
                            "busy agents detected — awaiting user confirmation"
                        );

                        let payload: Vec<serde_json::Value> = busy_agents
                            .iter()
                            .map(|a| {
                                serde_json::json!({
                                    "agent_id": a.get("descriptor").and_then(|d| d.get("agent_id")).and_then(|v| v.as_str()).unwrap_or(""),
                                    "display_name": a.get("descriptor").and_then(|d| d.get("display_name")).and_then(|v| v.as_str()).unwrap_or(""),
                                    "system_state": a.get("system_state").and_then(|v| v.as_str()).unwrap_or(""),
                                    "active_session_id": a.get("active_session_id").and_then(|v| v.as_str()),
                                })
                            })
                            .collect();
                        let _ = handle.emit("shutdown:busy-agents", &payload);

                        // Wait for the frontend to confirm or cancel.
                        let (tx, rx) = oneshot::channel::<bool>();
                        {
                            let mut guard = SHUTDOWN_CONFIRM_TX
                                .lock()
                                .expect("SHUTDOWN_CONFIRM_TX lock");
                            *guard = Some(tx);
                        }
                        let confirmed = match tokio::time::timeout(
                            std::time::Duration::from_secs(60),
                            rx,
                        )
                        .await
                        {
                            Ok(Ok(v)) => v,
                            _ => false,
                        };

                        if !confirmed {
                            tracing::info!(
                                "shutdown cancelled by user or confirmation timed out"
                            );
                            SHUTTING_DOWN.store(false, Ordering::SeqCst);
                            let _ = handle.emit("shutdown:cancelled", ());
                            return;
                        }
                    }

                    // 4. Proceed with graceful shutdown.
                    let _ = handle.emit("shutdown:started", ());

                    // Signal the SSE listener to stop reconnecting.
                    crate::SSE_SHOULD_STOP.store(true, Ordering::Release);

                    // 5. Graceful HTTP shutdown (best-effort, 5 s timeout).
                    let base_url = {
                        let guard = gc.lock().await;
                        guard.as_ref().map(|c| c.base_url.clone())
                    };
                    if let Some(ref url) = base_url
                        && let Ok(http_client) = reqwest::Client::builder()
                            .no_proxy()
                            .timeout(std::time::Duration::from_secs(5))
                            .build()
                    {
                        let _ = http_client
                            .post(format!("{url}/agent/shutdown"))
                            .header("x-aman-confirm", "yes")
                            .send()
                            .await;
                    }

                    // 6. Clear the client from state.
                    {
                        let mut guard = gc.lock().await;
                        *guard = None;
                    }

                    // 7. Kill the child process if we own it.
                    if owns_gateway {
                        let mut proc_guard = gp.lock().await;
                        if let Some(mut child) = proc_guard.take() {
                            let _ = child.kill().await;
                            let _ = child.wait().await;
                        }
                    }

                    // 8. Let the frontend animation breathe before closing.
                    let _ = handle.emit("shutdown:complete", ());
                    tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
                    // Re-enters CloseRequested; SHUTTING_DOWN is true → returns
                    // without prevent_close() → window actually closes.
                    let _ = handle
                        .get_webview_window("main")
                        .map(|w| w.close());
                });
            }
        })
        .manage(app_state)
        .invoke_handler(tauri::generate_handler![
            commands::get_runtime_status,
            commands::try_connect_gateway,
            commands::get_runtime_config,
            commands::start_runtime,
            commands::stop_runtime,
            commands::respond_shutdown,
            commands::get_gateway_port,
            commands::get_metrics,
            commands::list_skills,
            commands::list_llm_skills,
            commands::reload_skills,
            commands::enable_skill,
            commands::disable_skill,
            commands::search_skills,
            commands::read_skill_content,
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
            commands::chat_session_rename,
            commands::explore_start,
            commands::idle_run,
            commands::list_idle_availability,
            commands::chat_session_branch,
            commands::chat_session_close,
            commands::chat_session_delete,
            commands::chat_session_history,
            commands::chat_session_state,
            commands::chat_session_state_local,
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
            commands::list_provider_models,
            // Multi-agent agent commands (P2)
            commands::list_agents,
            commands::create_agent,
            commands::update_agent,
            commands::delete_agent,
            commands::get_agent_soul,
            commands::get_agent_emotions,
            commands::select_agent,
            commands::get_active_agent,
            // Multi-agent config/status (P2)
            commands::get_aman_config,
            commands::get_secrets_mode,
            commands::get_mcp_enabled,
            commands::get_locale,
            commands::get_ui_style,
            commands::get_agents_viewer,
            commands::has_any_provider,
            commands::has_any_agent,
            commands::get_default_model,
            // Tool authorization
            commands::show_tool_auth_dialog,
            // Plugin capability authorization
            commands::show_plugin_auth_dialog,
            commands::show_confirm_dialog,
            // Third-party service keys
            commands::list_third_party_services,
            commands::set_third_party_key,
            commands::set_third_party_config,
            // IM Channels
            commands::list_im_channels,
            commands::save_im_channel,
            commands::delete_im_channel_field,
            commands::delete_im_channel_instance,
            commands::test_im_channel,
            commands::reload_im_channel,
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
            // Code agents — external CLI tools
            commands::list_code_agents,
            commands::launch_code_agent,
            // Finance cards
            commands::list_finance_cards,
            commands::add_finance_card,
            commands::remove_finance_card,
            // Plugin UI pages
            commands::get_plugin_pages,
            // MCP server management
            commands::list_mcp_servers,
            commands::create_mcp_server,
            commands::delete_mcp_server,
            commands::connect_mcp_server,
            commands::disconnect_mcp_server,
            commands::list_agent_keys,
        ])
        .setup(move |app: &mut tauri::App<tauri::Wry>| {
            // Set the window/dock icon explicitly for dev mode.
            // The bundle.icon config only applies to production builds.
            let icon_data = include_bytes!("../icons/128x128@2x.png");
            if let Ok(icon) = tauri::image::Image::from_bytes(icon_data)
                && let Some(window) = app.get_webview_window("main") {
                    let _ = window.set_icon(icon);
                }

            // ── Native window vibrancy (frosted glass) ────────────
            // macOS: NSVisualEffectView with Active state so the
            //   blur intensity does NOT change when the window loses
            //   focus (fixes the "mouse away → transparent" issue).
            // Windows: apply_blur for a dark frosted-glass effect.
            #[cfg(target_os = "macos")]
            if let Some(window) = app.get_webview_window("main") {
                use window_vibrancy::{apply_vibrancy, NSVisualEffectMaterial, NSVisualEffectState};
                let _ = apply_vibrancy(
                    &window,
                    NSVisualEffectMaterial::Sidebar,
                    Some(NSVisualEffectState::Active),
                    None,
                );
            }

            #[cfg(target_os = "windows")]
            if let Some(window) = app.get_webview_window("main") {
                use window_vibrancy::apply_blur;
                // Dark tint matching the --bg colour: rgba(11,13,19,0.55)
                let _ = apply_blur(&window, Some((11, 13, 19, 140)));
            }

            // Build menu bar (i18n-aware via app state locale).
            let handle = app.handle();
            let t = app.state::<AppState>().locale;
            let translator = Translator::new(t);

            let reload = MenuItem::with_id(
                handle, "reload_skills",
                translator.translate("desktop.menu.reload_skills"),
                true, Some("CmdOrCtrl+R"),
            )?;
            let separator = PredefinedMenuItem::separator(handle)?;
            let quit = PredefinedMenuItem::quit(
                handle,
                Some(translator.translate("desktop.menu.quit")),
            )?;
            let file_menu = Submenu::with_items(
                handle,
                translator.translate("desktop.menu.file"),
                true,
                &[&reload, &separator, &quit],
            )?;
            let cut = PredefinedMenuItem::cut(handle, None)?;
            let copy = PredefinedMenuItem::copy(handle, None)?;
            let paste = PredefinedMenuItem::paste(handle, None)?;
            let select_all = PredefinedMenuItem::select_all(handle, None)?;
            let edit_sep = PredefinedMenuItem::separator(handle)?;
            let edit_menu = Submenu::with_items(
                handle,
                translator.translate("desktop.menu.edit"),
                true,
                &[&cut, &copy, &paste, &edit_sep, &select_all],
            )?;
            let about = PredefinedMenuItem::about(
                handle,
                Some(translator.translate("desktop.menu.about")),
                None,
            )?;
            let devtools = MenuItem::with_id(
                handle, "devtools",
                translator.translate("desktop.menu.devtools"),
                true,
                Some("CmdOrCtrl+Shift+I"),
            )?;
            let help_menu = Submenu::with_items(
                handle,
                translator.translate("desktop.menu.help"),
                true,
                &[&about, &devtools],
            )?;
            let menu = Menu::with_items(handle, &[&file_menu, &edit_menu, &help_menu])?;
            app.set_menu(menu)?;

            let sse_handle = app.handle().clone();

            // Auto-reader for TTS — created when desktop.auto_read + model.tts are configured.
            let auto_reader = tts::AutoReader::from_config().map(Arc::new);

            // Single SSE listener replacing 5 polling loops.
            rt.spawn(async move {
                sse_client::run_sse_listener(sse_handle, sse_gc, auto_reader).await;
            });

            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
