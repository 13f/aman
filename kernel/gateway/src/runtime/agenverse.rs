// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! The gateway's independent lifecycle state — the "agenverse" (agents universe).
//!
//! [`Agenverse`] owns the phase/status/lock triad that gates `start()` and
//! `shutdown()` on [`AgentRuntime`](super::agent_runtime::AgentRuntime). It
//! represents the world-level lifecycle: the gateway process exists
//! independently of any individual agent — agents are the life within the
//! agenverse, not the universe itself.

use kernel::Error;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;
use tokio::sync::{Mutex, RwLock};

use super::agent_runtime::AgentRuntime;

// ── Runtime phase ────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimePhase {
    Phase0 = 0,
    Phase05 = 1,
    Phase1 = 2,
    Phase2 = 3,
    Phase3 = 4,
    Phase4 = 5,
    Phase5 = 6,
}

// ── Runtime status ───────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeStatus {
    New,
    Starting,
    Ready,
    ShuttingDown,
    Shutdown,
}

// ── Agenverse ────────────────────────────────────────────────────────────────

/// The gateway's independent lifecycle state — the "agenverse" (agents universe).
///
/// This struct owns the phase/status/lock triad that gates `start()` and
/// `shutdown()`. It represents the world-level lifecycle: the gateway process
/// exists independently of any individual agent — agents are the life within
/// the agenverse, not the universe itself.
///
/// `Agenverse` is the top-level container created first by `main()`. After
/// [`AgentRuntime`] is built, it is stored via [`set_runtime`](Self::set_runtime)
/// and retrieved via [`runtime`](Self::runtime). `AgentRuntime` holds an
/// `Arc<Agenverse>` back-reference for lifecycle delegation.
pub struct Agenverse {
    phase: AtomicU8,
    status: RwLock<RuntimeStatus>,
    transition_lock: Mutex<()>,
    shutdown_requested: AtomicBool,
    shutdown_notify: tokio::sync::Notify,
    startup_pause: Duration,
    /// The built agent runtime. Set once after [`AgentRuntimeBuilder`] completes.
    runtime: OnceLock<Arc<AgentRuntime>>,
}

impl Agenverse {
    /// Construct a fresh agenverse in `New` state.
    pub fn new(startup_pause: Duration) -> Self {
        Self {
            phase: AtomicU8::new(RuntimePhase::Phase0 as u8),
            status: RwLock::new(RuntimeStatus::New),
            transition_lock: Mutex::new(()),
            shutdown_requested: AtomicBool::new(false),
            shutdown_notify: tokio::sync::Notify::new(),
            startup_pause,
            runtime: OnceLock::new(),
        }
    }

    /// Current runtime phase.
    #[must_use]
    pub fn phase(&self) -> RuntimePhase {
        match self.phase.load(Ordering::Acquire) {
            x if x == RuntimePhase::Phase0 as u8 => RuntimePhase::Phase0,
            x if x == RuntimePhase::Phase05 as u8 => RuntimePhase::Phase05,
            x if x == RuntimePhase::Phase1 as u8 => RuntimePhase::Phase1,
            x if x == RuntimePhase::Phase2 as u8 => RuntimePhase::Phase2,
            x if x == RuntimePhase::Phase3 as u8 => RuntimePhase::Phase3,
            x if x == RuntimePhase::Phase4 as u8 => RuntimePhase::Phase4,
            x if x == RuntimePhase::Phase5 as u8 => RuntimePhase::Phase5,
            _ => RuntimePhase::Phase0,
        }
    }

    /// Set the runtime phase.
    pub fn set_phase(&self, phase: RuntimePhase) {
        self.phase.store(phase as u8, Ordering::Release);
    }

    /// Current runtime status.
    pub async fn status(&self) -> RuntimeStatus {
        *self.status.read().await
    }

    /// Whether the runtime has reached `Ready` and is therefore live.
    #[must_use]
    pub fn is_ready(&self) -> bool {
        self.phase() == RuntimePhase::Phase5
    }

