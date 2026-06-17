// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use event_bus::EventBus;
use kernel::agent::{AgentInstance, AgentStatus, AgentSystemState};
use kernel::interrupt::InterruptFlag;
use context_manager::TokenBudgetPolicy;
use kernel::event::{Event, EventType};
use kernel::llm::{self, LlmChatRequest};
use kernel::prompt::PromptPipeline;
use kernel::react::{
    self, ChatMessage, ChatMessageRole, ParsedToolCall, ReActContext, ReActEngine as _, ReActTurn,
    SoulSnapshot, StreamEvent, ToolDescriptor,
};
use kernel::types::{ExecutionModel, SourceId};
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

/// Outcome of the ReAct loop.
#[derive(Debug)]
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

// ── Detached process helpers ──────────────────────────────────────────────

/// Captures the completion event from a detached process monitor thread.
///
/// The monitor publishes `tool:completed` (source `tool:detached`) when the
/// child process exits. This struct subscribes to that event on the agent's
/// local bus and provides a `wait()` method that blocks until the event
/// arrives or the caller is interrupted.
struct DetachCapture {
    result: Arc<Mutex<Option<Event>>>,
    notify: Arc<tokio::sync::Notify>,
}

impl DetachCapture {
    fn new() -> Self {
        Self {
            result: Arc::new(Mutex::new(None)),
            notify: Arc::new(tokio::sync::Notify::new()),
        }
    }

    /// Wait for the completion event, polling the interrupt flag every 200 ms.
    async fn wait(&self, interrupt_flag: Option<&InterruptFlag>, _pid: u32) -> Option<Event> {
        loop {
            // Check interrupt first
            if let Some(flag) = interrupt_flag
                && flag.is_interrupted()
            {
                return None;
            }

            // Check if result has arrived
            {
                let mut guard = self
                    .result
                    .lock()
                    .expect("DetachCapture lock poisoned");
                if let Some(event) = guard.take() {
                    return Some(event);
                }
            }

            // Wait with timeout so we can poll the interrupt flag
            tokio::select! {
                _ = self.notify.notified() => {
                    // Woken — loop back to check result
                    continue;
                }
                _ = tokio::time::sleep(Duration::from_millis(200)) => {
                    // Timeout — loop back to check interrupt
                    continue;
                }
            }
        }
    }
}

/// Event handler that captures a `tool:completed` event in a `DetachCapture`.
///
/// Uses `Arc` for the shared state because `EventHandler` requires `'static`.
struct DetachEventHandler {
    result: Arc<Mutex<Option<Event>>>,
    notify: Arc<tokio::sync::Notify>,
}

impl DetachEventHandler {
    fn new(capture: &DetachCapture) -> Self {
        Self {
            result: Arc::clone(&capture.result),
            notify: Arc::clone(&capture.notify),
        }
    }
}

#[async_trait::async_trait]
impl event_bus::EventHandler for DetachEventHandler {
    async fn handle(&self, event: Event) -> kernel::AmanResult<()> {
        let mut guard = self.result.lock().expect("DetachEventHandler lock poisoned");
        *guard = Some(event);
        self.notify.notify_one();
        Ok(())
    }
}

/// Kill a process by PID. SIGTERM first, then SIGKILL if it doesn't exit.
fn kill_process(pid: u32) {
    let pid_str = pid.to_string();
    // SIGTERM
    let _ = std::process::Command::new("kill")
        .arg("-TERM")
        .arg(&pid_str)
        .status();
    // Brief wait, then SIGKILL
    std::thread::sleep(Duration::from_millis(500));
    let _ = std::process::Command::new("kill")
        .arg("-KILL")
        .arg(&pid_str)
        .status();
}

// ── ToolExecutor ──────────────────────────────────────────────────────────

/// Wraps tool execution with permission checks and event publishing.
pub struct ToolExecutor {
    registry: Arc<ToolRegistry>,
    agent_registry: Arc<AgentRegistry>,
    bus: Arc<dyn EventBus>,
    /// Optional path/network/command allowlist config for the ReAct path.
    security_config: Option<ToolSecurityConfig>,
    /// Per-tool timeout (ms), sourced from `runtime.tool_timeout_sec` config.
    tool_timeout_ms: u64,
    /// Optional interrupt flag for interrupting detached process execution.
    interrupt_flag: Option<Arc<InterruptFlag>>,
}

impl ToolExecutor {
    pub fn new(
        registry: Arc<ToolRegistry>,
        agent_registry: Arc<AgentRegistry>,
        bus: Arc<dyn EventBus>,
        tool_timeout_ms: u64,
    ) -> Self {
        Self {
            registry,
            agent_registry,
            bus,
            security_config: None,
            tool_timeout_ms,
            interrupt_flag: None,
        }
    }

    /// Set a security config for path/network/command allowlist checks.
    #[must_use]
    #[allow(dead_code)]
    pub fn with_security_config(mut self, config: ToolSecurityConfig) -> Self {
        self.security_config = Some(config);
        self
    }

    /// Set an interrupt flag for cancelling long-running tool operations.
    #[must_use]
    pub fn with_interrupt_flag(mut self, flag: Arc<InterruptFlag>) -> Self {
        self.interrupt_flag = Some(flag);
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
                pending_detach: None,
            };
        }

        self.execute(call, agent_id, session_id).await
    }

    /// Publish an event to the agent's local bus, falling back to the global bus
    /// if the agent has no dedicated local bus.
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
        let (success, output, pending_detach) = match tool {
            Some(t) => {
                if let Some(reason) = hardline_blocked {
                    (false, format!("hardline_blocked: {reason}"), None)
                } else if let Some(ref reason) = config_blocked {
                    (false, format!("security_denied: {reason}"), None)
                } else {
                    // Reset consecutive read tracking when a non-read tool runs.
                    if tool_name != "read" {
                        tool::fs_tools::reset_read_tracker();
                    }

                    let mut ctx = kernel::context::ToolContext::default();
                    ctx.base.timeout_ms = Some(self.tool_timeout_ms);
                    // Wire the agent's local bus so tools can publish
                    // progress/completion events (e.g. exec in detach mode).
                    // The actual subscription for detach completion happens
                    // in execute_tools() below.
                    let monitor_bus: Arc<dyn EventBus> = self
                        .agent_registry
                        .get_local_bus(agent_id)
                        .await
                        .unwrap_or_else(|| Arc::clone(&self.bus));
                    ctx.base.event_bus =
                        Some(Arc::new(event_bus::BusEventPublisher::new(
                            Arc::clone(&monitor_bus),
                        )));
                    ctx.base
                        .extensions
                        .insert("agent_id".to_owned(), serde_json::json!(agent_id));
                    match t.execute(call.args.clone(), ctx).await {
                        Ok(value) => {
                            // Detect detach results — the exec tool returns
                            // {ok, pid, detached:true} immediately when
                            // spawning a background process. Mark as pending
                            // so the caller (execute_tools) waits for the
                            // real completion event before feeding to the LLM.
                            let pending_detach = value
                                .get("detached")
                                .and_then(|v| v.as_bool())
                                .unwrap_or(false)
                                .then(|| {
                                    value
                                        .get("pid")
                                        .and_then(|v| v.as_u64())
                                        .unwrap_or(0) as u32
                                });
                            (true, value.to_string(), pending_detach)
                        }
                        Err(e) => (false, format!("tool error: {e}"), None),
                    }
                }
            }
            None => (false, format!("tool not found: {tool_name}"), None),
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
            pending_detach,
        }
    }
}

