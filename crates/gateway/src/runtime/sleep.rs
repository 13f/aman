// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! Sleep runner — triggered by IdleEvent{kind="sleep"} events when the agent
//! reaches idle depth 20+. Runs cognitive housekeeping: memory consolidation,
//! temporal cleanup, cache expiry, index monitoring, and health reporting.
//!
//! Architecture ref: idle-patch.md §4
//!
//! The real Sleep workflow lives here (not in crates/idle/). It follows the
//! same dependency-injection pattern as [`ReflectionRunner`](super::reflection::ReflectionRunner):
//! OnceLock fields are populated during [`AgentRuntimeBuilder::build`] and the
//! runner subscribes to idle events on the global bus.

use async_trait::async_trait;
use config::SleepConfig;
use event_bus::EventHandler;
use kernel::event::{Event, EventType};
use kernel::memory::{MemoryProvider, MemoryStats, ThinkConfig, ThinkResult};
use kernel::AmanResult;
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock, RwLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tracing::{debug, info, warn};

use super::agent_registry::AgentRegistry;
use super::session_store::SessionStore;

// ---------------------------------------------------------------------------
// SleepPhaseOutput
// ---------------------------------------------------------------------------

/// Result of a single Sleep workflow phase.
#[derive(Debug, Clone, serde::Serialize)]
struct SleepPhaseOutput {
    label: String,
    #[serde(skip_serializing_if = "serde_json::Value::is_null")]
    info: serde_json::Value,
}

impl SleepPhaseOutput {
    fn new(label: &str) -> Self {
        Self {
            label: label.to_owned(),
            info: serde_json::Value::Null,
        }
    }

    fn with_info(mut self, info: serde_json::Value) -> Self {
        self.info = info;
        self
    }
}

// ---------------------------------------------------------------------------
// CpuTracker
// ---------------------------------------------------------------------------

/// Tracks wall-clock time against a per-run CPU budget.
struct CpuTracker {
    max_secs: f64,
    total_elapsed: f64,
    phase_start: Option<Instant>,
}

impl CpuTracker {
    fn new(max_secs: f64) -> Self {
        Self {
            max_secs,
            total_elapsed: 0.0,
            phase_start: None,
        }
    }

    fn budget_remaining(&self) -> bool {
        self.total_elapsed < self.max_secs
    }

    fn start_phase(&mut self) {
        self.phase_start = Some(Instant::now());
    }

    fn end_phase(&mut self) -> Duration {
        self.phase_start
            .take()
            .map(|start| {
                let dur = start.elapsed();
                self.total_elapsed += dur.as_secs_f64();
                dur
            })
            .unwrap_or(Duration::ZERO)
    }

    fn total_elapsed(&self) -> f64 {
        self.total_elapsed
    }
}

// ---------------------------------------------------------------------------
// SleepRunner
// ---------------------------------------------------------------------------

/// Handles IdleEvent{kind="sleep"} → cognitive housekeeping workflow.
///
/// Subscribes to the global event bus and processes idle events of kind
/// `"sleep"`. Dependencies are injected via the `OnceLock` pattern (same
/// as `ReflectionRunner` and `ReadSkillTool`).
pub struct SleepRunner {
    agent_registry: OnceLock<Arc<AgentRegistry>>,
    memory_provider: OnceLock<Arc<dyn MemoryProvider>>,
    session_store: OnceLock<Arc<SessionStore>>,
    sleep_config: OnceLock<SleepConfig>,
    /// Prevents overlapping Sleep runs per agent.
    active_runs: RwLock<HashSet<String>>,
}

impl SleepRunner {
    pub fn new() -> Self {
        Self {
            agent_registry: OnceLock::new(),
            memory_provider: OnceLock::new(),
            session_store: OnceLock::new(),
            sleep_config: OnceLock::new(),
            active_runs: RwLock::new(HashSet::new()),
        }
    }

    pub fn set_agent_registry(&self, registry: Arc<AgentRegistry>) {
        let _ = self.agent_registry.set(registry);
    }

