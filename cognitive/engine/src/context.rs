// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! Cognitive context — the environment in which a cognitive engine operates.

use cognitive_react::ChatMessage;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Context for a cognitive engine processing cycle.
///
/// This provides the engine with everything it needs to know about the
/// agent, the session, and the available capabilities — without assuming
/// any specific model architecture.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CognitiveContext {
    /// The agent's unique identifier.
    pub agent_id: String,
    /// The session identifier.
    pub session_id: String,
    /// The agent's identity and boundaries (engine-agnostic).
    pub identity: CognitiveIdentity,
    /// Available capabilities (tools, skills) this agent can use.
    pub capabilities: Vec<Capability>,
    /// Recent context / retrieved memories (engine-agnostic representation).
    pub memory_context: Vec<MemoryItem>,
    /// Conversation history for this session (previous turns).
    ///
    /// Each [`ChatMessage`] represents one turn in the conversation.
    /// Engines use this to maintain dialogue continuity across
    /// multiple `process()` calls.
    pub conversation_history: Vec<ChatMessage>,
    /// Engine-specific configuration blob.
    ///
    /// For LLM engines this might contain model name, temperature, etc.
    /// For world-model engines this might contain latent dimension config.
    pub engine_config: Value,
    /// Grounding assessment — how well-informed the agent is for this task.
    ///
    /// Computed by the gateway after memory retrieval, before engine processing.
    /// The engine reads this to modulate behavior (e.g., skip scout phase when
    /// both Knowledge and Situation are favorable).
    #[serde(default)]
    pub grounding: Grounding,
}

/// Grounding assessment — the agent's "information readiness" for a task.
///
/// Two independent dimensions:
/// - **Knowledge**: Does the agent have relevant domain knowledge?
/// - **Situation**: Is the user's request clear and well-formed?
///
/// These are orthogonal — an agent can know a lot but face a vague question,
/// or face a clear question about an unknown domain.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct Grounding {
    /// Knowledge dimension — does the agent have relevant domain knowledge?
    pub knowledge: KnowledgeSignal,
    /// Situation dimension — is the user's request clear?
    pub situation: SituationSignal,
}

/// Knowledge dimension of grounding.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KnowledgeSignal {
    /// Agent has relevant, fresh knowledge for this domain.
    #[default]
    Informed,
    /// Agent lacks knowledge for this domain.
    Uninformed,
    /// Agent has knowledge but it may be stale.
    Outdated,
}

/// Situation dimension of grounding.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SituationSignal {
    /// User's request is clear and well-formed.
    #[default]
    Clear,
    /// User's request is vague or ambiguous.
    Vague,
    /// Context is overloaded — too much information, goal unclear.
    Overloaded,
}

/// Cognitive state — the agent's capacity to reason right now.
///
/// Driven by `BackendHealth` in the gateway; engines read this to
/// gracefully short-circuit the ReAct loop when the LLM backend is
/// unavailable.
///
/// | Variant      | Meaning                                              |
/// |--------------|------------------------------------------------------|
/// | `Lucid`      | Healthy — LLM backend nominal.                       |
/// | `Groggy`     | Degraded — high latency / elevated error rate.       |
/// | `Catatonic`  | Down — agent perceives events but cannot invoke LLM. |
/// | `Coma`       | Prolonged downtime — even perception is throttled.   |
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[repr(u8)]
#[serde(rename_all = "snake_case")]
pub enum CognitiveState {
    /// LLM backend is healthy; agent may reason freely.
    #[default]
    Lucid = 0,
    /// LLM backend degraded — can still attempt calls (retries absorb).
    Groggy = 1,
    /// LLM backend down — cannot invoke the reasoning engine.
    Catatonic = 2,
    /// Prolonged downtime — operator has been notified.
    Coma = 3,
}

impl CognitiveState {
    /// Convert a raw `u8` (e.g. from an `AtomicU8`) to a `CognitiveState`.
    ///
    /// Unknown values fall back to `Lucid` (fail-safe default).
    pub fn from_u8(v: u8) -> Self {
        match v {
            0 => Self::Lucid,
            1 => Self::Groggy,
            2 => Self::Catatonic,
            3 => Self::Coma,
            _ => Self::Lucid,
        }
    }

    /// Check whether the current cognitive state allows LLM processing.
    ///
    /// Returns `None` if processing is allowed, or `Some(message)` with a
    /// user-facing message explaining why processing was skipped.
    ///
    /// - `Lucid` / `Groggy` → allowed (retry logic handles degradation).
    /// - `Catatonic` / `Coma` → blocked (LLM is unavailable).
    pub fn guard_check(&self) -> Option<&'static str> {
        match self {
            Self::Lucid | Self::Groggy => None,
            Self::Catatonic => Some(
                "I can't think right now — my reasoning engine is unavailable. Please try again shortly.",
            ),
            Self::Coma => Some(
                "I'm unable to process requests right now. My reasoning engine has been down for a while — an operator has been notified.",
            ),
        }
    }

    /// Whether the agent can invoke the LLM at all.
    pub fn can_think(&self) -> bool {
        matches!(self, Self::Lucid | Self::Groggy)
    }
}

