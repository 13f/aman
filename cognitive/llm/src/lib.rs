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
//!     ├── Run ReAct loop (LlmReActEngine)
//!     │   ├── LlmProvider::chat_completion()
//!     │   ├── Execute tools
//!     │   └── Track token budget
//!     └── Convert ReActTurn → Decisions
//! ```

#![forbid(unsafe_code)]

pub mod anthropic;
pub mod delegate_task;
pub mod embed;
pub(crate) mod net_proxy;
pub mod openai;
pub mod prompt;
pub mod provider;
pub mod react;
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
}

impl Default for LlmEngineConfig {
    fn default() -> Self {
        Self {
            model: "gpt-4o".into(),
            max_turns: 64,
            token_limit: 128_000,
            max_output_tokens: 4096,
        }
    }
}

/// An LLM-based cognitive engine.
///
/// Wraps an `LlmProvider`, a `PromptPipeline`, and a ReAct loop to
/// implement the `CognitiveEngine` trait. This is the "brain" that
/// powers aman agents today.
pub struct LlmCognitiveEngine {
    provider: Arc<dyn LlmProvider>,
    prompt_pipeline: Arc<dyn PromptPipeline>,
    config: LlmEngineConfig,
    listeners: Arc<Mutex<Vec<Arc<dyn CognitiveListener>>>>,
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
        }
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
    /// Exposed as `pub` so that streaming integrations (and the contract
    /// tests in `tests/cognitive_engine_contract.rs`) can drive the
    /// listener registry directly. Production callers should treat this
    /// as a building block for a future streaming PR — `process()` does
    /// not yet invoke `emit` automatically.
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

        let session_id = &ctx.session_id;

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

        // Build messages from observations
        let messages = Self::observations_to_messages(&observations, &[]);

        // Build the LLM request
        let request = LlmChatRequest {
            model: self.config.model.clone(),
            system_prompt: soul.system_prompt.clone(),
            messages,
            tools,
            max_output_tokens: self.config.max_output_tokens as u32,
            response_format: None,
        };

        // Call the LLM provider
        let response = self
            .provider
            .chat_completion(request, None)
            .await
            .map_err(|e| CognitiveError::EngineError {
                engine_name: self.name().to_owned(),
                message: e,
            })?;

        // ── Output validation (security harness §8.2) ────────────────
        // Validate LLM response for secret leaks, system prompt disclosure,
        // and tool injection before converting to decisions.
        let content = {
            let mut validator = kernel::validator::OutputValidator::new();
            match validator.validate(&response.content, kernel::types::TrustLevel::Untrusted) {
                kernel::validator::ValidationOutcome::Pass => response.content,
                kernel::validator::ValidationOutcome::Fail { reason, .. } => {
                    tracing::warn!(
                        session_id = %session_id,
                        reason,
                        "LLM response blocked by output validator (cognitive engine)"
                    );
                    "[I apologize, but I cannot provide that response \
                     as it may contain sensitive information.]"
                        .to_owned()
                }
                kernel::validator::ValidationOutcome::Error { message } => {
                    tracing::error!(
                        session_id = %session_id,
                        error = %message,
                        "output validator error (fail-closed, cognitive engine)"
                    );
                    return Err(CognitiveError::EngineError {
                        engine_name: self.name().to_owned(),
                        message: format!("output validation error: {message}"),
                    });
                }
            }
        };

        // Convert to decisions
        let turn = if !response.tool_calls.is_empty() {
            ReActTurn::ToolCalls {
                content,
                calls: response.tool_calls,
                reasoning_content: response.reasoning_content,
            }
        } else {
            ReActTurn::Finished {
                content,
                finish_reason: response.finish_reason,
            }
        };

        Ok(Self::turn_to_decisions(turn, session_id))
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