/// Concrete ReAct engine that calls an LLM provider.
pub struct LlmReActEngine {
    tool_registry: Arc<ToolRegistry>,
    agent_registry: Arc<AgentRegistry>,
    bus: Arc<dyn EventBus>,
    /// Prompt pipeline for building system prompts.
    prompt_pipeline: Box<dyn PromptPipeline>,
    /// Per-tool timeout (ms), sourced from runtime.tool_timeout_sec.
    tool_timeout_ms: u64,
}

impl LlmReActEngine {
    pub fn new(
        tool_registry: Arc<ToolRegistry>,
        agent_registry: Arc<AgentRegistry>,
        bus: Arc<dyn EventBus>,
        prompt_pipeline: Box<dyn PromptPipeline>,
        tool_timeout_ms: u64,
    ) -> Self {
        Self {
            tool_registry,
            agent_registry,
            bus,
            prompt_pipeline,
            tool_timeout_ms,
        }
    }

    /// Publish an event to the agent's local bus, falling back to the global bus
    /// if the agent has no dedicated local bus.
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

    /// LLM API errors that are worth retrying (transient network/HTTP issues).
    fn is_retryable_llm_error(err: Option<&kernel::Error>) -> bool {
        let Some(e) = err else { return false };
        let msg = e.to_string().to_lowercase();
        // ── permanent errors — don't retry ──
        // HTTP 400 — bad request
        if msg.contains("400") || msg.contains("bad request") {
            return false;
        }
        // Auth errors
        if msg.contains("401") || msg.contains("403") {
            return false;
        }
        // HTTP 402 / billing / quota exhausted — not transient
        if msg.contains("402")
            || msg.contains("payment required")
            || msg.contains("insufficient_quota")
            || msg.contains("billing")
        {
            return false;
        }
        // ── transient — retry ──
        // 429 is only retryable for rate-limiting; insufficient_quota is
        // caught by the check above and won't reach here.
        msg.contains("error sending request")
            || msg.contains("timeout")
            || msg.contains("connection")
            || msg.contains("429")
            || msg.contains("500")
            || msg.contains("502")
            || msg.contains("503")
            || msg.contains("504")
            // reqwest stream/decode errors — transient body corruption
            || msg.contains("error decoding response body")
            || msg.contains("error decoding chunk")
            || msg.contains("stream closed")
            || msg.contains("connection closed")
            || msg.contains("unexpected eof")
    }

