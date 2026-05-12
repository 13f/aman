use config::AgentConfig;
use runtime::{serve, AgentRuntimeBuilder, HttpServerConfig};
use serde_json::json;
use source::{SourceMode, TimerSource, TrustLevel};
use std::fs;
use workflow::{
    ErrorRecovery, RetryFailurePolicy, StateDef, Transition, TransitionFrom, TransitionTo,
    WorkflowDef,
};

fn url(addr: std::net::SocketAddr, path: &str) -> String {
    format!("http://{}{}", addr, path)
}

fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .no_proxy()
        .timeout(std::time::Duration::from_secs(3))
        .build()
        .expect("client")
}

#[tokio::test]
async fn http_all_endpoints_smoke() {
    let mut config = AgentConfig::default();
    config.security.risky_capabilities_enabled = true;
    let runtime = AgentRuntimeBuilder::new(config)
        .with_bind_addr("127.0.0.1:0".parse().expect("addr"))
        .with_api_token(Some("token".to_owned()))
        .build()
        .expect("build runtime");
    let skills_dir = runtime.runtime_dir().join("skills");
    fs::create_dir_all(&skills_dir).expect("create skills dir");
    fs::write(
        skills_dir.join("echo-skill.yaml"),
        r#"name: echo-skill
version: "1.0.0"
description: echo
triggers:
  - event_types: ["message_received"]
"#,
    )
    .expect("write skill");
    runtime
        .workflow_engine()
        .register_workflow(WorkflowDef {
            name: "approval".to_owned(),
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
                    name: "error".to_owned(),
                },
            ],
            initial_state: "pending".to_owned(),
            final_states: vec!["approved".to_owned()],
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
            state_timeouts: Vec::new(),
            error_recovery: ErrorRecovery {
                auto_retry_count: 0,
                max_retry_count: 1,
                retry_backoff: kernel::retry::RetryBackoff::Immediate,
                on_retry_failure: RetryFailurePolicy::Archive,
            },
        })
        .expect("register workflow");
    runtime
        .sources()
        .register(
            Box::new(TimerSource::new("timer:test", 1_000, false)),
            SourceMode::Pull,
            TrustLevel::Trusted,
        )
        .await
        .expect("register timer source");

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

    assert_eq!(
        c.get(url(addr, "/health/live"))
            .send()
            .await
            .expect("live")
            .status(),
        reqwest::StatusCode::OK
    );
    assert_eq!(
        c.get(url(addr, "/health/ready"))
            .send()
            .await
            .expect("ready")
            .status(),
        reqwest::StatusCode::SERVICE_UNAVAILABLE
    );

    assert_eq!(
        c.post(url(addr, "/agent/start"))
            .bearer_auth("token")
            .send()
            .await
            .expect("start")
            .status(),
        reqwest::StatusCode::OK
    );

    assert_eq!(
        c.get(url(addr, "/workflows"))
            .bearer_auth("token")
            .send()
            .await
            .expect("workflows list")
            .status(),
        reqwest::StatusCode::OK
    );
    assert_eq!(
        c.get(url(addr, "/workflow/approval"))
            .bearer_auth("token")
            .send()
            .await
            .expect("workflow info")
            .status(),
        reqwest::StatusCode::OK
    );
    let created = c
        .post(url(addr, "/workflow/approval/create"))
        .bearer_auth("token")
        .header("x-aman-operator", "tester")
        .json(&json!({"data":{"k":1}}))
        .send()
        .await
        .expect("workflow create")
        .json::<serde_json::Value>()
        .await
        .expect("workflow create json");
    let instance_id = created
        .get("id")
        .and_then(|v| v.as_str())
        .expect("instance id");
    assert_eq!(
        c.get(url(addr, "/workflow-instances"))
            .bearer_auth("token")
            .send()
            .await
            .expect("workflow instances")
            .status(),
        reqwest::StatusCode::OK
    );
    assert_eq!(
        c.get(url(addr, &format!("/workflow-instance/{instance_id}")))
            .bearer_auth("token")
            .send()
            .await
            .expect("workflow instance")
            .status(),
        reqwest::StatusCode::OK
    );

    assert_eq!(
        c.get(url(addr, "/skills"))
            .bearer_auth("token")
            .send()
            .await
            .expect("skills list")
            .status(),
        reqwest::StatusCode::OK
    );
    assert_eq!(
        c.get(url(addr, "/skills/search?q=echo&limit=10"))
            .bearer_auth("token")
            .send()
            .await
            .expect("skills search")
            .status(),
        reqwest::StatusCode::OK
    );
    assert_eq!(
        c.get(url(addr, "/skill/echo-skill"))
            .bearer_auth("token")
            .send()
            .await
            .expect("skill info")
            .status(),
        reqwest::StatusCode::OK
    );
    assert_eq!(
        c.post(url(addr, "/skill/echo-skill/disable"))
            .bearer_auth("token")
            .header("x-aman-operator", "tester")
            .send()
            .await
            .expect("skill disable")
            .status(),
        reqwest::StatusCode::OK
    );
    assert_eq!(
        c.post(url(addr, "/skill/echo-skill/enable"))
            .bearer_auth("token")
            .header("x-aman-operator", "tester")
            .send()
            .await
            .expect("skill enable")
            .status(),
        reqwest::StatusCode::OK
    );
    assert_eq!(
        c.get(url(addr, "/skill/echo-skill/versions"))
            .bearer_auth("token")
            .send()
            .await
            .expect("skill versions")
            .status(),
        reqwest::StatusCode::OK
    );
    assert_eq!(
        c.post(url(addr, "/skill/echo-skill/rollback"))
            .bearer_auth("token")
            .header("x-aman-operator", "tester")
            .header("x-aman-confirm", "yes")
            .json(&json!({"version":"1.0.0"}))
            .send()
            .await
            .expect("skill rollback")
            .status(),
        reqwest::StatusCode::OK
    );

    assert_eq!(
        c.post(url(addr, "/event-source/timer:test/pause"))
            .bearer_auth("token")
            .header("x-aman-operator", "tester")
            .send()
            .await
            .expect("pause")
            .status(),
        reqwest::StatusCode::OK
    );
    assert_eq!(
        c.post(url(addr, "/source/timer:test/resume"))
            .bearer_auth("token")
            .header("x-aman-operator", "tester")
            .send()
            .await
            .expect("resume")
            .status(),
        reqwest::StatusCode::OK
    );
    assert_eq!(
        c.put(url(addr, "/source/timer:test/config"))
            .bearer_auth("token")
            .header("x-aman-operator", "tester")
            .json(&json!({"interval_ms":500}))
            .send()
            .await
            .expect("config")
            .status(),
        reqwest::StatusCode::OK
    );

    assert_eq!(
        c.get(url(addr, "/plugins"))
            .bearer_auth("token")
            .header("x-aman-operator", "tester")
            .send()
            .await
            .expect("plugins")
            .status(),
        reqwest::StatusCode::OK
    );

    assert_eq!(
        c.post(url(addr, "/cron/add"))
            .bearer_auth("token")
            .header("x-aman-operator", "tester")
            .json(&json!({"id":"cron:test","expression":"0/5 * * * * *"}))
            .send()
            .await
            .expect("cron add")
            .status(),
        reqwest::StatusCode::OK
    );
    assert_eq!(
        c.post(url(addr, "/cron/cron:test/update"))
            .bearer_auth("token")
            .header("x-aman-operator", "tester")
            .json(&json!({"enabled":false}))
            .send()
            .await
            .expect("cron update")
            .status(),
        reqwest::StatusCode::OK
    );
    assert_eq!(
        c.post(url(addr, "/cron/cron:test/remove"))
            .bearer_auth("token")
            .header("x-aman-operator", "tester")
            .send()
            .await
            .expect("cron remove")
            .status(),
        reqwest::StatusCode::OK
    );

    let injected = c
        .post(url(addr, "/inject-event"))
        .bearer_auth("token")
        .header("x-aman-operator", "tester")
        .json(&json!({"source":"debug","event_type":"message_received","payload":{"hello":"world"}}))
        .send()
        .await
        .expect("inject")
        .json::<serde_json::Value>()
        .await
        .expect("inject json");
    let event_id = injected.get("id").and_then(|v| v.as_str()).expect("id");

    assert_eq!(
        c.get(url(addr, &format!("/events/dump/{event_id}")))
            .bearer_auth("token")
            .send()
            .await
            .expect("dump")
            .status(),
        reqwest::StatusCode::OK
    );

    let dumped = c
        .get(url(addr, &format!("/events/dump/{event_id}")))
        .bearer_auth("token")
        .send()
        .await
        .expect("dump2")
        .json::<serde_json::Value>()
        .await
        .expect("dump json");
    let trace_id = dumped
        .pointer("/metadata/trace_id")
        .and_then(|v| v.as_str())
        .expect("trace_id");

    assert_eq!(
        c.get(url(addr, &format!("/events/trace/{trace_id}")))
            .bearer_auth("token")
            .send()
            .await
            .expect("trace")
            .status(),
        reqwest::StatusCode::OK
    );

    let dlq_id = runtime
        .enqueue_dlq(
            kernel::event::Event::new(
                "pipeline:test",
                kernel::event::EventType::MessageReceived,
                json!({"seq": 1}),
            ),
            "PipelineFailed",
            30,
        )
        .expect("enqueue dlq");
    assert_eq!(
        c.get(url(addr, "/dlq"))
            .bearer_auth("token")
            .send()
            .await
            .expect("dlq list")
            .status(),
        reqwest::StatusCode::OK
    );
    assert_eq!(
        c.post(url(addr, &format!("/dlq/{dlq_id}/retry")))
            .bearer_auth("token")
            .header("x-aman-operator", "tester")
            .header("x-aman-confirm", "yes")
            .json(&json!({"reason":"requeue"}))
            .send()
            .await
            .expect("dlq retry")
            .status(),
        reqwest::StatusCode::OK
    );
    assert_eq!(
        c.post(url(addr, &format!("/dlq/{dlq_id}/discard")))
            .bearer_auth("token")
            .header("x-aman-operator", "tester")
            .send()
            .await
            .expect("dlq discard")
            .status(),
        reqwest::StatusCode::OK
    );

    assert_eq!(
        c.get(url(addr, "/metrics"))
            .send()
            .await
            .expect("metrics")
            .status(),
        reqwest::StatusCode::OK
    );

    assert_eq!(
        c.get(url(addr, "/audit-log"))
            .bearer_auth("token")
            .send()
            .await
            .expect("audit")
            .status(),
        reqwest::StatusCode::OK
    );

    assert_eq!(
        c.post(url(addr, "/agent/shutdown"))
            .bearer_auth("token")
            .header("x-aman-operator", "tester")
            .header("x-aman-confirm", "yes")
            .send()
            .await
            .expect("shutdown")
            .status(),
        reqwest::StatusCode::OK
    );

    server.shutdown();
}
