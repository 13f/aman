// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! LLM-based Cognitive Engine for aman.
//!
//! This crate consolidates all LLM-specific code — provider abstraction,
//! ReAct loop types, OpenAI API client, prompt pipeline, token budgeting,
//! and context management — into a single `LlmCognitiveEngine` that
//! implements the `CognitiveEngine` trait.
//!
//! # Architecture
//!
//! ```text
//! CognitiveEngine::process(observations) → decisions
//!     │
//!     ├── Convert Observations → ChatMessages
//!     ├── Build system prompt (PromptPipeline)
//!     ├── Call LLM (with retry + backoff)
//!     │   ├── OutputValidator + ContentFilter
//!     │   └── Publish events via event_sink
//!     └── Convert ReActTurn → Decisions
//! ```

#![forbid(unsafe_code)]

pub mod anthropic;
pub mod delegate_task;
pub mod local;
pub mod embed;
pub(crate) mod net_proxy;
pub mod openai;
pub mod prompt;
pub mod provider;
pub mod react;
pub mod shared;
pub mod simple;
pub mod subagent;

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use cognitive_engine::{
    CognitiveContext, CognitiveEngine, CognitiveError, CognitiveEvent, CognitiveListener,
    Decision, Observation, ObservationPayload, ToolCallRequest,
};

use crate::openai::LlmOpenaiProvider;
use crate::prompt::{DefaultPromptPipeline, PromptPipeline};
use crate::provider::{LlmChatRequest, LlmProvider};
use crate::react::{ChatMessage, ReActTurn, SoulSnapshot};

// ── LlmCognitiveEngine ─────────────────────────────────────────────────

/// Configuration for the LLM cognitive engine.
#[derive(Debug, Clone)]
pub struct LlmEngineConfig {
    /// The LLM model name (e.g. "gpt-4o").
    pub model: String,
    /// Maximum ReAct loop turns (default: 64).
    pub max_turns: u32,
    /// Token budget limit (default: 128000).
    pub token_limit: u64,
    /// Maximum output tokens per LLM call.
    pub max_output_tokens: u64,
    /// Maximum LLM call retries on transient errors (default: 5).
    /// Set to 1 to disable retries.
    pub max_llm_retries: u32,
}

impl Default for LlmEngineConfig {
    fn default() -> Self {
        Self {
            model: "gpt-4o".into(),
            max_turns: 64,
            token_limit: 128_000,
            max_output_tokens: 4096,
            max_llm_retries: 5,
        }
    }
}

/// An LLM-based cognitive engine.
///
/// ## Target architecture
///
/// ```text
/// Gateway → CognitiveEngine::process(observations) → decisions
///              ↑
///         LlmCognitiveEngine (ReAct loop strategy)
///              ├── LlmProvider::chat_completion()
///              ├── ToolExecutor::execute_tools()
///              └── TokenBudget tracking
/// ```
///
/// ## Current state
///
/// Currently implements a **single-turn** call to the LLM provider.
/// The full ReAct loop (multi-turn think-act-observe) is implemented
/// externally in `LlmReActEngine` (kernel/gateway). The plan is to
/// absorb the ReAct loop into this engine's `process()` method, so
/// the gateway only calls `CognitiveEngine::process()` and receives
/// the final result after all internal tool-use iterations complete.
///
/// When the ReAct loop is internalized:
/// - `LlmReActEngine` can be retired
/// - `CognitiveReActEngine` (deleted in fd52423) is no longer needed
/// - The gateway is fully decoupled from the ReAct implementation
pub struct LlmCognitiveEngine {
    provider: Arc<dyn LlmProvider>,
    prompt_pipeline: Arc<dyn PromptPipeline>,
    config: LlmEngineConfig,
    listeners: Arc<Mutex<Vec<Arc<dyn CognitiveListener>>>>,
    /// Optional event sink for publishing lifecycle events.
    event_sink: Option<Arc<dyn Fn(kernel::event::Event) + Send + Sync>>,
    /// Tool registry for executing tool calls (ReAct loop).
    tool_registry: Option<Arc<tool::ToolRegistry>>,
    /// Event bus for publishing tool lifecycle events.
    bus: Option<Arc<dyn event_bus::EventBus>>,
    /// Per-tool timeout in milliseconds.
    tool_timeout_ms: u64,
    /// Tool security config for path/network/command allowlist.
    tool_security: Option<tool::ToolSecurityConfig>,
}

