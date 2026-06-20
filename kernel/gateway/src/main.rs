#![forbid(unsafe_code)]
// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0


//! aman Gateway Daemon
//!
//! Standalone background process that wraps the agent runtime and serves
//! its HTTP API. Designed to run as a systemd/launchd service, independent
//! of any desktop UI.
//!
//! Usage:
//!   aman [--config PATH] [--bind ADDR] [--token TOKEN] [--soul PATH] [--no-tui]

use config::ConfigLoader;
use gateway::runtime::{serve, AgentRuntimeBuilder, HttpServerConfig, RedactWriter};
use gateway::ai_signal::AmanSignalV1;
use kernel::event::{Event, EventType};
use std::fs::File;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

/// eprintln! wrapper that redacts sensitive data before printing.
/// Used for startup errors before the tracing subscriber is active.
macro_rules! safe_eprintln {
    ($($arg:tt)*) => {
        let msg = format!($($arg)*);
        eprintln!("{}", kernel::redactor::redact_sensitive_data(&msg));
    };
}

static _AI_SIGNAL: () = {
    let _ = std::any::TypeId::of::<AmanSignalV1>();
};

const DEFAULT_BIND: &str = "127.0.0.1:9999";
const PID_FILE: &str = ".aman/aman.pid";

#[tokio::main(flavor = "multi_thread")]
async fn main() {
    // TUI mode is the default. Pass --no-tui to use plain stdout logging instead.
    let args: Vec<String> = std::env::args().skip(1).collect();
    let no_tui = args.iter().any(|a| a == "--no-tui");

    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    let log_dir = PathBuf::from(&home).join(".aman");
    let _ = std::fs::create_dir_all(&log_dir);
    let log_path = log_dir.join("gateway.log");

    if no_tui {
        let log_file = File::create(&log_path).expect("failed to create gateway log file");
        let file_layer = tracing_subscriber::fmt::layer()
            .with_ansi(false)
            .with_writer(Mutex::new(RedactWriter::new(log_file)));
        let env_filter = tracing_subscriber::EnvFilter::builder()
            .with_default_directive(tracing::level_filters::LevelFilter::INFO.into())
            .from_env_lossy();

        // Non-TUI mode: log to stdout + file.
        let stdout_layer = tracing_subscriber::fmt::layer()
            .with_writer(|| RedactWriter::new(std::io::stdout()));

        tracing_subscriber::registry()
            .with(env_filter)
            .with(stdout_layer)
            .with(file_layer)
            .init();

        if let Err(code) = run().await {
            std::process::exit(code);
        }
    } else {
        let log_file = File::create(&log_path).expect("failed to create gateway log file");
        let file_layer = tracing_subscriber::fmt::layer()
            .with_ansi(false)
            .with_writer(Mutex::new(RedactWriter::new(log_file)));
        let env_filter = tracing_subscriber::EnvFilter::builder()
            .with_default_directive(tracing::level_filters::LevelFilter::INFO.into())
            .from_env_lossy();

        // TUI mode (default): log to file + TUI ring buffer, NOT stdout.
        // The TUI renders logs in its left panel instead.
        let log_buffer = Arc::new(gateway::tui::LogBuffer::default());
        let tui_layer = gateway::tui::TuiLogLayer::new(Arc::clone(&log_buffer));

        // Order: fmt::Layer (file) must come before TuiLogLayer because
        // fmt::Layer requires LookupSpan on the inner subscriber.
        tracing_subscriber::registry()
            .with(file_layer)
            .with(env_filter)
            .with(tui_layer)
            .init();

        if let Err(code) = run_tui_mode(args, log_buffer).await {
            std::process::exit(code);
        }
    }
}