    /// Returns true if the error looks transient (worth retrying).
    /// Permanent errors like "unrecoverable", "not found", "no such file"
    /// should NOT be retried.
    fn is_retryable_error(output: &str) -> bool {
        let lower = output.to_lowercase();
        // Permanent failures — skip retry
        if lower.contains("unrecoverable") {
            return false;
        }
        if lower.contains("no such file") || lower.contains("not found") {
            return false;
        }
        if lower.contains("permission denied") || lower.contains("not allowed") {
            return false;
        }
        if lower.contains("invalid") && lower.contains("configuration") {
            return false;
        }
        // Transient failures — worth retrying
        lower.contains("timeout")
            || lower.contains("connection")
            || lower.contains("refused")
            || lower.contains("reset")
            || lower.contains("temporary")
            || lower.contains("rate limit")
            || lower.contains("too many requests")
            || lower.contains("error sending request")
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
        let model = ctx.model.clone();
        let tools = ctx.agent_tools.clone();
        let max_tokens = ctx.token_budget.max_output_tokens as u32;

        let Some(llm_provider) = self.agent_registry.get_llm_provider(&ctx.agent_id).await else {
            return Err(kernel::react::ReActError::LlmError(
                format!("no LLM provider configured for agent '{}'", ctx.agent_id)
            ));
        };

        // Retry LLM API calls with exponential backoff: 1s, 3s, 9s, 27s, 81s
        const LLM_MAX_RETRIES: u32 = 5;
        let mut llm_attempt = 0;
        let result = loop {
            llm_attempt += 1;
            let req = LlmChatRequest {
                model: model.clone(),
                system_prompt: system_prompt.clone(),
                messages: messages.clone(),
                tools: tools.clone(),
                max_output_tokens: max_tokens,
                response_format: None,
            };
            let r = llm_provider.chat_completion(req, cb.clone()).await;
            let should_retry = r.is_err()
                && llm_attempt < LLM_MAX_RETRIES
                && Self::is_retryable_llm_error(r.as_ref().err());
            if !should_retry {
                break r;
            }
            // Exponential backoff: 1s, 3s, 9s, 27s, 81s (capped at 120s)
            let delay_secs = (3_u64.pow(llm_attempt - 1)).min(120);
            tracing::warn!(
                agent_id = %ctx.agent_id,
                session_id = %ctx.session_id,
                turn = ctx.turn,
                attempt = llm_attempt,
                delay_secs,
                error = %r.as_ref().err().map(|e| e.to_string()).unwrap_or_default(),
                "LLM API call failed, retrying"
            );
            tokio::time::sleep(std::time::Duration::from_secs(delay_secs)).await;
        };

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
        block_on_detach: bool,
    ) -> Result<kernel::react::ToolExecutionResult, kernel::react::ReActError> {
        let mut executor_builder = ToolExecutor::new(
            Arc::clone(&self.tool_registry),
            Arc::clone(&self.agent_registry),
            Arc::clone(&self.bus),
            self.tool_timeout_ms,
        );
        // Pass interrupt flag so detached processes can be cancelled
        if let Some(ref flag) = ctx.interrupt_flag {
            executor_builder = executor_builder.with_interrupt_flag(Arc::clone(flag));
        }
        let executor = Arc::new(executor_builder);

        const TOOL_MAX_RETRIES: u32 = 3;
        const TOOL_RETRY_DELAY_SECS: u64 = 1;

        // ── Classify calls by execution model ──────────────────────────
        // (original_index, is_independent)
        let models: Vec<(usize, ExecutionModel)> = calls
            .iter()
            .enumerate()
            .map(|(i, call)| {
                let model = self
                    .tool_registry
                    .get(&call.tool_name)
                    .map(|t| t.execution_model())
                    .unwrap_or_default();
                (i, model)
            })
            .collect();

        // ── Phase 1: Launch all Independent calls concurrently ─────────
        let mut independent_handles: Vec<(usize, tokio::task::JoinHandle<react::ToolCallResult>)> =
            Vec::new();

        for (i, call) in calls.iter().enumerate() {
            if models.iter().any(|(idx, m)| *idx == i && *m == ExecutionModel::Independent) {
                let exec = Arc::clone(&executor);
                let agent_id = ctx.agent_id.clone();
                let session_id = ctx.session_id.clone();
                let call = call.clone();
                let handle = tokio::spawn(async move {
                    let mut attempt = 0;
                    loop {
                        attempt += 1;
                        let r = exec.execute_for_agent(&call, &agent_id, &session_id).await;
                        if r.success
                            || attempt >= TOOL_MAX_RETRIES
                            || !Self::is_retryable_error(&r.output)
                        {
                            return r;
                        }
                        tracing::warn!(
                            agent_id = %agent_id,
                            session_id = %session_id,
                            tool = %call.tool_name,
                            attempt,
                            error = %r.output,
                            "tool call failed, retrying"
                        );
                        tokio::time::sleep(std::time::Duration::from_secs(TOOL_RETRY_DELAY_SECS))
                            .await;
                    }
                });
                independent_handles.push((i, handle));
            }
        }

        // ── Phase 2: Execute Stateful/SideEffect calls sequentially ────
        let mut serial_results: Vec<(usize, react::ToolCallResult)> = Vec::new();

        for (i, call) in calls.iter().enumerate() {
            let is_independent = models
                .iter()
                .any(|(idx, m)| *idx == i && *m == ExecutionModel::Independent);
            if is_independent {
                continue;
            }

            let mut attempt = 0;
            let result = loop {
                attempt += 1;
                let r = executor
                    .execute_for_agent(call, &ctx.agent_id, &ctx.session_id)
                    .await;
                if r.success
                    || attempt >= TOOL_MAX_RETRIES
                    || !Self::is_retryable_error(&r.output)
                {
                    break r;
                }
                tracing::warn!(
                    agent_id = %ctx.agent_id,
                    session_id = %ctx.session_id,
                    tool = %call.tool_name,
                    attempt,
                    error = %r.output,
                    "tool call failed, retrying"
                );
                tokio::time::sleep(std::time::Duration::from_secs(TOOL_RETRY_DELAY_SECS)).await;
            };
            serial_results.push((i, result));
        }

        // ── Await all independent futures ──────────────────────────────
        let mut independent_results: Vec<(usize, react::ToolCallResult)> = Vec::new();
        for (i, handle) in independent_handles {
            match handle.await {
                Ok(result) => independent_results.push((i, result)),
                Err(join_err) => independent_results.push((
                    i,
                    react::ToolCallResult {
                        id: String::new(),
                        tool_name: String::new(),
                        success: false,
                        output: format!("tool task panicked or was cancelled: {join_err}"),
                        duration_ms: 0,
                        pending_detach: None,
                    },
                )),
            }
        }

        // ── Merge results in original call order ───────────────────────
        let mut all: Vec<(usize, react::ToolCallResult)> = Vec::with_capacity(calls.len());
        all.extend(independent_results);
        all.extend(serial_results);
        all.sort_by_key(|(i, _)| *i);

        // ── Wait for any detached processes to complete ───────────────
        // Detach results carry pending_detach = Some(pid).
        // When block_on_detach is true (react_loop): subscribe to the
        //   tool:completed event and block until the process exits.
        // When block_on_detach is false (direct_act): skip the wait,
        //   publish agent:awaiting_detach, and return the pending info
        //   so the caller can continue asynchronously.
        let mut pending_detach: Option<(u32, String)> = None;
        for (_, result) in &mut all {
            let Some(pid) = result.pending_detach else {
                continue;
            };

            if !block_on_detach {
                // Non-blocking: record pending info and keep spawn result.
                // Publish to local bus only — process_message publishes
                // a more complete version to the global bus (with
                // skill_name / background context).
                pending_detach = Some((pid, result.id.clone()));
                let _ = self
                    .publish_to_agent_bus(
                        &ctx.agent_id,
                        Event::new(
                            "agent:harness",
                            EventType::Custom("agent:awaiting_detach".to_owned()),
                            json!({
                                "agent_id": ctx.agent_id,
                                "session_id": ctx.session_id,
                                "tool_call_id": result.id,
                                "tool_name": result.tool_name,
                                "pid": pid,
                            }),
                        ),
                    )
                    .await;
                continue;
            }

            // ── Blocking path (react_loop) ────────────────────────

            // Publish awaiting event so the UI knows the session is alive
            let _ = self
                .publish_to_agent_bus(
                    &ctx.agent_id,
                    Event::new(
                        "agent:harness",
                        EventType::Custom("agent:awaiting_detach".to_owned()),
                        json!({
                            "agent_id": ctx.agent_id,
                            "session_id": ctx.session_id,
                            "tool_call_id": result.id,
                            "tool_name": result.tool_name,
                            "pid": pid,
                        }),
                    ),
                )
                .await;

            // Get agent's local bus for the completion subscription
            let monitor_bus: Arc<dyn EventBus> = self
                .agent_registry
                .get_local_bus(&ctx.agent_id)
                .await
                .unwrap_or_else(|| Arc::clone(&self.bus));

            let capture = Arc::new(DetachCapture::new());
            let sub_filter = event_bus::SubscriptionFilter {
                event_types: Some(vec![
                    EventType::Custom("tool:completed".to_owned()),
                ]),
                sources: Some(vec![
                    SourceId::from("tool:detached"),
                ]),
                ..Default::default()
            };
            let sub_id = match monitor_bus
                .subscribe(sub_filter, Box::new(DetachEventHandler::new(&capture)))
                .await
            {
                Ok(id) => id,
                Err(e) => {
                    tracing::warn!(
                        %pid,
                        error = %e,
                        "failed to subscribe for detach completion; keeping spawn result"
                    );
                    continue;
                }
            };

            // Wait for process exit or interrupt
            let interrupt_ref = ctx.interrupt_flag.as_deref();
            let result_event = capture.wait(interrupt_ref, pid).await;

            // Clean up subscription
            monitor_bus.unsubscribe(sub_id).await;

            match result_event {
                Some(event) => {
                    let p = &event.payload;
                    let real_success =
                        p["success"].as_bool().unwrap_or(false);
                    let exit_code =
                        p["exit_code"].as_i64().unwrap_or(-1);
                    let stdout =
                        p["stdout"].as_str().unwrap_or("");
                    let stderr =
                        p["stderr"].as_str().unwrap_or("");

                    result.success = real_success;
                    if real_success {
                        result.output = format!(
                            "Process exited with code {exit_code}\nstdout:\n{stdout}"
                        );
                    } else {
                        result.output = format!(
                            "Process exited with code {exit_code}\nstdout:\n{stdout}\nstderr:\n{stderr}"
                        );
                    }
                }
                None => {
                    // Interrupted — kill the process
                    kill_process(pid);
                    result.success = false;
                    result.output = format!("Process (PID {pid}) was interrupted and terminated");
                }
            }
            result.pending_detach = None;
        }

        let messages: Vec<ChatMessage> = all
            .into_iter()
            .map(|(_, result)| {
                ChatMessage::tool_result(&result.id, &result.tool_name, &result.output)
            })
            .collect();

        Ok(kernel::react::ToolExecutionResult {
            messages,
            pending_detach,
        })
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
    /// Compression configuration.
    compression_config: context_manager::CompressorConfig,
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
        prompt_pipeline: Box<dyn PromptPipeline>,
        session_history: Box<dyn SessionHistoryStore>,
        budget_policy: Box<dyn TokenBudgetPolicy>,
        agent_router: Box<dyn AgentRouter>,
        compression_config: context_manager::CompressorConfig,
        tool_timeout_ms: u64,
        runtime: tokio::runtime::Handle,
    ) -> Self {
        let engine = LlmReActEngine::new(
            Arc::clone(&tool_registry),
            Arc::clone(&registry),
            Arc::clone(&bus),
            prompt_pipeline,
            tool_timeout_ms,
        );
        Self {
            registry,
            tool_registry,
            engine,
            bus,
            session_history,
            active_interrupts: RwLock::new(HashMap::new()),
            max_react_turns: DEFAULT_MAX_REACT_TURNS,
            budget_policy,
            agent_router,
            compression_config,
            runtime,
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
        skill_name: Option<String>,
        react_mode: Option<skill::ReactMode>,
        background: bool,
    ) -> tokio::task::JoinHandle<()> {
        let harness = Arc::clone(self);
        self.runtime.spawn(async move {
            if let Err(e) = harness
                .process_message(&agent_id, &session_id, &user_text, &model, soul_snapshot, skill_name.as_deref(), react_mode, background)
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
        self: &Arc<Self>,
        agent_id: &str,
        session_id: &str,
        user_text: &str,
        model: &str,
        soul_snapshot: SoulSnapshot,
        skill_name: Option<&str>,
        react_mode: Option<skill::ReactMode>,
        background: bool,
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
        // Pick the right system state for the UI:
        // - Work-item sessions (kanban Act! / startup / idle_run with work tag) → Working
        // - Foreground user messages → Chatting
        // - Background boredom runs → already set by boredom actor, leave as-is
        let is_work_session = super::session::work_session::is_plugin_work_session(session_id);
        if is_work_session {
            self.registry.set_system_state(agent_id, AgentSystemState::Working).await;
        } else if !background {
            self.registry.set_system_state(agent_id, AgentSystemState::Chatting).await;
        }

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

        // For work-item sessions, if in-memory history is empty (e.g. after
        // gateway restart), restore from the persisted JSONL so the agent
        // can resume ("断点续传") instead of starting from scratch.
        if history.is_empty()
            && super::session::work_session::parse_work_session_id(session_id).is_some()
        {
            if let Some(store) = self.registry.get_session_store(agent_id).await {
                let _ = super::session::work_session::resume_work_session(
                    self, &store, session_id, 0,
                ).await;
                // Reload after restore
                history = self.session_history.get(session_id);
            }
        }

        history.push(ChatMessage::user(user_text));

        // 5. Initialize model-aware token budget (M4)
        // Estimate history tokens immediately so the budget reflects the full
        // session history before the first react_loop iteration. Without this,
        // needs_trim() returns false on the first pass regardless of history
        // size, and the full context is sent to the LLM unfiltered.
        // Values must come from config, never silently defaulted.
        let mut token_budget = match (instance.descriptor.max_context_tokens, instance.descriptor.max_output_tokens) {
            (Some(ctx), Some(out)) => {
                context_manager::TokenBudget::with_window(model, ctx, out)
            }
            (Some(ctx), None) => {
                context_manager::TokenBudget::with_window(model, ctx, self.budget_policy.max_output_tokens(model, instance.descriptor.max_output_tokens))
            }
            _ => {
                let ctx = self.budget_policy.context_window(model);
                let out = self.budget_policy.max_output_tokens(model, None);
                context_manager::TokenBudget::with_window(model, ctx, out)
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
        token_budget.set_system_tokens(context_manager::TokenBudget::estimate_tokens(&soul_snapshot.system_prompt));
        // Estimate tool schema tokens
        let tool_schema_text: String = available_tools
            .iter()
            .map(|t| format!("{}: {}", t.name, t.parameters))
            .collect::<Vec<_>>()
            .join("\n");
        token_budget.set_tool_schema_tokens(context_manager::TokenBudget::estimate_tokens(&tool_schema_text));
        // Estimate history tokens from loaded session history so the budget
        // check in react_loop triggers compression before the first LLM call.
        let initial_history_tokens: usize = history
            .iter()
            .map(|m| context_manager::TokenBudget::estimate_tokens(&m.content))
            .sum();
        token_budget.set_history_tokens(initial_history_tokens);

        // 6. Retrieve memories relevant to user input (M5 T5.1)
        let memory_results = match self.registry.get_memory_provider(agent_id).await {
            Some(provider) => provider.recall(agent_id, user_text, 10).await,
            None => Vec::new(),
        };
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
            soul_snapshot.clone(),
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
        // Also attach to ReActContext so tool executors can cancel
        // long-running detached processes on /stop.
        ctx.interrupt_flag = Some(Arc::clone(&interrupt_flag));

        // 8. Execute — route to DirectAct or ReAct loop based on skill mode
        let result = if react_mode == Some(skill::ReactMode::Direct) {
            let _ = self
                .publish_to_agent_bus(
                    agent_id,
                    Event::new(
                        "agent:harness",
                        EventType::Custom("agent:direct_act_started".to_owned()),
                        json!({
                            "agent_id": agent_id,
                            "session_id": session_id,
                            "skill_name": skill_name,
                        }),
                    ),
                )
                .await;
            self.direct_act(&mut ctx, &mut token_budget, Some(&interrupt_flag))
                .await
        } else {
            self.react_loop(&mut ctx, &mut token_budget, Some(&interrupt_flag), background)
                .await
        };

        // ── Handle AwaitingDetach (non-blocking detach path) ──────────
        // Must be checked BEFORE saving history / unregistering interrupt,
        // because the continuation task needs both to survive the async gap.
        if let Ok(ReactOutcome::AwaitingDetach {
            session_id: ref detach_sid,
            pid,
            tool_call_id: ref tcid,
        }) = result
        {
            // Save Turn 1 history for cross-turn continuity
            self.session_history.clear(session_id);
            self.session_history.extend(session_id, ctx.history.clone());

            // Publish agent:awaiting_detach to GLOBAL bus for SSE → frontend
            let _ = self
                .bus
                .publish(Event::new(
                    "agent:harness",
                    EventType::Custom("agent:awaiting_detach".to_owned()),
                    json!({
                        "agent_id": agent_id,
                        "session_id": detach_sid,
                        "pid": pid,
                        "tool_call_id": tcid,
                        "skill_name": skill_name,
                        "background": background,
                    }),
                ))
                .await;

            // Spawn continuation — runs Turn 2 after detach completes
            let harness = Arc::clone(self);
            let cont_aid = agent_id.to_owned();
            let cont_sid = detach_sid.clone();
            let cont_tcid = tcid.clone();
            let cont_soul = soul_snapshot.clone();
            let cont_tools = ctx.agent_tools.clone();
            let cont_model = model.to_owned();
            let cont_max_out = ctx.token_budget.max_output_tokens;
            let cont_hist = ctx.history.clone();
            let cont_turn = ctx.turn;
            let cont_flag = Arc::clone(&interrupt_flag);
            let cont_bg = background;
            let cont_sn = skill_name.map(String::from);

            tokio::spawn(async move {
                harness
                    .run_direct_act_continuation(
                        cont_aid,
                        cont_sid,
                        pid,
                        cont_tcid,
                        cont_soul,
                        cont_tools,
                        cont_model,
                        cont_max_out,
                        cont_hist,
                        cont_turn,
                        cont_flag,
                        cont_bg,
                        cont_sn,
                    )
                    .await;
            });

            // Don't go idle — the continuation task handles cleanup.
            // Keep status as Busy so the idle detector doesn't kick in
            // while the detached process is still running. system_state
            // shows Waiting so the UI can reflect the detach-pending state.
            let _ = self.registry.set_active_session(agent_id, None).await;
            self.registry.set_system_state(agent_id, AgentSystemState::Waiting).await;
            // Reset the idle signal so the boredom timer doesn't fire
            // during the detach wait (which can last several minutes).
            if let Some(coord) = self.registry.get_idle_coordination(agent_id).await {
                coord.reset_idle_signal().await;
            }
            return Ok(String::new());
        }

        // Save conversation history for cross-turn continuity.
        self.session_history.clear(session_id);
        self.session_history.extend(session_id, ctx.history.clone());

        // M6: Unregister interrupt flag
        self.unregister_interrupt(session_id);

        // 9. Handle outcome — Finished, Interrupted, MaxTurnsReached, or Error
        let mut max_turns_reached = false;
        let (raw_reply, event_type): (String, &str) = match result {
            Ok(ReactOutcome::Finished(reply)) => (reply, "agent:reply_ready"),
            Ok(ReactOutcome::Interrupted(reply)) => (reply, "agent:reply_interrupted"),
            Ok(ReactOutcome::MaxTurnsReached { turns }) => {
                max_turns_reached = true;
                let msg = format!(
                    "[max {} turns reached — session saved, send /continue to resume]",
                    turns
                );
                (msg, "agent:reply_ready")
            }
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
                // Still go to Idle on error — reset both status and system_state
                // so the agent doesn't get stuck in Chatting/Working.
                let _ = self.registry.set_active_session(agent_id, None).await;
                let _ = self.registry.set_status(agent_id, AgentStatus::Idle).await;
                self.registry.set_system_state(agent_id, AgentSystemState::Idle).await;
                self.registry.set_activity(agent_id, "").await;
                return Err(e);
            }
            _ => {
                // AwaitingDetach is handled above — this arm is unreachable
                // but required for exhaustiveness.
                unreachable!("AwaitingDetach handled before this point");
            }
        };

        // 10. Auto-write memories from [remember: ...] patterns (M5 T5.2)
        let (final_reply, remembered) = process_remember_commands(&raw_reply);
        for content in &remembered {
            if let Some(provider) = self.registry.get_memory_provider(agent_id).await {
                provider.store(agent_id, content, vec!["auto".to_owned()]);
            }
        }

        // Sanitize API key patterns from output before publishing.
        // LLMs may hallucinate apiKey patterns when exposed to tool schemas
        // or skill documentation that references API-based services.
        let final_reply = sanitize_api_keys(&final_reply);

        // 11. Publish reply event
        let mut reply_payload = json!({
            "agent_id": agent_id,
            "session_id": session_id,
            "reply": final_reply,
            "turns_processed": ctx.turn,
            "background": background,
        });
        if let Some(sn) = skill_name {
            reply_payload["skill_name"] = json!(sn);
        }
        let _ = self
            .bus
            .publish(Event::new(
                "agent:harness",
                EventType::Custom(event_type.to_owned()),
                reply_payload,
            ))
            .await;

        // 12. Update status to Idle (skip for MaxTurnsReached — keep session alive)
        if max_turns_reached {
            // Session stays active; agent stays Busy; user can /continue.
            // Publish a distinct event so the UI can show "turn limit reached".
            let _ = self
                .publish_to_agent_bus(
                    agent_id,
                    Event::new(
                        "agent:harness",
                        EventType::Custom("agent:max_turns_reached".to_owned()),
                        json!({
                            "agent_id": agent_id,
                            "session_id": session_id,
                        }),
                    ),
                )
                .await;
        } else {
            self.registry
                .set_active_session(agent_id, None)
                .await?;
            self.registry
                .set_status(agent_id, AgentStatus::Idle)
                .await?;
            self.registry.set_system_state(agent_id, AgentSystemState::Idle).await;

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
        }

        Ok(final_reply)
    }

    /// Direct execution mode for skills that only invoke a fixed script/API.
    ///
    /// Unlike the full ReAct loop, this runs exactly 2 turns:
    /// 1. LLM reads the methodology → outputs tool calls (no reasoning/search)
    /// 2. Tools execute → LLM reports results
    ///
    /// When a tool spawns a detached process, Turn 1 returns immediately with
    /// `AwaitingDetach`; a continuation task runs Turn 2 after the process exits.
    /// No multi-turn exploration, no compression, no token budget tracking.
    async fn direct_act(
        &self,
        ctx: &mut ReActContext,
        token_budget: &mut context_manager::TokenBudget,
        interrupt: Option<&InterruptFlag>,
    ) -> Result<ReactOutcome, Error> {
        // Turn 1: LLM parses the methodology and outputs tool calls.
        // The methodology is already in ctx.history from the user message.
        let turn_messages = ctx.history.clone();
        let stream_handle = self.spawn_stream_forwarder(ctx);

        let turn1 = match self.engine.execute_turn(ctx, turn_messages).await {
            Ok(t) => t,
            Err(e) => {
                ctx.stream_cb = None;
                let _ = stream_handle.await;
                return Err(Error::ConfigInvalid {
                    message: format!("direct act tool-selection failed: {e}"),
                });
            }
        };
        match turn1 {
            ReActTurn::ToolCalls { content, calls, reasoning_content } => {
                ctx.stream_cb = None;
                // Wait for the forwarder to drain so reply_stream_done
                // is published before tool execution and Turn 2.
                let _ = stream_handle.await;

                // Record the assistant message with tool calls
                let formatted_calls = llm::format_tool_calls_for_history(&calls);
                ctx.history.push(ChatMessage {
                    role: ChatMessageRole::Assistant,
                    content,
                    tool_call_id: None,
                    tool_name: None,
                    tool_calls: Some(formatted_calls),
                    reasoning_content,
                });

                // Publish got_tool_calls for UI consistency
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

                // Execute tools — non-blocking for detach (direct_act)
                let exec_result = self.engine.execute_tools(ctx, &calls, false).await.map_err(|e| {
                    Error::ConfigInvalid {
                        message: format!("tool execution failed: {e}"),
                    }
                })?;

                ctx.history.extend(exec_result.messages);
                ctx.turn += 1;

                // If a tool spawned a detached process, return early —
                // the caller spawns a continuation to run Turn 2 later.
                if let Some((pid, tool_call_id)) = exec_result.pending_detach {
                    return Ok(ReactOutcome::AwaitingDetach {
                        session_id: ctx.session_id.clone(),
                        pid,
                        tool_call_id,
                    });
                }

                // No detach — continue with shared ReAct loop for remaining turns
                self.react_loop(ctx, token_budget, interrupt, false).await
            }
            ReActTurn::Finished { content, .. } => {
                // LLM chose not to use any tools — just return its response.
                ctx.stream_cb = None;
                let _ = stream_handle.await;
                ctx.history.push(ChatMessage::assistant(content.clone()));
                Ok(ReactOutcome::Finished(content))
            }
            ReActTurn::Error(e) => {
                ctx.stream_cb = None;
                let _ = stream_handle.await;
                Err(Error::ConfigInvalid {
                    message: format!("direct act failed: {e}"),
                })
            },
        }
    }

    /// Run the Turn 2 LLM report phase for `direct_act`.
    ///
    /// Assumes `ctx.history` already contains Turn 1 assistant message and
    /// tool results.  Calls the LLM to produce a human-readable summary.
    /// Wait for a detached process to complete, returning the
    /// `tool:completed` event (or `None` if interrupted).
    async fn wait_for_detach(
        &self,
        agent_id: &str,
        pid: u32,
        interrupt_flag: &InterruptFlag,
    ) -> Option<Event> {
        let monitor_bus: Arc<dyn EventBus> = self
            .registry
            .get_local_bus(agent_id)
            .await
            .unwrap_or_else(|| Arc::clone(&self.bus));

        let capture = Arc::new(DetachCapture::new());
        let sub_filter = event_bus::SubscriptionFilter {
            event_types: Some(vec![EventType::Custom("tool:completed".to_owned())]),
            sources: Some(vec![SourceId::from("tool:detached")]),
            ..Default::default()
        };
        let sub_id = match monitor_bus
            .subscribe(sub_filter, Box::new(DetachEventHandler::new(&capture)))
            .await
        {
            Ok(id) => id,
            Err(e) => {
                tracing::warn!(
                    %pid,
                    error = %e,
                    "wait_for_detach: failed to subscribe"
                );
                return None;
            }
        };

        let result = capture.wait(Some(interrupt_flag), pid).await;
        monitor_bus.unsubscribe(sub_id).await;
        result
    }

    /// Replace the tool result for `tool_call_id` in the conversation history
    /// with the final output (after process exit).
    fn replace_tool_result(
        &self,
        mut history: Vec<ChatMessage>,
        tool_call_id: &str,
        final_output: &str,
    ) -> Vec<ChatMessage> {
        for msg in &mut history {
            if msg.tool_call_id.as_deref() == Some(tool_call_id)
                && msg.role == ChatMessageRole::Tool
            {
                msg.content = final_output.to_owned();
                break;
            }
        }
        history
    }

    /// Publish final reply and set agent to idle.
    async fn cleanup_session(&self, agent_id: &str, session_id: &str) {
        self.unregister_interrupt(session_id);
        let _ = self.registry.set_active_session(agent_id, None).await;
        let _ = self.registry.set_status(agent_id, AgentStatus::Idle).await;
        self.registry.set_system_state(agent_id, AgentSystemState::Idle).await;
        self.registry.set_activity(agent_id, "").await;
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
    }

    /// Continuation for `direct_act` after a detached process completes.
    ///
    /// Spawned by `process_message` when `direct_act` returns `AwaitingDetach`.
    /// Waits for the process, updates the tool result in history, runs Turn 2,
    /// and publishes the final reply.
    async fn run_direct_act_continuation(
        self: Arc<Self>,
        agent_id: String,
        session_id: String,
        pid: u32,
        tool_call_id: String,
        soul_snapshot: SoulSnapshot,
        agent_tools: Vec<ToolDescriptor>,
        model: String,
        max_output_tokens: u64,
        history: Vec<ChatMessage>,
        turn: u32,
        interrupt_flag: Arc<InterruptFlag>,
        background: bool,
        skill_name: Option<String>,
    ) {
        // 1. Wait for detach completion
        let result_event = self
            .wait_for_detach(&agent_id, pid, &interrupt_flag)
            .await;

        // 2. Update the tool result in history
        let final_output;
        let hook_stdout;
        let hook_exit_code;
        let hook_success;
        match result_event {
            Some(ref event) => {
                let p = &event.payload;
                let success = p["success"].as_bool().unwrap_or(false);
                let exit_code = p["exit_code"].as_i64().unwrap_or(-1);
                let stdout = p["stdout"].as_str().unwrap_or("");
                let stderr = p["stderr"].as_str().unwrap_or("");
                hook_stdout = stdout.to_owned();
                hook_exit_code = exit_code;
                hook_success = success;
                if success {
                    final_output = format!("Process exited with code {exit_code}\nstdout:\n{stdout}");
                } else {
                    final_output = format!(
                        "Process exited with code {exit_code}\nstdout:\n{stdout}\nstderr:\n{stderr}"
                    );
                }
            }
            None => {
                kill_process(pid);
                hook_stdout = String::new();
                hook_exit_code = -1;
                hook_success = false;
                final_output = format!("Process (PID {pid}) was interrupted and terminated");
            }
        };

        // Publish skill:completed event so subscribers can react
        // to any skill's background task completion.
        if let Some(ref sn) = skill_name {
            let message = if hook_success {
                let kb = hook_stdout.len() / 1024;
                format!("{sn} completed — exit 0, {kb} KB output")
            } else if hook_stdout.is_empty() {
                format!("{sn} was interrupted (PID {pid})")
            } else {
                format!("{sn} failed — exit {hook_exit_code}")
            };
            let _ = self.bus.publish(Event::new(
                "agent:harness",
                EventType::Custom("skill:completed".to_owned()),
                json!({
                    "agent_id": agent_id,
                    "session_id": session_id,
                    "skill_name": sn,
                    "pid": pid,
                    "success": hook_success,
                    "exit_code": hook_exit_code,
                    "stdout": hook_stdout,
                    "message": message,
                }),
            )).await;
        }

        let history =
            self.replace_tool_result(history, &tool_call_id, &final_output);

        // 3. Rebuild ReActContext and run remaining turns via shared loop
        let ctx_model = model.clone();
        let mut token_budget = context_manager::TokenBudget::new(ctx_model.clone());
        let mut ctx = ReActContext::new(
            agent_id.clone(),
            session_id.clone(),
            soul_snapshot,
            history,
            agent_tools,
            ctx_model,
            self.max_react_turns,
            self.budget_policy.session_token_limit(),
            max_output_tokens,
        );
        ctx.turn = turn;
        let reply = match self.react_loop(&mut ctx, &mut token_budget, None, false).await {
            Ok(ReactOutcome::Finished(reply)) => reply,
            Ok(ReactOutcome::Interrupted(reply)) => {
                reply
            }
            Ok(ReactOutcome::MaxTurnsReached { turns }) => {
                format!("[max {} turns reached again — session saved]", turns)
            }
            Ok(ReactOutcome::AwaitingDetach { .. }) => {
                // Nested detach — unexpected, treat as error
                tracing::error!(
                    agent_id = %agent_id,
                    session_id = %session_id,
                    "nested AwaitingDetach in continuation — this is unexpected"
                );
                String::new()
            }
            Err(e) => {
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
                self.cleanup_session(&agent_id, &session_id).await;
                return;
            }
        };

        // 4. Sanitize and remember
        let (final_reply, remembered) = process_remember_commands(&reply);
        for content in &remembered {
            if let Some(provider) = self.registry.get_memory_provider(&agent_id).await {
                provider.store(&agent_id, content, vec!["auto".to_owned()]);
            }
        }
        let final_reply = sanitize_api_keys(&final_reply);

        // 5. Save final history
        self.session_history.clear(&session_id);
        self.session_history.extend(&session_id, ctx.history);

        // 6. Publish agent:reply_ready to global bus
        let mut reply_payload = json!({
            "agent_id": agent_id,
            "session_id": session_id,
            "reply": final_reply,
            "turns_processed": ctx.turn,
            "background": background,
        });
        if let Some(ref sn) = skill_name {
            reply_payload["skill_name"] = json!(sn);
        }
        let _ = self
            .bus
            .publish(Event::new(
                "agent:harness",
                EventType::Custom("agent:reply_ready".to_owned()),
                reply_payload,
            ))
            .await;

        // 7. Clean up and go idle
        self.cleanup_session(&agent_id, &session_id).await;
    }

    /// The core think-act-observe loop with M4 token budget management.
    /// Shared core: process one ReAct turn (LLM → tools → results).
    ///
    /// Returns `Ok(true)` if the caller should continue looping (tools executed,
    /// results added to history), `Ok(false)` if a final reply was produced,
    /// or `Err` on failure.
    ///
    /// Used by both [`react_loop`] and [`direct_act`] — after skill selection
    /// the logic is identical.
    async fn process_react_turn(
        &self,
        ctx: &mut ReActContext,
        token_budget: &mut context_manager::TokenBudget,
        loaded_skill_body: &mut Option<String>,
    ) -> Result<bool, Error> {
        let turn_messages = ctx.history.clone();
        let stream_handle = self.spawn_stream_forwarder(ctx);

        self.registry.set_activity(&ctx.agent_id, "Thinking...").await;

        match self.engine.execute_turn(ctx, turn_messages).await {
            Ok(ReActTurn::Finished { ref content, .. }) => {
                ctx.stream_cb = None;
                let _ = stream_handle.await;
                ctx.history.push(ChatMessage::assistant(content.clone()));
                let completion_tokens =
                    context_manager::TokenBudget::estimate_tokens(content);
                token_budget.record_usage(0, completion_tokens);
                Ok(false) // done
            }
            Ok(ReActTurn::ToolCalls { content: tool_text, calls, reasoning_content }) => {
                ctx.stream_cb = None;
                let _ = stream_handle.await;

                let tool_names: Vec<&str> = calls.iter().map(|c| c.tool_name.as_str()).collect();
                self.registry.set_activity(
                    &ctx.agent_id,
                    format!("Using tools: {}", tool_names.join(", ")),
                ).await;

                self.publish_tool_calls_event(ctx, &calls).await;

                let formatted_calls = llm::format_tool_calls_for_history(&calls);
                ctx.history.push(ChatMessage {
                    role: ChatMessageRole::Assistant,
                    content: tool_text,
                    tool_call_id: None,
                    tool_name: None,
                    tool_calls: Some(formatted_calls),
                    reasoning_content,
                });

                let exec_result = self.engine.execute_tools(ctx, &calls, true).await.map_err(|e| {
                    Error::ConfigInvalid {
                        message: format!("tool execution failed: {e}"),
                    }
                })?;
                let results = exec_result.messages;

                self.publish_tool_results_event(ctx, results.len()).await;

                // read_skill detection + reinforcement
                let has_read_skill = calls.iter().any(|c| c.tool_name == "read_skill");
                if has_read_skill {
                    *loaded_skill_body = calls.iter()
                        .position(|c| c.tool_name == "read_skill")
                        .and_then(|idx| results.get(idx))
                        .map(|r| r.content.clone());
                }

                ctx.history.extend(results);

                if has_read_skill
                    && let Some(call) = calls.iter().find(|c| c.tool_name == "read_skill") {
                        let skill_name = call.args.get("skill")
                            .and_then(|v| v.as_str())
                            .unwrap_or("skill");
                        ctx.history.push(skill::formatting::build_read_skill_reinforcement(skill_name));
                    }

                // Format reminder after data-gathering turns
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

                ctx.turn += 1;
                Ok(true) // continue looping
            }
            Ok(ReActTurn::Error(react_err)) => {
                ctx.stream_cb = None;
                let _ = stream_handle.await;
                let _ = self.publish_llm_error(ctx, &react_err.to_string()).await;
                Err(Error::ConfigInvalid {
                    message: format!("ReAct turn error at {}: {react_err}", ctx.turn),
                })
            }
            Err(e) => {
                ctx.stream_cb = None;
                let _ = stream_handle.await;
                let _ = self.publish_llm_error(ctx, &e.to_string()).await;
                Err(Error::ConfigInvalid {
                    message: format!("ReAct loop error at turn {}: {e}", ctx.turn),
                })
            }
        }
    }

    /// Publish agent:got_tool_calls event.
    async fn publish_tool_calls_event(&self, ctx: &ReActContext, calls: &[ParsedToolCall]) {
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
    }

    /// Publish agent:tool_results_fed_back event.
    async fn publish_tool_results_event(&self, ctx: &ReActContext, result_count: usize) {
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
                        "result_count": result_count,
                    }),
                ),
            )
            .await;
    }

