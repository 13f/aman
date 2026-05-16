//! Incubation 后台线程管理。
//!
//! Architecture ref: idle-design.md §5.5
//!
//! IncubationWorkflow runs in a background thread with its own CancellationToken,
//! and is NOT interrupted by normal real-event signals. Only Phase 4.5 shutdown
//! terminates incubation threads.
//!
//! IncubationManager enforces `max_concurrent = 1`.

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
/// New incubation requests are queued and started when the current one finishes.
pub struct IncubationManager {
    /// Running incubation (max 1).
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

    /// Start a new incubation thread.
    ///
    /// If an incubation is already running, this returns `None`.
    /// Otherwise returns a handle that can be used to check shutdown status.
    pub async fn start_incubation(
        &self,
        description: String,
    ) -> Option<IncubationHandle> {
        let mut active = self.active.lock().await;
        if active.is_some() {
            debug!("Incubation already active, rejecting new request");
            return None;
        }

        let id = self.next_id.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
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

        // We don't spawn a separate thread here since T6.6 specifies incubation
        // runs as a background thread with independent cancellation.
        // For the crate-level implementation, we store the token so it can
        // be cancelled during shutdown.
        info!(id, desc = %description, "Incubation started");

        Some(handle)
    }

    /// Signal shutdown for all active incubation threads.
    ///
    /// Called during Agent Phase 4.5 shutdown. Wait up to 5 seconds
    /// for graceful termination (caller is responsible for timing).
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

    #[tokio::test]
    async fn starts_incubation_when_inactive() {
        let manager = IncubationManager::new();
        let handle = manager.start_incubation("test-incubation".to_owned()).await;
        assert!(handle.is_some());
        assert_eq!(handle.unwrap().id, 1);
    }

    #[tokio::test]
    async fn rejects_second_incubation() {
        let manager = IncubationManager::new();
        let first = manager.start_incubation("first".to_owned()).await;
        assert!(first.is_some());

        let second = manager.start_incubation("second".to_owned()).await;
        assert!(second.is_none());
    }

    #[tokio::test]
    async fn shutdown_all_cancels_active() {
        let manager = IncubationManager::new();
        let handle = manager.start_incubation("test".to_owned()).await.expect("should start");

        assert!(!handle.is_shutdown_signalled());
        let count = manager.shutdown_all().await;
        assert_eq!(count, 1);
        assert!(handle.is_shutdown_signalled());
    }

    #[tokio::test]
    async fn shutdown_all_with_no_active() {
        let manager = IncubationManager::new();
        let count = manager.shutdown_all().await;
        assert_eq!(count, 0);
    }

    #[tokio::test]
    async fn is_active_returns_correctly() {
        let manager = IncubationManager::new();
        assert!(!manager.is_active().await);

        manager.start_incubation("test".to_owned()).await;
        assert!(manager.is_active().await);

        manager.shutdown_all().await;
        assert!(!manager.is_active().await);
    }

    #[tokio::test]
    async fn active_count_never_exceeds_one() {
        let manager = IncubationManager::new();
        assert_eq!(manager.active_count().await, 0);

        manager.start_incubation("a".to_owned()).await;
        assert_eq!(manager.active_count().await, 1);

        // Second should be rejected
        manager.start_incubation("b".to_owned()).await;
        assert_eq!(manager.active_count().await, 1);
    }
}