// eprintln! is used here for startup errors that occur BEFORE the tracing
// subscriber is initialized. Once tracing is up, all logging goes through
// the RedactWriter-wrapped subscriber.
#[allow(clippy::print_stderr)]
async fn run() -> Result<(), i32> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    let (config_path, bind, api_token, soul_path) = parse_args(&args)?;

    // Load config from file or default path.
    let config = ConfigLoader::load(config_path.as_deref(), None)
        .map_err(|e| {
            safe_eprintln!("Config load error: {e}");
            1
        })?
        .config;

    let runtime = build_runtime(config, bind, api_token, soul_path, tokio::runtime::Handle::current())?;

    tracing::info!(bind = %bind, "starting gateway");

    let server = serve(
        Arc::clone(&runtime),
        HttpServerConfig { bind },
    )
    .await
    .map_err(|e| {
        safe_eprintln!("HTTP server error: {e}");
        1
    })?;

    write_pid_file();

    // Publish gateway lifecycle event before starting runtime.
    let _ = runtime.publish_event(Event::new(
        "gateway:lifecycle",
        EventType::Custom("gateway:starting".to_owned()),
        serde_json::json!({"bind": bind.to_string()}),
    )).await;

    // Register signal handlers before runtime.start() so Ctrl+C can
    // interrupt a stuck startup (e.g. Phase 4 source init).
    let shutdown_notify = runtime.shutdown_notify();

    #[cfg(unix)]
    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .map_err(|_| 1)?;

    // Race startup against interrupt signals.
    #[cfg(unix)]
    {
        tokio::select! {
            r = tokio::time::timeout(Duration::from_secs(30), runtime.start()) => {
                match r {
                    Ok(Ok(())) => {}
                    Ok(Err(e)) => {
                        safe_eprintln!("Runtime start error: {e}");
                        return Err(1);
                    }
                    Err(_) => {
                        let phase = runtime.phase();
                        safe_eprintln!("Runtime start timed out after 30s (phase={phase:?})");
                        return Err(1);
                    }
                }
            }
            _ = tokio::signal::ctrl_c() => {
                tracing::info!("received SIGINT during startup, shutting down");
                let _ = runtime.shutdown().await;
                server.shutdown();
                return Err(1);
            }
            _ = sigterm.recv() => {
                tracing::info!("received SIGTERM during startup, shutting down");
                let _ = runtime.shutdown().await;
                server.shutdown();
                return Err(1);
            }
        }
    }

    #[cfg(not(unix))]
    {
        tokio::select! {
            r = tokio::time::timeout(Duration::from_secs(30), runtime.start()) => {
                match r {
                    Ok(Ok(())) => {}
                    Ok(Err(e)) => {
                        safe_eprintln!("Runtime start error: {e}");
                        return Err(1);
                    }
                    Err(_) => {
                        let phase = runtime.phase();
                        safe_eprintln!("Runtime start timed out after 30s (phase={phase:?})");
                        return Err(1);
                    }
                }
            }
            _ = tokio::signal::ctrl_c() => {
                tracing::info!("received SIGINT during startup, shutting down");
                let _ = runtime.shutdown().await;
                server.shutdown();
                return Err(1);
            }
        }
    }

    let addr = server.local_addr();
    tracing::info!(%addr, "gateway ready");

    let _ = runtime.publish_event(Event::new(
        "gateway:lifecycle",
        EventType::Custom("gateway:ready".to_owned()),
        serde_json::json!({"bind": bind.to_string(), "addr": addr.to_string()}),
    )).await;

    // Wait for shutdown signal or HTTP-initiated shutdown completion.
    #[cfg(unix)]
    {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                tracing::info!("received SIGINT, shutting down");
            }
            _ = sigterm.recv() => {
                tracing::info!("received SIGTERM, shutting down");
            }
            _ = shutdown_notify.notified() => {
                tracing::info!("shutdown completed via HTTP, exiting");
            }
        }
    }

    #[cfg(not(unix))]
    {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                tracing::info!("received SIGINT, shutting down");
            }
            _ = shutdown_notify.notified() => {
                tracing::info!("shutdown completed via HTTP, exiting");
            }
        }
    }

    do_shutdown(&runtime, server).await;
    Ok(())
}