impl LlmCognitiveEngine {
    /// Create a new LLM cognitive engine with the given provider.
    pub fn new(
        provider: Arc<dyn LlmProvider>,
        config: LlmEngineConfig,
    ) -> Self {
        Self {
            provider,
            prompt_pipeline: Arc::new(DefaultPromptPipeline),
            config,
            listeners: Arc::new(Mutex::new(Vec::new())),
            event_sink: None,
            tool_registry: None,
            bus: None,
            tool_timeout_ms: 30_000,
            tool_security: None,
        }
    }

    /// Create with a custom prompt pipeline.
    pub fn with_prompt_pipeline(
        provider: Arc<dyn LlmProvider>,
        config: LlmEngineConfig,
        prompt_pipeline: Arc<dyn PromptPipeline>,
    ) -> Self {
        Self {
            provider,
            prompt_pipeline,
            config,
            listeners: Arc::new(Mutex::new(Vec::new())),
            event_sink: None,
            tool_registry: None,
            bus: None,
            tool_timeout_ms: 30_000,
            tool_security: None,
        }
    }

    /// Set an event sink for publishing lifecycle events.
    #[must_use]
    pub fn with_event_sink(
        mut self,
        sink: Arc<dyn Fn(kernel::event::Event) + Send + Sync>,
    ) -> Self {
        self.event_sink = Some(sink);
        self
    }

    /// Set the tool registry + event bus for executing tool calls.
    #[must_use]
    pub fn with_tool_executor(
        mut self,
        registry: Arc<tool::ToolRegistry>,
        bus: Arc<dyn event_bus::EventBus>,
        timeout_ms: u64,
    ) -> Self {
        self.tool_registry = Some(registry);
        self.bus = Some(bus);
        self.tool_timeout_ms = timeout_ms;
        self
    }

    /// Set tool security config for path/network/command enforcement.
    #[must_use]
    pub fn with_tool_security(mut self, config: tool::ToolSecurityConfig) -> Self {
        self.tool_security = Some(config);
        self
    }

    /// Convenience: create with an OpenAI-compatible provider.
    pub fn openai(api_key: String, base_url: String, model: impl Into<String>) -> Self {
        let config = LlmEngineConfig {
            model: model.into(),
            ..Default::default()
        };
        Self::new(
            Arc::new(LlmOpenaiProvider::new(api_key, base_url)),
            config,
        )
    }

    /// Emit a cognitive event to all registered listeners.
    ///
    /// `process()` automatically emits `StreamStart`, `TextChunk`,
    /// `StreamDone`, and `StreamError` events when listeners are
    /// registered and the provider supports streaming.
    pub fn emit(&self, event: CognitiveEvent) {
        if let Ok(listeners) = self.listeners.lock() {
            for listener in listeners.iter() {
                listener.on_cognitive_event(event.clone());
            }
        }
    }

    /// Generate a unique decision ID.
    fn new_decision_id() -> String {
        uuid::Uuid::now_v7().to_string()
    }

    /// Convert a stream of Observations into ChatMessages for the LLM.
    fn observations_to_messages(
        observations: &[Observation],
        existing_history: &[ChatMessage],
    ) -> Vec<ChatMessage> {
        let mut messages = existing_history.to_vec();

        for obs in observations {
            match &obs.payload {
                ObservationPayload::UserMessage { text } => {
                    messages.push(ChatMessage::user(text.as_str()));
                }
                ObservationPayload::ToolCompleted {
                    tool_call_id,
                    tool_name,
                    output,
                    ..
                } => {
                    messages.push(ChatMessage::tool_result(
                        tool_call_id.as_str(),
                        tool_name.as_str(),
                        output.as_str(),
                    ));
                }
                ObservationPayload::DetachedCompleted {
                    tool_call_id,
                    output,
                    ..
                } => {
                    messages.push(ChatMessage::tool_result(
                        tool_call_id.as_str(),
                        "detached",
                        output.as_str(),
                    ));
                }
                _ => {
                    // Timer, system events, world state changes — for now,
                    // the LLM engine only handles user messages and tool results.
                    // Future: translate these into structured context.
                }
            }
        }

        messages
    }

    // ── Tool execution (mirrors LlmReActEngine::execute_tools) ─────

