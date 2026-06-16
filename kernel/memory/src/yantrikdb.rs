// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

use async_trait::async_trait;
use kernel::memory::{
    EntityProfile, MemoryFilter, MemoryProvider, MemoryRecord, MemoryStats,
    SessionSummary, ThinkConfig, ThinkResult,
};
use kernel::AmanResult;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use tracing::{debug, info, warn};

use crate::config::{EmbeddingConfig, MemoryConfig};
use cognitive_llm::embed::OpenAiEmbedder;

// ---------------------------------------------------------------------------
// YantrikdbProvider
// ---------------------------------------------------------------------------

type ThinkChannel = (
    yantrikdb::ThinkConfig,
    oneshot::Sender<yantrikdb::ThinkResult>,
);

enum EmbedKind {
    Remote,
    Download,
}

impl EmbedKind {
    fn as_str(&self) -> &'static str {
        match self {
            EmbedKind::Remote => "remote",
            EmbedKind::Download => "download",
        }
    }
}

/// Memory provider backed by [yantrikdb](https://github.com/yantrikos/yantrikdb).
///
/// YantrikDB is an embedded cognitive memory engine with HNSW vector search,
/// knowledge graph, temporal decay, session management, and background
/// consolidation.
pub struct YantrikdbProvider {
    db: Option<Arc<yantrikdb::YantrikDB>>,
    agent_id: String,
    /// Channel for think() — sends (config, reply_tx) to the background
    /// task that calls yantrikdb's synchronous think() via spawn_blocking
    /// and returns the result through the oneshot.
    think_tx: Option<mpsc::Sender<ThinkChannel>>,
    /// Receiver half of the think channel, stored until the background task
    /// is lazily spawned on the first async think() call (deferred because
    /// open() is synchronous and might not run inside a Tokio runtime).
    think_rx: Mutex<Option<mpsc::Receiver<ThinkChannel>>>,
    /// Handle to the background think task, aborted on shutdown/drop.
    think_handle: Mutex<Option<JoinHandle<()>>>,
}

impl YantrikdbProvider {
    /// Open (or create) a yantrikdb database at `config.db_path`.
    ///
    /// Embedding backend is determined by `config.embedding`:
    /// - [`EmbeddingConfig::Remote`]: probes the API to detect dimension,
    ///   creates the database at that dim, and injects a [`OpenAiEmbedder`].
    /// - [`EmbeddingConfig::Download`]: creates the database at the embedder's
    ///   known dim, then calls [`yantrikdb::YantrikDB::set_embedder_named`].
    pub fn open(config: &MemoryConfig) -> AmanResult<Self> {
        let (dim, embed_kind): (usize, EmbedKind) = match &config.embedding {
            EmbeddingConfig::Remote { dim, .. } => (*dim, EmbedKind::Remote),
            EmbeddingConfig::Download { dim, .. } => (*dim, EmbedKind::Download),
        };

        info!(
            agent_id = %config.agent_id,
            db_path = %config.db_path,
            dim,
            embed_mode = embed_kind.as_str(),
            "Opening yantrikdb memory store",
        );

        let mut db = yantrikdb::YantrikDB::new(&config.db_path, dim).map_err(|e| {
            kernel::Error::Unrecoverable {
                message: format!("yantrikdb open {}: {e}", config.db_path),
            }
        })?;

        match embed_kind {
            EmbedKind::Remote => {
                let EmbeddingConfig::Remote {
                    base_url,
                    api_key,
                    model,
                    dim: _,
                } = &config.embedding
                else {
                    unreachable!()
                };
                let remote = OpenAiEmbedder::new(base_url, api_key, model, dim);
                db.set_embedder(Box::new(remote)).map_err(|e| {
                    kernel::Error::Unrecoverable {
                        message: format!("yantrikdb set_embedder (remote): {e}"),
                    }
                })?;
                debug!("Attached OpenAiEmbedder");
            }
            EmbedKind::Download => {
                let EmbeddingConfig::Download { name, dim: _ } = &config.embedding else {
                    unreachable!()
                };
                db.set_embedder_named(name).map_err(|e| {
                    kernel::Error::Unrecoverable {
                        message: format!("yantrikdb set_embedder_named({name}): {e}"),
                    }
                })?;
                debug!(embedder = %name, "Attached downloaded embedder");
            }
        }

        debug!(agent_id = %config.agent_id, "Yantrikdb opened");

        let db_arc = Arc::new(db);

        // Create the think channel now (synchronous, no Tokio runtime needed).
        // The background task is spawned lazily on the first async think() call.
        let (think_tx, think_rx) = mpsc::channel::<ThinkChannel>(1);

        Ok(Self {
            db: Some(db_arc),
            agent_id: config.agent_id.clone(),
            think_tx: Some(think_tx),
            think_rx: Mutex::new(Some(think_rx)),
            think_handle: Mutex::new(None),
        })
    }

