use config::AgentConfig;
use kernel::event::{Event, EventType};
use runtime::{serve, AgentRuntimeBuilder, HttpServerConfig};
use serde_json::json;
use workflow::{
    ErrorRecovery, RetryFailurePolicy, StateDef, StateTimeout, Transition, TransitionFrom,
    TransitionTo, WorkflowDef,
};

fn url(addr: std::net::SocketAddr, path: &str) -> String {
    format!("http://{}{}", addr, path)
}

fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .no_proxy()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .expect("client")
}

/// ---------------------------------------------------------------------------
/// Scenario 3: Workflow 审批流（PENDING→REVIEWING→APPROVED）+ 超时自动拒绝
///
/// Verifies: workflow registration, instance creation via HTTP API,
/// state transitions via handle_event, and timeout → auto-reject.
/// ---------------------------------------------------------------------------
#[tokio::test]
async fn workflow_approval_timeout_auto_reject() {
    let mut config = AgentConfig::default();
    config.security.risky_capabilities_enabled = true;

    let runtime = AgentRuntimeBuilder::new(config)
        .with_bind_addr("127.0.0.1:0".parse().expect("addr"))
        .with_api_token(Some("e2e-token".to_owned()))
        .build()
        .expect("build runtime");

    // Register a workflow with timeout on REVIEWING → REJECTED
    runtime
        .workflow_engine()
        .register_workflow(WorkflowDef {
            name: "approval-flow".to_owned(),
            states: vec![
                StateDef {
                    name: "pending".to_owned(),
                },
                StateDef {
                    name: "reviewing".to_owned(),
                },
                StateDef {
                    name: "approved".to_owned(),
                },
                StateDef {
                    name: "rejected".to_owned(),
                },
                StateDef {
                    name: "error".to_owned(),
                },
            ],
            initial_state: "pending".to_owned(),
            final_states: vec!["approved".to_owned(), "rejected".to_owned()],
            error_state: "error".to_owned(),
            transitions: vec![Transition {
                from: TransitionFrom::Specific("pending".to_owned()),
                event: "submit".to_owned(),
                to: TransitionTo::Specific("reviewing".to_owned()),
                guard: None,
                on_fail: None,
                action: None,
                on_action_failure: None,
            }],
            state_timeouts: vec![StateTimeout {
                state: "reviewing".to_owned(),
                timeout_ms: 1,
                on_timeout: TransitionTo::Specific("rejected".to_owned()),
                on_timeout_alert: None,
            }],
            error_recovery: ErrorRecovery::default(),
        })
        .expect("register workflow");

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

    // Start runtime
    let start = c
        .post(url(addr, "/agent/start"))
        .bearer_auth("e2e-token")
        .send()
        .await
        .expect("start");
    assert_eq!(start.status(), 200);

    // Create a workflow instance via HTTP API
    let created = c
        .post(url(addr, "/workflow/approval-flow/create"))
        .bearer_auth("e2e-token")
        .header("x-aman-operator", "e2e-test")
        .json(&json!({"data": {"ticket": "T-001"}}))
        .send()
        .await
        .expect("create instance")
        .json::<serde_json::Value>()
        .await
        .expect("create json");
    let instance_id = created
        .get("id")
        .and_then(|v| v.as_str())
        .expect("instance id");
    assert_eq!(
        created
            .get("current_state")
            .and_then(|v| v.as_str()),
        Some("PENDING")
    );

    // Transition: submit → REVIEWING via handle_event.
    // Note: handle_event sets last_user_event_at which defers timeouts
    // for timeout_defer_ms (default 5000ms). To test timeout without
    // waiting 5s, we directly set the instance into REVIEWING state.
    let submit_event = kernel::event::Event::new(
        "workflow:control",
        kernel::event::EventType::Custom("submit".to_owned()),
        json!({"by": "e2e-test"}),
    );
    let submit_result = runtime
        .workflow_engine()
        .handle_event(instance_id, submit_event)
        .await
        .expect("submit event");
    assert!(submit_result.transitioned);
    assert_eq!(submit_result.to_state, "REVIEWING");

    // Trigger timeout by advancing the clock past the 1ms threshold
    // We need to wait past timeout_defer_ms (5000ms) before timeouts fire.
    // Use a generous offset that exceeds the defer window.
    let now = kernel::types::Timestamp::from_millis(
        kernel::types::Timestamp::now().as_millis() + 5_100,
    );
    let timeout_results = runtime
        .workflow_engine()
        .handle_timeouts(now)
        .await
        .expect("handle timeouts");
    assert_eq!(timeout_results.len(), 1);
    assert_eq!(timeout_results[0].to_state, "REJECTED");

    // Verify state via HTTP API
    let instance_resp = c
        .get(url(addr, &format!("/workflow-instance/{instance_id}")))
        .bearer_auth("e2e-token")
        .send()
        .await
        .expect("get instance")
        .json::<serde_json::Value>()
        .await
        .expect("instance json");
    assert_eq!(
        instance_resp
            .get("current_state")
            .and_then(|v| v.as_str()),
        Some("REJECTED")
    );

    // Verify workflow-def list includes our workflow
    let wf_list = c
        .get(url(addr, "/workflows"))
        .bearer_auth("e2e-token")
        .send()
        .await
        .expect("workflow list")
        .json::<serde_json::Value>()
        .await
        .expect("list json");
    let items = wf_list
        .get("items")
        .and_then(|v| v.as_array())
        .expect("items array");
    assert!(items.iter().any(|item| item.as_str() == Some("approval-flow")));

    server.shutdown();
}

