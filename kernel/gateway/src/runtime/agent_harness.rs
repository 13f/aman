// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use event_bus::EventBus;
use kernel::agent::{AgentInstance, AgentStatus, AgentSystemState};
use kernel::content_filter::ContentFilter;
use kernel::interrupt::InterruptFlag;
use context_manager::{
    CompressionStrategy, HistoryCompressor, TokenBudget, TokenBudgetPolicy,
};
use cognitive_engine::{CognitiveContext, CognitiveEngine as _, CognitiveIdentity, Observation};
use cognitive_llm::LlmCognitiveEngine;
use cognitive_react as _;
use kernel::event::{Event, EventType};
use kernel::llm::LlmProvider;
use kernel::react::{
    ChatMessage, ChatMessageRole,
    SoulSnapshot, ToolDescriptor,
};
use kernel::router::AgentRouter;
use kernel::session_history::SessionHistoryStore;
use kernel::{AmanResult, Error};
use serde_json::json;
use tool::ToolRegistry;
use tool::ToolSecurityConfig;

use super::event_consts::{
    SOURCE_AGENT_HARNESS, EVT_AGENT_BUSY,
    EVT_AGENT_DIRECT_ACT_STARTED, EVT_AGENT_IDLE,
    EVT_AGENT_REPLY_INTERRUPTED, EVT_AGENT_REPLY_READY,
    EVT_AGENT_REPLY_STREAM_ERROR, EVT_AGENT_CONFIG_WARNING,
};
use super::AgentRegistry;

/// Default maximum ReAct loop iterations.
const DEFAULT_MAX_REACT_TURNS: u32 = 64;

/// Hard timeout for a single `engine.process()` call.
///
/// Must be **strictly less** than the `PROCESSING` timeout declared by the
/// `message-session` workflow (120s, see `session/mod.rs`).  When the engine
/// exceeds this budget we treat the call as hung, trigger its interrupt flag
/// and return an error so the harness's normal error path runs — rather than
/// letting the (potentially 600s) streaming-client timeout be the only guard.
/// Keeping it under the workflow timeout is what guarantees the session is
/// still in `PROCESSING` when the error path's `LLM_REPLY_READY` fires, so
/// the session transitions cleanly back to `IDLE` (see Bug 2).
const ENGINE_PROCESS_TIMEOUT_SECS: u64 = 90;

/// Default agent router — selects the first enabled agent.
pub struct FirstEnabledAgentRouter;

#[async_trait::async_trait]
impl AgentRouter for FirstEnabledAgentRouter {
    async fn route(&self, _user_text: &str, agents: &[AgentInstance]) -> Option<AgentInstance> {
        agents.iter().find(|a| a.descriptor.enabled).cloned()
    }
}

/// Outcome of the ReAct loop.
#[derive(Debug)]
#[allow(dead_code)]
pub enum ReactOutcome {
    /// Normal completion with the final reply text.
    Finished(String),
    /// Loop was interrupted (user /stop), partial content if any.
    Interrupted(String),
    /// Max turns reached — session should be saved and be resumable.
    MaxTurnsReached { turns: u32 },
    /// Turn 1 completed and a tool spawned a detached process.
    /// The harness must run Turn 2 when the process exits.
    AwaitingDetach {
        session_id: String,
        pid: u32,
        tool_call_id: String,
    },
}

/// Distinguishes "继续" (continue) from "恢复" (replay) — two fundamentally
/// different session-resumption paths.
///
/// ## Continue ("继续")
/// User clicks "继续" after `MaxTurnsReached`, or sends `/continue`.
/// The agent compresses the raw session history into a structured summary
/// (goals, progress, key findings, tool usage stats) and sends that
/// compressed context to the LLM.  It does **not** replay events to the
/// EventBus, and it does **not** dump raw tool outputs into the prompt.
///
/// ## Replay ("恢复")
/// Used only after gateway restart or explicit session-restore.  The full
/// conversation history is faithfully reconstructed from the JSONL event
/// log via [`restore_session_history`] so the agent can pick up exactly
/// where it left off.  This is the expensive path — it preserves every
/// tool call/result pair.
///
/// ## Fresh
/// Normal first message or mid-conversation message.  Append to existing
/// history as-is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContinuationMode {
    /// Normal fresh message — append to history.
    Fresh,
    /// User clicked "继续" — compress history into structured summary.
    Continue,
    /// Gateway-restart recovery — faithful full-history reconstruction.
    Replay,
}

/// Handle to a running anonymous agent spawned via [`AgentHarness::spawn_anonymous`].
///
/// The anonymous agent runs in a background task with its own ReAct loop.
/// Call [`AnonymousAgentHandle::wait`] to await its final reply.
///
/// Dropping the handle does **not** cancel the agent — it runs to completion
/// independently. The result is simply discarded if not awaited.
pub struct AnonymousAgentHandle {
    /// Unique identifier for the anonymous agent (format: `anon-{uuid}`).
    pub agent_id: String,
    /// Session identifier for this execution.
    pub session_id: String,
    result_rx: tokio::sync::oneshot::Receiver<AmanResult<String>>,
}

impl AnonymousAgentHandle {
    /// Wait for the anonymous agent to complete and return its final reply.
    ///
    /// Returns an error if the agent task panicked or was cancelled.
    #[allow(clippy::missing_errors_doc)]
    pub async fn wait(self) -> AmanResult<String> {
        self.result_rx
            .await
            .map_err(|_| Error::ConfigInvalid {
                message: "anonymous agent task cancelled".to_owned(),
            })?
    }
}

/// Agent Harness — orchestrates the ReAct loop for a single agent.
///
/// One harness instance processes one message at a time through
/// the think-act-observe iteration, coordinating context assembly,
/// LLM calls, tool execution, and event publishing.
pub struct AgentHarness {
    registry: Arc<AgentRegistry>,
    tool_registry: Arc<ToolRegistry>,
    bus: Arc<dyn EventBus>,
    /// Per-session conversation history for cross-turn continuity.
    session_history: Box<dyn SessionHistoryStore>,
    /// Per-session interrupt flags for external stop (M6).
    active_interrupts: RwLock<HashMap<String, Arc<InterruptFlag>>>,
    /// Per-session task abort handles for shutdown force-cancel (M6).
    active_tasks: RwLock<HashMap<String, tokio::task::AbortHandle>>,
    /// Default max ReAct turns.
    max_react_turns: u32,
    /// Pluggable token budget policy.
    budget_policy: Box<dyn TokenBudgetPolicy>,
    /// Pluggable agent routing strategy.
    agent_router: Box<dyn AgentRouter>,
    /// Compression configuration (retained for future use).
    #[allow(dead_code)]
    compression_config: context_manager::CompressorConfig,
    /// Stream forwarder capacity (retained for future use).
    #[allow(dead_code)]
    stream_forwarder_capacity: usize,
    /// Handle to the main tokio runtime, used to spawn tasks from any thread
    /// (including non-tokio threads like the plugin bridge).
    runtime: tokio::runtime::Handle,
}

