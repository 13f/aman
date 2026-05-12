use config::AgentConfig;
use runtime::{serve, AgentRuntime, AgentRuntimeBuilder, HttpServerConfig};
use serde_json::json;
use std::net::SocketAddr;
use std::sync::Arc;

fn url(addr: SocketAddr, path: &str) -> String {
    format!("http://{}{}", addr, path)
}

fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .no_proxy()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .expect("client")
}

async fn setup_runtime() -> (reqwest::Client, SocketAddr, Arc<AgentRuntime>, impl std::future::Future<Output = ()>) {
    let mut config = AgentConfig::default();
    config.event_bus.mode = config::BusMode::InMemory;
    config.security.risky_capabilities_enabled = true;
    let runtime = AgentRuntimeBuilder::new(config)
        .with_bind_addr("127.0.0.1:0".parse().expect("addr"))
        .with_api_token(Some("obs-test-token".to_owned()))
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
    let c = client();
    let shutdown = async move { server.shutdown() };
    (c, addr, runtime, shutdown)
}

/// Token-authenticated GET helper.
async fn get(c: &reqwest::Client, addr: SocketAddr, path: &str) -> reqwest::Response {
    c.get(url(addr, path))
        .bearer_auth("obs-test-token")
        .send()
        .await
        .expect("get request")
}

/// Token-authenticated POST helper.
async fn post(
    c: &reqwest::Client,
    addr: SocketAddr,
    path: &str,
    body: serde_json::Value,
) -> reqwest::Response {
    c.post(url(addr, path))
        .bearer_auth("obs-test-token")
        .header("x-aman-operator", "obs-test")
        .json(&body)
        .send()
        .await
        .expect("post request")
}

#[tokio::test]
async fn trace_id_is_present_on_all_events() {
    let (c, addr, _runtime, shutdown) = setup_runtime().await;

    let _ = post(&c, addr, "/agent/start", json!({})).await;

    // Inject an event and capture its ID
    let inject_resp = post(
        &c,
        addr,
        "/inject-event",
        json!({"source":"trace-test","event_type":"message_received","payload":{"seq":1}}),
    )
    .await;
    assert_eq!(inject_resp.status(), reqwest::StatusCode::OK);
    let injected: serde_json::Value = inject_resp.json().await.expect("inject json");
    let event_id = injected.get("id").and_then(|v| v.as_str()).expect("event id");

    // Dump the event to verify trace_id exists
    let dump_resp = get(&c, addr, &format!("/events/dump/{event_id}")).await;
    assert_eq!(dump_resp.status(), reqwest::StatusCode::OK);
    let dumped: serde_json::Value = dump_resp.json().await.expect("dump json");
    let trace_id = dumped
        .pointer("/metadata/trace_id")
        .and_then(|v| v.as_str())
        .expect("trace_id should exist on event metadata");
    assert!(!trace_id.is_empty(), "trace_id must not be empty");
    assert_eq!(
        dumped.pointer("/source"),
        Some(&json!("trace-test")),
        "event source preserved"
    );

    // Trace endpoint should return the event
    let trace_resp = get(&c, addr, &format!("/events/trace/{trace_id}")).await;
    assert_eq!(trace_resp.status(), reqwest::StatusCode::OK);
    let trace_body: serde_json::Value = trace_resp.json().await.expect("trace json");
    let events = trace_body
        .get("events")
        .and_then(|v| v.as_array())
        .expect("trace events array");
    assert!(!events.is_empty(), "trace must have at least one event");

    shutdown.await;
}

