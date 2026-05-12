use config::AgentConfig;
use runtime::{serve, AgentRuntimeBuilder, HttpServerConfig};
use serde_json::json;
use std::fs;

fn url(addr: std::net::SocketAddr, path: &str) -> String {
    format!("http://{}{}", addr, path)
}

fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .no_proxy()
        .build()
        .expect("client")
}

#[tokio::test]
async fn events_dump_and_trace_and_audit_log_work() {
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

    let start = client()
        .post(url(addr, "/agent/start"))
        .bearer_auth("token")
        .send()
        .await
        .expect("start");
    assert_eq!(start.status(), reqwest::StatusCode::OK);

    let inject = client()
        .post(url(addr, "/inject-event"))
        .bearer_auth("token")
        .header("x-aman-operator", "tester")
        .json(&json!({"source":"debug","event_type":"message_received","payload":{"k":1}}))
        .send()
        .await
        .expect("inject");
    assert_eq!(inject.status(), reqwest::StatusCode::OK);
    let injected = inject.json::<serde_json::Value>().await.expect("json");
    let id = injected
        .get("id")
        .and_then(|value| value.as_str())
        .expect("id")
        .to_owned();

    let dump = client()
        .get(url(addr, &format!("/events/dump/{id}")))
        .bearer_auth("token")
        .send()
        .await
        .expect("dump");
    assert_eq!(dump.status(), reqwest::StatusCode::OK);
    let dumped = dump.json::<serde_json::Value>().await.expect("dump json");
    assert_eq!(dumped.get("id").and_then(|v| v.as_str()), Some(id.as_str()));
    let trace_id = dumped
        .pointer("/metadata/trace_id")
        .and_then(|v| v.as_str())
        .expect("trace_id")
        .to_owned();

    let trace = client()
        .get(url(addr, &format!("/events/trace/{trace_id}")))
        .bearer_auth("token")
        .send()
        .await
        .expect("trace");
    assert_eq!(trace.status(), reqwest::StatusCode::OK);
    let traced = trace.json::<serde_json::Value>().await.expect("trace json");
    let events = traced
        .get("events")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    assert!(events
        .iter()
        .any(|e| e.get("id").and_then(|v| v.as_str()) == Some(id.as_str())));

    let audit = client()
        .get(url(addr, "/audit-log?action=event.inject"))
        .bearer_auth("token")
        .send()
        .await
        .expect("audit");
    assert_eq!(audit.status(), reqwest::StatusCode::OK);
    let items = audit.json::<serde_json::Value>().await.expect("audit json");
    let items = items.as_array().cloned().unwrap_or_default();
    assert!(items.iter().any(|item| {
        item.get("operator").and_then(|v| v.as_str()) == Some("tester")
            && item.get("action").and_then(|v| v.as_str()) == Some("event.inject")
            && item.get("outcome").and_then(|v| v.as_str()) == Some("ok")
    }));

    let shutdown_need_confirm = client()
        .post(url(addr, "/agent/shutdown"))
        .bearer_auth("token")
        .send()
        .await
        .expect("shutdown");
    assert_eq!(shutdown_need_confirm.status(), reqwest::StatusCode::CONFLICT);

    server.shutdown();
}

#[test]
fn with_soul_injects_into_tool_context() {
    let temp_dir = std::env::temp_dir().join("aman-runtime-with-soul-test");
    let _ = std::fs::create_dir_all(&temp_dir);
    let soul_file = temp_dir.join("SOUL.md");
    std::fs::write(
        &soul_file,
        "# Aman\n## identity\n- test\n## boundaries\n- never leak secrets\n",
    )
    .expect("write soul");

    let config = AgentConfig::default();
    let runtime = AgentRuntimeBuilder::new(config)
        .with_bind_addr("127.0.0.1:0".parse().expect("addr"))
        .with_soul(soul_file)
        .build()
        .expect("build runtime");

    let ctx = kernel::context::ToolContext {
        base: kernel::context::BaseContext::new(kernel::types::TraceId::new()),
        tool_name: Some("test".to_owned()),
        working_directory: None,
    };
    let injected = runtime.inject_tool_context(ctx);
    assert!(injected.base.extensions.contains_key("soul.system_prompt"));
    assert_eq!(
        injected
            .base
            .extensions
            .get("soul.name")
            .and_then(serde_json::Value::as_str),
        Some("Aman")
    );
}

