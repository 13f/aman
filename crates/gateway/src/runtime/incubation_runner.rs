// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! Incubation runner — triggered by IdleEvent{kind="incubation"} when the
//! agent reaches idle depth 200+. Runs the deepest idle cycle: creative
//! synthesis, long-term memory trends, and emergent insight generation.
//!
//! Follows the same dependency-injection pattern as [`ExplorationRunner`].

use async_trait::async_trait;
use config::IncubationConfig;
use event_bus::{EventHandler, EventBus};
use idle::IdleKind;
use kernel::event::{Event, EventType};
use kernel::memory::{MemoryProvider, MemoryRecord, ThinkConfig};
use kernel::AmanResult;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::Ordering;
use std::sync::{Arc, OnceLock, RwLock};
use std::time::Instant;
use tracing::{debug, info};

use super::agent_registry::AgentRegistry;

/// Cross-domain recall batch size per query perspective.
const RECALL_LIMIT: usize = 25;
/// Max seeds to load for evolution.
const SEED_EVOLUTION_LIMIT: usize = 5;

/// Diverse query perspectives for cross-domain sampling (Phase 1).
const RECALL_QUERIES: &[&str] = &[
    "interesting patterns",
    "unexpected connections",
    "creative insights",
    "unusual solutions",
];

pub struct IncubationRunner {
    agent_registry: OnceLock<Arc<AgentRegistry>>,
    incubation_config: OnceLock<IncubationConfig>,
    global_bus: OnceLock<Arc<dyn EventBus>>,
    active_runs: RwLock<HashSet<String>>,
}

impl IncubationRunner {
    pub fn new() -> Self {
        Self {
            agent_registry: OnceLock::new(),
            incubation_config: OnceLock::new(),
            global_bus: OnceLock::new(),
            active_runs: RwLock::new(HashSet::new()),
        }
    }

    pub fn set_agent_registry(&self, registry: Arc<AgentRegistry>) {
        let _ = self.agent_registry.set(registry);
    }

    /// Look up the per-agent memory provider from the registry.
    async fn memory_for(&self, agent_id: &str) -> Option<Arc<dyn MemoryProvider>> {
        self.agent_registry.get()?.get_memory_provider(agent_id).await
    }

    pub fn set_incubation_config(&self, config: IncubationConfig) {
        let _ = self.incubation_config.set(config);
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
        let cooldown_secs = self
            .incubation_config
            .get()
            .map(|c| c.cooldown_secs)
            .unwrap_or(10800);
        coord
            .set_kind_cooldown(IdleKind::Incubation, cooldown_secs)
            .await;
        debug!(
            agent_id,
            cooldown_secs,
            "Incubation: cooldown set",
        );
    }

