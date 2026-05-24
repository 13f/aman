// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! Exploration runner — triggered by IdleEvent{kind="exploration"} when the
//! agent reaches idle depth 50+. Generates queries from memory/knowledge gaps,
//! searches external sources via info-hub adapters, and stores discoveries.
//!
//! Architecture ref: idle-patch.md §5
//!
//! Follows the same dependency-injection pattern as [`SleepRunner`] and
//! [`ReflectionRunner`]: OnceLock fields populated during build, subscribes
//! to idle events on the global bus.

use async_trait::async_trait;
use config::ExplorationConfig;
use event_bus::EventHandler;
use idle::IdleKind;
use info_hub::adapters;
use info_hub::config::InfoHubConfig;
use info_hub::merge;
use info_hub::types::{InfoItem, InfoSearchInput};
use kernel::event::{Event, EventType};
use kernel::memory::MemoryProvider;
use kernel::AmanResult;
use rand::seq::SliceRandom;
use std::collections::HashSet;
use std::sync::{Arc, OnceLock, RwLock};
use std::time::{Duration, Instant};
use tokio::time::sleep;
use tracing::{debug, info, warn};

use super::agent_registry::AgentRegistry;

// ---------------------------------------------------------------------------
// ExplorationRunner
// ---------------------------------------------------------------------------

/// Handles IdleEvent{kind="exploration"} → external information discovery.
pub struct ExplorationRunner {
    agent_registry: OnceLock<Arc<AgentRegistry>>,
    memory_provider: OnceLock<Arc<dyn MemoryProvider>>,
    info_hub_config: OnceLock<InfoHubConfig>,
    exploration_config: OnceLock<ExplorationConfig>,
    active_runs: RwLock<HashSet<String>>,
}

impl ExplorationRunner {
    pub fn new() -> Self {
        Self {
            agent_registry: OnceLock::new(),
            memory_provider: OnceLock::new(),
            info_hub_config: OnceLock::new(),
            exploration_config: OnceLock::new(),
            active_runs: RwLock::new(HashSet::new()),
        }
    }

    pub fn set_agent_registry(&self, registry: Arc<AgentRegistry>) {
        let _ = self.agent_registry.set(registry);
    }

    pub fn set_memory_provider(&self, provider: Arc<dyn MemoryProvider>) {
        let _ = self.memory_provider.set(provider);
    }

    pub fn set_info_hub_config(&self, config: InfoHubConfig) {
        let _ = self.info_hub_config.set(config);
    }

    pub fn set_exploration_config(&self, config: ExplorationConfig) {
        let _ = self.exploration_config.set(config);
    }

    // -- guard helpers -------------------------------------------------------

    fn try_acquire(&self, agent_id: &str) -> bool {
        self.active_runs
            .write()
            .unwrap()
            .insert(agent_id.to_owned())
    }

    fn release(&self, agent_id: &str) {
        self.active_runs.write().unwrap().remove(agent_id);
    }

    // -- phase 1: query generation ------------------------------------------

