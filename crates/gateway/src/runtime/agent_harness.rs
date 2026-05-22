use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
use std::time::Instant;

use event_bus::EventBus;
use kernel::agent::{AgentInstance, AgentStatus};
use kernel::budget::TokenBudgetPolicy;
use kernel::event::{Event, EventType};
use kernel::llm::{self, LlmChatRequest, LlmProvider};
use kernel::memory::MemoryRetrieval;
use kernel::prompt::PromptPipeline;
use kernel::react::{
    self, ChatMessage, ChatMessageRole, ParsedToolCall, ReActContext, ReActEngine as _, ReActTurn,
    SoulSnapshot, StreamEvent, ToolDescriptor,
};
use kernel::router::AgentRouter;
use kernel::session_history::SessionHistoryStore;
use kernel::{AmanResult, Error};
use serde_json::json;
use tool::security;
use tool::ToolRegistry;
use tool::ToolSecurityConfig;

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

/// Thread-safe flag for interrupting the ReAct loop.
#[derive(Debug, Default)]
pub struct InterruptFlag {
    interrupted: AtomicBool,
}

impl InterruptFlag {
    pub fn new() -> Self {
        Self {
            interrupted: AtomicBool::new(false),
        }
    }

    /// Signal interruption.
    pub fn interrupt(&self) {
        self.interrupted.store(true, Ordering::Release);
    }

    /// Check if interruption was signaled.
    pub fn is_interrupted(&self) -> bool {
        self.interrupted.load(Ordering::Acquire)
    }

    /// Reset the flag.
    pub fn reset(&self) {
        self.interrupted.store(false, Ordering::Release);
    }
}

/// Outcome of the ReAct loop.
#[derive(Debug)]
pub enum ReactOutcome {
    /// Normal completion with the final reply text.
    Finished(String),
    /// Loop was interrupted (user /stop), partial content if any.
    Interrupted(String),
}

/// Wraps tool execution with permission checks and event publishing.
pub struct ToolExecutor {
    registry: Arc<ToolRegistry>,
    agent_registry: Arc<AgentRegistry>,
    bus: Arc<dyn EventBus>,
    /// Optional path/network/command allowlist config for the ReAct path.
    security_config: Option<ToolSecurityConfig>,
}

impl ToolExecutor {
    pub fn new(
        registry: Arc<ToolRegistry>,
        agent_registry: Arc<AgentRegistry>,
        bus: Arc<dyn EventBus>,
    ) -> Self {
        Self {
            registry,
            agent_registry,
            bus,
            security_config: None,
        }
    }

    /// Set a security config for path/network/command allowlist checks.
    #[must_use]
    #[allow(dead_code)]
    pub fn with_security_config(mut self, config: ToolSecurityConfig) -> Self {
        self.security_config = Some(config);
        self
    }

    /// Execute a tool call for a specific agent, checking permissions first.
    ///
    /// Returns a structured result — permission denials are returned as
    /// failed results so the LLM can adapt, rather than aborting the loop.
    pub async fn execute_for_agent(
        &self,
        call: &ParsedToolCall,
        agent_id: &str,
        session_id: &str,
    ) -> react::ToolCallResult {
        let tool_name = &call.tool_name;

        // Permission check: is this agent allowed to use this tool?
        if !self
            .agent_registry
            .tool_allowed(agent_id, tool_name)
            .await
        {
            return react::ToolCallResult {
                id: call.id.clone(),
                tool_name: tool_name.clone(),
                success: false,
                output: format!(
                    "permission_denied: agent '{agent_id}' is not allowed to use tool '{tool_name}'"
                ),
                duration_ms: 0,
            };
        }

        self.execute(call, agent_id, session_id).await
    }

    /// Publish an event to the agent's local bus, falling back to the global bus.
    async fn publish_to_agent_bus(
        &self,
        agent_id: &str,
        event: Event,
    ) -> AmanResult<()> {
        match self.agent_registry.get_local_bus(agent_id).await {
            Some(local_bus) => local_bus.publish(event).await,
            None => self.bus.publish(event).await,
        }
    }

