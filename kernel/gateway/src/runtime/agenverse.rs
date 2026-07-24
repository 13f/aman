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
use kernel::event::{Event, EventType};
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;
use tokio::sync::{Mutex, RwLock};
use tokio_util::sync::CancellationToken;

use super::agent_runtime::AgentRuntime;
use super::http::HttpServerHandle;

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

// ── Agenverse era ───────────────────────────────────────────────────────────
//
//  The "soul" lifecycle of the agents universe — from nothingness, through
//  a formative Chaos, into the fullness of Genesis.
//
//  虚无 (Void)  →  混沌 (Chaos)  →  创世纪 (Genesis)
//
//  Void:    the agenverse exists but has not yet been initialised.
//  Chaos:   startup has completed; agents are "forming" and can only Daze.
//           The autonomous idle system (boredom → work/study/daily-life) is
//           suppressed so that a user who steps away at boot does not come
//           back to find every agent busy in work/study.
//  Genesis: the Chaos period has elapsed; agents awaken fully and the idle
//           system runs normally.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Era {
    /// 虚无 — the agenverse has not yet been initialised. Default state.
    Void = 0,
    /// 混沌 — agents are forming; idle system suppressed, only Daze allowed.
    Chaos = 1,
    /// 创世纪 — agents fully awakened; idle system runs normally.
    Genesis = 2,
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
    /// Handle to the HTTP server, stored for shutdown orchestration.
    /// `Option` + `Mutex` because [`HttpServerHandle::shutdown`] takes `self`
    /// by value; [`shutdown`](Self::shutdown) calls `take()`.
    server: Mutex<Option<HttpServerHandle>>,
    /// The agenverse era (Void → Chaos → Genesis). Shared with the idle
    /// system via [`era_arc`](Self::era_arc) so that idle managers can gate
    /// their behaviour during Chaos.
    era: Arc<AtomicU8>,
    /// Seconds the agenverse stays in Chaos before auto-transitioning to Genesis.
    chaos_duration: Duration,
}

