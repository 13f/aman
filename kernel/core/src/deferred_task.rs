// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! Deferred task queue — trait and shared types for delayed + prioritized
//! task scheduling across memory-backed and file-backed queues.
//!
//! Three implementations share this trait:
//! - `MemoryDeferredTaskQueue` (in `idle`) — in-memory only
//! - `FileDeferredTaskQueue` (in `persistence`) — one JSON file per task

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};
use crate::AmanResult;

/// Priority level for deferred tasks.
///
/// Higher variants sort *before* lower variants in `dequeue()`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum TaskPriority {
    Low,
    #[default]
    Normal,
    High,
    Critical,
}

/// A deferred (delayed / scheduled) task.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeferredTask {
    /// Unique task identifier.
    pub id: String,
    /// Short human-readable title.
    pub title: String,
    /// Longer description or instruction.
    pub description: String,
    /// Origin of the task (e.g. "boredom", "reflection", "manual").
    pub source: String,
    /// Priority — higher values are dequeued first.
    pub priority: TaskPriority,
    /// Creation timestamp (UNIX ms).
    pub created_at_ms: i64,
    /// Execute no earlier than this timestamp (UNIX ms). `None` means immediate.
    pub execute_after_ms: Option<i64>,
    /// Opaque per-task metadata.
    pub metadata: HashMap<String, serde_json::Value>,
}

impl DeferredTask {
    /// Whether this task is ready to execute at the given `now_ms` timestamp.
    #[must_use]
    pub fn is_ready_at(&self, now_ms: i64) -> bool {
        self.execute_after_ms.is_none_or(|ts| now_ms >= ts)
    }

    /// Create a new task with a random UUID v7 id.
    #[must_use]
    pub fn new(title: impl Into<String>, description: impl Into<String>, source: impl Into<String>) -> Self {
        Self {
            id: uuid::Uuid::now_v7().to_string(),
            title: title.into(),
            description: description.into(),
            source: source.into(),
            priority: TaskPriority::default(),
            created_at_ms: current_time_ms(),
            execute_after_ms: None,
            metadata: HashMap::new(),
        }
    }

    /// Set priority (builder-style).
    #[must_use]
    pub fn with_priority(mut self, priority: TaskPriority) -> Self {
        self.priority = priority;
        self
    }

    /// Set delay (builder-style).
    #[must_use]
    pub fn with_delay_ms(mut self, delay_ms: i64) -> Self {
        self.execute_after_ms = Some(current_time_ms() + delay_ms);
        self
    }

    /// Set execute-after timestamp directly (builder-style).
    #[must_use]
    pub fn with_execute_after(mut self, ts_ms: i64) -> Self {
        self.execute_after_ms = Some(ts_ms);
        self
    }

    /// Add metadata key-value (builder-style).
    #[must_use]
    pub fn with_meta(mut self, key: impl Into<String>, value: impl Into<serde_json::Value>) -> Self {
        self.metadata.insert(key.into(), value.into());
        self
    }
}

/// Unified interface for deferred task queues.
#[async_trait]
pub trait DeferredTaskQueue: Send + Sync {
    /// Human-readable name for diagnostics.
    fn name(&self) -> &str;

    /// Enqueue a task. Returns the task ID.
    async fn enqueue(&self, task: DeferredTask) -> AmanResult<String>;

    /// Dequeue up to `limit` pending tasks, highest priority first.
    /// The task is removed from the queue when dequeued.
    async fn dequeue(&self, limit: usize) -> AmanResult<Vec<DeferredTask>>;

    /// Mark a task as completed with a result summary. Returns whether the
    /// task existed.
    async fn mark_complete(&self, task_id: &str, result: &str) -> AmanResult<bool>;

    /// Mark a task as failed with an error message. Returns whether the
    /// task existed.
    async fn mark_failed(&self, task_id: &str, error: &str) -> AmanResult<bool>;

    /// Number of pending tasks.
    async fn pending_count(&self) -> AmanResult<usize>;

    /// List pending tasks (highest priority first), without removing them.
    async fn list_pending(&self, limit: usize) -> AmanResult<Vec<DeferredTask>>;
}

/// Extension trait with default method implementations shared across
/// all [`DeferredTaskQueue`] implementations.
#[async_trait]
pub trait DeferredTaskQueueExt: DeferredTaskQueue {
    /// Dequeue tasks that are ready to execute (filtered by `execute_after_ms`).
    async fn dequeue_ready(&self, limit: usize) -> AmanResult<Vec<DeferredTask>> {
        let now = current_time_ms();
        let tasks = self.dequeue(limit).await?;
        Ok(tasks.into_iter().filter(|t| t.is_ready_at(now)).collect())
    }

    /// Enqueue multiple tasks in a batch.
    async fn enqueue_batch(&self, tasks: Vec<DeferredTask>) -> AmanResult<Vec<String>> {
        let mut ids = Vec::with_capacity(tasks.len());
        for task in tasks {
            ids.push(self.enqueue(task).await?);
        }
        Ok(ids)
    }
}

/// Blanket implementation — all [`DeferredTaskQueue`] impls get the extension
/// methods automatically.
impl<T: DeferredTaskQueue> DeferredTaskQueueExt for T {}

/// Current time in milliseconds since UNIX epoch.
#[must_use]
pub fn current_time_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}