    /// Execute tool calls and return results as ChatMessages.
    /// Includes security checks, retry, and parallel/serial execution.
    async fn execute_tool_calls(
        &self,
        calls: &[crate::react::ParsedToolCall],
    ) -> Vec<crate::react::ToolCallResult> {
        let Some(ref registry) = self.tool_registry else {
            return calls.iter().map(|c| crate::react::ToolCallResult {
                id: c.id.clone(),
                tool_name: c.tool_name.clone(),
                success: false,
                output: "tool registry not configured".into(),
                duration_ms: 0,
                pending_detach: None,
            }).collect();
        };
        let Some(ref bus) = self.bus else {
            return calls.iter().map(|c| crate::react::ToolCallResult {
                id: c.id.clone(), tool_name: c.tool_name.clone(),
                success: false, output: "event bus not configured".into(),
                duration_ms: 0, pending_detach: None,
            }).collect();
        };

        const TOOL_MAX_RETRIES: u32 = 3;
        let registry = Arc::clone(registry);
        let bus: Arc<dyn event_bus::EventBus> = Arc::clone(bus);
        let timeout = self.tool_timeout_ms;

        // Classify calls by execution model
        let models: Vec<bool> = calls.iter()
            .map(|c| registry.get(&c.tool_name)
                .map(|t| t.execution_model() == kernel::types::ExecutionModel::Independent)
                .unwrap_or(false))
            .collect();

        // Phase 1: Independent calls concurrently
        let mut handles: Vec<(usize, tokio::task::JoinHandle<crate::react::ToolCallResult>)> = Vec::new();
        for (i, call) in calls.iter().enumerate() {
            if models[i] {
                let reg = Arc::clone(&registry);
                let b = Arc::clone(&bus);
                let c = call.clone();
                let t = timeout;
                handles.push((i, tokio::spawn(async move {
                    execute_one_with_retry(&reg, &b, &c, t, TOOL_MAX_RETRIES).await
                })));
            }
        }

        // Phase 2: Stateful/SideEffect calls sequentially
        let mut serial_results: Vec<(usize, crate::react::ToolCallResult)> = Vec::new();
        for (i, call) in calls.iter().enumerate() {
            if !models[i] {
                let result = execute_one_with_retry(&registry, &bus, call, timeout, TOOL_MAX_RETRIES).await;
                serial_results.push((i, result));
            }
        }

        // Collect independent results
        let mut independent_results: Vec<(usize, crate::react::ToolCallResult)> = Vec::new();
        for (i, handle) in handles {
            match handle.await {
                Ok(r) => independent_results.push((i, r)),
                Err(e) => independent_results.push((i, crate::react::ToolCallResult {
                    id: String::new(), tool_name: String::new(),
                    success: false,
                    output: format!("tool panicked: {e}"),
                    duration_ms: 0, pending_detach: None,
                })),
            }
        }

        // Merge in original order
        let mut all = Vec::with_capacity(calls.len());
        all.extend(independent_results);
        all.extend(serial_results);
        all.sort_by_key(|(i, _)| *i);
        let mut results: Vec<crate::react::ToolCallResult> = all.into_iter().map(|(_, r)| r).collect();

        // ── Detach handling (block on background processes) ──────────
        // Mirror of LlmReActEngine::execute_tools() lines 1108-1210
        for result in &mut results {
            let Some(pid) = result.pending_detach else { continue; };

            // Subscribe to tool:completed events
            let filter = event_bus::SubscriptionFilter {
                event_types: Some(vec![kernel::event::EventType::Custom("tool:completed".into())]),
                ..Default::default()
            };
            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
            struct DetachWaiter(tokio::sync::mpsc::UnboundedSender<kernel::event::Event>);
            #[async_trait::async_trait]
            impl event_bus::EventHandler for DetachWaiter {
                async fn handle(&self, event: kernel::event::Event) -> kernel::AmanResult<()> {
                    let _ = self.0.send(event);
                    Ok(())
                }
            }
            let sub_result = bus.subscribe(filter, Box::new(DetachWaiter(tx))).await;

            if let Ok(sub_id) = sub_result {
                // Block until tool:completed with matching pid, or timeout
                let deadline = tokio::time::Instant::now()
                    + std::time::Duration::from_secs(timeout / 1000 + 30); // extra 30s for detach
                loop {
                    match tokio::time::timeout_at(deadline, rx.recv()).await {
                        Ok(Some(event)) => {
                            let event_pid = event.payload.get("pid").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
                            if event_pid == pid {
                                result.success = event.payload.get("success").and_then(|v| v.as_bool()).unwrap_or(false);
                                if let Some(output) = event.payload.get("output").and_then(|v| v.as_str()) {
                                    result.output = output.to_owned();
                                }
                                result.duration_ms = event.payload.get("duration_ms").and_then(|v| v.as_u64()).unwrap_or(0);
                                result.pending_detach = None;
                                break;
                            }
                        }
                        _ => {
                            // Timeout — mark as failed
                            result.success = false;
                            result.output = format!("detached process {pid} timed out");
                            break;
                        }
                    }
                }
                let _ = bus.unsubscribe(sub_id).await;
            }
        }

        results
    }

