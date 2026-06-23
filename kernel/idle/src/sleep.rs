// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! Sleep actor — triggered by IdleEvent{kind="sleep"} events when the agent
//! reaches idle depth 20+. Runs cognitive housekeeping: session cleanup,
//! session backfill (catches what Reflection missed), memory consolidation,
//! temporal cleanup, cache expiry, index monitoring, and health reporting.
//!
//! Architecture ref: idle-patch.md §4
//!
//! The SleepActor orchestrates phases using an injected [`SleepHousekeeper`]
//! trait implementation. The gateway crate provides the concrete implementation
//! that delegates to `SessionStore`, `MemoryProvider`, `LlmProvider`, etc.
//! This crate only defines the orchestration — all gateway-specific I/O lives
//! behind the trait.

use async_trait::async_trait;
use event_bus::EventHandler;
use kernel::event::{Event, EventType};
use kernel::AmanResult;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

// ---------------------------------------------------------------------------
// SleepPhaseOutput
// ---------------------------------------------------------------------------

/// Result of a single Sleep workflow phase.
#[derive(Debug, Clone, serde::Serialize)]
pub struct SleepPhaseOutput {
    pub label: String,
    #[serde(skip_serializing_if = "serde_json::Value::is_null")]
    pub info: serde_json::Value,
}

impl SleepPhaseOutput {
    pub fn new(label: &str) -> Self {
        Self {
            label: label.to_owned(),
            info: serde_json::Value::Null,
        }
    }

    pub fn with_info(mut self, info: serde_json::Value) -> Self {
        self.info = info;
        self
    }
}

// ---------------------------------------------------------------------------
// CpuTracker
// ---------------------------------------------------------------------------

/// Tracks wall-clock time against a per-run CPU budget.
pub struct CpuTracker {
    max_secs: f64,
    total_elapsed: f64,
    phase_start: Option<Instant>,
}

impl CpuTracker {
    pub fn new(max_secs: f64) -> Self {
        Self {
            max_secs,
            total_elapsed: 0.0,
            phase_start: None,
        }
    }

    pub fn budget_remaining(&self) -> bool {
        self.total_elapsed < self.max_secs
    }

    pub fn start_phase(&mut self) {
        self.phase_start = Some(Instant::now());
    }

    pub fn end_phase(&mut self) -> Duration {
        self.phase_start
            .take()
            .map(|start| {
                let dur = start.elapsed();
                self.total_elapsed += dur.as_secs_f64();
                dur
            })
            .unwrap_or(Duration::ZERO)
    }

    pub fn total_elapsed(&self) -> f64 {
        self.total_elapsed
    }
}

// ---------------------------------------------------------------------------
// SleepActorConfig
// ---------------------------------------------------------------------------

/// Per-agent configuration for the SleepActor.
/// Mirrors the gateway's `SleepConfig` but lives in the idle crate so the
/// SleepActor has no dependency on the `config` crate.
#[derive(Debug, Clone)]
pub struct SleepActorConfig {
    /// Max wall-clock seconds per Sleep run (phases 0-5, excluding health report).
    pub max_cpu_seconds: u64,
    /// Days before short-term memories are considered stale.
    pub short_term_retention_days: u64,
    /// Days before cached files are eligible for deletion.
    pub cache_expiry_days: u64,
    /// Days before a stale background session is eligible for cleanup.
    pub stale_background_retention_days: u64,
    /// Minimum total characters in agent replies to keep a background session.
    pub stale_background_min_reply_chars: usize,
}

impl Default for SleepActorConfig {
    fn default() -> Self {
        Self {
            max_cpu_seconds: 300,
            short_term_retention_days: 7,
            cache_expiry_days: 30,
            stale_background_retention_days: 7,
            stale_background_min_reply_chars: 200,
        }
    }
}

// ---------------------------------------------------------------------------
// SleepHousekeeper trait
// ---------------------------------------------------------------------------

/// Gateway-provided implementation of Sleep-phase operations.
///
/// Each method corresponds to one phase of the Sleep workflow. The
/// [`SleepActor`] handles phase ordering, CPU budget tracking, and
/// cancellation — implementations should focus on the I/O work.
#[async_trait]
pub trait SleepHousekeeper: Send + Sync {
    /// Phase 0: Delete sessions that were created but never used (`message_count = 0`).
    async fn empty_session_cleanup(
        &self,
        agent_id: &str,
        cancel: &CancellationToken,
    ) -> AmanResult<serde_json::Value>;