    async fn run_phases(&self, agent_id: &str) -> AmanResult<()> {
        let started = Instant::now();

        let Some(provider) = self.memory_for(agent_id).await else {
            debug!(agent_id, "IncubationRunner: no MemoryProvider");
            return Ok(());
        };

        // ── Phase 1: 跨域记忆采样 ──────────────────────────────────────
        let mut all_records: Vec<MemoryRecord> = Vec::new();
        for query in RECALL_QUERIES {
            let batch = provider.recall(agent_id, query, RECALL_LIMIT).await;
            all_records.extend(batch);
        }
        // Deduplicate by rid
        let mut seen = HashSet::new();
        all_records.retain(|r| seen.insert(r.rid.clone()));

        // Group by domain (default to "general" if unset)
        let mut by_domain: HashMap<String, Vec<&MemoryRecord>> = HashMap::new();
        for r in &all_records {
            let domain = r.domain.clone().unwrap_or_else(|| "general".to_owned());
            by_domain.entry(domain).or_default().push(r);
        }

        if by_domain.len() < 2 {
            debug!(
                agent_id,
                domains = by_domain.len(),
                total = all_records.len(),
                "Incubation: not enough domains for cross-domain sampling",
            );
            self.run_think_and_finish(agent_id, &*provider, started).await?;
            return Ok(());
        }

        let domains: Vec<&String> = by_domain.keys().collect();
        info!(
            agent_id,
            domains = domains.len(),
            records = all_records.len(),
            "Incubation Phase 1: cross-domain sampling complete",
        );

        // ── Phase 2: 跨域联想 ──────────────────────────────────────────
        let mut hypotheses: Vec<Hypothesis> = Vec::new();
        for i in 0..domains.len() {
            for j in (i + 1)..domains.len() {
                let dom_a = domains[i];
                let dom_b = domains[j];
                let mems_a = &by_domain[dom_a];
                let mems_b = &by_domain[dom_b];

                // Sample up to 3 memories from each domain to generate hypotheses
                for (ai, mem_a) in mems_a.iter().take(3).enumerate() {
                    for (bi, mem_b) in mems_b.iter().take(3).enumerate() {
                        // Extract entity names from content via search_entities
                        let entities_a =
                            self.extract_entities(&*provider, &mem_a.content).await;
                        let entities_b =
                            self.extract_entities(&*provider, &mem_b.content).await;

                        // Cross-domain entity discovery
                        let mut shared_entities: Vec<String> = Vec::new();
                        for ea in &entities_a {
                            if entities_b.contains(ea) {
                                shared_entities.push(ea.clone());
                            }
                        }

                        // Search procedural memory for analogies
                        let analogy_context =
                            format!("{} vs {}", &mem_a.content, &mem_b.content);
                        let analogies = provider
                            .surface_procedural(agent_id, &analogy_context, 3)
                            .await
                            .unwrap_or_default();

                        // Generate hypothesis strings
                        let domain_a = mem_a.domain.clone().unwrap_or_else(|| "general".to_owned());
                        let domain_b = mem_b.domain.clone().unwrap_or_else(|| "general".to_owned());
                        let snippet_a = Self::truncate_content(&mem_a.content, 120);
                        let snippet_b = Self::truncate_content(&mem_b.content, 120);

                        let h1 = format!(
                            "Could the pattern from {domain_a} ({snippet_a}) apply to {domain_b}?"
                        );
                        let h2 = format!(
                            "What if the approach from {domain_b} ({snippet_b}) were used for {domain_a}?"
                        );

                        for (hi, h) in [h1, h2].into_iter().enumerate() {
                            hypotheses.push(Hypothesis {
                                text: h,
                                domain_a: domain_a.clone(),
                                domain_b: domain_b.clone(),
                                shared_entity_count: shared_entities.len(),
                                analogy_count: analogies.len(),
                                pair_index: (ai * 3 + bi) * 2 + hi,
                                mem_a_rid: mem_a.rid.clone(),
                                mem_b_rid: mem_b.rid.clone(),
                            });
                        }
                    }
                }
            }
        }

        info!(
            agent_id,
            hypotheses = hypotheses.len(),
            "Incubation Phase 2: cross-domain association complete",
        );

        // ── Phase 3: 灵感评分 ──────────────────────────────────────────
        let incubation_threshold = self
            .incubation_config
            .get()
            .map(|c| c.incubation_threshold)
            .unwrap_or(0.7);
        let high_value_threshold = self
            .incubation_config
            .get()
            .map(|c| c.high_value_threshold)
            .unwrap_or(0.85);

        let existing_inspirations =
            provider.recall(agent_id, "inspiration incubation", 50).await;

        let mut stored_count = 0;
        let mut high_value_count = 0;

        for hyp in &hypotheses {
            let novelty = self.estimate_novelty(hyp, &existing_inspirations);
            let feasibility = self.estimate_feasibility(hyp);
            let score = novelty * 0.6 + feasibility * 0.4;

            if score >= incubation_threshold {
                let tags = if score >= high_value_threshold {
                    high_value_count += 1;
                    vec![
                        "inspiration".to_owned(),
                        "incubation".to_owned(),
                        "high_value".to_owned(),
                    ]
                } else {
                    vec!["inspiration".to_owned(), "incubation".to_owned()]
                };

                let content = format!(
                    "[Inspiration] {text} (score={score:.2}, novelty={novelty:.2}, feasibility={feasibility:.2})",
                    text = hyp.text,
                );
                let stored_rid = provider.store(agent_id, &content, tags);

                // Create KG cross-domain edge if entities are identified
                let _ = provider
                    .relate(&hyp.mem_a_rid, &hyp.mem_b_rid, "cross_domain_inspiration")
                    .await;

                // Publish high-value inspirations as events
                if score >= high_value_threshold {
                    if let Some(bus) = self.global_bus.get() {
                        let event = Event::new(
                            format!("idle:incubation:{agent_id}:{stored_rid}"),
                            EventType::Custom("idle.inspiration".to_owned()),
                            serde_json::json!({
                                "agentId": agent_id,
                                "score": score,
                                "novelty": novelty,
                                "feasibility": feasibility,
                                "hypothesis": hyp.text,
                                "domainA": hyp.domain_a,
                                "domainB": hyp.domain_b,
                            }),
                        );
                        let _ = bus.publish(event).await;
                    }
                }

                stored_count += 1;
            }
        }

        info!(
            agent_id,
            stored_count,
            high_value_count,
            total_hypotheses = hypotheses.len(),
            "Incubation Phase 3: inspiration scoring complete",
        );

        // ── Phase 4: 种子演进 ──────────────────────────────────────────
        let seeds = provider
            .recall(agent_id, "inspiration incubation", SEED_EVOLUTION_LIMIT)
            .await;
        let mut evolved_count = 0;

        for seed in &seeds {
            for variant_i in 1..=2 {
                let variant = format!(
                    "[Seed Evolution v{variant_i}] Building on: \"{seed_content}\" — \
                     what if this were extended to a different context or reversed?",
                    seed_content = Self::truncate_content(&seed.content, 100),
                );

                // Simple heuristic scoring for variants
                let v_novelty = 0.8; // variant is inherently novel relative to parent
                let v_feasibility = 0.5; // conservative baseline
                let v_score = v_novelty * 0.6 + v_feasibility * 0.4;

                if v_score >= incubation_threshold {
                    provider.store(
                        agent_id,
                        &variant,
                        vec![
                            "inspiration".to_owned(),
                            "seed_evolution".to_owned(),
                            seed.rid.clone(),
                        ],
                    );
                    evolved_count += 1;
                }
            }
        }

        if evolved_count > 0 {
            info!(
                agent_id,
                evolved_count,
                seeds_loaded = seeds.len(),
                "Incubation Phase 4: seed evolution complete",
            );
        }

        // ── Phase 5: 认知循环 (think, 轻量) ────────────────────────────
        // 仅触发 relationship_insight 和 entity_anomaly trigger，
        // 不合并（保留多样性）、不扫描冲突
        self.run_think_and_finish(agent_id, &*provider, started).await?;

        // Signal depth reset if we produced inspirations
        let total_stored = stored_count + evolved_count;
        if total_stored > 0 {
            if let Some(registry) = self.agent_registry.get() {
                if let Some(coord) = registry.get_idle_coordination(agent_id).await {
                    coord.pending_depth_reset.store(true, Ordering::SeqCst);
                }
            }
        }

        self.signal_cooldown(agent_id).await;
        Ok(())
    }