    pub fn set_memory_provider(&self, provider: Arc<dyn MemoryProvider>) {
        let _ = self.memory_provider.set(provider);
    }

    pub fn set_session_store(&self, store: Arc<SessionStore>) {
        let _ = self.session_store.set(store);
    }

    pub fn set_sleep_config(&self, config: SleepConfig) {
        let _ = self.sleep_config.set(config);
    }

    // -- phase 1: session compression backfill ------------------------------

    /// Backfill sessions that Reflection missed (crash / timeout / restart
    /// edge cases). Normally empty — Reflection handles this live per-session.
    async fn phase_session_backfill(
        &self,
        agent_id: &str,
        _cancel: &tokio_util::sync::CancellationToken,
        cpu: &mut CpuTracker,
    ) -> SleepPhaseOutput {
        cpu.start_phase();
        let output = SleepPhaseOutput::new("session_backfill");

        let Some(provider) = self.memory_provider.get() else {
            debug!("SleepRunner: no MemoryProvider, skipping phase 1");
            cpu.end_phase();
            return output.with_info(serde_json::json!({"status": "skipped", "reason": "no memory provider"}));
        };

        // Query recent sessions
        let sessions = match provider.session_history(agent_id, 20).await {
            Ok(s) => s,
            Err(e) => {
                warn!(agent_id, error = %e, "Sleep phase 1: session_history failed");
                cpu.end_phase();
                return output.with_info(serde_json::json!({"error": e.to_string()}));
            }
        };

        if sessions.is_empty() {
            debug!(agent_id, "Sleep phase 1: no sessions to backfill");
            cpu.end_phase();
            return output.with_info(serde_json::json!({"compressions_done": 0}));
        }

        // v1: Reflection handles the primary path. Backfill needs
        // `is_compressed` field on SessionSummary + LLM wiring (deferred).
        let eligible_count = sessions.len();
        debug!(
            agent_id,
            count = eligible_count,
            "Sleep phase 1: sessions present (backfill deferred — Reflection handles primary path)"
        );

        cpu.end_phase();
        output.with_info(serde_json::json!({
            "compressions_done": 0,
            "eligible_count": eligible_count,
            "note": "LLM backfill deferred; Reflection handles primary session→YantrikDB path"
        }))
    }

    // -- phase 2: temporal housekeeping -------------------------------------

    /// Query stale memories and clean up: forget low-importance ones,
    /// flag high-importance for review, leave mid-range to natural decay.
    async fn phase_temporal_housekeeping(
        &self,
        agent_id: &str,
        cancel: &tokio_util::sync::CancellationToken,
        cpu: &mut CpuTracker,
        retention_days: u64,
    ) -> SleepPhaseOutput {
        cpu.start_phase();
        let output = SleepPhaseOutput::new("temporal_housekeeping");

        let Some(provider) = self.memory_provider.get() else {
            debug!("SleepRunner: no MemoryProvider, skipping phase 2");
            cpu.end_phase();
            return output.with_info(serde_json::json!({"status": "skipped", "reason": "no memory provider"}));
        };

        let stale = match provider.stale_memories(agent_id, retention_days as u32).await {
            Ok(s) => s,
            Err(e) => {
                warn!(agent_id, error = %e, "Sleep phase 2: stale_memories failed");
                cpu.end_phase();
                return output.with_info(serde_json::json!({"error": e.to_string()}));
            }
        };

        let (mut forgotten, mut flagged, mut skipped) = (0u64, 0u64, 0u64);

        for mem in &stale {
            if cancel.is_cancelled() {
                debug!(agent_id, "Sleep phase 2: cancelled mid-housekeeping");
                break;
            }
            match mem.importance {
                Some(imp) if imp >= 0.6 => {
                    flagged += 1;
                }
                Some(imp) if imp < 0.3 => {
                    provider.forget(agent_id, &mem.rid);
                    forgotten += 1;
                }
                _ => {
                    skipped += 1;
                }
            }
        }

        cpu.end_phase();
        output.with_info(serde_json::json!({
            "stale_count": stale.len(),
            "forgotten": forgotten,
            "flagged_for_review": flagged,
            "no_action": skipped,
            "retention_days": retention_days,
        }))
    }

