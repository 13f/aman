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
pub use personality::WorkPersonality;
pub use system::WorkSystem;
pub use trace::WorkTraceEvent;
pub use types::{
    IdleSignal, Priority, Step, StepOutput, WorkCheckpoint, WorkContext, WorkError, WorkEvent,
    WorkItem, WorkItemId, WorkItemResult, WorkItemSource, WorkOutcome, WorkResult, WorkState,
};
