// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use crate::AmanResult;

// ---------------------------------------------------------------------------
// MemoryEntry — used by the original MemoryRetrieval trait (kept for compat)
// ---------------------------------------------------------------------------

/// A single memory entry returned by a [`MemoryRetrieval`] implementation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryEntry {
    pub id: u64,
    pub content: String,
    pub tags: Vec<String>,
    pub created_at_ms: u64,
}

// ---------------------------------------------------------------------------
// MemoryRetrieval — minimal pluggable memory (existing trait, kept for compat)
// ---------------------------------------------------------------------------

/// Pluggable memory retrieval for long-term recall.
///
/// Each agent has its own namespace identified by `agent_id`.
#[async_trait]
pub trait MemoryRetrieval: Send + Sync {
    fn store(&self, agent_id: &str, content: &str, tags: Vec<String>) -> u64;
    async fn retrieve(&self, agent_id: &str, query: &str) -> Vec<MemoryEntry>;
    fn list(&self, agent_id: &str) -> Vec<MemoryEntry>;
    fn delete(&self, agent_id: &str, entry_id: u64) -> bool;
}

// ---------------------------------------------------------------------------
// MemoryRecord — richer entry with a string record id (used by MemoryProvider)
// ---------------------------------------------------------------------------

/// A single memory record returned by a [`MemoryProvider`] implementation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryRecord {
    pub rid: String,
    pub content: String,
    pub tags: Vec<String>,
    pub created_at_ms: u64,
    /// Optional domain / category label.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub domain: Option<String>,
    /// Importance weight (0.0–1.0).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub importance: Option<f64>,
}

// ---------------------------------------------------------------------------
// Supporting types for MemoryProvider
// ---------------------------------------------------------------------------

/// Health snapshot of the memory store.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryStats {
    pub total_entries: u64,
    pub index_size_bytes: u64,
    pub graph_nodes: u64,
    pub graph_edges: u64,
    pub pending_conflicts: u64,
}

/// Summarised profile of a knowledge-graph entity.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EntityProfile {
    pub name: String,
    pub aliases: Vec<String>,
    pub edge_count: usize,
    pub related_entities: Vec<String>,
}

/// Summary returned when a session ends.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionSummary {
    pub session_id: String,
    pub memory_count: u64,
    pub duration_secs: f64,
    pub topics: Vec<String>,
}

/// Filter for listing / querying memories.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MemoryFilter {
    pub tags: Vec<String>,
    pub domain: Option<String>,
    pub since_ms: Option<u64>,
    pub limit: Option<usize>,
}

/// Options passed to [`MemoryProvider::initialize`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryInitOpts {
    pub db_path: String,
    pub agent_id: String,
}

// ---------------------------------------------------------------------------
// Think — cognitive processing (triggered by idle skills)
// ---------------------------------------------------------------------------

/// Configuration for a [`MemoryProvider::think`] pass.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThinkConfig {
    /// Minimum importance for decay-triggered memories (0.0–1.0).
    pub importance_threshold: f64,
    /// Whether to run memory consolidation (merge similar memories).
    pub run_consolidation: bool,
    /// Whether to scan for conflicts / contradictions.
    pub run_conflict_scan: bool,
}

impl Default for ThinkConfig {
    fn default() -> Self {
        Self {
            importance_threshold: 0.5,
            run_consolidation: true,
            run_conflict_scan: true,
        }
    }
}

/// Summary returned by [`MemoryProvider::think`].
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ThinkResult {
    /// Number of triggers fired.
    pub triggers_fired: usize,
    /// Number of memories consolidated / merged.
    pub consolidation_count: usize,
    /// Number of conflicts detected.
    pub conflicts_found: usize,
    /// New patterns discovered during pattern mining.
    pub patterns_new: usize,
    /// Existing patterns updated / reinforced.
    pub patterns_updated: usize,
    /// Duration of the think pass in milliseconds.
    pub duration_ms: u64,
}

// ---------------------------------------------------------------------------
// MemoryProvider — rich memory trait for agent + idle system needs
// ---------------------------------------------------------------------------

/// Full-featured memory provider.
///
/// Covers regular agent CRUD plus idle-system needs:
/// session management, knowledge graph, temporal queries, procedural memory,
/// and health stats.
///
/// # Default implementations
///
/// Only [`name`](Self::name), [`store`](Self::store), and [`recall`](Self::recall)
/// are required (no default body). All other methods have safe no-op defaults
/// returning empty results or `Ok(())`. Implementors can override only the
/// subset they support — no `unimplemented!()` panics.
#[async_trait]
pub trait MemoryProvider: Send + Sync {
    // -- Identity ----------------------------------------------------------

    /// Short identifier for this provider (e.g. "yantrikdb").
    fn name(&self) -> &str;

    /// Whether the provider is configured and ready to serve requests.
    fn is_available(&self) -> bool {
        true
    }

    // -- CRUD --------------------------------------------------------------