impl AgentHarness {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        registry: Arc<AgentRegistry>,
        tool_registry: Arc<ToolRegistry>,
        bus: Arc<dyn EventBus>,
        session_history: Box<dyn SessionHistoryStore>,
        budget_policy: Box<dyn TokenBudgetPolicy>,
        agent_router: Box<dyn AgentRouter>,
        compression_config: context_manager::CompressorConfig,
        _tool_timeout_ms: u64,
        stream_forwarder_capacity: usize,
        _security_config: Option<ToolSecurityConfig>,
        runtime: tokio::runtime::Handle,
    ) -> Self {
        Self {
            registry,
            tool_registry,
            bus,
            session_history,
            active_interrupts: RwLock::new(HashMap::new()),
            active_tasks: RwLock::new(HashMap::new()),
            max_react_turns: DEFAULT_MAX_REACT_TURNS,
            budget_policy,
            agent_router,
            compression_config,
            stream_forwarder_capacity,
            runtime,
        }
    }

    /// Test constructor — builds a minimal `AgentHarness` with an in-memory
    /// session history, a no-op tool registry, and a stub event bus.  Only
    /// useful for unit tests that exercise history / filtering logic without
    /// needing a live LLM backend.
    #[cfg(test)]
    fn new_test() -> Self {
        use event_bus::InMemoryBus;
        use kernel::session_history::InMemorySessionHistory;
        Self::new(
            Arc::new(AgentRegistry::new(Arc::new(InMemoryBus::new(Default::default())))),
            Arc::new(ToolRegistry::new()),
            Arc::new(InMemoryBus::new(Default::default())),
            Box::new(InMemorySessionHistory::with_max_messages(100)),
            Box::new(context_manager::DefaultTokenBudgetPolicy::new()),
            Box::new(FirstEnabledAgentRouter),
            context_manager::CompressorConfig::default(),
            30_000,
            256,
            None,
            tokio::runtime::Handle::current(),
        )
    }

    async fn build_cognitive_engine(
        &self, agent_id: &str, model: &str, session_id: &str, background: bool,
        interrupt_flag: Option<Arc<InterruptFlag>>,
        token_budget: &context_manager::TokenBudget,
    ) -> AmanResult<LlmCognitiveEngine> {
        let kernel_provider = self.registry.get_llm_provider(agent_id).await
            .ok_or_else(|| Error::ConfigInvalid { message: format!("no LLM provider for agent '{agent_id}'") })?;
        // Adapt kernel::llm::LlmProvider → cognitive_llm::provider::LlmProvider
        struct KernelProviderAdapter(Arc<dyn LlmProvider>);
        #[async_trait::async_trait]
        impl cognitive_llm::provider::LlmProvider for KernelProviderAdapter {
            fn name(&self) -> &str { self.0.name() }
            fn base_url(&self) -> &str { self.0.base_url() }
            async fn chat_completion(&self, req: cognitive_llm::provider::LlmChatRequest, cb: Option<Arc<dyn Fn(cognitive_llm::provider::StreamEvent) + Send + Sync>>) -> Result<cognitive_llm::provider::LlmResponse, String> {
                let kr = kernel::llm::LlmChatRequest { model: req.model, system_prompt: req.system_prompt, messages: req.messages.into_iter().map(|m| kernel::react::ChatMessage { role: match m.role { cognitive_react::ChatMessageRole::System => kernel::react::ChatMessageRole::System, cognitive_react::ChatMessageRole::User => kernel::react::ChatMessageRole::User, cognitive_react::ChatMessageRole::Assistant => kernel::react::ChatMessageRole::Assistant, cognitive_react::ChatMessageRole::Tool => kernel::react::ChatMessageRole::Tool }, content: m.content, tool_call_id: m.tool_call_id, tool_name: m.tool_name, tool_calls: m.tool_calls, reasoning_content: m.reasoning_content }).collect(), tools: req.tools.into_iter().map(|t| kernel::react::ToolDescriptor { name: t.name, description: t.description, parameters: t.parameters }).collect(), max_output_tokens: req.max_output_tokens, response_format: req.response_format.map(|f| match f { cognitive_llm::provider::ResponseFormat::JsonObject => kernel::llm::ResponseFormat::JsonObject, cognitive_llm::provider::ResponseFormat::JsonSchema { name, schema, strict } => kernel::llm::ResponseFormat::JsonSchema { name, schema, strict } }) };
                let kcb = cb.map(|c| { let c2 = c; Arc::new(move |e: kernel::llm::StreamEvent| c2(match e { kernel::llm::StreamEvent::Start => cognitive_llm::provider::StreamEvent::Start, kernel::llm::StreamEvent::Chunk(s) => cognitive_llm::provider::StreamEvent::Chunk(s), kernel::llm::StreamEvent::Done { finish_reason } => cognitive_llm::provider::StreamEvent::Done { finish_reason }, kernel::llm::StreamEvent::Error(s) => cognitive_llm::provider::StreamEvent::Error(s), })) as Arc<dyn Fn(cognitive_llm::provider::StreamEvent) + Send + Sync> });
                self.0.chat_completion(kr, kcb).await.map(|r| cognitive_llm::provider::LlmResponse { content: r.content, finish_reason: r.finish_reason, tool_calls: r.tool_calls.into_iter().map(|c| cognitive_react::ParsedToolCall { id: c.id, tool_name: c.tool_name, args: c.args }).collect(), reasoning_content: r.reasoning_content, usage: r.usage.map(|u| cognitive_llm::provider::TokenUsage { prompt_tokens: u.prompt_tokens, completion_tokens: u.completion_tokens, total_tokens: u.total_tokens }) }).map_err(|e| e.to_string())
            }
        }
        let provider: Arc<dyn cognitive_llm::provider::LlmProvider> = Arc::new(KernelProviderAdapter(kernel_provider));
        let bus: Arc<dyn EventBus> = self.registry.get_local_bus(agent_id).await.unwrap_or_else(|| Arc::clone(&self.bus));
        let eb = Arc::clone(&bus);
        let sink: Arc<dyn Fn(Event) + Send + Sync> = Arc::new(move |e| { let b = Arc::clone(&eb); tokio::spawn(async move { let _ = b.publish(e).await; }); });
        let engine = cognitive_llm::LlmCognitiveEngine::new(provider, cognitive_llm::LlmEngineConfig {
            model: model.into(), max_turns: self.max_react_turns,
            token_limit: token_budget.context_window as u64,
            max_output_tokens: token_budget.max_output_tokens as u64,
            max_llm_retries: 5, background, max_continuations: 5,
            format_reminder_turns: 0,
        }).with_event_sink(sink).with_tool_executor(Arc::clone(&self.tool_registry), bus, 30_000);
        // Wire interrupt flag if provided
        let mut engine = engine;
        if let Some(flag) = interrupt_flag {
            engine = engine.with_interrupt_flag(flag);
        }
        // Wire a consciousness provider so the engine self-gates when the
        // backend is Catatonic/Coma. The harness-level guard (further below)
        // remains as a first-pass shortcut that avoids even building the
        // engine under full downtime.
        if let Some(machine) = self.registry.get_cognitive_state_machine(agent_id).await {
            struct GatewayConsciousness(std::sync::Arc<super::CognitiveStateMachine>);
            impl cognitive_engine::ConsciousnessProvider for GatewayConsciousness {
                fn state(&self) -> cognitive_engine::CognitiveState {
                    self.0.state()
                }
            }
            engine = engine.with_consciousness_provider(Arc::new(GatewayConsciousness(machine)));
        }
        let lb: Arc<dyn EventBus> = self.registry.get_local_bus(agent_id).await.unwrap_or_else(|| Arc::clone(&self.bus));
        // Use an mpsc channel so streaming chunks are published in
        // order.  tokio::spawn does NOT guarantee FIFO scheduling
        // across threads, which would scramble CJK text.
        let aid = agent_id.to_owned();
        let sid = session_id.to_owned();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Event>();
        let lb2 = Arc::clone(&lb);
        tokio::spawn(async move {
            while let Some(event) = rx.recv().await {
                let _ = lb2.publish(event).await;
            }
        });
        struct SB {
            tx: tokio::sync::mpsc::UnboundedSender<Event>,
            aid: String,
            sid: String,
        }
        impl cognitive_engine::CognitiveListener for SB {
            fn on_cognitive_event(&self, e: cognitive_engine::CognitiveEvent) {
                let (et, pl) = match e {
                    cognitive_engine::CognitiveEvent::StreamStart { .. } => ("agent:reply_stream_start", json!({})),
                    cognitive_engine::CognitiveEvent::TextChunk { text, .. } => ("agent:reply_chunk", json!({"delta": text})),
                    cognitive_engine::CognitiveEvent::StreamDone { finish_reason, .. } => ("agent:reply_stream_done", json!({"finish_reason": finish_reason})),
                    cognitive_engine::CognitiveEvent::StreamError { error, .. } => ("agent:reply_stream_error", json!({"error": error})),
                    _ => return,
                };
                let _ = self.tx.send(Event::new(SOURCE_AGENT_HARNESS, EventType::Custom(et.into()), json!({"agent_id": &self.aid, "session_id": &self.sid, "extra": pl})));
            }
        }
        engine.subscribe(Arc::new(SB { tx, aid, sid }));
        Ok(engine)
    }

    #[allow(clippy::too_many_arguments)] // Mirrors `process_message` args + skill_name for DailyLife tracking.
    pub async fn process_message_v2(
        self: &Arc<Self>, agent_id: &str, session_id: &str, user_text: &str,
        model: &str, soul_snapshot: SoulSnapshot, skill_name: Option<&str>,
        background: bool, continuation_mode: ContinuationMode,
    ) -> AmanResult<String> {
        let inst = self.prepare_agent_session(agent_id, session_id, background).await?;
        if let Some(c) = self.registry.get_idle_coordination(agent_id).await { c.reset_idle_signal().await; c.arousal.boost(0.3); }
        let tools = self.build_tool_descriptors(agent_id).await;
        // Get past conversation history (without current message — it will be
        // passed as an Observation below, avoiding duplication).
        let mut past_history = self.session_history.get(session_id);

        // Continue ("继续") mode: the user is picking up a prior task.  The
        // in-memory session_history only ever holds user + final-assistant
        // messages — it drops the mid-turn tool calls and tool results that
        // the ReAct loop produced.  To give the LLM genuine continuity (not
        // just a vague summary), we *always* rebuild the full transcript
        // from the persisted JSONL, which now preserves the enriched
        // `agent:got_tool_calls` and `tool:completed` events as first-class
        // assistant+tool_calls and Tool-role messages.
        //
        // The rebuilt history is then passed to the LLM verbatim (filtered
        // only to strip system noise), so the agent sees exactly what it
        // was doing — every tool it called and every result it got back.
        //
        // `build_continuation_context` is retained as a *fallback* for the
        // rare case where JSONL restoration yields nothing (e.g. brand-new
        // session with no persisted events yet).
        //
        // We deliberately do this BEFORE the content-filter / consciousness
        // checks so the "继续" intent is recognised even when past_history
        // would otherwise be empty.
        let continuation_history = if continuation_mode == ContinuationMode::Continue {
            self.restore_session_history_from_jsonl(session_id, agent_id, true).await;
            past_history = self.session_history.get(session_id);
            if !past_history.is_empty() {
                // Full-fidelity path: hand the LLM the complete ReAct transcript.
                Some(filter_conversation_history_with_tools(&past_history))
            } else {
                // Fallback: no persisted events — nothing to restore.
                None
            }
        } else {
            None
        };

        let tb = self.init_token_budget(agent_id, session_id, model, &inst, &soul_snapshot, &past_history, &tools).await;
        let mem = self.retrieve_relevant_memories(agent_id, user_text).await;

        // ── Input content filter: block sensitive data before it reaches the LLM ──
        let cf = ContentFilter::new();
        match cf.filter(user_text) {
            kernel::content_filter::FilterDecision::Block { reason, matched_rules } => {
                tracing::warn!(%agent_id, %session_id, %reason, rules=?matched_rules, "content_filter blocked user input");
                let msg = format!("Your message contains sensitive data that cannot be processed: {reason}");
                // Still need to clean up — unregister interrupt that hasn't been created yet
                return Ok(msg);
            }
            kernel::content_filter::FilterDecision::Flag { matched_rules } => {
                tracing::info!(%agent_id, %session_id, rules=?matched_rules, "content_filter flagged user input (allowed)");
            }
            _ => {}
        }

        let flag = Arc::new(InterruptFlag::new()); self.register_interrupt(session_id, Arc::clone(&flag));
        let engine = self.build_cognitive_engine(agent_id, model, session_id, background, Some(Arc::clone(&flag)), &tb).await?;

        // Compute grounding — how well-informed the agent is for this task
        let grounding = self.compute_grounding(agent_id, user_text, &mem, &tb).await;

        // Build the conversation history that the LLM sees:
        // - Continue ("继续") mode — the full reconstructed ReAct transcript
        //   (user messages, assistant+tool_calls, tool results, final replies)
        //   so the agent can pick up exactly where it left off.
        // - Fresh / Replay — the standard user↔assistant dialogue strip.
        let conversation_history = match continuation_history {
            Some(ref history) => history.clone(),
            None => filter_conversation_history(&past_history),
        };
        let ctx = CognitiveContext {
            agent_id: agent_id.into(), session_id: session_id.into(),
            identity: CognitiveIdentity { name: soul_snapshot.name.clone(), identity: soul_snapshot.system_prompt.clone(), boundaries: soul_snapshot.boundaries.clone(), expertise: vec![], vibe: None, raw: soul_snapshot.system_prompt.clone() },
            capabilities: tools.iter().map(|t| cognitive_engine::Capability { name: t.name.clone(), description: t.description.clone(), parameters: t.parameters.clone(), cap_type: cognitive_engine::CapabilityType::Tool }).collect(),
            memory_context: mem.map(|m| vec![cognitive_engine::MemoryItem { key: "retrieved".into(), content: m, importance: 0.5, timestamp: None }]).unwrap_or_default(),
            engine_config: json!({"model": model}),
            conversation_history,
            grounding,
        };
        // ── Consciousness check: skip processing if LLM is unavailable ──
        let consciousness = self.registry.get_cognitive_state(agent_id).await
            .unwrap_or(crate::runtime::CognitiveState::Lucid);
        if let Some(msg) = consciousness.guard_check() {
            tracing::warn!(%agent_id, %session_id, state = ?consciousness, "cognitive state prevents processing: {}", msg);
            return Ok(msg.to_string());
        }

        let obs = vec![Observation::user_message(uuid::Uuid::now_v7().to_string(), session_id, user_text)];
        tracing::info!(%agent_id, %session_id, "process_message_v2: calling engine.process()");
        // Bound the engine call so a hung LLM provider (no stream timeout of
        // its own for the first byte) cannot block this task indefinitely.
        // On timeout we trigger the interrupt flag — the engine checks it
        // between ReAct turns — and surface an error so the harness error
        // path below drives the session back to IDLE.
        let result = match tokio::time::timeout(
            std::time::Duration::from_secs(ENGINE_PROCESS_TIMEOUT_SECS),
            engine.process(&ctx, obs),
        )
        .await
        {
            Ok(r) => r,
            Err(_) => {
                tracing::warn!(
                    %agent_id,
                    %session_id,
                    limit = ENGINE_PROCESS_TIMEOUT_SECS,
                    "process_message_v2: engine.process() timed out — triggering interrupt"
                );
                self.interrupt_session(session_id);
                Err(cognitive_engine::CognitiveError::EngineError {
                    engine_name: "LlmCognitiveEngine".into(),
                    message: format!(
                        "engine.process() timed out after {ENGINE_PROCESS_TIMEOUT_SECS}s"
                    ),
                })
            }
        };
        tracing::info!(%agent_id, %session_id, success = result.is_ok(), "process_message_v2: engine.process() completed");

        // Report LLM call result to BackendHealth for cognitive state tracking.
        if let Some(health) = self.registry.get_agent_backend_health(agent_id).await {
            let config = self.registry.backend_health_registry().config();
            let changed = if result.is_ok() {
                health.record_success(config)
            } else {
                let err_msg = result.as_ref().err().map(|e| e.to_string()).unwrap_or_default();
                health.record_failure(&err_msg, config)
            };
            if let Some(ev) = changed {
                self.publish_backend_health_event(ev);
            }
        }

        // ── Error path: full state cleanup (matches old process_message) ──
        if result.is_err() {
            let err_msg = result.as_ref().err().map(|e| e.to_string()).unwrap_or_default();
            // Publish error event so frontend knows something went wrong
            let _ = self.bus.publish(Event::new(
                SOURCE_AGENT_HARNESS,
                EventType::Custom(EVT_AGENT_REPLY_STREAM_ERROR.to_owned()),
                serde_json::json!({
                    "agent_id": agent_id,
                    "session_id": session_id,
                    "error": err_msg,
                }),
            )).await;
            // Transition the session workflow engine back to IDLE so the
            // session doesn't stay stuck in PROCESSING forever. The
            // SessionReplyHandler subscribes to reply_ready / reply_interrupted
            // and drives session_manager.handle_reply() (PROCESSING → IDLE).
            let _ = self.bus.publish(Event::new(
                SOURCE_AGENT_HARNESS,
                EventType::Custom(EVT_AGENT_REPLY_INTERRUPTED.to_owned()),
                serde_json::json!({
                    "agent_id": agent_id,
                    "session_id": session_id,
                    "error": err_msg,
                }),
            )).await;
            self.unregister_interrupt(session_id);
            let _ = self.registry.set_active_session(agent_id, None).await;
            let _ = self.registry.set_status(agent_id, AgentStatus::Idle).await;
            if background && skill_name.is_some() {
                // Background idle_run (boredom) failed — the exec returned in
                // ~1ms so the agent is logically idle even though a detached
                // script may still be running. Set system_state to Ready so
                // the UI shows the agent as available. The idle system will
                // start only when the agent window loses focus (UI-driven).
                let _ = self.registry.set_system_state(agent_id, AgentSystemState::Ready).await;
                let _ = self.registry.set_activity(agent_id, skill_name.unwrap_or("")).await;
            } else {
                let _ = self.registry.set_system_state(agent_id, AgentSystemState::Ready).await;
                let _ = self.registry.set_activity(agent_id, "").await;
            }
            self.session_history.clear(session_id);
        }

        let raw_reply = result.map_err(|e| Error::ConfigInvalid { message: format!("cognitive engine: {e}") })?
            .iter().find_map(|d| match &d.kind { cognitive_engine::DecisionKind::Reply { text, is_final: true } => Some(text.clone()), _ => None }).unwrap_or_else(|| "[no reply]".into());

        // ── Post-processing: [remember:] extraction + API key sanitization ──
        let (cleaned, remembered) = process_remember_commands(&raw_reply);
        for content in &remembered {
            if let Some(provider) = self.registry.get_memory_provider(agent_id).await {
                let _ = provider.store(agent_id, content, vec!["auto".to_owned()]);
            }
        }
        let reply = kernel::redactor::redact_sensitive_data(&cleaned).into_owned();

        // A2A sessions are handled by AgentMessageHandler — don't persist
        // to agent session stores or publish user-visible events.
        let is_a2a = session_id.starts_with("a2a:");

        if !is_a2a {
            // ── Publish reply_ready event so downstream (desktop, channels) can react ──
            let _ = self.bus.publish(Event::new(
                SOURCE_AGENT_HARNESS,
                EventType::Custom(EVT_AGENT_REPLY_READY.to_owned()),
                serde_json::json!({
                    "agent_id": agent_id,
                    "session_id": session_id,
                    "reply": reply,
                    "background": background,
                }),
            )).await;
        }

        // ── Persist history + cleanup ──
        // Persist current exchange to session history
        self.session_history.append(session_id, ChatMessage::user(user_text));
        self.session_history.append(session_id, ChatMessage::assistant(&reply));
        self.unregister_interrupt(session_id);
        let _ = self.registry.set_active_session(agent_id, None).await;
        if background && skill_name.is_some() {
            // Background idle_run (boredom): the exec(detached) returned in
            // ~1ms so the agent is logically idle even though a detached
            // script may still be running in the background. Set
            // system_state to Ready so the UI shows the agent as available.
            // The idle system will start only when the agent window loses
            // focus (UI-driven). Keep the activity text so the UI still
            // shows what skill was triggered.
            let _ = self.registry.set_status(agent_id, AgentStatus::Idle).await;
            let _ = self.registry.set_system_state(agent_id, AgentSystemState::Ready).await;
            let _ = self.registry.set_activity(agent_id, skill_name.unwrap_or("")).await;
        } else {
            // Session complete → Ready. The idle system will start only when
            // the agent window loses focus (UI-driven).
            let _ = self.registry.set_status(agent_id, AgentStatus::Idle).await;
            let _ = self.registry.set_system_state(agent_id, AgentSystemState::Ready).await;
            let _ = self.registry.set_activity(agent_id, "").await;
        }
        if !is_a2a {
            // Publish agent:idle to the agent's local bus
            self.try_publish_to_agent_bus(agent_id, Event::new(
                SOURCE_AGENT_HARNESS,
                EventType::Custom(EVT_AGENT_IDLE.to_owned()),
                serde_json::json!({ "agent_id": agent_id, "session_id": session_id }),
            )).await;
        }
        Ok(reply)
    }

    /// Publish an event to the agent's local bus, falling back to the global bus.
    async fn publish_to_agent_bus(
        &self,
        agent_id: &str,
        event: Event,
    ) -> AmanResult<()> {
        match self.registry.get_local_bus(agent_id).await {
            Some(local_bus) => local_bus.publish(event).await,
            None => self.bus.publish(event).await,
        }
    }

    /// Like [`Self::publish_to_agent_bus`] but logs the error via
    /// `tracing::warn!` and returns `()` instead of `AmanResult<()>`.
    ///
    /// Use this at the call sites that previously had
    /// `let _ = self.publish_to_agent_bus(...).await;` — the
    /// doc's "silent data-loss" smell is that an event-bus
    /// publish failure (queue full, serialization, etc.) was
    /// happening without any operator-visible signal. The warn
    /// surfaces the failure in the diagnostic log without changing
    /// the call-site type signature.
    async fn try_publish_to_agent_bus(&self, agent_id: &str, event: Event) {
        if let Err(e) = self.publish_to_agent_bus(agent_id, event).await {
            tracing::warn!(
                agent_id,
                error = %e,
                "publish_to_agent_bus failed; event dropped"
            );
        }
    }

    /// Register an interrupt flag for an active session (M6).
    pub fn register_interrupt(&self, session_id: &str, flag: Arc<InterruptFlag>) {
        self.active_interrupts
            .write()
            .expect("interrupt lock")
            .insert(session_id.to_owned(), flag);
    }

    /// Unregister an interrupt flag when processing completes (M6).
    pub fn unregister_interrupt(&self, session_id: &str) {
        self.active_interrupts
            .write()
            .expect("interrupt lock")
            .remove(session_id);
    }

    /// Interrupt an active session by session_id (M6).
    ///
    /// Called from an event bus subscriber when a `STOP_GENERATION` event arrives.
    pub fn interrupt_session(&self, session_id: &str) {
        if let Some(flag) = self
            .active_interrupts
            .read()
            .expect("interrupt lock")
            .get(session_id)
        {
            flag.interrupt();
        }
    }

    /// Remove a completed task handle from the active-tasks map.
    fn remove_task(&self, session_id: &str) {
        self.active_tasks
            .write()
            .expect("active_tasks lock")
            .remove(session_id);
    }

    /// Force-abort a running session's tokio task by session_id.
    ///
    /// Returns `true` if a task was found and aborted. The task future is
    /// dropped immediately, interrupting even a mid-stream LLM HTTP call.
    /// The caller is responsible for resetting agent state (status, system
    /// state, activity) after aborting.
    pub fn abort_task(&self, session_id: &str) -> bool {
        let mut tasks = self.active_tasks.write().expect("active_tasks lock");
        if let Some(handle) = tasks.remove(session_id) {
            handle.abort();
            tracing::info!(%session_id, "aborted agent task");
            true
        } else {
            false
        }
    }

    /// Publish a `agent:reply_stream_error` event to the agent's local bus.
    ///
    /// Called by the PROCESSING-timeout poller after `abort_task()` cancels a
    /// hung task — the task's own error path never runs, so without this the
    /// frontend would see the agent return to IDLE with no reply and no
    /// error indication.
    pub async fn publish_timeout_error(&self, agent_id: &str, session_id: &str) {
        let _ = self.bus.publish(Event::new(
            SOURCE_AGENT_HARNESS,
            EventType::Custom(EVT_AGENT_REPLY_STREAM_ERROR.to_owned()),
            json!({
                "agent_id": agent_id,
                "session_id": session_id,
                "error": "Request timed out after 120s — the LLM provider did not respond in time",
            }),
        )).await;
    }

    /// List session_ids of currently running tasks.
    pub fn active_task_ids(&self) -> Vec<String> {
        self.active_tasks.read().expect("active_tasks lock").keys().cloned().collect()
    }

    /// Interrupt every currently active session.
    ///
    /// Called during gateway shutdown to signal all in-flight ReAct loops
    /// that they should finish at their next check-point.
    pub fn interrupt_all_sessions(&self) {
        let interrupts = self
            .active_interrupts
            .read()
            .expect("interrupt lock");
        for flag in interrupts.values() {
            flag.interrupt();
        }
        tracing::info!(
            count = interrupts.len(),
            "interrupted all active agent sessions for shutdown"
        );
    }

    /// Abort every tracked task handle (best-effort force cancel).
    ///
    /// Called after the grace period in shutdown, when agents haven't
    /// responded to the interrupt signal in time.
    pub fn abort_all_tasks(&self) {
        let mut tasks = self
            .active_tasks
            .write()
            .expect("active_tasks lock");
        let count = tasks.len();
        for (sid, handle) in tasks.drain() {
            handle.abort();
            tracing::warn!(%sid, "aborted lingering agent task during shutdown");
        }
        if count > 0 {
            tracing::info!(count, "aborted all lingering agent tasks");
        }
    }

    /// Rebuild session history from persisted JSONL events — **replay path**.
    ///
    /// This is the **replay** (恢复) operation, distinct from **continue** (继续):
    /// - **Replay** (this function): faithfully reconstructs the full conversation
    ///   history from the JSONL event log after a gateway restart. Every
    ///   `MessageReceived` and `reply_ready` event is converted into a
    ///   `ChatMessage` so the agent can pick up exactly where it left off.
    /// - **Continue** ([`build_continuation_context`]): compresses session
    ///   history into a structured summary for the LLM — does NOT replay events
    ///   to the EventBus, and does NOT dump raw tool outputs into the prompt.
    ///
    /// Converts stored events into `ChatMessage` objects so the agent's
    /// conversation context is restored.
    ///
    /// Reconstructs the *full* ReAct transcript — user messages, assistant
    /// replies, **assistant messages carrying `tool_calls`**, and `Tool`-role
    /// result messages — so that a later "继续" (continue) sees exactly what
    /// happened, not just a flat user+assistant list.
    ///
    /// Handles both the new enriched event format (objects with id/name/args)
    /// and the legacy format (tool names as plain strings) for backward
    /// compatibility with old JSONL files.
    pub fn restore_session_history(&self, session_id: &str, events: &[serde_json::Value]) {
        for event in events {
            let event_type = match event["event_type"].as_str() {
                Some(et) => et,
                None => continue,
            };
            let payload = &event["payload"];

            if event_type == "MessageReceived" {
                let text = payload["text"].as_str().unwrap_or("");
                if !text.is_empty() {
                    self.session_history.append(session_id, ChatMessage {
                        role: ChatMessageRole::User,
                        content: text.to_owned(),
                        tool_call_id: None,
                        tool_name: None,
                        tool_calls: None,
                        reasoning_content: String::new(),
                    });
                }
            } else if event_type.contains("reply_ready") || event_type == "llm_reply_ready" {
                let reply = payload["reply"]
                    .as_str()
                    .or_else(|| payload["full_text"].as_str())
                    .unwrap_or("");
                if !reply.is_empty() {
                    self.session_history.append(session_id, ChatMessage {
                        role: ChatMessageRole::Assistant,
                        content: reply.to_owned(),
                        tool_call_id: None,
                        tool_name: None,
                        tool_calls: None,
                        reasoning_content: String::new(),
                    });
                }
            } else if event_type == "agent:got_tool_calls" {
                // Reconstruct an Assistant message carrying `tool_calls`.
                // The enriched format stores full detail per tool so the LLM
                // sees the same structured tool_calls it originally produced.
                if let Some(tools) = payload["tools"].as_array() {
                    let tool_calls: Vec<serde_json::Value> = tools
                        .iter()
                        .map(|t| {
                            if let Some(name) = t.as_str() {
                                // Legacy format: bare tool name string.
                                serde_json::json!({
                                    "id": name,
                                    "type": "function",
                                    "function": { "name": name, "arguments": "{}" }
                                })
                            } else {
                                // Enriched format: { id, tool_name, args }.
                                serde_json::json!({
                                    "id": t["id"].as_str().unwrap_or(""),
                                    "type": "function",
                                    "function": {
                                        "name": t["tool_name"].as_str().unwrap_or("unknown"),
                                        "arguments": t["args"].as_str().unwrap_or("{}")
                                    }
                                })
                            }
                        })
                        .collect();
                    if !tool_calls.is_empty() {
                        self.session_history.append(session_id, ChatMessage {
                            role: ChatMessageRole::Assistant,
                            content: String::new(),
                            tool_call_id: None,
                            tool_name: None,
                            tool_calls: Some(tool_calls),
                            reasoning_content: String::new(),
                        });
                    }
                }
            } else if event_type == "tool:completed" {
                // Reconstruct a Tool-role result message carrying the
                // actual output so the LLM knows what each tool returned.
                let call_id = payload["tool_call_id"].as_str().unwrap_or("");
                let tool_name = payload["tool_name"].as_str().unwrap_or("unknown");
                // `output` is present in enriched events; fall back to
                // deriving a placeholder from `success` for legacy events.
                let output = payload["output"]
                    .as_str()
                    .unwrap_or(if payload["success"].as_bool().unwrap_or(false) {
                        "(success)"
                    } else {
                        "(failed)"
                    });
                self.session_history.append(session_id, ChatMessage {
                    role: ChatMessageRole::Tool,
                    content: output.to_owned(),
                    tool_call_id: Some(call_id.to_owned()),
                    tool_name: Some(tool_name.to_owned()),
                    tool_calls: None,
                    reasoning_content: String::new(),
                });
            }
        }
    }

    /// Rebuild in-memory session history from the persisted JSONL file.
    ///
    /// Used by the "继续" (continue) path: when the previous task was
    /// aborted (e.g. workflow timeout) or the gateway restarted, the
    /// in-memory `session_history` may be empty or incomplete even though
    /// the JSONL still holds the full conversation including tool calls and
    /// results.  This bridges the gap so the agent can resume with real
    /// context.
    ///
    /// When `force` is false, the rebuild is skipped if in-memory history
    /// is already populated (lazy restore).  When `force` is true, the
    /// in-memory history is cleared and fully reconstructed from JSONL —
    /// this is what the Continue path uses so it always gets the complete
    /// ReAct transcript (tool calls + results) rather than the lossy
    /// user+assistant-only subset kept in memory.
    async fn restore_session_history_from_jsonl(&self, session_id: &str, agent_id: &str, force: bool) {
        if !force && !self.session_history.get(session_id).is_empty() {
            return; // already populated — nothing to do
        }
        // Resolve the owning agent: prefer the explicit agent_id, but fall
        // back to scanning all stores (handles events published before
        // agent resolution, e.g. MessageReceived).
        let store = self.registry.get_session_store(agent_id).await
            .or_else(|| {
                // Synchronous poll of all stores — acceptable because this
                // only runs on the cold "继续" path, not the hot loop.
                let stores = pollster::block_on(self.registry.all_session_stores());
                stores.into_iter().find(|s| s.has_session(session_id))
            });
        if let Some(store) = store {
            let events = store.load_session_events(session_id);
            if !events.is_empty() {
                if force {
                    self.session_history.clear(session_id);
                }
                self.restore_session_history(session_id, &events);
                tracing::info!(%agent_id, %session_id, events = events.len(), force,
                    "restored session history from JSONL for continue");
            }
        }
    }

    /// Get a copy of the current conversation history for a session.
    pub fn get_session_history(&self, session_id: &str) -> Vec<ChatMessage> {
        self.session_history.get(session_id)
    }

    /// Check token budget and compress history if over the threshold.
    ///
    /// Returns the number of messages removed (0 if no compression was needed).
    /// Uses the configured [`CompressorConfig`] and estimates tokens via the
    /// [`TokenBudget`] heuristic.
    pub fn compress_session_history(
        &self,
        session_id: &str,
        model: &str,
        max_history_tokens: usize,
    ) -> usize {
        let mut history = self.session_history.get(session_id);
        if history.is_empty() {
            return 0;
        }

        let context_window = if max_history_tokens > 0 {
            max_history_tokens
        } else {
            context_manager::context_window_for_model(model)
        };

        // Reserve 20% for output + system prompt overhead
        let prompt_budget = (context_window as f64 * 0.80) as usize;

        let estimated_tokens: usize = history
            .iter()
            .map(|m| TokenBudget::estimate_tokens(&m.content))
            .sum();

        if estimated_tokens <= prompt_budget {
            return 0; // Under budget, no compression needed
        }

        let mut budget = TokenBudget::with_window(model, context_window, 0);
        budget.set_history_tokens(estimated_tokens);

        let compressor = HistoryCompressor::new(CompressionStrategy::Truncate);
        let config = self.compression_config.clone();
        let result = compressor.compress_with_boundaries(&mut history, &mut budget, &config);

        if result.messages_removed > 0 {
            // Replace history with compressed version
            self.session_history.clear(session_id);
            self.session_history.extend(session_id, history);
        }

        result.messages_removed
    }

    /// Resolve the first enabled agent from the registry.
    ///
    /// Used by `MessageReceivedHandler` when no target agent is specified.
    /// Delegates to the configured [`AgentRouter`] strategy.
    pub async fn resolve_first_enabled_agent(&self, user_text: &str) -> Option<AgentInstance> {
        let agents = self.registry.list().await;
        self.agent_router.route(user_text, &agents).await
    }

    /// Resolve an agent by ID, returning `None` if not found or disabled.
    pub async fn resolve_agent(&self, agent_id: &str) -> Option<AgentInstance> {
        let instance = self.registry.get(agent_id).await?;
        if instance.descriptor.enabled {
            Some(instance)
        } else {
            None
        }
    }

    /// Spawn a background task running `process_message`, with error logging.
    ///
    /// `continuation_mode` distinguishes "继续" ([`ContinuationMode::Continue`])
    /// from normal messages ([`ContinuationMode::Fresh`]) and gateway-restart
    /// recovery ([`ContinuationMode::Replay`]).
    #[allow(clippy::too_many_arguments)] // Dispatcher signature mirrors `process_message` args.
    pub fn spawn_process_message(
        self: &Arc<Self>,
        agent_id: String,
        session_id: String,
        user_text: String,
        model: String,
        soul_snapshot: SoulSnapshot,
        skill_name: Option<String>,
        react_mode: Option<skill::ReactMode>,
        background: bool,
        continuation_mode: ContinuationMode,
    ) -> tokio::task::JoinHandle<()> {
        let harness = Arc::clone(self);
        // Clone session_id before moving into the async closure —
        // we need the original to insert into active_tasks below.
        let sid = session_id.clone();
        let handle = self.runtime.spawn(async move {
            if let Err(e) = harness
                .process_message(&agent_id, &session_id, &user_text, &model, soul_snapshot.clone(), skill_name.as_deref(), react_mode, background, continuation_mode)
                .await
            {
                tracing::error!(
                    error = %e, session_id = %session_id, agent_id = %agent_id,
                    "process_message failed"
                );
                // NOTE: we do NOT publish agent:reply_interrupted here — that
                // is already done by the process_message_v2 error path (Fix 1).
                // Publishing again would double-flip the session workflow.
                // Reset agent state on error so it doesn't stay stuck.
                // Session ended → Ready (idle system is UI-driven now).
                harness.unregister_interrupt(&session_id);
                let _ = harness.registry.set_active_session(&agent_id, None).await;
                let _ = harness.registry.set_status(&agent_id, AgentStatus::Idle).await;
                let _ = harness.registry.set_system_state(&agent_id, AgentSystemState::Ready).await;
                let _ = harness.registry.set_activity(&agent_id, "").await;
                harness.session_history.clear(&session_id);
            }
            // Remove the task handle — the session is done (success or error).
            harness.remove_task(&session_id);
        });
        // Abort any existing task for this session before registering the new
        // one. Without this guard, a fast double-send would overwrite the old
        // task's abort handle in the map, turning it into a non-abortable
        // "ghost" whose later remove_task() call would erase the *new* task's
        // handle.
        let mut tasks = self.active_tasks.write().expect("active_tasks lock");
        if let Some(old) = tasks.remove(&sid) {
            old.abort();
            tracing::info!(session_id = %sid, "aborted previous task for session (new message received)");
        }
        // Save the abort handle so shutdown can force-cancel lingering tasks.
        tasks.insert(sid, handle.abort_handle());
        handle
    }

    /// Spawn an anonymous, ephemeral agent that runs a single task and
    /// returns its result via an [`AnonymousAgentHandle`].
    ///
    /// The anonymous agent is **not** registered in the `AgentRegistry`
    /// agents HashMap — it has no persistent identity, no SOUL.md, and
    /// leaves no trace after completion. It inherits the caller's LLM
    /// provider and uses the given [`AgentDescriptor`] for model, tool
    /// allow/deny lists, and token budget configuration.
    ///
    /// # Arguments
    /// * `descriptor` — Inline agent configuration (model, provider key,
    ///   tool allow/deny lists, token limits).  Constructed at runtime,
    ///   not read from config.
    /// * `soul_snapshot` — System prompt for this agent.  Built from an
    ///   arbitrary string — no `SOUL.md` file needed.
    /// * `user_text` — The task prompt to send to the agent.
    /// * `llm_provider` — The LLM API client to use (typically cloned
    ///   from the parent agent's provider).
    /// * `background` — If true, enables auto-continuation on max turns.
    #[allow(clippy::too_many_arguments)]
    pub fn spawn_anonymous(
        self: &Arc<Self>,
        descriptor: kernel::agent::AgentDescriptor,
        soul_snapshot: SoulSnapshot,
        user_text: String,
        llm_provider: Arc<dyn LlmProvider>,
        background: bool,
    ) -> AnonymousAgentHandle {
        let anon_id = format!("anon-{}", uuid::Uuid::new_v4());
        let session_id = uuid::Uuid::new_v4().to_string();
        let (result_tx, result_rx) = tokio::sync::oneshot::channel();

        // Clone for the handle before moving into the spawned task
        let handle_agent_id = anon_id.clone();
        let handle_session_id = session_id.clone();

        let harness = Arc::clone(self);
        self.runtime.spawn(async move {
            // Register the LLM provider so execute_turn can find it via
            // registry.get_llm_provider(anon_id).  This maps to the
            // llm_providers HashMap — NOT the agents HashMap — so the
            // anonymous agent is never visible in agent listings.
            harness
                .registry
                .set_llm_provider(&anon_id, llm_provider)
                .await;

            let result = harness
                .process_anonymous_message(
                    &anon_id,
                    &session_id,
                    &user_text,
                    &descriptor,
                    soul_snapshot,
                    background,
                )
                .await;

            // Clean up the temporary LLM provider mapping
            harness
                .registry
                .remove_llm_provider(&anon_id)
                .await;

            if let Err(ref e) = result {
                tracing::warn!(
                    agent_id = %anon_id,
                    session_id = %session_id,
                    error = %e,
                    "anonymous agent failed"
                );
            }

            let _ = result_tx.send(result);
        });

        AnonymousAgentHandle {
            agent_id: handle_agent_id,
            session_id: handle_session_id,
            result_rx,
        }
    }

    /// Process a user message through the full ReAct loop.
    ///
    /// This is the main entry point called when a `MESSAGE_RECEIVED` event arrives.
    ///
    /// ## `continuation_mode`
    ///
    /// Distinguishes three paths:
    /// - [`ContinuationMode::Fresh`] — normal message, append to history.
    /// - [`ContinuationMode::Continue`] — user clicked "继续" after max turns.
    ///   Compresses session history into a structured summary via
    ///   [`build_continuation_context`] instead of dumping raw tool outputs.
    /// - [`ContinuationMode::Replay`] — gateway-restart recovery.  Full
    ///   history is faithfully reconstructed by [`restore_session_history`]
    ///   before this function is called.
    #[allow(clippy::too_many_lines)]
    #[allow(clippy::too_many_arguments)]
    pub async fn process_message(
        self: &Arc<Self>,
        agent_id: &str,
        session_id: &str,
        user_text: &str,
        model: &str,
        soul_snapshot: SoulSnapshot,
        skill_name: Option<&str>,
        react_mode: Option<skill::ReactMode>,
        background: bool,
        continuation_mode: ContinuationMode,
    ) -> AmanResult<String> {
        // 8. Delegate to cognitive engine
        if skill_name.is_some() {
            self.try_publish_to_agent_bus(agent_id, Event::new(
                SOURCE_AGENT_HARNESS,
                EventType::Custom(EVT_AGENT_DIRECT_ACT_STARTED.to_owned()),
                json!({"agent_id": agent_id, "session_id": session_id, "skill_name": skill_name}),
            )).await;
        }
        // `react_mode` (skill execution mode) drives skill-level behaviour
        // via the soul/skill prompt — nothing to do here at the harness layer.
        let _ = react_mode;
        self.process_message_v2(agent_id, session_id, user_text, model, soul_snapshot, skill_name, background, continuation_mode).await
    }

    /// Process a message through the ReAct loop for an anonymous agent.
    ///
    /// This is a streamlined version of [`process_message`] that bypasses
    /// all registry operations (no agent lookup, no idle coordination, no
    /// memory retrieval, no status updates).  The anonymous agent is
    /// self-contained — everything it needs comes from the inline
    /// [`AgentDescriptor`] and [`SoulSnapshot`].
    async fn process_anonymous_message(
        self: &Arc<Self>,
        agent_id: &str,
        session_id: &str,
        user_text: &str,
        descriptor: &kernel::agent::AgentDescriptor,
        soul_snapshot: SoulSnapshot,
        background: bool,
    ) -> AmanResult<String> {
        self.process_message_v2(agent_id, session_id, user_text, &descriptor.model, soul_snapshot, None, background, ContinuationMode::Fresh).await
    }

    // ── process_message helpers ──────────────────────────────────────
    //
    // These methods were extracted from the previously 465-line
    // `process_message` (P0-5 in `docs/code-review-20260614.md`) to keep
    // the high-level orchestration legible. Each helper is a single
    // responsibility and can be tested in isolation once unit tests are
    // added (tracked in P1 #6).

    /// Look up the agent, flip status to Busy, and announce on the bus.
    ///
    /// Performs steps 1 + 2 of the original `process_message`:
    /// 1. Fetch the `AgentInstance` from the registry, refusing if the
    ///    agent is missing or disabled.
    /// 2. Mark the agent Busy in the registry, pick the right system
    ///    state for the UI (Working / Chatting), and publish
    ///    `agent:busy` on the local bus.
    ///
    /// The caller still owns the subsequent idle-coordination reset
    /// (boosting arousal) because that step touches subsystems outside
    /// the registry.
    async fn prepare_agent_session(
        &self,
        agent_id: &str,
        session_id: &str,
        background: bool,
    ) -> AmanResult<AgentInstance> {
        // 1. Get AgentInstance from registry
        let instance = self
            .registry
            .get(agent_id)
            .await
            .ok_or_else(|| Error::ConfigInvalid {
                message: format!("agent '{agent_id}' not found"),
            })?;

        if instance.status == AgentStatus::Disabled {
            return Err(Error::ConfigInvalid {
                message: format!("agent '{agent_id}' is disabled"),
            });
        }

        // 冷启动 reflection 还没完成前，拒绝接 LLM 调用。
        // idle loop 会通过 mark_cold_start_complete 把状态从 Preparing 切到 Idle。
        if instance.status == AgentStatus::Preparing {
            return Err(Error::ConfigInvalid {
                message: format!("agent '{agent_id}' is still starting up, please wait a moment ..."),
            });
        }

        // 2. Update status to Busy
        self.registry
            .set_active_session(agent_id, Some(session_id.to_owned()))
            .await?;
        self.registry.set_status(agent_id, AgentStatus::Busy).await?;
        // Pick the right system state for the UI:
        // - Work-item sessions (kanban Act! / startup / idle_run with work tag) → Working
        // - Foreground user messages → Chatting
        // - Background boredom runs → already set by boredom actor, leave as-is
        let is_work_session = super::session::work_session::is_plugin_work_session(session_id);
        if is_work_session {
            self.registry
                .set_system_state(agent_id, AgentSystemState::Working)
                .await;
        } else if !background {
            self.registry
                .set_system_state(agent_id, AgentSystemState::Chatting)
                .await;
        }

        // Publish agent:busy event to the agent's local bus
        self
            .try_publish_to_agent_bus(
                agent_id,
                Event::new(
                    SOURCE_AGENT_HARNESS,
                    EventType::Custom(EVT_AGENT_BUSY.to_owned()),
                    json!({
                        "agent_id": agent_id,
                        "session_id": session_id,
                    }),
                ),
            )

            .await;

        Ok(instance)
    }
    //
    // These methods were extracted from the previously 465-line
    // `process_message` (P0-5 in `docs/code-review-20260614.md`) to keep
    // the high-level orchestration legible. Each helper is a single
    // responsibility and can be tested in isolation once unit tests are
    // added (tracked in P1 #6).

    /// Initialize the model-aware token budget (M4).
    ///
    /// Pulls the context window and max-output tokens from the agent
    /// descriptor, falling back to `budget_policy` for the model-specific
    /// defaults. Emits a `agent:config_warning` event when either value
    /// resolves to 0 (so operators see misconfiguration in the SSE feed).
    /// Finally estimates system/tool-schema/history tokens so the very
    /// first `react_loop` iteration can apply compression if needed.
    #[allow(clippy::too_many_arguments)]
    async fn init_token_budget(
        &self,
        agent_id: &str,
        session_id: &str,
        model: &str,
        instance: &AgentInstance,
        soul_snapshot: &SoulSnapshot,
        history: &[ChatMessage],
        available_tools: &[ToolDescriptor],
    ) -> context_manager::TokenBudget {
        let mut token_budget = match (
            instance.descriptor.max_context_tokens,
            instance.descriptor.max_output_tokens,
        ) {
            (Some(ctx), Some(out)) => context_manager::TokenBudget::with_window(model, ctx, out),
            (Some(ctx), None) => context_manager::TokenBudget::with_window(
                model,
                ctx,
                self.budget_policy.max_output_tokens(
                    model,
                    instance.descriptor.max_output_tokens,
                ),
            ),
            _ => {
                let ctx = self.budget_policy.context_window(model);
                let out = self.budget_policy.max_output_tokens(model, None);
                context_manager::TokenBudget::with_window(model, ctx, out)
            }
        };

        if token_budget.max_output_tokens == 0 {
            let _ = self
                .bus
                .publish(Event::new(
                    SOURCE_AGENT_HARNESS,
                    EventType::Custom(EVT_AGENT_CONFIG_WARNING.to_owned()),
                    json!({
                        "agent_id": agent_id,
                        "session_id": session_id,
                        "config_key": "max_output_tokens",
                        "message": "max_output_tokens is 0 (not configured) — LLM API will use its provider default, which may truncate long responses",
                    }),
                ))
                .await;
        }
        if token_budget.context_window == 0 {
            let _ = self
                .bus
                .publish(Event::new(
                    SOURCE_AGENT_HARNESS,
                    EventType::Custom(EVT_AGENT_CONFIG_WARNING.to_owned()),
                    json!({
                        "agent_id": agent_id,
                        "session_id": session_id,
                        "config_key": "max_context_tokens",
                        "message": "max_context_tokens is 0 (not configured) — token budgeting is disabled",
                    }),
                ))
                .await;
        }

        // Estimate system prompt tokens
        token_budget
            .set_system_tokens(context_manager::TokenBudget::estimate_tokens(&soul_snapshot.system_prompt));
        // Estimate tool schema tokens
        let tool_schema_text: String = available_tools
            .iter()
            .map(|t| format!("{}: {}", t.name, t.parameters))
            .collect::<Vec<_>>()
            .join("\n");
        token_budget.set_tool_schema_tokens(
            context_manager::TokenBudget::estimate_tokens(&tool_schema_text),
        );
        // Estimate history tokens so the budget check in react_loop can
        // trigger compression before the first LLM call.
        let initial_history_tokens: usize = history
            .iter()
            .map(|m| context_manager::TokenBudget::estimate_tokens(&m.content))
            .sum();
        token_budget.set_history_tokens(initial_history_tokens);

        token_budget
    }

    /// Retrieve memories relevant to `user_text` and format them as a
    /// bullet list suitable for `ReActContext::memory_context` (M5 T5.1).
    ///
    /// Returns `None` when no memory provider is registered or when the
    /// recall returned an empty set, so the caller can short-circuit.
    async fn retrieve_relevant_memories(
        &self,
        agent_id: &str,
        user_text: &str,
    ) -> Option<String> {
        let provider = self.registry.get_memory_provider(agent_id).await?;
        let results = provider.recall(agent_id, user_text, 10).await;
        if results.is_empty() {
            return None;
        }
        let mem_text: Vec<String> = results
            .iter()
            .map(|m| format!("- {} (tags: {})", m.content, m.tags.join(", ")))
            .collect();
        Some(mem_text.join("\n"))
    }

    /// Compute grounding — how well-informed the agent is for this task.
    ///
    /// Uses Knowledge (from memory retrieval) and Situation (from user message
    /// clarity) to produce a Grounding assessment that the cognitive engine
    /// can use for behavior modulation.
    async fn compute_grounding(
        &self,
        _agent_id: &str,
        user_text: &str,
        mem: &Option<String>,
        tb: &context_manager::TokenBudget,
    ) -> cognitive_engine::Grounding {
        // Knowledge dimension: estimate from memory retrieval results
        let memory_input = cognitive_engine::KnowledgeInput {
            memory_count: mem.as_ref().map(|m| m.lines().count()).unwrap_or(0),
            avg_importance: if mem.is_some() { 0.5 } else { 0.0 },
            avg_age_days: None, // would need per-record timestamps; None = skip staleness check
            domain_count: 1,
        };
        let knowledge_signal = cognitive_engine::evaluate_knowledge(
            &memory_input,
            cognitive_engine::KnowledgeThresholds::default(),
        );

        // Situation dimension: from user message clarity and context fullness
        let context_tokens = mem.as_ref().map(|m| m.len()).unwrap_or(0)
            + user_text.len();
        let situation_input = cognitive_engine::SituationInput {
            user_text: user_text.to_string(),
            context_tokens,
            token_budget: tb.context_window,
        };
        let situation_signal = cognitive_engine::evaluate_situation(
            &situation_input,
            cognitive_engine::SituationThresholds::default(),
        );

        cognitive_engine::Grounding {
            knowledge: knowledge_signal,
            situation: situation_signal,
        }
    }

    /// Build tool descriptors from the tool registry for the given agent.
    async fn build_tool_descriptors(&self, agent_id: &str) -> Vec<ToolDescriptor> {
        let names = self.tool_registry.list_tools();
        let mut descriptors = Vec::new();

        for name in names {
            // Skip LLM provider tools (internal)
            if name.starts_with("llm_") || name.starts_with("llm_provider_") {
                continue;
            }

            // Check if this agent is allowed to use this tool
            if !self.registry.tool_allowed(agent_id, &name).await {
                continue;
            }

            if let Some(tool) = self.tool_registry.get(&name) {
                descriptors.push(ToolDescriptor {
                    name: tool.name().to_owned(),
                    description: tool.description().to_owned(),
                    parameters: serde_json::to_value(tool.parameters()).unwrap_or_default(),
                });
            }
        }

        descriptors
    }

    /// Build the full system prompt (soul + skills + tools + date + discipline + hints).
    ///
    /// Used by both foreground HTTP sessions and background idle_run sessions so the
    /// LLM always receives a consistent, complete system prompt.  The caller supplies
    /// the soul content, skill list, and the [`super::self_bridge::SelfBridge`] for
    /// Python-first prompt assembly; this method handles tool filtering (via
    /// [`build_tool_descriptors`]) and the Rust fallback path.
    pub async fn build_full_system_prompt(
        &self,
        agent_id: &str,
        soul_raw: &str,
        skills: &[skill::SkillInfo],
        self_bridge: &super::self_bridge::SelfBridge,
        cwd: Option<&str>,
    ) -> String {
        let skills_json = serde_json::to_value(skills).unwrap_or_default();

        // Build tool descriptors filtered by agent policy.
        let tool_descriptors = self.build_tool_descriptors(agent_id).await;
        let tools_json = serde_json::to_value(&tool_descriptors).unwrap_or_default();

        let prompt_ctx = super::self_bridge::SystemPromptContext {
            claude_md_content: None,
            cwd,
            platform: "desktop",
            model: None,
            provider: None,
        };

        // Python-first: unified system_prompt.py
        if let Some(prompt) = self_bridge.build_full_system_prompt(
            soul_raw,
            &skills_json,
            &tools_json,
            None, // memory is retrieved per-turn
            &prompt_ctx,
        ) {
            return prompt;
        }

        // Rust fallback when Python is unavailable
        let soul_prompt = self_bridge
            .build_soul_prompt(soul_raw)
            .unwrap_or_else(|| soul_raw.to_owned());
        let skills_prompt = self_bridge
            .build_skills_prompt(&skills_json)
            .unwrap_or_default();
        super::self_bridge::build_system_prompt_fallback(
            &soul_prompt,
            &skills_prompt,
            &tool_descriptors,
        )
    }

    /// Publish an agent-to-agent message to the event bus (M7).
    pub async fn publish_agent_message(
        &self,
        from_agent: &str,
        to_agent: &str,
        content_type: kernel::agent::AgentMessageType,
        payload: serde_json::Value,
        reply_to: Option<uuid::Uuid>,
    ) -> AmanResult<()> {
        let msg = kernel::agent::AgentMessage {
            message_id: uuid::Uuid::new_v4(),
            from_agent: from_agent.to_owned(),
            to_agent: to_agent.to_owned(),
            content_type,
            payload,
            reply_to,
            session_id: None,
        };
        let payload = serde_json::to_value(msg)?;
        self.bus.publish(Event::new(
            SOURCE_AGENT_HARNESS,
            EventType::AgentMessage,
            payload,
        )).await?;
        Ok(())
    }

    /// Publish a backend health change event to the global event bus.
    fn publish_backend_health_event(self: &Arc<Self>, changed: super::backend_health::BackendHealthChanged) {
        let event_type = match changed.to {
            super::backend_health::BackendStatus::Ok => "llm_backend_recovered",
            super::backend_health::BackendStatus::Degraded => "llm_backend_degraded",
            super::backend_health::BackendStatus::Down => "llm_backend_down",
            super::backend_health::BackendStatus::Unknown => "llm_backend_unknown",
        };
        let payload = match serde_json::to_value(&changed) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(error = %e, "failed to serialize BackendHealthChanged");
                serde_json::json!({ "base_url": changed.base_url })
            }
        };
        let bus = Arc::clone(&self.bus);
        tokio::spawn(async move {
            let _ = bus
                .publish(Event::new(
                    "llm_health",
                    EventType::Custom(event_type.to_owned()),
                    payload,
                ))
                .await;
        });
    }
}

