// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! Contract tests for the `CognitiveEngine` trait, exercised through
//! `LlmCognitiveEngine` (the only production implementation today).
//!
//! These tests pin the documented behaviour of every trait method so
//! that future engines (world-model, hybrid) can be drop-in replacements
//! once they implement the same surface.
//!
//! The `StubLlmProvider` defined inline here implements
//! `cognitive_llm::provider::LlmProvider` (the cognitive-side trait with
//! `chat_completion(LlmChatRequest, Option<callback>)`). It is intentionally
//! local to this file: the two `LlmProvider` traits in the workspace
//! (`kernel::llm::LlmProvider` and `cognitive_llm::provider::LlmProvider`)
//! are parallel duplicates per the P0-1 finding, and bridging them would
//! add a new kernel→cognitive edge that reverses the decoupling P0-1
//! established. Keeping the stub local is the cleanest path.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use cognitive_engine::{
    Capability, CapabilityType, CognitiveContext, CognitiveEngine, CognitiveError,
    CognitiveEvent, CognitiveIdentity, CognitiveListener, DecisionKind,
    Observation, ObservationPayload,
};
use cognitive_llm::provider::{LlmChatRequest, LlmProvider, LlmResponse, StreamEvent};
use cognitive_llm::{LlmCognitiveEngine, LlmEngineConfig};
use cognitive_llm::react::ParsedToolCall;
use serde_json::{json, Value};

// These crates are transitive deps of cognitive-llm and available in tests.
use event_bus::{InMemoryBus, InMemoryBusConfig};
use kernel::context::ToolContext;
use kernel::types::{ExecutionModel, ToolMode};
use kernel::{AmanResult, schema};
use kernel::tool::Tool;
use cognitive_llm::ToolRegistry;

// ── StubLlmProvider ────────────────────────────────────────────────────

/// Clone a `Result<LlmResponse, String>` by destructuring the response
/// field-by-field. Necessary because `LlmResponse` does not derive
/// `Clone` (it wraps a `Vec<ParsedToolCall>` whose `Clone` impl is also
/// not derived — see `cognitive_llm::react::ParsedToolCall`).
fn clone_llm_result(r: &Result<LlmResponse, String>) -> Result<LlmResponse, String> {
    match r {
        Ok(resp) => Ok(LlmResponse {
            content: resp.content.clone(),
            finish_reason: resp.finish_reason.clone(),
            tool_calls: resp.tool_calls.clone(),
            reasoning_content: resp.reasoning_content.clone(),
        }),
        Err(e) => Err(e.clone()),
    }
}

/// A minimal stub of `LlmProvider` for contract testing `LlmCognitiveEngine`.
///
/// Each call pops the next entry from `responses`; the last entry repeats
/// forever. If `cb` is `Some`, the stub also emits a `Start`, a `Chunk(s)`,
/// and a `Done` event before returning the configured response.
struct StubLlmProvider {
    name: String,
    responses: Mutex<Vec<Result<LlmResponse, String>>>,
    stream_chunk: Option<String>,
    call_count: AtomicUsize,
}

impl StubLlmProvider {
    fn new(responses: Vec<Result<LlmResponse, String>>) -> Self {
        Self {
            name: "stub-llm".to_owned(),
            responses: Mutex::new(responses),
            stream_chunk: None,
            call_count: AtomicUsize::new(0),
        }
    }

    // Builder methods kept for future tests that may need to customize
    // the engine name (e.g. error-wrapping assertion) or exercise the
    // streaming callback path explicitly.
    #[allow(dead_code)]
    fn with_name(mut self, name: &str) -> Self {
        self.name = name.to_owned();
        self
    }

    #[allow(dead_code)]
    fn with_stream_chunk(mut self, chunk: &str) -> Self {
        self.stream_chunk = Some(chunk.to_owned());
        self
    }