/// TUI mode: gateway runs in the background, TUI renders on the main thread.
/// When the user presses `q` in the TUI, the gateway shuts down.
async fn run_tui_mode(
    args: Vec<String>,
    log_buffer: Arc<gateway::tui::LogBuffer>,
) -> Result<(), i32> {
    let (config_path, bind, api_token, soul_path) = parse_args(&args)?;

    let config = ConfigLoader::load(config_path.as_deref(), None)
        .map_err(|e| {
            tracing::error!(error = %e, "config load error");
            1
        })?
        .config;

    let runtime = build_runtime(config, bind, api_token, soul_path, tokio::runtime::Handle::current())?;

    tracing::info!(bind = %bind, "starting gateway (TUI mode)");

    let server = serve(
        Arc::clone(&runtime),
        HttpServerConfig { bind },
    )
    .await
    .map_err(|e| {
        tracing::error!(error = %e, "HTTP server error");
        1
    })?;

    write_pid_file();

    let _ = runtime.publish_event(Event::new(
        "gateway:lifecycle",
        EventType::Custom("gateway:starting".to_owned()),
        serde_json::json!({"bind": bind.to_string()}),
    )).await;

    // Start the runtime in the background — it races against Ctrl+C.
    let startup_runtime = Arc::clone(&runtime);
    let startup_handle = tokio::spawn(async move {
        tokio::select! {
            r = tokio::time::timeout(Duration::from_secs(30), startup_runtime.start()) => {
                match r {
                    Ok(Ok(())) => {}
                    Ok(Err(e)) => {
                        tracing::error!(error = %e, "runtime start error");
                    }
                    Err(_) => {
                        let phase = startup_runtime.phase();
                        tracing::error!(phase = ?phase, "runtime start timed out after 30s");
                    }
                }
            }
            _ = tokio::signal::ctrl_c() => {
                tracing::info!("received SIGINT during TUI startup");
            }
        }
    });

    // Wait for startup to complete (or timeout).
    let _ = tokio::time::timeout(Duration::from_secs(35), startup_handle).await;

    let addr = server.local_addr();
    tracing::info!(%addr, "gateway ready (TUI mode)");

    // Run the TUI on the current (main) thread. This blocks until the user
    // presses `q`. Worker threads continue running the tokio runtime.
    let tui_runtime = Arc::clone(&runtime);
    let tui_log_buffer = Arc::clone(&log_buffer);

    // Spawn the TUI on a dedicated thread (since the main thread is the tokio
    // runtime thread and we need it responsive for signal handling).
    let tui_result = tokio::task::spawn_blocking(move || {
        gateway::tui::run_tui(tui_log_buffer, tui_runtime)
    })
    .await;

    match tui_result {
        Ok(Ok(())) => tracing::info!("TUI exited normally"),
        Ok(Err(e)) => tracing::error!(error = %e, "TUI error"),
        Err(e) => tracing::error!(error = %e, "TUI spawn_blocking error"),
    }

    tracing::info!("shutting down (TUI exited)");
    do_shutdown(&runtime, server).await;
    Ok(())
}

// ── Helpers ────────────────────────────────────────────────────────────────