    /// Whether the runtime is not fully shut down. Returns `true` if the
    /// status lock is currently held, matching the previous `AgentRuntime`
    /// behavior.
    #[must_use]
    pub fn is_live(&self) -> bool {
        match self.status.try_read() {
            Ok(guard) => *guard != RuntimeStatus::Shutdown,
            Err(_) => true,
        }
    }

    /// Configured pause between startup/shutdown phase transitions.
    #[must_use]
    pub fn startup_pause(&self) -> Duration {
        self.startup_pause
    }

    /// Whether a shutdown has been requested (e.g. via HTTP from the desktop
    /// app). The TUI polls this to know when to exit.
    #[must_use]
    pub fn shutdown_requested(&self) -> bool {
        self.shutdown_requested.load(Ordering::Acquire)
    }

    /// Returns a reference to the shutdown-complete notification channel.
    #[must_use]
    pub fn shutdown_notify(&self) -> &tokio::sync::Notify {
        &self.shutdown_notify
    }

    /// Notify waiters that shutdown has completed.
    pub fn notify_shutdown_complete(&self) {
        self.shutdown_notify.notify_one();
    }

    /// Wait for the shutdown-complete notification.
    pub async fn wait_shutdown_complete(&self) {
        self.shutdown_notify.notified().await;
    }

    /// Atomic `New → Starting` gate. Returns `Ok(())` if we acquired the
    /// gate, `Err` if a previous `start()` is in progress, or if the
    /// runtime is already shutting down / shut down.
    pub async fn try_acquire_start_gate(&self) -> Result<(), Error> {
        let _guard = self.transition_lock.lock().await;
        let current = *self.status.read().await;
        match current {
            RuntimeStatus::Ready | RuntimeStatus::Starting => return Ok(()),
            RuntimeStatus::ShuttingDown | RuntimeStatus::Shutdown => {
                return Err(Error::InvalidStateTransition {
                    message: "runtime is shutting down".to_owned(),
                });
            }
            RuntimeStatus::New => {}
        }
        self.shutdown_requested.store(false, Ordering::Release);
        *self.status.write().await = RuntimeStatus::Starting;
        self.phase.store(RuntimePhase::Phase0 as u8, Ordering::Release);
        Ok(())
    }

    /// Atomic `Ready → ShuttingDown` gate. Returns `Ok(())` on success,
    /// `Err(())` if the runtime is already shut down.
    pub async fn try_acquire_shutdown_gate(&self) -> Result<(), ()> {
        self.shutdown_requested.store(true, Ordering::Release);
        let _guard = self.transition_lock.lock().await;
        let current = *self.status.read().await;
        if current == RuntimeStatus::Shutdown {
            return Err(());
        }
        *self.status.write().await = RuntimeStatus::ShuttingDown;
        Ok(())
    }

    /// Mark the runtime as fully shut down.
    pub async fn mark_shutdown(&self) {
        *self.status.write().await = RuntimeStatus::Shutdown;
        self.notify_shutdown_complete();
    }

    /// Mark the runtime as ready after start completes.
    pub async fn mark_ready(&self) {
        *self.status.write().await = RuntimeStatus::Ready;
    }

    /// Access the built [`AgentRuntime`]. Panics if called before
    /// [`set_runtime`](Self::set_runtime).
    #[must_use]
    pub fn runtime(&self) -> &Arc<AgentRuntime> {
        self.runtime
            .get()
            .expect("Agenverse::runtime() called before AgentRuntime was set")
    }

    /// Store the [`AgentRuntime`] after it is built. Panics if called more
    /// than once.
    pub fn set_runtime(&self, rt: Arc<AgentRuntime>) {
        match self.runtime.set(rt) {
            Ok(()) => {}
            Err(_) => panic!("Agenverse::set_runtime() called more than once"),
        }
    }
}