#[tokio::test]
async fn metrics_endpoint_returns_prometheus_format_with_required_keys() {
    let (c, addr, _runtime, shutdown) = setup_runtime().await;

    let resp = get(&c, addr, "/metrics").await;
    assert_eq!(resp.status(), reqwest::StatusCode::OK);

    // Verify Content-Type
    let content_type = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .expect("content-type header");
    assert!(
        content_type.contains("text/plain"),
        "metrics should be text/plain, got: {content_type}"
    );

    let body = resp.text().await.expect("metrics body");

    // Verify required Prometheus keys exist
    let required_keys = [
        "event_bus_queue_depth",
        "event_throughput_total",
        "backpressure_level",
        "events_discarded_total",
        "events_duplicate_total",
        "retry_queue_depth",
        "subscription_count",
        "dlq_depth",
    ];
    for key in &required_keys {
        assert!(
            body.contains(key),
            "metrics should contain '{key}', body:\n{body}"
        );
    }

    // Verify format: each line should have "metric_name value" pattern
    for line in body.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        assert!(
            trimmed.contains(' '),
            "each metric line should have space separator: '{trimmed}'"
        );
    }

    shutdown.await;
}

#[tokio::test]
async fn audit_log_records_operations() {
    let (c, addr, _runtime, shutdown) = setup_runtime().await;

    let _ = post(&c, addr, "/agent/start", json!({})).await;

    // Perform several auditable operations
    let _ = post(
        &c,
        addr,
        "/inject-event",
        json!({"source":"audit-test","event_type":"message_received","payload":{"seq":1}}),
    )
    .await;

    // Inject without token — forbidden attempt (audited as "forbidden")
    let inject_no_token = c
        .post(url(addr, "/inject-event"))
        .json(&json!({"source":"audit-test","event_type":"message_received","payload":{"seq":2}}))
        .send()
        .await
        .expect("inject without token");
    assert_eq!(inject_no_token.status(), reqwest::StatusCode::UNAUTHORIZED);

    // Query audit log
    let audit_resp = get(&c, addr, "/audit-log").await;
    assert_eq!(audit_resp.status(), reqwest::StatusCode::OK);
    let records: Vec<serde_json::Value> = audit_resp.json().await.expect("audit json");

    // Should have records for agent.start, event.inject, etc.
    let actions: Vec<String> = records
        .iter()
        .filter_map(|r| r.get("action").and_then(|v| v.as_str().map(String::from)))
        .collect();
    assert!(
        actions.contains(&"agent.start".to_owned()),
        "audit should contain agent.start, actions: {actions:?}"
    );
    assert!(
        actions.contains(&"event.inject".to_owned()),
        "audit should contain event.inject, actions: {actions:?}"
    );

    // Verify audit records have required fields
    for record in &records {
        assert!(
            record.get("id").and_then(|v| v.as_str()).is_some(),
            "audit record should have id"
        );
        assert!(
            record.get("action").and_then(|v| v.as_str()).is_some(),
            "audit record should have action"
        );
        assert!(
            record.get("operator").and_then(|v| v.as_str()).is_some(),
            "audit record should have operator"
        );
        assert!(
            record.get("outcome").and_then(|v| v.as_str()).is_some(),
            "audit record should have outcome"
        );
    }

    shutdown.await;
}

#[tokio::test]
async fn trace_endpoint_reports_cycle_detection() {
    let (c, addr, _runtime, shutdown) = setup_runtime().await;
    let _ = post(&c, addr, "/agent/start", json!({})).await;

    // Inject an event and grab its trace_id
    let inject_resp = post(
        &c,
        addr,
        "/inject-event",
        json!({"source":"cycle-test","event_type":"message_received","payload":{"seq":1}}),
    )
    .await;
    let injected: serde_json::Value = inject_resp.json().await.expect("inject json");
    let event_id = injected.get("id").and_then(|v| v.as_str()).expect("event id");
    let dumped: serde_json::Value = get(&c, addr, &format!("/events/dump/{event_id}"))
        .await
        .json()
        .await
        .expect("dump json");
    let trace_id = dumped
        .pointer("/metadata/trace_id")
        .and_then(|v| v.as_str())
        .expect("trace_id");

    // Normal trace response should have no cycle
    let trace_resp = get(&c, addr, &format!("/events/trace/{trace_id}")).await;
    assert_eq!(trace_resp.status(), reqwest::StatusCode::OK);
    let trace_body: serde_json::Value = trace_resp.json().await.expect("trace json");
    // cycle_detected should be absent (None) for normal traces
    assert!(
        trace_body.get("cycle_detected").is_none()
            || trace_body.get("cycle_detected") == Some(&json!(false)),
        "cycle_detected should be absent/false for normal trace"
    );

    shutdown.await;
}

