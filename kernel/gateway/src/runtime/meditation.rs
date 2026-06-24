// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! Meditation runner — triggered by IdleEvent{kind="meditation"} when the
//! agent reaches idle depth 100+. Runs deep memory reflection: surfaces
//! patterns, connects distant concepts, and stores insights.
//!
//! Follows the same dependency-injection pattern as [`ExplorationRunner`].

use async_trait::async_trait;
use config::MeditationConfig;
use event_bus::{try_publish, EventHandler, EventBus};
use idle::IdleKind;
use kernel::event::{Event, EventType};
use kernel::memory::{EntityProfile, MemoryProvider, MemoryStats, ThinkConfig, ThinkResult};
use kernel::trace::TraceStore;
use kernel::AmanResult;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock, RwLock};
use std::time::Instant;
use tracing::{debug, info, warn};

use super::agent_registry::AgentRegistry;

/// Default poll_interval used to convert min_interval_ticks → seconds.
const DEFAULT_POLL_INTERVAL_SECS: f64 = 5.0;

/// Max entities to introspect per meditation cycle.
const MAX_ENTITY_INTROSPECTIONS: usize = 12;

type EntityIntrospection = (
    String,
    Option<EntityProfile>,
    Vec<(String, String, String)>,
);

pub struct MeditationRunner {
    agent_registry: OnceLock<Arc<AgentRegistry>>,
    meditation_config: OnceLock<MeditationConfig>,
    global_bus: OnceLock<Arc<dyn EventBus>>,
    active_runs: RwLock<HashSet<String>>,
    last_meditation_at: RwLock<HashMap<String, Instant>>,
    meditations_completed: AtomicU64,
}

impl MeditationRunner {
    pub fn new() -> Self {
        Self {
            agent_registry: OnceLock::new(),
            meditation_config: OnceLock::new(),
            global_bus: OnceLock::new(),
            active_runs: RwLock::new(HashSet::new()),
            last_meditation_at: RwLock::new(HashMap::new()),
            meditations_completed: AtomicU64::new(0),
        }
    }

    pub fn set_agent_registry(&self, registry: Arc<AgentRegistry>) {
        let _ = self.agent_registry.set(registry);
    }

    /// Look up the per-agent memory provider from the registry.
    async fn memory_for(&self, agent_id: &str) -> Option<Arc<dyn MemoryProvider>> {
        self.agent_registry.get()?.get_memory_provider(agent_id).await
    }

    /// Look up the per-agent trace store from the registry.
    async fn trace_store_for(&self, agent_id: &str) -> Option<Arc<dyn TraceStore>> {
        self.agent_registry.get()?.get_trace_store(agent_id).await
    }

    pub fn set_meditation_config(&self, config: MeditationConfig) {
        let _ = self.meditation_config.set(config);
    }

    pub fn set_global_bus(&self, bus: Arc<dyn EventBus>) {
        let _ = self.global_bus.set(bus);
    }

    fn try_acquire(&self, agent_id: &str) -> bool {
        self.active_runs
            .write()
            .unwrap()
            .insert(agent_id.to_owned())
    }

    fn release(&self, agent_id: &str) {
        self.active_runs.write().unwrap().remove(agent_id);
    }

    async fn signal_cooldown(&self, agent_id: &str) {
        let Some(registry) = self.agent_registry.get() else {
            return;
        };
        let Some(coord) = registry.get_idle_coordination(agent_id).await else {
            return;
        };
        let config = self.meditation_config.get();
        let cooldown_secs = config.map(|c| c.cooldown_secs).unwrap_or(7200);
        let wakeup_delay = config.map(|c| c.wakeup_delay_secs).unwrap_or(60);
        let wakeup_steps = config.map(|c| c.wakeup_poll_steps).unwrap_or(2);
        coord
            .set_kind_cooldown(IdleKind::Meditation, cooldown_secs)
            .await;
        debug!(
            agent_id,
            cooldown_secs,
            "Meditation: cooldown set",
        );
        coord.schedule_wakeup(wakeup_delay, wakeup_steps).await;
        info!(
            agent_id,
            delay_secs = wakeup_delay,
            poll_steps = wakeup_steps,
            "Meditation: wake-up scheduled (Ouroboros)"
        );
    }