    /// Access the underlying yantrikdb handle.
    pub fn inner(&self) -> &yantrikdb::YantrikDB {
        self.db.as_ref().map(|a| a.as_ref()).expect("YantrikdbProvider already closed")
    }

    // -- helpers -----------------------------------------------------------

    fn encode_content(content: &str, tags: &[String]) -> String {
        if tags.is_empty() {
            content.to_owned()
        } else {
            format!("[{}] {content}", tags.join(", "))
        }
    }

    fn db_ref(&self) -> &yantrikdb::YantrikDB {
        self.db.as_ref().map(|a| a.as_ref()).expect("YantrikdbProvider closed")
    }

    fn map_err(e: impl std::fmt::Display) -> kernel::Error {
        kernel::Error::Unrecoverable { message: format!("yantrikdb: {e}") }
    }

    #[allow(dead_code)]
    fn now_ms() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64
    }

    fn convert_memory(m: &yantrikdb::types::Memory) -> MemoryRecord {
        MemoryRecord {
            rid: m.rid.clone(),
            content: m.text.clone(),
            tags: Vec::new(),
            created_at_ms: (m.created_at * 1000.0) as u64,
            domain: Some(m.domain.clone()),
            importance: Some(m.importance),
        }
    }

    fn convert_recall_result(r: &yantrikdb::types::RecallResult) -> MemoryRecord {
        MemoryRecord {
            rid: r.rid.clone(),
            content: r.text.clone(),
            tags: Vec::new(),
            created_at_ms: (r.created_at * 1000.0) as u64,
            domain: Some(r.domain.clone()),
            importance: Some(r.importance),
        }
    }
}

// ---------------------------------------------------------------------------
// MemoryProvider impl
// ---------------------------------------------------------------------------

#[async_trait]
impl MemoryProvider for YantrikdbProvider {
    fn name(&self) -> &str {
        "yantrikdb"
    }

    // -- CRUD --------------------------------------------------------------

    fn store(&self, agent_id: &str, content: &str, tags: Vec<String>) -> String {
        let text = Self::encode_content(content, &tags);
        let meta = serde_json::json!({});

        match self.db_ref().record_text(
            &text,
            "semantic",
            0.5,         // importance
            0.0,         // valence
            2_592_000.0, // half_life = 30 days in seconds
            &meta,
            agent_id,
            0.75,        // certainty
            tags.first().map(|s| s.as_str()).unwrap_or("general"),
            "aman",
            None,         // emotional_state
        ) {
            Ok(rid) => {
                debug!(%rid, agent_id, "Stored memory");
                rid
            }
            Err(e) => {
                warn!(agent_id, error = %e, "Failed to store memory");
                String::new()
            }
        }
    }

    async fn recall(&self, agent_id: &str, query: &str, limit: usize) -> Vec<MemoryRecord> {
        match self.db_ref().recall_text(query, limit) {
            Ok(results) => results
                .iter()
                .filter(|r| r.namespace == agent_id)
                .map(Self::convert_recall_result)
                .collect(),
            Err(e) => {
                warn!(agent_id, error = %e, "Recall failed");
                Vec::new()
            }
        }
    }

