use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

use event_bus::EventBus;
use kernel::agent::AgentStatus;
use kernel::event::{Event, EventType};
use kernel::react::{
    self, ChatMessage, ParsedToolCall, ReActContext, ReActEngine as _, ReActTurn, SoulSnapshot,
    ToolDescriptor,
};
use kernel::tool::Tool;
use kernel::{AmanResult, Error};
use serde_json::json;
use tool::ToolRegistry;

use super::AgentRegistry;

/// Default maximum ReAct loop iterations.
const DEFAULT_MAX_REACT_TURNS: u32 = 10;
/// Default token budget limit.
const DEFAULT_TOKEN_BUDGET_LIMIT: u64 = 100_000;

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

/// Wraps tool execution with permission checks and event publishing.
pub struct ToolExecutor {
    registry: Arc<ToolRegistry>,
    agent_registry: Arc<AgentRegistry>,
    bus: Arc<dyn EventBus>,
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
        }
    }

    /// Execute a tool call for a specific agent, checking permissions first.
    ///
    /// Returns a structured result — permission denials are returned as
    /// failed results so the LLM can adapt, rather than aborting the loop.
    pub async fn execute_for_agent(
        &self,
        call: &ParsedToolCall,
        agent_id: &str,
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

        self.execute(call, agent_id).await
    }

    /// Execute a tool call, publishing lifecycle events.
    pub async fn execute(
        &self,
        call: &ParsedToolCall,
        agent_id: &str,
    ) -> react::ToolCallResult {
        let start = Instant::now();
        let tool_id = call.id.clone();
        let tool_name = call.tool_name.clone();

        // Publish tool:dispatched
        let _ = self
            .bus
            .publish(Event::new(
                "agent:harness",
                EventType::Custom("tool:dispatched".to_owned()),
                json!({
                    "agent_id": agent_id,
                    "tool_call_id": tool_id,
                    "tool_name": tool_name,
                    "args": call.args,
                }),
            ))
            .await;

        let tool = self.registry.get(&tool_name);
        let (success, output) = match tool {
            Some(t) => {
                let ctx = kernel::context::ToolContext::default();
                match t.execute(call.args.clone(), ctx).await {
                    Ok(value) => (true, value.to_string()),
                    Err(e) => (false, format!("tool error: {e}")),
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
            .bus
            .publish(Event::new(
                "agent:harness",
                EventType::Custom(event_type.to_owned()),
                json!({
                    "agent_id": agent_id,
                    "tool_call_id": tool_id,
                    "tool_name": tool_name,
                    "success": success,
                    "duration_ms": duration_ms,
                    "output": output,
                }),
            ))
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

/// Assembles the context prompt from SOUL, history, memory, and tool schemas.
pub struct ContextAssembler;

impl ContextAssembler {
    /// Build the system prompt for the LLM.
    pub fn assemble(
        soul: &SoulSnapshot,
        tools: &[ToolDescriptor],
        memory: Option<&str>,
    ) -> String {
        let mut parts: Vec<String> = Vec::new();

        // SOUL system prompt
        parts.push(soul.system_prompt.clone());

        // Available tools
        if !tools.is_empty() {
            let tool_list: Vec<String> = tools
                .iter()
                .map(|t| format!("- {}: {} (parameters: {})", t.name, t.description, t.parameters))
                .collect();
            parts.push(format!(
                "\n## Available Tools\nYou can use these tools when responding:\n{}",
                tool_list.join("\n")
            ));
            parts.push(
                "\nWhen you need to use a tool, respond with a JSON tool call in the format:\
                 \n```tool_call\n{\"name\": \"tool_name\", \"arguments\": {...}}\n```"
                    .to_owned(),
            );
        }

        // Memory context
        if let Some(mem) = memory {
            if !mem.is_empty() {
                parts.push(format!("\n## Retrieved Memories\n{mem}"));
            }
        }

        parts.join("\n\n")
    }
}

/// Concrete ReAct engine that calls an LLM provider tool.
pub struct LlmReActEngine {
    tool_registry: Arc<ToolRegistry>,
    agent_registry: Arc<AgentRegistry>,
    bus: Arc<dyn EventBus>,
}

impl LlmReActEngine {
    pub fn new(
        tool_registry: Arc<ToolRegistry>,
        agent_registry: Arc<AgentRegistry>,
        bus: Arc<dyn EventBus>,
    ) -> Self {
        Self {
            tool_registry,
            agent_registry,
            bus,
        }
    }

    /// Find the LLM provider tool from the registry.
    fn find_llm_tool(&self) -> Option<Arc<dyn Tool>> {
        // Search for any tool whose name starts with "llm_" or "llm_provider_"
        let names = self.tool_registry.list_tools();
        for n in &names {
            if n.starts_with("llm_") || n.starts_with("llm_provider_") {
                return self.tool_registry.get(n);
            }
        }
        None
    }
}

#[async_trait::async_trait]
impl kernel::react::ReActEngine for LlmReActEngine {
    async fn execute_turn(
        &self,
        ctx: &ReActContext,
        messages: Vec<ChatMessage>,
    ) -> Result<ReActTurn, kernel::react::ReActError> {
        // Publish llm:call_started
        let _ = self
            .bus
            .publish(Event::new(
                "agent:harness",
                EventType::Custom("llm:call_started".to_owned()),
                json!({
                    "agent_id": ctx.agent_id,
                    "session_id": ctx.session_id,
                    "turn": ctx.turn,
                }),
            ))
            .await;

        let llm_tool = self.find_llm_tool().ok_or_else(|| {
            kernel::react::ReActError::LlmError("no LLM provider tool registered".to_owned())
        })?;

        // Build the payload: system prompt from soul + conversation history
        let system_prompt = ContextAssembler::assemble(
            &ctx.soul_snapshot,
            &ctx.agent_tools,
            ctx.memory_context.as_deref(),
        );

        let history: Vec<serde_json::Value> = messages
            .iter()
            .map(|m| {
                json!({
                    "role": format!("{:?}", m.role).to_lowercase(),
                    "content": m.content,
                })
            })
            .collect();

        let params = json!({
            "system_prompt": system_prompt,
            "messages": history,
            "max_turns": 1,
        });

        let tool_ctx = kernel::context::ToolContext::default();
        let result = llm_tool.execute(params, tool_ctx).await;

        // Publish llm:call_ended
        let _ = self
            .bus
            .publish(Event::new(
                "agent:harness",
                EventType::Custom("llm:call_ended".to_owned()),
                json!({
                    "agent_id": ctx.agent_id,
                    "session_id": ctx.session_id,
                    "turn": ctx.turn,
                    "success": result.is_ok(),
                }),
            ))
            .await;

        match result {
            Ok(value) => {
                // Try to parse tool calls from the response
                let content = value
                    .get("content")
                    .and_then(|c| c.as_str())
                    .unwrap_or("")
                    .to_owned();
                let tool_calls = value
                    .get("tool_calls")
                    .and_then(|tc| serde_json::from_value::<Vec<ParsedToolCall>>(tc.clone()).ok())
                    .unwrap_or_default();

                if tool_calls.is_empty() {
                    // Publish token usage estimate
                    let estimated_tokens = (content.len() / 4) as u64;
                    let _ = self
                        .bus
                        .publish(Event::new(
                            "agent:harness",
                            EventType::Custom("agent:token_used".to_owned()),
                            json!({
                                "agent_id": ctx.agent_id,
                                "session_id": ctx.session_id,
                                "turn": ctx.turn,
                                "tokens": estimated_tokens,
                            }),
                        ))
                        .await;

                    Ok(ReActTurn::Finished {
                        content,
                        finish_reason: value
                            .get("finish_reason")
                            .and_then(|r| r.as_str())
                            .unwrap_or("stop")
                            .to_owned(),
                    })
                } else {
                    Ok(ReActTurn::ToolCalls(tool_calls))
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
            let result = executor.execute_for_agent(call, &ctx.agent_id).await;
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
    /// Default max ReAct turns.
    max_react_turns: u32,
    /// Default token budget limit.
    token_budget_limit: u64,
}

impl AgentHarness {
    pub fn new(
        registry: Arc<AgentRegistry>,
        tool_registry: Arc<ToolRegistry>,
        bus: Arc<dyn EventBus>,
    ) -> Self {
        let engine = LlmReActEngine::new(
            Arc::clone(&tool_registry),
            Arc::clone(&registry),
            Arc::clone(&bus),
        );
        Self {
            registry,
            tool_registry,
            engine,
            bus,
            max_react_turns: DEFAULT_MAX_REACT_TURNS,
            token_budget_limit: DEFAULT_TOKEN_BUDGET_LIMIT,
        }
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
        soul_snapshot: SoulSnapshot,
        interrupt: Option<&InterruptFlag>,
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

        // Publish agent:busy event
        let _ = self
            .bus
            .publish(Event::new(
                "agent:harness",
                EventType::Custom("agent:busy".to_owned()),
                json!({
                    "agent_id": agent_id,
                    "session_id": session_id,
                }),
            ))
            .await;

        // 3. Build tool descriptors from registered tools
        let available_tools = self.build_tool_descriptors(agent_id).await;

        // 4. Build conversation history
        let history = vec![ChatMessage::user(user_text)];

        // 5. Create ReAct context
        let mut ctx = ReActContext::new(
            agent_id.to_owned(),
            session_id.to_owned(),
            soul_snapshot,
            history,
            available_tools,
            self.max_react_turns,
            self.token_budget_limit,
        );

        // 6. Execute ReAct loop
        let final_reply = self.react_loop(&mut ctx, interrupt).await?;

        // 7. Publish agent:reply_ready
        let _ = self
            .bus
            .publish(Event::new(
                "agent:harness",
                EventType::Custom("agent:reply_ready".to_owned()),
                json!({
                    "agent_id": agent_id,
                    "session_id": session_id,
                    "reply": final_reply,
                }),
            ))
            .await;

        // 8. Update status to Idle
        self.registry
            .set_active_session(agent_id, None)
            .await?;
        self.registry
            .set_status(agent_id, AgentStatus::Idle)
            .await?;

        // Publish agent:idle event
        let _ = self
            .bus
            .publish(Event::new(
                "agent:harness",
                EventType::Custom("agent:idle".to_owned()),
                json!({
                    "agent_id": agent_id,
                    "session_id": session_id,
                }),
            ))
            .await;

        Ok(final_reply)
    }

    /// The core think-act-observe loop.
    async fn react_loop(
        &self,
        ctx: &mut ReActContext,
        interrupt: Option<&InterruptFlag>,
    ) -> AmanResult<String> {
        loop {
            // Check budget
            if ctx.token_budget.is_exceeded() {
                return Err(Error::ConfigInvalid {
                    message: format!(
                        "token budget exceeded: {}/{}",
                        ctx.token_budget.used, ctx.token_budget.limit
                    ),
                });
            }

            // Check max turns
            if ctx.turn >= ctx.max_turns {
                return Err(Error::ConfigInvalid {
                    message: format!("max ReAct turns ({}) reached", ctx.max_turns),
                });
            }

            // Check interrupt
            if let Some(flag) = interrupt {
                if flag.is_interrupted() {
                    return Err(Error::ConfigInvalid {
                        message: "ReAct loop interrupted by user".to_owned(),
                    });
                }
            }

            // Trim history if budget is tight (over 80%)
            if ctx.token_budget.fraction_used() > 0.8 && ctx.history.len() > 5 {
                // Keep system message + last 5 messages
                let keep = ctx.history.len().min(5);
                ctx.history = ctx.history.split_off(ctx.history.len() - keep);
            }

            let turn_messages = ctx.history.clone();

            // Execute one ReAct turn
            match self.engine.execute_turn(ctx, turn_messages).await {
                Ok(ReActTurn::Finished { content, .. }) => {
                    ctx.history.push(ChatMessage::assistant(&content));
                    return Ok(content);
                }
                Ok(ReActTurn::ToolCalls(calls)) => {
                    // Publish agent:got_tool_calls
                    let _ = self
                        .bus
                        .publish(Event::new(
                            "agent:harness",
                            EventType::Custom("agent:got_tool_calls".to_owned()),
                            json!({
                                "agent_id": ctx.agent_id,
                                "session_id": ctx.session_id,
                                "turn": ctx.turn,
                                "tool_calls": calls.iter().map(|c| json!({"name": c.tool_name, "id": c.id})).collect::<Vec<_>>(),
                            }),
                        ))
                        .await;

                    // Execute tools
                    let results = self.engine.execute_tools(ctx, &calls).await.map_err(|e| {
                        Error::ConfigInvalid {
                            message: format!("tool execution failed: {e}"),
                        }
                    })?;

                    // Publish agent:tool_results_fed_back
                    let _ = self
                        .bus
                        .publish(Event::new(
                            "agent:harness",
                            EventType::Custom("agent:tool_results_fed_back".to_owned()),
                            json!({
                                "agent_id": ctx.agent_id,
                                "session_id": ctx.session_id,
                                "turn": ctx.turn,
                                "result_count": results.len(),
                            }),
                        ))
                        .await;

                    // Append tool results to history
                    ctx.history.extend(results);

                    // Increment turn
                    ctx.turn += 1;
                }
                Ok(ReActTurn::Error(react_err)) => {
                    let _ = self
                        .bus
                        .publish(Event::new(
                            "agent:harness",
                            EventType::Custom("llm:error".to_owned()),
                            json!({
                                "agent_id": ctx.agent_id,
                                "session_id": ctx.session_id,
                                "turn": ctx.turn,
                                "error": react_err.to_string(),
                            }),
                        ))
                        .await;
                    return Err(Error::ConfigInvalid {
                        message: format!("ReAct turn error at {}: {react_err}", ctx.turn),
                    });
                }
                Err(e) => {
                    // Publish llm:error
                    let _ = self
                        .bus
                        .publish(Event::new(
                            "agent:harness",
                            EventType::Custom("llm:error".to_owned()),
                            json!({
                                "agent_id": ctx.agent_id,
                                "session_id": ctx.session_id,
                                "turn": ctx.turn,
                                "error": e.to_string(),
                            }),
                        ))
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
                    description: format!("{:?}", tool.mode()),
                    parameters: serde_json::to_value(tool.parameters()).unwrap_or_default(),
                });
            }
        }

        descriptors
    }
}