#[tokio::test]
async fn inject_event_is_forbidden_when_disabled() {
    let mut config = AgentConfig::default();
    config.security.risky_capabilities_enabled = false;
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

    let res = client()
        .post(url(addr, "/inject-event"))
        .bearer_auth("token")
        .json(&json!({"source":"debug","event_type":"message_received","payload":{"k":1}}))
        .send()
        .await
        .expect("inject");
    assert_eq!(res.status(), reqwest::StatusCode::FORBIDDEN);

    server.shutdown();
}

#[tokio::test]
async fn dlq_retry_republishes_event_and_metrics_exposes_throughput() {
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

    let before = client()
        .get(url(addr, "/metrics"))
        .send()
        .await
        .expect("metrics");
    assert_eq!(before.status(), reqwest::StatusCode::OK);
    let before_text = before.text().await.expect("metrics body");

    let event = kernel::event::Event::new(
        "pipeline:test",
        kernel::event::EventType::MessageReceived,
        json!({"seq": 1}),
    );
    let id = runtime
        .enqueue_dlq(event, "PipelineFailed", 30)
        .expect("enqueue dlq");

    let list = client()
        .get(url(addr, "/dlq"))
        .bearer_auth("token")
        .send()
        .await
        .expect("dlq list");
    assert_eq!(list.status(), reqwest::StatusCode::OK);

    let retry = client()
        .post(url(addr, &format!("/dlq/{id}/retry")))
        .bearer_auth("token")
        .header("x-aman-operator", "tester")
        .header("x-aman-confirm", "yes")
        .json(&json!({"reason":"requeue"}))
        .send()
        .await
        .expect("dlq retry");
    assert_eq!(retry.status(), reqwest::StatusCode::OK);

    let after = client()
        .get(url(addr, "/metrics"))
        .send()
        .await
        .expect("metrics");
    let after_text = after.text().await.expect("metrics body");
    assert!(before_text.contains("event_throughput_total"));
    assert!(after_text.contains("event_throughput_total"));

    let discard = client()
        .post(url(addr, &format!("/dlq/{id}/discard")))
        .bearer_auth("token")
        .header("x-aman-operator", "tester")
        .send()
        .await
        .expect("dlq discard");
    assert_eq!(discard.status(), reqwest::StatusCode::OK);

    server.shutdown();
}

#[tokio::test]
async fn cron_add_update_remove_persists_override_file() {
    let config = AgentConfig::default();
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

    let res = client()
        .post(url(addr, "/cron/add"))
        .bearer_auth("token")
        .header("x-aman-operator", "tester")
        .json(&json!({"id":"job1","expression":"*/5 * * * *"}))
        .send()
        .await
        .expect("cron add");
    assert_eq!(res.status(), reqwest::StatusCode::OK);

    let res = client()
        .post(url(addr, "/cron/job1/update"))
        .bearer_auth("token")
        .json(&json!({"timezone":"UTC"}))
        .send()
        .await
        .expect("cron update");
    assert_eq!(res.status(), reqwest::StatusCode::OK);

    let override_path = runtime.runtime_dir().join("cron_override.yaml");
    let content = fs::read_to_string(&override_path).expect("override file");
    assert!(content.contains("job1"));

    let res = client()
        .post(url(addr, "/cron/job1/remove"))
        .bearer_auth("token")
        .send()
        .await
        .expect("cron remove");
    assert_eq!(res.status(), reqwest::StatusCode::OK);

    let content = fs::read_to_string(&override_path).expect("override file");
    assert!(content.contains("job1"));
    assert!(content.contains("removed: true"));

    server.shutdown();
}

#[tokio::test]
async fn plugin_uninstall_requires_token_and_removes_files() {
    let config = AgentConfig::default();
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

    let plugin_dir = runtime.runtime_dir().join("plugins").join("demo");
    fs::create_dir_all(&plugin_dir).expect("create plugin dir");
    fs::write(plugin_dir.join("plugin.yaml"), "name: demo\nversion: 0.1.0\n").expect("write");

    let unauth = client()
        .post(url(addr, "/plugin/demo/uninstall"))
        .send()
        .await
        .expect("uninstall");
    assert_eq!(unauth.status(), reqwest::StatusCode::UNAUTHORIZED);

    let ok = client()
        .post(url(addr, "/plugin/demo/uninstall"))
        .bearer_auth("token")
        .send()
        .await
        .expect("uninstall");
    assert_eq!(ok.status(), reqwest::StatusCode::OK);
    assert!(!plugin_dir.exists());

    server.shutdown();
}