/// Source of truth for an agent's [`CognitiveState`].
///
/// Engines call [`Self::state()`] inside [`CognitiveEngine::process()`] to
/// decide whether to enter the ReAct loop or short-circuit with a graceful
/// "unavailable" reply.
///
/// The gateway implements this against its internal `CognitiveStateMachine`;
/// tests use [`FixedConsciousness`].
pub trait ConsciousnessProvider: Send + Sync {
    /// Returns the current cognitive state for this agent.
    fn state(&self) -> CognitiveState;
}

/// Test / placeholder provider — always returns a fixed [`CognitiveState`].
#[derive(Debug, Clone, Copy)]
pub struct FixedConsciousness(pub CognitiveState);

impl ConsciousnessProvider for FixedConsciousness {
    fn state(&self) -> CognitiveState {
        self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guard_check_mapping() {
        use CognitiveState::*;
        assert_eq!(Lucid.guard_check(), None);
        assert_eq!(Groggy.guard_check(), None);
        assert!(Catatonic.guard_check().unwrap().contains("can't think"));
        assert!(Coma.guard_check().unwrap().contains("operator"));
    }

    #[test]
    fn can_think_boundary() {
        use CognitiveState::*;
        assert!(Lucid.can_think());
        assert!(Groggy.can_think());
        assert!(!Catatonic.can_think());
        assert!(!Coma.can_think());
    }

    #[test]
    fn from_u8_roundtrip() {
        use CognitiveState::*;
        for v in [Lucid, Groggy, Catatonic, Coma] {
            assert_eq!(CognitiveState::from_u8(v as u8), v);
        }
        // Unknown → Lucid (fail-safe).
        assert_eq!(CognitiveState::from_u8(99), Lucid);
    }

    #[test]
    fn fixed_consciousness_provider() {
        let p = FixedConsciousness(CognitiveState::Catatonic);
        assert_eq!(p.state(), CognitiveState::Catatonic);
        assert!(p.state().guard_check().is_some());
    }
}

/// Engine-agnostic agent identity.
///
/// Derived from the agent's SOUL.md but rendered in an engine-neutral form.
/// Each engine decides how to translate this into its internal representation
/// (e.g., an LLM engine converts it to a system prompt; a world-model engine
/// converts it to a goal vector).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CognitiveIdentity {
    /// The agent's display name.
    pub name: String,
    /// Core identity statement — who the agent is.
    pub identity: String,
    /// Behavioral boundaries — what the agent must not do.
    pub boundaries: Vec<String>,
    /// Areas of expertise.
    pub expertise: Vec<String>,
    /// Communication style preferences.
    pub vibe: Option<String>,
    /// Raw configuration for engine-specific interpretation.
    pub raw: String,
}

/// A capability available to the agent (tool or skill).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Capability {
    /// Unique name of the capability.
    pub name: String,
    /// Human-readable description.
    pub description: String,
    /// JSON Schema for the capability's parameters.
    pub parameters: Value,
    /// Capability type hint.
    pub cap_type: CapabilityType,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum CapabilityType {
    Tool,
    Skill,
    Other(String),
}

/// A retrieved memory item.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryItem {
    pub key: String,
    pub content: String,
    pub importance: f64,
    pub timestamp: Option<String>,
}

/// Errors that can occur during cognitive processing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum CognitiveError {
    /// The engine encountered an internal error.
    EngineError {
        engine_name: String,
        message: String,
    },
    /// A requested tool is not available.
    ToolNotFound {
        tool_name: String,
    },
    /// A tool execution failed.
    ToolError {
        tool_name: String,
        reason: String,
    },
    /// Resource budget exceeded (tokens, compute, memory, etc.).
    BudgetExceeded {
        resource: String,
        used: u64,
        limit: u64,
    },
    /// Processing was interrupted.
    Interrupted,
    /// Maximum processing depth reached.
    MaxDepthReached {
        depth: u32,
    },
    /// Invalid observation or context.
    InvalidInput {
        reason: String,
    },
}

impl std::fmt::Display for CognitiveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EngineError { engine_name, message } => {
                write!(f, "engine '{engine_name}' error: {message}")
            }
            Self::ToolNotFound { tool_name } => write!(f, "tool '{tool_name}' not found"),
            Self::ToolError { tool_name, reason } => {
                write!(f, "tool '{tool_name}' failed: {reason}")
            }
            Self::BudgetExceeded { resource, used, limit } => {
                write!(f, "{resource} budget exceeded: {used}/{limit}")
            }
            Self::Interrupted => write!(f, "cognitive processing interrupted"),
            Self::MaxDepthReached { depth } => write!(f, "max depth reached: {depth}"),
            Self::InvalidInput { reason } => write!(f, "invalid input: {reason}"),
        }
    }
}

impl std::error::Error for CognitiveError {}