/// Build a compressed session summary for "继续" (continue).
///
/// ## Continue vs Replay
///
/// This is the **continue** path — it produces a structured, human-readable
/// summary of the session history so the LLM can pick up the task without
/// dumping every raw tool output into the prompt (which would waste tokens
/// and flood the SSE channel with events from the new tool calls).
///
/// The **replay** path ([`restore_session_history`]) is different: it
/// faithfully reconstructs the full conversation from the JSONL event log
/// after a gateway restart, preserving every tool-call/result pair so the
/// agent can resume exactly where it left off.
///
/// ## Summary structure
///
/// ```text
/// [Previous Session Summary]
/// Goal: <what the user originally asked for>
/// Progress: <what has been accomplished>
/// Key findings: <important discoveries from tool outputs>
/// Tool usage: <N calls across M unique tools>
/// Status: <complete | incomplete | collision_found | stuck>
/// Last actions: <what was happening when the session paused>
/// ```
/// Retained as a utility for tests and a potential fallback.  The primary
/// Continue path now uses [`filter_conversation_history_with_tools`] against
/// the JSONL-rebuilt history for full fidelity; this summary path remains
/// available for scenarios where a compact representation is preferred.
#[allow(dead_code)]
fn build_continuation_context(history: &[ChatMessage]) -> Vec<ChatMessage> {
    use std::collections::BTreeMap;

    let mut user_messages: Vec<&str> = Vec::new();
    let mut assistant_replies: Vec<&str> = Vec::new();
    let mut tool_calls: BTreeMap<String, usize> = BTreeMap::new();
    let mut total_tool_calls: usize = 0;
    let mut key_findings: Vec<String> = Vec::new();
    let mut last_assistant_reply: &str = "";

    for msg in history {
        match msg.role {
            ChatMessageRole::User => {
                let text = msg.content.trim();
                if !text.is_empty()
                    && text != "/continue"
                    && text != "继续"
                    && !text.starts_with("[ACTIVATED SKILL:")
                {
                    user_messages.push(text);
                }
            }
            ChatMessageRole::Assistant => {
                let text = msg.content.trim();
                // Skip max-turns-reached markers — they're system-generated noise.
                if text.starts_with("[max ") && text.contains("turns reached") {
                    continue;
                }
                if !text.is_empty() {
                    assistant_replies.push(text);
                    last_assistant_reply = text;
                }
                // Count structured tool calls in this assistant message.
                if let Some(ref tcs) = msg.tool_calls {
                    for tc in tcs {
                        if let Some(name) = tc["function"]["name"].as_str() {
                            *tool_calls.entry(name.to_owned()).or_default() += 1;
                            total_tool_calls += 1;
                        }
                    }
                }
            }
            ChatMessageRole::Tool => {
                let name = msg.tool_name.as_deref().unwrap_or("unknown");
                *tool_calls.entry(name.to_owned()).or_default() += 1;
                total_tool_calls += 1;

                // Extract key findings: look for "COLLISION FOUND", "found: true",
                // "error", exit codes, or other significant markers.
                let content = &msg.content;
                if content.contains("COLLISION FOUND")
                    || content.contains("found\": true")
                    || content.contains("found: true")
                {
                    key_findings.push(format!(
                        "[{name}] COLLISION FOUND — goal achieved"
                    ));
                } else if let Some(line) = content
                    .lines()
                    .find(|l| {
                        l.contains("best_residual")
                            || l.contains("best_partial")
                            || l.contains("elapsed_seconds")
                    })
                {
                    key_findings.push(format!("[{name}] {}", line.trim()));
                }
            }
            ChatMessageRole::System => {
                // System messages are not user/assistant conversation;
                // skip them — the soul prompt is injected separately.
            }
        }
    }

    // ── Build the structured summary ──

    let mut summary = String::from("[Previous Session Summary]\n");

    // Goal: first user message.
    if let Some(first) = user_messages.first() {
        summary.push_str(&format!("Goal: {}\n", first));
    }

    // Progress: summarize the conversation arc.
    let msg_count = user_messages.len() + assistant_replies.len();
    if msg_count > 0 {
        summary.push_str(&format!(
            "Progress: {} messages exchanged ({} user, {} assistant)\n",
            msg_count,
            user_messages.len(),
            assistant_replies.len(),
        ));
    }

    // Key findings.
    if !key_findings.is_empty() {
        summary.push_str("Key findings:\n");
        for f in &key_findings {
            summary.push_str(&format!("  - {}\n", f));
        }
    }

    // Tool usage.
    if total_tool_calls > 0 {
        let unique = tool_calls.len();
        summary.push_str(&format!(
            "Tool usage: {} calls across {} unique tools\n",
            total_tool_calls, unique
        ));
        if unique <= 8 {
            for (name, count) in &tool_calls {
                summary.push_str(&format!("  - {}: {} calls\n", name, count));
            }
        }
    }

    // Status: infer from last messages.
    let status = if key_findings.iter().any(|f| f.contains("COLLISION FOUND")) {
        "complete — collision found"
    } else if last_assistant_reply.contains("stuck")
        || last_assistant_reply.contains("no progress")
    {
        "stuck"
    } else {
        "incomplete — task still in progress"
    };
    summary.push_str(&format!("Status: {}\n", status));

    // Last assistant context.
    if !last_assistant_reply.is_empty() {
        // Truncate very long replies.
        let truncated = if last_assistant_reply.len() > 500 {
            format!("{}…[truncated]", &last_assistant_reply[..500])
        } else {
            last_assistant_reply.to_owned()
        };
        summary.push_str(&format!("Last action: {}\n", truncated));
    }

    tracing::info!(
        summary_len = summary.len(),
        original_messages = history.len(),
        original_tool_calls = total_tool_calls,
        "continuation: built compressed session summary"
    );

    vec![ChatMessage::system(summary)]
}
// ──────────────────────────────────────────────────────────────────────────

