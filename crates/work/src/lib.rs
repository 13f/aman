#![forbid(unsafe_code)]
#![doc = "Work system for the aman agent framework."]
// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

pub mod config;
pub mod system;
pub mod trace;
pub mod types;

pub use config::{
    ExecutionConfig, HookAction, HookDefinition, HooksConfig, QueueConfig, RetryConfig, WorkConfig,
};
pub use system::WorkSystem;
pub use trace::WorkTraceEvent;
pub use types::{
    IdleSignal, Priority, Step, StepOutput, WorkCheckpoint, WorkContext, WorkError, WorkEvent,
    WorkItem, WorkItemFailedEvent, WorkItemId, WorkItemResult, WorkItemResultEvent,
    WorkItemSource, WorkOutcome, WorkResult, WorkState,
};