    /// Execute a tool call, publishing lifecycle events.
    pub async fn execute(
        &self,
        call: &ParsedToolCall,
        agent_id: &str,
        session_id: &str,
    ) -> react::ToolCallResult {
        let start = Instant::now();
        let tool_id = call.id.clone();
        let tool_name = call.tool_name.clone();

        // Publish tool:dispatched to local bus
        let _ = self
            .publish_to_agent_bus(
                agent_id,
                Event::new(
                    "agent:harness",
                    EventType::Custom("tool:dispatched".to_owned()),
                    json!({
                        "agent_id": agent_id,
                        "session_id": session_id,
                        "tool_call_id": tool_id,
                        "tool_name": tool_name,
                        "args": call.args,
                    }),
                ),
            )
            .await;

        // ── Security checks ──────────────────────────────────────────
        let hardline_blocked: Option<&str> =
            security::check_hardline_block(&tool_name, &call.args);

        let config_blocked: Option<String> = self.security_config.as_ref().and_then(|config| {
            tool::check_tool_security(config, &call.args)
                .err()
                .map(|e| e.to_string())
        });

        // Publish security denied events to local bus.
        if let Some(reason) = hardline_blocked {
            let _ = self
                .publish_to_agent_bus(
                    agent_id,
                    Event::new(
                        "agent:harness",
                        EventType::Custom("tool:security_denied".to_owned()),
                        json!({
                            "agent_id": agent_id,
                            "session_id": session_id,
                            "tool_call_id": tool_id,
                            "tool_name": tool_name,
                            "block_type": "hardline",
                            "reason": reason,
                        }),
                    ),
                )
                .await;
        }
        if let Some(ref reason) = config_blocked {
            let _ = self
                .publish_to_agent_bus(
                    agent_id,
                    Event::new(
                        "agent:harness",
                        EventType::Custom("tool:security_denied".to_owned()),
                        json!({
                            "agent_id": agent_id,
                            "session_id": session_id,
                            "tool_call_id": tool_id,
                            "tool_name": tool_name,
                            "block_type": "path_denied",
                            "reason": reason,
                        }),
                    ),
                )
                .await;
        }

        // ── Tool execution (or short-circuit if security blocked) ─────
        let tool = self.registry.get(&tool_name);
        let (success, output) = match tool {
            Some(t) => {
                if let Some(reason) = hardline_blocked {
                    (false, format!("hardline_blocked: {reason}"))
                } else if let Some(ref reason) = config_blocked {
                    (false, format!("security_denied: {reason}"))
                } else {
                    // Reset consecutive read tracking when a non-read tool runs.
                    if tool_name != "read" {
                        tool::fs_tools::reset_read_tracker();
                    }

                    let mut ctx = kernel::context::ToolContext::default();
                    ctx.base
                        .extensions
                        .insert("agent_id".to_owned(), serde_json::json!(agent_id));
                    match t.execute(call.args.clone(), ctx).await {
                        Ok(value) => (true, value.to_string()),
                        Err(e) => (false, format!("tool error: {e}")),
                    }
                }
            }
            None => (false, format!("tool not found: {tool_name}")),
        };

        let duration_ms = start.elapsed().as_millis() as u64;

        let event_type = if success {
            "tool:completed"
        } else {
            "tool:failed"
        };
        let _ = self
            .publish_to_agent_bus(
                agent_id,
                Event::new(
                    "agent:harness",
                    EventType::Custom(event_type.to_owned()),
                    json!({
                        "agent_id": agent_id,
                        "session_id": session_id,
                        "tool_call_id": tool_id,
                        "tool_name": tool_name,
                        "success": success,
                        "duration_ms": duration_ms,
                        "output": output,
                    }),
                ),
            )
            .await;

        react::ToolCallResult {
            id: tool_id,
            tool_name,
            success,
            output,
            duration_ms,
        }
    }
}

/// Concrete ReAct engine that calls an LLM provider.
pub struct LlmReActEngine {
    tool_registry: Arc<ToolRegistry>,
    agent_registry: Arc<AgentRegistry>,
    bus: Arc<dyn EventBus>,
    /// The LLM provider implementation (OpenAI, etc.).
    llm_provider: Arc<dyn LlmProvider>,
    /// Prompt pipeline for building system prompts.
    prompt_pipeline: Box<dyn PromptPipeline>,
}