    async fn run_phases(&self, agent_id: &str) -> AmanResult<()> {
        let started = Instant::now();

        // Get cancel token from idle coordination so real events can interrupt
        // deep introspection (design: idle-patch.md §4.4 — "丢弃，temp+rename 文件安全").
        let cancel_token = {
            let Some(registry) = self.agent_registry.get() else {
                debug!(agent_id, "MeditationRunner: no AgentRegistry");
                return Ok(());
            };
            match registry.get_idle_coordination(agent_id).await {
                Some(coord) => coord.idle_cancel_token.read().await.clone(),
                None => {
                    debug!(agent_id, "MeditationRunner: no idle coordination, running uncancellable");
                    tokio_util::sync::CancellationToken::new()
                }
            }
        };

        macro_rules! check_cancel {
            ($phase:literal) => {
                if cancel_token.is_cancelled() {
                    info!(
                        agent_id,
                        phase = $phase,
                        "Meditation: cancelled by real event, discarding work"
                    );
                    return Ok(());
                }
            };
        }

        let Some(provider) = self.memory_for(agent_id).await else {
            debug!(agent_id, "MeditationRunner: no MemoryProvider");
            return Ok(());
        };

        // ── Phase 1: 前置检查 ──────────────────────────────────────────
        let review_depth = self
            .meditation_config
            .get()
            .map(|c| c.review_depth)
            .unwrap_or(20);
        let min_interval_ticks = self
            .meditation_config
            .get()
            .map(|c| c.min_interval_ticks)
            .unwrap_or(20) as f64;
        let min_interval_secs = (min_interval_ticks * DEFAULT_POLL_INTERVAL_SECS) as u64;
        {
            let last = self.last_meditation_at.read().unwrap();
            if let Some(prev) = last.get(agent_id) {
                let elapsed = prev.elapsed().as_secs();
                if elapsed < min_interval_secs {
                    debug!(
                        agent_id,
                        elapsed,
                        min_interval_secs,
                        min_interval_ticks,
                        "Meditation skipped — min_interval not met"
                    );
                    return Ok(());
                }
            }
        }
        // 先占位，防止并发触发
        {
            self.last_meditation_at
                .write()
                .unwrap()
                .insert(agent_id.to_owned(), Instant::now());
        }

        check_cancel!(1);

        // ── Phase 2: 加载经验链 ────────────────────────────────────────
        let traces = match self.trace_store_for(agent_id).await {
            Some(ts) => match ts.load_recent(agent_id, review_depth).await {
                Ok(t) => t,
                Err(e) => {
                    warn!(agent_id, error = %e, "Meditation: failed to load traces");
                    Vec::new()
                }
            },
            None => {
                debug!(agent_id, "Meditation: no TraceStore, skipping trace-dependent phases");
                Vec::new()
            }
        };

        check_cancel!(2);

        // ── Phase 3: 知识图谱内省 ──────────────────────────────────────
        let kg_stats = provider.stats(agent_id).await?;
        info!(
            agent_id,
            nodes = kg_stats.graph_nodes,
            edges = kg_stats.graph_edges,
            pending_conflicts = kg_stats.pending_conflicts,
            total_entries = kg_stats.total_entries,
            "Meditation: KG snapshot",
        );

        // Per-entity introspections from recent trace entities
        let mut entity_introspections: Vec<EntityIntrospection> = Vec::new();
        if !traces.is_empty() {
            let mut seen = HashSet::new();
            for trace in traces.iter().take(review_depth) {
                if cancel_token.is_cancelled() {
                    break;
                }
                for entity in &trace.entities {
                    if seen.len() >= MAX_ENTITY_INTROSPECTIONS {
                        break;
                    }
                    if seen.insert(entity.clone()) {
                        let profile = provider.entity_profile(entity).await.unwrap_or_else(|e| {
                            debug!(agent_id, entity, error = %e, "Meditation: entity_profile failed");
                            None
                        });
                        let edges = provider.get_edges(entity).await.unwrap_or_else(|e| {
                            debug!(agent_id, entity, error = %e, "Meditation: get_edges failed");
                            Vec::new()
                        });
                        entity_introspections.push((entity.clone(), profile, edges));
                    }
                }
            }
            if !entity_introspections.is_empty() {
                info!(
                    agent_id,
                    entities_introspected = entity_introspections.len(),
                    "Meditation: entity introspection complete",
                );
            }
        }

        if cancel_token.is_cancelled() {
            info!(agent_id, "Meditation: cancelled during entity introspection");
            return Ok(());
        }

        // Conflict detection / pending conflicts from stats
        if kg_stats.pending_conflicts > 0 {
            info!(
                agent_id,
                conflicts = kg_stats.pending_conflicts,
                "Meditation: pending KG conflicts detected",
            );
        }

        check_cancel!(3);

        // ── Phase 4: 模式提取 ──────────────────────────────────────────
        let mut success_patterns: Vec<String> = Vec::new();
        let mut failure_patterns: Vec<String> = Vec::new();
        let mut procedural_updates: u64 = 0;

        if !traces.is_empty() {
            for trace in &traces {
                if cancel_token.is_cancelled() {
                    break;
                }
                let context = format!(
                    "{}: {} → {:?}",
                    trace.task_type, trace.description, trace.outcome
                );
                let strategies = provider
                    .surface_procedural(agent_id, &context, 5usize)
                    .await
                    .unwrap_or_else(|e| {
                        debug!(agent_id, error = %e, "Meditation: surface_procedural failed");
                        Vec::new()
                    });

                match trace.outcome {
                    kernel::trace::TraceOutcome::Success => {
                        let desc = format!("{} — {}", trace.task_type, trace.description);
                        if strategies.is_empty() {
                            // Novel success — candidate for new procedural memory
                            let pattern_name = format!("success_pattern_{}", &trace.trace_id[..8.min(trace.trace_id.len())]);
                            let schema = serde_json::json!({
                                "task_type": trace.task_type,
                                "description": trace.description,
                                "entities": trace.entities,
                                "tool_count": trace.tool_calls.len(),
                                "decision_count": trace.decision_points.len(),
                                "duration_ms": trace.duration_ms,
                            });
                            match provider
                                .store_procedural(
                                    agent_id,
                                    &pattern_name,
                                    &schema.to_string(),
                                    "success_candidate",
                                )
                                .await
                            {
                                Ok(_) => {
                                    procedural_updates += 1;
                                    success_patterns.push(desc);
                                }
                                Err(e) => {
                                    debug!(agent_id, error = %e, "Meditation: store_procedural failed");
                                }
                            }
                        } else {
                            success_patterns.push(desc);
                        }
                    }
                    kernel::trace::TraceOutcome::Failure => {
                        let desc = format!(
                            "{} — {} (errors: {})",
                            trace.task_type,
                            trace.description,
                            trace
                                .errors
                                .iter()
                                .map(|e| e.error_type.as_str())
                                .collect::<Vec<_>>()
                                .join(", ")
                        );
                        // Store failure signatures for future avoidance
                        for err in &trace.errors {
                            let schema = serde_json::json!({
                                "error_type": err.error_type,
                                "error_message": err.error_message,
                                "recovered": err.recovered,
                                "task_type": trace.task_type,
                                "recovery_action": err.recovery_action,
                            });
                            let _ = provider
                                .store_procedural(
                                    agent_id,
                                    &format!("failure_sig_{}", &err.error_type),
                                    &schema.to_string(),
                                    "failure_signature",
                                )
                                .await;
                        }
                        failure_patterns.push(desc);
                        procedural_updates += 1;
                    }
                    _ => {} // Partial / Cancelled — skip for now
                }
            }

            if !success_patterns.is_empty() || !failure_patterns.is_empty() {
                info!(
                    agent_id,
                    success = success_patterns.len(),
                    failure = failure_patterns.len(),
                    procedural_updates,
                    "Meditation: pattern extraction complete",
                );
            }
        } else {
            debug!(agent_id, "Meditation: no traces loaded, skipping pattern extraction");
        }

        check_cancel!(4);

        // ── Phase 5: 认知循环 (think) ──────────────────────────────────
        let config = ThinkConfig {
            importance_threshold: 0.4,
            run_consolidation: true,
            run_conflict_scan: true,
        };

        let think_result: ThinkResult = provider.think(agent_id, &config).await?;
        let stored = think_result.triggers_fired + think_result.consolidation_count;

        check_cancel!(5);

        // ── Phase 6: 冥想报告 ──────────────────────────────────────────
        let report_path = self.write_meditation_report(
            agent_id,
            &kg_stats,
            &think_result,
            started.elapsed().as_millis() as u64,
            &entity_introspections,
            &success_patterns,
            &failure_patterns,
            procedural_updates,
            traces.len(),
        )?;
        info!(agent_id, %report_path, "Meditation: report written");

        // ── Phase 7: 收尾 ──────────────────────────────────────────────
        let completed = self.meditations_completed.fetch_add(1, Ordering::SeqCst) + 1;
        info!(
            agent_id,
            meditations_completed = completed,
            duration_ms = started.elapsed().as_millis(),
            "Meditation: cycle complete",
        );

        // Publish completion event + reset depth if this cycle was productive.
        let productive = procedural_updates > 0
            || !entity_introspections.is_empty()
            || stored > 0;
        if productive {
            if let Some(bus) = self.global_bus.get() {
                let event = Event::new(
                    format!("idle:meditation:{agent_id}"),
                    EventType::Custom("idle.cycle_completed".to_owned()),
                    serde_json::json!({
                        "kind": "meditation",
                        "agentId": agent_id,
                        "stored": stored,
                        "proceduralUpdates": procedural_updates,
                        "entitiesIntrospected": entity_introspections.len(),
                        "durationMs": started.elapsed().as_millis(),
                    }),
                );
                try_publish(&**bus, event).await;
            }
            if let Some(registry) = self.agent_registry.get()
                && let Some(coord) = registry.get_idle_coordination(agent_id).await {
                    coord.pending_depth_reset.store(true, Ordering::SeqCst);
                }
        }

        self.signal_cooldown(agent_id).await;
        Ok(())
    }

