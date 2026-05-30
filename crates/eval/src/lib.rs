// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! Evaluation system for LLM outputs and agent work items.
//!
//! # Architecture
//!
//! The eval system provides four strategies for scoring agent/LLM outputs:
//!
//! 1. **Rule-based** — substring/regex pattern matching (good for safety checks)
//! 2. **Assertion** — structural JSON assertions (good for tool output validation)
//! 3. **Heuristic** — weighted signal extraction (fast, no LLM cost)
//! 4. **LLM-as-Judge** — use a separate LLM to score output quality
//!
//! # Example
//!
//! ```ignore
//! use eval::{EvalEngine, EvalTarget, EvalConfig};
//!
//! let config = EvalConfig::default();
//! let mut engine = EvalEngine::from_config(&config);
//! // Register strategies...
//!
//! let target = EvalTarget::LlmOutput {
//!     content: "The answer is 42.".into(),
//!     model: Some("deepseek-v4".into()),
//!     turn: 1,
//!     query: Some("What is the answer?".into()),
//! };
//!
//! let results = engine.evaluate(&target).await;
//! ```

pub mod config;
pub mod engine;
pub mod error;
pub mod rule;
pub mod score;
pub mod strategy;
pub mod target;

// Strategies are implemented in submodules:
pub mod strategies;

// Tools and hook for runtime integration:
pub mod hook;
pub mod tools;
