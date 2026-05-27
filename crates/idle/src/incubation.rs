// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! Incubation 后台线程管理。
//!
//! Architecture ref: idle-design.md §5.5
//!
//! Incubation runs in a background task with its own CancellationToken,
//! and is NOT interrupted by normal real-event signals. Only Phase 4.5 shutdown
//! terminates incubation threads.
//!
//! IncubationManager enforces `max_concurrent = 1`.

use std::future::Future;
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info};

/// Handle to a running incubation thread.
#[derive(Debug)]
pub struct IncubationHandle {
    /// Unique identifier for this incubation run.
    pub id: u64,
    /// Token used to signal shutdown (from Phase 4.5).
    shutdown_token: CancellationToken,
}

impl IncubationHandle {
    /// Returns `true` if the incubation thread has been signalled to shut down.
    #[must_use]
    pub fn is_shutdown_signalled(&self) -> bool {
        self.shutdown_token.is_cancelled()
    }
}

/// Manager for incubation background threads.
///
/// Enforces `max_concurrent = 1` — only one incubation can be active at a time.
pub struct IncubationManager {
    /// Running incubation (max 1). Shared with the spawned task so it can
    /// clear the slot on completion.
    active: Arc<Mutex<Option<ActiveIncubation>>>,
    /// Next ID for a new incubation.
    next_id: std::sync::atomic::AtomicU64,
}

struct ActiveIncubation {
    id: u64,
    shutdown_token: CancellationToken,
    #[allow(dead_code)]
    description: String,
}

impl IncubationManager {
    #[must_use]
    pub fn new() -> Self {
        Self {
            active: Arc::new(Mutex::new(None)),
            next_id: std::sync::atomic::AtomicU64::new(1),
        }
    }

    /// Spawn a new incubation background task.
    ///
    /// If an incubation is already running, returns `None`.
    /// Otherwise spawns the future in a tokio task, stores a
    /// [`CancellationToken`] for shutdown, and returns a handle.
    /// The active slot is automatically cleared when the task completes.
    pub async fn spawn(
        &self,
        description: String,
        future: impl Future<Output = ()> + Send + 'static,
    ) -> Option<IncubationHandle> {
        let mut active = self.active.lock().await;
        if active.is_some() {
            debug!("Incubation already active, rejecting new request");
            return None;
        }

        let id = self
            .next_id
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let shutdown_token = CancellationToken::new();
        let handle = IncubationHandle {
            id,
            shutdown_token: shutdown_token.clone(),
        };

        *active = Some(ActiveIncubation {
            id,
            shutdown_token: shutdown_token.clone(),
            description: description.clone(),
        });

        // Spawn the real background task — clears the active slot on
        // completion, cancellation, or panic.
        let active_clone = Arc::clone(&self.active);
        let desc = description.clone();
        tokio::spawn(async move {
            let handle = tokio::spawn(future);
            let _ = handle.await;
            let mut guard = active_clone.lock().await;
            if let Some(ref incumbent) = *guard
                && incumbent.id == id
            {
                info!(id, desc = %desc, "Incubation background task completed");
                guard.take();
            }
        });

        info!(id, desc = %description, "Incubation spawned");
        Some(handle)
    }

    /// Signal shutdown for all active incubation threads.
    ///
    /// Called during Agent Phase 4.5 shutdown. Cancels the shutdown token;
    /// the background task should check this token and exit gracefully.
    /// Returns the number of incubations cancelled.
    pub async fn shutdown_all(&self) -> usize {
        let mut active = self.active.lock().await;
        if let Some(incubation) = active.take() {
            incubation.shutdown_token.cancel();
            info!(id = incubation.id, "Incubation shutdown signalled");
            1
        } else {
            0
        }
    }

    /// Returns `true` if an incubation thread is currently active.
    #[must_use]
    pub async fn is_active(&self) -> bool {
        self.active.lock().await.is_some()
    }