    /// Publish llm_error event.
    async fn publish_llm_error(&self, ctx: &ReActContext, error: &str) {
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
                        "error": error,
                    }),
                ),
            )
            .await;
    }

    async fn react_loop(
        &self,
        ctx: &mut ReActContext,
        token_budget: &mut context_manager::TokenBudget,
        interrupt: Option<&InterruptFlag>,
        background: bool,
    ) -> Result<ReactOutcome, Error> {
        let compressor = context_manager::HistoryCompressor::new(
            context_manager::CompressionStrategy::Truncate,
        );
        let mut loaded_skill_body: Option<String> = None;
        /// Max auto-continuations for background idle runs (prevents infinite loop).
        const MAX_CONTINUATIONS: u32 = 5;
        let mut continuation_count: u32 = 0;

        loop {
            // --- pre-turn checks ---
            if ctx.turn >= ctx.max_turns {
                // Compress and persist session state.
                let history_tokens: usize = ctx
                    .history
                    .iter()
                    .map(|m| context_manager::TokenBudget::estimate_tokens(&m.content))
                    .sum();
                token_budget.set_history_tokens(history_tokens);
                if token_budget.needs_trim() {
                    let _ = compressor.compress_with_boundaries(
                        &mut ctx.history,
                        token_budget,
                        &self.compression_config,
                    );
                }
                // Persist to session store
                if let Some(store) = self.registry.get_session_store(&ctx.agent_id).await {
                    for msg in &ctx.history {
                        let ts = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_millis();
                        let entry = serde_json::json!({
                            "role": msg.role,
                            "content": msg.content,
                            "timestamp_ms": ts,
                        });
                        let _ = store.append_session_event(&ctx.session_id, &entry);
                    }
                }
                // Evaluate progress before deciding to auto-continue.
                let messages: Vec<(String, String)> = ctx.history.iter()
                    .map(|m| {
                        let role = match m.role {
                            ChatMessageRole::System => "system",
                            ChatMessageRole::User => "user",
                            ChatMessageRole::Assistant => "assistant",
                            ChatMessageRole::Tool => "tool",
                        };
                        (role.to_string(), m.content.clone())
                    })
                    .collect();
                let progress = eval::session_progress::evaluate(&messages);
                tracing::info!(
                    agent_id = %ctx.agent_id,
                    session_id = %ctx.session_id,
                    turns = ctx.turn,
                    collision_found = progress.collision_found,
                    partial_match = progress.best_partial_match,
                    stuck = progress.looks_stuck,
                    unique_tools = progress.unique_tools.len(),
                    "max turns — evaluating session progress"
                );

                // Stop if: collision found (done!), looks stuck, or exhausted continuations.
                let should_continue = background
                    && continuation_count < MAX_CONTINUATIONS
                    && !progress.collision_found
                    && !progress.looks_stuck;

                if !should_continue && background {
                    let reason = if progress.collision_found {
                        "collision found — stopping"
                    } else if progress.looks_stuck {
                        "agent appears stuck — stopping"
                    } else {
                        "all continuations exhausted"
                    };
                    tracing::info!(
                        agent_id = %ctx.agent_id,
                        session_id = %ctx.session_id,
                        reason,
                        "auto-continue stopped"
                    );
                    let _ = self.bus.publish(Event::new(
                        "agent:harness",
                        EventType::Custom("agent:auto_continue_stopped".to_owned()),
                        json!({
                            "agent_id": ctx.agent_id,
                            "session_id": ctx.session_id,
                            "reason": reason,
                            "collision_found": progress.collision_found,
                            "best_partial_match": progress.best_partial_match,
                            "looks_stuck": progress.looks_stuck,
                            "total_turns": ctx.turn,
                            "continuations": continuation_count,
                        }),
                    )).await;
                    return Ok(ReactOutcome::MaxTurnsReached {
                        turns: ctx.max_turns * (continuation_count + 1),
                    });
                }

                if background {
                    continuation_count += 1;
                    tracing::info!(
                        agent_id = %ctx.agent_id,
                        session_id = %ctx.session_id,
                        continuation = continuation_count,
                        max = MAX_CONTINUATIONS,
                        "max turns reached — auto-continuing (background idle run)"
                    );
                    // Publish event for notification (auto-dismiss after 3s).
                    let _ = self
                        .bus
                        .publish(Event::new(
                            "agent:harness",
                            EventType::Custom("agent:auto_continue".to_owned()),
                            json!({
                                "agent_id": ctx.agent_id,
                                "session_id": ctx.session_id,
                                "continuation": continuation_count,
                                "max_continuations": MAX_CONTINUATIONS,
                            }),
                        ))
                        .await;
                    ctx.turn = 0;
                    continue;
                }
                return Ok(ReactOutcome::MaxTurnsReached {
                    turns: ctx.max_turns * (continuation_count + 1),
                });
            }

            if let Some(flag) = interrupt
                && flag.is_interrupted() {
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

            // --- token budget & compression ---
            let history_tokens: usize = ctx
                .history
                .iter()
                .map(|m| context_manager::TokenBudget::estimate_tokens(&m.content))
                .sum();
            token_budget.set_history_tokens(history_tokens);

            if token_budget.needs_trim() {
                let config = self.compression_config.clone();
                let result = compressor.compress_with_boundaries(
                    &mut ctx.history,
                    token_budget,
                    &config,
                );
                if result.messages_removed > 0 || result.tokens_saved > 0 {
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
                                    "remaining_messages": ctx.history.len(),
                                    "token_usage_pct": token_budget.usage_percent(),
                                    "strategy": if result.strategy.is_truncate() { "truncate" } else { "summarize" },
                                    "preflight": true,
                                    "compression_paused": token_budget.compression_paused,
                                }),
                            ),
                        )
                        .await;
                }
            }

            // --- process one turn (shared with direct_act) ---
            let should_continue = self.process_react_turn(
                ctx, token_budget, &mut loaded_skill_body,
            ).await?;
            if !should_continue {
                let reply = ctx.history.last()
                    .map(|m| m.content.clone())
                    .unwrap_or_default();
                return Ok(ReactOutcome::Finished(reply));
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
    ///
    /// Returns a [`JoinHandle`] that resolves once the forwarder has finished
    /// draining the channel.  Callers MUST await this handle after clearing
    /// [`ReActContext::stream_cb`] and BEFORE publishing `reply_ready` or `idle`,
    /// so that `reply_stream_done` is guaranteed to appear first in the event log.
    fn spawn_stream_forwarder(
        &self,
        ctx: &mut ReActContext,
    ) -> tokio::task::JoinHandle<()> {
        let (stream_tx, mut stream_rx) = tokio::sync::mpsc::unbounded_channel::<StreamEvent>();
        {
            let tx = stream_tx.clone();
            ctx.stream_cb = Some(Arc::new(move |event| {
                let _ = tx.send(event);
            }) as Arc<dyn Fn(StreamEvent) + Send + Sync>);
        }
        // Publish streaming events to the global bus so that cross-cutting
        // subscribers (ChatReplyHandler, SSE, persistence) can see them.
        // Also publish to the agent's local bus when one exists, for any
        // agent-internal consumers.
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
                // Always publish to the global bus — IM streaming consumers
                // (StreamingChatReplyHandler) and SSE listen here.
                let _ = global_bus.publish(e.clone()).await;
                // Also push to the local bus when available.
                if let Some(ref local_bus) = registry.get_local_bus(&aid).await {
                    let _ = local_bus.publish(e).await;
                }
            }
        })
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