impl LlmReActEngine {
    pub fn new(
        tool_registry: Arc<ToolRegistry>,
        agent_registry: Arc<AgentRegistry>,
        bus: Arc<dyn EventBus>,
        llm_provider: Arc<dyn LlmProvider>,
        prompt_pipeline: Box<dyn PromptPipeline>,
    ) -> Self {
        Self {
            tool_registry,
            agent_registry,
            bus,
            llm_provider,
            prompt_pipeline,
        }
    }

    /// Publish an event to the agent's local bus, falling back to the global bus.
    async fn publish_to_agent_bus(
        &self,
        agent_id: &str,
        event: Event,
    ) -> AmanResult<()> {
        match self.agent_registry.get_local_bus(agent_id).await {
            Some(local_bus) => local_bus.publish(event).await,
            None => self.bus.publish(event).await,
        }
    }
}

#[async_trait::async_trait]
impl kernel::react::ReActEngine for LlmReActEngine {
    async fn execute_turn(
        &self,
        ctx: &ReActContext,
        messages: Vec<ChatMessage>,
    ) -> Result<ReActTurn, kernel::react::ReActError> {
        // Publish llm:call_started to local bus
        let _ = self
            .publish_to_agent_bus(
                &ctx.agent_id,
                Event::new(
                    "agent:harness",
                    EventType::Custom("llm:call_started".to_owned()),
                    json!({
                        "agent_id": ctx.agent_id,
                        "session_id": ctx.session_id,
                        "turn": ctx.turn,
                    }),
                ),
            )
            .await;

        // Build the system prompt from soul + conversation history
        let system_prompt = self
            .prompt_pipeline
            .build_system_prompt(
                &ctx.soul_snapshot,
                &ctx.agent_tools,
                ctx.memory_context.as_deref(),
            )
            .await;

        let cb = ctx.stream_cb.as_ref().map(Arc::clone);

        let req = LlmChatRequest {
            model: ctx.model.clone(),
            system_prompt,
            messages,
            tools: ctx.agent_tools.clone(),
            max_output_tokens: ctx.token_budget.max_output_tokens as u32,
        };

        let result = self.llm_provider.chat_completion(req, cb).await;

        // Publish llm:call_ended to local bus
        let _ = self
            .publish_to_agent_bus(
                &ctx.agent_id,
                Event::new(
                    "agent:harness",
                    EventType::Custom("llm:call_ended".to_owned()),
                    json!({
                        "agent_id": ctx.agent_id,
                        "session_id": ctx.session_id,
                        "turn": ctx.turn,
                        "success": result.is_ok(),
                    }),
                ),
            )
            .await;

        match result {
            Ok(response) => {
                if response.tool_calls.is_empty() {
                    // Publish token usage estimate to local bus
                    let estimated_tokens = (response.content.len() / 4) as u64;
                    let _ = self
                        .publish_to_agent_bus(
                            &ctx.agent_id,
                            Event::new(
                                "agent:harness",
                                EventType::Custom("agent:token_used".to_owned()),
                                json!({
                                    "agent_id": ctx.agent_id,
                                    "session_id": ctx.session_id,
                                    "turn": ctx.turn,
                                    "tokens": estimated_tokens,
                                }),
                            ),
                        )
                        .await;

                    Ok(ReActTurn::Finished {
                        content: response.content,
                        finish_reason: response.finish_reason,
                    })
                } else {
                    Ok(ReActTurn::ToolCalls {
                        content: response.content,
                        calls: response.tool_calls,
                        reasoning_content: response.reasoning_content,
                    })
                }
            }
            Err(e) => Err(kernel::react::ReActError::LlmError(e.to_string())),
        }
    }

