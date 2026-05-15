use config::AgentConfig;
use gateway::runtime::{serve, AgentRuntimeBuilder, HttpServerConfig};
use std::process::Command;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cli_can_call_metrics_and_audit_log_and_event_dump_trace() {
    let mut config = AgentConfig::default();
    config.security.risky_capabilities_enabled = true;
    let runtime = AgentRuntimeBuilder::new(config)
        .with_bind_addr("127.0.0.1:0".parse().expect("addr"))
        .with_api_token(Some("token".to_owned()))
        .build()
        .expect("build runtime");
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

    let bin = env!("CARGO_BIN_EXE_aman");
    let addr_arg = addr.to_string();

    let status = tokio::task::spawn_blocking({
        let addr_arg = addr_arg.clone();
        move || {
            Command::new(bin)
                .args(["metrics", "--addr", &addr_arg, "--token", "token"])
                .status()
        }
    })
    .await
    .expect("join metrics")
    .expect("run metrics");
    assert!(status.success());

    let status = tokio::task::spawn_blocking({
        let addr_arg = addr_arg.clone();
        move || {
            Command::new(bin)
                .args([
                    "event",
                    "inject",
                    "--addr",
                    &addr_arg,
                    "--token",
                    "token",
                    "--operator",
                    "tester",
                    "--source",
                    "debug",
                    "--type",
                    "message_received",
                    "--payload",
                    "{\"hello\":\"world\"}",
                ])
                .status()
        }
    })
    .await
    .expect("join inject")
    .expect("run inject");
    assert!(status.success());

    let status = tokio::task::spawn_blocking({
        let addr_arg = addr_arg.clone();
        move || {
            Command::new(bin)
                .args([
                    "audit-log",
                    "--addr",
                    &addr_arg,
                    "--token",
                    "token",
                    "--action",
                    "event.inject",
                ])
                .status()
        }
    })
    .await
    .expect("join audit")
    .expect("run audit");
    assert!(status.success());

    server.shutdown();
}

