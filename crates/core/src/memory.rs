use async_trait::async_trait;

/// A single memory entry returned by a [`MemoryRetrieval`] implementation.
#[derive(Debug, Clone)]
pub struct MemoryEntry {
    pub id: u64,
    pub content: String,
    pub tags: Vec<String>,
    pub created_at_ms: u64,
}

/// Pluggable memory retrieval for long-term recall.
///
/// Each agent has its own namespace identified by `agent_id`.
/// Implementations can back this with in-memory storage, vector DBs,
/// external RAG services, etc.
#[async_trait]
pub trait MemoryRetrieval: Send + Sync {
    /// Store a memory entry for the given agent.
    fn store(&self, agent_id: &str, content: &str, tags: Vec<String>) -> u64;

    /// Retrieve memories relevant to the query for the given agent.
    async fn retrieve(&self, agent_id: &str, query: &str) -> Vec<MemoryEntry>;

    /// List all memories for an agent.
    fn list(&self, agent_id: &str) -> Vec<MemoryEntry>;

    /// Delete a memory entry by ID. Returns true if an entry was removed.
    fn delete(&self, agent_id: &str, entry_id: u64) -> bool;
}
