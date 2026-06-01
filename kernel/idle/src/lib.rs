#![forbid(unsafe_code)]
#![doc = "Idle state system for the aman agent framework."]
// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0


pub mod arousal;
pub mod boredom;
pub mod config;
pub mod coordination;
pub mod deferred_memory;
pub mod detector;
pub mod incubation;
pub mod manager;
pub mod metrics;
pub mod personality;
pub mod types;

pub use types::{
    BoredomActivity, BoredomConfig, ChatMode, IdleContext, IdleEvent, IdleKind,
    IdlePersonality, PressureMapping, QueueDrained, WorkPressureConfig, ArousalBehavior,
};
pub use coordination::IdleCoordination;
pub use manager::AgentIdleManager;
pub use boredom::BoredomActor;
pub use deferred_memory::MemoryDeferredTaskQueue;