    // -- phase 3: cache cleanup ---------------------------------------------

    /// Walk `~/.aman/agents/{agent_id}/cache/` and delete files older than
    /// `cache_expiry_days`. Cache is per-agent state, not system-wide.
    async fn phase_cache_cleanup(
        &self,
        agent_id: &str,
        cancel: &tokio_util::sync::CancellationToken,
        cpu: &mut CpuTracker,
        cache_expiry_days: u64,
    ) -> SleepPhaseOutput {
        cpu.start_phase();
        let output = SleepPhaseOutput::new("cache_cleanup");

        let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_owned());
        let cache_dir = PathBuf::from(&home)
            .join(".aman")
            .join("agents")
            .join(agent_id)
            .join("cache");

        if !cache_dir.exists() || !cache_dir.is_dir() {
            debug!(agent_id, path = %cache_dir.display(), "Sleep phase 3: cache dir not found, skipping");
            cpu.end_phase();
            return output.with_info(serde_json::json!({"status": "skipped", "reason": "no cache directory"}));
        }

        let cutoff = match SystemTime::now().checked_sub(Duration::from_secs(cache_expiry_days * 86400)) {
            Some(t) => t,
            None => {
                cpu.end_phase();
                return output.with_info(serde_json::json!({"status": "skipped", "reason": "clock underflow"}));
            }
        };

        let (mut deleted, mut bytes_freed) = (0u64, 0u64);
        if let Err(e) = Self::walk_and_clean(&cache_dir, &cutoff, cancel, &mut deleted, &mut bytes_freed) {
            warn!(agent_id, error = %e, "Sleep phase 3: cache walk error");
        }