#[allow(clippy::print_stderr, clippy::type_complexity)] // CLI argument parsing: complex tuple is local and self-contained.
fn parse_args(args: &[String]) -> Result<(Option<PathBuf>, SocketAddr, Option<String>, Option<PathBuf>), i32> {
    let mut config_path: Option<PathBuf> = None;
    let mut bind: SocketAddr = DEFAULT_BIND.parse().expect("default bind");
    let mut api_token: Option<String> = std::env::var("AMAN_API_TOKEN").ok();
    let mut soul_path: Option<PathBuf> = None;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--config" => {
                let path = args.get(i + 1).ok_or(2)?;
                config_path = Some(PathBuf::from(path));
                i += 2;
            }
            "--bind" => {
                let raw = args.get(i + 1).ok_or(2)?;
                bind = raw.parse::<SocketAddr>().map_err(|_| 2)?;
                i += 2;
            }
            "--token" => {
                let raw = args.get(i + 1).ok_or(2)?;
                api_token = Some(raw.to_owned());
                i += 2;
            }
            "--soul" => {
                let path = args.get(i + 1).ok_or(2)?;
                soul_path = Some(PathBuf::from(path));
                i += 2;
            }
            "--no-tui" => {
                // Already handled in main(); silently skip here.
                i += 1;
            }
            _ => {
                safe_eprintln!("Usage: aman [--config PATH] [--bind ADDR] [--token TOKEN] [--soul PATH] [--no-tui]");
                return Err(2);
            }
        }
    }

    Ok((config_path, bind, api_token, soul_path))
}

#[allow(clippy::print_stderr)] // Pre-tracing startup error: runtime construction failed before subscriber is wired.
fn build_runtime(
    config: config::AgentConfig,
    bind: SocketAddr,
    api_token: Option<String>,
    soul_path: Option<PathBuf>,
    handle: tokio::runtime::Handle,
) -> Result<Arc<gateway::runtime::AgentRuntime>, i32> {
    let mut builder = AgentRuntimeBuilder::new(config)
        .with_bind_addr(bind)
        .with_api_token(api_token)
        .with_runtime_handle(handle.clone());
    if let Some(path) = soul_path {
        builder = builder.with_soul(path);
    }

    let runtime = std::thread::spawn(move || {
        // Enter the Tokio runtime context so that any tokio::spawn calls
        // inside builder.build() (e.g. source registry) find a reactor.
        let _guard = handle.enter();
        builder.build().map_err(|e| {
            safe_eprintln!("Runtime build error: {e}");
            1
        })
    })
    .join()
    .expect("build thread panicked")?;

    Ok(runtime)
}

fn write_pid_file() {
    if let Ok(home) = std::env::var("HOME") {
        let pid_path = PathBuf::from(&home).join(PID_FILE);
        if let Some(parent) = pid_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(&pid_path, std::process::id().to_string());
        tracing::info!(pid_path = %pid_path.display(), "pid file written");
    }
}

async fn do_shutdown(
    runtime: &gateway::runtime::AgentRuntime,
    server: gateway::runtime::HttpServerHandle,
) {
    let _ = runtime.publish_event(Event::new(
        "gateway:lifecycle",
        EventType::Custom("gateway:stopping".to_owned()),
        serde_json::json!({}),
    )).await;

    const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(10);

    let (force_quit_tx, mut force_quit_rx) = tokio::sync::oneshot::channel::<()>();
    tokio::spawn(async move {
        match tokio::signal::ctrl_c().await {
            Ok(()) => {
                let _ = force_quit_tx.send(());
            }
            Err(e) => {
                tracing::error!(error = %e, "failed to register second SIGINT handler");
            }
        }
    });

    tokio::select! {
        _ = &mut force_quit_rx => {
            tracing::error!("second SIGINT received, force quitting");
            std::process::exit(1);
        }
        _ = tokio::time::sleep(SHUTDOWN_TIMEOUT) => {
            tracing::error!(
                "shutdown timed out after {}s, force exiting",
                SHUTDOWN_TIMEOUT.as_secs()
            );
            std::process::exit(1);
        }
        result = runtime.shutdown() => {
            if let Err(e) = result {
                tracing::error!(error = %e, "shutdown completed with errors");
            }
        }
    }

    server.shutdown();

    // Clean up PID file.
    if let Ok(home) = std::env::var("HOME") {
        let pid_path = PathBuf::from(&home).join(PID_FILE);
        let _ = std::fs::remove_file(pid_path);
    }

    tracing::info!("gateway shut down gracefully");
}
