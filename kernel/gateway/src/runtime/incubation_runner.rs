// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! Incubation runner — triggered by IdleEvent{kind="incubation"} when the
//! agent reaches idle depth 200+. Runs the deepest idle cycle: creative
//! synthesis, long-term memory trends, and emergent insight generation.
//!
//! Architecture: the EventHandler spawns a background task via
//! [`IncubationManager`](idle::IncubationManager) and returns immediately.
//! The manager enforces max_concurrent=1. This matches the idle-patch.md
//! design: Pipeline triggers → spawn → return (<1ms).

use async_trait::async_trait;
use config::{IncubationConfig, MemoryLlmConfig};
use event_bus::{try_publish, EventHandler, EventBus};
use idle::IdleKind;
use kernel::event::{Event, EventType};
use kernel::llm::{LlmChatRequest, LlmProvider, ResponseFormat};
use kernel::memory::{MemoryProvider, MemoryRecord, ThinkConfig};
use kernel::react::ChatMessage;
use kernel::AmanResult;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::Ordering;
use std::sync::{Arc, OnceLock};
use std::time::Instant;
use cognitive_llm::simple::parse_json_response;
use tracing::{debug, info, warn};

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
    memory_llm: OnceLock<MemoryLlmConfig>,
    self_bridge: OnceLock<super::self_bridge::SelfBridge>,
}

impl IncubationRunner {
    pub fn new() -> Self {
        Self {
            agent_registry: OnceLock::new(),
            incubation_config: OnceLock::new(),
            global_bus: OnceLock::new(),
            memory_llm: OnceLock::new(),
            self_bridge: OnceLock::new(),
        }
    }

    pub fn set_agent_registry(&self, registry: Arc<AgentRegistry>) {
        let _ = self.agent_registry.set(registry);
    }

    pub fn set_incubation_config(&self, config: IncubationConfig) {
        let _ = self.incubation_config.set(config);
    }

    pub fn set_global_bus(&self, bus: Arc<dyn EventBus>) {
        let _ = self.global_bus.set(bus);
    }

    pub fn set_memory_llm(&self, config: MemoryLlmConfig) {
        let _ = self.memory_llm.set(config);
    }

    pub fn set_self_bridge(&self, bridge: super::self_bridge::SelfBridge) {
        let _ = self.self_bridge.set(bridge);
    }
}

