// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! Integration test: dispatcher pipeline output reaches the event bus.
//!
//! Exercises the post-dispatch publishing path the gateway would take —
//! `Dispatcher::dispatch` returns a `DispatchResult` with `output_events`,
//! and the test publishes each through a `FakeEventBus` to assert that
//! the bus is the wire a downstream consumer would actually see.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use dispatcher::{
    DispatchTarget, Dispatcher, MatchCondition, RouteRule, TransformRule,
};
use event_bus::EventBus;
use kernel::context::ToolContext;
use kernel::event::{Event, EventType};
use kernel::pipeline::{PipelineStep, StepType};
use kernel::retry::{RetryBackoff, RetryPolicy};
use kernel::schema::JsonSchema;
use kernel::tool::Tool;
use kernel::types::{ConcurrencyModel, ToolMode};
use kernel::{AmanResult, Error};
use pipeline::PipelineDefinition;
use serde_json::{json, Value};
use test_utils::fake_event_bus::{FakeBusConfig, FakeEventBus};

/// A `Tool` that yields one canned output, used to keep the test focused
/// on dispatch + bus wiring rather than pipeline execution semantics.
#[derive(Debug)]
struct TaggedOutputStubTool {
    outputs: Mutex<VecDeque<AmanResult<Value>>>,
}

#[async_trait]
impl Tool for TaggedOutputStubTool {
    fn name(&self) -> &str {
        "tag-output"
    }

    fn mode(&self) -> ToolMode {
        ToolMode::Local
    }

    fn parameters(&self) -> &JsonSchema {
        static PARAMS: std::sync::LazyLock<JsonSchema> =
            std::sync::LazyLock::new(|| JsonSchema::from(json!({"type": "object"})));
        &PARAMS
    }

    fn returns(&self) -> &JsonSchema {
        static RETURNS: std::sync::LazyLock<JsonSchema> =
            std::sync::LazyLock::new(|| JsonSchema::from(json!({"type": "object"})));
        &RETURNS
    }

    async fn execute(&self, _params: Value, _ctx: ToolContext) -> AmanResult<Value> {
        self.outputs
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .pop_front()
            .unwrap_or_else(|| Ok(Value::Null))
    }
}

fn build_pipeline(id: &str, outputs: Vec<AmanResult<Value>>) -> PipelineDefinition {
    PipelineDefinition::new(
        id.to_owned(),
        ConcurrencyModel::Serial,
        vec![PipelineStep {
            id: "tag".to_owned(),
            step_type: StepType::Transform,
            tool: Arc::new(TaggedOutputStubTool {
                outputs: Mutex::new(VecDeque::from(outputs)),
            }),
            compensate: None,
            retry: RetryPolicy {
                max_attempts: 1,
                retry_backoff: RetryBackoff::Immediate,
            },
        }],
    )
}

fn webhook_event() -> Event {
    Event::new(
        "webhook:billing",
        EventType::WebhookReceived,
        json!({"invoice": "inv_42"}),
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dispatcher_output_events_publish_through_fake_event_bus() {
    // Keep a typed `Arc<FakeEventBus>` so the test can both publish through
    // the `dyn` reference (no stable downcast) and assert through the
    // concrete accessor methods.
    let fake: Arc<FakeEventBus> = Arc::new(FakeEventBus::new(FakeBusConfig::default()));
    let bus: Arc<dyn EventBus> = fake.clone();

    let mut dispatcher = Dispatcher::new();
    dispatcher.register_pipeline(build_pipeline(
        "p-tag",
        vec![Ok(json!({"tag": "routed"}))],
    ));
    dispatcher.rebuild_routes(vec![RouteRule {
        id: "tag-webhook".to_owned(),
        priority: 10,
        condition: MatchCondition::Type(EventType::WebhookReceived),
        targets: vec![DispatchTarget::Pipeline("p-tag".to_owned())],
        transform: Some(TransformRule::SetEventType(EventType::Custom(
            "tagged".into(),
        ))),
        filter: None,
    }]);

    // Drive the dispatcher synchronously.
    let result = dispatcher.dispatch(webhook_event()).await;
    assert!(result.failures.is_empty(), "dispatch should not fail");
    assert_eq!(result.pipeline_runs.len(), 1);
    assert_eq!(
        result.output_events.len(),
        1,
        "pipeline should emit exactly one output"
    );

    // Simulate the gateway publishing each output event through the bus.
    for ev in &result.output_events {
        bus.publish(ev.clone())
            .await
            .expect("FakeEventBus::publish should not fail");
    }

    // Assert the bus received exactly the dispatcher's outputs.
    assert_eq!(fake.event_count(), 1, "bus should record 1 event");
    assert!(
        fake.has_event(|e| matches!(&e.event_type, EventType::Custom(t) if t == "tagged")),
        "bus should record the tagged output event"
    );
    assert!(
        fake.events_matching(|e| e.payload == json!({"tag": "routed"})).len() == 1,
        "bus should record the exact payload emitted by the pipeline"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fake_event_bus_backpressure_remains_normal_after_publish() {
    // Smaller smoke test: confirms FakeEventBus returns Normal pressure
    // for low traffic, exercising the backpressure helper code path
    // introduced when test-utils was adopted as a dev-dep.
    let fake: Arc<FakeEventBus> = Arc::new(FakeEventBus::new(FakeBusConfig::default()));
    let bus: Arc<dyn EventBus> = fake.clone();

    for i in 0..3 {
        bus.publish(Event::new(
            "test",
            EventType::Custom(format!("smoke.{i}")),
            json!({ "i": i }),
        ))
        .await
        .expect("publish should succeed");
    }
    assert_eq!(fake.event_count(), 3);
    assert_eq!(
        fake.backpressure_level(),
        kernel::types::BackpressureLevel::Normal,
        "3 events should be well below L1 threshold (default 5)"
    );
}

// Helper: ensure Error is referenced so unused-import warnings don't sneak
// in if future edits drop the failure path. Cheap and explicit.
#[allow(dead_code)]
fn _ensure_kernel_error_in_scope() -> Error {
    Error::Unrecoverable {
        message: "test-utils smoke".to_owned(),
    }
}
