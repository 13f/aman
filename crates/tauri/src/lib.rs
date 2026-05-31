#![forbid(unsafe_code)]
#![doc = "Tauri desktop integration library for aman."]
// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0


pub mod agent_fs;
pub mod code_agents;
pub mod commands;
pub mod finance_cards;
pub mod gateway_client;
pub mod models;
pub mod rate_limiter;
pub mod sse_client;
pub mod state;

use state::AppState;
use std::sync::atomic::{AtomicBool, Ordering};
use tauri::Emitter;
use tauri::Manager;
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem, Submenu};

/// Guard to prevent re-entrant `CloseRequested` from `window.close()` after
/// the gateway shutdown sequence has already started.
static SHUTTING_DOWN: AtomicBool = AtomicBool::new(false);

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
                if SHUTTING_DOWN.load(Ordering::SeqCst) {
                    return;
                }

                // Only intercept if gateway client is still connected.
                let has_gateway = rt.block_on(async {
                    let guard = shutdown_gc.lock().await;
                    guard.is_some()
                });
                if !has_gateway {
                    return;
                }

                // Check whether we own a child process before spawning the
                // background task (so we can move the handle in).
                let owns_gateway = rt.block_on(async {
                    let guard = shutdown_gp.lock().await;
                    guard.is_some()
                });

                // Show the shutdown animation immediately, then perform the
                // actual shutdown work in the background so the user sees
                // feedback instead of a frozen / instantly-closing window.
                SHUTTING_DOWN.store(true, Ordering::SeqCst);
                api.prevent_close();
                let _ = window.emit("shutdown:started", ());

                let gc = shutdown_gc.clone();
                let gp = shutdown_gp.clone();
                let handle = window.app_handle().clone();
                rt.spawn(async move {
                    // 1. Graceful HTTP shutdown (best-effort).
                    let base_url = {
                        let guard = gc.lock().await;
                        guard.as_ref().map(|c| c.base_url.clone())
                    };
                    if let Some(ref url) = base_url
                        && let Ok(http_client) = reqwest::Client::builder()
                            .no_proxy()
                            .build()
                    {
                        let _ = http_client
                            .post(format!("{url}/agent/shutdown"))
                            .header("x-aman-confirm", "yes")
                            .send()
                            .await;
                    }

                    // 2. Clear the client from state.
                    {
                        let mut guard = gc.lock().await;
                        *guard = None;
                    }

                    // 3. Kill the child process if we own it.
                    if owns_gateway {
                        let mut proc_guard = gp.lock().await;
                        if let Some(mut child) = proc_guard.take() {
                            let _ = child.kill().await;
                            let _ = child.wait().await;
                        }
                    }

                    // 4. Let the frontend animation breathe before closing.
                    let _ = handle.emit("shutdown:complete", ());
                    tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
                    // Close window (re-enters CloseRequested, SHUTTING_DOWN lets it through).
                    let _ = handle.get_webview_window("main").map(|w| w.close());
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
            commands::select_agent,
            commands::get_active_agent,
            // Multi-agent config/status (P2)
            commands::get_aman_config,
            commands::get_secrets_mode,
            commands::has_any_provider,
            commands::has_any_agent,
            commands::get_default_model,
            // Tool authorization
            commands::show_tool_auth_dialog,
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
        ])
        .setup(move |app: &mut tauri::App<tauri::Wry>| {
            // Set the window/dock icon explicitly for dev mode.
            // The bundle.icon config only applies to production builds.
            let icon_data = include_bytes!("../icons/128x128@2x.png");
            if let Ok(icon) = tauri::image::Image::from_bytes(icon_data)
                && let Some(window) = app.get_webview_window("main") {
                    let _ = window.set_icon(icon);
                }

            // Build menu bar
            let handle = app.handle();
            let reload = MenuItem::with_id(handle, "reload_skills", "Reload Skills", true, Some("CmdOrCtrl+R"))?;
            let separator = PredefinedMenuItem::separator(handle)?;
            let quit = PredefinedMenuItem::quit(handle, Some("Quit aman desktop"))?;
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
            let about = PredefinedMenuItem::about(handle, Some("About aman desktop"), None)?;
            let devtools = MenuItem::with_id(handle, "devtools", "Toggle DevTools", true, Some("CmdOrCtrl+Shift+I"))?;
            let help_menu = Submenu::with_items(handle, "Help", true, &[&about, &devtools])?;
            let menu = Menu::with_items(handle, &[&file_menu, &edit_menu, &help_menu])?;
            app.set_menu(menu)?;

            let sse_handle = app.handle().clone();

            // Single SSE listener replacing 5 polling loops.
            rt.spawn(async move {
                sse_client::run_sse_listener(sse_handle, sse_gc).await;
            });

            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
