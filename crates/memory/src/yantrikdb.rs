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
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tracing::{debug, info, warn};

use crate::config::{EmbeddingConfig, MemoryConfig};
use crate::ollama_embedder::OllamaEmbedder;
use crate::remote_embedder::RemoteEmbedder;

// ---------------------------------------------------------------------------
// YantrikdbProvider
// ---------------------------------------------------------------------------

enum EmbedKind {
    Remote,
    Ollama,
    Download,
}

impl EmbedKind {
    fn as_str(&self) -> &'static str {
        match self {
            EmbedKind::Remote => "remote",
            EmbedKind::Ollama => "ollama",
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
    /// Fire-and-forget channel for think() — sends config to the background
    /// task that calls yantrikdb's synchronous think() via spawn_blocking.
    think_tx: Option<mpsc::Sender<yantrikdb::ThinkConfig>>,
    /// Receiver half of the think channel, stored until the background task
    /// is lazily spawned on the first async think() call (deferred because
    /// open() is synchronous and might not run inside a Tokio runtime).
    think_rx: Mutex<Option<mpsc::Receiver<yantrikdb::ThinkConfig>>>,
    /// Handle to the background think task, aborted on shutdown/drop.
    think_handle: Mutex<Option<JoinHandle<()>>>,
}

impl YantrikdbProvider {
    /// Open (or create) a yantrikdb database at `config.db_path`.
    ///
    /// Embedding backend is determined by `config.embedding`:
    /// - [`EmbeddingConfig::Remote`]: probes the API to detect dimension,
    ///   creates the database at that dim, and injects a [`RemoteEmbedder`].
    /// - [`EmbeddingConfig::Download`]: creates the database at the embedder's
    ///   known dim, then calls [`yantrikdb::YantrikDB::set_embedder_named`].
    pub fn open(config: &MemoryConfig) -> AmanResult<Self> {
        let (dim, embed_kind): (usize, EmbedKind) = match &config.embedding {
            EmbeddingConfig::Remote { dim, .. } => (*dim, EmbedKind::Remote),
            EmbeddingConfig::Ollama { dim, .. } => (*dim, EmbedKind::Ollama),
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
                let remote = RemoteEmbedder::new(base_url, api_key, model, dim);
                db.set_embedder(Box::new(remote)).map_err(|e| {
                    kernel::Error::Unrecoverable {
                        message: format!("yantrikdb set_embedder (remote): {e}"),
                    }
                })?;
                debug!("Attached RemoteEmbedder");
            }
            EmbedKind::Ollama => {
                let EmbeddingConfig::Ollama {
                    base_url,
                    model,
                    dim: _,
                } = &config.embedding
                else {
                    unreachable!()
                };
                let ollama = OllamaEmbedder::new(base_url, model, dim);
                db.set_embedder(Box::new(ollama)).map_err(|e| {
                    kernel::Error::Unrecoverable {
                        message: format!("yantrikdb set_embedder (ollama): {e}"),
                    }
                })?;
                debug!("Attached OllamaEmbedder");
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
        let (think_tx, think_rx) = mpsc::channel::<yantrikdb::ThinkConfig>(1);

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
        let meta = serde_json::json!({"type": session_type});
        self.db_ref()
            .session_start(agent_id, session_type, &meta)
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

    /// Fire-and-forget trigger for yantrikdb's synchronous `think()`.
    ///
    /// Sends the config through an mpsc channel (capacity 1). A background
    /// task receives it and runs [`YantrikDB::think`] via
    /// [`tokio::task::spawn_blocking`] so the async runtime is never
    /// stalled by yantrikdb's CPU-intensive graph computation. If a
    /// previous think is still running, the new request is silently
    /// dropped (channel full) to prevent backlog.
    ///
    /// The background task is spawned lazily on first call — this avoids
    /// requiring a Tokio runtime during synchronous [`YantrikdbProvider::open`].
    async fn think(&self, _agent_id: &str, config: &ThinkConfig) -> AmanResult<ThinkResult> {
        // Lazy spawn the background task on first call (we're in an async
        // context so a Tokio runtime is guaranteed to exist).
        {
            let mut handle_guard = self.think_handle.lock().unwrap();
            if handle_guard.is_none() {
                if let Some(rx) = self.think_rx.lock().unwrap().take() {
                    let db_weak = Arc::downgrade(
                        self.db.as_ref().expect("YantrikdbProvider closed"),
                    );
                    *handle_guard = Some(tokio::spawn(async move {
                        think_background(rx, db_weak).await;
                    }));
                }
            }
        }

        let ydb_config = yantrikdb::ThinkConfig {
            importance_threshold: config.importance_threshold,
            run_consolidation: config.run_consolidation,
            run_conflict_scan: config.run_conflict_scan,
            ..Default::default()
        };

        match self.think_tx.as_ref() {
            Some(tx) => match tx.try_send(ydb_config) {
                Ok(()) | Err(mpsc::error::TrySendError::Full(_)) => {
                    debug!("yantrikdb think() triggered (fire-and-forget)");
                }
                Err(mpsc::error::TrySendError::Closed(_)) => {
                    warn!("yantrikdb think channel closed, skipping think()");
                }
            },
            None => {
                debug!("yantrikdb think channel not initialized");
            }
        }

        // Always return empty — actual results flow through yantrikdb's
        // internal consolidation/conflict machinery.
        Ok(ThinkResult::default())
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

/// Background task that drains the think channel and runs yantrikdb's
/// synchronous `think()` on a blocking thread.
async fn think_background(
    mut rx: mpsc::Receiver<yantrikdb::ThinkConfig>,
    db_weak: std::sync::Weak<yantrikdb::YantrikDB>,
) {
    while let Some(config) = rx.recv().await {
        if let Some(db) = db_weak.upgrade() {
            let _ = tokio::task::spawn_blocking(move || {
                if let Err(e) = db.think(&config) {
                    warn!(error = %e, "yantrikdb think() background task failed");
                }
            })
            .await;
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
        {
            if let Err(e) = db.close() {
                tracing::error!(error = %e, "Error closing yantrikdb");
            }
        }
    }
}