        cpu.end_phase();
        output.with_info(serde_json::json!({
            "cache_dir": cache_dir.display().to_string(),
            "deleted_files": deleted,
            "bytes_freed": bytes_freed,
            "cache_expiry_days": cache_expiry_days,
        }))
    }

    /// Recursively walk `dir`, deleting regular files older than `cutoff`.
    fn walk_and_clean(
        dir: &std::path::Path,
        cutoff: &SystemTime,
        cancel: &tokio_util::sync::CancellationToken,
        deleted: &mut u64,
        bytes_freed: &mut u64,
    ) -> Result<(), std::io::Error> {
        let entries = std::fs::read_dir(dir)?;
        for entry in entries.flatten() {
            if cancel.is_cancelled() {
                return Ok(());
            }
            let path = entry.path();
            let Ok(meta) = entry.metadata() else {
                continue;
            };
            if meta.is_dir() {
                let _ = Self::walk_and_clean(&path, cutoff, cancel, deleted, bytes_freed);
            } else if meta.is_file()
                && let Ok(modified) = meta.modified()
                    && modified < *cutoff {
                        *bytes_freed += meta.len();
                        if std::fs::remove_file(&path).is_ok() {
                            *deleted += 1;
                        }
                    }
        }
        Ok(())
    }

    // -- phase 4: index monitoring ------------------------------------------

    /// Collect memory index stats and warn if index size exceeds threshold.
    async fn phase_index_monitoring(
        &self,
        agent_id: &str,
        _cancel: &tokio_util::sync::CancellationToken,
        cpu: &mut CpuTracker,
    ) -> SleepPhaseOutput {
        cpu.start_phase();
        let output = SleepPhaseOutput::new("index_monitoring");

        let Some(provider) = self.memory_provider.get() else {
            debug!("SleepRunner: no MemoryProvider, skipping phase 4");
            cpu.end_phase();
            return output.with_info(serde_json::json!({"status": "skipped", "reason": "no memory provider"}));
        };

        let stats = match provider.stats(agent_id).await {
            Ok(s) => s,
            Err(e) => {
                warn!(agent_id, error = %e, "Sleep phase 4: stats failed");
                cpu.end_phase();
                return output.with_info(serde_json::json!({"error": e.to_string()}));
            }
        };

        const INDEX_SIZE_WARN_MB: u64 = 100;
        let size_mb = stats.index_size_bytes / (1024 * 1024);
        if size_mb > INDEX_SIZE_WARN_MB {
            warn!(
                agent_id,
                size_mb,
                total_entries = stats.total_entries,
                "Sleep phase 4: index size exceeds {}MB threshold",
                INDEX_SIZE_WARN_MB,
            );
        }

        cpu.end_phase();
        output.with_info(serde_json::json!({
            "total_entries": stats.total_entries,
            "index_size_bytes": stats.index_size_bytes,
            "index_size_mb": size_mb,
            "graph_nodes": stats.graph_nodes,
            "graph_edges": stats.graph_edges,
            "pending_conflicts": stats.pending_conflicts,
        }))
    }

    // -- phase 5: cognitive consolidation -----------------------------------

    /// Run a think() pass for memory consolidation and conflict detection.
    /// Currently returns empty results — YantrikDB `think()` bridging is
    /// pending (see idle-patch.md §0.3).
    async fn phase_cognitive_consolidation(
        &self,
        agent_id: &str,
        _cancel: &tokio_util::sync::CancellationToken,
        cpu: &mut CpuTracker,
    ) -> SleepPhaseOutput {
        cpu.start_phase();
        let output = SleepPhaseOutput::new("cognitive_consolidation");

        let Some(provider) = self.memory_provider.get() else {
            debug!("SleepRunner: no MemoryProvider, skipping phase 5");
            cpu.end_phase();
            return output.with_info(serde_json::json!({"status": "skipped", "reason": "no memory provider"}));
        };

        let think_cfg = ThinkConfig::default();
        let result = provider.think(agent_id, &think_cfg).await.unwrap_or_else(|e| {
            warn!(agent_id, error = %e, "Sleep phase 5: think() failed");
            ThinkResult::default()
        });

        info!(
            agent_id,
            triggers = result.triggers_fired,
            consolidated = result.consolidation_count,
            conflicts = result.conflicts_found,
            duration_ms = result.duration_ms,
            "Sleep phase 5: think pass complete"
        );

        cpu.end_phase();
        output.with_info(serde_json::json!({
            "triggers_fired": result.triggers_fired,
            "consolidation_count": result.consolidation_count,
            "conflicts_found": result.conflicts_found,
            "duration_ms": result.duration_ms,
            "note": "think() bridge not yet active; results empty until YantrikdbProvider::think() bridging",
        }))
    }

    // -- phase 6: health report ---------------------------------------------

    /// Aggregate stats from all phases + memory provider and write a JSON
    /// health snapshot to `~/.aman/agents/{agent_id}/health/sleep_{timestamp_ms}.json`.
    async fn phase_health_report(
        &self,
        agent_id: &str,
        _cancel: &tokio_util::sync::CancellationToken,
        cpu: &mut CpuTracker,
        phase_outputs: &[SleepPhaseOutput],
    ) -> SleepPhaseOutput {
        cpu.start_phase();
        let output = SleepPhaseOutput::new("health_report");

        let provider = self.memory_provider.get();

        // Collect current memory stats
        let stats = if let Some(p) = provider {
            p.stats(agent_id).await.unwrap_or(MemoryStats {
                total_entries: 0,
                index_size_bytes: 0,
                graph_nodes: 0,
                graph_edges: 0,
                pending_conflicts: 0,
            })
        } else {
            MemoryStats {
                total_entries: 0,
                index_size_bytes: 0,
                graph_nodes: 0,
                graph_edges: 0,
                pending_conflicts: 0,
            }
        };

        let recent_memory_count: u64 = if let Some(p) = provider {
            p.session_history(agent_id, 100)
                .await
                .unwrap_or_default()
                .iter()
                .map(|s| s.memory_count)
                .sum()
        } else {
            0
        };

        let timestamp_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();

        let phases_json: serde_json::Map<_, _> = phase_outputs
            .iter()
            .map(|p| {
                (
                    p.label.clone(),
                    if p.info.is_null() {
                        serde_json::Value::String("skipped".into())
                    } else {
                        p.info.clone()
                    },
                )
            })
            .collect();

        let snapshot = serde_json::json!({
            "timestamp_ms": timestamp_ms,
            "agent_id": agent_id,
            "phases": phases_json,
            "memory_stats": {
                "total_entries": stats.total_entries,
                "index_size_bytes": stats.index_size_bytes,
                "graph_nodes": stats.graph_nodes,
                "graph_edges": stats.graph_edges,
                "pending_conflicts": stats.pending_conflicts,
                "recent_memory_count": recent_memory_count,
            },
            "total_cpu_seconds": cpu.total_elapsed(),
        });

        // Write to ~/.aman/agents/{agent_id}/health/
        let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_owned());
        let health_dir = PathBuf::from(&home)
            .join(".aman")
            .join("agents")
            .join(agent_id)
            .join("health");
        if let Err(e) = std::fs::create_dir_all(&health_dir) {
            warn!(agent_id, error = %e, path = %health_dir.display(), "Sleep phase 6: failed to create health dir");
            cpu.end_phase();
            return output.with_info(serde_json::json!({"error": format!("mkdir failed: {e}")}));
        }

        let filename = format!("sleep_{timestamp_ms}.json");
        let health_path = health_dir.join(&filename);
        let content = serde_json::to_string_pretty(&snapshot).unwrap_or_default();

        match std::fs::write(&health_path, &content) {
            Ok(()) => {
                info!(
                    agent_id,
                    path = %health_path.display(),
                    cpu_secs = cpu.total_elapsed(),
                    "Sleep phase 6: health report written"
                );
            }
            Err(e) => {
                warn!(agent_id, error = %e, path = %health_path.display(), "Sleep phase 6: failed to write health report");
            }
        }

        cpu.end_phase();
        output.with_info(snapshot)
    }

    // -- guard helpers -------------------------------------------------------

    /// Try to acquire the per-agent run guard. Returns true if acquired.
    fn try_acquire(&self, agent_id: &str) -> bool {
        self.active_runs
            .write()
            .unwrap()
            .insert(agent_id.to_owned())
    }

    /// Release the per-agent run guard.
    fn release(&self, agent_id: &str) {
        self.active_runs.write().unwrap().remove(agent_id);
    }
}

