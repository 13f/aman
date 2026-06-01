// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! Eval-specific error types.

use kernel::Error;
use std::fmt;

/// Errors specific to the evaluation subsystem.
#[derive(Debug)]
pub enum EvalError {
    /// The requested strategy is not registered in the engine.
    StrategyNotFound(String),
    /// The requested rule was not found.
    RuleNotFound(String),
    /// Failed to parse the LLM judge's JSON response.
    JudgeParseError(String),
    /// A required dimension is missing from the evaluation result.
    MissingDimension(String),
    /// Invalid configuration (e.g., threshold out of range, missing fields).
    InvalidConfig(String),
    /// A heuristic extractor referenced an unknown custom extractor name.
    UnknownExtractor(String),
    /// Wraps a kernel-level error.
    Kernel(Error),
}

impl fmt::Display for EvalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::StrategyNotFound(s) => write!(f, "strategy not found: {s}"),
            Self::RuleNotFound(s) => write!(f, "rule not found: {s}"),
            Self::JudgeParseError(s) => write!(f, "judge parse error: {s}"),
            Self::MissingDimension(s) => write!(f, "missing dimension: {s}"),
            Self::InvalidConfig(s) => write!(f, "invalid config: {s}"),
            Self::UnknownExtractor(s) => write!(f, "unknown extractor: {s}"),
            Self::Kernel(e) => write!(f, "kernel error: {e}"),
        }
    }
}

impl std::error::Error for EvalError {}

impl From<Error> for EvalError {
    fn from(e: Error) -> Self {
        Self::Kernel(e)
    }
}