/// Filter conversation history to user↔agent dialogue only.
///
/// Strips tool-call invocations, tool results, system messages, and
/// internal markers so the LLM sees a clean chat transcript — not
/// the raw ReAct loop internals.
///
/// Keeps:
/// - `User` messages (always)
/// - `Assistant` messages **without** tool_calls (final replies)
///
/// Drops:
/// - `Assistant` messages **with** tool_calls (tool invocations)
/// - `Tool` messages (tool results)
/// - `System` messages (internal markers, summaries)
fn filter_conversation_history(history: &[ChatMessage]) -> Vec<ChatMessage> {
    history
        .iter()
        .filter(|msg| match msg.role {
            ChatMessageRole::User => {
                // Skip internal command markers
                let text = msg.content.trim();
                !text.is_empty()
                    && text != "/continue"
                    && text != "继续"
                    && !text.starts_with("[ACTIVATED SKILL:")
            }
            ChatMessageRole::Assistant => {
                // Keep only final replies (no tool_calls — those are
                // tool invocations, not user-visible replies).
                msg.tool_calls.is_none() && !msg.content.trim().is_empty()
            }
            ChatMessageRole::Tool | ChatMessageRole::System => false,
        })
        .cloned()
        .collect()
}

/// Filter conversation history for the "继续" (continue) path.
///
/// Unlike [`filter_conversation_history`] (used for Fresh messages), this
/// variant **preserves** the full ReAct transcript:
///
/// Keeps:
/// - `User` messages (always, minus internal markers)
/// - `Assistant` messages — **including those carrying `tool_calls`**
/// - `Tool` messages (tool execution results)
///
/// Drops:
/// - `System` messages (internal markers, summaries)
///
/// This is what makes "继续" meaningful: the LLM sees every tool it called
/// and every result it got back, so it can pick up exactly where it left
/// off instead of starting over from a lossy summary.
fn filter_conversation_history_with_tools(history: &[ChatMessage]) -> Vec<ChatMessage> {
    history
        .iter()
        .filter(|msg| match msg.role {
            ChatMessageRole::User => {
                // Skip internal command markers
                let text = msg.content.trim();
                !text.is_empty()
                    && text != "/continue"
                    && text != "继续"
                    && !text.starts_with("[ACTIVATED SKILL:")
            }
            // Keep ALL assistant messages (including tool_calls) and Tool results.
            ChatMessageRole::Assistant | ChatMessageRole::Tool => true,
            ChatMessageRole::System => false,
        })
        .cloned()
        .collect()
}

