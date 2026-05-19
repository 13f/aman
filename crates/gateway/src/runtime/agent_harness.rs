use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
use std::time::Instant;

use event_bus::EventBus;
use futures_util::StreamExt;
use kernel::agent::AgentStatus;
use kernel::event::{Event, EventType};
use kernel::react::{
    self, ChatMessage, ParsedToolCall, ReActContext, ReActEngine as _, ReActTurn, SoulSnapshot,
    StreamEvent, ToolDescriptor,
};
use kernel::tool::Tool;
use kernel::{AmanResult, Error};
use serde_json::json;
use tool::ToolRegistry;

use super::memory_store::MemoryStore;
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
    /// API key for direct streaming HTTP calls to the LLM provider.
    api_key: String,
    /// Base URL for direct streaming HTTP calls to the LLM provider.
    base_url: String,
}

impl LlmReActEngine {
    pub fn new(
        tool_registry: Arc<ToolRegistry>,
        agent_registry: Arc<AgentRegistry>,
        bus: Arc<dyn EventBus>,
        api_key: String,
        base_url: String,
    ) -> Self {
        Self {
            tool_registry,
            agent_registry,
            bus,
            api_key,
            base_url,
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

    /// Execute a streaming LLM call via SSE, calling the callback for each delta.
    ///
    /// Returns (full_content, finish_reason, tool_calls_map).
    async fn streaming_llm_call(
        &self,
        system_prompt: &str,
        history: &[serde_json::Value],
        ctx: &ReActContext,
        cb: Arc<dyn Fn(StreamEvent) + Send + Sync>,
    ) -> Result<(String, String, Vec<ParsedToolCall>), kernel::Error> {
        let mut request_messages: Vec<serde_json::Value> = Vec::with_capacity(history.len() + 1);
        request_messages.push(json!({"role": "system", "content": system_prompt}));
        request_messages.extend(history.iter().cloned());

        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(120))
            .build()
            .map_err(|e| Error::Io(std::io::Error::new(std::io::ErrorKind::Other, e.to_string())))?;

        let response = client
            .post(format!("{}/chat/completions", self.base_url))
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .json(&serde_json::json!({
                "model": ctx.model,
                "messages": request_messages,
                "stream": true,
                "temperature": 0.7,
                "max_tokens": 4096,
            }))
            .send()
            .await
            .map_err(|e| Error::Io(std::io::Error::new(std::io::ErrorKind::Other, e.to_string())))?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(Error::ConfigInvalid {
                message: format!("LLM API streaming error HTTP {status}: {body}"),
            });
        }

        let mut stream = response.bytes_stream();
        let mut full_content = String::new();
        let mut buffer = String::new();
        let mut finish_reason = "stop".to_owned();
        let mut tool_call_acc: HashMap<usize, serde_json::Value> = HashMap::new();

        cb(StreamEvent::Start);

        while let Some(chunk_result) = stream.next().await {
            let chunk = chunk_result
                .map_err(|e| Error::Io(std::io::Error::new(std::io::ErrorKind::Other, e.to_string())))?;
            let chunk_str = String::from_utf8_lossy(&chunk);
            buffer.push_str(&chunk_str);

            // Process complete lines in buffer
            loop {
                let newline_pos = match buffer.find('\n') {
                    Some(p) => p,
                    None => break,
                };
                let line = buffer[..newline_pos].to_string();
                buffer = buffer[newline_pos + 1..].to_string();

                if !line.starts_with("data: ") {
                    continue;
                }
                let data = line[6..].trim();
                if data == "[DONE]" {
                    break;
                }
                let Ok(sse) = serde_json::from_str::<serde_json::Value>(data) else {
                    continue;
                };
                let Some(choices) = sse.get("choices").and_then(|c| c.as_array()) else {
                    continue;
                };
                let Some(choice) = choices.first() else {
                    continue;
                };

                // Extract delta content (text)
                if let Some(delta) = choice.get("delta") {
                    if let Some(content) = delta.get("content").and_then(|c| c.as_str()) {
                        if !content.is_empty() {
                            full_content.push_str(content);
                            cb(StreamEvent::Chunk(content.to_owned()));
                        }
                    }

                    // Accumulate tool call deltas
                    if let Some(tc_arr) = delta.get("tool_calls").and_then(|t| t.as_array()) {
                        for tc in tc_arr {
                            let idx = tc.get("index").and_then(|i| i.as_u64()).unwrap_or(0) as usize;
                            let entry = tool_call_acc.entry(idx).or_insert_with(|| {
                                serde_json::json!({
                                    "id": null,
                                    "type": "function",
                                    "function": {"name": null, "arguments": ""}
                                })
                            });
                            if let Some(id) = tc.get("id").and_then(|i| i.as_str()) {
                                entry["id"] = serde_json::json!(id);
                            }
                            if let Some(name) = tc.get("function")
                                .and_then(|f| f.get("name"))
                                .and_then(|n| n.as_str())
                            {
                                entry["function"]["name"] = serde_json::json!(name);
                            }
                            if let Some(args) = tc.get("function")
                                .and_then(|f| f.get("arguments"))
                                .and_then(|a| a.as_str())
                            {
                                let current = entry["function"]["arguments"]
                                    .as_str()
                                    .unwrap_or("")
                                    .to_owned();
                                entry["function"]["arguments"] = serde_json::json!(current + args);
                            }
                        }
                    }
                }

                // Check finish_reason
                if let Some(reason) = choice.get("finish_reason").and_then(|r| r.as_str()) {
                    if !reason.is_empty() && reason != "null" && reason != "null" {
                        finish_reason = reason.to_owned();
                        cb(StreamEvent::Done {
                            finish_reason: finish_reason.clone(),
                        });
                    }
                }
            }
        }

        // Convert accumulated tool calls to ParsedToolCall vec
        let tool_calls: Vec<ParsedToolCall> = tool_call_acc
            .into_values()
            .filter_map(|tc| {
                let id = tc.get("id")?.as_str()?.to_owned();
                let name = tc.get("function")?.get("name")?.as_str()?.to_owned();
                let args_str = tc.get("function")?.get("arguments")?.as_str()?.to_owned();
                let args: serde_json::Value =
                    serde_json::from_str(&args_str).unwrap_or(serde_json::Value::Object(Default::default()));
                Some(ParsedToolCall {
                    id,
                    tool_name: name,
                    args,
                })
            })
            .collect();

        Ok((full_content, finish_reason, tool_calls))
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

        let result = if let Some(ref cb) = ctx.stream_cb {
            // ── Streaming path (T2.4): direct SSE HTTP call ──
            let cb = Arc::clone(cb);
            match self
                .streaming_llm_call(&system_prompt, &history, ctx, cb)
                .await
            {
                Ok((content, finish_reason, tool_calls)) => {
                    if tool_calls.is_empty() {
                        Ok(json!({
                            "content": content,
                            "finish_reason": finish_reason,
                        }))
                    } else {
                        Ok(json!({
                            "content": content,
                            "finish_reason": finish_reason,
                            "tool_calls": tool_calls,
                        }))
                    }
                }
                Err(e) => Err(e),
            }
        } else {
            // ── Non-streaming tool path ──
            let llm_tool = self.find_llm_tool().ok_or_else(|| {
                kernel::react::ReActError::LlmError("no LLM provider tool registered".to_owned())
            })?;
            let params = json!({
                "system_prompt": system_prompt,
                "messages": history,
                "max_turns": 1,
            });
            let tool_ctx = kernel::context::ToolContext::default();
            llm_tool.execute(params, tool_ctx).await
        };

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
    /// Memory store for long-term recall (M5).
    memory_store: Arc<MemoryStore>,
    /// Per-session interrupt flags for external stop (M6).
    active_interrupts: RwLock<HashMap<String, Arc<InterruptFlag>>>,
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
        memory_store: Arc<MemoryStore>,
        api_key: String,
        base_url: String,
    ) -> Self {
        let engine = LlmReActEngine::new(
            Arc::clone(&tool_registry),
            Arc::clone(&registry),
            Arc::clone(&bus),
            api_key,
            base_url,
        );
        Self {
            registry,
            tool_registry,
            engine,
            bus,
            memory_store,
            active_interrupts: RwLock::new(HashMap::new()),
            max_react_turns: DEFAULT_MAX_REACT_TURNS,
            token_budget_limit: DEFAULT_TOKEN_BUDGET_LIMIT,
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

        // 5. Initialize model-aware token budget (M4)
        let mut token_budget = crate::runtime::token_budget::TokenBudget::new(model);
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
        let memory_results = self.memory_store.retrieve(agent_id, user_text);
        let memory_context = if memory_results.is_empty() {
            None
        } else {
            let mem_text: Vec<String> = memory_results
                .iter()
                .map(|m| format!("- {} (tags: {})", m.content, m.tags.join(", ")))
                .collect();
            Some(mem_text.join("\n"))
        };

        // 7. Create ReAct context
        let mut ctx = ReActContext::new(
            agent_id.to_owned(),
            session_id.to_owned(),
            soul_snapshot,
            history,
            available_tools,
            model,
            self.max_react_turns,
            self.token_budget_limit,
        );
        ctx.memory_context = memory_context;

        // M6: Register interrupt flag for this session
        let interrupt_flag = Arc::new(InterruptFlag::new());
        self.register_interrupt(session_id, Arc::clone(&interrupt_flag));

        // 8. Execute ReAct loop with token budget management
        let result = self
            .react_loop(&mut ctx, &mut token_budget, Some(&interrupt_flag))
            .await;

        // M6: Unregister interrupt flag
        self.unregister_interrupt(session_id);

        // 9. Handle outcome — Finished, Interrupted, or Error
        let (raw_reply, event_type): (String, &str) = match result {
            Ok(ReactOutcome::Finished(reply)) => (reply, "agent:reply_ready"),
            Ok(ReactOutcome::Interrupted(reply)) => (reply, "agent:reply_interrupted"),
            Err(e) => {
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
                        .bus
                        .publish(Event::new(
                            "agent:harness",
                            EventType::Custom("agent:reply_interrupted".to_owned()),
                            json!({
                                "agent_id": ctx.agent_id,
                                "session_id": ctx.session_id,
                                "turn": ctx.turn,
                            }),
                        ))
                        .await;
                    return Ok(ReactOutcome::Interrupted(String::new()));
                }
            }

            // M4: Check token budget and compress history if needed
            if token_budget.needs_trim() {
                let result = compressor.compress(&mut ctx.history, token_budget, 3);
                if result.messages_removed > 0 {
                    let _ = self
                        .bus
                        .publish(Event::new(
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
                        ))
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
            let (stream_tx, mut stream_rx) = tokio::sync::mpsc::unbounded_channel::<StreamEvent>();
            {
                let tx = stream_tx.clone();
                ctx.stream_cb = Some(Arc::new(move |event| {
                    let _ = tx.send(event);
                }) as Arc<dyn Fn(StreamEvent) + Send + Sync>);
            }

            // Spawn consumer to forward stream events to the event bus
            let bus = Arc::clone(&self.bus);
            let aid = ctx.agent_id.clone();
            let sid = ctx.session_id.clone();
            let t = ctx.turn;
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
                    let _ = bus
                        .publish(Event::new(
                            "agent:harness",
                            EventType::Custom(etype.to_owned()),
                            json!({
                                "agent_id": aid,
                                "session_id": sid,
                                "turn": t,
                                "event_type": etype,
                                "extra": extra,
                            }),
                        ))
                        .await;
                }
            });

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
                Ok(ReActTurn::ToolCalls(calls)) => {
                    // Clear streaming callback (will be reset next iteration)
                    ctx.stream_cb = None;
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