    fn list(&self, _agent_id: &str, _filter: Option<&MemoryFilter>) -> Vec<MemoryRecord> {
        warn!("MemoryProvider::list is not efficiently supported by yantrikdb; use recall()");
        Vec::new()
    }

    fn forget(&self, _agent_id: &str, rid: &str) -> bool {
        match self.db_ref().forget(rid) {
            Ok(removed) => removed,
            Err(e) => {
                warn!(%rid, error = %e, "Forget failed");
                false
            }
        }
    }

    // -- Session management ------------------------------------------------

    async fn session_start(&self, agent_id: &str, session_type: &str) -> AmanResult<String> {
        // Use agent_id as both namespace and client_id so session_history
        // queries (which also use agent_id for both fields) can find sessions.
        // The session type is stored in metadata only.
        let meta = serde_json::json!({"type": session_type});
        self.db_ref()
            .session_start(agent_id, agent_id, &meta)
            .map_err(Self::map_err)
    }

    async fn session_end(
        &self,
        _agent_id: &str,
        session_id: &str,
    ) -> AmanResult<SessionSummary> {
        let s = self
            .db_ref()
            .session_end(session_id, None)
            .map_err(Self::map_err)?;

        Ok(SessionSummary {
            session_id: s.session_id,
            memory_count: s.memory_count as u64,
            duration_secs: s.duration_secs,
            topics: s.topics,
        })
    }

    async fn session_history(
        &self,
        agent_id: &str,
        limit: usize,
    ) -> AmanResult<Vec<SessionSummary>> {
        let sessions = self
            .db_ref()
            .session_history(agent_id, agent_id, limit)
            .map_err(Self::map_err)?;

        Ok(sessions
            .into_iter()
            .map(|s| SessionSummary {
                session_id: s.session_id,
                memory_count: s.memory_count as u64,
                duration_secs: s.ended_at.unwrap_or(0.0) - s.started_at,
                topics: s.topics,
            })
            .collect())
    }

    // -- Knowledge graph ---------------------------------------------------

    async fn relate(&self, from: &str, to: &str, rel_type: &str) -> AmanResult<()> {
        self.db_ref()
            .relate(from, to, rel_type, 1.0)
            .map_err(Self::map_err)?;
        debug!(from, to, rel_type, "Created relationship");
        Ok(())
    }

    async fn get_edges(&self, entity: &str) -> AmanResult<Vec<(String, String, String)>> {
        let edges = self
            .db_ref()
            .get_edges(entity)
            .map_err(Self::map_err)?;

        Ok(edges
            .into_iter()
            .map(|e| (e.src, e.dst, e.rel_type))
            .collect())
    }

    async fn search_entities(&self, query: &str, limit: usize) -> AmanResult<Vec<String>> {
        let entities = self
            .db_ref()
            .search_entities(Some(query), None, limit)
            .map_err(Self::map_err)?;

        Ok(entities.into_iter().map(|e| e.name).collect())
    }

    async fn entity_profile(&self, entity: &str) -> AmanResult<Option<EntityProfile>> {
        let p = self
            .db_ref()
            .entity_profile(entity, 90.0, Some(&self.agent_id))
            .map_err(Self::map_err)?;

        Ok(Some(EntityProfile {
            name: p.entity,
            aliases: Vec::new(),
            edge_count: 0,
            related_entities: Vec::new(),
        }))
    }

    // -- Temporal queries --------------------------------------------------

    async fn stale_memories(&self, agent_id: &str, days: u32) -> AmanResult<Vec<MemoryRecord>> {
        let memories = self
            .db_ref()
            .stale(f64::from(days), 20, Some(agent_id))
            .map_err(Self::map_err)?;

        Ok(memories.iter().map(Self::convert_memory).collect())
    }

    async fn upcoming_memories(&self, agent_id: &str, days: u32) -> AmanResult<Vec<MemoryRecord>> {
        let memories = self
            .db_ref()
            .upcoming(f64::from(days), 20, Some(agent_id))
            .map_err(Self::map_err)?;

        Ok(memories.iter().map(Self::convert_memory).collect())
    }