    fn call_count(&self) -> usize {
        self.call_count.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl LlmProvider for StubLlmProvider {
    fn name(&self) -> &str {
        &self.name
    }

    async fn chat_completion(
        &self,
        _req: LlmChatRequest,
        cb: Option<Arc<dyn Fn(StreamEvent) + Send + Sync>>,
    ) -> Result<LlmResponse, String> {
        self.call_count.fetch_add(1, Ordering::SeqCst);

        if let (Some(cb), Some(chunk)) = (cb.as_ref(), self.stream_chunk.as_ref()) {
            cb(StreamEvent::Start);
            cb(StreamEvent::Chunk(chunk.clone()));
            cb(StreamEvent::Done {
                finish_reason: "stop".to_owned(),
            });
        }

        let mut responses = self.responses
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        if responses.is_empty() {
            return Ok(LlmResponse::default());
        }

        // FIFO with the last entry repeating. `LlmResponse` is not
        // `Clone` and `Result<LlmResponse, String>` isn't either, so
        // when the queue has one entry we clone by destructuring.
        if responses.len() > 1 {
            responses.remove(0)
        } else {
            clone_llm_result(&responses[0])
        }
    }
}

// ── RecordingListener ──────────────────────────────────────────────────

/// Captures every `CognitiveEvent` delivered to its `on_cognitive_event`
/// method, so tests can assert on the delivery contract of
/// `subscribe`/`unsubscribe`.
struct RecordingListener {
    events: Mutex<Vec<CognitiveEvent>>,
}

impl RecordingListener {
    fn new() -> Self {
        Self {
            events: Mutex::new(Vec::new()),
        }
    }

    fn snapshot(&self) -> Vec<CognitiveEvent> {
        self.events
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }
}

impl CognitiveListener for RecordingListener {
    fn on_cognitive_event(&self, event: CognitiveEvent) {
        self.events
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .push(event);
    }
}

// ── Fixtures ───────────────────────────────────────────────────────────

fn make_context(session_id: &str) -> CognitiveContext {
    CognitiveContext {
        agent_id: "agent-1".to_owned(),
        session_id: session_id.to_owned(),
        identity: CognitiveIdentity {
            name: "Test Agent".to_owned(),
            identity: "I am a test agent.".to_owned(),
            boundaries: vec![],
            expertise: vec![],
            vibe: None,
            raw: "raw config".to_owned(),
        },
        capabilities: vec![],
        memory_context: vec![],
        conversation_history: vec![],
        engine_config: Value::Null,
    }
}

fn make_context_with_capabilities(session_id: &str) -> CognitiveContext {
    let mut ctx = make_context(session_id);
    ctx.capabilities = vec![Capability {
        name: "echo".to_owned(),
        description: "Echoes back its argument".to_owned(),
        parameters: json!({"type": "object"}),
        cap_type: CapabilityType::Tool,
    }];
    ctx
}

fn make_user_message(session_id: &str, text: &str) -> Observation {
    Observation::user_message(format!("obs-{}", session_id), session_id, text)
}

fn make_response_with_content(content: &str) -> LlmResponse {
    LlmResponse {
        content: content.to_owned(),
        finish_reason: "stop".to_owned(),
        tool_calls: vec![],
        reasoning_content: String::new(),
    }
}

fn make_response_with_tool_call(tool_name: &str, args: Value) -> LlmResponse {
    LlmResponse {
        content: String::new(),
        finish_reason: "tool_calls".to_owned(),
        tool_calls: vec![ParsedToolCall {
            id: "call-1".to_owned(),
            tool_name: tool_name.to_owned(),
            args,
        }],
        reasoning_content: String::new(),
    }
}

// ── Contract tests ─────────────────────────────────────────────────────

#[tokio::test]
async fn process_with_empty_observations_returns_empty_decisions() {
    let stub = Arc::new(StubLlmProvider::new(vec![Ok(make_response_with_content(
        "should-not-be-used",
    ))]));
    let engine = LlmCognitiveEngine::new(stub.clone(), LlmEngineConfig::default());

    let result = engine
        .process(&make_context("s1"), vec![])
        .await
        .expect("empty observations should be Ok");

    assert!(
        result.is_empty(),
        "process(empty) should return Vec<Decision> with no entries; got {result:?}"
    );
    assert_eq!(
        stub.call_count(),
        0,
        "provider must not be called when observations is empty"
    );
}

#[tokio::test]
async fn process_wraps_provider_error_as_engine_error() {
    let stub = Arc::new(StubLlmProvider::new(vec![Err("upstream timeout".to_owned())]));
    let config = LlmEngineConfig { max_llm_retries: 1, ..LlmEngineConfig::default() };
    let engine = LlmCognitiveEngine::new(stub.clone(), config);

    let result = engine
        .process(&make_context("s2"), vec![make_user_message("s2", "hi")])
        .await;

    match result {
        Err(CognitiveError::EngineError { engine_name, message }) => {
            assert_eq!(engine_name, "stub-llm");
            assert_eq!(message, "upstream timeout");
        }
        other => panic!("expected EngineError, got {other:?}"),
    }
    assert_eq!(stub.call_count(), 1, "provider should be called exactly once");
}

#[tokio::test]
async fn process_with_text_response_produces_reply_decision() {
    let stub = Arc::new(StubLlmProvider::new(vec![Ok(make_response_with_content(
        "Hello!",
    ))]));
    let engine = LlmCognitiveEngine::new(stub.clone(), LlmEngineConfig::default());

    let decisions = engine
        .process(
            &make_context("s3"),
            vec![make_user_message("s3", "Hi")],
        )
        .await
        .expect("text response should succeed");

    assert_eq!(decisions.len(), 1, "expected exactly one decision");
    let d = &decisions[0];
    assert_eq!(d.session_id, "s3", "decision should carry the session id");
    assert!(!d.id.is_empty(), "decision id should be non-empty");
    match &d.kind {
        DecisionKind::Reply { text, is_final } => {
            assert_eq!(text, "Hello!");
            assert!(is_final, "Decision::reply marks is_final=true");
        }
        other => panic!("expected DecisionKind::Reply, got {other:?}"),
    }
}

#[tokio::test]
async fn process_with_tool_calls_produces_call_tools_decision() {
    let stub = Arc::new(StubLlmProvider::new(vec![Ok(make_response_with_tool_call(
        "echo",
        json!({ "x": 1 }),
    ))]));
    let engine = LlmCognitiveEngine::new(stub.clone(), LlmEngineConfig::default());

    let decisions = engine
        .process(
            &make_context_with_capabilities("s4"),
            vec![make_user_message("s4", "echo please")],
        )
        .await
        .expect("tool call response should succeed");

    assert_eq!(decisions.len(), 1);
    let d = &decisions[0];
    match &d.kind {
        DecisionKind::CallTools { calls, .. } => {
            assert_eq!(calls.len(), 1);
            assert_eq!(calls[0].tool_name, "echo");
            assert_eq!(calls[0].args, json!({ "x": 1 }));
        }
        other => panic!("expected DecisionKind::CallTools, got {other:?}"),
    }
}

#[tokio::test]
async fn subscribe_then_emit_routes_event_to_listener() {
    let stub = Arc::new(StubLlmProvider::new(vec![Ok(make_response_with_content("ignored"))]));
    let engine = LlmCognitiveEngine::new(stub.clone(), LlmEngineConfig::default());
    let listener = Arc::new(RecordingListener::new());

    engine.subscribe(listener.clone());

    // Drive the listener registry directly. `process()` does not yet call
    // `emit`; wiring that up is a separate streaming PR. The contract
    // being pinned here is that a subscribed listener receives events
    // emitted to the registry.
    engine.emit(CognitiveEvent::TextChunk {
        session_id: "s5".to_owned(),
        text: "chunk-1".to_owned(),
    });

    let captured = listener.snapshot();
    assert_eq!(captured.len(), 1, "listener should receive exactly one event");
    match &captured[0] {
        CognitiveEvent::TextChunk { session_id, text } => {
            assert_eq!(session_id, "s5");
            assert_eq!(text, "chunk-1");
        }
        other => panic!("expected TextChunk, got {other:?}"),
    }
}

#[tokio::test]
async fn unsubscribe_stops_event_delivery() {
    let stub = Arc::new(StubLlmProvider::new(vec![Ok(make_response_with_content("ignored"))]));
    let engine = LlmCognitiveEngine::new(stub.clone(), LlmEngineConfig::default());
    let l1: Arc<RecordingListener> = Arc::new(RecordingListener::new());
    let l2: Arc<RecordingListener> = Arc::new(RecordingListener::new());
    // The trait method signatures take `Arc<dyn CognitiveListener>`; keep
    // a `dyn` reference alongside the typed one so we can pass it to
    // `unsubscribe` while still snapshotting typed state.
    let l1_dyn: Arc<dyn CognitiveListener> = l1.clone();
    let l2_dyn: Arc<dyn CognitiveListener> = l2.clone();

    engine.subscribe(l1_dyn.clone());
    engine.subscribe(l2_dyn.clone());

    engine.emit(CognitiveEvent::StreamStart {
        session_id: "s6".to_owned(),
    });
    assert_eq!(l1.snapshot().len(), 1, "L1 should receive first emit");
    assert_eq!(l2.snapshot().len(), 1, "L2 should receive first emit");

    engine.unsubscribe(&l1_dyn);

    engine.emit(CognitiveEvent::StreamStart {
        session_id: "s6".to_owned(),
    });
    assert_eq!(
        l1.snapshot().len(),
        1,
        "L1 should NOT receive second emit after unsubscribe"
    );
    assert_eq!(
        l2.snapshot().len(),
        2,
        "L2 should still receive second emit"
    );
}

#[tokio::test]
async fn reset_session_is_idempotent_and_returns_ok() {
    let stub = Arc::new(StubLlmProvider::new(vec![Ok(make_response_with_content("ok"))]));
    let engine = LlmCognitiveEngine::new(stub.clone(), LlmEngineConfig::default());

    let r1 = engine.reset_session("s7").await;
    let r2 = engine.reset_session("s7").await;
    assert!(r1.is_ok(), "first reset_session should be Ok; got {r1:?}");
    assert!(r2.is_ok(), "second reset_session should be Ok; got {r2:?}");

    // The engine should still be usable after a reset.
    let decisions = engine
        .process(&make_context("s7"), vec![make_user_message("s7", "ping")])
        .await
        .expect("process after reset should succeed");
    assert_eq!(decisions.len(), 1);
}

// ── Sanity tests for the stub itself ───────────────────────────────────

#[tokio::test]
async fn stub_uses_first_response_then_repeats_last() {
    let stub = StubLlmProvider::new(vec![
        Ok(make_response_with_content("first")),
        Ok(make_response_with_content("second")),
    ]);
    let s = Arc::new(stub);

    let r1 = s
        .chat_completion(
            LlmChatRequest {
                model: "m".to_owned(),
                system_prompt: String::new(),
                messages: vec![],
                tools: vec![],
                max_output_tokens: 0,
                response_format: None,
            },
            None,
        )
        .await
        .unwrap();
    assert_eq!(r1.content, "first");

    let r2 = s
        .chat_completion(
            LlmChatRequest {
                model: "m".to_owned(),
                system_prompt: String::new(),
                messages: vec![],
                tools: vec![],
                max_output_tokens: 0,
                response_format: None,
            },
            None,
        )
        .await
        .unwrap();
    assert_eq!(r2.content, "second");

    // Third call: queue has one entry left (`second`). The stub clones
    // and returns it; the queue is preserved so subsequent calls would
    // also return `second`.
    let r3 = s
        .chat_completion(
            LlmChatRequest {
                model: "m".to_owned(),
                system_prompt: String::new(),
                messages: vec![],
                tools: vec![],
                max_output_tokens: 0,
                response_format: None,
            },
            None,
        )
        .await
        .unwrap();
    assert_eq!(
        r3.content, "second",
        "the last entry should repeat forever once the queue collapses to one"
    );

    // Fourth call: still `second` — confirms repeat-last semantics.
    let r4 = s
        .chat_completion(
            LlmChatRequest {
                model: "m".to_owned(),
                system_prompt: String::new(),
                messages: vec![],
                tools: vec![],
                max_output_tokens: 0,
                response_format: None,
            },
            None,
        )
        .await
        .unwrap();
    assert_eq!(r4.content, "second");
}

// ── Multi-turn ReAct loop integration test ────────────────────────────

/// A minimal echo tool for integration testing the ReAct loop.
/// Returns its input args as output.
struct EchoTool {
    params: schema::JsonSchema,
    returns_schema: schema::JsonSchema,
}

impl EchoTool {
    fn new() -> Self {
        Self {
            params: schema::JsonSchema::empty(),
            returns_schema: schema::JsonSchema::empty(),
        }
    }
}

#[async_trait]
impl Tool for EchoTool {
    fn name(&self) -> &str { "echo" }
    fn description(&self) -> &str { "Echoes back its arguments" }
    fn parameters(&self) -> &schema::JsonSchema { &self.params }
    fn returns(&self) -> &schema::JsonSchema { &self.returns_schema }
    fn mode(&self) -> ToolMode { ToolMode::Local }
    fn execution_model(&self) -> ExecutionModel { ExecutionModel::Independent }
    async fn execute(&self, args: serde_json::Value, _ctx: ToolContext) -> AmanResult<serde_json::Value> {
        Ok(args)
    }
}

#[tokio::test]
async fn multi_turn_react_loop_executes_tools_and_returns_final_reply() {
    // Provider: first call → tool_call, second call → final text
    let stub = Arc::new(StubLlmProvider::new(vec![
        Ok(make_response_with_tool_call("echo", json!({"message": "hello"}))),
        Ok(make_response_with_content("Echo executed. All done!")),
    ]));

    // Set up tool registry with echo tool
    let registry = ToolRegistry::new();
    let _ = registry.register(Arc::new(EchoTool::new()));

    // Set up event bus
    let bus_config = InMemoryBusConfig::default();
    let bus = Arc::new(InMemoryBus::new(bus_config));

    let config = LlmEngineConfig {
        max_llm_retries: 1,
        ..LlmEngineConfig::default()
    };
    let engine = LlmCognitiveEngine::new(stub.clone(), config)
        .with_tool_executor(Arc::new(registry), bus, 30_000);

    let decisions = engine
        .process(
            &make_context_with_capabilities("multi-turn"),
            vec![make_user_message("multi-turn", "echo hello then respond")],
        )
        .await
        .expect("multi-turn ReAct loop should succeed");

    // Should produce a final Reply decision (not CallTools)
    assert_eq!(decisions.len(), 1, "multi-turn should return exactly one Reply decision");
    match &decisions[0].kind {
        DecisionKind::Reply { text, is_final } => {
            assert!(is_final, "final decision should be marked final");
            assert_eq!(text, "Echo executed. All done!");
        }
        other => panic!("expected Reply, got {other:?}"),
    }

    // Provider should have been called exactly twice (tool_call + final)
    assert_eq!(stub.call_count(), 2, "LLM should be called twice: once for tool_call, once for final reply");
}

/// Regression: verify auto-continue doesn't fire when not in background mode.
#[tokio::test]
async fn non_background_mode_stops_at_max_turns() {
    // Provider returns tool_calls forever (will hit max_turns)
    let stub = Arc::new(StubLlmProvider::new(vec![
        Ok(make_response_with_tool_call("echo", json!({"x": 1}))),
    ]));

    let registry = ToolRegistry::new();
    let _ = registry.register(Arc::new(EchoTool::new()));
    let bus_config = InMemoryBusConfig::default();
    let bus = Arc::new(InMemoryBus::new(bus_config));

    let config = LlmEngineConfig {
        max_turns: 3,
        max_llm_retries: 1,
        background: false,
        ..LlmEngineConfig::default()
    };
    let engine = LlmCognitiveEngine::new(stub.clone(), config)
        .with_tool_executor(Arc::new(registry), bus, 30_000);

    let result = engine
        .process(
            &make_context_with_capabilities("max-turns"),
            vec![make_user_message("max-turns", "loop forever")],
        )
        .await;

    // Non-background should error with MaxDepthReached, not auto-continue
    match result {
        Err(CognitiveError::MaxDepthReached { depth }) => {
            assert_eq!(depth, 3, "should stop exactly at max_turns");
        }
        other => panic!("expected MaxDepthReached, got {other:?}"),
    }
}

// Reference `ObservationPayload::ToolCompleted` so a future test that
// exercises tool-completion observations can be added without
// re-discovering the variant name.
#[allow(dead_code)]
fn _reference_tool_completed_payload() -> ObservationPayload {
    ObservationPayload::ToolCompleted {
        tool_call_id: "t1".to_owned(),
        tool_name: "echo".to_owned(),
        output: "out".to_owned(),
        success: true,
        duration_ms: 1,
    }
}