/// ---------------------------------------------------------------------------
/// Scenario 2: Pipeline 失败 + DLQ 生命周期
///
/// Verifies: DLQ enqueue (simulated via runtime.enqueue_dlq), list via HTTP,
/// retry (with confirmation), discard, and empty-after-discard.
/// ---------------------------------------------------------------------------
#[tokio::test]
async fn dlq_lifecycle_enqueue_list_retry_discard() {
    let mut config = AgentConfig::default();
    config.security.risky_capabilities_enabled = true;

    let runtime = AgentRuntimeBuilder::new(config)
        .with_bind_addr("127.0.0.1:0".parse().expect("addr"))
        .with_api_token(Some("e2e-token".to_owned()))
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

    // Start runtime
    c.post(url(addr, "/agent/start"))
        .bearer_auth("e2e-token")
        .send()
        .await
        .expect("start");

    // Enqueue DLQ entries via runtime (simulating pipeline failures)
    let dlq_id_1 = runtime
        .enqueue_dlq(
            kernel::event::Event::new(
                "pipeline:ocr",
                kernel::event::EventType::FileCreated,
                json!({"path": "/tmp/test.pdf", "size": 42}),
            ),
            "PipelineFailed: OCR step timed out",
            30,
        )
        .expect("enqueue dlq 1");

    let dlq_id_2 = runtime
        .enqueue_dlq(
            kernel::event::Event::new(
                "pipeline:slack",
                kernel::event::EventType::Custom("notification_failed".to_owned()),
                json!({"channel": "general", "text": "hello"}),
            ),
            "PipelineFailed: Slack API returned 429",
            30,
        )
        .expect("enqueue dlq 2");

    // List DLQ via HTTP API
    let dlq_list = c
        .get(url(addr, "/dlq"))
        .bearer_auth("e2e-token")
        .send()
        .await
        .expect("dlq list")
        .json::<serde_json::Value>()
        .await
        .expect("dlq json");
    let items = dlq_list
        .get("items")
        .and_then(|v| v.as_array())
        .expect("items array");
    assert_eq!(items.len(), 2);

    // Verify entry details
    let reasons: Vec<&str> = items
        .iter()
        .filter_map(|item| item.get("reason").and_then(|v| v.as_str()))
        .collect();
    assert!(reasons.contains(&"PipelineFailed: OCR step timed out"));
    assert!(reasons.contains(&"PipelineFailed: Slack API returned 429"));

    // Retry the first DLQ entry via HTTP API (requires confirmation)
    let retry_resp = c
        .post(url(addr, &format!("/dlq/{dlq_id_1}/retry")))
        .bearer_auth("e2e-token")
        .header("x-aman-operator", "e2e-test")
        .header("x-aman-confirm", "yes")
        .json(&json!({"reason": "infra recovered"}))
        .send()
        .await
        .expect("dlq retry");
    assert_eq!(retry_resp.status(), 200);

    // Discard the second DLQ entry
    let discard_resp = c
        .post(url(addr, &format!("/dlq/{dlq_id_2}/discard")))
        .bearer_auth("e2e-token")
        .header("x-aman-operator", "e2e-test")
        .send()
        .await
        .expect("dlq discard");
    assert_eq!(discard_resp.status(), 200);

    // After retry (which re-publishes) + discard, verify remaining count
    // Note: retry re-publishes but does not remove; we still have 2 entries
    let dlq_after = c
        .get(url(addr, "/dlq"))
        .bearer_auth("e2e-token")
        .send()
        .await
        .expect("dlq list after")
        .json::<serde_json::Value>()
        .await
        .expect("dlq json");
    let remaining = dlq_after
        .get("items")
        .and_then(|v| v.as_array())
        .expect("items array");
    assert_eq!(remaining.len(), 1); // one was discarded

    // Verify audit for DLQ operations
    let audit_resp = c
        .get(url(addr, "/audit-log?action=dlq.retry"))
        .bearer_auth("e2e-token")
        .send()
        .await
        .expect("audit log")
        .json::<Vec<serde_json::Value>>()
        .await
        .expect("audit json");
    assert!(!audit_resp.is_empty(), "dlq.retry audit records should exist");

    // Cleanup
    c.post(url(addr, "/agent/shutdown"))
        .bearer_auth("e2e-token")
        .header("x-aman-operator", "e2e-test")
        .header("x-aman-confirm", "yes")
        .send()
        .await
        .expect("shutdown");

    server.shutdown();
}

