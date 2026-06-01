// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! Cognitive Engine abstraction for aman.
//!
//! This crate defines the `CognitiveEngine` trait — the contract between
//! the agent gateway and any "brain" implementation (LLM, world model,
//! hybrid system, etc.).
//!
//! # Architecture
//!
//! ```text
//! EventBus → Observation → CognitiveEngine::process() → Vec<Decision> → EventBus
//!                              ↑
//!                     CognitiveContext
//!                     (identity, capabilities, memory, engine_config)
//! ```
//!
//! The engine is a pure function from observations to decisions. It does
//! not interact with the event bus, tools, or memory directly — those are
//! provided by the gateway through the context and observation streams.

#![forbid(unsafe_code)]

pub mod context;
pub mod decision;
pub mod observation;

use async_trait::async_trait;
use std::sync::Arc;

pub use context::{Capability, CapabilityType, CognitiveContext, CognitiveError, CognitiveIdentity, MemoryItem};
pub use decision::{Decision, DecisionKind, ToolCallRequest};
pub use observation::{Observation, ObservationPayload};

/// A listener for intermediate cognitive events.
///
/// Engines can emit streaming updates (text chunks, reasoning traces,
/// confidence scores) during processing. The gateway subscribes to these
/// and forwards them to the event bus for real-time user feedback.
pub trait CognitiveListener: Send + Sync {
    /// Called when the engine emits an intermediate event.
    fn on_cognitive_event(&self, event: CognitiveEvent);
}

/// Intermediate events emitted during cognitive processing.
#[derive(Debug, Clone)]
pub enum CognitiveEvent {
    /// A text chunk from a streaming response.
    TextChunk {
        session_id: String,
        text: String,
    },
    /// Streaming has started for a session.
    StreamStart {
        session_id: String,
    },
    /// Streaming has completed for a session.
    StreamDone {
        session_id: String,
        finish_reason: String,
    },
    /// An error occurred during streaming.
    StreamError {
        session_id: String,
        error: String,
    },
    /// Engine-specific diagnostic event.
    Diagnostic {
        session_id: String,
        engine_name: String,
        data: serde_json::Value,
    },
}

/// A cognitive engine is an agent's "brain" — it receives observations
/// from the event bus, deliberates, and produces decisions.
///
/// # Design
///
/// This trait abstracts over the underlying model type. Today's
/// implementation is LLM-based ([`LlmCognitiveEngine`] in `cognitive-llm`),
/// but future implementations could use world models, hybrid systems,
/// or other architectures — all behind the same trait.
///
/// # Lifecycle
///
/// 1. The gateway creates an engine instance per agent at startup.
/// 2. When events arrive for an agent, the gateway packages them as
///    `Observation`s and calls `process()`.
/// 3. The engine returns `Decision`s, which the gateway translates
///    into events on the event bus (tool calls, replies, etc.).
/// 4. Tool results arrive as new `Observation::ToolCompleted`, and
///    the cycle continues.
///
/// The engine may maintain internal state across `process()` calls
/// (conversation history, world state, etc.) — that is opaque to
/// the gateway.
#[async_trait]
pub trait CognitiveEngine: Send + Sync {
    /// Unique identifier for this engine type (e.g. "llm-openai", "world-model-v1").
    fn name(&self) -> &str;

    /// Process observations and produce decisions.
    ///
    /// The engine receives a batch of observations and the current cognitive
    /// context, and returns zero or more decisions. The engine may:
    ///
    /// - Run multi-turn reasoning internally (e.g., ReAct loop)
    /// - Manage its own resource budgets
    /// - Emit streaming events via registered listeners
    /// - Maintain internal state across calls
    ///
    /// All of this is opaque to the caller — the gateway only sees
    /// observations in and decisions out.
    async fn process(
        &self,
        ctx: &CognitiveContext,
        observations: Vec<Observation>,
    ) -> Result<Vec<Decision>, CognitiveError>;

    /// Subscribe to intermediate cognitive events.
    ///
    /// The listener receives streaming text chunks, reasoning traces,
    /// and diagnostic events during `process()`.
    fn subscribe(&self, listener: Arc<dyn CognitiveListener>);

    /// Unsubscribe a previously registered listener.
    fn unsubscribe(&self, listener: &Arc<dyn CognitiveListener>);

    /// Reset the engine's internal state for a given session.
    ///
    /// This clears conversation history, latent state, or any other
    /// session-scoped data. Called when a session ends or is interrupted.
    async fn reset_session(&self, session_id: &str) -> Result<(), CognitiveError>;
}
