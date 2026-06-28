// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use event_bus::EventBus;
use kernel::agent::{AgentInstance, AgentStatus, AgentSystemState};
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
    EVT_AGENT_REPLY_READY, EVT_AGENT_REPLY_STREAM_ERROR,
    EVT_AGENT_CONFIG_WARNING,
};
use super::AgentRegistry;

/// Default maximum ReAct loop iterations.
const DEFAULT_MAX_REACT_TURNS: u32 = 64;

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
            max_react_turns: DEFAULT_MAX_REACT_TURNS,
            budget_policy,
            agent_router,
            compression_config,
            stream_forwarder_capacity,
            runtime,
        }
    }

    async fn build_cognitive_engine(
        &self, agent_id: &str, model: &str, session_id: &str, background: bool,
    ) -> AmanResult<LlmCognitiveEngine> {
        let kernel_provider = self.registry.get_llm_provider(agent_id).await
            .ok_or_else(|| Error::ConfigInvalid { message: format!("no LLM provider for agent '{agent_id}'") })?;
        // Adapt kernel::llm::LlmProvider → cognitive_llm::provider::LlmProvider
        struct KernelProviderAdapter(Arc<dyn LlmProvider>);
        #[async_trait::async_trait]
        impl cognitive_llm::provider::LlmProvider for KernelProviderAdapter {
            fn name(&self) -> &str { self.0.name() }
            async fn chat_completion(&self, req: cognitive_llm::provider::LlmChatRequest, cb: Option<Arc<dyn Fn(cognitive_llm::provider::StreamEvent) + Send + Sync>>) -> Result<cognitive_llm::provider::LlmResponse, String> {
                let kr = kernel::llm::LlmChatRequest { model: req.model, system_prompt: req.system_prompt, messages: req.messages.into_iter().map(|m| kernel::react::ChatMessage { role: match m.role { cognitive_react::ChatMessageRole::System => kernel::react::ChatMessageRole::System, cognitive_react::ChatMessageRole::User => kernel::react::ChatMessageRole::User, cognitive_react::ChatMessageRole::Assistant => kernel::react::ChatMessageRole::Assistant, cognitive_react::ChatMessageRole::Tool => kernel::react::ChatMessageRole::Tool }, content: m.content, tool_call_id: m.tool_call_id, tool_name: m.tool_name, tool_calls: m.tool_calls, reasoning_content: m.reasoning_content }).collect(), tools: req.tools.into_iter().map(|t| kernel::react::ToolDescriptor { name: t.name, description: t.description, parameters: t.parameters }).collect(), max_output_tokens: req.max_output_tokens, response_format: req.response_format.map(|f| match f { cognitive_llm::provider::ResponseFormat::JsonObject => kernel::llm::ResponseFormat::JsonObject, cognitive_llm::provider::ResponseFormat::JsonSchema { name, schema, strict } => kernel::llm::ResponseFormat::JsonSchema { name, schema, strict } }) };
                let kcb = cb.map(|c| { let c2 = c; Arc::new(move |e: kernel::llm::StreamEvent| c2(match e { kernel::llm::StreamEvent::Start => cognitive_llm::provider::StreamEvent::Start, kernel::llm::StreamEvent::Chunk(s) => cognitive_llm::provider::StreamEvent::Chunk(s), kernel::llm::StreamEvent::Done { finish_reason } => cognitive_llm::provider::StreamEvent::Done { finish_reason }, kernel::llm::StreamEvent::Error(s) => cognitive_llm::provider::StreamEvent::Error(s), })) as Arc<dyn Fn(cognitive_llm::provider::StreamEvent) + Send + Sync> });
                self.0.chat_completion(kr, kcb).await.map(|r| cognitive_llm::provider::LlmResponse { content: r.content, finish_reason: r.finish_reason, tool_calls: r.tool_calls.into_iter().map(|c| cognitive_react::ParsedToolCall { id: c.id, tool_name: c.tool_name, args: c.args }).collect(), reasoning_content: r.reasoning_content }).map_err(|e| e.to_string())
            }
        }
        let provider: Arc<dyn cognitive_llm::provider::LlmProvider> = Arc::new(KernelProviderAdapter(kernel_provider));
        let bus: Arc<dyn EventBus> = self.registry.get_local_bus(agent_id).await.unwrap_or_else(|| Arc::clone(&self.bus));
        let eb = Arc::clone(&bus);
        let sink: Arc<dyn Fn(Event) + Send + Sync> = Arc::new(move |e| { let b = Arc::clone(&eb); tokio::spawn(async move { let _ = b.publish(e).await; }); });
        let engine = cognitive_llm::LlmCognitiveEngine::new(provider, cognitive_llm::LlmEngineConfig {
            model: model.into(), max_turns: self.max_react_turns, token_limit: self.budget_policy.session_token_limit(),
            max_output_tokens: 4096, max_llm_retries: 5, background, max_continuations: 5,
        }).with_event_sink(sink).with_tool_executor(Arc::clone(&self.tool_registry), bus, 30_000);
        let lb: Arc<dyn EventBus> = self.registry.get_local_bus(agent_id).await.unwrap_or_else(|| Arc::clone(&self.bus));
        let sid = session_id.to_owned(); let aid = agent_id.to_owned();
        struct SB { bus: Arc<dyn EventBus>, aid: String, sid: String }
        impl cognitive_engine::CognitiveListener for SB {
            fn on_cognitive_event(&self, e: cognitive_engine::CognitiveEvent) {
                let b = Arc::clone(&self.bus); let a = self.aid.clone(); let s = self.sid.clone();
                tokio::spawn(async move {
                    let (et, pl) = match e {
                        cognitive_engine::CognitiveEvent::StreamStart { .. } => ("agent:reply_stream_start", json!({})),
                        cognitive_engine::CognitiveEvent::TextChunk { text, .. } => ("agent:reply_chunk", json!({"delta": text})),
                        cognitive_engine::CognitiveEvent::StreamDone { finish_reason, .. } => ("agent:reply_stream_done", json!({"finish_reason": finish_reason})),
                        cognitive_engine::CognitiveEvent::StreamError { error, .. } => ("agent:reply_stream_error", json!({"error": error})),
                        _ => return,
                    };
                    let _ = b.publish(Event::new(SOURCE_AGENT_HARNESS, EventType::Custom(et.into()), json!({"agent_id": a, "session_id": s, "extra": pl}))).await;
                });
            }
        }
        engine.subscribe(Arc::new(SB { bus: lb, aid, sid }));
        Ok(engine)
    }

    pub async fn process_message_v2(
        self: &Arc<Self>, agent_id: &str, session_id: &str, user_text: &str,
        model: &str, soul_snapshot: SoulSnapshot, background: bool,
    ) -> AmanResult<String> {
        let inst = self.prepare_agent_session(agent_id, session_id, background).await?;
        if let Some(c) = self.registry.get_idle_coordination(agent_id).await { c.reset_idle_signal().await; c.arousal.boost(0.3); }
        let tools = self.build_tool_descriptors(agent_id).await;
        let mut hist = self.session_history.get(session_id); hist.push(ChatMessage::user(user_text));
        let _tb = self.init_token_budget(agent_id, session_id, model, &inst, &soul_snapshot, &hist, &tools).await;
        let mem = self.retrieve_relevant_memories(agent_id, user_text).await;
        let flag = Arc::new(InterruptFlag::new()); self.register_interrupt(session_id, Arc::clone(&flag));
        let engine = self.build_cognitive_engine(agent_id, model, session_id, background).await?;
        let ctx = CognitiveContext {
            agent_id: agent_id.into(), session_id: session_id.into(),
            identity: CognitiveIdentity { name: soul_snapshot.name.clone(), identity: soul_snapshot.system_prompt.clone(), boundaries: soul_snapshot.boundaries.clone(), expertise: vec![], vibe: None, raw: soul_snapshot.system_prompt.clone() },
            capabilities: tools.iter().map(|t| cognitive_engine::Capability { name: t.name.clone(), description: t.description.clone(), parameters: t.parameters.clone(), cap_type: cognitive_engine::CapabilityType::Tool }).collect(),
            memory_context: mem.map(|m| vec![cognitive_engine::MemoryItem { key: "retrieved".into(), content: m, importance: 0.5, timestamp: None }]).unwrap_or_default(),
            engine_config: json!({"model": model}),
        };
        let obs = vec![Observation::user_message(uuid::Uuid::now_v7().to_string(), session_id, user_text)];
        tracing::info!(%agent_id, %session_id, "process_message_v2: calling engine.process()");
        let result = engine.process(&ctx, obs).await;
        tracing::info!(%agent_id, %session_id, success = result.is_ok(), "process_message_v2: engine.process() completed");

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
            self.unregister_interrupt(session_id);
            let _ = self.registry.set_active_session(agent_id, None).await;
            let _ = self.registry.set_status(agent_id, AgentStatus::Idle).await;
            let _ = self.registry.set_system_state(agent_id, AgentSystemState::Idle).await;
            let _ = self.registry.set_activity(agent_id, "").await;
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
        self.session_history.clear(session_id);
        self.session_history.extend(session_id, vec![ChatMessage::user(user_text), ChatMessage::assistant(&reply)]);
        self.unregister_interrupt(session_id);
        let _ = self.registry.set_active_session(agent_id, None).await;
        // Reset state to Idle on success
        let _ = self.registry.set_status(agent_id, AgentStatus::Idle).await;
        let _ = self.registry.set_system_state(agent_id, AgentSystemState::Idle).await;
        let _ = self.registry.set_activity(agent_id, "").await;
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
    /// Converts stored `MessageReceived` and `reply_ready` events into
    /// `ChatMessage` objects so the agent's conversation context is restored.
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
        self.runtime.spawn(async move {
            if let Err(e) = harness
                .process_message(&agent_id, &session_id, &user_text, &model, soul_snapshot.clone(), skill_name.as_deref(), react_mode, background, continuation_mode)
                .await
            {
                tracing::error!(
                    error = %e, session_id = %session_id, agent_id = %agent_id,
                    "process_message failed"
                );
                // Reset agent state on error so it doesn't stay stuck.
                harness.unregister_interrupt(&session_id);
                let _ = harness.registry.set_active_session(&agent_id, None).await;
                let _ = harness.registry.set_status(&agent_id, AgentStatus::Idle).await;
                let _ = harness.registry.set_system_state(&agent_id, AgentSystemState::Idle).await;
                let _ = harness.registry.set_activity(&agent_id, "").await;
                harness.session_history.clear(&session_id);
            }
        })
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
        let _ = (&react_mode, &continuation_mode); // consumed by process_message_v2
        self.process_message_v2(agent_id, session_id, user_text, model, soul_snapshot, background).await
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
        self.process_message_v2(agent_id, session_id, user_text, &descriptor.model, soul_snapshot, background).await
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
}
