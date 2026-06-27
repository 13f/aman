// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! CognitiveReActEngine — implements `kernel::react::ReActEngine` by delegating
//! to a `CognitiveEngine::process()` call.
//!
//! This bridges the new cognitive engine architecture (`Observation → Decision`)
//! into the gateway's existing ReAct loop. Messages are converted to observations,
//! decisions are mapped back to ReAct turns, and streaming events are forwarded
//! via the engine's listener mechanism.

use std::sync::Arc;

use async_trait::async_trait;
use cognitive_engine::{CognitiveEngine, CognitiveListener, Decision, DecisionKind, Observation};
use cognitive_react::{
    ChatMessage, ChatMessageRole, ParsedToolCall, ReActContext, ReActEngine, ReActError, ReActTurn,
    StreamEvent, ToolExecutionResult,
};
/// A ReAct engine backed by a [`CognitiveEngine`].
///
/// Converts between the gateway's `ReActContext`/`ChatMessage` world and the
/// cognitive engine's `CognitiveContext`/`Observation`/`Decision` world.
pub struct CognitiveReActEngine {
    engine: Arc<dyn CognitiveEngine>,
}

impl CognitiveReActEngine {
    /// Create a new CognitiveReActEngine wrapping the given cognitive engine.
    pub fn new(engine: Arc<dyn CognitiveEngine>) -> Self {
        Self { engine }
    }

    /// Convert the latest user message to an Observation.
    fn messages_to_observations(
        session_id: &str,
        messages: &[ChatMessage],
    ) -> Vec<Observation> {
        // Find the last user message and convert it to an observation.
        // Tool result messages are also converted for multi-turn tool use.
        let mut observations: Vec<Observation> = Vec::new();
        for msg in messages.iter().rev() {
            match msg.role {
                ChatMessageRole::User => {
                    observations.push(Observation::user_message(
                        uuid::Uuid::now_v7().to_string(),
                        session_id,
                        &msg.content,
                    ));
                    break; // Only the latest user message
                }
                ChatMessageRole::Tool => {
                    // Tool result — convert to ToolCompleted observation
                    if let Some(ref tool_call_id) = msg.tool_call_id {
                        observations.push(Observation::tool_completed(
                            uuid::Uuid::now_v7().to_string(),
                            session_id,
                            tool_call_id,
                            msg.tool_name.as_deref().unwrap_or("unknown"),
                            &msg.content,
                            true, // tool result presence implies success
                            0,
                        ));
                    }
                }
                _ => {}
            }
        }
        observations.reverse();
        observations
    }

    /// Build a CognitiveContext from a ReActContext.
    fn build_cognitive_context(ctx: &ReActContext) -> cognitive_engine::CognitiveContext {
        // Map agent tools to Capabilities
        let capabilities: Vec<cognitive_engine::Capability> = ctx
            .agent_tools
            .iter()
            .map(|t| cognitive_engine::Capability {
                name: t.name.clone(),
                description: t.description.clone(),
                parameters: t.parameters.clone(),
                cap_type: cognitive_engine::CapabilityType::Tool,
            })
            .collect();

        // Map memory context to MemoryItems
        let memory_context: Vec<cognitive_engine::MemoryItem> = ctx
            .memory_context
            .as_ref()
            .map(|mem| {
                vec![cognitive_engine::MemoryItem {
                    key: "retrieved".to_owned(),
                    content: mem.clone(),
                    importance: 0.5,
                    timestamp: None,
                }]
            })
            .unwrap_or_default();

        // Build CognitiveIdentity from SoulSnapshot
        let identity = cognitive_engine::CognitiveIdentity {
            name: ctx.soul_snapshot.name.clone(),
            identity: ctx.soul_snapshot.system_prompt.clone(),
            boundaries: ctx.soul_snapshot.boundaries.clone(),
            expertise: Vec::new(),
            vibe: None,
            raw: ctx.soul_snapshot.system_prompt.clone(),
        };

        cognitive_engine::CognitiveContext {
            agent_id: ctx.agent_id.clone(),
            session_id: ctx.session_id.clone(),
            identity,
            capabilities,
            memory_context,
            engine_config: serde_json::json!({
                "model": ctx.model,
                "max_turns": ctx.max_turns,
                "token_limit": ctx.token_budget.limit,
            }),
        }
    }

