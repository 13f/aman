// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! Gateway implementation of [`SleepHousekeeper`] — provides the concrete
//! phase logic for the Sleep workflow, delegating to [`AgentRegistry`],
//! [`SessionStore`], [`MemoryProvider`], [`LlmProvider`], and the filesystem.
//!
//! Phase orchestration (ordering, CPU budget, cancellation) lives in the
//! idle crate's [`SleepActor`]; this module only implements the I/O work.
//!
//! Architecture ref: idle-patch.md §4

use async_trait::async_trait;
use config::MemoryLlmConfig;
use idle::{IdleKind, SleepHousekeeper, SleepPhaseOutput};
use kernel::memory::{MemoryProvider, MemoryStats, ThinkConfig, ThinkResult};
use kernel::AmanResult;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use super::agent_registry::AgentRegistry;

// ---------------------------------------------------------------------------
// GatewaySleepHousekeeper
// ---------------------------------------------------------------------------

/// Gateway implementation of [`SleepHousekeeper`].
///
/// Holds references to the agent registry and per-agent services. Each phase
/// method looks up the necessary service(s) from the registry, performs the
/// work, and returns a JSON info blob for the health report.
pub struct GatewaySleepHousekeeper {
    agent_registry: Arc<AgentRegistry>,
    /// LLM config for Phase 1 session backfill (same model as Reflection).
    memory_llm: Option<MemoryLlmConfig>,
    /// Sleep actor config (cooldown, wake-up timing, etc.).
    sleep_config: idle::SleepActorConfig,
}

impl GatewaySleepHousekeeper {
    pub fn new(
        agent_registry: Arc<AgentRegistry>,
        memory_llm: Option<MemoryLlmConfig>,
        sleep_config: idle::SleepActorConfig,
    ) -> Self {
        Self {
            agent_registry,
            memory_llm,
            sleep_config,
        }
    }

    /// Max health report files to retain per agent before pruning oldest.
    const MAX_HEALTH_FILES: usize = 64;

    /// Look up the per-agent memory provider from the registry.
    async fn memory_for(&self, agent_id: &str) -> Option<Arc<dyn MemoryProvider>> {
        self.agent_registry.get_memory_provider(agent_id).await
    }

    /// Prune oldest health report files, keeping at most `MAX_HEALTH_FILES`.
    fn prune_old_health_reports(health_dir: &std::path::Path, agent_id: &str) {
        let mut entries: Vec<(u128, PathBuf)> = match std::fs::read_dir(health_dir) {
            Ok(iter) => iter
                .filter_map(|e| e.ok())
                .filter_map(|e| {
                    let name = e.file_name().to_string_lossy().to_string();
                    let ts = name
                        .strip_prefix("sleep_")?
                        .strip_suffix(".json")?
                        .parse::<u128>()
                        .ok()?;
                    Some((ts, e.path()))
                })
                .collect(),
            Err(_) => return,
        };

        if entries.len() <= Self::MAX_HEALTH_FILES {
            return;
        }

        entries.sort_by(|a, b| b.0.cmp(&a.0));
        for (_ts, path) in entries.iter().skip(Self::MAX_HEALTH_FILES) {
            if let Err(e) = std::fs::remove_file(path) {
                warn!(agent_id, path = %path.display(), error = %e, "failed to prune old health report");
            }
        }

        let pruned = entries.len().saturating_sub(Self::MAX_HEALTH_FILES);
        debug!(
            agent_id,
            pruned,
            retained = Self::MAX_HEALTH_FILES.min(entries.len()),
            "pruned old health reports"
        );
    }
}

// ---------------------------------------------------------------------------
// SleepHousekeeper impl
// ---------------------------------------------------------------------------

#[async_trait]
impl SleepHousekeeper for GatewaySleepHousekeeper {
    // -- phase 0: empty session cleanup ------------------------------------