impl Default for IncubationRunner {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl EventHandler for IncubationRunner {
    async fn handle(&self, event: Event) -> AmanResult<()> {
        // Filter: only Idle events with kind == "incubation"
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

        // Skip if in chat mode — deep creative synthesis doesn't fit
        // conversational contexts.
        if event
            .payload
            .get("from_chat_mode")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
        {
            debug!(agent_id, "Incubation: skipped (chat mode)");
            return Ok(());
        }

        // Get the per-agent IncubationManager for concurrency gating.
        let Some(registry) = self.agent_registry.get() else {
            return Ok(());
        };
        let Some(idle_manager) = registry.get_idle_manager(agent_id).await else {
            debug!(agent_id, "Incubation: no AgentIdleManager");
            return Ok(());
        };
        let incubation_manager = idle_manager.incubation().clone();

        // Clone Arcs for the background task. IdleEvent handler returns
        // immediately after spawning.
        let agent_id_owned = agent_id.to_owned();
        let config = self.incubation_config.get().cloned().unwrap_or_default();
        let memory_llm_cfg = self.memory_llm.get().cloned();
        let registry_clone = Arc::clone(registry);
        let bus = self.global_bus.get().cloned();

        // Get entity extraction prompt from Python bridge (no hardcoded Rust prompts).
        let extraction_prompt = self
            .self_bridge
            .get()
            .and_then(|b: &super::self_bridge::SelfBridge| b.entity_extraction_prompt())
            .unwrap_or_default();

        let spawned = incubation_manager
            .spawn(
                format!("incubation:{agent_id_owned}"),
                async move {
                    let _ = run_phases(
                        &agent_id_owned,
                        &config,
                        memory_llm_cfg.as_ref(),
                        &registry_clone,
                        bus.as_deref(),
                        &extraction_prompt,
                    )
                    .await;
                },
            )
            .await;

        if spawned.is_some() {
            debug!(agent_id, "Incubation: background task spawned");
        } else {
            debug!(agent_id, "Incubation: already running, skipping");
        }

        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Phase logic (runs in the background task)
// ---------------------------------------------------------------------------

async fn run_phases(
    agent_id: &str,
    config: &IncubationConfig,
    memory_llm: Option<&MemoryLlmConfig>,
    registry: &AgentRegistry,
    global_bus: Option<&dyn EventBus>,
    extraction_prompt: &str,
) -> AmanResult<()> {
    let started = Instant::now();

    let Some(provider) = registry.get_memory_provider(agent_id).await else {
        debug!(agent_id, "Incubation: no MemoryProvider");
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
        run_think_and_finish(agent_id, &*provider, started, global_bus).await?;
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
    //
    // Pre-collect all unique content strings and batch-extract entities
    // via LLM (or keyword fallback) in one call instead of calling per-pair.
    let llm = registry.get_llm_provider(agent_id).await;
    let mut entities_by_content: HashMap<&str, Vec<String>> = HashMap::new();

    {
        let mut unique_contents: Vec<&str> = Vec::new();
        let mut content_index: HashMap<&str, usize> = HashMap::new();

        for i in 0..domains.len() {
            for j in (i + 1)..domains.len() {
                let mems_a = &by_domain[domains[i]];
                let mems_b = &by_domain[domains[j]];
                for mem_a in mems_a.iter().take(3) {
                    for mem_b in mems_b.iter().take(3) {
                        for content in [mem_a.content.as_str(), mem_b.content.as_str()] {
                            if !content_index.contains_key(content) {
                                content_index.insert(content, unique_contents.len());
                                unique_contents.push(content);
                            }
                        }
                    }
                }
            }
        }

        let entities_batch: Vec<Vec<String>> = if let Some(ref llm) = llm {
            extract_entities_batch(
                llm,
                memory_llm,
                &unique_contents,
                &*provider,
                extraction_prompt,
            )
            .await
        } else {
            debug!(agent_id, "Incubation: no LLM provider, using keyword entity extraction");
            fallback_extract_entities(&*provider, &unique_contents).await
        };

        for (content, idx) in content_index {
            entities_by_content.insert(content, entities_batch[idx].clone());
        }
    }

    let mut hypotheses: Vec<Hypothesis> = Vec::new();
    for i in 0..domains.len() {
        for j in (i + 1)..domains.len() {
            let dom_a = domains[i];
            let dom_b = domains[j];
            let mems_a = &by_domain[dom_a];
            let mems_b = &by_domain[dom_b];

            for (ai, mem_a) in mems_a.iter().take(3).enumerate() {
                for (bi, mem_b) in mems_b.iter().take(3).enumerate() {
                    let entities_a = entities_by_content
                        .get(mem_a.content.as_str())
                        .cloned()
                        .unwrap_or_default();
                    let entities_b = entities_by_content
                        .get(mem_b.content.as_str())
                        .cloned()
                        .unwrap_or_default();

                    let mut shared_entities: Vec<String> = Vec::new();
                    for ea in &entities_a {
                        if entities_b.contains(ea) {
                            shared_entities.push(ea.clone());
                        }
                    }

                    let analogy_context =
                        format!("{} vs {}", &mem_a.content, &mem_b.content);
                    let analogies = provider
                        .surface_procedural(agent_id, &analogy_context, 3)
                        .await
                        .unwrap_or_default();

                    let domain_a = mem_a.domain.clone().unwrap_or_else(|| "general".to_owned());
                    let domain_b = mem_b.domain.clone().unwrap_or_else(|| "general".to_owned());
                    let snippet_a = truncate_content(&mem_a.content, 120);
                    let snippet_b = truncate_content(&mem_b.content, 120);

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
    let incubation_threshold = config.incubation_threshold;
    let high_value_threshold = config.high_value_threshold;

    let existing_inspirations =
        provider.recall(agent_id, "inspiration incubation", 50).await;

    let mut stored_count = 0;
    let mut high_value_count = 0;

    for hyp in &hypotheses {
        let novelty = estimate_novelty(hyp, &existing_inspirations);
        let feasibility = estimate_feasibility(hyp);
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

            let _ = provider
                .relate(&hyp.mem_a_rid, &hyp.mem_b_rid, "cross_domain_inspiration")
                .await;

            if score >= high_value_threshold
                && let Some(bus) = global_bus {
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
                    try_publish(bus, event).await;
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
                seed_content = truncate_content(&seed.content, 100),
            );

            let v_novelty = 0.8;
            let v_feasibility = 0.5;
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
    run_think_and_finish(agent_id, &*provider, started, global_bus).await?;

    // Signal depth reset if we produced inspirations
    let total_stored = stored_count + evolved_count;
    if total_stored > 0
        && let Some(coord) = registry.get_idle_coordination(agent_id).await {
            coord.pending_depth_reset.store(true, Ordering::SeqCst);
        }

    signal_cooldown(agent_id, config, registry).await;
    Ok(())
}

/// Phase 5: run the lightweight think pass and publish completion event.
async fn run_think_and_finish(
    agent_id: &str,
    provider: &dyn MemoryProvider,
    started: Instant,
    global_bus: Option<&dyn EventBus>,
) -> AmanResult<()> {
    let config = ThinkConfig {
        importance_threshold: 0.3,
        run_consolidation: false,
        run_conflict_scan: false,
    };

    let result = provider.think(agent_id, &config).await?;
    let stored = result.triggers_fired + result.consolidation_count;

    let elapsed = started.elapsed();
    info!(
        agent_id,
        stored,
        duration_ms = elapsed.as_millis(),
        "Incubation: cycle complete",
    );

    if stored > 0
        && let Some(bus) = global_bus {
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
            try_publish(bus, event).await;
        }

    Ok(())
}

async fn signal_cooldown(
    agent_id: &str,
    config: &IncubationConfig,
    registry: &AgentRegistry,
) {
    let Some(coord) = registry.get_idle_coordination(agent_id).await else {
        return;
    };
    coord
        .set_kind_cooldown(IdleKind::Incubation, config.cooldown_secs)
        .await;
    debug!(agent_id, cooldown_secs = config.cooldown_secs, "Incubation: cooldown set");
}

/// Extract named entities from a batch of content strings using the LLM.
///
/// Makes a single LLM call with all content strings and returns one entity
/// list per input. Falls back to keyword-based extraction if the LLM call
/// fails or returns unparseable output.
async fn extract_entities_batch(
    llm: &Arc<dyn LlmProvider>,
    memory_llm: Option<&MemoryLlmConfig>,
    contents: &[&str],
    provider: &dyn MemoryProvider,
    system_prompt: &str,
) -> Vec<Vec<String>> {
    if contents.is_empty() {
        return Vec::new();
    }

    // Build a numbered content list for the LLM
    let mut numbered = String::new();
    for (i, c) in contents.iter().enumerate() {
        numbered.push_str(&format!("--- Content {} ---\n{}\n\n", i + 1, c));
    }

    let model = memory_llm
        .map(|c| c.model.as_str())
        .unwrap_or("default");

    // JSON Schema for structured output: array of arrays of strings,
    // one inner array per content block, in order.
    let entity_schema = serde_json::json!({
        "type": "object",
        "properties": {
            "entities": {
                "type": "array",
                "items": {
                    "type": "array",
                    "items": { "type": "string" }
                }
            }
        },
        "required": ["entities"],
        "additionalProperties": false
    });

    let req = LlmChatRequest {
        model: model.to_owned(),
        system_prompt: system_prompt.to_owned(),
        messages: vec![ChatMessage::user(numbered)],
        tools: Vec::new(),
        max_output_tokens: 2048,
        response_format: Some(ResponseFormat::JsonSchema {
            name: "entity_extraction".to_owned(),
            schema: entity_schema,
            strict: true,
        }),
    };

    let resp = match llm.chat_completion(req, None).await {
        Ok(r) => r,
        Err(e) => {
            warn!("Incubation: LLM entity extraction failed, falling back to keyword search: {e}");
            return fallback_extract_entities(provider, contents).await;
        }
    };

    // Parse JSON response (robust: handles edge cases even with structured output)
    let parsed: serde_json::Value = match parse_json_response(&resp.content) {
        Ok(v) => v,
        Err(e) => {
            warn!("Incubation: LLM returned unparseable JSON, falling back to keyword search: {e}");
            return fallback_extract_entities(provider, contents).await;
        }
    };

    // Extract entities by position: parsed.entities[i] → Vec<String>
    let entity_arrays = parsed
        .get("entities")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .map(|inner| {
                    inner
                        .as_array()
                        .map(|a| {
                            a.iter()
                                .filter_map(|v| v.as_str().map(String::from))
                                .collect::<Vec<String>>()
                        })
                        .unwrap_or_default()
                })
                .collect::<Vec<Vec<String>>>()
        })
        .unwrap_or_default();

    // Pad to match content count if LLM returned fewer arrays
    let mut results = entity_arrays;
    results.resize_with(contents.len(), Vec::new);
    results
}

/// Fallback: keyword-based entity extraction, adapted for batch input.
async fn fallback_extract_entities(
    provider: &dyn MemoryProvider,
    contents: &[&str],
) -> Vec<Vec<String>> {
    let mut results = Vec::with_capacity(contents.len());
    for content in contents {
        let words: Vec<&str> = content
            .split_whitespace()
            .filter(|w| w.len() > 3)
            .take(10)
            .collect();
        let mut entities = Vec::new();
        for word in words {
            if let Ok(e) = provider.search_entities(word, 5).await {
                entities.extend(e);
            }
        }
        entities.sort();
        entities.dedup();
        entities.truncate(20);
        results.push(entities);
    }
    results
}

/// Estimate novelty: how different the hypothesis is from existing
/// inspirations. Simple bag-of-words overlap heuristic.
fn estimate_novelty(hyp: &Hypothesis, existing: &[MemoryRecord]) -> f64 {
    if existing.is_empty() {
        return 0.9;
    }
    let hyp_words: HashSet<&str> = hyp.text.split_whitespace().collect();
    let mut max_overlap = 0.0;
    for rec in existing {
        let rec_words: HashSet<&str> = rec.content.split_whitespace().collect();
        let union = hyp_words.union(&rec_words).count() as f64;
        if union > 0.0 {
            let intersection = hyp_words.intersection(&rec_words).count() as f64;
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
fn estimate_feasibility(hyp: &Hypothesis) -> f64 {
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

// ---------------------------------------------------------------------------
// Supporting types
// ---------------------------------------------------------------------------

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