    /// Convert a final ReActTurn into Decisions.
    fn turn_to_decisions(
        turn: ReActTurn,
        session_id: &str,
    ) -> Vec<Decision> {
        match turn {
            ReActTurn::Finished { content, .. } => {
                vec![Decision::reply(
                    Self::new_decision_id(),
                    session_id,
                    content,
                )]
            }
            ReActTurn::ToolCalls { calls, .. } => {
                let tool_requests: Vec<ToolCallRequest> = calls
                    .into_iter()
                    .map(|tc| ToolCallRequest {
                        id: tc.id,
                        tool_name: tc.tool_name,
                        args: tc.args,
                        detach: false,
                    })
                    .collect();
                vec![Decision::call_tools(
                    Self::new_decision_id(),
                    session_id,
                    tool_requests,
                )]
            }
            ReActTurn::Error(e) => {
                tracing::error!(%e, "ReAct error");
                vec![Decision::reply(
                    Self::new_decision_id(),
                    session_id,
                    format!("I encountered an error: {e}"),
                )]
            }
        }
    }
}

// ── Retry helpers (mirrors LlmReActEngine) ────────────────────────

/// Execute one tool call with retry, security checks, and event publishing.
async fn execute_one_with_retry(
    registry: &tool::ToolRegistry,
    bus: &Arc<dyn event_bus::EventBus>,
    call: &crate::react::ParsedToolCall,
    timeout_ms: u64,
    max_retries: u32,
) -> crate::react::ToolCallResult {
    let start = std::time::Instant::now();

    // Security: hardline block check
    if let Some(reason) = tool::security::check_hardline_block(&call.tool_name, &call.args) {
        return crate::react::ToolCallResult {
            id: call.id.clone(), tool_name: call.tool_name.clone(),
            success: false, output: format!("hardline_blocked: {reason}"),
            duration_ms: start.elapsed().as_millis() as u64, pending_detach: None,
        };
    }

    let mut attempt = 0;
    loop {
        attempt += 1;
        let tool = match registry.get(&call.tool_name) {
            Some(t) => t,
            None => return crate::react::ToolCallResult {
                id: call.id.clone(), tool_name: call.tool_name.clone(),
                success: false, output: format!("tool '{}' not found", call.tool_name),
                duration_ms: start.elapsed().as_millis() as u64, pending_detach: None,
            },
        };

        let mut ctx = kernel::context::ToolContext::default();
        ctx.base.timeout_ms = Some(timeout_ms);

        let result = tool.execute(call.args.clone(), ctx).await;
        let duration_ms = start.elapsed().as_millis() as u64;
        let success = result.is_ok();
        let output = match &result {
            Ok(v) => serde_json::to_string(v).unwrap_or_default(),
            Err(e) => e.to_string(),
        };

        // Publish tool:completed
        let _ = bus.publish(kernel::event::Event::new(
            "cognitive-engine",
            kernel::event::EventType::Custom("tool:completed".into()),
            serde_json::json!({
                "tool_call_id": call.id, "tool_name": call.tool_name,
                "success": success, "duration_ms": duration_ms,
            }),
        )).await;

        if success || attempt >= max_retries || !is_retryable_tool_error(&output) {
            return crate::react::ToolCallResult {
                id: call.id.clone(), tool_name: call.tool_name.clone(),
                success, output, duration_ms, pending_detach: None,
            };
        }
        tracing::warn!(tool=%call.tool_name, attempt, error=%output, "tool call failed, retrying");
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }
}

fn is_retryable_tool_error(output: &str) -> bool {
    let lower = output.to_lowercase();
    if lower.contains("unrecoverable") || lower.contains("no such file")
        || lower.contains("not found") || lower.contains("permission denied")
        || lower.contains("not allowed") { return false; }
    lower.contains("timeout") || lower.contains("connection")
        || lower.contains("refused") || lower.contains("reset")
        || lower.contains("temporary") || lower.contains("rate limit")
        || lower.contains("too many requests")
}

// ── Retry helper (mirrors LlmReActEngine::is_retryable_llm_error) ─

