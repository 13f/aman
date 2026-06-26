// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! File-backed [`DeferredTaskQueue`] implementation.
//!
//! Each task is persisted as a single JSON file under
//! `{data_dir}/{task_id}.json`. Completed/failed tasks are moved to
//! `completed/` and `failed/` subdirectories for auditability.
//!
//! Writes use [`kernel::fs::atomic_write`] for crash safety.

use async_trait::async_trait;
use kernel::deferred_task::{DeferredTask, DeferredTaskQueue};
use kernel::AmanResult;
use std::fs;
use std::path::{Path, PathBuf};

pub struct FileDeferredTaskQueue {
    name: String,
    data_dir: PathBuf,
}

impl FileDeferredTaskQueue {
    /// Open (or create) the deferred task store rooted at `data_dir`.
    ///
    /// Creates the `completed/` and `failed/` subdirectories for archival.
    pub fn open(data_dir: &Path) -> AmanResult<Self> {
        fs::create_dir_all(data_dir)?;
        fs::create_dir_all(data_dir.join("completed"))?;
        fs::create_dir_all(data_dir.join("failed"))?;
        Ok(Self {
            name: "file".to_owned(),
            data_dir: data_dir.to_owned(),
        })
    }

    // ── internal helpers ──────────────────────────────────────────────

    fn task_path(&self, task_id: &str) -> PathBuf {
        self.data_dir.join(format!("{task_id}.json"))
    }

    fn completed_path(&self, task_id: &str) -> PathBuf {
        self.data_dir.join("completed").join(format!("{task_id}.json"))
    }

    fn failed_path(&self, task_id: &str) -> PathBuf {
        self.data_dir.join("failed").join(format!("{task_id}.json"))
    }

    fn read_task(path: &Path) -> Option<DeferredTask> {
        let data = fs::read_to_string(path).ok()?;
        serde_json::from_str(&data).ok()
    }

    /// List all `.json` task files (excludes `completed/` and `failed/`
    /// subdirectories).
    fn list_task_files(&self) -> AmanResult<Vec<PathBuf>> {
        let mut files = Vec::new();
        if !self.data_dir.exists() {
            return Ok(files);
        }
        for entry in fs::read_dir(&self.data_dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.is_file()
                && path.extension().is_some_and(|e| e == "json")
                && !path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with(".tmp."))
            {
                files.push(path);
            }
        }
        Ok(files)
    }
}

#[async_trait]
impl DeferredTaskQueue for FileDeferredTaskQueue {
    fn name(&self) -> &str {
        &self.name
    }

    async fn enqueue(&self, task: DeferredTask) -> AmanResult<String> {
        let path = self.task_path(&task.id);
        let json = serde_json::to_string_pretty(&task)?;
        kernel::fs::atomic_write(&path, json.as_bytes())?;
        Ok(task.id)
    }

    async fn dequeue(&self, limit: usize) -> AmanResult<Vec<DeferredTask>> {
        let files = self.list_task_files()?;
        let mut tasks: Vec<DeferredTask> =
            files.iter().filter_map(|p| Self::read_task(p)).collect();
        tasks.sort_by_key(|b| std::cmp::Reverse(b.priority));
        tasks.truncate(limit);
        // Remove dequeued files from disk
        for task in &tasks {
            let _ = fs::remove_file(self.task_path(&task.id));
        }
        Ok(tasks)
    }

    async fn mark_complete(&self, task_id: &str, _result: &str) -> AmanResult<bool> {
        let path = self.task_path(task_id);
        if !path.exists() {
            return Ok(false);
        }
        let dest = self.completed_path(task_id);
        fs::rename(&path, &dest)?;
        Ok(true)
    }

    async fn mark_failed(&self, task_id: &str, _error: &str) -> AmanResult<bool> {
        let path = self.task_path(task_id);
        if !path.exists() {
            return Ok(false);
        }
        let dest = self.failed_path(task_id);
        fs::rename(&path, &dest)?;
        Ok(true)
    }