    /// Phase 0b: Delete stale background sessions whose agent replies are
    /// too short to contain useful output (prize games, luck, etc.).
    async fn stale_background_cleanup(
        &self,
        agent_id: &str,
        cancel: &CancellationToken,
        retention_days: u64,
        min_reply_chars: usize,
    ) -> AmanResult<serde_json::Value>;

    /// Phase 1: Backfill one unreflected session — run the LLM extraction
    /// pipeline that Reflection normally handles.
    async fn session_backfill(
        &self,
        agent_id: &str,
        cancel: &CancellationToken,
    ) -> AmanResult<serde_json::Value>;

    /// Phase 2: Query stale memories and clean up: forget low-importance,
    /// flag high-importance, leave mid-range to natural decay.
    async fn temporal_housekeeping(
        &self,
        agent_id: &str,
        cancel: &CancellationToken,
        retention_days: u64,
    ) -> AmanResult<serde_json::Value>;

    /// Phase 3: Walk the agent's cache directory and delete files older
    /// than `cache_expiry_days`.
    async fn cache_cleanup(
        &self,
        agent_id: &str,
        cancel: &CancellationToken,
        cache_expiry_days: u64,
    ) -> AmanResult<serde_json::Value>;

    /// Phase 4: Collect memory index stats and warn if index size exceeds threshold.
    async fn index_monitoring(
        &self,
        agent_id: &str,
    ) -> AmanResult<serde_json::Value>;

    /// Phase 5: Run a `think()` pass for memory consolidation and conflict detection.
    async fn cognitive_consolidation(
        &self,
        agent_id: &str,
    ) -> AmanResult<serde_json::Value>;

    /// Final phase: Aggregate stats from all phases and write a health report.
    /// Always runs regardless of CPU budget.
    async fn health_report(
        &self,
        agent_id: &str,
        phase_outputs: &[SleepPhaseOutput],
        cpu_secs: f64,
    ) -> AmanResult<serde_json::Value>;
}

// ---------------------------------------------------------------------------
// SleepActor
// ---------------------------------------------------------------------------

/// Handles IdleEvent{kind="sleep"} → cognitive housekeeping workflow.
///
/// Subscribes to the global event bus and processes idle events of kind
/// `"sleep"` for all agents. The actual I/O work is delegated to a
/// [`SleepHousekeeper`] implementation injected at construction time.
///
/// Uses a per-agent guard (`HashSet`) to prevent overlapping Sleep runs
/// for the same agent.
pub struct SleepActor {
    config: SleepActorConfig,
    housekeeper: Arc<dyn SleepHousekeeper>,
    /// Per-agent guard: set of agent_ids currently running Sleep phases.
    active_runs: RwLock<std::collections::HashSet<String>>,
}

impl SleepActor {
    pub fn new(config: SleepActorConfig, housekeeper: Arc<dyn SleepHousekeeper>) -> Self {
        Self {
            config,
            housekeeper,
            active_runs: RwLock::new(std::collections::HashSet::new()),
        }
    }

    /// Try to acquire the run guard for a given agent. Returns true if acquired.
    fn try_acquire(&self, agent_id: &str) -> bool {
        self.active_runs.write().unwrap().insert(agent_id.to_owned())
    }

    /// Release the run guard for a given agent.
    fn release(&self, agent_id: &str) {
        self.active_runs.write().unwrap().remove(agent_id);
    }

    // -- phase orchestration --------------------------------------------------