impl Agenverse {
    /// Construct a fresh agenverse in `New` state and [`Era::Void`].
    ///
    /// `chaos_duration` is the seconds the agenverse will remain in
    /// [`Era::Chaos`] after [`enter_chaos`](Self::enter_chaos) is called,
    /// before auto-transitioning to [`Era::Genesis`].
    pub fn new(startup_pause: Duration, chaos_duration: Duration) -> Self {
        Self {
            phase: AtomicU8::new(RuntimePhase::Phase0 as u8),
            status: RwLock::new(RuntimeStatus::New),
            transition_lock: Mutex::new(()),
            shutdown_requested: AtomicBool::new(false),
            shutdown_notify: tokio::sync::Notify::new(),
            startup_pause,
            runtime: OnceLock::new(),
            server: Mutex::new(None),
            era: Arc::new(AtomicU8::new(Era::Void as u8)),
            chaos_duration,
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

    /// Current agenverse era (Void / Chaos / Genesis).
    #[must_use]
    pub fn era(&self) -> Era {
        match self.era.load(Ordering::Acquire) {
            x if x == Era::Chaos as u8 => Era::Chaos,
            x if x == Era::Genesis as u8 => Era::Genesis,
            _ => Era::Void,
        }
    }

    /// Whether the agenverse has reached Genesis (agents fully awakened).
    #[must_use]
    pub fn is_genesis(&self) -> bool {
        self.era() == Era::Genesis
    }

    /// Whether the agenverse is still in Chaos (agents forming, idle suppressed).
    #[must_use]
    pub fn is_chaos(&self) -> bool {
        self.era() == Era::Chaos
    }

    /// Return a shareable handle to the era atomic, for passing into idle
    /// managers and other subsystems that need to gate on the era.
    #[must_use]
    pub fn era_arc(&self) -> Arc<AtomicU8> {
        Arc::clone(&self.era)
    }

    /// Seconds the agenverse stays in Chaos before auto-transitioning to Genesis.
    #[must_use]
    pub fn chaos_duration(&self) -> Duration {
        self.chaos_duration
    }

    /// Transition Void → Chaos and schedule the auto-transition to Genesis.
    ///
    /// Called once after startup completes. During Chaos agents can only
    /// Daze — the autonomous idle system is suppressed. After
    /// [`chaos_duration`](Self::chaos_duration) seconds the agenverse
    /// automatically transitions to Genesis and agents awaken fully.
    ///
    /// Idempotent: if the agenverse is already past Void (Chaos or Genesis),
    /// this is a no-op (the existing Genesis timer, if any, is left intact).
    pub fn enter_chaos(&self) {
        // CAS Void → Chaos; bail if already past Void.
        if self
            .era
            .compare_exchange(
                Era::Void as u8,
                Era::Chaos as u8,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_err()
        {
            return;
        }

        let secs = self.chaos_duration.as_secs();
        tracing::info!(chaos_secs = secs, "agenverse entering 混沌 (Chaos): agents forming, idle system suppressed");

        // Complete cold-start for every agent at the start of Chaos.  The idle
        // system is suppressed for the whole Chaos window (manager.rs gates on
        // is_genesis()), so the idle loop never emits COLD_START_DONE_EVENT
        // and the harness never flips AgentStatus Preparing → Idle on its own.
        // Driving the transition directly here unblocks agent interaction: the
        // frontend gates every click while status === Preparing ("Agent is
        // loading", Home.svelte).  mark_cold_start_complete is idempotent —
        // agents already past Preparing are skipped silently.
        // Spawned as its own task because enter_chaos() is sync.
        if let Some(runtime) = self.runtime.get() {
            let registry = runtime.agent_registry();
            tokio::spawn(async move {
                let instances = registry.list().await;
                for inst in &instances {
                    let _ = registry
                        .mark_cold_start_complete(&inst.descriptor.agent_id)
                        .await;
                }
                tracing::info!(
                    count = instances.len(),
                    "Chaos: cold-start complete for all agents"
                );
            });
        }

        // Schedule Chaos → Genesis.
        let era = Arc::clone(&self.era);
        let sleep_duration = self.chaos_duration;
        // Start idle loops when Genesis begins.  The runtime is set by the
        // time enter_chaos() is called (main.rs sets it after build), so
        // runtime() will succeed.  If for some reason it isn't set yet, we
        // log and skip — the idle loops simply won't start.
        let runtime = Arc::clone(
            self.runtime
                .get()
                .expect("Agenverse::enter_chaos() called before AgentRuntime was set"),
        );
        tokio::spawn(async move {
            tokio::time::sleep(sleep_duration).await;
            era.store(Era::Genesis as u8, Ordering::Release);
            tracing::info!("agenverse entered 创世纪 (Genesis): agents fully awakened, starting idle system");
            // Now that we're in Genesis, start the per-agent idle loops.
            runtime.agent_registry().start_all_idle_loops().await;
            tracing::info!("idle system started");
        });
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
    /// `Err(())` if a shutdown is already in progress or complete.
    pub async fn try_acquire_shutdown_gate(&self) -> Result<(), ()> {
        self.shutdown_requested.store(true, Ordering::Release);
        let _guard = self.transition_lock.lock().await;
        let current = *self.status.read().await;
        // Bail out if a shutdown is already in progress (`ShuttingDown`) or
        // complete (`Shutdown`). Without the `ShuttingDown` check, a second
        // Ctrl+C during shutdown would re-enter `runtime.shutdown()` and run
        // the entire phase sequence again — re-stopping watchers that are
        // already being stopped and re-checkpointing the WAL.
        if matches!(
            current,
            RuntimeStatus::Shutdown | RuntimeStatus::ShuttingDown
        ) {
            return Err(());
        }
        *self.status.write().await = RuntimeStatus::ShuttingDown;
        Ok(())
    }

    /// Signal the TUI event loop to exit.
    ///
    /// This only sets the `shutdown_requested` flag that the TUI polls; it
    /// does **not** acquire the shutdown gate or transition runtime status
    /// (unlike [`try_acquire_shutdown_gate`](Self::try_acquire_shutdown_gate)).
    /// The TUI-mode signal handler uses it so that Ctrl+C / SIGTERM can
    /// unwind the terminal (raw mode + alternate screen) gracefully *before*
    /// the full runtime shutdown runs. Without this, an in-flight signal
    /// would otherwise terminate the process via the default handler and
    /// leave the terminal stuck in raw mode.
    pub fn request_tui_exit(&self) {
        self.shutdown_requested.store(true, Ordering::Release);
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

    /// Number of agents currently alive in the agenverse.
    pub async fn agent_count(&self) -> usize {
        self.runtime().agent_registry().list().await.len()
    }

    /// Store the HTTP server handle so [`shutdown`](Self::shutdown) can stop
    /// it. Called once by `main()` after the server is built.
    pub async fn set_server_handle(&self, handle: HttpServerHandle) {
        *self.server.lock().await = Some(handle);
    }

    /// Full graceful shutdown orchestration.
    ///
    /// 1. Publishes `"gateway:stopping"` lifecycle event.
    /// 2. Acquires the shutdown gate. If successful, runs
    ///    [`AgentRuntime::shutdown`] guarded by a 10-second timeout
    ///    (forced exit on timeout or second SIGINT).
    /// 3. Stops the HTTP server.
    ///
    /// Idempotent: safe to call after an HTTP-initiated shutdown has
    /// already run `AgentRuntime::shutdown()`.
    pub async fn shutdown(&self) {
        // Publish the stopping event regardless of gate state — the
        // event bus is still operational even when shutting down.
        let _ = self.runtime().publish_event(Event::new(
            "gateway:lifecycle",
            EventType::Custom("gateway:stopping".to_owned()),
            serde_json::json!({}),
        ))
        .await;

        // Acquire the shutdown gate. If it fails (already shutting
        // down / shut down), skip the runtime phase transitions.
        if self.try_acquire_shutdown_gate().await.is_ok() {
            // Phase5 drain + Phase4 event-bus drain each take up to
            // drain_timeout_sec (clamped 3-10 s). 30 s gives generous
            // headroom so the outer timeout only fires when something
            // is genuinely stuck, not during a normal slow drain.
            //
            // We install a second ctrl_c() listener here to give the
            // operator an "accelerate" signal: a second Ctrl+C cancels
            // the CancellationToken passed into runtime.shutdown(),
            // which makes every bounded drain loop bail out
            // immediately instead of burning the rest of its grace
            // period. Without this, a second Ctrl+C would either hit
            // the default SIGINT handler (hard-kill, skipping WAL
            // checkpoint and tracing flush) or do nothing at all,
            // leaving the operator to wait out the full timeout with
            // no feedback. The CancellationToken is cancelled-drop
            // safe: letting it drop without cancelling releases all
            // waiters (i.e. the normal one-shot case).
            const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(30);

            // The "I really mean it now" signal.
            let force_quit = CancellationToken::new();
            // In every arm below, `runtime.shutdown(&force_quit)` is the
            // unit of work; the select only decides *with which token
            // state* we run it (normal, timed-out, or force-cancelled).
            let shutdown_result: Result<(), _> = tokio::select! {
                _ = tokio::time::sleep(SHUTDOWN_TIMEOUT) => {
                    tracing::error!(
                        "shutdown timed out after {}s — returning (process will exit naturally)",
                        SHUTDOWN_TIMEOUT.as_secs()
                    );
                    // Do NOT call std::process::exit here — let the
                    // process unwind naturally so tracing subscribers
                    // flush to disk and destructors run.
                    // Return Ok: the timeout is the ultimate backstop and
                    // the outer main() will exit the process next.
                    Ok(())
                }
                result = self.runtime().shutdown(&force_quit) => result,
                _ = tokio::signal::ctrl_c() => {
                    // The operator pressed Ctrl+C again. Tell the
                    // runtime to skip the rest of its grace periods
                    // and head straight for the final WAL checkpoint.
                    // We keep awaiting runtime.shutdown() so the
                    // process still unwinds naturally with full
                    // cleanup.
                    tracing::warn!("received second Ctrl+C during shutdown — fast-exiting remaining drain");
                    force_quit.cancel();
                    // Continue waiting for the runtime shutdown to
                    // drain what it can and complete WAL checkpoint.
                    self.runtime().shutdown(&force_quit).await
                }
            };
            if let Err(e) = shutdown_result {
                tracing::error!(
                    error = %e,
                    "shutdown completed with errors"
                );
            }
        }

        // Stop the HTTP server. Option::take ensures at-most-once
        // delivery even if shutdown() is called multiple times.
        // Await the graceful shutdown so in-flight requests (e.g. the
        // /agent/shutdown POST that triggered this teardown) are
        // actually responded to before the process exits.
        if let Some(handle) = self.server.lock().await.take() {
            handle.shutdown().await;
        }

        tracing::info!("gateway shut down gracefully");
    }
}