    /// Phase 5 extraction: run the lightweight think pass and publish
    /// completion event.
    async fn run_think_and_finish(
        &self,
        agent_id: &str,
        provider: &dyn MemoryProvider,
        started: Instant,
    ) -> AmanResult<()> {
        let config = ThinkConfig {
            importance_threshold: 0.3,
            run_consolidation: false,
            run_conflict_scan: false,
        };

        let result = provider.think(agent_id, &config).await?;
        let stored = result.triggers_fired + result.consolidation_count;
        // NOTE: YantrikdbProvider::think() 桥接层当前 fire-and-forget，
        //       ThinkResult 永远返回全零。桥接完成后 stored 才会 > 0。

        let elapsed = started.elapsed();
        info!(
            agent_id,
            stored,
            duration_ms = elapsed.as_millis(),
            "Incubation: cycle complete",
        );

        if stored > 0 {
            if let Some(bus) = self.global_bus.get() {
                let event = Event::new(
                    format!("idle:incubation:{agent_id}"),
                    EventType::Custom("idle.cycle_completed".to_owned()),
                    serde_json::json!({
                        "kind": "incubation",
                        "agentId": agent_id,
                        "stored": stored,
                        "durationMs": elapsed.as_millis(),
                    }),
                );
                let _ = bus.publish(event).await;
            }
        }

        Ok(())
    }