    /// Map a Vec<Decision> to a ReActTurn.
    fn decisions_to_turn(decisions: Vec<Decision>) -> Result<ReActTurn, ReActError> {
        for d in decisions {
            match d.kind {
                DecisionKind::Reply { text, is_final } => {
                    if is_final {
                        return Ok(ReActTurn::Finished {
                            content: text,
                            finish_reason: "stop".to_owned(),
                        });
                    }
                    // Streaming chunks are handled by the listener, not returned here
                }
                DecisionKind::CallTools {
                    calls,
                    block_on_completion: _,
                } => {
                    let parsed_calls: Vec<ParsedToolCall> = calls
                        .into_iter()
                        .map(|c| ParsedToolCall {
                            id: c.id,
                            tool_name: c.tool_name,
                            args: c.args,
                        })
                        .collect();
                    return Ok(ReActTurn::ToolCalls {
                        content: String::new(),
                        calls: parsed_calls,
                        reasoning_content: String::new(),
                    });
                }
                DecisionKind::Delegate { .. } => {
                    // Not yet supported in ReActTurn — skip
                }
                DecisionKind::WaitFor { .. } => {
                    // Not yet supported — skip
                }
                DecisionKind::Remember { .. } => {
                    // Handled by the gateway's remember extraction
                }
                DecisionKind::NoOp => {}
            }
        }
        // If no actionable decision found, treat as finished with empty content
        Ok(ReActTurn::Finished {
            content: String::new(),
            finish_reason: "stop".to_owned(),
        })
    }
}

// ---------------------------------------------------------------------------
// ReActEngine implementation
// ---------------------------------------------------------------------------

#[async_trait]
impl ReActEngine for CognitiveReActEngine {
    async fn execute_turn(
        &self,
        ctx: &ReActContext,
        messages: Vec<ChatMessage>,
    ) -> Result<ReActTurn, ReActError> {
        let cognitive_ctx = Self::build_cognitive_context(ctx);
        let observations = Self::messages_to_observations(&ctx.session_id, &messages);

        // Set up a streaming listener if the caller wants streaming
        if let Some(ref stream_cb) = ctx.stream_cb {
            let cb = Arc::clone(stream_cb);
            let session_id = ctx.session_id.clone();
            struct StreamListener {
                cb: Arc<dyn Fn(StreamEvent) + Send + Sync>,
                #[allow(dead_code)]
                session_id: String,
            }
            impl CognitiveListener for StreamListener {
                fn on_cognitive_event(&self, event: cognitive_engine::CognitiveEvent) {
                    match event {
                        cognitive_engine::CognitiveEvent::StreamStart { .. } => {
                            (self.cb)(StreamEvent::Start);
                        }
                        cognitive_engine::CognitiveEvent::TextChunk { text, .. } => {
                            (self.cb)(StreamEvent::Chunk(text));
                        }
                        cognitive_engine::CognitiveEvent::StreamDone {
                            finish_reason, ..
                        } => {
                            (self.cb)(StreamEvent::Done { finish_reason });
                        }
                        cognitive_engine::CognitiveEvent::StreamError { error, .. } => {
                            (self.cb)(StreamEvent::Error(error));
                        }
                        cognitive_engine::CognitiveEvent::Diagnostic { .. } => {
                            // Diagnostics are for debugging, not streaming
                        }
                    }
                }
            }
            let listener = Arc::new(StreamListener {
                cb,
                session_id,
            });
            self.engine.subscribe(listener.clone());
        }

        let decisions = self
            .engine
            .process(&cognitive_ctx, observations)
            .await
            .map_err(|e| ReActError::LlmError(e.to_string()))?;

        Self::decisions_to_turn(decisions)
    }