    /// Generate search queries from memory gaps and knowledge graph orphan entities.
    async fn generate_queries(&self, agent_id: &str) -> Vec<String> {
        let mut queries: Vec<String> = Vec::new();

        let Some(provider) = self.memory_provider.get() else {
            debug!("ExplorationRunner: no MemoryProvider, skipping query generation");
            return queries;
        };

        // 1a. Stale but important memories → queries about latest info
        if let Ok(stale) = provider.stale_memories(agent_id, 7).await {
            for mem in &stale {
                if mem.importance.unwrap_or(0.0) > 0.4 {
                    let content = mem.content.lines().next().unwrap_or(&mem.content);
                    let snippet: String = content.chars().take(120).collect();
                    queries.push(format!("latest information about: {snippet}"));
                }
                if queries.len() >= 15 {
                    break;
                }
            }
        }

        // 1b. Orphan entities in knowledge graph → "what is X"
        if let Ok(entities) = provider.search_entities("*", 10).await {
            for entity in &entities {
                if let Ok(Some(profile)) = provider.entity_profile(entity).await {
                    if profile.edge_count == 0 {
                        queries.push(format!("what is {entity} and how does it relate to other things?"));
                    }
                }
            }
        }

        // 1c. Dedup and truncate
        let mut seen = HashSet::new();
        queries.retain(|q| seen.insert(q.to_lowercase()));
        let max = 30usize;
        queries.truncate(max);

        // 1d. Cold-start fallback: if no queries from memories/entities,
        //     pick random primary interests from INTERESTS.md
        if queries.is_empty() {
            let interests = self.load_primary_interests(agent_id);
            if !interests.is_empty() {
                let mut rng = rand::thread_rng();
                let n = 3.min(interests.len());
                let picks: Vec<_> =
                    interests.choose_multiple(&mut rng, n).cloned().collect();
                for interest in &picks {
                    queries.push(format!(
                        "latest developments about {interest}"
                    ));
                }
            }
        }

        debug!(
            agent_id,
            count = queries.len(),
            "Exploration: generated queries from memory gaps",
        );

        queries
    }

