#![forbid(unsafe_code)]
#![doc = "Study system for the aman agent framework."]
// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

pub mod config;
pub mod spec;
pub mod system;
pub mod trace;
pub mod types;

pub use config::{
    KnowledgeGraphConfig, LearningConfig, MaterialsConfig, SpacedRepetitionConfig, StudyConfig,
};
pub use system::StudySystem;
pub use trace::StudyTraceEvent;
pub use types::{
    IdleSignal, LearningModule, LearningPath, MaterialRef, Priority, StepOutput, StudyContext,
    StudyDepth, StudyError, StudyEvent, StudyItem, StudyItemId, StudyItemSource, StudyNotes,
    StudyOutcome, StudyPhase, StudyResult, StudyState,
};