/// Check if an LLM error is transient (worth retrying) vs permanent.
fn is_retryable_llm_error(err: Option<&String>) -> bool {
    let Some(e) = err else { return false };
    let msg = e.to_lowercase();
    // Permanent errors — don't retry
    if msg.contains("400") || msg.contains("bad request") { return false; }
    if msg.contains("401") || msg.contains("403") { return false; }
    if msg.contains("402") || msg.contains("payment required")
        || msg.contains("insufficient_quota") || msg.contains("billing")
    { return false; }
    // Transient — retry
    msg.contains("error sending request")
        || msg.contains("timeout")
        || msg.contains("connection")
        || msg.contains("429")
        || msg.contains("500") || msg.contains("502") || msg.contains("503") || msg.contains("504")
        || msg.contains("error decoding response body")
        || msg.contains("error decoding chunk")
        || msg.contains("stream closed")
        || msg.contains("connection closed")
        || msg.contains("unexpected eof")
}

#[async_trait]
impl CognitiveEngine for LlmCognitiveEngine {
    fn name(&self) -> &str {
        self.provider.name()
    }

    async fn process(
        &self,
        ctx: &CognitiveContext,
        observations: Vec<Observation>,
    ) -> Result<Vec<Decision>, CognitiveError> {
        if observations.is_empty() {
            return Ok(vec![]);
        }

        let session_id = ctx.session_id.clone();
        let agent_id = ctx.agent_id.clone();
        let max_turns = self.config.max_turns;
        let max_retries = self.config.max_llm_retries;

        // Build the SoulSnapshot from CognitiveIdentity
        let system_prompt = self
            .prompt_pipeline
            .build_system_prompt(
                &SoulSnapshot::new(&ctx.identity.name, &ctx.identity.raw),
                &[],
                ctx.memory_context.first().map(|m| m.content.as_str()),
            )
            .await;

        let soul = SoulSnapshot {
            name: ctx.identity.name.clone(),
            system_prompt,
            boundaries: ctx.identity.boundaries.clone(),
        };

        // Convert capabilities to ToolDescriptors
        let tools: Vec<crate::react::ToolDescriptor> = ctx
            .capabilities
            .iter()
            .map(|c| crate::react::ToolDescriptor {
                name: c.name.clone(),
                description: c.description.clone(),
                parameters: c.parameters.clone(),
            })
            .collect();

        // Build initial messages from observations
        let mut messages = Self::observations_to_messages(&observations, &[]);

        // ── ReAct loop ──────────────────────────────────────────────
        let final_content: String;
        let mut turn = 0u32;
        loop {
            if turn >= max_turns {
                return Err(CognitiveError::MaxDepthReached { depth: turn });
            }

            // Publish llm:call_started
            if let Some(ref sink) = self.event_sink {
                sink(kernel::event::Event::new(
                    "cognitive-engine",
                    kernel::event::EventType::Custom("llm:call_started".into()),
                    serde_json::json!({ "agent_id": &agent_id, "session_id": &session_id, "turn": turn }),
                ));
            }

            let request = LlmChatRequest {
                model: self.config.model.clone(),
                system_prompt: soul.system_prompt.clone(),
                messages: messages.clone(),
                tools: tools.clone(),
                max_output_tokens: self.config.max_output_tokens as u32,
                response_format: None,
            };

            // ── Streaming callback (if listeners registered) ──────────
            let stream_cb: Option<Arc<dyn Fn(crate::provider::StreamEvent) + Send + Sync>> = {
                let guard = self.listeners.lock().ok();
                if guard.as_ref().is_some_and(|l| !l.is_empty()) {
                    let listeners = Arc::clone(&self.listeners);
                    let sid = session_id.clone();
                    Some(Arc::new(move |evt: crate::provider::StreamEvent| {
                        if let Ok(guard) = listeners.lock() {
                            let ce = match evt {
                                crate::provider::StreamEvent::Start =>
                                    CognitiveEvent::StreamStart { session_id: sid.clone() },
                                crate::provider::StreamEvent::Chunk(text) =>
                                    CognitiveEvent::TextChunk { session_id: sid.clone(), text },
                                crate::provider::StreamEvent::Done { finish_reason } =>
                                    CognitiveEvent::StreamDone { session_id: sid.clone(), finish_reason },
                                crate::provider::StreamEvent::Error(err) =>
                                    CognitiveEvent::StreamError { session_id: sid.clone(), error: err },
                            };
                            for l in guard.iter() { l.on_cognitive_event(ce.clone()); }
                        }
                    }))
                } else { None }
            };

            // LLM call with retry + streaming
            let mut llm_attempt = 0u32;
            let response = loop {
                llm_attempt += 1;
                let r = self.provider.chat_completion(request.clone(), stream_cb.clone()).await;
                if !r.is_err() || llm_attempt >= max_retries
                    || !is_retryable_llm_error(r.as_ref().err())
                { break r; }
                let delay = (3_u64.pow(llm_attempt - 1)).min(120);
                tracing::warn!(%agent_id, %session_id, turn, attempt=llm_attempt, delay, "LLM retry (cognitive engine)");
                tokio::time::sleep(std::time::Duration::from_secs(delay)).await;
            };

            // Publish llm:call_ended
            if let Some(ref sink) = self.event_sink {
                sink(kernel::event::Event::new(
                    "cognitive-engine",
                    kernel::event::EventType::Custom("llm:call_ended".into()),
                    serde_json::json!({ "agent_id": &agent_id, "session_id": &session_id, "turn": turn, "success": response.is_ok() }),
                ));
            }

            let response = response.map_err(|e| CognitiveError::EngineError {
                engine_name: self.name().to_owned(), message: e,
            })?;

            // Output validation
            let content = {
                let mut v = kernel::validator::OutputValidator::new();
                match v.validate(&response.content, kernel::types::TrustLevel::Untrusted) {
                    kernel::validator::ValidationOutcome::Pass => response.content,
                    kernel::validator::ValidationOutcome::Fail { .. } => {
                        "[I apologize, I cannot provide that response.]".into()
                    }
                    kernel::validator::ValidationOutcome::Error { message } => {
                        return Err(CognitiveError::EngineError {
                            engine_name: self.name().to_owned(),
                            message: format!("output validation error: {message}"),
                        });
                    }
                }
            };

            // Content filter
            let cf = kernel::content_filter::ContentFilter::new();
            let content = match cf.filter(&content) {
                kernel::content_filter::FilterDecision::Block { .. } => {
                    "[I apologize, I cannot provide that response.]".into()
                }
                _ => content,
            };

            // Token event
            if let Some(ref sink) = self.event_sink {
                sink(kernel::event::Event::new(
                    "cognitive-engine",
                    kernel::event::EventType::Custom("agent:token_used".into()),
                    serde_json::json!({ "agent_id": &agent_id, "session_id": &session_id, "turn": turn, "tokens": (content.len() / 4) as u64 }),
                ));
            }

            if response.tool_calls.is_empty() {
                // Finished
                final_content = content;
                break;
            }

            // If tool execution is not configured, return tool calls as decisions
            // (single-turn mode — the caller handles tool execution)
            if self.tool_registry.is_none() {
                return Ok(vec![Decision::call_tools(
                    Self::new_decision_id(),
                    &session_id,
                    response.tool_calls.into_iter().map(|c| ToolCallRequest {
                        id: c.id, tool_name: c.tool_name, args: c.args, detach: false,
                    }).collect(),
                )]);
            }

            // Tool calls — execute and feed back
            let parsed_calls = response.tool_calls;
            messages.push(ChatMessage {
                role: crate::react::ChatMessageRole::Assistant,
                content,
                tool_call_id: None,
                tool_name: None,
                tool_calls: Some(parsed_calls.iter().map(|c| serde_json::json!({
                    "id": c.id, "type": "function",
                    "function": { "name": c.tool_name, "arguments": serde_json::to_string(&c.args).unwrap_or_default() }
                })).collect()),
                reasoning_content: response.reasoning_content,
            });

            // Execute tools (with retry + security)
            let results = self.execute_tool_calls(&parsed_calls).await;
            for r in &results {
                messages.push(ChatMessage::tool_result(&r.id, &r.tool_name, &r.output));
            }

            turn += 1;
        }

        Ok(vec![Decision::reply(
            Self::new_decision_id(),
            &session_id,
            final_content,
        )])
    }

    fn subscribe(&self, listener: Arc<dyn CognitiveListener>) {
        if let Ok(mut guard) = self.listeners.lock() {
            guard.push(listener);
        }
    }

    fn unsubscribe(&self, listener: &Arc<dyn CognitiveListener>) {
        if let Ok(mut guard) = self.listeners.lock() {
            let ptr = Arc::as_ptr(listener);
            guard.retain(|l| !std::ptr::addr_eq(Arc::as_ptr(l), ptr));
        }
    }

    async fn reset_session(&self, _session_id: &str) -> Result<(), CognitiveError> {
        // Session state is managed externally (conversation history is
        // passed via observations). Nothing to reset internally.
        Ok(())
    }
}