    /// Parse primary interests from an agent's INTERESTS.md.
    fn load_primary_interests(&self, agent_id: &str) -> Vec<String> {
        let path = super::agent_seed::aman_data_dir()
            .join("agents")
            .join(agent_id)
            .join("INTERESTS.md");
        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(_) => return Vec::new(),
        };
        let mut in_primary = false;
        let mut interests = Vec::new();
        for line in content.lines() {
            let t = line.trim();
            if t.starts_with("## ") {
                in_primary =
                    t.contains("核心兴趣") || t.to_lowercase().contains("primary");
                continue;
            }
            if in_primary && t.starts_with("- **") {
                if let Some(end) = t[4..].find("**") {
                    let name = t[4..4 + end].trim().to_string();
                    if !name.is_empty() {
                        interests.push(name);
                    }
                }
            }
        }
        interests
    }

    // -- phase 2: execute searches ------------------------------------------

    /// Run external searches via info-hub adapters with rate limiting.
    async fn execute_searches(
        &self,
        queries: &[String],
        cancel: &tokio_util::sync::CancellationToken,
    ) -> Vec<InfoItem> {
        let Some(info_cfg) = self.info_hub_config.get() else {
            debug!("ExplorationRunner: no InfoHubConfig, skipping searches");
            return Vec::new();
        };

        if info_cfg.sources.is_empty() {
            debug!("ExplorationRunner: no info-hub sources configured");
            return Vec::new();
        }

        let _cfg = self.exploration_config.get();
        let rate = _cfg.map(|c| c.api_rate_per_minute).unwrap_or(10).max(1);
        let delay_ms = 60_000 / rate as u64;

        let mut all_items: Vec<InfoItem> = Vec::new();
        let sources = info_cfg.filter_sources(&[]);
        let timeout_ms = info_cfg.timeout_ms;

        for (i, query) in queries.iter().enumerate() {
            if cancel.is_cancelled() {
                debug!("Exploration: cancelled during search phase");
                break;
            }

            if i > 0 {
                sleep(Duration::from_millis(delay_ms)).await;
            }

            let input = InfoSearchInput {
                query: query.clone(),
                limit: 5,
                offset: 0,
                sources: None,
            };

            for source in &sources {
                if cancel.is_cancelled() {
                    break;
                }
                let adapter = adapters::build_adapter(source, timeout_ms);
                match adapter.search(&input).await {
                    Ok(items) => all_items.extend(items),
                    Err(e) => warn!(source = source.name(), error = %e, "Exploration: adapter search failed"),
                }
            }
        }

        merge::merge(all_items, 20)
    }

    // -- phase 3: score & store results -------------------------------------

    /// Score results with a simple heuristic (keyword overlap + freshness),
    /// store high-value discoveries to MemoryProvider.
    async fn process_results(
        &self,
        agent_id: &str,
        results: &[InfoItem],
    ) -> usize {
        let Some(provider) = self.memory_provider.get() else {
            return 0;
        };

        let _cfg = self.exploration_config.get();
        let min_score: f64 = 0.3; // reasonable default

        let mut stored = 0usize;
        for item in results {
            // Simple heuristic: title length as proxy for substance,
            // presence of URL as signal of actual content
            let has_url = !item.url.is_empty();
            let title_len = item.title.len().min(80) as f64 / 80.0;
            let summary_len = item.summary.len().min(200) as f64 / 200.0;
            let has_date = item.published.is_some();
            let score = title_len * 0.3 + summary_len * 0.3 + (if has_url { 0.2 } else { 0.0 }) + (if has_date { 0.2 } else { 0.0 });

            if score < min_score {
                continue;
            }

            let content = format!(
                "[Exploration] {}\nURL: {}\n{}\nSource: {} | Published: {}",
                item.title,
                item.url,
                item.summary,
                item.source,
                item.published.as_deref().unwrap_or("unknown"),
            );

            let mut tags = vec!["exploration".to_string(), item.source.clone()];
            if score > 0.7 {
                tags.push("high_value".to_string());
            }

            provider.store(agent_id, &content, tags);
            stored += 1;
        }

        if stored > 0 {
            info!(agent_id, stored, "Exploration: stored discoveries to memory");
        }

        stored
    }

    // -- phase 4: fallback — local semantic search --------------------------

    /// When external sources are unavailable, fall back to local memory search.
    async fn local_fallback(&self, agent_id: &str) {
        let Some(provider) = self.memory_provider.get() else {
            return;
        };

        let results = provider
            .recall(agent_id, "interesting developments new information", 10)
            .await;

        if !results.is_empty() {
            debug!(
                agent_id,
                count = results.len(),
                "Exploration: local fallback — recalled existing memories",
            );
        }
    }

    // -- cooldown helper ----------------------------------------------------

    async fn signal_cooldown(&self, agent_id: &str) {
        let Some(registry) = self.agent_registry.get() else {
            return;
        };
        let Some(coord) = registry.get_idle_coordination(agent_id).await else {
            return;
        };
        let cooldown_secs = self
            .exploration_config
            .get()
            .map(|c| c.cooldown_secs)
            .unwrap_or(3600);
        coord
            .set_kind_cooldown(IdleKind::Exploration, cooldown_secs)
            .await;
        debug!(
            agent_id,
            cooldown_secs,
            "Exploration: cooldown set",
        );
    }

    // -- orchestration -------------------------------------------------------

    async fn run_phases(&self, agent_id: &str) -> AmanResult<()> {
        let cancel_token = {
            let Some(registry) = self.agent_registry.get() else {
                debug!("ExplorationRunner: no AgentRegistry, skipping");
                return Ok(());
            };
            let Some(coord) = registry.get_idle_coordination(agent_id).await else {
                debug!(agent_id, "ExplorationRunner: no idle coordination, skipping");
                return Ok(());
            };
            coord.idle_cancel_token.read().await.clone()
        };

        let started = Instant::now();

        // Phase 1: Generate queries from memory gaps
        if cancel_token.is_cancelled() {
            self.signal_cooldown(agent_id).await;
            return Ok(());
        }
        let queries = self.generate_queries(agent_id).await;

        if queries.is_empty() {
            debug!(agent_id, "Exploration: no queries generated, falling back to local");
            self.local_fallback(agent_id).await;
            self.signal_cooldown(agent_id).await;
            return Ok(());
        }

        // Phase 2: Execute external searches
        let results = self.execute_searches(&queries, &cancel_token).await;

        if results.is_empty() {
            debug!(agent_id, "Exploration: no external results, falling back to local");
            self.local_fallback(agent_id).await;
            self.signal_cooldown(agent_id).await;
            return Ok(());
        }

        // Phase 3: Score & store
        if cancel_token.is_cancelled() {
            self.signal_cooldown(agent_id).await;
            return Ok(());
        }
        let stored = self.process_results(agent_id, &results).await;

        let elapsed = started.elapsed();
        info!(
            agent_id,
            queries = queries.len(),
            results = results.len(),
            stored,
            duration_ms = elapsed.as_millis(),
            "Exploration: cycle complete",
        );

        self.signal_cooldown(agent_id).await;
        Ok(())
    }
}

