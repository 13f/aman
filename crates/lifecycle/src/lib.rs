#![forbid(unsafe_code)]
#![doc = "Shared lifecycle engine for work, study, and daily-life systems."]
// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

pub mod config;
pub mod engine;
pub mod spec;
pub mod types;

pub use config::{
    ExecutionConfig, HookAction, HookDefinition, HooksConfig, QueueConfig, RetryConfig,
};
pub use engine::LifecycleEngine;
pub use spec::SystemSpec;
pub use types::{
    Checkpoint, IdleSignal, ItemId, LifecycleContext, LifecycleError, LifecycleResult,
    LifecycleState, Priority, StepOutput,
};