impl Default for SleepRunner {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// EventHandler impl
// ---------------------------------------------------------------------------

#[async_trait]
impl EventHandler for SleepRunner {
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
        let Some(agent_id) = event.payload.get("agentId").and_then(|v| v.as_str()) else {
            return Ok(());
        };
        if agent_id.is_empty() {
            return Ok(());
        }

        // Guard: skip if already running for this agent
        if !self.try_acquire(agent_id) {
            debug!(
                agent_id,
                "SleepRunner: already running for agent, skipping duplicate trigger"
            );
            return Ok(());
        }

        // Ensure release on all exit paths
        let result = self.run_phases(agent_id).await;
        self.release(agent_id);
        result
    }
}

// ---------------------------------------------------------------------------
// Phase orchestration
// ---------------------------------------------------------------------------

impl SleepRunner {
    /// Run all 6 Sleep phases sequentially with cancellation and CPU budget checks.
    async fn run_phases(&self, agent_id: &str) -> AmanResult<()> {
        // Get cancel token from the agent's idle coordination
        let cancel_token = {
            let Some(registry) = self.agent_registry.get() else {
                debug!("SleepRunner: no AgentRegistry, skipping");
                return Ok(());
            };
            let Some(coord) = registry.get_idle_coordination(agent_id).await else {
                debug!(agent_id, "SleepRunner: no idle coordination for agent, skipping");
                return Ok(());
            };
            coord.idle_cancel_token.read().await.clone()
        };

        // Get sleep config (with defaults if not injected)
        let sleep_cfg = self.sleep_config.get().cloned().unwrap_or_default();

        let mut cpu = CpuTracker::new(sleep_cfg.max_cpu_seconds as f64);
        let mut phase_outputs: Vec<SleepPhaseOutput> = Vec::with_capacity(6);

        // Macro for per-phase guard: check cancel + budget, run phase, push output
        macro_rules! run_phase {
            ($phase_num:literal, $method:ident $(, $arg:expr)*) => {
                if cancel_token.is_cancelled() {
                    info!(
                        agent_id,
                        completed_phases = phase_outputs.len(),
                        "SleepRunner: cancelled before phase {}",
                        $phase_num,
                    );
                    return Ok(());
                }
                if !cpu.budget_remaining() && $phase_num < 6 {
                    debug!(
                        agent_id,
                        phase = $phase_num,
                        elapsed = cpu.total_elapsed(),
                        "SleepRunner: CPU budget exhausted, skipping phase {}",
                        $phase_num,
                    );
                    // fall through: health report (phase 6) always runs
                } else {
                    let output = self.$method(agent_id, &cancel_token, &mut cpu $(, $arg)*).await;
                    debug!(
                        agent_id,
                        phase = $phase_num,
                        label = %output.label,
                        "SleepRunner: phase {} complete",
                        $phase_num,
                    );
                    phase_outputs.push(output);
                }
            };
        }

        // Phases 1–5: subject to CPU budget (except phase 6 always runs)
        run_phase!(1, phase_session_backfill);
        run_phase!(2, phase_temporal_housekeeping, sleep_cfg.short_term_retention_days);
        run_phase!(3, phase_cache_cleanup, sleep_cfg.cache_expiry_days);
        run_phase!(4, phase_index_monitoring);
        run_phase!(5, phase_cognitive_consolidation);

        // Phase 6: health report always runs (cost is negligible, serves as completion signal)
        {
            let output = self
                .phase_health_report(agent_id, &cancel_token, &mut cpu, &phase_outputs)
                .await;
            phase_outputs.push(output);
        }

        // Log completion snapshot
        let phases_run = phase_outputs.len();
        let total_cpu = cpu.total_elapsed();
        info!(
            agent_id,
            phases_run,
            total_cpu_secs = total_cpu,
            "SleepRunner: all phases complete"
        );

        Ok(())
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
    use kernel::memory::MemoryRecord;
    use std::sync::atomic::{AtomicBool, Ordering};

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
        // info is null and should be skipped
        assert!(json.get("info").is_none());
    }