/// Extract [remember: ...] commands from agent reply text.
///
/// Returns the cleaned text (with markers removed) and a list of
/// content strings to store as memories.
#[allow(dead_code)]
fn process_remember_commands(text: &str) -> (String, Vec<String>) {
    let mut remembered = Vec::new();
    let mut result = String::with_capacity(text.len());
    let mut remaining = text;

    while let Some(start) = remaining.find("[remember:") {
        // Append everything before this marker
        result.push_str(&remaining[..start]);
        remaining = &remaining[start + "[remember:".len()..];

        if let Some(end) = remaining.find(']') {
            let content = remaining[..end].trim();
            if !content.is_empty() && content.len() >= 2 {
                remembered.push(content.to_owned());
            }
            remaining = &remaining[end + 1..];
        } else {
            // No closing bracket, treat as literal text
            result.push_str("[remember:");
            break;
        }
    }

    // Append remaining text
    result.push_str(remaining);

    // Clean up whitespace artifacts
    let cleaned = result.trim().to_owned();

    (cleaned, remembered)
}

/// Strip common API key patterns from LLM output to prevent accidental leakage.
///
/// LLMs trained on code and API documentation may hallucinate patterns like
/// `"apiKey": "sk-..."` or `Authorization: Bearer sk-...` when their context
/// includes tool schemas or skill docs referencing API-based services.
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn process_remember_commands_extracts_single_marker() {
        let (text, memories) = process_remember_commands("Hello [remember: user likes rust] world");
        assert_eq!(text, "Hello  world");
        assert_eq!(memories, vec!["user likes rust"]);
    }

    #[test]
    fn process_remember_commands_extracts_multiple_markers() {
        let (text, memories) =
            process_remember_commands("[remember: foo] start [remember: bar] end [remember: baz]");
        assert_eq!(text, "start  end");
        assert_eq!(memories, vec!["foo", "bar", "baz"]);
    }

    #[test]
    fn process_remember_commands_keeps_literal_when_unclosed() {
        let (text, memories) = process_remember_commands("Note [remember: unfinished");
        assert_eq!(text, "Note [remember: unfinished");
        assert!(memories.is_empty());
    }

    #[test]
    fn process_remember_commands_ignores_empty_or_short_content() {
        let (text, memories) = process_remember_commands("[remember: ] [remember: x] valid");
        assert_eq!(text, "valid");
        assert!(memories.is_empty());
    }

    #[test]
    fn build_continuation_context_empty_history() {
        let ctx = build_continuation_context(&[]);
        assert_eq!(ctx.len(), 1);
        assert_eq!(ctx[0].role, ChatMessageRole::System);
        assert!(ctx[0].content.contains("[Previous Session Summary]"));
    }

    #[test]
    fn build_continuation_context_summarizes_user_assistant_exchange() {
        let history = vec![
            ChatMessage::user("Find the answer"),
            ChatMessage::assistant("Working on it"),
            ChatMessage::user("Any update?"),
            ChatMessage::assistant("Still searching"),
        ];
        let ctx = build_continuation_context(&history);
        let summary = &ctx[0].content;
        assert!(summary.contains("Goal: Find the answer"));
        assert!(summary.contains("4 messages exchanged"));
        assert!(summary.contains("Status: incomplete"));
        assert!(summary.contains("Last action: Still searching"));
    }

    #[test]
    fn build_continuation_context_detects_collision_found() {
        let history = vec![
            ChatMessage::user("Search collision"),
            ChatMessage::tool_result("tc1", "search", "COLLISION FOUND for block 42"),
        ];
        let ctx = build_continuation_context(&history);
        let summary = &ctx[0].content;
        assert!(summary.contains("COLLISION FOUND"));
        assert!(summary.contains("Status: complete"));
    }

    #[test]
    fn build_continuation_context_counts_tool_calls() {
        let mut assistant_msg = ChatMessage::assistant("using tools");
        assistant_msg.tool_calls = Some(vec![
            serde_json::json!({"function": {"name": "grep"}}),
            serde_json::json!({"function": {"name": "grep"}}),
            serde_json::json!({"function": {"name": "cat"}}),
        ]);
        let history = vec![ChatMessage::user("Run tools"), assistant_msg];
        let ctx = build_continuation_context(&history);
        let summary = &ctx[0].content;
        assert!(summary.contains("Tool usage: 3 calls across 2 unique tools"));
        assert!(summary.contains("grep: 2 calls"));
        assert!(summary.contains("cat: 1 calls"));
    }

    #[test]
    fn filter_conversation_history_keeps_user_and_final_assistant() {
        let history = vec![
            ChatMessage::user("Hello"),
            ChatMessage::assistant("Hi there!"),
            ChatMessage::user("What's the weather?"),
            ChatMessage::assistant("It's sunny!"),
        ];
        let filtered = filter_conversation_history(&history);
        assert_eq!(filtered.len(), 4);
        assert_eq!(filtered[0].content, "Hello");
        assert_eq!(filtered[1].content, "Hi there!");
        assert_eq!(filtered[2].content, "What's the weather?");
        assert_eq!(filtered[3].content, "It's sunny!");
    }

    #[test]
    fn filter_conversation_history_strips_tool_calls_and_results() {
        let mut tool_invocation = ChatMessage::assistant("");
        tool_invocation.tool_calls = Some(vec![
            serde_json::json!({"function": {"name": "search"}}),
        ]);
        let history = vec![
            ChatMessage::user("Search for cats"),
            tool_invocation,
            ChatMessage::tool_result("tc1", "search", "Found 5 cats"),
            ChatMessage::assistant("I found 5 cats for you!"),
        ];
        let filtered = filter_conversation_history(&history);
        assert_eq!(filtered.len(), 2, "should keep only user msg and final reply");
        assert_eq!(filtered[0].content, "Search for cats");
        assert_eq!(filtered[1].content, "I found 5 cats for you!");
    }

    #[test]
    fn filter_conversation_history_strips_system_and_internal_markers() {
        let history = vec![
            ChatMessage::system("[Previous Session Summary]"),
            ChatMessage::user("/continue"),
            ChatMessage::user("继续"),
            ChatMessage::user("[ACTIVATED SKILL: search]"),
            ChatMessage::user("Real question"),
            ChatMessage::assistant("Real answer"),
        ];
        let filtered = filter_conversation_history(&history);
        assert_eq!(filtered.len(), 2, "should keep only real user question and answer");
        assert_eq!(filtered[0].content, "Real question");
        assert_eq!(filtered[1].content, "Real answer");
    }

    // ── Continue-mode summary behaviour (Bug C) ──────────────────────────

    #[test]
    fn build_continuation_context_skips_bare_continue_marker() {
        // A user who previously asked something, then sent "继续" to resume.
        // The summary must reflect the *prior* task, not the marker itself.
        let history = vec![
            ChatMessage::user("Write a bip32 script"),
            ChatMessage::assistant("Working on it..."),
            ChatMessage::user("继续"),
        ];
        let ctx = build_continuation_context(&history);
        assert_eq!(ctx.len(), 1);
        let summary = &ctx[0].content;
        // The actual task is captured as the goal...
        assert!(
            summary.contains("Goal: Write a bip32 script"),
            "summary should capture prior task as Goal, got: {summary}"
        );
        // ...and the bare continuation marker is filtered out of user_messages.
        assert!(
            !summary.contains("Goal: 继续"),
            "bare marker must NOT become the Goal, got: {summary}"
        );
    }

    #[test]
    fn build_continuation_context_marks_prior_task_when_aborted_mid_reply() {
        // Mirrors the real bug scenario: the user asked something and the
        // agent started replying but never finished (LLM call hung, got
        // aborted).  Restoration must still capture the goal and mark the
        // session as incomplete.
        let history = vec![
            ChatMessage::user("Write a python script for bip32"),
            ChatMessage::assistant("I'll create the script now."),
        ];
        let ctx = build_continuation_context(&history);
        let summary = &ctx[0].content;
        assert!(summary.contains("Goal: Write a python script for bip32"));
        assert!(summary.contains("Status: incomplete"));
        assert!(summary.contains("Last action: I'll create the script now."));
    }

    // ── Tool-history restoration (the "继续" fix) ───────────────────────

    #[test]
    fn filter_conversation_history_with_tools_keeps_tool_calls_and_results() {
        // The Continue path must preserve the full ReAct transcript —
        // assistant messages carrying tool_calls and Tool-role results —
        // so the LLM can see exactly what it did before being resumed.
        let mut tool_invocation = ChatMessage::assistant("");
        tool_invocation.tool_calls = Some(vec![
            serde_json::json!({"id": "tc1", "type": "function", "function": {"name": "search", "arguments": "{}"}}),
        ]);
        let history = vec![
            ChatMessage::user("Search for cats"),
            tool_invocation,
            ChatMessage::tool_result("tc1", "search", "Found 5 cats"),
            ChatMessage::assistant("I found 5 cats for you!"),
        ];
        let filtered = filter_conversation_history_with_tools(&history);
        assert_eq!(filtered.len(), 4, "should keep user, tool_calls, tool result, and final reply");
        assert_eq!(filtered[0].role, ChatMessageRole::User);
        assert_eq!(filtered[1].role, ChatMessageRole::Assistant);
        assert!(filtered[1].tool_calls.is_some(), "tool_calls must be preserved");
        assert_eq!(filtered[2].role, ChatMessageRole::Tool);
        assert_eq!(filtered[2].content, "Found 5 cats");
        assert_eq!(filtered[3].role, ChatMessageRole::Assistant);
    }

    #[test]
    fn filter_conversation_history_with_tools_strips_system_and_markers() {
        // Even in tool-preserving mode, system messages and internal
        // continuation markers must still be stripped.
        let history = vec![
            ChatMessage::system("[Previous Session Summary]"),
            ChatMessage::user("/continue"),
            ChatMessage::user("继续"),
            ChatMessage::user("[ACTIVATED SKILL: search]"),
            ChatMessage::user("Real question"),
            ChatMessage::assistant("Real answer"),
        ];
        let filtered = filter_conversation_history_with_tools(&history);
        assert_eq!(filtered.len(), 2, "should keep only real user question and answer");
        assert_eq!(filtered[0].content, "Real question");
        assert_eq!(filtered[1].content, "Real answer");
    }

    #[tokio::test]
    async fn restore_session_history_reconstructs_tool_calls_from_enriched_event() {
        // The enriched `agent:got_tool_calls` event carries full detail
        // (id, tool_name, args).  Restoration must rebuild an Assistant
        // message with `tool_calls` populated.
        let harness = AgentHarness::new_test();
        let session_id = "test-session";
        let events = vec![
            serde_json::json!({
                "event_type": "MessageReceived",
                "payload": { "text": "Search the web" }
            }),
            serde_json::json!({
                "event_type": "agent:got_tool_calls",
                "payload": {
                    "tools": [
                        { "id": "call_1", "tool_name": "web_search", "args": "{\"query\":\"rust\"}" },
                        { "id": "call_2", "tool_name": "write", "args": "{\"path\":\"out.txt\"}" }
                    ]
                }
            }),
        ];
        harness.restore_session_history(session_id, &events);
        let history = harness.get_session_history(session_id);
        assert_eq!(history.len(), 2, "user + assistant-with-tool_calls");
        assert_eq!(history[0].role, ChatMessageRole::User);
        assert_eq!(history[0].content, "Search the web");
        assert_eq!(history[1].role, ChatMessageRole::Assistant);
        let tool_calls = history[1].tool_calls.as_ref().expect("tool_calls must be populated");
        assert_eq!(tool_calls.len(), 2);
        assert_eq!(tool_calls[0]["function"]["name"], "web_search");
        assert_eq!(tool_calls[0]["id"], "call_1");
        assert_eq!(tool_calls[1]["function"]["name"], "write");
    }

    #[tokio::test]
    async fn restore_session_history_reconstructs_tool_results_from_enriched_event() {
        // The enriched `tool:completed` event carries the output.
        // Restoration must rebuild a Tool-role message with that content.
        let harness = AgentHarness::new_test();
        let session_id = "test-session";
        let events = vec![
            serde_json::json!({
                "event_type": "tool:completed",
                "payload": {
                    "tool_call_id": "call_1",
                    "tool_name": "web_search",
                    "success": true,
                    "output": "Found 42 results about Rust",
                    "duration_ms": 150
                }
            }),
        ];
        harness.restore_session_history(session_id, &events);
        let history = harness.get_session_history(session_id);
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].role, ChatMessageRole::Tool);
        assert_eq!(history[0].tool_call_id.as_deref(), Some("call_1"));
        assert_eq!(history[0].tool_name.as_deref(), Some("web_search"));
        assert_eq!(history[0].content, "Found 42 results about Rust");
    }

    #[tokio::test]
    async fn restore_session_history_handles_legacy_tool_name_format() {
        // Old JSONL files stored tool names as plain strings (not objects).
        // Restoration must still work — producing tool_calls with the name
        // as both id and function name.
        let harness = AgentHarness::new_test();
        let session_id = "test-session";
        let events = vec![
            serde_json::json!({
                "event_type": "agent:got_tool_calls",
                "payload": { "tools": ["web_search", "write"] }
            }),
        ];
        harness.restore_session_history(session_id, &events);
        let history = harness.get_session_history(session_id);
        assert_eq!(history.len(), 1);
        let tool_calls = history[0].tool_calls.as_ref().expect("tool_calls must be populated");
        assert_eq!(tool_calls.len(), 2);
        assert_eq!(tool_calls[0]["function"]["name"], "web_search");
        assert_eq!(tool_calls[1]["function"]["name"], "write");
    }

    #[tokio::test]
    async fn restore_session_history_full_react_round_trip() {
        // End-to-end: a full ReAct transcript (user → tool_calls → results →
        // reply) round-trips through JSONL restoration with full fidelity.
        let harness = AgentHarness::new_test();
        let session_id = "test-session";
        let events = vec![
            serde_json::json!({
                "event_type": "MessageReceived",
                "payload": { "text": "Write a bip32 script" }
            }),
            serde_json::json!({
                "event_type": "agent:got_tool_calls",
                "payload": {
                    "tools": [
                        { "id": "c1", "tool_name": "write", "args": "{\"path\":\"bip32.py\"}" }
                    ]
                }
            }),
            serde_json::json!({
                "event_type": "tool:completed",
                "payload": {
                    "tool_call_id": "c1", "tool_name": "write",
                    "success": true, "output": "File written successfully"
                }
            }),
            serde_json::json!({
                "event_type": "agent:reply_ready",
                "payload": { "reply": "Script written. Let me verify it compiles." }
            }),
        ];
        harness.restore_session_history(session_id, &events);
        let history = harness.get_session_history(session_id);
        assert_eq!(history.len(), 4, "user + assistant+tool_calls + tool_result + assistant_reply");
        assert_eq!(history[0].role, ChatMessageRole::User);
        assert_eq!(history[0].content, "Write a bip32 script");
        assert_eq!(history[1].role, ChatMessageRole::Assistant);
        assert!(history[1].tool_calls.is_some());
        assert_eq!(history[2].role, ChatMessageRole::Tool);
        assert_eq!(history[2].content, "File written successfully");
        assert_eq!(history[3].role, ChatMessageRole::Assistant);
        assert_eq!(history[3].content, "Script written. Let me verify it compiles.");

        // And the Continue-path filter preserves all of it.
        let filtered = filter_conversation_history_with_tools(&history);
        assert_eq!(filtered.len(), 4, "all 4 messages preserved for the LLM on continue");
    }

    #[tokio::test]
    async fn restore_session_history_force_rebuild_clears_existing() {
        // When `force` is true, existing in-memory history is cleared
        // before rebuilding from JSONL — so the Continue path always gets
        // the full transcript, not a stale user+assistant subset.
        let harness = AgentHarness::new_test();
        let session_id = "test-session";
        // Pre-populate with stale data.
        harness.session_history.append(session_id, ChatMessage::user("stale"));
        assert_eq!(harness.get_session_history(session_id).len(), 1);
        // Force-rebuild from JSONL.
        let events = vec![
            serde_json::json!({
                "event_type": "MessageReceived",
                "payload": { "text": "fresh from JSONL" }
            }),
        ];
        harness.restore_session_history(session_id, &events);
        // Without the force flag in restore_session_history_from_jsonl this
        // would be appended; with clear+rebuild it replaces.
        // (This test exercises clear() directly since
        // restore_session_history_from_jsonl is async + needs registry.)
        harness.session_history.clear(session_id);
        harness.restore_session_history(session_id, &events);
        let history = harness.get_session_history(session_id);
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].content, "fresh from JSONL");
    }
}
