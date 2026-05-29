// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! Shared types for the lifecycle engine — used by work, study, and daily-life systems.

use kernel::types::Timestamp;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::time::Duration;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// ItemId — universal newtype for all lifecycle systems
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ItemId(pub Uuid);

impl ItemId {
    #[must_use]
    pub fn new() -> Self {
        Self(Uuid::now_v7())
    }
}

impl Default for ItemId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for ItemId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

// ---------------------------------------------------------------------------
// Priority
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Priority {
    Low = 1,
    #[default]
    Normal = 2,
    High = 3,
    Critical = 4,
}

// ---------------------------------------------------------------------------
// StepOutput — universal across all lifecycle systems
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepOutput {
    pub success: bool,
    pub summary: String,
    #[serde(default)]
    pub artifacts: Vec<String>,
    pub duration: Duration,
}

// ---------------------------------------------------------------------------
// LifecycleError / LifecycleResult
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LifecycleError {
    pub code: String,
    pub message: String,
    pub retryable: bool,
}

pub type LifecycleResult<T> = Result<T, LifecycleError>;

// ---------------------------------------------------------------------------
// IdleSignal — feedback to Idle System
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IdleSignal {
    Satisfaction { item_id: ItemId },
    Frustration { reason: Option<String> },
}

// ---------------------------------------------------------------------------
// Checkpoint
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Checkpoint {
    pub item_id: Option<ItemId>,
    pub step_index: usize,
    pub timestamp: Timestamp,
    pub reason: String,
}

// ---------------------------------------------------------------------------
// Generic lifecycle context
// ---------------------------------------------------------------------------

/// Generic context shared by all lifecycle systems.
///
/// Parameterized over item type `I` and step type `St`.
#[derive(Debug, Clone)]
pub struct LifecycleContext<I: Clone, St: Clone> {
    pub state: LifecycleState,
    pub queue: VecDeque<I>,
    pub current: Option<I>,
    pub steps: Vec<St>,
    pub step_index: usize,
    pub step_outputs: Vec<StepOutput>,
}

impl<I: Clone, St: Clone> LifecycleContext<I, St> {
    #[must_use]
    pub fn new() -> Self {
        Self {
            state: LifecycleState::Idle,
            queue: VecDeque::new(),
            current: None,
            steps: Vec::new(),
            step_index: 0,
            step_outputs: Vec::new(),
        }
    }

    pub fn enqueue(&mut self, item: I) {
        self.queue.push_back(item);
    }

    pub fn dequeue(&mut self) -> Option<I> {
        self.queue.pop_front()
    }

    pub fn push_front(&mut self, item: I) {
        self.queue.push_front(item);
    }

    pub fn reset_to_idle(&mut self) {
        self.state = LifecycleState::Idle;
        self.current = None;
        self.steps.clear();
        self.step_index = 0;
        self.step_outputs.clear();
    }

    pub fn interrupt(&mut self, reason: &str) -> Checkpoint {
        let item_id = self.current.as_ref().map(|_| ItemId::new());
        let checkpoint = Checkpoint {
            item_id,
            step_index: self.step_index,
            timestamp: Timestamp::now(),
            reason: reason.to_string(),
        };
        self.reset_to_idle();
        checkpoint
    }

    #[must_use]
    pub fn is_idle(&self) -> bool {
        self.state == LifecycleState::Idle
    }

    #[must_use]
    pub fn queue_len(&self) -> usize {
        self.queue.len()
    }
}

impl<I: Clone, St: Clone> Default for LifecycleContext<I, St> {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// LifecycleState — the universal 2-state enum
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LifecycleState {
    Idle,
    Busy,
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn item_id_default_is_v7() {
        let id = ItemId::default();
        assert_eq!(id.0.get_version_num(), 7);
    }

    #[test]
    fn item_id_display() {
        let id = ItemId::new();
        assert_eq!(format!("{id}"), id.0.to_string());
    }

    #[test]
    fn priority_ordering() {
        assert!(Priority::Critical > Priority::High);
        assert!(Priority::High > Priority::Normal);
        assert!(Priority::Normal > Priority::Low);
    }

    // Dummy step type for tests
    #[derive(Debug, Clone)]
    struct TestStep {
        index: usize,
    }

    #[test]
    fn context_new_is_idle() {
        let ctx = LifecycleContext::<String, TestStep>::new();
        assert!(ctx.is_idle());
        assert!(ctx.current.is_none());
        assert!(ctx.queue.is_empty());
        assert!(ctx.steps.is_empty());
        assert_eq!(ctx.step_index, 0);
    }

    #[test]
    fn context_enqueue_dequeue_fifo() {
        let mut ctx = LifecycleContext::<String, TestStep>::new();
        ctx.enqueue("a".to_string());
        ctx.enqueue("b".to_string());
        assert_eq!(ctx.queue_len(), 2);
        assert_eq!(ctx.dequeue(), Some("a".to_string()));
        assert_eq!(ctx.dequeue(), Some("b".to_string()));
        assert!(ctx.dequeue().is_none());
    }

    #[test]
    fn context_push_front() {
        let mut ctx = LifecycleContext::<String, TestStep>::new();
        ctx.enqueue("a".to_string());
        ctx.push_front("b".to_string());
        assert_eq!(ctx.dequeue(), Some("b".to_string()));
        assert_eq!(ctx.dequeue(), Some("a".to_string()));
    }

    #[test]
    fn context_reset_to_idle_clears_all() {
        let mut ctx = LifecycleContext::<String, TestStep>::new();
        ctx.state = LifecycleState::Busy;
        ctx.current = Some("item".to_string());
        ctx.steps = vec![TestStep { index: 0 }];
        ctx.step_index = 3;
        ctx.step_outputs = vec![StepOutput {
            success: true,
            summary: "done".into(),
            artifacts: vec![],
            duration: Duration::from_secs(1),
        }];
        ctx.reset_to_idle();
        assert!(ctx.is_idle());
        assert!(ctx.current.is_none());
        assert!(ctx.steps.is_empty());
        assert_eq!(ctx.step_index, 0);
        assert!(ctx.step_outputs.is_empty());
    }

    #[test]
    fn context_interrupt_saves_checkpoint() {
        let mut ctx = LifecycleContext::<String, TestStep>::new();
        ctx.state = LifecycleState::Busy;
        ctx.current = Some("task".to_string());
        ctx.step_index = 5;

        let checkpoint = ctx.interrupt("test_reason");
        assert_eq!(checkpoint.step_index, 5);
        assert!(ctx.is_idle());
    }

    #[test]
    fn lifecycle_state_serde_roundtrip() {
        for state in &[LifecycleState::Idle, LifecycleState::Busy] {
            let json = serde_json::to_string(state).expect("serialize");
            let deser: LifecycleState = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(*state, deser);
        }
    }

    #[test]
    fn idle_signal_serde() {
        let sig = IdleSignal::Satisfaction {
            item_id: ItemId::new(),
        };
        let json = serde_json::to_string(&sig).expect("serialize");
        let deser: IdleSignal = serde_json::from_str(&json).expect("deserialize");
        match deser {
            IdleSignal::Satisfaction { .. } => {}
            _ => panic!("wrong variant"),
        }
    }
}