    #[test]
    fn phase_output_with_info_serializes_info() {
        let output = SleepPhaseOutput::new("test_phase")
            .with_info(serde_json::json!({"count": 42}));
        let json = serde_json::to_value(&output).unwrap();
        assert_eq!(json["label"], "test_phase");
        assert_eq!(json["info"]["count"], 42);
    }

    // -----------------------------------------------------------------------
    // SleepRunner guard tests
    // -----------------------------------------------------------------------

    #[test]
    fn guard_acquire_twice_fails() {
        let runner = SleepRunner::new();
        assert!(runner.try_acquire("agent-1"));
        assert!(!runner.try_acquire("agent-1"));
        runner.release("agent-1");
        // After release, can acquire again
        assert!(runner.try_acquire("agent-1"));
        runner.release("agent-1");
    }

    #[test]
    fn guard_different_agents_independent() {
        let runner = SleepRunner::new();
        assert!(runner.try_acquire("agent-1"));
        assert!(runner.try_acquire("agent-2"));
        runner.release("agent-1");
        runner.release("agent-2");
    }

    // -----------------------------------------------------------------------
    // EventHandler filter tests
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn handle_ignores_non_idle_events() {
        let runner = SleepRunner::new();
        let event = Event::new(
            "chat:user",
            EventType::MessageReceived,
            serde_json::json!({"text": "hello"}),
        );
        let result = runner.handle(event).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn handle_ignores_non_sleep_idle_events() {
        let runner = SleepRunner::new();
        let event = Event::new(
            "idle.system",
            EventType::Idle,
            serde_json::json!({"kind": "daze", "depth": 0}),
        );
        let result = runner.handle(event).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn handle_ignores_idle_without_agent_id() {
        let runner = SleepRunner::new();
        let event = Event::new(
            "idle.system",
            EventType::Idle,
            serde_json::json!({"kind": "sleep", "depth": 20}),
        );
        let result = runner.handle(event).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn handle_skips_when_already_running() {
        let runner = SleepRunner::new();
        runner.try_acquire("agent-1");

        let event = Event::new(
            "idle.system",
            EventType::Idle,
            serde_json::json!({"kind": "sleep", "depth": 20, "agentId": "agent-1"}),
        );
        let result = runner.handle(event).await;
        assert!(result.is_ok());
        // Cleanup for other tests
        runner.release("agent-1");
    }

    // -----------------------------------------------------------------------
    // Phase unit tests (with real in-memory MemoryProvider)
    // -----------------------------------------------------------------------

    /// An in-memory MemoryProvider for unit testing Sleep phases.
    struct TestMemoryProvider {
        available: AtomicBool,
    }

    impl TestMemoryProvider {
        fn new() -> Self {
            Self {
                available: AtomicBool::new(true),
            }
        }
    }

    #[async_trait::async_trait]
    impl MemoryProvider for TestMemoryProvider {
        fn name(&self) -> &str {
            "test-memory"
        }

        fn is_available(&self) -> bool {
            self.available.load(Ordering::Relaxed)
        }

        fn store(&self, _agent_id: &str, _content: &str, _tags: Vec<String>) -> String {
            "test-rid".into()
        }

        async fn recall(
            &self,
            _agent_id: &str,
            _query: &str,
            _limit: usize,
        ) -> Vec<MemoryRecord> {
            vec![]
        }

        fn list(
            &self,
            _agent_id: &str,
            _filter: Option<&kernel::memory::MemoryFilter>,
        ) -> Vec<MemoryRecord> {
            vec![]
        }

        fn forget(&self, _agent_id: &str, _rid: &str) -> bool {
            true
        }

        async fn session_start(&self, _agent_id: &str, _session_type: &str) -> AmanResult<String> {
            Ok("test-session".into())
        }

        async fn session_end(
            &self,
            _agent_id: &str,
            _session_id: &str,
        ) -> AmanResult<kernel::memory::SessionSummary> {
            Ok(kernel::memory::SessionSummary {
                session_id: "test-session".into(),
                memory_count: 0,
                duration_secs: 0.0,
                topics: vec![],
            })
        }

        async fn session_history(
            &self,
            _agent_id: &str,
            _limit: usize,
        ) -> AmanResult<Vec<kernel::memory::SessionSummary>> {
            Ok(vec![])
        }

        async fn relate(&self, _from: &str, _to: &str, _rel_type: &str) -> AmanResult<()> {
            Ok(())
        }

        async fn get_edges(&self, _entity: &str) -> AmanResult<Vec<(String, String, String)>> {
            Ok(vec![])
        }

        async fn search_entities(&self, _query: &str, _limit: usize) -> AmanResult<Vec<String>> {
            Ok(vec![])
        }

        async fn entity_profile(
            &self,
            _entity: &str,
        ) -> AmanResult<Option<kernel::memory::EntityProfile>> {
            Ok(None)
        }

        async fn stale_memories(
            &self,
            _agent_id: &str,
            _days: u32,
        ) -> AmanResult<Vec<MemoryRecord>> {
            Ok(vec![])
        }

        async fn upcoming_memories(
            &self,
            _agent_id: &str,
            _days: u32,
        ) -> AmanResult<Vec<MemoryRecord>> {
            Ok(vec![])
        }

        async fn store_procedural(
            &self,
            _agent_id: &str,
            _name: &str,
            _schema: &str,
            _kind: &str,
        ) -> AmanResult<String> {
            Ok("proc-1".into())
        }

        async fn surface_procedural(
            &self,
            _agent_id: &str,
            _context: &str,
            _limit: usize,
        ) -> AmanResult<Vec<MemoryRecord>> {
            Ok(vec![])
        }

        async fn stats(&self, _agent_id: &str) -> AmanResult<kernel::memory::MemoryStats> {
            Ok(kernel::memory::MemoryStats {
                total_entries: 100,
                index_size_bytes: 1024 * 1024,
                graph_nodes: 10,
                graph_edges: 25,
                pending_conflicts: 0,
            })
        }

        async fn think(
            &self,
            _agent_id: &str,
            _config: &ThinkConfig,
        ) -> AmanResult<ThinkResult> {
            Ok(ThinkResult::default())
        }
    }

    #[tokio::test]
    async fn phase_2_temporal_housekeeping_no_stale_memories() {
        let provider: Arc<dyn MemoryProvider> = Arc::new(TestMemoryProvider::new());
        let runner = SleepRunner::new();
        runner.set_memory_provider(provider);

        let cancel = tokio_util::sync::CancellationToken::new();
        let mut cpu = CpuTracker::new(300.0);
        let output = runner
            .phase_temporal_housekeeping("agent-1", &cancel, &mut cpu, 7)
            .await;

        let json = serde_json::to_value(&output).unwrap();
        assert_eq!(json["label"], "temporal_housekeeping");
        assert_eq!(json["info"]["stale_count"], 0);
    }

    #[tokio::test]
    async fn phase_4_index_monitoring_returns_stats() {
        let provider: Arc<dyn MemoryProvider> = Arc::new(TestMemoryProvider::new());
        let runner = SleepRunner::new();
        runner.set_memory_provider(provider);

        let cancel = tokio_util::sync::CancellationToken::new();
        let mut cpu = CpuTracker::new(300.0);
        let output = runner
            .phase_index_monitoring("agent-1", &cancel, &mut cpu)
            .await;

        let json = serde_json::to_value(&output).unwrap();
        assert_eq!(json["label"], "index_monitoring");
        assert_eq!(json["info"]["total_entries"], 100);
        assert_eq!(json["info"]["graph_nodes"], 10);
    }

    #[tokio::test]
    async fn phase_5_cognitive_consolidation_returns_empty() {
        let provider: Arc<dyn MemoryProvider> = Arc::new(TestMemoryProvider::new());
        let runner = SleepRunner::new();
        runner.set_memory_provider(provider);

        let cancel = tokio_util::sync::CancellationToken::new();
        let mut cpu = CpuTracker::new(300.0);
        let output = runner
            .phase_cognitive_consolidation("agent-1", &cancel, &mut cpu)
            .await;

        let json = serde_json::to_value(&output).unwrap();
        assert_eq!(json["label"], "cognitive_consolidation");
        assert_eq!(json["info"]["consolidation_count"], 0);
        assert!(json["info"]["note"].as_str().unwrap().contains("bridge"));
    }

    #[tokio::test]
    async fn phase_1_session_backfill_skips_without_provider() {
        let runner = SleepRunner::new();
        let cancel = tokio_util::sync::CancellationToken::new();
        let mut cpu = CpuTracker::new(300.0);
        let output = runner
            .phase_session_backfill("agent-1", &cancel, &mut cpu)
            .await;
        let json = serde_json::to_value(&output).unwrap();
        assert_eq!(json["info"]["status"], "skipped");
    }

    #[tokio::test]
    async fn phase_2_skips_without_provider() {
        let runner = SleepRunner::new();
        let cancel = tokio_util::sync::CancellationToken::new();
        let mut cpu = CpuTracker::new(300.0);
        let output = runner
            .phase_temporal_housekeeping("agent-1", &cancel, &mut cpu, 7)
            .await;
        let json = serde_json::to_value(&output).unwrap();
        assert_eq!(json["info"]["status"], "skipped");
    }
}