    /// Run all Sleep phases sequentially with cancellation and CPU budget checks.
    async fn run_phases(&self, agent_id: &str, cancel: &CancellationToken) -> AmanResult<()> {
        let mut cpu = CpuTracker::new(self.config.max_cpu_seconds as f64);
        let mut phase_outputs: Vec<SleepPhaseOutput> = Vec::with_capacity(8);

        // Helper to run a phase with cancel + budget + timing checks.
        macro_rules! run_phase {
            ($phase_label:literal, $method:ident $(, $arg:expr)*) => {
                if cancel.is_cancelled() {
                    info!(
                        agent_id,
                        completed_phases = phase_outputs.len(),
                        "SleepActor: cancelled before {}",
                        $phase_label,
                    );
                    return Ok(());
                }
                if !cpu.budget_remaining() {
                    debug!(
                        agent_id,
                        phase = $phase_label,
                        elapsed = cpu.total_elapsed(),
                        "SleepActor: CPU budget exhausted, skipping {}",
                        $phase_label,
                    );
                } else {
                    cpu.start_phase();
                    let info = self.$method(agent_id, cancel $(, $arg)*).await;
                    cpu.end_phase();
                    let output = SleepPhaseOutput::new($phase_label).with_info(info);
                    debug!(
                        agent_id,
                        phase = $phase_label,
                        label = %output.label,
                        "SleepActor: {} complete",
                        $phase_label,
                    );
                    phase_outputs.push(output);
                }
            };
        }

        // Phase 0: empty session cleanup (cheap, runs first)
        run_phase!("empty_session_cleanup", run_empty_session_cleanup);
        // Phase 0b: stale background session cleanup
        run_phase!(
            "stale_background_cleanup",
            run_stale_background_cleanup,
            self.config.stale_background_retention_days,
            self.config.stale_background_min_reply_chars
        );
        // Phase 1: session backfill (expensive — LLM extraction)
        run_phase!("session_backfill", run_session_backfill);
        // Phase 2: temporal housekeeping
        run_phase!(
            "temporal_housekeeping",
            run_temporal_housekeeping,
            self.config.short_term_retention_days
        );
        // Phase 3: cache cleanup
        run_phase!(
            "cache_cleanup",
            run_cache_cleanup,
            self.config.cache_expiry_days
        );
        // Phase 4: index monitoring
        run_phase!("index_monitoring", run_index_monitoring);
        // Phase 5: cognitive consolidation
        run_phase!("cognitive_consolidation", run_cognitive_consolidation);

        // Final phase: health report always runs (cost is negligible)
        {
            let info = self
                .housekeeper
                .health_report(agent_id, &phase_outputs, cpu.total_elapsed())
                .await
                .unwrap_or_else(|e| {
                    warn!(
                        agent_id,
                        error = %e,
                        "SleepActor: health report failed"
                    );
                    serde_json::json!({"error": e.to_string()})
                });
            let output = SleepPhaseOutput::new("health_report").with_info(info);
            phase_outputs.push(output);
        }

        let total_cpu = cpu.total_elapsed();
        info!(
            agent_id,
            phases_run = phase_outputs.len(),
            total_cpu_secs = total_cpu,
            "SleepActor: all phases complete"
        );

        Ok(())
    }

    // -- per-phase helpers (delegate to housekeeper) --------------------------

    async fn run_empty_session_cleanup(
        &self, agent_id: &str, cancel: &CancellationToken,
    ) -> serde_json::Value {
        self.housekeeper
            .empty_session_cleanup(agent_id, cancel)
            .await
            .unwrap_or_else(|e| serde_json::json!({"error": e.to_string()}))
    }

    async fn run_stale_background_cleanup(
        &self, agent_id: &str, cancel: &CancellationToken,
        retention_days: u64, min_reply_chars: usize,
    ) -> serde_json::Value {
        self.housekeeper
            .stale_background_cleanup(agent_id, cancel, retention_days, min_reply_chars)
            .await
            .unwrap_or_else(|e| serde_json::json!({"error": e.to_string()}))
    }

    async fn run_session_backfill(
        &self, agent_id: &str, cancel: &CancellationToken,
    ) -> serde_json::Value {
        self.housekeeper
            .session_backfill(agent_id, cancel)
            .await
            .unwrap_or_else(|e| serde_json::json!({"error": e.to_string()}))
    }

    async fn run_temporal_housekeeping(
        &self, agent_id: &str, cancel: &CancellationToken, retention_days: u64,
    ) -> serde_json::Value {
        self.housekeeper
            .temporal_housekeeping(agent_id, cancel, retention_days)
            .await
            .unwrap_or_else(|e| serde_json::json!({"error": e.to_string()}))
    }

    async fn run_cache_cleanup(
        &self, agent_id: &str, cancel: &CancellationToken, cache_expiry_days: u64,
    ) -> serde_json::Value {
        self.housekeeper
            .cache_cleanup(agent_id, cancel, cache_expiry_days)
            .await
            .unwrap_or_else(|e| serde_json::json!({"error": e.to_string()}))
    }

