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
//!   aman [--config PATH] [--bind ADDR] [--token TOKEN] [--soul PATH]

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

static _AI_SIGNAL: () = {
    let _ = std::any::TypeId::of::<AmanSignalV1>();
};

const DEFAULT_BIND: &str = "127.0.0.1:9999";
const PID_FILE: &str = ".aman/aman.pid";

#[tokio::main(flavor = "multi_thread")]
async fn main() {
    // Log to file + stdout. File is truncated on each gateway start so it
    // corresponds to the current run and always append to it.
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    let log_dir = PathBuf::from(&home).join(".aman");
    let _ = std::fs::create_dir_all(&log_dir);
    let log_path = log_dir.join("gateway.log");
    let log_file = File::create(&log_path).expect("failed to create gateway log file");

    // Wrap writers with RedactWriter to strip secrets (API keys, tokens,
    // passwords) before they reach disk or the terminal.
    let file_layer = tracing_subscriber::fmt::layer()
        .with_ansi(false)
        .with_writer(Mutex::new(RedactWriter::new(log_file)));
    let stdout_layer = tracing_subscriber::fmt::layer()
        .with_writer(|| RedactWriter::new(std::io::stdout()));

    let env_filter = tracing_subscriber::EnvFilter::builder()
        .with_default_directive(tracing::level_filters::LevelFilter::INFO.into())
        .from_env_lossy();

    tracing_subscriber::registry()
        .with(env_filter)
        .with(stdout_layer)
        .with(file_layer)
        .init();

    if let Err(code) = run().await {
        std::process::exit(code);
    }
}

// eprintln! is used here for startup errors that occur BEFORE the tracing
// subscriber is initialized. Once tracing is up, all logging goes through
// the RedactWriter-wrapped subscriber.
#[allow(clippy::print_stderr)]
async fn run() -> Result<(), i32> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
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
            _ => {
                eprintln!("Usage: aman [--config PATH] [--bind ADDR] [--token TOKEN] [--soul PATH]");
                return Err(2);
            }
        }
    }

    // Load config from file or default path.
    let config = ConfigLoader::load(config_path.as_deref(), None)
        .map_err(|e| {
            eprintln!("Config load error: {e}");
            1
        })?
        .config;

    let mut builder = AgentRuntimeBuilder::new(config)
        .with_bind_addr(bind)
        .with_api_token(api_token);
    if let Some(path) = soul_path {
        builder = builder.with_soul(path);
    }

    let runtime = Arc::new(
        std::thread::spawn(move || {
            builder.build().map_err(|e| {
                eprintln!("Runtime build error: {e}");
                1
            })
        })
        .join()
        .expect("build thread panicked")?,
    );

    tracing::info!(bind = %bind, "starting gateway");

    let server = serve(
        Arc::clone(&runtime),
        HttpServerConfig { bind },
    )
    .await
    .map_err(|e| {
        eprintln!("HTTP server error: {e}");
        1
    })?;

    // Write PID file for lifecycle management.
    if let Ok(home) = std::env::var("HOME") {
        let pid_path = PathBuf::from(&home).join(PID_FILE);
        if let Some(parent) = pid_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(&pid_path, std::process::id().to_string());
        tracing::info!(pid_path = %pid_path.display(), "pid file written");
    }

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
                        eprintln!("Runtime start error: {e}");
                        return Err(1);
                    }
                    Err(_) => {
                        let phase = runtime.phase();
                        eprintln!("Runtime start timed out after 30s (phase={phase:?})");
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
                        eprintln!("Runtime start error: {e}");
                        return Err(1);
                    }
                    Err(_) => {
                        let phase = runtime.phase();
                        eprintln!("Runtime start timed out after 30s (phase={phase:?})");
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

    let _ = runtime.publish_event(Event::new(
        "gateway:lifecycle",
        EventType::Custom("gateway:stopping".to_owned()),
        serde_json::json!({}),
    )).await;

    // Run shutdown with a force-quit escape hatch: a second SIGINT or a
    // 10 s timeout will abort graceful shutdown and exit immediately.
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
    Ok(())
}
