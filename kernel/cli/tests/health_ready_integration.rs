// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

mod common;

use config::AgentConfig;
use gateway::runtime::{serve, AgentRuntimeBuilder, Agenverse, HttpServerConfig};
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn aman_health_ready_hits_runtime_endpoint() {
    let config = AgentConfig::default();
    let agenverse = Arc::new(Agenverse::new(Duration::from_millis(0)));
    let runtime = AgentRuntimeBuilder::new(config)
        .with_bind_addr("127.0.0.1:0".parse().expect("addr"))
        .with_runtime_handle(tokio::runtime::Handle::current())
        .build(Arc::clone(&agenverse))
        .expect("build runtime");
    agenverse.set_runtime(Arc::clone(&runtime));
    let server = serve(
        runtime.clone(),
        HttpServerConfig {
            bind: runtime.bind_addr(),
        },
    )
    .await
    .expect("serve");
    let addr = server.local_addr();

    runtime.start().await.expect("start runtime");

    let bin = common::aman_cli_bin();
    let addr_arg = addr.to_string();
    let status = tokio::task::spawn_blocking(move || {
        Command::new(bin)
            .args(["health", "ready", "--addr", &addr_arg])
            .status()
    })
    .await
    .expect("join cli")
    .expect("run cli");
    assert!(status.success());

    server.shutdown();
}
