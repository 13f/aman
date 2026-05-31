//! Thread-safe interrupt flag for the ReAct loop and long-running tool operations.
//!
//! Used to signal that an agent session should stop processing — e.g. user `/stop`
//! or shutdown. The flag is shared between the harness (which checks it) and the
//! gateway (which sets it on user command).

use std::sync::atomic::{AtomicBool, Ordering};

/// Thread-safe flag for interrupting the ReAct loop or long-running operations.
///
/// Shared via `Arc<InterruptFlag>` between the agent harness (reader) and the
/// session manager (writer, triggered by user `/stop`).
#[derive(Debug, Default)]
pub struct InterruptFlag {
    interrupted: AtomicBool,
}

impl InterruptFlag {
    pub fn new() -> Self {
        Self {
            interrupted: AtomicBool::new(false),
        }
    }

    /// Signal interruption.
    pub fn interrupt(&self) {
        self.interrupted.store(true, Ordering::Release);
    }

    /// Check if interruption was signaled.
    pub fn is_interrupted(&self) -> bool {
        self.interrupted.load(Ordering::Acquire)
    }

    /// Reset the flag (e.g. for reuse in a new session).
    pub fn reset(&self) {
        self.interrupted.store(false, Ordering::Release);
    }
}
