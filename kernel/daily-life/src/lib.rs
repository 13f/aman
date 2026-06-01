#![forbid(unsafe_code)]
#![doc = "Daily-life system for the aman agent framework."]
// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

pub mod config;
pub mod spec;
pub mod system;
pub mod trace;
pub mod types;

pub use config::{DailyLifeConfig, RoutinesPerWindow};
pub use system::DailyLifeSystem;
pub use trace::DailyTraceEvent;
pub use types::{
    DailyContext, DailyError, DailyEvent, DailyItem, DailyItemId, DailyItemOutcome,
    DailyItemSource, DailyResult, DailyState, IdleSignal, Priority, Routine, RoutineAction,
    RoutinePriority, StepOutput, TimeWindow,
};