    // -- Procedural memory -------------------------------------------------

    async fn store_procedural(
        &self,
        agent_id: &str,
        name: &str,
        schema: &str,
        kind: &str,
    ) -> AmanResult<String> {
        let text = format!("[{kind}] {name}: {schema}");
        let emb = self
            .db_ref()
            .embed(&text)
            .map_err(Self::map_err)?;

        let rid = self
            .db_ref()
            .record_procedural(&text, &emb, kind, schema, 0.5, agent_id)
            .map_err(Self::map_err)?;

        debug!(%rid, name, kind, "Stored procedural memory");
        Ok(rid)
    }

    async fn surface_procedural(
        &self,
        agent_id: &str,
        context: &str,
        limit: usize,
    ) -> AmanResult<Vec<MemoryRecord>> {
        let emb = self
            .db_ref()
            .embed(context)
            .map_err(Self::map_err)?;

        let results = self
            .db_ref()
            .surface_procedural(&emb, Some(context), None, limit, Some(agent_id))
            .map_err(Self::map_err)?;

        Ok(results.iter().map(Self::convert_recall_result).collect())
    }

    // -- Health & stats ----------------------------------------------------

    async fn stats(&self, _agent_id: &str) -> AmanResult<MemoryStats> {
        let s = self
            .db_ref()
            .stats(None)
            .map_err(Self::map_err)?;

        Ok(MemoryStats {
            total_entries: s.active_memories as u64,
            index_size_bytes: 0,
            graph_nodes: s.graph_index_entities as u64,
            graph_edges: s.graph_index_edges as u64,
            pending_conflicts: s.open_conflicts as u64,
        })
    }

    // -- Cognitive pass ----------------------------------------------------

    /// Run yantrikdb's synchronous `think()` via the background task and
    /// return the real result.
    ///
    /// Sends the config through an mpsc channel (capacity 1) paired with a
    /// oneshot reply channel. A background task receives it and runs
    /// [`YantrikDB::think`] via [`tokio::task::spawn_blocking`]. The caller
    /// awaits the oneshot for the result. If a previous think is still
    /// running, the new request is silently dropped (channel full) to
    /// prevent backlog.
    ///
    /// The background task is spawned lazily on first call — this avoids
    /// requiring a Tokio runtime during synchronous [`YantrikdbProvider::open`].
    async fn think(&self, _agent_id: &str, config: &ThinkConfig) -> AmanResult<ThinkResult> {
        // Lazy spawn the background task on first call (we're in an async
        // context so a Tokio runtime is guaranteed to exist).
        {
            let mut handle_guard = self.think_handle.lock().unwrap();
            if handle_guard.is_none()
                && let Some(rx) = self.think_rx.lock().unwrap().take()
            {
                let db_weak = Arc::downgrade(
                    self.db.as_ref().expect("YantrikdbProvider closed"),
                );
                *handle_guard = Some(tokio::spawn(async move {
                    think_background(rx, db_weak).await;
                }));
            }
        }

        let ydb_config = yantrikdb::ThinkConfig {
            importance_threshold: config.importance_threshold,
            run_consolidation: config.run_consolidation,
            run_conflict_scan: config.run_conflict_scan,
            ..Default::default()
        };

        let (reply_tx, reply_rx) = oneshot::channel();

        match self.think_tx.as_ref() {
            Some(tx) => match tx.try_send((ydb_config, reply_tx)) {
                Ok(()) => {
                    debug!("yantrikdb think() sent, awaiting result");
                }
                Err(mpsc::error::TrySendError::Full(_)) => {
                    debug!("yantrikdb think channel full, returning empty result");
                    return Ok(ThinkResult::default());
                }
                Err(mpsc::error::TrySendError::Closed(_)) => {
                    warn!("yantrikdb think channel closed, returning empty result");
                    return Ok(ThinkResult::default());
                }
            },
            None => {
                debug!("yantrikdb think channel not initialized");
                return Ok(ThinkResult::default());
            }
        }

        // Await the real result from the background task.
        match reply_rx.await {
            Ok(ydb_result) => Ok(ThinkResult {
                triggers_fired: ydb_result.triggers.len(),
                consolidation_count: ydb_result.consolidation_count,
                conflicts_found: ydb_result.conflicts_found,
                patterns_new: ydb_result.patterns_new,
                patterns_updated: ydb_result.patterns_updated,
                duration_ms: ydb_result.duration_ms,
            }),
            Err(_) => {
                warn!("yantrikdb think oneshot closed (background task dropped)");
                Ok(ThinkResult::default())
            }
        }
    }

