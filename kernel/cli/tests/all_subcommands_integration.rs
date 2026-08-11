// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

mod common;

use config::AgentConfig;
use gateway::runtime::{serve, AgentRuntimeBuilder, Agenverse, HttpServerConfig};
use serde_json::json;
use std::fs;
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

fn run_ok(args: &[&str]) {
    let bin = common::aman_cli_bin();
    let status = Command::new(bin).args(args).status().expect("run");
    assert!(status.success(), "expected success: {:?}", args);
}

fn run_exit(args: &[&str]) -> i32 {
    let bin = common::aman_cli_bin();
    let out = Command::new(bin).args(args).output().expect("run");
    out.status.code().unwrap_or(-1)
}

fn run_stdout(args: &[&str]) -> String {
    let bin = common::aman_cli_bin();
    let out = Command::new(bin).args(args).output().expect("run");
    assert!(out.status.success(), "expected success: {:?}", args);
    String::from_utf8_lossy(&out.stdout).to_string()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cli_smoke_all_current_subcommands() {
    let mut config = AgentConfig::default();
    config.security.risky_capabilities_enabled = true;
    let agenverse = Arc::new(Agenverse::new(Duration::from_millis(0), Duration::from_secs(720)));
    let runtime = AgentRuntimeBuilder::new(config)
        .with_bind_addr("127.0.0.1:0".parse().expect("addr"))
        .with_api_token(Some("token".to_owned()))
        .with_runtime_handle(tokio::runtime::Handle::current())
        .build(Arc::clone(&agenverse))
        .expect("build runtime");
    agenverse.set_runtime(Arc::clone(&runtime));
    let skills_dir = runtime.runtime_dir().join("skills");
    fs::create_dir_all(&skills_dir).expect("create skills dir");

    let server = serve(
        runtime.clone(),
        HttpServerConfig {
            bind: runtime.bind_addr(),
        },
    )
    .await
    .expect("serve");
    let addr = server.local_addr().to_string();

    assert_eq!(
        run_exit(&["agent", "shutdown", "--addr", &addr, "--token", "token"]),
        3
    );
    run_ok(&[
        "agent",
        "start",
        "--addr",
        &addr,
        "--token",
        "token",
        "--operator",
        "tester",
    ]);
    run_ok(&["health", "ready", "--addr", &addr, "--token", "token"]);
    run_ok(&["metrics", "--addr", &addr, "--token", "token"]);
    run_ok(&["audit-log", "--addr", &addr, "--token", "token"]);

    assert_eq!(
        run_exit(&[
            "source",
            "pause",
            "--id",
            "timer:test",
            "--addr",
            &addr,
            "--token",
            "token",
            "--operator",
            "tester",
        ]),
        1
    );
    assert_eq!(
        run_exit(&[
            "source",
            "resume",
            "--id",
            "timer:test",
            "--addr",
            &addr,
            "--token",
            "token",
            "--operator",
            "tester",
        ]),
        1
    );
    assert_eq!(
        run_exit(&[
            "source",
            "config",
            "--id",
            "timer:test",
            "--json",
            "{\"interval_ms\":500}",
            "--addr",
            &addr,
            "--token",
            "token",
            "--operator",
            "tester",
        ]),
        1
    );

    run_ok(&[
        "cron",
        "add",
        "--id",
        "cron:test",
        "--expression",
        "0/5 * * * * *",
        "--addr",
        &addr,
        "--token",
        "token",
        "--operator",
        "tester",
    ]);
    run_ok(&[
        "cron",
        "update",
        "--id",
        "cron:test",
        "--json",
        "{\"timezone\":\"UTC\"}",
        "--addr",
        &addr,
        "--token",
        "token",
        "--operator",
        "tester",
    ]);
    run_ok(&[
        "cron",
        "remove",
        "--id",
        "cron:test",
        "--addr",
        &addr,
        "--token",
        "token",
        "--operator",
        "tester",
    ]);

    run_ok(&["plugin", "list", "--addr", &addr, "--token", "token"]);
    assert_eq!(
        run_exit(&[
            "plugin",
            "enable",
            "--name",
            "nope",
            "--addr",
            &addr,
            "--token",
            "token"
        ]),
        1
    );
    assert_eq!(
        run_exit(&[
            "plugin",
            "disable",
            "--name",
            "nope",
            "--confirm",
            "--addr",
            &addr,
            "--token",
            "token"
        ]),
        1
    );

    run_ok(&["skill", "list", "--addr", &addr, "--token", "token"]);
    run_ok(&[
        "skill",
        "search",
        "--q",
        "echo",
        "--addr",
        &addr,
        "--token",
        "token",
    ]);

    run_ok(&["workflow", "list", "--addr", &addr, "--token", "token"]);
    assert_eq!(
        run_exit(&[
            "workflow",
            "show",
            "--id",
            "nope",
            "--addr",
            &addr,
            "--token",
            "token"
        ]),
        1
    );
    assert_eq!(
        run_exit(&[
            "workflow",
            "retry",
            "--id",
            "nope",
            "--addr",
            &addr,
            "--token",
            "token",
            "--operator",
            "tester"
        ]),
        3
    );
    assert_eq!(
        run_exit(&[
            "workflow",
            "cancel",
            "--id",
            "nope",
            "--addr",
            &addr,
            "--token",
            "token",
            "--operator",
            "tester"
        ]),
        3
    );

    let inject = run_stdout(&[
        "event",
        "inject",
        "--source",
        "debug",
        "--type",
        "message_received",
        "--payload",
        "{\"k\":1}",
        "--addr",
        &addr,
        "--token",
        "token",
        "--operator",
        "tester",
    ]);
    let injected = serde_json::from_str::<serde_json::Value>(&inject).expect("inject json");
    let id = injected.get("id").and_then(|v| v.as_str()).expect("id");
    let dump = run_stdout(&[
        "event",
        "dump",
        "--id",
        id,
        "--addr",
        &addr,
        "--token",
        "token",
    ]);
    let dumped = serde_json::from_str::<serde_json::Value>(&dump).expect("dump json");
    let trace_id = dumped
        .pointer("/metadata/trace_id")
        .and_then(|v| v.as_str())
        .expect("trace_id");
    run_ok(&[
        "event",
        "trace",
        "--trace-id",
        trace_id,
        "--addr",
        &addr,
        "--token",
        "token",
    ]);

    let dlq_id = runtime
        .enqueue_dlq(
            kernel::event::Event::new("pipeline:test", kernel::event::EventType::MessageReceived, json!({"seq": 1})),
            "PipelineFailed",
            30,
        )
        .expect("enqueue dlq");
    run_ok(&["dlq", "list", "--addr", &addr, "--token", "token"]);
    run_ok(&[
        "dlq",
        "retry",
        "--id",
        &dlq_id,
        "--confirm",
        "--addr",
        &addr,
        "--token",
        "token",
        "--operator",
        "tester",
    ]);
    run_ok(&[
        "dlq",
        "discard",
        "--id",
        &dlq_id,
        "--addr",
        &addr,
        "--token",
        "token",
        "--operator",
        "tester",
    ]);

    run_ok(&["config", "show"]);

    run_ok(&[
        "agent",
        "shutdown",
        "--confirm",
        "--addr",
        &addr,
        "--token",
        "token",
        "--operator",
        "tester",
    ]);

    server.shutdown().await;
}
