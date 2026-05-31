// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

use crate::context::HookContext;
use crate::error::AmanResult;
use crate::event::Event;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HookPoint {
    AgentStarting,
    AgentReady,
    AgentShuttingDown,
    AgentShutdown,
    EventReceived,
    EventPublished,
    EventDiscarded,
    EventRetried,
    DispatcherRouting,
    DispatcherRouted,
    PipelineStarting,
    PipelineStepStarting,
    PipelineStepCompleted,
    PipelineStepFailed,
    PipelineCompleted,
    PipelineFailed,
    SkillLoading,
    SkillLoaded,
    SkillExecuting,
    SkillExecuted,
    ToolExecuting,
    ToolExecuted,
    WorkflowTransitioning,
    WorkflowTransitioned,
    PluginLoading,
    PluginLoaded,
    PluginUnloading,
    PluginUnloaded,
    SourceStarting,
    SourceStarted,
    SourcePaused,
    SourceResumed,
    SourceStopped,
    EvaluationCompleted,
}

/// Minimal event-publishing interface so hooks can push events to the bus
/// without `core` depending on the full `event-bus` crate.
#[async_trait]
pub trait EventPublisher: Send + Sync + std::fmt::Debug {
    /// Publish an event to the bus. Returns an error if the bus is full or
    /// the event is rejected.
    async fn publish(&self, event: Event) -> AmanResult<()>;
}

#[async_trait]
pub trait Hook: Send + Sync {
    fn name(&self) -> &str;
    fn priority(&self) -> i32;
    fn hook_points(&self) -> &[HookPoint];

    async fn execute(&self, point: HookPoint, ctx: HookContext) -> AmanResult<()>;
}