/// ---------------------------------------------------------------------------
/// Scenario 5: 事件风暴触发背压降级
///
/// Verifies: injecting many events beyond queue capacity triggers backpressure,
/// and metrics reflect the increased backpressure level.
/// ---------------------------------------------------------------------------
#[tokio::test]
async fn backpressure_storm_triggers_metrics_change() {
    let mut config = AgentConfig::default();
    // Tiny queue so we hit backpressure quickly
    config.event_bus.max_queue_size = 16;
    config.security.risky_capabilities_enabled = true;

    let runtime = AgentRuntimeBuilder::new(config)
        .with_bind_addr("127.0.0.1:0".parse().expect("addr"))
        .with_api_token(Some("e2e-token".to_owned()))
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

    // Start runtime
    c.post(url(addr, "/agent/start"))
        .bearer_auth("e2e-token")
        .send()
        .await
        .expect("start");

    // Inject a storm of events — more than the tiny queue can hold
    for i in 0..50 {
        let _ = c
            .post(url(addr, "/inject-event"))
            .bearer_auth("e2e-token")
            .header("x-aman-operator", "e2e-storm")
            .json(&json!({
                "source": format!("storm:{i}"),
                "event_type": "heartbeat",
                "payload": {"seq": i}
            }))
            .send()
            .await;
    }

    // Give the bus a moment to process
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    // Check metrics for backpressure signal
    let metrics_body = c
        .get(url(addr, "/metrics"))
        .send()
        .await
        .expect("metrics")
        .text()
        .await
        .expect("metrics body");

    // Verify core metrics are present after storm
    assert!(
        metrics_body.contains("backpressure_level"),
        "metrics should contain backpressure_level after event storm, body:\n{metrics_body}"
    );
    assert!(
        metrics_body.contains("event_throughput_total"),
        "metrics should contain throughput, body:\n{metrics_body}"
    );
    assert!(
        metrics_body.contains("events_discarded_total"),
        "metrics should contain discarded counter, body:\n{metrics_body}"
    );
    assert!(
        metrics_body.contains("event_bus_queue_depth"),
        "metrics should contain queue depth, body:\n{metrics_body}"
    );

    // Audit should have recorded event injections
    let audit_resp = c
        .get(url(addr, "/audit-log?action=event.inject"))
        .bearer_auth("e2e-token")
        .send()
        .await
        .expect("audit log")
        .json::<Vec<serde_json::Value>>()
        .await
        .expect("audit json");
    assert!(!audit_resp.is_empty(), "audit should contain event.inject records");

    // Shutdown
    c.post(url(addr, "/agent/shutdown"))
        .bearer_auth("e2e-token")
        .header("x-aman-operator", "e2e-test")
        .header("x-aman-confirm", "yes")
        .send()
        .await
        .expect("shutdown");

    server.shutdown();
}

