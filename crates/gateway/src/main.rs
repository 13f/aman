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
//!   gateway [--config PATH] [--bind ADDR] [--token TOKEN] [--soul PATH]

use config::ConfigLoader;
use gateway::runtime::{serve, AgentRuntimeBuilder, HttpServerConfig};
use kernel::event::{Event, EventType};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

const DEFAULT_BIND: &str = "127.0.0.1:9999";
const PID_FILE: &str = ".aman/gateway.pid";

#[tokio::main(flavor = "multi_thread")]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::builder()
                .with_default_directive(tracing::level_filters::LevelFilter::INFO.into())
                .from_env_lossy(),
        )
        .init();

    if let Err(code) = run().await {
        std::process::exit(code);
    }
}

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
                eprintln!("Usage: gateway [--config PATH] [--bind ADDR] [--token TOKEN] [--soul PATH]");
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
        builder.build().map_err(|e| {
            eprintln!("Runtime build error: {e}");
            1
        })?,
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

    // Start runtime with a timeout so the process doesn't hang forever
    // if a startup phase gets stuck (e.g. Phase 4 source init).
    match tokio::time::timeout(Duration::from_secs(30), runtime.start()).await {
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

    let addr = server.local_addr();
    tracing::info!(%addr, "gateway ready");

    let _ = runtime.publish_event(Event::new(
        "gateway:lifecycle",
        EventType::Custom("gateway:ready".to_owned()),
        serde_json::json!({"bind": bind.to_string(), "addr": addr.to_string()}),
    )).await;

    // Wait for shutdown signal.
    #[cfg(unix)]
    {
        let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .map_err(|_| 1)?;

        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                tracing::info!("received SIGINT, shutting down");
            }
            _ = sigterm.recv() => {
                tracing::info!("received SIGTERM, shutting down");
            }
        }
    }

    #[cfg(not(unix))]
    {
        tokio::signal::ctrl_c().await.map_err(|_| 1)?;
        tracing::info!("received SIGINT, shutting down");
    }

    let _ = runtime.publish_event(Event::new(
        "gateway:lifecycle",
        EventType::Custom("gateway:stopping".to_owned()),
        serde_json::json!({}),
    )).await;
    let _ = runtime.shutdown().await;
    server.shutdown();

    // Clean up PID file.
    if let Ok(home) = std::env::var("HOME") {
        let pid_path = PathBuf::from(&home).join(PID_FILE);
        let _ = std::fs::remove_file(pid_path);
    }

    tracing::info!("gateway shut down gracefully");
    Ok(())
}