    async fn execute_tools(
        &self,
        _ctx: &ReActContext,
        _calls: &[ParsedToolCall],
        _block_on_detach: bool,
    ) -> Result<ToolExecutionResult, ReActError> {
        // Tool execution is handled by the gateway's ToolExecutor.
        // The cognitive engine only decides WHAT tools to call; the gateway
        // handles HOW to execute them and feeds results back as ToolCompleted
        // observations on the next execute_turn().
        //
        // This method exists for trait compatibility but should not be called
        // when using the cognitive engine path — the gateway should use its
        // own ToolExecutor directly.
        Err(ReActError::LlmError(
            "CognitiveReActEngine does not execute tools directly — \
             use the gateway's ToolExecutor and feed results back \
             as ToolCompleted observations"
                .to_owned(),
        ))
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use cognitive_engine::{
        CognitiveContext, CognitiveError, CognitiveEvent, CognitiveIdentity, CognitiveListener,
        Decision,
    };
    use cognitive_react::{SoulSnapshot, TokenBudget, ToolDescriptor};

    /// Stub cognitive engine that returns a fixed reply.
    struct StubCognitiveEngine {
        reply: String,
    }

    #[async_trait]
    impl CognitiveEngine for StubCognitiveEngine {
        fn name(&self) -> &str {
            "stub"
        }

        async fn process(
            &self,
            _ctx: &CognitiveContext,
            observations: Vec<Observation>,
        ) -> Result<Vec<Decision>, CognitiveError> {
            // Echo the user message as a reply
            let text = observations
                .iter()
                .find_map(|o| match &o.payload {
                    cognitive_engine::ObservationPayload::UserMessage { text } => {
                        Some(format!("echo: {text}"))
                    }
                    _ => None,
                })
                .unwrap_or_else(|| self.reply.clone());
            Ok(vec![Decision::reply("d1", "s1", text)])
        }

        fn subscribe(&self, _listener: Arc<dyn CognitiveListener>) {}
        fn unsubscribe(&self, _listener: &Arc<dyn CognitiveListener>) {}
        async fn reset_session(&self, _session_id: &str) -> Result<(), CognitiveError> {
            Ok(())
        }
    }

    fn make_context() -> ReActContext {
        ReActContext {
            agent_id: "test-agent".into(),
            session_id: "test-session".into(),
            turn: 1,
            max_turns: 64,
            soul_snapshot: SoulSnapshot::new("TestAgent", "You are a test agent."),
            history: Vec::new(),
            agent_tools: vec![ToolDescriptor {
                name: "search".into(),
                description: "Search the web".into(),
                parameters: serde_json::json!({"type": "object"}),
            }],
            memory_context: Some("some memory".into()),
            token_budget: TokenBudget::new(128000),
            model: "test-model".into(),
            stream_cb: None,
            interrupt_flag: None,
            anon_tool_policy: None,
        }
    }

    #[tokio::test]
    async fn user_message_becomes_reply() {
        let engine = CognitiveReActEngine::new(Arc::new(StubCognitiveEngine {
            reply: "Hello!".into(),
        }));
        let ctx = make_context();
        let messages = vec![ChatMessage {
            role: ChatMessageRole::User,
            content: "Hi there".into(),
            tool_call_id: None,
            tool_name: None,
            tool_calls: None,
            reasoning_content: String::new(),
        }];

        let turn = engine.execute_turn(&ctx, messages).await.expect("should succeed");
        match turn {
            ReActTurn::Finished { content, .. } => {
                assert_eq!(content, "echo: Hi there");
            }
            other => panic!("expected Finished, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn empty_messages_produces_fallback_reply() {
        let engine = CognitiveReActEngine::new(Arc::new(StubCognitiveEngine {
            reply: "default".into(),
        }));
        let ctx = make_context();
        let turn = engine
            .execute_turn(&ctx, vec![])
            .await
            .expect("should succeed");
        match turn {
            ReActTurn::Finished { content, .. } => {
                assert_eq!(content, "default");
            }
            other => panic!("expected Finished, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn execute_tools_returns_error() {
        let engine = CognitiveReActEngine::new(Arc::new(StubCognitiveEngine {
            reply: String::new(),
        }));
        let ctx = make_context();
        let result = engine.execute_tools(&ctx, &[], false).await;
        assert!(result.is_err());
    }

    #[test]
    fn tool_calls_decision_maps_to_tool_calls_turn() {
        let decisions = vec![Decision::call_tools(
            "d1",
            "s1",
            vec![cognitive_engine::ToolCallRequest {
                id: "tc1".into(),
                tool_name: "search".into(),
                args: serde_json::json!({"q": "test"}),
                detach: false,
            }],
        )];
        let turn = CognitiveReActEngine::decisions_to_turn(decisions).expect("should succeed");
        match turn {
            ReActTurn::ToolCalls { calls, .. } => {
                assert_eq!(calls.len(), 1);
                assert_eq!(calls[0].tool_name, "search");
            }
            other => panic!("expected ToolCalls, got {other:?}"),
        }
    }

    #[test]
    fn build_cognitive_context_maps_tools_and_memory() {
        let ctx = make_context();
        let cog_ctx = CognitiveReActEngine::build_cognitive_context(&ctx);
        assert_eq!(cog_ctx.agent_id, "test-agent");
        assert_eq!(cog_ctx.session_id, "test-session");
        assert_eq!(cog_ctx.capabilities.len(), 1);
        assert_eq!(cog_ctx.capabilities[0].name, "search");
        assert_eq!(cog_ctx.memory_context.len(), 1);
        assert_eq!(cog_ctx.memory_context[0].content, "some memory");
    }

    #[test]
    fn messages_to_observations_extracts_user_message() {
        let messages = vec![
            ChatMessage {
                role: ChatMessageRole::User,
                content: "hello".into(),
                tool_call_id: None,
                tool_name: None,
                tool_calls: None,
                reasoning_content: String::new(),
            },
        ];
        let obs = CognitiveReActEngine::messages_to_observations("s1", &messages);
        assert_eq!(obs.len(), 1);
        match &obs[0].payload {
            cognitive_engine::ObservationPayload::UserMessage { text } => {
                assert_eq!(text, "hello");
            }
            _ => panic!("expected UserMessage"),
        }
    }
}
