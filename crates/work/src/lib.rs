#![forbid(unsafe_code)]
#![doc = "Work system for the aman agent framework."]
// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

pub mod config;
pub mod personality;
pub mod system;
pub mod trace;
pub mod types;

pub use config::WorkConfig;
pub use personality::{
    DecompositionStrategy, RetryStrategy, TaskSelectionStrategy, WorkPersonality,
};
pub use system::WorkSystem;
pub use trace::WorkTraceEvent;
pub use types::{
    Step, StepOutput, TaskBoardChangeType, TaskBrief, TaskId, TaskResult, WorkCheckpoint,
    WorkContext, WorkError, WorkEvent, WorkOutcome, WorkResult, WorkState,
};