    async fn empty_session_cleanup(
        &self,
        agent_id: &str,
        _cancel: &CancellationToken,
    ) -> AmanResult<serde_json::Value> {
        let Some(store) = self.agent_registry.get_session_store(agent_id).await else {
            return Ok(serde_json::json!({"status": "skipped", "reason": "no session store"}));
        };

        match store.delete_empty_sessions() {
            Ok(0) => Ok(serde_json::json!({"deleted": 0})),
            Ok(n) => {
                info!(agent_id, deleted = n, "Sleep: cleaned up empty sessions");
                Ok(serde_json::json!({"deleted": n}))
            }
            Err(e) => {
                warn!(agent_id, error = %e, "Sleep: empty session cleanup failed");
                Ok(serde_json::json!({"error": e.to_string()}))
            }
        }
    }

    // -- phase 0b: stale background session cleanup -----------------------

    async fn stale_background_cleanup(
        &self,
        agent_id: &str,
        _cancel: &CancellationToken,
        retention_days: u64,
        min_reply_chars: usize,
    ) -> AmanResult<serde_json::Value> {
        let Some(store) = self.agent_registry.get_session_store(agent_id).await else {
            return Ok(serde_json::json!({"status": "skipped", "reason": "no session store"}));
        };

        match store.delete_stale_low_value_sessions(agent_id, retention_days, min_reply_chars) {
            Ok(0) => Ok(serde_json::json!({
                "deleted": 0,
                "retention_days": retention_days,
                "min_reply_chars": min_reply_chars,
            })),
            Ok(n) => {
                info!(
                    agent_id,
                    deleted = n,
                    retention_days,
                    min_reply_chars,
                    "Sleep: cleaned up stale low-value background sessions"
                );
                Ok(serde_json::json!({
                    "deleted": n,
                    "retention_days": retention_days,
                    "min_reply_chars": min_reply_chars,
                }))
            }
            Err(e) => {
                warn!(agent_id, error = %e, "Sleep: stale background session cleanup failed");
                Ok(serde_json::json!({"error": e.to_string()}))
            }
        }
    }

    // -- phase 1: session compression backfill ------------------------------

    async fn session_backfill(
        &self,
        agent_id: &str,
        cancel: &CancellationToken,
    ) -> AmanResult<serde_json::Value> {
        let Some(store) = self.agent_registry.get_session_store(agent_id).await else {
            debug!(agent_id, "Sleep phase 1: no SessionStore, skipping");
            return Ok(serde_json::json!({"status": "skipped", "reason": "no session store"}));
        };

        let Some(llm) = self.agent_registry.get_llm_provider(agent_id).await else {
            debug!(agent_id, "Sleep phase 1: no LlmProvider, skipping");
            return Ok(serde_json::json!({"status": "skipped", "reason": "no llm provider"}));
        };

        let Some(memory) = self.agent_registry.get_memory_provider(agent_id).await else {
            debug!(agent_id, "Sleep phase 1: no MemoryProvider, skipping");
            return Ok(serde_json::json!({"status": "skipped", "reason": "no memory provider"}));
        };

        // Process at most one unreflected session per Sleep cycle.
        let session = match store.list_unreflected() {
            Ok(Some(s)) => s,
            Ok(None) => {
                return Ok(serde_json::json!({"extracted": 0, "note": "no unreflected sessions"}));
            }
            Err(e) => {
                warn!(agent_id, error = %e, "Sleep phase 1: list_unreflected failed");
                return Ok(serde_json::json!({"error": e.to_string()}));
            }
        };

        if cancel.is_cancelled() {
            return Ok(serde_json::json!({"status": "cancelled"}));
        }

        let events = store.load_recent_events(&session.id, 200).await;
        if events.len() < 2 {
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as i64;
            let _ = store.mark_reflected(&session.id, now);
            return Ok(serde_json::json!({"extracted": 0, "note": "too few events"}));
        }

        info!(
            agent_id,
            session_id = %session.id,
            event_count = events.len(),
            "Sleep phase 1: backfilling unreflected session",
        );

        match super::reflection::session_extract_and_store(
            self.memory_llm.as_ref(),
            &llm,
            &memory,
            agent_id,
            &session.id,
            &events,
            48000,
        )
        .await
        {
            Ok(()) => {
                let now = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis() as i64;
                let _ = store.mark_reflected(&session.id, now);
                info!(agent_id, session_id = %session.id, "Sleep phase 1: session backfilled");
                Ok(serde_json::json!({"extracted": 1, "session_id": session.id}))
            }
            Err(e) => {
                warn!(agent_id, session_id = %session.id, error = %e, "Sleep phase 1: extraction failed");
                Ok(serde_json::json!({"error": e.to_string()}))
            }
        }
    }

