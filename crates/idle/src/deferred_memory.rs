// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! In-memory [`DeferredTaskQueue`] implementation backed by `RwLock<Vec>`.
//!
//! Tasks are lost on restart. Use [`FileDeferredTaskQueue`] for persistence.

use async_trait::async_trait;
use kernel::deferred_task::{DeferredTask, DeferredTaskQueue};
use kernel::AmanResult;
use std::sync::RwLock;

/// In-memory deferred task queue.
///
/// Tasks survive for the lifetime of the process. The backing `Vec` is
/// protected by a [`RwLock`] so both producers (idle boredom system) and
/// consumers (agent harness) can access it concurrently.
pub struct MemoryDeferredTaskQueue {
    name: String,
    tasks: RwLock<Vec<DeferredTask>>,
}

impl MemoryDeferredTaskQueue {
    /// Create a new, empty in-memory queue.
    #[must_use]
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            tasks: RwLock::new(Vec::new()),
        }
    }

    /// Number of tasks currently stored (convenience, same as `pending_count`).
    #[must_use]
    pub fn len(&self) -> usize {
        self.tasks.read().unwrap().len()
    }

    /// Whether the queue is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.tasks.read().unwrap().is_empty()
    }
}

#[async_trait]
impl DeferredTaskQueue for MemoryDeferredTaskQueue {
    fn name(&self) -> &str {
        &self.name
    }

    async fn enqueue(&self, task: DeferredTask) -> AmanResult<String> {
        let id = task.id.clone();
        self.tasks.write().unwrap().push(task);
        Ok(id)
    }

    async fn dequeue(&self, limit: usize) -> AmanResult<Vec<DeferredTask>> {
        let mut tasks = self.tasks.write().unwrap();
        // Highest priority first
        tasks.sort_by(|a, b| b.priority.cmp(&a.priority));
        let count = limit.min(tasks.len());
        let result: Vec<DeferredTask> = tasks.drain(0..count).collect();
        Ok(result)
    }

    async fn mark_complete(&self, task_id: &str, _result: &str) -> AmanResult<bool> {
        let mut tasks = self.tasks.write().unwrap();
        let len_before = tasks.len();
        tasks.retain(|t| t.id != task_id);
        Ok(tasks.len() != len_before)
    }

    async fn mark_failed(&self, task_id: &str, _error: &str) -> AmanResult<bool> {
        let mut tasks = self.tasks.write().unwrap();
        let len_before = tasks.len();
        tasks.retain(|t| t.id != task_id);
        Ok(tasks.len() != len_before)
    }

    async fn pending_count(&self) -> AmanResult<usize> {
        Ok(self.tasks.read().unwrap().len())
    }

    async fn list_pending(&self, limit: usize) -> AmanResult<Vec<DeferredTask>> {
        let tasks = self.tasks.read().unwrap();
        let mut sorted = tasks.clone();
        sorted.sort_by(|a, b| b.priority.cmp(&a.priority));
        sorted.truncate(limit);
        Ok(sorted)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kernel::deferred_task::{current_time_ms, TaskPriority};

    fn make_task(id: &str) -> DeferredTask {
        DeferredTask {
            id: id.to_owned(),
            title: format!("task {id}"),
            description: String::new(),
            source: "test".to_owned(),
            priority: TaskPriority::Normal,
            created_at_ms: current_time_ms(),
            execute_after_ms: None,
            metadata: std::collections::HashMap::new(),
        }
    }

    #[tokio::test]
    async fn enqueue_and_dequeue() {
        let queue = MemoryDeferredTaskQueue::new("test");
        queue.enqueue(make_task("t1")).await.unwrap();
        queue.enqueue(make_task("t2")).await.unwrap();
        assert_eq!(queue.pending_count().await.unwrap(), 2);

        let dequeued = queue.dequeue(1).await.unwrap();
        assert_eq!(dequeued.len(), 1);
        assert_eq!(queue.pending_count().await.unwrap(), 1);
    }

    #[tokio::test]
    async fn dequeue_respects_priority() {
        let queue = MemoryDeferredTaskQueue::new("test");
        let low = DeferredTask {
            priority: TaskPriority::Low,
            ..make_task("low")
        };
        let critical = DeferredTask {
            priority: TaskPriority::Critical,
            ..make_task("critical")
        };
        let high = DeferredTask {
            priority: TaskPriority::High,
            ..make_task("high")
        };

        queue.enqueue(low).await.unwrap();
        queue.enqueue(critical).await.unwrap();
        queue.enqueue(high).await.unwrap();

        let dequeued = queue.dequeue(3).await.unwrap();
        assert_eq!(dequeued[0].priority, TaskPriority::Critical);
        assert_eq!(dequeued[1].priority, TaskPriority::High);
        assert_eq!(dequeued[2].priority, TaskPriority::Low);
    }

    #[tokio::test]
    async fn mark_complete_removes() {
        let queue = MemoryDeferredTaskQueue::new("test");
        queue.enqueue(make_task("t1")).await.unwrap();
        assert!(queue.mark_complete("t1", "done").await.unwrap());
        assert!(!queue.mark_complete("t1", "nonexistent").await.unwrap());
        assert_eq!(queue.pending_count().await.unwrap(), 0);
    }

    #[tokio::test]
    async fn mark_failed_removes() {
        let queue = MemoryDeferredTaskQueue::new("test");
        queue.enqueue(make_task("t1")).await.unwrap();
        assert!(queue.mark_failed("t1", "error").await.unwrap());
        assert_eq!(queue.pending_count().await.unwrap(), 0);
    }

    #[tokio::test]
    async fn list_pending_does_not_remove() {
        let queue = MemoryDeferredTaskQueue::new("test");
        queue.enqueue(make_task("t1")).await.unwrap();
        queue.enqueue(make_task("t2")).await.unwrap();

        let pending = queue.list_pending(10).await.unwrap();
        assert_eq!(pending.len(), 2);
        assert_eq!(queue.pending_count().await.unwrap(), 2); // still there
    }

    #[tokio::test]
    async fn dequeue_ready_filters_by_delay() {
        use kernel::deferred_task::DeferredTaskQueueExt;

        let queue = MemoryDeferredTaskQueue::new("test");
        let ready = make_task("ready");
        let delayed = DeferredTask {
            execute_after_ms: Some(current_time_ms() + 3600_000), // 1 hour from now
            ..make_task("delayed")
        };

        queue.enqueue(ready).await.unwrap();
        queue.enqueue(delayed).await.unwrap();

        let ready_tasks = queue.dequeue_ready(10).await.unwrap();
        assert_eq!(ready_tasks.len(), 1);
        assert_eq!(ready_tasks[0].id, "ready");
    }

    #[tokio::test]
    async fn enqueue_batch() {
        use kernel::deferred_task::DeferredTaskQueueExt;

        let queue = MemoryDeferredTaskQueue::new("test");
        let ids = queue
            .enqueue_batch(vec![make_task("a"), make_task("b"), make_task("c")])
            .await
            .unwrap();
        assert_eq!(ids.len(), 3);
        assert_eq!(queue.pending_count().await.unwrap(), 3);
    }
}
