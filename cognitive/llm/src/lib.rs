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
//!     ├── ReAct loop:
//!     │   ├── Token budget check + auto-continue (background)
//!     │   ├── LLM call (retry, streaming via CognitiveListener)
//!     │   ├── OutputValidator + ContentFilter
//!     │   └── If tool_calls: execute (parallel/serial, retry,
//!     │       security, detach) → feed back → loop
//!     └── Return Reply decision
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
use crate::react::{ChatMessage, SoulSnapshot};

// Re-export for integration tests
pub use tool::ToolRegistry;

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
    /// Whether this is a background (idle) run. Background runs
    /// auto-continue after max_turns is reached, up to max_continuations.
    pub background: bool,
    /// Maximum auto-continuations for background runs (default: 5).
    pub max_continuations: u32,
    /// Inject a format reminder after this many turns if tools were used
    /// (0 = disabled). The reminder nudges the LLM to produce complete output
    /// with all sections filled when working on complex multi-turn tasks.
    pub format_reminder_turns: u32,
}

impl Default for LlmEngineConfig {
    fn default() -> Self {
        Self {
            model: "gpt-4o".into(),
            max_turns: 64,
            token_limit: 128_000,
            max_output_tokens: 4096,
            max_llm_retries: 5,
            background: false,
            max_continuations: 5,
            format_reminder_turns: 0,
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
    /// Optional interrupt flag for external /stop during ReAct loop.
    interrupt_flag: Option<Arc<cognitive_react::InterruptFlag>>,
    /// Optional consciousness provider — gates the ReAct loop when the
    /// LLM backend is unavailable (Catatonic / Coma).
    consciousness: Option<Arc<dyn cognitive_engine::ConsciousnessProvider>>,
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
            interrupt_flag: None,
            consciousness: None,
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
            interrupt_flag: None,
            consciousness: None,
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

    /// Set an interrupt flag that the engine checks during the ReAct loop.
    ///
    /// When set, each iteration of the loop checks `flag.is_interrupted()`
    /// before the LLM call and after tool execution. If the flag is set,
    /// the engine returns `CognitiveError::Interrupted`.
    #[must_use]
    pub fn with_interrupt_flag(mut self, flag: Arc<cognitive_react::InterruptFlag>) -> Self {
        self.interrupt_flag = Some(flag);
        self
    }

    /// Set a consciousness provider that gates the ReAct loop.
    ///
    /// When the agent's cognitive state is [`CognitiveState::Catatonic`] or
    /// [`CognitiveState::Coma`], [`CognitiveEngine::process()`] returns a
    /// graceful "unavailable" reply **without** invoking the LLM. This avoids
    /// wasted calls and cascading failures when the backend is down.
    ///
    /// When `None` (the default), the engine behaves as before — callers
    /// remain responsible for any health gating.
    #[must_use]
    pub fn with_consciousness_provider(
        mut self,
        provider: Arc<dyn cognitive_engine::ConsciousnessProvider>,
    ) -> Self {
        self.consciousness = Some(provider);
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
        session_id: &str,
        agent_id: &str,
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
                let sid = session_id.to_owned();
                let aid = agent_id.to_owned();
                handles.push((i, tokio::spawn(async move {
                    execute_one_with_retry(&reg, &b, &c, t, TOOL_MAX_RETRIES, &sid, &aid).await
                })));
            }
        }

        // Phase 2: Stateful/SideEffect calls sequentially
        let mut serial_results: Vec<(usize, crate::react::ToolCallResult)> = Vec::new();
        for (i, call) in calls.iter().enumerate() {
            if !models[i] {
                let result = execute_one_with_retry(&registry, &bus, call, timeout, TOOL_MAX_RETRIES, session_id, agent_id).await;
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

}

// ── Retry helpers (mirrors LlmReActEngine) ────────────────────────

/// Execute one tool call with retry, security checks, and event publishing.
async fn execute_one_with_retry(
    registry: &tool::ToolRegistry,
    bus: &Arc<dyn event_bus::EventBus>,
    call: &crate::react::ParsedToolCall,
    timeout_ms: u64,
    max_retries: u32,
    session_id: &str,
    agent_id: &str,
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
        // Propagate session_id so stateful tools (e.g. exec detach → child
        // registry) can tag spawned processes with their owning session and
        // kill them when the session ends.
        ctx.base
            .extensions
            .insert("session_id".to_string(), serde_json::json!(session_id));

        let result = tool.execute(call.args.clone(), ctx).await;
        let duration_ms = start.elapsed().as_millis() as u64;
        let success = result.is_ok();
        let output = match &result {
            Ok(v) => serde_json::to_string(v).unwrap_or_default(),
            Err(e) => e.to_string(),
        };

        // Publish tool:completed.  Includes `output` so that a later
        // "继续" (continue) can reconstruct the Tool-role ChatMessage
        // with the actual result text — not just a success/fail flag.
        let _ = bus.publish(kernel::event::Event::new(
            "cognitive-engine",
            kernel::event::EventType::Custom("tool:completed".into()),
            serde_json::json!({
                "tool_call_id": call.id, "tool_name": call.tool_name,
                "success": success, "output": output,
                "duration_ms": duration_ms,
                "session_id": session_id,
                "agent_id": agent_id,
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

// ── Continuation Context — prescriptive auto-continue guidance ─────
//
// Instead of a flat descriptive summary, the continuation context now
// provides strategic direction for the next round:
//   1. Tool effectiveness analysis (what worked vs what to avoid)
//   2. Approach tracking (what strategy was attempted)
//   3. Unresolved items (what still needs attention)
//   4. Lessons learned (concrete guidance for the next round)
//
// This turns auto-continue from blind retry into guided iteration.

/// Record of a completed continuation round — enables cross-round memory.
///
/// After each auto-continue, the engine stores one of these so the next
/// round's continuation context can reference what was tried before:
/// "Round 1: grep was effective. Round 2: find failed. Round 3: do NOT use find."
#[derive(Debug, Clone)]
struct ContinuationRecord {
    round: u32,
    approach: String,
    effective_tools: Vec<String>,
    ineffective_tools: Vec<String>,
    key_findings: Vec<String>,
    lesson: Option<String>,
}

/// Structured continuation context — prescriptive, not just descriptive.
struct ContinuationContext {
    goal: String,
    round: u32,
    max_rounds: u32,
    approach_description: String,
    effective_patterns: Vec<EffectivePattern>,
    ineffective_patterns: Vec<IneffectivePattern>,
    unresolved_items: Vec<String>,
    tool_stats: Vec<ToolStat>,
    key_findings: Vec<String>,
    status: ContinuationStatus,
    last_action: String,
    lesson: Option<String>,
    /// Prior continuation rounds — enables cross-round methodological memory.
    prior_rounds: Vec<ContinuationRecord>,
}

struct EffectivePattern {
    description: String,
    evidence: String,
}

struct IneffectivePattern {
    description: String,
    reason: String,
}

struct ToolStat {
    name: String,
    calls: u32,
    successes: u32,
    key_output: Option<String>,
}

#[derive(Debug, PartialEq, Eq)]
enum ContinuationStatus {
    CollisionFound,
    Advancing,
    Incomplete,
    Stuck,
}

impl ContinuationContext {
    fn render(&self) -> String {
        let mut s = String::new();

        // ── Prior rounds (cross-round memory) ──
        if !self.prior_rounds.is_empty() {
            s.push_str("[Prior Rounds]\n");
            for pr in &self.prior_rounds {
                s.push_str(&format!("  Round {}: {}\n", pr.round, pr.approach));
                if !pr.effective_tools.is_empty() {
                    s.push_str(&format!("    ✅ effective: {}\n", pr.effective_tools.join(", ")));
                }
                if !pr.ineffective_tools.is_empty() {
                    s.push_str(&format!("    ❌ ineffective: {}\n", pr.ineffective_tools.join(", ")));
                }
                if !pr.key_findings.is_empty() {
                    for f in &pr.key_findings {
                        s.push_str(&format!("    🔍 {}\n", f));
                    }
                }
                if let Some(ref lesson) = pr.lesson {
                    s.push_str(&format!("    💡 lesson: {}\n", lesson));
                }
            }
            s.push('\n');
        }

        s.push_str(&format!(
            "[Continuation Context — Round {}/{}]\n",
            self.round, self.max_rounds
        ));
        s.push_str(&format!("Goal: {}\n", self.goal));

        if !self.approach_description.is_empty() {
            s.push_str(&format!("\nApproach so far: {}\n", self.approach_description));
        }

        // ── Effective patterns ──
        if !self.effective_patterns.is_empty() {
            s.push_str("\n✅ EFFECTIVE (continue using):\n");
            for p in &self.effective_patterns {
                s.push_str(&format!("  {} — {}\n", p.description, p.evidence));
            }
        }

        // ── Ineffective patterns ──
        if !self.ineffective_patterns.is_empty() {
            s.push_str("\n❌ INEFFECTIVE (DO NOT retry):\n");
            for p in &self.ineffective_patterns {
                s.push_str(&format!("  {} — {}\n", p.description, p.reason));
            }
        }

        // ── Unresolved items ──
        if !self.unresolved_items.is_empty() {
            s.push_str("\n⚠️ UNRESOLVED (needs attention this round):\n");
            for item in &self.unresolved_items {
                s.push_str(&format!("  - {}\n", item));
            }
        }

        // ── Tool stats ──
        let total_calls: u32 = self.tool_stats.iter().map(|t| t.calls).sum();
        let total_successes: u32 = self.tool_stats.iter().map(|t| t.successes).sum();
        if total_calls > 0 {
            s.push_str(&format!(
                "\nTool usage: {} calls across {} tools ({} succeeded, {} failed)\n",
                total_calls,
                self.tool_stats.len(),
                total_successes,
                total_calls.saturating_sub(total_successes),
            ));
            for stat in &self.tool_stats {
                s.push_str(&format!(
                    "  {}: {} calls ({} successes)",
                    stat.name, stat.calls, stat.successes
                ));
                if let Some(ref ko) = stat.key_output {
                    s.push_str(&format!(" — key output: {}", ko));
                }
                s.push('\n');
            }
        }

        // ── Key findings ──
        if !self.key_findings.is_empty() {
            s.push_str("\nKey findings:\n");
            for f in &self.key_findings {
                s.push_str(&format!("  - {}\n", f));
            }
        }

        // ── Status ──
        let status_str = match self.status {
            ContinuationStatus::CollisionFound => "complete — goal achieved",
            ContinuationStatus::Advancing => "advancing — clear progress, continue current approach",
            ContinuationStatus::Incomplete => "incomplete — task still in progress",
            ContinuationStatus::Stuck => "stuck — no progress detected, MUST change approach",
        };
        s.push_str(&format!("\nStatus: {}\n", status_str));

        // ── Last action ──
        if !self.last_action.is_empty() {
            let truncated = if self.last_action.len() > 500 {
                format!("{}…[truncated]", &self.last_action[..500])
            } else {
                self.last_action.clone()
            };
            s.push_str(&format!("Last action: {}\n", truncated));
        }

        // ── Lesson (prescriptive guidance) ──
        if let Some(ref lesson) = self.lesson {
            s.push_str(&format!(
                "\n💡 LESSON FOR THIS ROUND: {}\n", lesson
            ));
        }

        s
    }
}

/// Infer whether a tool execution succeeded from its output text.
///
/// Heuristic-based — ToolCallResult.success is discarded when converting
/// to ChatMessage, so we analyze the output string.
fn infer_tool_success(output: &str) -> bool {
    let lower = output.to_lowercase();
    // Explicit failure JSON
    if lower.contains("\"success\": false") || lower.contains("\"success\":false") {
        return false;
    }
    // Common error patterns
    if lower.contains("error:") || lower.contains("failed:") || lower.contains("timed out")
        || lower.contains("permission denied") || lower.contains("not found")
        || lower.contains("no such file") || lower.contains("connection refused")
        || lower.contains("tool panicked") || lower.contains("hardline_blocked")
    {
        return false;
    }
    // Explicit success JSON
    if lower.contains("\"success\": true") || lower.contains("\"success\":true") {
        return true;
    }
    // Default: non-empty output without obvious errors → likely success
    !output.trim().is_empty()
}

/// Extract a short signal string from tool output for human-readable context.
///
/// Looks for count/quantity indicators and trims to a short line.
fn extract_tool_output_signal(output: &str) -> Option<String> {
    let trimmed = output.trim();
    if trimmed.is_empty() || trimmed.len() > 2000 {
        return None;
    }
    // Try to extract a key metric line
    for line in trimmed.lines() {
        let lower = line.to_lowercase();
        if lower.contains("found") || lower.contains("match")
            || lower.contains("result") || lower.contains("count")
            || lower.contains("total") || lower.contains("file")
            || lower.contains("line") || lower.contains("bytes")
            || lower.contains("elapsed") || lower.contains("rows")
            || lower.contains("items")
        {
            let short = if line.len() > 120 {
                format!("{}…", &line[..120])
            } else {
                line.to_owned()
            };
            return Some(short);
        }
    }
    // Fallback: first non-empty line (truncated)
    let first_line = trimmed.lines().next()?;
    let short = if first_line.len() > 100 {
        format!("{}…", &first_line[..100])
    } else {
        first_line.to_owned()
    };
    Some(short)
}

/// Build a prescriptive continuation context that guides the next round.
///
/// Unlike the old `build_continuation_context_summary` which only described
/// what happened, this analyzes tool effectiveness, identifies patterns to
/// continue/avoid, and generates prescriptive guidance for the next round.
fn build_continuation_context(
    messages: &[crate::react::ChatMessage],
    round: u32,
    max_rounds: u32,
    prior_rounds: Vec<ContinuationRecord>,
) -> ContinuationContext {
    use std::collections::BTreeMap;

    let mut user_messages: Vec<&str> = Vec::new();
    let mut assistant_replies: Vec<&str> = Vec::new();
    let mut last_assistant_reply: &str = "";

    // Per-tool stats: (calls, successes, best_output_signal)
    let mut tool_raw: BTreeMap<String, (u32, u32, Option<String>)> = BTreeMap::new();
    let mut key_findings: Vec<String> = Vec::new();

    for msg in messages {
        match msg.role {
            crate::react::ChatMessageRole::User => {
                let text = msg.content.trim();
                if !text.is_empty() && text != "/continue" && text != "继续"
                    && !text.starts_with("[ACTIVATED SKILL:")
                {
                    user_messages.push(text);
                }
            }
            crate::react::ChatMessageRole::Assistant => {
                let text = msg.content.trim();
                if text.starts_with("[max ") && text.contains("turns reached") {
                    continue;
                }
                if !text.is_empty() {
                    assistant_replies.push(text);
                    last_assistant_reply = text;
                }
                // Count tool calls from assistant messages (pre-execution)
                if let Some(ref tcs) = msg.tool_calls {
                    for tc in tcs {
                        if let Some(name) = tc["function"]["name"].as_str() {
                            let entry = tool_raw.entry(name.to_owned()).or_insert((0, 0, None));
                            entry.0 += 1;
                        }
                    }
                }
            }
            crate::react::ChatMessageRole::Tool => {
                let name = msg.tool_name.as_deref().unwrap_or("unknown");
                let content = &msg.content;
                let success = infer_tool_success(content);
                let signal = extract_tool_output_signal(content);

                let entry = tool_raw.entry(name.to_owned()).or_insert((0, 0, None));
                entry.0 += 1;
                if success {
                    entry.1 += 1;
                }
                // Keep the best signal (prefer success signals)
                if signal.is_some() && (entry.2.is_none() || success) {
                    entry.2 = signal;
                }

                // Extract key findings
                if content.contains("COLLISION FOUND")
                    || content.contains("found\": true")
                    || content.contains("found: true")
                {
                    key_findings.push(format!("[{name}] COLLISION FOUND — goal achieved"));
                } else if let Some(line) = content.lines().find(|l| {
                    l.contains("best_residual") || l.contains("best_partial")
                        || l.contains("elapsed_seconds")
                }) {
                    key_findings.push(format!("[{name}] {}", line.trim()));
                }
            }
            crate::react::ChatMessageRole::System => {}
        }
    }

    // ── Build tool stats ──
    let tool_stats: Vec<ToolStat> = tool_raw
        .into_iter()
        .map(|(name, (calls, successes, key_output))| ToolStat {
            name,
            calls,
            successes,
            key_output,
        })
        .collect();

    // ── Categorize effective / ineffective patterns ──
    let mut effective_patterns = Vec::new();
    let mut ineffective_patterns = Vec::new();

    for stat in &tool_stats {
        if stat.calls == 0 {
            continue;
        }
        let rate = stat.successes as f64 / stat.calls as f64;
        let signal_desc = stat.key_output.as_deref().unwrap_or("no output signal");

        if rate >= 0.7 && stat.calls >= 2 {
            effective_patterns.push(EffectivePattern {
                description: format!("{} ({}/{} calls)", stat.name, stat.successes, stat.calls),
                evidence: signal_desc.to_owned(),
            });
        } else if rate <= 0.3 && stat.calls >= 2 {
            let reason = if stat.successes == 0 {
                format!("all {} calls failed — do not use this tool for this task again", stat.calls)
            } else {
                format!("{}/{} calls failed — reconsider approach", stat.calls - stat.successes, stat.calls)
            };
            ineffective_patterns.push(IneffectivePattern {
                description: format!("{} ({}/{})", stat.name, stat.successes, stat.calls),
                reason,
            });
        }
    }

    // ── Extract approach description from assistant replies ──
    let approach_description = extract_approach_description(&assistant_replies);

    // ── Detect unresolved items ──
    let unresolved_items = detect_unresolved_items(&assistant_replies, &tool_stats);

    // ── Determine status ──
    let status = if key_findings.iter().any(|f| f.contains("COLLISION FOUND")) {
        ContinuationStatus::CollisionFound
    } else if last_assistant_reply.contains("stuck")
        || last_assistant_reply.contains("no progress")
    {
        ContinuationStatus::Stuck
    } else if !effective_patterns.is_empty() {
        ContinuationStatus::Advancing
    } else {
        ContinuationStatus::Incomplete
    };

    // ── Generate lesson (with cross-round history) ──
    let lesson = generate_lesson(
        round,
        &effective_patterns,
        &ineffective_patterns,
        &unresolved_items,
        &status,
        &prior_rounds,
    );

    let goal = user_messages
        .first()
        .map(|s| s.to_string())
        .unwrap_or_else(|| "(unknown goal)".to_owned());

    ContinuationContext {
        goal,
        round,
        max_rounds,
        approach_description,
        effective_patterns,
        ineffective_patterns,
        unresolved_items,
        tool_stats,
        key_findings,
        status,
        last_action: last_assistant_reply.to_owned(),
        lesson,
        prior_rounds,
    }
}

/// Extract a human-readable approach description from assistant replies.
///
/// Looks for strategic intent signals: "I'll", "let me", "my approach",
/// "I'm going to", "the plan is", etc.
fn extract_approach_description(assistant_replies: &[&str]) -> String {
    // Scan the first 3 assistant replies for approach-defining language
    for reply in assistant_replies.iter().take(3) {
        let lower = reply.to_lowercase();
        for marker in [
            "i'll", "let me", "my approach", "i'm going to",
            "the plan is", "first, i'll", "strategy:",
            "i will start by", "let's begin by",
        ] {
            if let Some(pos) = lower.find(marker) {
                let start = reply[..pos].rfind(['.', '\n'])
                    .map(|p| p + 1)
                    .unwrap_or(0);
                let end = reply[start..]
                    .find('\n')
                    .map(|p| start + p)
                    .unwrap_or(reply.len());
                let snippet = reply[start..end].trim();
                if snippet.len() >= 20 && snippet.len() <= 300 {
                    return snippet.to_owned();
                }
            }
        }
    }
    // Fallback: use first assistant reply (truncated)
    assistant_replies
        .first()
        .map(|r| {
            if r.len() > 200 {
                format!("{}…", &r[..200])
            } else {
                r.to_string()
            }
        })
        .unwrap_or_default()
}

/// Detect unresolved items from assistant messages.
///
/// Scans for signals that the agent acknowledged something isn't done yet.
fn detect_unresolved_items(
    assistant_replies: &[&str],
    _tool_stats: &[ToolStat],
) -> Vec<String> {
    let mut items = Vec::new();
    for reply in assistant_replies.iter().rev().take(2) {
        let lower = reply.to_lowercase();
        for marker in [
            "still need to", "haven't yet", "not yet", "remaining:",
            "still missing", "todo:", "left to do",
        ] {
            if let Some(pos) = lower.find(marker) {
                let end = reply[pos..]
                    .find(['.', '\n'])
                    .map(|p| pos + p)
                    .unwrap_or(reply.len());
                let item = reply[pos..end].trim();
                let item_str = item.to_string();
                if item.len() >= 10 && !items.contains(&item_str) {
                    items.push(item_str);
                }
            }
        }
    }
    items
}

/// Generate prescriptive lesson for the next round.
///
/// This is the key upgrade from descriptive → prescriptive.
/// The lesson gives the LLM concrete guidance on what to do differently.
fn generate_lesson(
    round: u32,
    effective: &[EffectivePattern],
    ineffective: &[IneffectivePattern],
    unresolved: &[String],
    status: &ContinuationStatus,
    prior_rounds: &[ContinuationRecord],
) -> Option<String> {
    // ── Cross-round: accumulate globally ineffective tools ──
    let mut global_ineffective: Vec<String> = Vec::new();
    for pr in prior_rounds {
        for tool in &pr.ineffective_tools {
            if !global_ineffective.contains(tool) {
                global_ineffective.push(tool.clone());
            }
        }
    }
    // Add current round's ineffective tools
    for p in ineffective {
        let name = p.description.split(' ').next().unwrap_or("unknown");
        if !global_ineffective.contains(&name.to_string()) {
            global_ineffective.push(name.to_string());
        }
    }

    // ── Cross-round: count distinct approaches tried ──
    let approaches_tried: Vec<&str> = prior_rounds.iter()
        .map(|pr| pr.approach.as_str())
        .collect();
    let distinct_approaches: std::collections::BTreeSet<&str> = approaches_tried.iter().copied().collect();

    match status {
        ContinuationStatus::CollisionFound => None,

        ContinuationStatus::Stuck => {
            if !global_ineffective.is_empty() {
                Some(format!(
                    "Previous approach has stalled. Across {} rounds, these tools consistently failed: {}. \
                     ABANDON your current strategy. Try a fundamentally different method — \
                     different tools, different search space, or break the problem into smaller sub-problems.",
                    prior_rounds.len() + 1,
                    global_ineffective.join(", ")
                ))
            } else {
                Some(
                    "Previous approach has stalled. ABANDON your current strategy entirely. \
                     Try a fundamentally different method — different tools, different search space, \
                     or break the problem into smaller sub-problems.".into()
                )
            }
        }

        ContinuationStatus::Advancing if !ineffective.is_empty() => {
            let avoid: Vec<&str> = ineffective.iter().map(|p| p.description.as_str()).collect();
            let global_avoid = if !global_ineffective.is_empty() {
                format!(
                    " (across all rounds, consistently avoid: {})",
                    global_ineffective.join(", ")
                )
            } else {
                String::new()
            };
            Some(format!(
                "Progress is good, but avoid: {}.{} Focus on the effective patterns \
                 and address unresolved items before the budget runs out.",
                avoid.join(", "),
                global_avoid
            ))
        }

        ContinuationStatus::Advancing => Some(
            "Good progress. Narrow further: use the effective patterns to drill \
             into specific findings rather than expanding scope.".into()
        ),

        ContinuationStatus::Incomplete if round >= 3 => {
            let focus: Vec<&str> = unresolved.iter().map(|s| s.as_str()).collect();
            let cross_round_note = if distinct_approaches.len() >= 2 {
                format!(
                    " You've tried {} different approaches: {}. ",
                    distinct_approaches.len(),
                    distinct_approaches.iter().copied().collect::<Vec<_>>().join(", ")
                )
            } else {
                String::new()
            };
            if focus.is_empty() {
                Some(format!(
                    "Running out of rounds.{}Prioritize: produce the BEST PARTIAL RESULT \
                     you can with current data — don't start new searches.",
                    cross_round_note
                ))
            } else {
                Some(format!(
                    "Running out of rounds.{}Focus ONLY on: {}. \
                     Do not start new investigations — complete what's pending.",
                    cross_round_note,
                    focus.join("; ")
                ))
            }
        }

        ContinuationStatus::Incomplete => {
            if effective.is_empty() && !unresolved.is_empty() {
                let focus: Vec<&str> = unresolved.iter().map(|s| s.as_str()).collect();
                Some(format!(
                    "No clear effective patterns yet. Narrow your focus: {}. \
                     Try ONE targeted approach and verify results before expanding.",
                    focus.join("; ")
                ))
            } else {
                None
            }
        }
    }
}

// ── Session progress evaluation (gradient) ──────────────────────────
//
// Replaces the binary SessionProgress with a five-level gradient.
// Each level drives different auto-continue behaviour:
//   Achieved  → stop, return result
//   Advancing → continue WITHOUT consuming budget (valuable work)
//   Creeping  → continue, consume budget, inject stronger pivot
//   Circling  → one more chance with forced approach-change
//   Stuck     → stop immediately

/// Five-level progress gradient for auto-continue decisions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProgressLevel {
    /// Goal achieved — collision found or task explicitly completed.
    Achieved,
    /// Clear forward progress — tools succeeding, new findings, search converging.
    Advancing,
    /// Slow progress — some output but high effort-to-signal ratio.
    Creeping,
    /// Going in circles — repetitive output, no new information.
    Circling,
    /// Completely stuck — no progress for many turns.
    Stuck,
}

/// Evaluate session progress as a gradient level from ChatMessages.
///
/// Detection signals (in priority order):
/// 1. Collision found → Achieved
/// 2. Assistant says "stuck"/"no progress" → Stuck
/// 3. Jaccard word overlap > 0.8 across 3+ consecutive replies → Circling
/// 4. 100+ messages with zero partial-progress signals → Stuck
/// 5. Tool success rate analysis:
///    - ≥70% success, new findings present → Advancing
///    - 30-70% success or many tools but few signals → Creeping
///    - <30% success after 3+ calls → Circling
/// 6. Default: Advancing (assume progress until proven otherwise)
fn evaluate_progress_level(messages: &[crate::react::ChatMessage]) -> ProgressLevel {
    use std::collections::BTreeSet;

    let recent: Vec<&str> = messages
        .iter()
        .rev()
        .take(64)
        .map(|m| m.content.as_str())
        .collect();

    let full_text = recent.join(" ").to_lowercase();

    // ── 1. Collision / explicit success detection ──
    let collision_found = full_text.contains("collision found")
        || full_text.contains("✅ collision")
        || full_text.contains("verify_collision_solution");
    if collision_found {
        return ProgressLevel::Achieved;
    }

    // ── 2. Explicit stuck declaration ──
    let last_assistant = messages
        .iter()
        .rev()
        .find(|m| m.role == crate::react::ChatMessageRole::Assistant)
        .map(|m| m.content.to_lowercase())
        .unwrap_or_default();
    if last_assistant.contains("stuck") || last_assistant.contains("no progress") {
        return ProgressLevel::Stuck;
    }

    // ── 3. Tool success rate analysis ──
    let (total_tool_msgs, total_tool_successes) = count_tool_results(messages);
    let tool_success_rate = if total_tool_msgs > 0 {
        total_tool_successes as f64 / total_tool_msgs as f64
    } else {
        1.0 // No tools used yet — can't judge
    };

    // ── 4. Jaccard word overlap on last 8 assistant messages → Circling ──
    let assistant_msgs: Vec<&str> = messages
        .iter()
        .rev()
        .filter(|m| m.role == crate::react::ChatMessageRole::Assistant)
        .take(8)
        .map(|m| m.content.as_str())
        .collect();

    let jaccard_similar_pairs = if assistant_msgs.len() >= 4 {
        assistant_msgs
            .windows(2)
            .filter(|w| {
                let wa: BTreeSet<&str> = w[0].split_whitespace().collect();
                let wb: BTreeSet<&str> = w[1].split_whitespace().collect();
                if wa.is_empty() || wb.is_empty() {
                    return false;
                }
                let intersection = wa.intersection(&wb).count();
                let union = wa.union(&wb).count();
                (intersection as f64 / union as f64) > 0.8
            })
            .count()
    } else {
        0
    };

    if jaccard_similar_pairs >= 3 {
        return ProgressLevel::Circling;
    }

    // ── 5. Long session + zero progress signals → Stuck ──
    if messages.len() > 100 {
        let has_partial = recent.iter().any(|line| {
            let lower = line.to_lowercase();
            lower.contains("best_match=")
                || lower.contains("match=")
                || lower.contains("best partial match:")
                || lower.contains("found")
                || lower.contains("result")
        });
        if !has_partial {
            return ProgressLevel::Stuck;
        }
    }

    // ── 6. Tool success rate + findings → gradient decision ──
    let has_new_findings = recent.iter().any(|line| {
        let lower = line.to_lowercase();
        lower.contains("found") || lower.contains("match")
            || lower.contains("discovered") || lower.contains("identified")
    });

    if total_tool_msgs >= 3 && tool_success_rate < 0.3 {
        // Most tools failing → Circling
        ProgressLevel::Circling
    } else if total_tool_msgs >= 5 && tool_success_rate < 0.5 {
        // Marginal success rate → Creeping
        ProgressLevel::Creeping
    } else if total_tool_msgs >= 10 && !has_new_findings {
        // Lots of tool calls but no discoveries → Creeping
        ProgressLevel::Creeping
    } else {
        // Default: assume advancing
        ProgressLevel::Advancing
    }
}

/// Count tool result messages and how many appear successful.
fn count_tool_results(messages: &[crate::react::ChatMessage]) -> (usize, usize) {
    let mut total = 0;
    let mut successes = 0;
    for msg in messages {
        if msg.role == crate::react::ChatMessageRole::Tool {
            total += 1;
            if infer_tool_success(&msg.content) {
                successes += 1;
            }
        }
    }
    (total, successes)
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

        // ── Consciousness guard: backend unavailable → graceful short-circuit ──
        // The gateway already checks this at the harness layer; the engine
        // checks again here as defence-in-depth so *any* caller (anonymous
        // messages, idle loops, direct engine users, tests) is covered.
        if let Some(ref provider) = self.consciousness {
            let state = provider.state();
            if let Some(msg) = state.guard_check() {
                tracing::warn!(
                    agent_id = %ctx.agent_id,
                    session_id = %ctx.session_id,
                    ?state,
                    "cognitive state blocks processing: {msg}"
                );
                return Ok(vec![Decision::reply(
                    Self::new_decision_id(),
                    &ctx.session_id,
                    msg.to_string(),
                )]);
            }
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
        let mut messages = Self::observations_to_messages(&observations, &ctx.conversation_history);

        // ── ReAct loop ──────────────────────────────────────────────
        let final_content: String;
        let mut turn = 0u32;
        let mut continuation = 0u32;
        let mut consecutive_circling = 0u32;
        let mut continuation_history: Vec<ContinuationRecord> = Vec::new();
        let mut format_reminder_fired = false;
        loop {
            // ── Pre-turn: max turns check + auto-continue ────────────
            if turn >= max_turns {
                if self.config.background && continuation < self.config.max_continuations {
                    // Evaluate session progress with gradient levels
                    let level = evaluate_progress_level(&messages);

                    match level {
                        ProgressLevel::Achieved | ProgressLevel::Stuck => {
                            if let Some(ref sink) = self.event_sink {
                                sink(kernel::event::Event::new(
                                    "cognitive-engine",
                                    kernel::event::EventType::Custom("agent:auto_continue_stopped".into()),
                                    serde_json::json!({
                                        "agent_id": &agent_id, "session_id": &session_id,
                                        "continuation": continuation,
                                        "level": format!("{:?}", level),
                                    }),
                                ));
                            }
                            return Err(CognitiveError::MaxDepthReached { depth: turn });
                        }
                        ProgressLevel::Circling => {
                            consecutive_circling += 1;
                            if consecutive_circling >= 2 {
                                if let Some(ref sink) = self.event_sink {
                                    sink(kernel::event::Event::new(
                                        "cognitive-engine",
                                        kernel::event::EventType::Custom("agent:auto_continue_stopped".into()),
                                        serde_json::json!({
                                            "agent_id": &agent_id, "session_id": &session_id,
                                            "continuation": continuation,
                                            "level": "Circling",
                                            "consecutive_circling": consecutive_circling,
                                        }),
                                    ));
                                }
                                return Err(CognitiveError::MaxDepthReached { depth: turn });
                            }
                            // Allow one more try — consume budget, force pivot
                            continuation += 1;
                        }
                        ProgressLevel::Creeping => {
                            consecutive_circling = 0;
                            continuation += 1; // Consume budget
                        }
                        ProgressLevel::Advancing => {
                            consecutive_circling = 0;
                            // Don't increment continuation — valuable work gets more budget
                        }
                    }

                    turn = 0;
                    // Build prescriptive continuation context with cross-round history
                    let ctx = build_continuation_context(
                        &messages, continuation, self.config.max_continuations,
                        continuation_history.clone(),
                    );
                    // Record this round before compressing messages
                    continuation_history.push(ContinuationRecord {
                        round: continuation,
                        approach: ctx.approach_description.clone(),
                        effective_tools: ctx.effective_patterns.iter()
                            .map(|p| p.description.clone()).collect(),
                        ineffective_tools: ctx.ineffective_patterns.iter()
                            .map(|p| p.description.clone()).collect(),
                        key_findings: ctx.key_findings.clone(),
                        lesson: ctx.lesson.clone(),
                    });
                    // Keep only the last 3 records to bound memory
                    if continuation_history.len() > 3 {
                        continuation_history.remove(0);
                    }
                    let summary = ctx.render();
                    let original_msg_count = messages.len();
                    messages = vec![crate::react::ChatMessage::system(summary)];
                    // Publish history_compressed
                    if let Some(ref sink) = self.event_sink {
                        sink(kernel::event::Event::new(
                            "cognitive-engine",
                            kernel::event::EventType::Custom("agent:history_compressed".into()),
                            serde_json::json!({ "agent_id": &agent_id, "session_id": &session_id, "messages_before": original_msg_count, "messages_after": 1 }),
                        ));
                    }
                    if let Some(ref sink) = self.event_sink {
                        sink(kernel::event::Event::new(
                            "cognitive-engine",
                            kernel::event::EventType::Custom("agent:auto_continue".into()),
                            serde_json::json!({ "agent_id": &agent_id, "session_id": &session_id, "continuation": continuation }),
                        ));
                    }
                    continue;
                }
                return Err(CognitiveError::MaxDepthReached { depth: turn });
            }

            // ── Interrupt check (before LLM call) ──────────────────
            // Returns Err(Interrupted) so the harness error path can react.
            // NOTE: we do NOT publish agent:reply_interrupted here — that
            // event goes to the local bus (via event_sink) which has no
            // subscribers that drive the session state machine. The harness
            // error path publishes the event on the global bus instead.
            if self.interrupt_flag.as_ref().map_or(false, |f| f.is_interrupted()) {
                return Err(CognitiveError::Interrupted);
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
                // Empty response with no tool calls is effectively a failure —
                // the LLM produced nothing actionable.  Retry it like a
                // transient error so we don't exit the ReAct loop with an
                // empty final_content (which would silently hang the agent).
                let is_empty_reply = r.as_ref().is_ok_and(|resp| {
                    resp.content.is_empty() && resp.tool_calls.is_empty()
                });
                if is_empty_reply && llm_attempt < max_retries {
                    let delay = (3_u64.pow(llm_attempt - 1)).min(120);
                    tracing::warn!(%agent_id, %session_id, turn, attempt=llm_attempt, delay, "LLM returned empty response — retrying (cognitive engine)");
                    tokio::time::sleep(std::time::Duration::from_secs(delay)).await;
                    continue;
                }
                if r.is_ok() || llm_attempt >= max_retries
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

            // Publish llm_error if the call failed after all retries
            if let Err(ref err_msg) = response
                && let Some(ref sink) = self.event_sink
            {
                sink(kernel::event::Event::new(
                    "cognitive-engine",
                    kernel::event::EventType::Custom("llm_error".into()),
                    serde_json::json!({ "agent_id": &agent_id, "session_id": &session_id, "turn": turn, "error": err_msg }),
                ));
            }

            let response = response.map_err(|e| CognitiveError::EngineError {
                engine_name: self.name().to_owned(), message: e,
            })?;

            // If the LLM returned empty content with no tool calls (even
            // after retries), treat it as an engine error rather than
            // silently completing with an empty reply.
            if response.content.is_empty() && response.tool_calls.is_empty() {
                return Err(CognitiveError::EngineError {
                    engine_name: self.name().to_owned(),
                    message: "LLM returned empty response with no tool calls".into(),
                });
            }

            // Output validation
            let content = {
                let mut v = kernel::validator::OutputValidator::new();
                match v.validate(&response.content, kernel::types::TrustLevel::Untrusted) {
                    kernel::validator::ValidationOutcome::Pass => response.content,
                    kernel::validator::ValidationOutcome::Fail { matched_rules, reason } => {
                        tracing::warn!(%agent_id, %session_id, turn, %reason, rules=?matched_rules, "output_validator blocked LLM output");
                        if let Some(ref sink) = self.event_sink {
                            sink(kernel::event::Event::new(
                                "cognitive-engine",
                                kernel::event::EventType::Custom("agent:reply_stream_error".into()),
                                serde_json::json!({
                                    "agent_id": &agent_id, "session_id": &session_id,
                                    "error": format!("output_validator blocked: {reason}"),
                                    "matched_rules": matched_rules,
                                }),
                            ));
                        }
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

            // Token event — report real usage from the provider when available,
            // falling back to the byte-length heuristic for providers/modes
            // that don't surface usage data (some local models, older streaming).
            //
            // Read the value *now* (before `response` is partially moved below)
            // and keep it as a plain u64 so we don't hold a borrow across the
            // response.tool_calls move.
            let tokens = match response.usage {
                Some(ref u) => u.total_tokens,
                None => {
                    tracing::debug!(
                        agent_id = %agent_id,
                        session_id = %session_id,
                        turn,
                        "agent:token_used falling back to byte heuristic (no provider usage)"
                    );
                    (content.len() / 4) as u64
                }
            };
            if let Some(ref sink) = self.event_sink {
                sink(kernel::event::Event::new(
                    "cognitive-engine",
                    kernel::event::EventType::Custom("agent:token_used".into()),
                    serde_json::json!({ "agent_id": &agent_id, "session_id": &session_id, "turn": turn, "tokens": tokens }),
                ));
            }

            if response.tool_calls.is_empty() {
                // Finished
                final_content = content;
                break;
            }

            // Publish got_tool_calls before execution.  The payload carries
            // full detail (id, name, args) so that a later "继续" (continue)
            // can faithfully reconstruct the assistant+tool_calls ChatMessage
            // from the persisted JSONL — not just the tool names.
            if let Some(ref sink) = self.event_sink {
                let tools: Vec<serde_json::Value> = response.tool_calls.iter().map(|c| {
                    serde_json::json!({
                        "id": c.id,
                        "tool_name": c.tool_name,
                        "args": serde_json::to_string(&c.args).unwrap_or_default()
                    })
                }).collect();
                sink(kernel::event::Event::new(
                    "cognitive-engine",
                    kernel::event::EventType::Custom("agent:got_tool_calls".into()),
                    serde_json::json!({ "agent_id": &agent_id, "session_id": &session_id, "turn": turn, "tools": tools }),
                ));
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
            let results = self.execute_tool_calls(&parsed_calls, &session_id, &agent_id).await;
            for r in &results {
                messages.push(ChatMessage::tool_result(&r.id, &r.tool_name, &r.output));
            }

            // Publish tool_results_fed_back
            if let Some(ref sink) = self.event_sink {
                let success_count = results.iter().filter(|r| r.success).count();
                sink(kernel::event::Event::new(
                    "cognitive-engine",
                    kernel::event::EventType::Custom("agent:tool_results_fed_back".into()),
                    serde_json::json!({ "agent_id": &agent_id, "session_id": &session_id, "turn": turn, "total": results.len(), "success": success_count }),
                ));
            }

            // ── Interrupt check (after tool execution) ──────────────
            // Same note as the pre-LLM check: the harness error path owns
            // the agent:reply_interrupted publish on the global bus.
            if self.interrupt_flag.as_ref().map_or(false, |f| f.is_interrupted()) {
                return Err(CognitiveError::Interrupted);
            }

            // ── Format reminder (once per session, after N turns with tool use) ─
            if self.config.format_reminder_turns > 0
                && turn >= self.config.format_reminder_turns
                && !format_reminder_fired
            {
                format_reminder_fired = true;
                messages.push(crate::react::ChatMessage::system(
                    "[Format Reminder] You are working on a complex task with multiple tool calls. \
                     Ensure your final response is complete — fill ALL sections, cover ALL dimensions, \
                     and provide a thorough, well-structured answer. Do not truncate."
                ));
                tracing::info!(%agent_id, %session_id, turn, "injected format reminder");
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