    // -- phase 2: temporal housekeeping -------------------------------------

    async fn temporal_housekeeping(
        &self,
        agent_id: &str,
        cancel: &CancellationToken,
        retention_days: u64,
    ) -> AmanResult<serde_json::Value> {
        let Some(provider) = self.memory_for(agent_id).await else {
            debug!("Sleep: no MemoryProvider, skipping phase 2");
            return Ok(serde_json::json!({"status": "skipped", "reason": "no memory provider"}));
        };

        let stale = match provider.stale_memories(agent_id, retention_days as u32).await {
            Ok(s) => s,
            Err(e) => {
                warn!(agent_id, error = %e, "Sleep phase 2: stale_memories failed");
                return Ok(serde_json::json!({"error": e.to_string()}));
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

        Ok(serde_json::json!({
            "stale_count": stale.len(),
            "forgotten": forgotten,
            "flagged_for_review": flagged,
            "no_action": skipped,
            "retention_days": retention_days,
        }))
    }

    // -- phase 3: cache cleanup ---------------------------------------------

    async fn cache_cleanup(
        &self,
        agent_id: &str,
        cancel: &CancellationToken,
        cache_expiry_days: u64,
    ) -> AmanResult<serde_json::Value> {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_owned());
        let cache_dir = PathBuf::from(&home)
            .join(".aman")
            .join("agents")
            .join(agent_id)
            .join("cache");

        if !cache_dir.exists() || !cache_dir.is_dir() {
            debug!(agent_id, path = %cache_dir.display(), "Sleep phase 3: cache dir not found, skipping");
            return Ok(serde_json::json!({"status": "skipped", "reason": "no cache directory"}));
        }

        let cutoff = match SystemTime::now()
            .checked_sub(Duration::from_secs(cache_expiry_days * 86400))
        {
            Some(t) => t,
            None => {
                return Ok(serde_json::json!({"status": "skipped", "reason": "clock underflow"}));
            }
        };

        let (mut deleted, mut bytes_freed) = (0u64, 0u64);
        if let Err(e) = Self::walk_and_clean(&cache_dir, &cutoff, cancel, &mut deleted, &mut bytes_freed)
        {
            warn!(agent_id, error = %e, "Sleep phase 3: cache walk error");
        }

        Ok(serde_json::json!({
            "cache_dir": cache_dir.display().to_string(),
            "deleted_files": deleted,
            "bytes_freed": bytes_freed,
            "cache_expiry_days": cache_expiry_days,
        }))
    }

    // -- phase 4: index monitoring ------------------------------------------

    async fn index_monitoring(&self, agent_id: &str) -> AmanResult<serde_json::Value> {
        let Some(provider) = self.memory_for(agent_id).await else {
            debug!("Sleep: no MemoryProvider, skipping phase 4");
            return Ok(serde_json::json!({"status": "skipped", "reason": "no memory provider"}));
        };

        let stats = match provider.stats(agent_id).await {
            Ok(s) => s,
            Err(e) => {
                warn!(agent_id, error = %e, "Sleep phase 4: stats failed");
                return Ok(serde_json::json!({"error": e.to_string()}));
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

        Ok(serde_json::json!({
            "total_entries": stats.total_entries,
            "index_size_bytes": stats.index_size_bytes,
            "index_size_mb": size_mb,
            "graph_nodes": stats.graph_nodes,
            "graph_edges": stats.graph_edges,
            "pending_conflicts": stats.pending_conflicts,
        }))
    }

    // -- phase 5: cognitive consolidation -----------------------------------

    async fn cognitive_consolidation(&self, agent_id: &str) -> AmanResult<serde_json::Value> {
        let Some(provider) = self.memory_for(agent_id).await else {
            debug!("Sleep: no MemoryProvider, skipping phase 5");
            return Ok(serde_json::json!({"status": "skipped", "reason": "no memory provider"}));
        };

        let think_cfg = ThinkConfig::default();
        let result = provider
            .think(agent_id, &think_cfg)
            .await
            .unwrap_or_else(|e| {
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

        Ok(serde_json::json!({
            "triggers_fired": result.triggers_fired,
            "consolidation_count": result.consolidation_count,
            "conflicts_found": result.conflicts_found,
            "patterns_new": result.patterns_new,
            "patterns_updated": result.patterns_updated,
            "duration_ms": result.duration_ms,
        }))
    }

    // -- final phase: health report -----------------------------------------

    async fn health_report(
        &self,
        agent_id: &str,
        phase_outputs: &[SleepPhaseOutput],
        cpu_secs: f64,
    ) -> AmanResult<serde_json::Value> {
        let provider = self.memory_for(agent_id).await;

        let stats = if let Some(ref p) = provider {
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

        let recent_memory_count: u64 = if let Some(ref p) = provider {
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
            "total_cpu_seconds": cpu_secs,
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
            return Ok(serde_json::json!({"error": format!("mkdir failed: {e}")}));
        }

        let filename = format!("sleep_{timestamp_ms}.json");
        let health_path = health_dir.join(&filename);
        let content = serde_json::to_string_pretty(&snapshot).unwrap_or_default();

        match std::fs::write(&health_path, &content) {
            Ok(()) => {
                info!(
                    agent_id,
                    path = %health_path.display(),
                    cpu_secs,
                    "Sleep phase 6: health report written"
                );
                Self::prune_old_health_reports(&health_dir, agent_id);
            }
            Err(e) => {
                warn!(agent_id, error = %e, path = %health_path.display(), "Sleep phase 6: failed to write health report");
            }
        }

        Ok(snapshot)
    }

    /// Called after all Sleep phases complete. Sets a cooldown on
    /// `IdleKind::Sleep` so the idle detector does not immediately
    /// produce another Sleep event.
    async fn on_sleep_complete(&self, agent_id: &str, cooldown_secs: u64) -> AmanResult<()> {
        if let Some(coord) = self.agent_registry.get_idle_coordination(agent_id).await {
            coord.set_kind_cooldown(IdleKind::Sleep, cooldown_secs).await;
            debug!(
                agent_id,
                cooldown_secs,
                "Sleep: cooldown set"
            );
            // Schedule Ouroboros wake-up: any deep state completion triggers
            // a progressive reset of depth + arousal after the delay.
            coord
                .schedule_wakeup(
                    self.sleep_config.wakeup_delay_secs,
                    self.sleep_config.wakeup_poll_steps,
                )
                .await;
            info!(
                agent_id,
                delay_secs = self.sleep_config.wakeup_delay_secs,
                poll_steps = self.sleep_config.wakeup_poll_steps,
                "Sleep: wake-up scheduled (Ouroboros)"
            );
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

impl GatewaySleepHousekeeper {
    /// Recursively walk `dir`, deleting regular files older than `cutoff`.
    fn walk_and_clean(
        dir: &std::path::Path,
        cutoff: &SystemTime,
        cancel: &CancellationToken,
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
                    && modified < *cutoff
            {
                *bytes_freed += meta.len();
                if std::fs::remove_file(&path).is_ok() {
                    *deleted += 1;
                }
            }
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use kernel::memory::MemoryRecord;

    /// An in-memory MemoryProvider for unit testing Sleep phases.
    struct TestMemoryProvider {
        available: std::sync::atomic::AtomicBool,
    }

    impl TestMemoryProvider {
        fn new() -> Self {
            Self {
                available: std::sync::atomic::AtomicBool::new(true),
            }
        }
    }

    #[async_trait::async_trait]
    impl MemoryProvider for TestMemoryProvider {
        fn name(&self) -> &str {
            "test-memory"
        }

        fn is_available(&self) -> bool {
            self.available
                .load(std::sync::atomic::Ordering::Relaxed)
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

        async fn session_start(
            &self,
            _agent_id: &str,
            _session_type: &str,
        ) -> AmanResult<String> {
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

        async fn get_edges(
            &self,
            _entity: &str,
        ) -> AmanResult<Vec<(String, String, String)>> {
            Ok(vec![])
        }

        async fn search_entities(
            &self,
            _query: &str,
            _limit: usize,
        ) -> AmanResult<Vec<String>> {
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
        let bus: Arc<dyn event_bus::EventBus> =
            Arc::new(event_bus::InMemoryBus::new(Default::default()));
        let registry = Arc::new(AgentRegistry::new(bus));
        pollster::block_on(registry.set_memory_provider("agent-1", provider));
        let hk = GatewaySleepHousekeeper::new(registry, None, idle::SleepActorConfig::default());

        let cancel = CancellationToken::new();
        let info = hk
            .temporal_housekeeping("agent-1", &cancel, 7)
            .await
            .expect("temporal_housekeeping");
        assert_eq!(info["stale_count"], 0);
    }

    #[tokio::test]
    async fn phase_4_index_monitoring_returns_stats() {
        let provider: Arc<dyn MemoryProvider> = Arc::new(TestMemoryProvider::new());
        let bus: Arc<dyn event_bus::EventBus> =
            Arc::new(event_bus::InMemoryBus::new(Default::default()));
        let registry = Arc::new(AgentRegistry::new(bus));
        pollster::block_on(registry.set_memory_provider("agent-1", provider));
        let hk = GatewaySleepHousekeeper::new(registry, None, idle::SleepActorConfig::default());

        let info = hk
            .index_monitoring("agent-1")
            .await
            .expect("index_monitoring");
        assert_eq!(info["total_entries"], 100);
        assert_eq!(info["graph_nodes"], 10);
    }

    #[tokio::test]
    async fn phase_5_cognitive_consolidation_returns_empty() {
        let provider: Arc<dyn MemoryProvider> = Arc::new(TestMemoryProvider::new());
        let bus: Arc<dyn event_bus::EventBus> =
            Arc::new(event_bus::InMemoryBus::new(Default::default()));
        let registry = Arc::new(AgentRegistry::new(bus));
        pollster::block_on(registry.set_memory_provider("agent-1", provider));
        let hk = GatewaySleepHousekeeper::new(registry, None, idle::SleepActorConfig::default());

        let info = hk
            .cognitive_consolidation("agent-1")
            .await
            .expect("cognitive_consolidation");
        assert_eq!(info["consolidation_count"], 0);
        assert!(info["patterns_new"].is_number());
    }

    #[tokio::test]
    async fn phase_1_session_backfill_skips_without_provider() {
        let bus: Arc<dyn event_bus::EventBus> =
            Arc::new(event_bus::InMemoryBus::new(Default::default()));
        let registry = Arc::new(AgentRegistry::new(bus));
        let hk = GatewaySleepHousekeeper::new(registry, None, idle::SleepActorConfig::default());

        let cancel = CancellationToken::new();
        let info = hk
            .session_backfill("agent-1", &cancel)
            .await
            .expect("session_backfill");
        assert_eq!(info["status"], "skipped");
    }

    #[tokio::test]
    async fn phase_2_skips_without_provider() {
        let bus: Arc<dyn event_bus::EventBus> =
            Arc::new(event_bus::InMemoryBus::new(Default::default()));
        let registry = Arc::new(AgentRegistry::new(bus));
        let hk = GatewaySleepHousekeeper::new(registry, None, idle::SleepActorConfig::default());

        let cancel = CancellationToken::new();
        let info = hk
            .temporal_housekeeping("agent-1", &cancel, 7)
            .await
            .expect("temporal_housekeeping");
        assert_eq!(info["status"], "skipped");
    }

    #[tokio::test]
    async fn health_report_writes_without_provider() {
        let bus: Arc<dyn event_bus::EventBus> =
            Arc::new(event_bus::InMemoryBus::new(Default::default()));
        let registry = Arc::new(AgentRegistry::new(bus));
        let hk = GatewaySleepHousekeeper::new(registry, None, idle::SleepActorConfig::default());

        // Use a unique agent id to avoid polluting the user's ~/.aman directory.
        let test_id = format!("test-health-report-{}", uuid::Uuid::new_v4());

        let phase_outputs = vec![];
        let info = hk
            .health_report(&test_id, &phase_outputs, 1.5)
            .await
            .expect("health_report");
        assert_eq!(info["agent_id"], test_id.as_str());
        assert_eq!(info["total_cpu_seconds"], 1.5);

        // Clean up.
        let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_owned());
        let health_dir = std::path::PathBuf::from(&home)
            .join(".aman")
            .join("agents")
            .join(&test_id);
        let _ = std::fs::remove_dir_all(&health_dir);
    }
}