#[cfg(test)]
mod tests {
    use super::{process_remember_commands, sanitize_api_keys};

    #[test]
    fn process_remember_commands_extracts_remembered_items() {
        let text = "Hello [remember:buy milk] world [remember:call mom]";
        let (cleaned, remembered) = process_remember_commands(text);
        assert_eq!(cleaned, "Hello  world");
        assert_eq!(remembered, vec!["buy milk", "call mom"]);
    }

    #[test]
    fn process_remember_commands_empty_text() {
        let text = "";
        let (cleaned, remembered) = process_remember_commands(text);
        assert_eq!(cleaned, "");
        assert!(remembered.is_empty());
    }

    #[test]
    fn process_remember_commands_no_commands() {
        let text = "Just a normal message without any remember commands.";
        let (cleaned, remembered) = process_remember_commands(text);
        assert_eq!(cleaned, "Just a normal message without any remember commands.");
        assert!(remembered.is_empty());
    }

    #[test]
    fn process_remember_commands_empty_content() {
        let text = "[remember:]";
        let (cleaned, remembered) = process_remember_commands(text);
        // Empty content should not be added to remembered
        assert!(remembered.is_empty());
    }

    #[test]
    fn sanitize_api_keys_redacts_openai_key_in_json() {
        let text = r#"{"apiKey": "sk-proj-ABCDEF1234567890abcdef1234567890"}"#;
        let result = sanitize_api_keys(text);
        assert!(!result.contains("sk-proj-"), "API key should be redacted: {result}");
        assert!(result.contains("[REDACTED]"), "result should contain [REDACTED]");
    }

    #[test]
    fn sanitize_api_keys_redacts_bearer_token() {
        let text = "Authorization: Bearer sk-ant-api03-ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
        let result = sanitize_api_keys(text);
        assert!(!result.contains("sk-ant-"), "API key should be redacted: {result}");
        assert!(result.contains("[REDACTED]"), "result should contain [REDACTED]");
    }

    #[test]
    fn sanitize_api_keys_leaves_short_sk_prefix() {
        // Short strings starting with sk- that don't look like API keys
        let text = "sky is blue, skill is power";
        let result = sanitize_api_keys(text);
        assert_eq!(result, "sky is blue, skill is power", "short sk- patterns should not be redacted");
    }

    #[test]
    fn sanitize_api_keys_no_change_for_clean_text() {
        let text = "This is a normal piece of text with no keys.";
        let result = sanitize_api_keys(text);
        assert_eq!(result, text);
    }
}