    /// Phase 6: Write meditation report via atomic write.
    #[allow(clippy::too_many_arguments)]
    fn write_meditation_report(
        &self,
        agent_id: &str,
        stats: &MemoryStats,
        think: &ThinkResult,
        duration_ms: u64,
        entity_introspections: &[EntityIntrospection],
        success_patterns: &[String],
        failure_patterns: &[String],
        procedural_updates: u64,
        traces_loaded: usize,
    ) -> AmanResult<String> {
        let data_dir = super::agent_seed::aman_data_dir();
        let report_dir = data_dir
            .join("narrative")
            .join("meditation")
            .join(agent_id);
        fs::create_dir_all(&report_dir)?;

        let unix_ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let final_path = report_dir.join(format!("{unix_ts}.md"));

        let entity_section =if entity_introspections.is_empty() {
            "- Entity introspection: skipped (no trace entities)\n".to_owned()
        } else {
            let mut s = String::new();
            for (name, profile, edges) in entity_introspections {
                let edge_count = profile.as_ref().map(|p| p.edge_count).unwrap_or(edges.len());
                let related = profile
                    .as_ref()
                    .map(|p| p.related_entities.join(", "))
                    .unwrap_or_default();
                s.push_str(&format!(
                    "  - **{name}** — {edge_count} edges, related: [{related}]\n"
                ));
            }
            s
        };

        let success_section = if success_patterns.is_empty() {
            "- None discovered\n".to_owned()
        } else {
            success_patterns
                .iter()
                .take(10)
                .map(|p| format!("  - {p}\n"))
                .collect::<String>()
        };

        let failure_section = if failure_patterns.is_empty() {
            "- None discovered\n".to_owned()
        } else {
            failure_patterns
                .iter()
                .take(10)
                .map(|p| format!("  - {p}\n"))
                .collect::<String>()
        };

        let report = format!(
            "# Meditation Report — {unix_ts}\n\
             \n\
             ## Executive Summary\n\
             Agent `{agent}` completed a deep introspection cycle ({duration_ms} ms).\n\
             Traces loaded: {traces_loaded}, entities introspected: {entities_count},\n\
             success patterns: {success_count}, failure patterns: {failure_count}.\n\
             \n\
             ## Knowledge Graph Status\n\
             - Nodes: {nodes}\n\
             - Edges: {edges}\n\
             - Pending Conflicts: {conflicts}\n\
             - Total Entries: {total}\n\
             \n\
             ## Entity Introspection\n\
             {entity_section}\
             \n\
             ## Success Patterns\n\
             {success_section}\
             \n\
             ## Failure Patterns\n\
             {failure_section}\
             \n\
             ## Procedural Memory Updates\n\
             - New/Updated: {proc_updates}\n\
             \n\
             ## Think Pass Summary\n\
             - Triggers Fired: {triggers}\n\
             - Consolidated: {consolidated}\n\
             - Conflicts Found: {conflicts_found}\n\
             \n\
             > Auto-generated by MeditationRunner — idle depth 100+\n",
            unix_ts = unix_ts,
            agent = agent_id,
            duration_ms = duration_ms,
            traces_loaded = traces_loaded,
            entities_count = entity_introspections.len(),
            success_count = success_patterns.len(),
            failure_count = failure_patterns.len(),
            nodes = stats.graph_nodes,
            edges = stats.graph_edges,
            conflicts = stats.pending_conflicts,
            total = stats.total_entries,
            entity_section = entity_section,
            success_section = success_section,
            failure_section = failure_section,
            proc_updates = procedural_updates,
            triggers = think.triggers_fired,
            consolidated = think.consolidation_count,
            conflicts_found = think.conflicts_found,
        );

        kernel::fs::atomic_write(&final_path, report.as_bytes())?;
        let _ = kernel::fs::cleanup_temp_files(&report_dir, 86400);

        Ok(final_path.display().to_string())
    }
}

impl Default for MeditationRunner {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl EventHandler for MeditationRunner {
    async fn handle(&self, event: Event) -> AmanResult<()> {
        if event.event_type != EventType::Idle {
            return Ok(());
        }
        let Some(kind) = event.payload.get("kind").and_then(|v| v.as_str()) else {
            return Ok(());
        };
        if kind != "meditation" {
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

        if !self.try_acquire(agent_id) {
            debug!(agent_id, "MeditationRunner: already running, skipping");
            return Ok(());
        }

        let result = self.run_phases(agent_id).await;
        self.release(agent_id);
        result
    }
}