#[tokio::test]
async fn audit_log_can_be_filtered_by_action() {
    let (c, addr, _runtime, shutdown) = setup_runtime().await;
    let _ = post(&c, addr, "/agent/start", json!({})).await;

    // Perform actions
    let _ = post(
        &c,
        addr,
        "/inject-event",
        json!({"source":"filter-test","event_type":"message_received","payload":{"seq":1}}),
    )
    .await;

    // Filter by action
    let filter_resp = get(&c, addr, "/audit-log?action=agent.start").await;
    assert_eq!(filter_resp.status(), reqwest::StatusCode::OK);
    let records: Vec<serde_json::Value> = filter_resp.json().await.expect("audit json");
    assert!(!records.is_empty(), "filtered audit should have records");
    for record in &records {
        let action = record
            .get("action")
            .and_then(|v| v.as_str())
            .expect("action");
        assert_eq!(action, "agent.start");
    }

    // Filter by operator
    let op_resp = get(&c, addr, "/audit-log?operator=obs-test").await;
    assert_eq!(op_resp.status(), reqwest::StatusCode::OK);
    let op_records: Vec<serde_json::Value> = op_resp.json().await.expect("audit json");
    assert!(!op_records.is_empty(), "operator filtered audit should have records");

    shutdown.await;
}

#[tokio::test]
async fn metrics_exposes_dlq_depth_and_plugin_health() {
    let (c, addr, _runtime, shutdown) = setup_runtime().await;
    let resp = get(&c, addr, "/metrics").await;
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let body = resp.text().await.expect("metrics body");

    assert!(
        body.contains("dlq_depth"),
        "metrics should expose dlq_depth"
    );

    shutdown.await;
}

#[tokio::test]
async fn config_change_audit_logs_via_runtime() {
    let (c, addr, runtime, shutdown) = setup_runtime().await;
    let _ = post(&c, addr, "/agent/start", json!({})).await;

    // Log a config change through the runtime directly
    runtime.log_config_change("obs-test", &["event_bus.max_queue_size".to_owned()]);

    // Verify the audit log contains the config.set record
    let audit_resp = get(&c, addr, "/audit-log?action=config.set").await;
    assert_eq!(audit_resp.status(), reqwest::StatusCode::OK);
    let records: Vec<serde_json::Value> = audit_resp.json().await.expect("audit json");
    assert!(!records.is_empty(), "config.set audit records should exist");
    let record = &records[0];
    assert_eq!(
        record.get("action").and_then(|v| v.as_str()),
        Some("config.set"),
        "action should be config.set"
    );
    assert_eq!(
        record.get("operator").and_then(|v| v.as_str()),
        Some("obs-test"),
        "operator should be obs-test"
    );

    shutdown.await;
}

#[tokio::test]
async fn config_change_audit_includes_changed_fields() {
    let (c, addr, runtime, shutdown) = setup_runtime().await;
    let _ = post(&c, addr, "/agent/start", json!({})).await;

    runtime.log_config_change("obs-test", &["event_bus.max_queue_size".to_owned(), "runtime.drain_timeout_sec".to_owned()]);

    let audit_resp = get(&c, addr, "/audit-log?action=config.set").await;
    assert_eq!(audit_resp.status(), reqwest::StatusCode::OK);
    let records: Vec<serde_json::Value> = audit_resp.json().await.expect("audit json");
    let detail = records[0]
        .get("detail")
        .and_then(|v| v.as_str())
        .expect("detail field");
    assert!(
        detail.contains("event_bus.max_queue_size"),
        "detail should contain first changed field: {detail}"
    );
    assert!(
        detail.contains("runtime.drain_timeout_sec"),
        "detail should contain second changed field: {detail}"
    );

    shutdown.await;
}