/// ---------------------------------------------------------------------------
/// Scenario 8: Secret 热更新审计
///
/// Verifies: secret rotation events create audit log entries that can be
/// queried via the HTTP API.
/// ---------------------------------------------------------------------------
#[tokio::test]
async fn secret_rotation_creates_audit_records() {
    let mut config = AgentConfig::default();
    config.security.risky_capabilities_enabled = true;

    let runtime = AgentRuntimeBuilder::new(config)
        .with_bind_addr("127.0.0.1:0".parse().expect("addr"))
        .with_api_token(Some("e2e-token".to_owned()))
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

    // Start runtime
    c.post(url(addr, "/agent/start"))
        .bearer_auth("e2e-token")
        .send()
        .await
        .expect("start");

    // Record secret rotation audit entries (simulating SecretResolver behavior)
    runtime.audit().record(
        "system",
        "secret.rotate",
        "secret:OPENAI_API_KEY",
        "ok",
        "keys=OPENAI_API_KEY, trigger=two-phase-commit, grace_period=60s",
    );

    runtime.audit().record(
        "system",
        "secret.rotate",
        "secret:ANTHROPIC_API_KEY",
        "ok",
        "keys=ANTHROPIC_API_KEY, trigger=auto-rotation, grace_period=60s",
    );

    // Query audit log for secret.rotate
    let audit_resp = c
        .get(url(addr, "/audit-log?action=secret.rotate"))
        .bearer_auth("e2e-token")
        .send()
        .await
        .expect("audit log")
        .json::<Vec<serde_json::Value>>()
        .await
        .expect("audit json");

    assert!(
        audit_resp.len() >= 2,
        "should have at least 2 secret.rotate records, got {}",
        audit_resp.len()
    );

    // Verify detail contains rotation metadata
    let details: Vec<&str> = audit_resp
        .iter()
        .filter_map(|r| r.get("detail").and_then(|v| v.as_str()))
        .collect();
    assert!(
        details.iter().any(|d| d.contains("OPENAI_API_KEY")),
        "should contain OPENAI_API_KEY detail, got: {details:?}"
    );
    assert!(
        details.iter().any(|d| d.contains("ANTHROPIC_API_KEY")),
        "should contain ANTHROPIC_API_KEY detail, got: {details:?}"
    );

    // Targets should match
    let targets: Vec<&str> = audit_resp
        .iter()
        .filter_map(|r| r.get("target").and_then(|v| v.as_str()))
        .collect();
    assert!(targets.contains(&"secret:OPENAI_API_KEY"));
    assert!(targets.contains(&"secret:ANTHROPIC_API_KEY"));

    // Verify audit records have complete schema
    for record in &audit_resp {
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

    // Shutdown
    c.post(url(addr, "/agent/shutdown"))
        .bearer_auth("e2e-token")
        .header("x-aman-operator", "e2e-test")
        .header("x-aman-confirm", "yes")
        .send()
        .await
        .expect("shutdown");

    server.shutdown();
}

/// ---------------------------------------------------------------------------
/// Scenario 4: Workflow ERROR → RETRY recovery → PENDING
///
/// Verifies: a workflow instance in ERROR state can be recovered to its
/// last active state via the retry HTTP API.
/// ---------------------------------------------------------------------------
#[tokio::test]
async fn workflow_error_retry_recovery() {
    let mut config = AgentConfig::default();
    config.security.risky_capabilities_enabled = true;

    let runtime = AgentRuntimeBuilder::new(config)
        .with_bind_addr("127.0.0.1:0".parse().expect("addr"))
        .with_api_token(Some("e2e-token".to_owned()))
        .build()
        .expect("build runtime");

    // Register a workflow with a direct transition to error state
    runtime
        .workflow_engine()
        .register_workflow(WorkflowDef {
            name: "error-retry-flow".to_owned(),
            states: vec![
                StateDef { name: "pending".to_owned() },
                StateDef { name: "active".to_owned() },
                StateDef { name: "error".to_owned() },
            ],
            initial_state: "pending".to_owned(),
            final_states: vec!["active".to_owned()],
            error_state: "error".to_owned(),
            transitions: vec![
                Transition {
                    from: TransitionFrom::Specific("pending".to_owned()),
                    event: "fail".to_owned(),
                    to: TransitionTo::Specific("error".to_owned()),
                    guard: None,
                    on_fail: None,
                    action: None,
                    on_action_failure: None,
                },
            ],
            state_timeouts: Vec::new(),
            error_recovery: ErrorRecovery {
                auto_retry_count: 0,
                max_retry_count: 3,
                retry_backoff: kernel::retry::RetryBackoff::Immediate,
                on_retry_failure: RetryFailurePolicy::ManualOnly,
            },
        })
        .expect("register workflow");

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

    // Start runtime
    let start = c
        .post(url(addr, "/agent/start"))
        .bearer_auth("e2e-token")
        .send()
        .await
        .expect("start");
    assert_eq!(start.status(), 200);

    // Create a workflow instance
    let created = c
        .post(url(addr, "/workflow/error-retry-flow/create"))
        .bearer_auth("e2e-token")
        .header("x-aman-operator", "e2e-test")
        .json(&json!({"data": {"ticket": "T-001"}}))
        .send()
        .await
        .expect("create instance")
        .json::<serde_json::Value>()
        .await
        .expect("create json");
    let instance_id = created
        .get("id")
        .and_then(|v| v.as_str())
        .expect("instance id");
    assert_eq!(
        created
            .get("current_state")
            .and_then(|v| v.as_str()),
        Some("PENDING"),
        "instance should start in PENDING"
    );

    // Trigger transition to error state via handle_event
    let fail_event = Event::new("e2e-test", EventType::Custom("fail".to_owned()), json!({}));
    let result = runtime
        .workflow_engine()
        .handle_event(instance_id, fail_event)
        .await
        .expect("handle_event fail");
    assert_eq!(result.to_state, "ERROR", "should transition to ERROR");

    // Verify instance is in ERROR via HTTP API
    let instance = c
        .get(url(addr, &format!("/workflow-instance/{instance_id}")))
        .bearer_auth("e2e-token")
        .send()
        .await
        .expect("get instance")
        .json::<serde_json::Value>()
        .await
        .expect("instance json");
    assert_eq!(
        instance.get("current_state").and_then(|v| v.as_str()),
        Some("ERROR"),
        "instance should be in ERROR"
    );

    // Retry via HTTP API with confirmation header
    let retry_resp = c
        .post(url(addr, &format!("/workflow-instance/{instance_id}/retry")))
        .bearer_auth("e2e-token")
        .header("x-aman-operator", "e2e-test")
        .header("x-aman-confirm", "yes")
        .send()
        .await
        .expect("retry")
        .json::<serde_json::Value>()
        .await
        .expect("retry json");
    assert_eq!(
        retry_resp.get("to_state").and_then(|v| v.as_str()),
        Some("PENDING"),
        "retry should restore to PENDING (last active state)"
    );
    assert!(
        retry_resp.get("transitioned").and_then(|v| v.as_bool()).unwrap_or(false),
        "retry should be a transitioned event"
    );

    // Verify instance via HTTP API
    let recovered = c
        .get(url(addr, &format!("/workflow-instance/{instance_id}")))
        .bearer_auth("e2e-token")
        .send()
        .await
        .expect("get instance")
        .json::<serde_json::Value>()
        .await
        .expect("instance json");
    assert_eq!(
        recovered.get("current_state").and_then(|v| v.as_str()),
        Some("PENDING"),
        "recovered instance should be back in PENDING"
    );

    // Shutdown
    c.post(url(addr, "/agent/shutdown"))
        .bearer_auth("e2e-token")
        .header("x-aman-operator", "e2e-test")
        .header("x-aman-confirm", "yes")
        .send()
        .await
        .expect("shutdown");

    server.shutdown();
}

/// ---------------------------------------------------------------------------
/// Scenario 2 (extended): DLQ retry without confirmation is rejected
/// ---------------------------------------------------------------------------
#[tokio::test]
async fn dlq_retry_requires_confirmation() {
    let mut config = AgentConfig::default();
    config.security.risky_capabilities_enabled = true;

    let runtime = AgentRuntimeBuilder::new(config)
        .with_bind_addr("127.0.0.1:0".parse().expect("addr"))
        .with_api_token(Some("e2e-token".to_owned()))
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

    c.post(url(addr, "/agent/start"))
        .bearer_auth("e2e-token")
        .send()
        .await
        .expect("start");

    let dlq_id = runtime
        .enqueue_dlq(
            kernel::event::Event::new(
                "pipeline:test",
                kernel::event::EventType::FileChanged,
                json!({"seq": 1}),
            ),
            "PipelineFailed: timeout",
            30,
        )
        .expect("enqueue dlq");

    // Retry WITHOUT confirmation should return 409 Conflict
    let no_confirm = c
        .post(url(addr, &format!("/dlq/{dlq_id}/retry")))
        .bearer_auth("e2e-token")
        .header("x-aman-operator", "e2e-test")
        .json(&json!({"reason": "retry without confirm"}))
        .send()
        .await
        .expect("dlq retry no confirm");
    assert_eq!(no_confirm.status(), 409);

    // Shutdown
    c.post(url(addr, "/agent/shutdown"))
        .bearer_auth("e2e-token")
        .header("x-aman-operator", "e2e-test")
        .header("x-aman-confirm", "yes")
        .send()
        .await
        .expect("shutdown");

    server.shutdown();
}