    /// Returns the ID of the active incubation, if any.
    #[must_use]
    pub async fn active_id(&self) -> Option<u64> {
        self.active.lock().await.as_ref().map(|a| a.id)
    }

    /// Return the number of active incubations (always 0 or 1).
    #[must_use]
    pub async fn active_count(&self) -> usize {
        if self.is_active().await { 1 } else { 0 }
    }
}

impl Default for IncubationManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    async fn noop() {}

    #[tokio::test]
    async fn spawn_when_inactive() {
        let manager = IncubationManager::new();
        let handle = manager.spawn("test".to_owned(), noop()).await;
        assert!(handle.is_some());
        assert_eq!(handle.unwrap().id, 1);
        // Wait for the task to finish
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        assert!(!manager.is_active().await);
    }

    #[tokio::test]
    async fn rejects_second_spawn() {
        // Use a future that blocks so the first incubation stays active
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        let manager = IncubationManager::new();
        let first = manager
            .spawn("first".to_owned(), async move {
                let _ = rx.await;
            })
            .await;
        assert!(first.is_some());

        let second = manager.spawn("second".to_owned(), noop()).await;
        assert!(second.is_none());

        // Clean up
        let _ = tx.send(());
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        assert!(!manager.is_active().await);
    }

    #[tokio::test]
    async fn shutdown_all_cancels_active() {
        let running = Arc::new(AtomicBool::new(true));
        let running_clone = Arc::clone(&running);

        let manager = IncubationManager::new();
        let handle = manager
            .spawn("test".to_owned(), async move {
                while running_clone.load(Ordering::SeqCst) {
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                }
            })
            .await
            .expect("should start");

        assert!(!handle.is_shutdown_signalled());
        let count = manager.shutdown_all().await;
        assert_eq!(count, 1);
        assert!(handle.is_shutdown_signalled());

        // Release the background task
        running.store(false, Ordering::SeqCst);
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        assert!(!manager.is_active().await);
    }

    #[tokio::test]
    async fn shutdown_all_with_no_active() {
        let manager = IncubationManager::new();
        let count = manager.shutdown_all().await;
        assert_eq!(count, 0);
    }

    #[tokio::test]
    async fn is_active_returns_correctly() {
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        let manager = IncubationManager::new();
        assert!(!manager.is_active().await);

        manager
            .spawn("test".to_owned(), async move {
                let _ = rx.await;
            })
            .await;
        assert!(manager.is_active().await);

        // Release and wait for auto-clear
        let _ = tx.send(());
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        assert!(!manager.is_active().await);
    }

    #[tokio::test]
    async fn active_count_never_exceeds_one() {
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        let manager = IncubationManager::new();
        assert_eq!(manager.active_count().await, 0);

        manager
            .spawn("a".to_owned(), async move {
                let _ = rx.await;
            })
            .await;
        assert_eq!(manager.active_count().await, 1);

        // Second should be rejected
        manager.spawn("b".to_owned(), noop()).await;
        assert_eq!(manager.active_count().await, 1);

        let _ = tx.send(());
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        assert_eq!(manager.active_count().await, 0);
    }

    #[tokio::test]
    async fn spawn_auto_clears_on_completion() {
        let manager = IncubationManager::new();
        manager.spawn("auto-clear".to_owned(), noop()).await;
        assert!(manager.is_active().await);

        // Wait for noop to complete
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        assert!(!manager.is_active().await, "active slot should auto-clear after task completes");
    }

    #[tokio::test]
    async fn spawn_auto_clears_even_on_panic() {
        let manager = IncubationManager::new();
        manager
            .spawn("panic-test".to_owned(), async {
                panic!("expected panic in incubation task");
            })
            .await;
        assert!(manager.is_active().await);

        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        // The tokio::spawn catches panics, so the active slot should still be
        // cleared (the future.await returns when the panic unwinds the task)
        assert!(!manager.is_active().await, "active slot should auto-clear after panic");
    }
}