    /// Store a memory entry. Returns the provider-assigned record id.
    fn store(&self, agent_id: &str, content: &str, tags: Vec<String>) -> String {
        let _ = (agent_id, content, tags);
        unimplemented!("MemoryProvider::store")
    }

    /// Semantic recall for the given query.
    async fn recall(&self, agent_id: &str, query: &str, limit: usize) -> Vec<MemoryRecord> {
        let _ = (agent_id, query, limit);
        unimplemented!("MemoryProvider::recall")
    }

    /// List memories, optionally filtered.
    fn list(&self, _agent_id: &str, _filter: Option<&MemoryFilter>) -> Vec<MemoryRecord> {
        vec![]
    }

    /// Delete a memory record by id. Returns true if removed.
    fn forget(&self, _agent_id: &str, _rid: &str) -> bool {
        false
    }

    // -- Session management ------------------------------------------------

    /// Start a new session. Returns the session id.
    async fn session_start(&self, _agent_id: &str, _session_type: &str) -> AmanResult<String> {
        Ok(String::new())
    }

    /// End a session, returning its summary.
    async fn session_end(
        &self,
        _agent_id: &str,
        _session_id: &str,
    ) -> AmanResult<SessionSummary> {
        Ok(SessionSummary {
            session_id: String::new(),
            memory_count: 0,
            duration_secs: 0.0,
            topics: vec![],
        })
    }

    /// List recent sessions with their summaries.
    async fn session_history(
        &self,
        _agent_id: &str,
        _limit: usize,
    ) -> AmanResult<Vec<SessionSummary>> {
        Ok(vec![])
    }

    // -- Knowledge graph ---------------------------------------------------

    /// Create a directed relationship between two entities.
    async fn relate(&self, _from: &str, _to: &str, _rel_type: &str) -> AmanResult<()> {
        Ok(())
    }

    /// Get all edges originating from an entity.
    /// Returns `(from, to, rel_type)` tuples.
    async fn get_edges(&self, _entity: &str) -> AmanResult<Vec<(String, String, String)>> {
        Ok(vec![])
    }

    /// Search for entities whose names match the query.
    async fn search_entities(&self, _query: &str, _limit: usize) -> AmanResult<Vec<String>> {
        Ok(vec![])
    }

    /// Get the full profile of a named entity.
    async fn entity_profile(&self, _entity: &str) -> AmanResult<Option<EntityProfile>> {
        Ok(None)
    }

    // -- Temporal queries --------------------------------------------------

    /// Return high-importance memories not accessed in `days` days.
    async fn stale_memories(
        &self,
        _agent_id: &str,
        _days: u32,
    ) -> AmanResult<Vec<MemoryRecord>> {
        Ok(vec![])
    }

    /// Return memories with approaching deadlines within `days` days.
    async fn upcoming_memories(
        &self,
        _agent_id: &str,
        _days: u32,
    ) -> AmanResult<Vec<MemoryRecord>> {
        Ok(vec![])
    }

    // -- Procedural memory -------------------------------------------------

    /// Store a procedural memory (strategy / pattern).
    async fn store_procedural(
        &self,
        _agent_id: &str,
        _name: &str,
        _schema: &str,
        _kind: &str,
    ) -> AmanResult<String> {
        Ok(String::new())
    }

    /// Find procedural memories relevant to a context.
    async fn surface_procedural(
        &self,
        _agent_id: &str,
        _context: &str,
        _limit: usize,
    ) -> AmanResult<Vec<MemoryRecord>> {
        Ok(vec![])
    }

    // -- Health & stats ----------------------------------------------------

    /// Return a snapshot of memory store statistics.
    async fn stats(&self, _agent_id: &str) -> AmanResult<MemoryStats> {
        Ok(MemoryStats {
            total_entries: 0,
            index_size_bytes: 0,
            graph_nodes: 0,
            graph_edges: 0,
            pending_conflicts: 0,
        })
    }

    // -- Cognitive processing ----------------------------------------------

    /// Run a cognitive pass: trigger detection, consolidation, conflict scanning.
    ///
    /// Idle skills (Sleep, Meditation, Incubation) call this to let the memory
    /// engine perform background reasoning. Providers that lack a native
    /// cognition loop return an empty [`ThinkResult`].
    async fn think(&self, _agent_id: &str, _config: &ThinkConfig) -> AmanResult<ThinkResult> {
        Ok(ThinkResult {
            triggers_fired: 0,
            consolidation_count: 0,
            conflicts_found: 0,
            patterns_new: 0,
            patterns_updated: 0,
            duration_ms: 0,
        })
    }

    // -- Lifecycle ---------------------------------------------------------

    /// One-time initialisation (open database, run migrations, etc.).
    async fn initialize(&self, _opts: &MemoryInitOpts) -> AmanResult<()> {
        Ok(())
    }

    /// Graceful shutdown.
    async fn shutdown(&self) -> AmanResult<()> {
        Ok(())
    }
}
