// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! Evaluation hook — automatically evaluates targets when lifecycle events fire.

use std::sync::Arc;

use async_trait::async_trait;
use kernel::context::HookContext;
use kernel::event::{Event, EventType};
use kernel::hook::{Hook, HookPoint};
use kernel::AmanResult;
use tokio::sync::RwLock;

use crate::engine::EvalEngine;
use crate::target::EvalTarget;

/// Callback for publishing evaluation events to the event bus.
///
/// The gateway wires this to [`EventBus::try_publish`] (or similar) so that
/// evaluation results flow into the standard event pipeline.
pub type EvalEventPublisher = Box<dyn Fn(Event) + Send + Sync>;

/// A hook that automatically evaluates agent outputs when lifecycle events fire.
///
/// Register this on the runtime's hook registry with the desired hook points.
/// When triggered, it constructs an `EvalTarget` from the hook context, runs
/// all matching rules through the engine, publishes evaluation results, and
/// emits [`EventType::EvaluationCompleted`] events to the event bus.
pub struct EvalHook {
    engine: Arc<RwLock<EvalEngine>>,
    /// Whether this hook is currently active.
    enabled: bool,
    /// Optional event publisher — if set, `EvaluationCompleted` events are
    /// published after each successful evaluation run.
    event_publisher: Option<EvalEventPublisher>,
}

impl EvalHook {
    /// Create a new eval hook attached to the given engine.
    ///
    /// Use [`Self::with_event_publisher`] to enable event publishing.
    #[must_use]
    pub fn new(engine: Arc<RwLock<EvalEngine>>) -> Self {
        Self {
            engine,
            enabled: true,
            event_publisher: None,
        }
    }

    /// Set the event publisher callback.
    ///
    /// When set, each evaluation run will publish an
    /// [`EventType::EvaluationCompleted`] event carrying the evaluation
    /// results as JSON payload.
    #[must_use]
    pub fn with_event_publisher(mut self, publisher: EvalEventPublisher) -> Self {
        self.event_publisher = Some(publisher);
        self
    }

    /// Enable or disable this hook at runtime.
    pub fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
    }
}

#[async_trait]
impl Hook for EvalHook {
    fn name(&self) -> &str {
        "eval-auto-hook"
    }

    fn priority(&self) -> i32 {
        100 // Run after primary processing
    }

    fn hook_points(&self) -> &[HookPoint] {
        &[
            HookPoint::ToolExecuted,
            HookPoint::PipelineCompleted,
            HookPoint::SkillExecuted,
        ]
    }

    async fn execute(&self, point: HookPoint, ctx: HookContext) -> AmanResult<()> {
        if !self.enabled {
            return Ok(());
        }

        // Build an EvalTarget from the hook context
        let target = match build_target_from_hook(point, &ctx) {
            Some(t) => t,
            None => return Ok(()),
        };

        // Run evaluation asynchronously (don't block the hook chain)
        let engine = self.engine.read().await;
        let results = engine.evaluate(&target).await;
        drop(engine);

        // Store results in the engine
        if !results.is_empty() {
            let mut engine = self.engine.write().await;
            for score in &results {
                engine.store_result(score.clone());
            }
        }

        // Phase 4: publish EvaluationCompleted events to the event bus.
        if let Some(publisher) = &self.event_publisher {
            for score in &results {
                let payload = serde_json::json!({
                    "target_kind": target.kind(),
                    "target_id": target.id(),
                    "rule_id": score.rule_id,
                    "strategy": score.strategy,
                    "aggregate_score": score.aggregate_score,
                    "threshold": score.threshold,
                    "outcome": score.outcome,
                    "dimensions": score.dimensions,
                    "hook_point": format!("{:?}", point),
                });
                let event = Event::new(
                    "eval:hook",
                    EventType::EvaluationCompleted,
                    payload,
                );
                publisher(event);
            }
        }

        Ok(())
    }
}

/// Build an [`EvalTarget`] from a hook point and its context.
fn build_target_from_hook(point: HookPoint, ctx: &HookContext) -> Option<EvalTarget> {
    let exts = &ctx.base.extensions;
    match point {
        HookPoint::ToolExecuted => {
            let tool_name = exts
                .get("tool_name")
                .and_then(|v: &serde_json::Value| v.as_str())
                .unwrap_or("unknown")
                .to_owned();
            let input = exts.get("tool_input").cloned().unwrap_or_default();
            let output = exts.get("tool_output").cloned().unwrap_or_default();

            Some(EvalTarget::ToolResult {
                tool_name,
                input,
                output,
            })
        }
        HookPoint::PipelineCompleted => {
            let pipeline_id = exts
                .get("pipeline_id")
                .and_then(|v: &serde_json::Value| v.as_str())
                .unwrap_or("unknown")
                .to_owned();
            let step = exts
                .get("pipeline_step")
                .and_then(|v: &serde_json::Value| v.as_str())
                .map(String::from);
            let output = exts.get("pipeline_output").cloned().unwrap_or_default();

            Some(EvalTarget::PipelineResult {
                pipeline_id,
                step,
                output,
            })
        }
        HookPoint::SkillExecuted => {
            let label = exts
                .get("skill_name")
                .and_then(|v: &serde_json::Value| v.as_str())
                .unwrap_or("unknown")
                .to_owned();
            let content = exts.get("skill_output").cloned().unwrap_or_default();

            Some(EvalTarget::Custom { label, content })
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::EvalEngine;
    #[test]
    fn hook_points_are_correct() {
        let engine = Arc::new(RwLock::new(EvalEngine::new()));
        let hook = EvalHook::new(engine);
        let points = hook.hook_points();
        assert!(points.contains(&HookPoint::ToolExecuted));
        assert!(points.contains(&HookPoint::PipelineCompleted));
        assert!(points.contains(&HookPoint::SkillExecuted));
    }

    #[tokio::test]
    async fn hook_disabled_returns_early() {
        let engine = Arc::new(RwLock::new(EvalEngine::new()));
        let mut hook = EvalHook::new(engine);
        hook.set_enabled(false);

        let ctx = HookContext::default();

        // Should not panic even with empty context when disabled
        let _result = hook.execute(HookPoint::ToolExecuted, ctx).await;
    }
}