    /// Extract known entity names from memory content via search_entities.
    async fn extract_entities(
        &self,
        provider: &dyn MemoryProvider,
        content: &str,
    ) -> Vec<String> {
        // Use the first few meaningful words as entity search queries
        let words: Vec<&str> = content
            .split_whitespace()
            .filter(|w| w.len() > 3)
            .take(10)
            .collect();
        let mut entities = Vec::new();
        for word in words {
            if let Ok(results) = provider.search_entities(word, 5).await {
                entities.extend(results);
            }
        }
        entities.sort();
        entities.dedup();
        entities.truncate(20);
        entities
    }

    /// Estimate novelty: how different the hypothesis is from existing
    /// inspirations. Simple bag-of-words overlap heuristic.
    fn estimate_novelty(
        &self,
        hyp: &Hypothesis,
        existing: &[MemoryRecord],
    ) -> f64 {
        if existing.is_empty() {
            return 0.9; // no existing inspirations → highly novel
        }
        let hyp_words: HashSet<&str> = hyp.text.split_whitespace().collect();
        let mut max_overlap = 0.0;
        for rec in existing {
            let rec_words: HashSet<&str> =
                rec.content.split_whitespace().collect();
            let union = hyp_words.union(&rec_words).count() as f64;
            if union > 0.0 {
                let intersection =
                    hyp_words.intersection(&rec_words).count() as f64;
                let overlap = intersection / union;
                if overlap > max_overlap {
                    max_overlap = overlap;
                }
            }
        }
        1.0 - max_overlap
    }

    /// Estimate feasibility: based on shared entities (suggests structural
    /// connection) and existing analogies in procedural memory.
    fn estimate_feasibility(&self, hyp: &Hypothesis) -> f64 {
        let entity_score = (hyp.shared_entity_count as f64 * 0.15).min(0.5);
        let analogy_score = (hyp.analogy_count as f64 * 0.15).min(0.5);
        entity_score + analogy_score
    }

    fn truncate_content(content: &str, max_len: usize) -> String {
        if content.len() <= max_len {
            content.to_owned()
        } else {
            let boundary = content[..max_len]
                .rfind(' ')
                .unwrap_or(max_len);
            format!("{}...", &content[..boundary])
        }
    }
}

/// A generated cross-domain hypothesis.
struct Hypothesis {
    text: String,
    domain_a: String,
    domain_b: String,
    shared_entity_count: usize,
    analogy_count: usize,
    #[allow(dead_code)]
    pair_index: usize,
    mem_a_rid: String,
    mem_b_rid: String,
}

impl Default for IncubationRunner {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl EventHandler for IncubationRunner {
    async fn handle(&self, event: Event) -> AmanResult<()> {
        if event.event_type != EventType::Idle {
            return Ok(());
        }
        let Some(kind) = event.payload.get("kind").and_then(|v| v.as_str()) else {
            return Ok(());
        };
        if kind != "incubation" {
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
            debug!(agent_id, "IncubationRunner: already running, skipping");
            return Ok(());
        }

        let result = self.run_phases(agent_id).await;
        self.release(agent_id);
        result
    }
}