    async fn run_index_monitoring(
        &self, agent_id: &str, _cancel: &CancellationToken,
    ) -> serde_json::Value {
        self.housekeeper
            .index_monitoring(agent_id)
            .await
            .unwrap_or_else(|e| serde_json::json!({"error": e.to_string()}))
    }

    async fn run_cognitive_consolidation(
        &self, agent_id: &str, _cancel: &CancellationToken,
    ) -> serde_json::Value {
        self.housekeeper
            .cognitive_consolidation(agent_id)
            .await
            .unwrap_or_else(|e| serde_json::json!({"error": e.to_string()}))
    }
}

// ---------------------------------------------------------------------------
// EventHandler impl
// ---------------------------------------------------------------------------

#[async_trait]
impl EventHandler for SleepActor {
    async fn handle(&self, event: Event) -> AmanResult<()> {
        // Filter: only Idle events with kind == "sleep"
        if event.event_type != EventType::Idle {
            return Ok(());
        }
        let Some(kind) = event.payload.get("kind").and_then(|v| v.as_str()) else {
            return Ok(());
        };
        if kind != "sleep" {
            return Ok(());
        }
        let Some(agent_id) = event
            .payload
            .get("agent_id")
            .and_then(|v| v.as_str())
        else {
            return Ok(());
        };
        if agent_id.is_empty() {
            return Ok(());
        }

        // Guard: skip if already running for this agent
        if !self.try_acquire(agent_id) {
            debug!(
                agent_id,
                "SleepActor: already running for agent, skipping duplicate trigger"
            );
            return Ok(());
        }

        // Ensure release on all exit paths
        let cancel = CancellationToken::new();
        let result = self.run_phases(agent_id, &cancel).await;
        self.release(agent_id);
        result
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use kernel::event::Event;
    use kernel::event::EventType;

    // -----------------------------------------------------------------------
    // CpuTracker tests
    // -----------------------------------------------------------------------

    #[test]
    fn cpu_tracker_budget_remaining_initially_true() {
        let tracker = CpuTracker::new(300.0);
        assert!(tracker.budget_remaining());
    }

    #[test]
    fn cpu_tracker_tracks_elapsed_time() {
        let mut tracker = CpuTracker::new(300.0);
        tracker.start_phase();
        std::thread::sleep(Duration::from_millis(10));
        let dur = tracker.end_phase();
        assert!(dur.as_millis() >= 10);
        assert!(tracker.total_elapsed() > 0.0);
    }

    #[test]
    fn cpu_tracker_budget_exhausted() {
        let mut tracker = CpuTracker::new(0.001); // 1ms budget
        tracker.start_phase();
        std::thread::sleep(Duration::from_millis(5));
        tracker.end_phase();
        assert!(!tracker.budget_remaining());
    }

    #[test]
    fn cpu_tracker_end_phase_without_start_is_zero() {
        let mut tracker = CpuTracker::new(300.0);
        let dur = tracker.end_phase();
        assert_eq!(dur, Duration::ZERO);
    }

    // -----------------------------------------------------------------------
    // SleepPhaseOutput tests
    // -----------------------------------------------------------------------

    #[test]
    fn phase_output_serializes_label() {
        let output = SleepPhaseOutput::new("test_phase");
        let json = serde_json::to_value(&output).unwrap();
        assert_eq!(json["label"], "test_phase");
        assert!(json.get("info").is_none());
    }

    #[test]
    fn phase_output_with_info_serializes_info() {
        let output =
            SleepPhaseOutput::new("test_phase").with_info(serde_json::json!({"count": 42}));
        let json = serde_json::to_value(&output).unwrap();
        assert_eq!(json["label"], "test_phase");
        assert_eq!(json["info"]["count"], 42);
    }

    // -----------------------------------------------------------------------
    // SleepActor guard tests
    // -----------------------------------------------------------------------

    #[test]
    fn guard_acquire_twice_fails() {
        let actor = SleepActor::new(
            SleepActorConfig::default(),
            Arc::new(StubHousekeeper),
        );
        assert!(actor.try_acquire("agent-1"));
        assert!(!actor.try_acquire("agent-1"));
        actor.release("agent-1");
        assert!(actor.try_acquire("agent-1"));
        actor.release("agent-1");
    }

    #[test]
    fn guard_different_agents_independent() {
        let actor = SleepActor::new(
            SleepActorConfig::default(),
            Arc::new(StubHousekeeper),
        );
        assert!(actor.try_acquire("agent-1"));
        assert!(actor.try_acquire("agent-2"));
        actor.release("agent-1");
        actor.release("agent-2");
    }

    // -----------------------------------------------------------------------
    // EventHandler filter tests
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn handle_ignores_non_idle_events() {
        let actor = SleepActor::new(
            SleepActorConfig::default(),
            Arc::new(StubHousekeeper),
        );
        let event = Event::new(
            "chat:user",
            EventType::MessageReceived,
            serde_json::json!({"text": "hello"}),
        );
        let result = actor.handle(event).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn handle_ignores_non_sleep_idle_events() {
        let actor = SleepActor::new(
            SleepActorConfig::default(),
            Arc::new(StubHousekeeper),
        );
        let event = Event::new(
            "idle.system",
            EventType::Idle,
            serde_json::json!({"kind": "daze", "depth": 0}),
        );
        let result = actor.handle(event).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn handle_ignores_idle_without_agent_id() {
        let actor = SleepActor::new(
            SleepActorConfig::default(),
            Arc::new(StubHousekeeper),
        );
        let event = Event::new(
            "idle.system",
            EventType::Idle,
            serde_json::json!({"kind": "sleep", "depth": 20}),
        );
        let result = actor.handle(event).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn handle_skips_when_already_running() {
        let actor = SleepActor::new(
            SleepActorConfig::default(),
            Arc::new(StubHousekeeper),
        );
        actor.try_acquire("agent-1");

        let event = Event::new(
            "idle.system",
            EventType::Idle,
            serde_json::json!({"kind": "sleep", "depth": 20, "agent_id": "agent-1"}),
        );
        let result = actor.handle(event).await;
        assert!(result.is_ok());
        actor.release("agent-1");
    }

    // -----------------------------------------------------------------------
    // Stub housekeeper for unit tests
    // -----------------------------------------------------------------------

    struct StubHousekeeper;

    #[async_trait]
    impl SleepHousekeeper for StubHousekeeper {
        async fn empty_session_cleanup(
            &self,
            _agent_id: &str,
            _cancel: &CancellationToken,
        ) -> AmanResult<serde_json::Value> {
            Ok(serde_json::json!({"deleted": 0}))
        }

        async fn stale_background_cleanup(
            &self,
            _agent_id: &str,
            _cancel: &CancellationToken,
            _retention_days: u64,
            _min_reply_chars: usize,
        ) -> AmanResult<serde_json::Value> {
            Ok(serde_json::json!({"deleted": 0}))
        }

        async fn session_backfill(
            &self,
            _agent_id: &str,
            _cancel: &CancellationToken,
        ) -> AmanResult<serde_json::Value> {
            Ok(serde_json::json!({"extracted": 0}))
        }

        async fn temporal_housekeeping(
            &self,
            _agent_id: &str,
            _cancel: &CancellationToken,
            _retention_days: u64,
        ) -> AmanResult<serde_json::Value> {
            Ok(serde_json::json!({"stale_count": 0}))
        }

        async fn cache_cleanup(
            &self,
            _agent_id: &str,
            _cancel: &CancellationToken,
            _cache_expiry_days: u64,
        ) -> AmanResult<serde_json::Value> {
            Ok(serde_json::json!({"deleted_files": 0}))
        }

        async fn index_monitoring(
            &self,
            _agent_id: &str,
        ) -> AmanResult<serde_json::Value> {
            Ok(serde_json::json!({"total_entries": 0}))
        }

        async fn cognitive_consolidation(
            &self,
            _agent_id: &str,
        ) -> AmanResult<serde_json::Value> {
            Ok(serde_json::json!({"consolidation_count": 0}))
        }

        async fn health_report(
            &self,
            _agent_id: &str,
            _phase_outputs: &[SleepPhaseOutput],
            _cpu_secs: f64,
        ) -> AmanResult<serde_json::Value> {
            Ok(serde_json::json!({"status": "ok"}))
        }
    }
}