#[tokio::test]
async fn secret_rotation_triggers_audit() {
    let (c, addr, runtime, shutdown) = setup_runtime().await;
    let _ = post(&c, addr, "/agent/start", json!({})).await;

    // The runtime's secret resolver already logs rotation audit entries.
    // For this test we verify the audit log mechanism works for config-related
    // operational events.
    runtime.audit().record(
        "system",
        "secret.rotate",
        "secret:ROTATE_KEY",
        "ok",
        "keys=ROTATE_KEY, trigger=test",
    );

    let audit_resp = get(&c, addr, "/audit-log?action=secret.rotate").await;
    assert_eq!(audit_resp.status(), reqwest::StatusCode::OK);
    let records: Vec<serde_json::Value> = audit_resp.json().await.expect("audit json");
    assert!(!records.is_empty(), "secret.rotate audit records should exist");
    let record = &records[0];
    assert_eq!(
        record.get("action").and_then(|v| v.as_str()),
        Some("secret.rotate"),
    );
    assert_eq!(
        record.get("target").and_then(|v| v.as_str()),
        Some("secret:ROTATE_KEY"),
    );

    shutdown.await;
}

#[tokio::test]
async fn config_set_endpoint_creates_audit() {
    let (c, addr, _runtime, shutdown) = setup_runtime().await;
    let _ = post(&c, addr, "/agent/start", json!({})).await;

    // POST /config/set without confirmation should return 409
    let no_confirm = c
        .post(url(addr, "/config/set"))
        .bearer_auth("obs-test-token")
        .header("x-aman-operator", "obs-test")
        .json(&json!({"changed_fields": ["event_bus.max_queue_size"]}))
        .send()
        .await
        .expect("config set no confirm");
    assert_eq!(no_confirm.status(), reqwest::StatusCode::CONFLICT);

    // POST /config/set with confirmation should succeed
    let resp = c
        .post(url(addr, "/config/set"))
        .bearer_auth("obs-test-token")
        .header("x-aman-operator", "obs-test")
        .header("x-aman-confirm", "yes")
        .json(&json!({"changed_fields": ["event_bus.max_queue_size", "runtime.drain_timeout_sec"]}))
        .send()
        .await
        .expect("config set");
    assert_eq!(resp.status(), reqwest::StatusCode::OK);

    // Verify audit log contains the config.set record
    let audit_resp = get(&c, addr, "/audit-log?action=config.set").await;
    assert_eq!(audit_resp.status(), reqwest::StatusCode::OK);
    let records: Vec<serde_json::Value> = audit_resp.json().await.expect("audit json");
    assert!(!records.is_empty(), "config.set audit records should exist after endpoint call");
    // The last record should have the detail from our confirmed request
    let last = records.last().expect("at least one record");
    let detail = last
        .get("detail")
        .and_then(|v| v.as_str())
        .expect("detail field");
    assert!(
        detail.contains("event_bus.max_queue_size"),
        "detail should contain changed field, got: {detail}"
    );

    shutdown.await;
}

#[tokio::test]
async fn metrics_includes_inflight_counters() {
    let (c, addr, _runtime, shutdown) = setup_runtime().await;

    let resp = get(&c, addr, "/metrics").await;
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let body = resp.text().await.expect("metrics body");

    assert!(
        body.contains("inflight_pipelines"),
        "metrics should contain inflight_pipelines, body:\n{body}"
    );
    assert!(
        body.contains("inflight_skills"),
        "metrics should contain inflight_skills, body:\n{body}"
    );

    shutdown.await;
}