    async fn pending_count(&self) -> AmanResult<usize> {
        Ok(self.list_task_files()?.len())
    }

    async fn list_pending(&self, limit: usize) -> AmanResult<Vec<DeferredTask>> {
        let files = self.list_task_files()?;
        let mut tasks: Vec<DeferredTask> =
            files.iter().filter_map(|p| Self::read_task(p)).collect();
        tasks.sort_by_key(|b| std::cmp::Reverse(b.priority));
        tasks.truncate(limit);
        Ok(tasks)
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

    fn temp_dir() -> PathBuf {
        std::env::temp_dir().join(format!("deferred_test_{}", uuid::Uuid::now_v7()))
    }

    #[test]
    fn enqueue_and_dequeue() {
        let dir = temp_dir();
        let queue = FileDeferredTaskQueue::open(&dir).unwrap();
        pollster::block_on(queue.enqueue(make_task("t1"))).unwrap();
        pollster::block_on(queue.enqueue(make_task("t2"))).unwrap();
        assert_eq!(pollster::block_on(queue.pending_count()).unwrap(), 2);

        let dequeued = pollster::block_on(queue.dequeue(1)).unwrap();
        assert_eq!(dequeued.len(), 1);
        assert_eq!(pollster::block_on(queue.pending_count()).unwrap(), 1);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn mark_complete_moves_file() {
        let dir = temp_dir();
        let queue = FileDeferredTaskQueue::open(&dir).unwrap();
        pollster::block_on(queue.enqueue(make_task("t1"))).unwrap();
        assert!(pollster::block_on(queue.mark_complete("t1", "done")).unwrap());
        assert!(!pollster::block_on(queue.mark_complete("t1", "nonexistent")).unwrap());
        assert!(dir.join("completed").join("t1.json").exists());
        assert!(!dir.join("t1.json").exists());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn mark_failed_moves_file() {
        let dir = temp_dir();
        let queue = FileDeferredTaskQueue::open(&dir).unwrap();
        pollster::block_on(queue.enqueue(make_task("t1"))).unwrap();
        assert!(pollster::block_on(queue.mark_failed("t1", "error")).unwrap());
        assert!(dir.join("failed").join("t1.json").exists());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn dequeue_respects_priority() {
        let dir = temp_dir();
        let queue = FileDeferredTaskQueue::open(&dir).unwrap();
        pollster::block_on(queue.enqueue(DeferredTask {
            priority: TaskPriority::Low,
            ..make_task("low")
        }))
        .unwrap();
        pollster::block_on(queue.enqueue(DeferredTask {
            priority: TaskPriority::Critical,
            ..make_task("critical")
        }))
        .unwrap();

        let dequeued = pollster::block_on(queue.dequeue(2)).unwrap();
        assert_eq!(dequeued[0].priority, TaskPriority::Critical);
        assert_eq!(dequeued[1].priority, TaskPriority::Low);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn list_pending_does_not_remove() {
        let dir = temp_dir();
        let queue = FileDeferredTaskQueue::open(&dir).unwrap();
        pollster::block_on(queue.enqueue(make_task("t1"))).unwrap();

        let pending = pollster::block_on(queue.list_pending(10)).unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pollster::block_on(queue.pending_count()).unwrap(), 1); // still on disk
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn persists_across_reopen() {
        let dir = temp_dir();
        {
            let queue = FileDeferredTaskQueue::open(&dir).unwrap();
            pollster::block_on(queue.enqueue(make_task("persist"))).unwrap();
        }
        {
            let queue = FileDeferredTaskQueue::open(&dir).unwrap();
            assert_eq!(pollster::block_on(queue.pending_count()).unwrap(), 1);
            let pending = pollster::block_on(queue.list_pending(1)).unwrap();
            assert_eq!(pending[0].id, "persist");
        }
        fs::remove_dir_all(&dir).unwrap();
    }
}