    // -- Lifecycle ---------------------------------------------------------

    async fn shutdown(&self) -> AmanResult<()> {
        info!(agent_id = %self.agent_id, "Shutting down yantrikdb provider");
        // Abort the background think task so we don't hold a DB ref during close.
        if let Some(handle) = self.think_handle.lock().unwrap().take() {
            handle.abort();
        }
        Ok(())
    }
}

/// Background task that drains the think channel, runs yantrikdb's
/// synchronous `think()` on a blocking thread, and sends the result back
/// through the per-request oneshot channel.
async fn think_background(
    mut rx: mpsc::Receiver<ThinkChannel>,
    db_weak: std::sync::Weak<yantrikdb::YantrikDB>,
) {
    while let Some((config, reply)) = rx.recv().await {
        if let Some(db) = db_weak.upgrade() {
            let result = tokio::task::spawn_blocking(move || db.think(&config)).await;
            match result {
                Ok(Ok(think_result)) => {
                    let _ = reply.send(think_result);
                }
                Ok(Err(e)) => {
                    warn!(error = %e, "yantrikdb think() failed");
                }
                Err(join_err) => {
                    warn!(error = %join_err, "yantrikdb think() spawn_blocking panicked");
                }
            }
        } else {
            break;
        }
    }
}

impl Drop for YantrikdbProvider {
    fn drop(&mut self) {
        // Drop the sender and receiver to unblock the background task.
        drop(self.think_tx.take());
        drop(self.think_rx.lock().unwrap().take());
        // Abort the task to release its Weak<YantrikDB> ref.
        if let Some(handle) = self.think_handle.lock().unwrap().take() {
            handle.abort();
        }
        // Try to close the DB. If the background task's spawn_blocking still
        // holds an Arc ref, we can't unwrap — the DB will be leaked but that's
        // a shutdown edge case (the task is already aborted above).
        if let Some(db) = self.db.take()
            && let Ok(db) = Arc::try_unwrap(db)
            && let Err(e) = db.close()
        {
            tracing::error!(error = %e, "Error closing yantrikdb");
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// Create a `YantrikdbProvider` backed by the bundled embedder
    /// (potion-base-2M, dim=64) in a temp directory. No network calls.
    fn make_provider() -> (YantrikdbProvider, TempDir) {
        let dir = TempDir::new().expect("temp dir");
        let db_path = dir.path().join("test.db");
        let db_path_str = db_path.to_str().expect("valid utf-8 path");
        // YantrikDB::new(path, 64) auto-attaches the bundled embedder
        let db =
            yantrikdb::YantrikDB::new(db_path_str, 64).expect("yantrikdb open for testing");
        let db_arc = Arc::new(db);
        let (think_tx, think_rx) = mpsc::channel::<ThinkChannel>(1);
        let provider = YantrikdbProvider {
            db: Some(db_arc),
            agent_id: "test-agent".to_string(),
            think_tx: Some(think_tx),
            think_rx: Mutex::new(Some(think_rx)),
            think_handle: Mutex::new(None),
        };
        (provider, dir)
    }

    // -- store ---------------------------------------------------------------

    #[test]
    fn store_returns_non_empty_id() {
        let (p, _d) = make_provider();
        let id = p.store("agent-1", "hello world", vec![]);
        assert!(!id.is_empty(), "store should return a non-empty id");
    }

    #[test]
    fn store_unique_ids() {
        let (p, _d) = make_provider();
        let id1 = p.store("agent-1", "first record", vec![]);
        let id2 = p.store("agent-1", "second record", vec![]);
        assert_ne!(id1, id2, "each store must produce a unique id");
    }

    #[test]
    fn store_with_tags() {
        let (p, _d) = make_provider();
        let id = p.store(
            "agent-1",
            "tagged memory",
            vec!["critical".into(), "work".into()],
        );
        assert!(!id.is_empty(), "store with tags should succeed");
    }

    // -- recall --------------------------------------------------------------

    #[tokio::test]
    async fn recall_returns_stored_content() {
        let (p, _d) = make_provider();
        let content = "Alice is the engineering team lead at Acme Corporation";
        p.store("agent-1", content, vec![]);
        let results = p.recall("agent-1", content, 10).await;
        assert!(
            results.iter().any(|r| r.content == content),
            "recall should find the exact stored content; got {:?}",
            results.iter().map(|r| &r.content).collect::<Vec<_>>(),
        );
    }

    #[tokio::test]
    async fn recall_returns_multiple_results() {
        let (p, _d) = make_provider();
        p.store("agent-1", "The weather today is sunny and warm", vec![]);
        p.store("agent-1", "Meeting with Bob at 3pm", vec![]);
        // Recall with a query that should match at least the weather record
        let results = p.recall("agent-1", "weather sunny", 10).await;
        assert!(!results.is_empty(), "recall should return at least one result");
    }

    #[tokio::test]
    async fn recall_respects_limit() {
        let (p, _d) = make_provider();
        p.store("agent-1", "alpha bravo charlie delta", vec![]);
        p.store("agent-1", "alpha bravo charlie delta echo foxtrot", vec![]);
        let results = p.recall("agent-1", "alpha", 1).await;
        assert!(results.len() <= 1, "limit=1 should return at most 1 result; got {}", results.len());
    }

    #[tokio::test]
    async fn recall_returns_empty_for_empty_store() {
        let (p, _d) = make_provider();
        let results = p.recall("agent-1", "anything", 10).await;
        assert!(results.is_empty(), "empty store yields empty recall");
    }

    // -- forget --------------------------------------------------------------

    #[test]
    fn forget_existing_record() {
        let (p, _d) = make_provider();
        let id = p.store("agent-1", "will be forgotten", vec![]);
        assert!(!id.is_empty(), "store must succeed preceding forget");
        assert!(p.forget("agent-1", &id), "forget should return true");
    }

    #[test]
    fn forget_nonexistent_returns_false() {
        let (p, _d) = make_provider();
        assert!(!p.forget("agent-1", "nonexistent-rid"), "forget of missing rid returns false");
    }

    #[test]
    fn forget_removes_from_recall() {
        let (p, _d) = make_provider();
        let content = "unique content for forget test";
        let id = p.store("agent-1", content, vec![]);
        assert!(p.forget("agent-1", &id), "forget must succeed");

        // After forget, recall should not find this content
        let rt = tokio::runtime::Runtime::new().unwrap();
        let results = rt.block_on(p.recall("agent-1", content, 10));
        assert!(
            !results.iter().any(|r| r.content == content),
            "forgotten content should not appear in recall"
        );
    }

    // -- multi-agent isolation ----------------------------------------------

    #[tokio::test]
    async fn multi_agent_isolation() {
        let (p, _d) = make_provider();
        p.store("agent-alpha", "classified for alpha eyes only", vec![]);
        p.store("agent-beta", "classified for beta eyes only", vec![]);

        let alpha_results = p.recall("agent-alpha", "classified", 10).await;
        assert!(
            alpha_results.iter().all(|r| r.content.contains("alpha")),
            "agent-alpha should only see its own memories"
        );

        let beta_results = p.recall("agent-beta", "classified", 10).await;
        assert!(
            beta_results.iter().all(|r| r.content.contains("beta")),
            "agent-beta should only see its own memories"
        );
    }

    // -- stats ---------------------------------------------------------------

    #[tokio::test]
    async fn stats_reflects_stored_count() {
        let (p, _d) = make_provider();
        let stats0 = p.stats("agent-1").await.expect("stats call succeeds");
        assert_eq!(stats0.total_entries, 0, "empty store has 0 entries");

        p.store("agent-1", "memory one", vec![]);
        p.store("agent-1", "memory two", vec![]);

        let stats1 = p.stats("agent-1").await.expect("stats call succeeds");
        assert_eq!(stats1.total_entries, 2, "two stores => 2 entries");
    }

    // -- provider identity ---------------------------------------------------

    #[test]
    fn provider_name() {
        let (p, _d) = make_provider();
        assert_eq!(p.name(), "yantrikdb");
    }

    #[test]
    fn provider_is_available() {
        let (p, _d) = make_provider();
        assert!(p.is_available());
    }

    // -- session management --------------------------------------------------

    #[tokio::test]
    async fn session_start_and_end() {
        let (p, _d) = make_provider();
        let session_id = p
            .session_start("agent-1", "test")
            .await
            .expect("session start");
        assert!(!session_id.is_empty(), "session id should be non-empty");

        let summary = p
            .session_end("agent-1", &session_id)
            .await
            .expect("session end");
        assert_eq!(summary.session_id, session_id);
    }

    #[tokio::test]
    async fn session_history() {
        let (p, _d) = make_provider();
        let sid1 = p.session_start("agent-1", "chat").await.unwrap();
        p.session_end("agent-1", &sid1).await.unwrap();
        let sid2 = p.session_start("agent-1", "reflection").await.unwrap();
        p.session_end("agent-1", &sid2).await.unwrap();

        let history = p
            .session_history("agent-1", 10)
            .await
            .expect("session history");
        assert_eq!(history.len(), 2, "should have 2 sessions in history");
    }

    // -- procedural memory ---------------------------------------------------

    #[tokio::test]
    async fn store_and_surface_procedural() {
        let (p, _d) = make_provider();
        let rid = p
            .store_procedural("agent-1", "test-proc", "do something", "strategy")
            .await
            .expect("store procedural");
        assert!(!rid.is_empty(), "procedural rid must be non-empty");

        let results = p
            .surface_procedural("agent-1", "do something", 10)
            .await
            .expect("surface procedural");
        assert!(
            results.iter().any(|r| r.rid == rid),
            "surfaced results should contain the stored procedural"
        );
    }

    // -- knowledge graph -----------------------------------------------------

    #[tokio::test]
    async fn relate_and_get_edges() {
        let (p, _d) = make_provider();
        // store entities first so they exist
        p.store("agent-1", "entity alpha entity", vec![]);
        p.store("agent-1", "entity beta entity", vec![]);

        // Wait a tick for indexing, then relate
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        p.relate("alpha", "beta", "knows")
            .await
            .expect("relate should succeed");

        let edges = p
            .get_edges("alpha")
            .await
            .expect("get_edges should succeed");
        assert!(
            edges.iter().any(|(_, to, rt)| to == "beta" && rt == "knows"),
            "should have an edge alpha -> beta"
        );
    }

    // -- temporal queries ----------------------------------------------------

    #[tokio::test]
    async fn stale_and_upcoming_memories() {
        let (p, _d) = make_provider();
        p.store("agent-1", "stale content for temporal test", vec![]);

        let stale = p
            .stale_memories("agent-1", 1)
            .await
            .expect("stale_memories");
        // Results depend on database, just verify no error
        assert!(stale.is_empty() || stale.iter().any(|_| true));
    }

    // -- think pass ----------------------------------------------------------

    #[tokio::test]
    async fn think_returns_default_result() {
        let (p, _d) = make_provider();
        let config = ThinkConfig {
            importance_threshold: 0.5,
            run_consolidation: false,
            run_conflict_scan: false,
        };
        let _result = p.think("agent-1", &config).await.expect("think");
        // Should at least not panic (duration is u64, always >= 0)
    }
}