impl Default for ExplorationRunner {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// EventHandler impl
// ---------------------------------------------------------------------------

#[async_trait]
impl EventHandler for ExplorationRunner {
    async fn handle(&self, event: Event) -> AmanResult<()> {
        if event.event_type != EventType::Idle {
            return Ok(());
        }
        let Some(kind) = event.payload.get("kind").and_then(|v| v.as_str()) else {
            return Ok(());
        };
        if kind != "exploration" {
            return Ok(());
        }
        let Some(agent_id) = event.payload.get("agentId").and_then(|v| v.as_str()) else {
            return Ok(());
        };
        if agent_id.is_empty() {
            return Ok(());
        }

        if !self.try_acquire(agent_id) {
            debug!(agent_id, "ExplorationRunner: already running, skipping duplicate");
            return Ok(());
        }

        let result = self.run_phases(agent_id).await;
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
    use kernel::memory::{
        EntityProfile, MemoryFilter, MemoryRecord, MemoryStats, SessionSummary, ThinkConfig,
        ThinkResult,
    };
    use std::sync::atomic::{AtomicBool, Ordering};

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

        async fn recall(&self, _agent_id: &str, _query: &str, _limit: usize) -> Vec<MemoryRecord> {
            vec![]
        }

        fn list(&self, _agent_id: &str, _filter: Option<&MemoryFilter>) -> Vec<MemoryRecord> {
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
        ) -> AmanResult<SessionSummary> {
            Ok(SessionSummary {
                session_id: "test".into(),
                memory_count: 0,
                duration_secs: 0.0,
                topics: vec![],
            })
        }

        async fn session_history(
            &self,
            _agent_id: &str,
            _limit: usize,
        ) -> AmanResult<Vec<SessionSummary>> {
            Ok(vec![])
        }

        async fn relate(&self, _from: &str, _to: &str, _rel_type: &str) -> AmanResult<()> {
            Ok(())
        }

        async fn get_edges(&self, _entity: &str) -> AmanResult<Vec<(String, String, String)>> {
            Ok(vec![])
        }

        async fn search_entities(&self, _query: &str, _limit: usize) -> AmanResult<Vec<String>> {
            Ok(vec!["rust".into(), "tokio".into()])
        }

        async fn entity_profile(
            &self,
            entity: &str,
        ) -> AmanResult<Option<EntityProfile>> {
            Ok(Some(EntityProfile {
                name: entity.to_owned(),
                aliases: vec![],
                edge_count: 0,
                related_entities: vec![],
            }))
        }

        async fn stale_memories(
            &self,
            _agent_id: &str,
            _days: u32,
        ) -> AmanResult<Vec<MemoryRecord>> {
            Ok(vec![MemoryRecord {
                rid: "r1".into(),
                content: "Bitcoin Layer 2 scaling solutions".into(),
                importance: Some(0.6),
                domain: Some("crypto".into()),
                tags: vec![],
                created_at_ms: 0,
            }])
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

        async fn stats(&self, _agent_id: &str) -> AmanResult<MemoryStats> {
            Ok(MemoryStats {
                total_entries: 100,
                index_size_bytes: 1024,
                graph_nodes: 5,
                graph_edges: 10,
                pending_conflicts: 0,
            })
        }

        async fn think(&self, _agent_id: &str, _config: &ThinkConfig) -> AmanResult<ThinkResult> {
            Ok(ThinkResult::default())
        }
    }

    // -----------------------------------------------------------------------
    // Guard tests
    // -----------------------------------------------------------------------

    #[test]
    fn guard_acquire_twice_fails() {
        let runner = ExplorationRunner::new();
        assert!(runner.try_acquire("agent-1"));
        assert!(!runner.try_acquire("agent-1"));
        runner.release("agent-1");
        assert!(runner.try_acquire("agent-1"));
        runner.release("agent-1");
    }

    #[test]
    fn guard_different_agents_independent() {
        let runner = ExplorationRunner::new();
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
        let runner = ExplorationRunner::new();
        let event = Event::new(
            "chat:user",
            EventType::MessageReceived,
            serde_json::json!({"text": "hello"}),
        );
        assert!(runner.handle(event).await.is_ok());
    }

    #[tokio::test]
    async fn handle_ignores_non_exploration_idle_events() {
        let runner = ExplorationRunner::new();
        let event = Event::new(
            "idle.system",
            EventType::Idle,
            serde_json::json!({"kind": "sleep", "depth": 20}),
        );
        assert!(runner.handle(event).await.is_ok());
    }

    #[tokio::test]
    async fn handle_ignores_idle_without_agent_id() {
        let runner = ExplorationRunner::new();
        let event = Event::new(
            "idle.system",
            EventType::Idle,
            serde_json::json!({"kind": "exploration", "depth": 50}),
        );
        assert!(runner.handle(event).await.is_ok());
    }

    // -----------------------------------------------------------------------
    // Query generation tests
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn generate_queries_from_stale_memories() {
        let provider: Arc<dyn MemoryProvider> = Arc::new(TestMemoryProvider::new());
        let runner = ExplorationRunner::new();
        runner.set_memory_provider(provider);

        let queries = runner.generate_queries("agent-1").await;
        // Should have at least the stale memory query + entity gap queries
        assert!(!queries.is_empty(), "should generate queries from memory gaps");
        let has_stale_query = queries.iter().any(|q| q.contains("latest information about"));
        assert!(has_stale_query, "should have query from stale memory");
    }

    #[tokio::test]
    async fn generate_queries_empty_without_provider() {
        let runner = ExplorationRunner::new();
        let queries = runner.generate_queries("agent-1").await;
        assert!(queries.is_empty());
    }

    // -----------------------------------------------------------------------
    // Process results tests
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn process_results_stores_high_quality_items() {
        let provider: Arc<dyn MemoryProvider> = Arc::new(TestMemoryProvider::new());
        let runner = ExplorationRunner::new();
        runner.set_memory_provider(provider);

        let items = vec![
            InfoItem {
                title: "Rust 2026 Edition Released".into(),
                url: "https://example.com/rust-2026".into(),
                summary: "The Rust team announced the 2026 edition with new features including...".into(),
                published: Some("2026-05-20".into()),
                source: "rsshub".into(),
                raw: serde_json::json!({}),
            },
            // Low-quality item: no URL, short summary
            InfoItem {
                title: "x".into(),
                url: "".into(),
                summary: "".into(),
                published: None,
                source: "test".into(),
                raw: serde_json::json!({}),
            },
        ];

        let stored = runner.process_results("agent-1", &items).await;
        assert!(stored >= 1, "should store at least the high-quality item");
    }

    #[tokio::test]
    async fn process_results_empty_without_provider() {
        let runner = ExplorationRunner::new();
        let items = vec![InfoItem {
            title: "Test".into(),
            url: "https://example.com".into(),
            summary: "A test".into(),
            published: Some("2026-05-20".into()),
            source: "test".into(),
            raw: serde_json::json!({}),
        }];
        let stored = runner.process_results("agent-1", &items).await;
        assert_eq!(stored, 0);
    }
}