    async fn execute_tools(
        &self,
        ctx: &ReActContext,
        calls: &[ParsedToolCall],
    ) -> Result<Vec<ChatMessage>, kernel::react::ReActError> {
        let executor = ToolExecutor::new(
            Arc::clone(&self.tool_registry),
            Arc::clone(&self.agent_registry),
            Arc::clone(&self.bus),
        );
        let mut results = Vec::with_capacity(calls.len());

        for call in calls {
            // Use execute_for_agent to enforce per-agent tool permissions (M3)
            let result = executor.execute_for_agent(call, &ctx.agent_id, &ctx.session_id).await;
            results.push(ChatMessage::tool_result(
                &result.id,
                &result.tool_name,
                &result.output,
            ));
        }

        Ok(results)
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
    engine: LlmReActEngine,
    bus: Arc<dyn EventBus>,
    /// Pluggable memory retrieval for long-term recall.
    memory_store: Arc<dyn MemoryRetrieval>,
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
}

impl AgentHarness {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        registry: Arc<AgentRegistry>,
        tool_registry: Arc<ToolRegistry>,
        bus: Arc<dyn EventBus>,
        memory_store: Arc<dyn MemoryRetrieval>,
        llm_provider: Arc<dyn LlmProvider>,
        prompt_pipeline: Box<dyn PromptPipeline>,
        session_history: Box<dyn SessionHistoryStore>,
        budget_policy: Box<dyn TokenBudgetPolicy>,
        agent_router: Box<dyn AgentRouter>,
    ) -> Self {
        let engine = LlmReActEngine::new(
            Arc::clone(&tool_registry),
            Arc::clone(&registry),
            Arc::clone(&bus),
            llm_provider,
            prompt_pipeline,
        );
        Self {
            registry,
            tool_registry,
            engine,
            bus,
            memory_store,
            session_history,
            active_interrupts: RwLock::new(HashMap::new()),
            max_react_turns: DEFAULT_MAX_REACT_TURNS,
            budget_policy,
            agent_router,
        }
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

    /// Rebuild session history from persisted JSONL events after a restart.
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
    pub fn spawn_process_message(
        self: &Arc<Self>,
        agent_id: String,
        session_id: String,
        user_text: String,
        model: String,
        soul_snapshot: SoulSnapshot,
    ) -> tokio::task::JoinHandle<()> {
        let harness = Arc::clone(self);
        tokio::spawn(async move {
            if let Err(e) = harness
                .process_message(&agent_id, &session_id, &user_text, &model, soul_snapshot)
                .await
            {
                tracing::error!(
                    error = %e, session_id = %session_id, agent_id = %agent_id,
                    "process_message failed"
                );
            }
        })
    }

    /// Process a user message through the full ReAct loop.
    ///
    /// This is the main entry point called when a `MESSAGE_RECEIVED` event arrives.
    #[allow(clippy::too_many_lines)]
    pub async fn process_message(
        &self,
        agent_id: &str,
        session_id: &str,
        user_text: &str,
        model: &str,
        soul_snapshot: SoulSnapshot,
    ) -> AmanResult<String> {
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

        // Cancel any running idle workflows for this agent and boost arousal
        if let Some(coord) = self.registry.get_idle_coordination(agent_id).await {
            coord.reset_idle_signal().await;
            coord.arousal.boost(0.3);
        }

        // Publish agent:busy event to the agent's local bus
        let _ = self
            .publish_to_agent_bus(
                agent_id,
                Event::new(
                    "agent:harness",
                    EventType::Custom("agent:busy".to_owned()),
                    json!({
                        "agent_id": agent_id,
                        "session_id": session_id,
                    }),
                ),
            )
            .await;

        // 3. Build tool descriptors from registered tools
        let available_tools = self.build_tool_descriptors(agent_id).await;

        // 4. Build conversation history — load existing session history
        // and append the new user message for cross-turn continuity.
        let mut history = self.session_history.get(session_id);
        history.push(ChatMessage::user(user_text));

        // 5. Initialize model-aware token budget (M4)
        // Values must come from config, never silently defaulted.
        let mut token_budget = match (instance.descriptor.max_context_tokens, instance.descriptor.max_output_tokens) {
            (Some(ctx), Some(out)) => {
                crate::runtime::token_budget::TokenBudget::with_window(model, ctx, out)
            }
            (Some(ctx), None) => {
                crate::runtime::token_budget::TokenBudget::with_window(model, ctx, self.budget_policy.max_output_tokens(model, instance.descriptor.max_output_tokens))
            }
            _ => {
                let ctx = self.budget_policy.context_window(model);
                let out = self.budget_policy.max_output_tokens(model, None);
                crate::runtime::token_budget::TokenBudget::with_window(model, ctx, out)
            }
        };
        // Emit config warning events when token budget values are 0 (not configured).
        if token_budget.max_output_tokens == 0 {
            let _ = self
                .bus
                .publish(Event::new(
                    "agent:harness",
                    EventType::Custom("agent:config_warning".to_owned()),
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
                    "agent:harness",
                    EventType::Custom("agent:config_warning".to_owned()),
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
        token_budget.set_system_tokens(crate::runtime::token_budget::TokenBudget::estimate_tokens(&soul_snapshot.system_prompt));
        // Estimate tool schema tokens
        let tool_schema_text: String = available_tools
            .iter()
            .map(|t| format!("{}: {}", t.name, t.parameters))
            .collect::<Vec<_>>()
            .join("\n");
        token_budget.set_tool_schema_tokens(crate::runtime::token_budget::TokenBudget::estimate_tokens(&tool_schema_text));

        // 6. Retrieve memories relevant to user input (M5 T5.1)
        let memory_results = self.memory_store.retrieve(agent_id, user_text).await;
        let memory_context = if memory_results.is_empty() {
            None
        } else {
            let mem_text: Vec<String> = memory_results
                .iter()
                .map(|m| format!("- {} (tags: {})", m.content, m.tags.join(", ")))
                .collect();
            Some(mem_text.join("\n"))
        };

        // 7. Create ReAct context with the config-correct max_output_tokens
        let max_output_tokens = token_budget.max_output_tokens as u64;
        let mut ctx = ReActContext::new(
            agent_id.to_owned(),
            session_id.to_owned(),
            soul_snapshot,
            history,
            available_tools,
            model,
            self.max_react_turns,
            self.budget_policy.session_token_limit(),
            max_output_tokens,
        );
        ctx.memory_context = memory_context;

        // M6: Register interrupt flag for this session
        let interrupt_flag = Arc::new(InterruptFlag::new());
        self.register_interrupt(session_id, Arc::clone(&interrupt_flag));

        // 8. Execute ReAct loop with token budget management
        let result = self
            .react_loop(&mut ctx, &mut token_budget, Some(&interrupt_flag))
            .await;

        // Save conversation history for cross-turn continuity.
        self.session_history.clear(session_id);
        self.session_history.extend(session_id, ctx.history.clone());

        // M6: Unregister interrupt flag
        self.unregister_interrupt(session_id);

        // 9. Handle outcome — Finished, Interrupted, or Error
        let (raw_reply, event_type): (String, &str) = match result {
            Ok(ReactOutcome::Finished(reply)) => (reply, "agent:reply_ready"),
            Ok(ReactOutcome::Interrupted(reply)) => (reply, "agent:reply_interrupted"),
            Err(e) => {
                // Publish fallback error event so the frontend doesn't hang
                let _ = self
                    .bus
                    .publish(Event::new(
                        "agent:harness",
                        EventType::Custom("agent:reply_stream_error".to_owned()),
                        json!({
                            "agent_id": agent_id,
                            "session_id": session_id,
                            "error": e.to_string(),
                        }),
                    ))
                    .await;
                // Still go to Idle on error
                let _ = self.registry.set_active_session(agent_id, None).await;
                let _ = self.registry.set_status(agent_id, AgentStatus::Idle).await;
                return Err(e);
            }
        };

        // 10. Auto-write memories from [remember: ...] patterns (M5 T5.2)
        let (final_reply, remembered) = process_remember_commands(&raw_reply);
        for content in &remembered {
            self.memory_store.store(agent_id, content, vec!["auto".to_owned()]);
        }

        // Sanitize API key patterns from output before publishing.
        // LLMs may hallucinate apiKey patterns when exposed to tool schemas
        // or skill documentation that references API-based services.
        let final_reply = sanitize_api_keys(&final_reply);

        // 11. Publish reply event
        let _ = self
            .bus
            .publish(Event::new(
                "agent:harness",
                EventType::Custom(event_type.to_owned()),
                json!({
                    "agent_id": agent_id,
                    "session_id": session_id,
                    "reply": final_reply,
                    "turns_processed": ctx.turn,
                }),
            ))
            .await;

        // 12. Update status to Idle
        self.registry
            .set_active_session(agent_id, None)
            .await?;
        self.registry
            .set_status(agent_id, AgentStatus::Idle)
            .await?;

        // Publish agent:idle event to the agent's local bus
        let _ = self
            .publish_to_agent_bus(
                agent_id,
                Event::new(
                    "agent:harness",
                    EventType::Custom("agent:idle".to_owned()),
                    json!({
                        "agent_id": agent_id,
                        "session_id": session_id,
                    }),
                ),
            )
            .await;

        Ok(final_reply)
    }

    /// The core think-act-observe loop with M4 token budget management.
    async fn react_loop(
        &self,
        ctx: &mut ReActContext,
        token_budget: &mut crate::runtime::token_budget::TokenBudget,
        interrupt: Option<&InterruptFlag>,
    ) -> Result<ReactOutcome, Error> {
        let compressor = crate::runtime::history_compressor::HistoryCompressor::new(
            crate::runtime::history_compressor::CompressionStrategy::Truncate,
        );

        // Track skill body so we can re-inject scoring methodology later (Task #18).
        let mut loaded_skill_body: Option<String> = None;

        loop {
            // Check max turns
            if ctx.turn >= ctx.max_turns {
                return Err(Error::ConfigInvalid {
                    message: format!("max ReAct turns ({}) reached", ctx.max_turns),
                });
            }

            // Check interrupt (M6)
            if let Some(flag) = interrupt {
                if flag.is_interrupted() {
                    let _ = self
                        .publish_to_agent_bus(
                            &ctx.agent_id,
                            Event::new(
                                "agent:harness",
                                EventType::Custom("agent:reply_interrupted".to_owned()),
                                json!({
                                    "agent_id": ctx.agent_id,
                                    "session_id": ctx.session_id,
                                    "turn": ctx.turn,
                                }),
                            ),
                        )
                        .await;
                    return Ok(ReactOutcome::Interrupted(String::new()));
                }
            }

            // M4: Check token budget and compress history if needed
            if token_budget.needs_trim() {
                let result = compressor.compress(&mut ctx.history, token_budget, 3);
                if result.messages_removed > 0 {
                    let _ = self
                        .publish_to_agent_bus(
                            &ctx.agent_id,
                            Event::new(
                                "agent:harness",
                                EventType::Custom("agent:history_compressed".to_owned()),
                                json!({
                                    "agent_id": ctx.agent_id,
                                    "session_id": ctx.session_id,
                                    "turn": ctx.turn,
                                    "messages_removed": result.messages_removed,
                                    "tokens_saved": result.tokens_saved,
                                    "strategy": if result.strategy.is_truncate() { "truncate" } else { "summarize" },
                                }),
                            ),
                        )
                        .await;
                }
            }

            let turn_messages = ctx.history.clone();

            // Estimate history tokens for budget tracking
            let history_tokens: usize = ctx
                .history
                .iter()
                .map(|m| crate::runtime::token_budget::TokenBudget::estimate_tokens(&m.content))
                .sum();
            token_budget.set_history_tokens(history_tokens);

            // Execute one ReAct turn (T2.4: with streaming support)
            self.spawn_stream_forwarder(ctx);

            match self.engine.execute_turn(ctx, turn_messages).await {
                Ok(ReActTurn::Finished { ref content, .. }) => {
                    // Clear streaming callback so the consumer task drops
                    ctx.stream_cb = None;
                    ctx.history.push(ChatMessage::assistant(content.clone()));
                    // Record token usage
                    let completion_tokens =
                        crate::runtime::token_budget::TokenBudget::estimate_tokens(content);
                    token_budget.record_usage(history_tokens, completion_tokens);
                    return Ok(ReactOutcome::Finished(content.clone()));
                }
                Ok(ReActTurn::ToolCalls { content: tool_text, calls, reasoning_content }) => {
                    // Clear streaming callback (will be reset next iteration)
                    ctx.stream_cb = None;
                    // Publish agent:got_tool_calls to local bus
                    let _ = self
                        .publish_to_agent_bus(
                            &ctx.agent_id,
                            Event::new(
                                "agent:harness",
                                EventType::Custom("agent:got_tool_calls".to_owned()),
                                json!({
                                    "agent_id": ctx.agent_id,
                                    "session_id": ctx.session_id,
                                    "turn": ctx.turn,
                                    "tool_calls": calls.iter().map(|c| json!({"name": c.tool_name, "id": c.id})).collect::<Vec<_>>(),
                                }),
                            ),
                        )
                        .await;

                    // Record assistant message with tool calls in history
                    let formatted_calls = llm::format_tool_calls_for_history(&calls);
                    ctx.history.push(ChatMessage {
                        role: ChatMessageRole::Assistant,
                        content: tool_text,
                        tool_call_id: None,
                        tool_name: None,
                        tool_calls: Some(formatted_calls),
                        reasoning_content,
                    });

                    // Execute tools
                    let results = self.engine.execute_tools(ctx, &calls).await.map_err(|e| {
                        Error::ConfigInvalid {
                            message: format!("tool execution failed: {e}"),
                        }
                    })?;

                    // Publish agent:tool_results_fed_back to local bus
                    let _ = self
                        .publish_to_agent_bus(
                            &ctx.agent_id,
                            Event::new(
                                "agent:harness",
                                EventType::Custom("agent:tool_results_fed_back".to_owned()),
                                json!({
                                    "agent_id": ctx.agent_id,
                                    "session_id": ctx.session_id,
                                    "turn": ctx.turn,
                                    "result_count": results.len(),
                                }),
                            ),
                        )
                        .await;

                    // If read_skill was called, save the skill body for later format reminders.
                    // Extract before `results` is moved into `extend` below.
                    let has_read_skill = calls.iter().any(|c| c.tool_name == "read_skill");
                    if has_read_skill {
                        loaded_skill_body = calls.iter()
                            .position(|c| c.tool_name == "read_skill")
                            .and_then(|idx| results.get(idx))
                            .map(|r| r.content.clone());
                    }

                    ctx.history.extend(results);

                    // Inject activation note if read_skill was called
                    if has_read_skill {
                        if let Some(call) = calls.iter().find(|c| c.tool_name == "read_skill") {
                            let skill_name = call.args.get("skill")
                                .and_then(|v| v.as_str())
                                .unwrap_or("skill");
                            ctx.history.push(skill::formatting::build_read_skill_reinforcement(skill_name));
                        }
                    }

                    // If a skill was loaded in a previous turn (via read_skill) and this
                    // turn has finished gathering data, remind the LLM of the output format
                    // template before it produces the final report. After many tool calls
                    // the skill content drifts out of the LLM's immediate context window,
                    // causing the final output to lose the prescribed template structure.
                    if ctx.turn >= 1 && !calls.iter().any(|c| c.tool_name == "read_skill") {
                        let skill_was_loaded = ctx.history.iter().any(|m| {
                            m.tool_calls.as_ref().is_some_and(|tcs| {
                                tcs.iter().any(|tc| {
                                    tc.get("function")
                                        .and_then(|f| f.get("name"))
                                        .and_then(|n| n.as_str())
                                        == Some("read_skill")
                                })
                            })
                        });
                        if skill_was_loaded {
                            ctx.history.push(skill::formatting::build_format_reminder(loaded_skill_body.as_deref()));
                        }
                    }

                    // Increment turn
                    ctx.turn += 1;
                }
                Ok(ReActTurn::Error(react_err)) => {
                    let _ = self
                        .publish_to_agent_bus(
                            &ctx.agent_id,
                            Event::new(
                                "agent:harness",
                                EventType::Custom("llm_error".to_owned()),
                                json!({
                                    "agent_id": ctx.agent_id,
                                    "session_id": ctx.session_id,
                                    "turn": ctx.turn,
                                    "error": react_err.to_string(),
                                }),
                            ),
                        )
                        .await;
                    return Err(Error::ConfigInvalid {
                        message: format!("ReAct turn error at {}: {react_err}", ctx.turn),
                    });
                }
                Err(e) => {
                    // Publish llm:error to local bus
                    let _ = self
                        .publish_to_agent_bus(
                            &ctx.agent_id,
                            Event::new(
                                "agent:harness",
                                EventType::Custom("llm_error".to_owned()),
                                json!({
                                    "agent_id": ctx.agent_id,
                                    "session_id": ctx.session_id,
                                    "turn": ctx.turn,
                                    "error": e.to_string(),
                                }),
                            ),
                        )
                        .await;
                    return Err(Error::ConfigInvalid {
                        message: format!("ReAct loop error at turn {}: {e}", ctx.turn),
                    });
                }
            }
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

    /// Set up streaming for one ReAct turn: create an mpsc channel, attach it
    /// as the streaming callback on the context, and spawn a task that forwards
    /// each event to the event bus as `agent:reply_*` events.
    fn spawn_stream_forwarder(&self, ctx: &mut ReActContext) {
        let (stream_tx, mut stream_rx) = tokio::sync::mpsc::unbounded_channel::<StreamEvent>();
        {
            let tx = stream_tx.clone();
            ctx.stream_cb = Some(Arc::new(move |event| {
                let _ = tx.send(event);
            }) as Arc<dyn Fn(StreamEvent) + Send + Sync>);
        }
        // Stream events are agent-internal → publish to local bus.
        // The AgentHarness uses self.registry (AgentRegistry) to look up
        // the correct Local Bus for each agent. The closure captures
        // agent_id so it can route streaming events to the right bus.
        let aid = ctx.agent_id.clone();
        let sid = ctx.session_id.clone();
        let t = ctx.turn;
        let registry = Arc::clone(&self.registry);
        let global_bus = Arc::clone(&self.bus);
        tokio::spawn(async move {
            while let Some(event) = stream_rx.recv().await {
                let (etype, extra) = match &event {
                    StreamEvent::Start => ("agent:reply_stream_start", json!({})),
                    StreamEvent::Chunk(delta) => {
                        ("agent:reply_chunk", json!({"delta": delta}))
                    }
                    StreamEvent::Done { finish_reason } => {
                        ("agent:reply_stream_done", json!({"finish_reason": finish_reason}))
                    }
                    StreamEvent::Error(err) => {
                        ("agent:reply_stream_error", json!({"error": err}))
                    }
                };
                let e = Event::new(
                    "agent:harness",
                    EventType::Custom(etype.to_owned()),
                    json!({
                        "agent_id": aid,
                        "session_id": sid,
                        "turn": t,
                        "event_type": etype,
                        "extra": extra,
                    }),
                );
                match registry.get_local_bus(&aid).await {
                    Some(ref local_bus) => { let _ = local_bus.publish(e).await; }
                    None => { let _ = global_bus.publish(e).await; }
                }
            }
        });
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
        };
        let payload = serde_json::to_value(msg)?;
        self.bus.publish(Event::new(
            "agent:harness",
            EventType::AgentMessage,
            payload,
        )).await?;
        Ok(())
    }
}

/// Extract [remember: ...] commands from agent reply text.
///
/// Returns the cleaned text (with markers removed) and a list of
/// content strings to store as memories.
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
fn sanitize_api_keys(text: &str) -> String {
    // Match common API key patterns: "apiKey": "sk-...", "api_key": "sk-...",
    // "Authorization: Bearer sk-...", etc.
    // Use simple scanning to avoid pulling in a regex crate.
    let lower = text.to_lowercase();
    let mut result = text.to_owned();

    // Find sk- patterns and check if preceded by api key context
    let mut search_start = 0;
    while let Some(pos) = lower[search_start..].find("sk-") {
        let abs_pos = search_start + pos;
        // Look backward up to 40 chars for API key context keywords
        let ctx_start = abs_pos.saturating_sub(40);
        let ctx = &lower[ctx_start..abs_pos];
        let is_api_context = ctx.contains("apikey")
            || ctx.contains("api_key")
            || ctx.contains("api-key")
            || ctx.contains("bearer")
            || ctx.contains("authorization");

        if is_api_context {
            // Find the end of the key (alphanumeric + hyphens + underscores)
            let key_start = abs_pos;
            let mut key_end = key_start;
            for ch in text[key_start..].chars() {
                if ch.is_alphanumeric() || ch == '-' || ch == '_' {
                    key_end += ch.len_utf8();
                } else {
                    break;
                }
            }
            if key_end - key_start >= 20 {
                // It's long enough to be a real key — redact it
                result.replace_range(key_start..key_end, "[REDACTED]");
                // Adjust lower to match (simplification: just break after first redaction)
                break;
            }
        }

        search_start = abs_pos + 3; // skip past "sk-"
    }

    result
}
