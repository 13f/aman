// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

use async_trait::async_trait;
use kernel::memory::{
    EntityProfile, MemoryEntry, MemoryFilter, MemoryRecord, MemoryStats,
    SessionSummary,
};
use kernel::AmanResult;
use std::collections::{HashMap, HashSet};
use std::sync::RwLock;
use std::time::{SystemTime, UNIX_EPOCH};

/// Simple in-memory memory store with keyword-based retrieval.
///
/// Each agent has its own namespace. Memories are indexed by agent_id
/// and searched by keyword overlap with the query text.
pub struct MemoryStore {
    /// agent_id → Vec<MemoryEntry>
    memories: RwLock<HashMap<String, Vec<MemoryEntry>>>,
    next_id: RwLock<u64>,
    /// Maximum results returned per query.
    max_results: usize,
}

impl MemoryStore {
    /// Create a new MemoryStore with default max_results of 5.
    pub fn new() -> Self {
        Self::with_max_results(5)
    }

    pub fn with_max_results(max_results: usize) -> Self {
        Self {
            memories: RwLock::new(HashMap::new()),
            next_id: RwLock::new(1),
            max_results,
        }
    }
}

#[async_trait]
impl kernel::memory::MemoryRetrieval for MemoryStore {
    fn store(&self, agent_id: &str, content: &str, tags: Vec<String>) -> u64 {
        let mut id = self.next_id.write().expect("next_id lock");
        let entry_id = *id;
        *id += 1;

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        let entry = MemoryEntry {
            id: entry_id,
            content: content.to_owned(),
            tags,
            created_at_ms: now,
        };

        let mut memories = self.memories.write().expect("memories lock");
        memories.entry(agent_id.to_owned()).or_default().push(entry);

        entry_id
    }

    async fn retrieve(&self, agent_id: &str, query: &str) -> Vec<MemoryEntry> {
        let memories = self.memories.read().expect("memories lock");
        let Some(entries) = memories.get(agent_id) else {
            return vec![];
        };

        let query_tokens: HashSet<String> = tokenize(query);

        let mut scored: Vec<(i64, &MemoryEntry)> = entries
            .iter()
            .map(|entry| {
                let content_tokens: HashSet<String> = tokenize(&entry.content);
                let tag_tokens: HashSet<String> = entry
                    .tags
                    .iter()
                    .flat_map(|t| tokenize(t))
                    .collect();
                let all_tokens: HashSet<String> =
                    content_tokens.union(&tag_tokens).cloned().collect();
                let overlap = query_tokens.intersection(&all_tokens).count() as i64;
                (overlap, entry)
            })
            .collect();

        scored.sort_by(|a, b| {
            b.0.cmp(&a.0)
                .then_with(|| b.1.created_at_ms.cmp(&a.1.created_at_ms))
        });

        scored
            .into_iter()
            .filter(|(score, _)| *score > 0)
            .take(self.max_results)
            .map(|(_, entry)| entry.clone())
            .collect()
    }

    fn list(&self, agent_id: &str) -> Vec<MemoryEntry> {
        let memories = self.memories.read().expect("memories lock");
        memories
            .get(agent_id)
            .cloned()
            .unwrap_or_default()
    }

    fn delete(&self, agent_id: &str, entry_id: u64) -> bool {
        let mut memories = self.memories.write().expect("memories lock");
        if let Some(entries) = memories.get_mut(agent_id) {
            let before = entries.len();
            entries.retain(|e| e.id != entry_id);
            entries.len() < before
        } else {
            false
        }
    }
}

/// Tokenize text into lowercase words for keyword matching.
fn tokenize(text: &str) -> HashSet<String> {
    text.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|s| !s.is_empty() && s.len() >= 2)
        .map(String::from)
        .collect()
}

// ---------------------------------------------------------------------------
// MemoryProvider impl — delegates CRUD to MemoryRetrieval, stubs the rest
// ---------------------------------------------------------------------------

impl MemoryStore {
    fn entry_to_record(&self, entry: &MemoryEntry) -> MemoryRecord {
        MemoryRecord {
            rid: entry.id.to_string(),
            content: entry.content.clone(),
            tags: entry.tags.clone(),
            created_at_ms: entry.created_at_ms,
            domain: None,
            importance: None,
        }
    }
}

#[async_trait]
impl kernel::memory::MemoryProvider for MemoryStore {
    fn name(&self) -> &str {
        "in-memory"
    }

    fn is_available(&self) -> bool {
        true
    }

    fn store(&self, agent_id: &str, content: &str, tags: Vec<String>) -> String {
        let id = <Self as kernel::memory::MemoryRetrieval>::store(self, agent_id, content, tags);
        id.to_string()
    }

    async fn recall(&self, agent_id: &str, query: &str, limit: usize) -> Vec<MemoryRecord> {
        let entries = <Self as kernel::memory::MemoryRetrieval>::retrieve(self, agent_id, query).await;
        entries.iter().take(limit).map(|e| self.entry_to_record(e)).collect()
    }

    fn list(&self, agent_id: &str, _filter: Option<&MemoryFilter>) -> Vec<MemoryRecord> {
        let entries = <Self as kernel::memory::MemoryRetrieval>::list(self, agent_id);
        entries.iter().map(|e| self.entry_to_record(e)).collect()
    }

    fn forget(&self, agent_id: &str, rid: &str) -> bool {
        let id: u64 = rid.parse().unwrap_or(0);
        <Self as kernel::memory::MemoryRetrieval>::delete(self, agent_id, id)
    }

    async fn session_start(&self, _agent_id: &str, _session_type: &str) -> AmanResult<String> {
        Ok(format!("mem-{}", SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()))
    }

    async fn session_end(&self, _agent_id: &str, session_id: &str) -> AmanResult<SessionSummary> {
        Ok(SessionSummary {
            session_id: session_id.to_owned(),
            memory_count: 0,
            duration_secs: 0.0,
            topics: Vec::new(),
        })
    }

    async fn session_history(&self, _agent_id: &str, _limit: usize) -> AmanResult<Vec<SessionSummary>> {
        Ok(Vec::new())
    }

    async fn relate(&self, _from: &str, _to: &str, _rel_type: &str) -> AmanResult<()> {
        Ok(())
    }

    async fn get_edges(&self, _entity: &str) -> AmanResult<Vec<(String, String, String)>> {
        Ok(Vec::new())
    }

    async fn search_entities(&self, _query: &str, _limit: usize) -> AmanResult<Vec<String>> {
        Ok(Vec::new())
    }

    async fn entity_profile(&self, _entity: &str) -> AmanResult<Option<EntityProfile>> {
        Ok(None)
    }

    async fn stale_memories(&self, _agent_id: &str, _days: u32) -> AmanResult<Vec<MemoryRecord>> {
        Ok(Vec::new())
    }

    async fn upcoming_memories(&self, _agent_id: &str, _days: u32) -> AmanResult<Vec<MemoryRecord>> {
        Ok(Vec::new())
    }

    async fn store_procedural(
        &self,
        _agent_id: &str,
        _name: &str,
        _schema: &str,
        _kind: &str,
    ) -> AmanResult<String> {
        Ok(String::new())
    }

    async fn surface_procedural(
        &self,
        _agent_id: &str,
        _context: &str,
        _limit: usize,
    ) -> AmanResult<Vec<MemoryRecord>> {
        Ok(Vec::new())
    }

    async fn stats(&self, _agent_id: &str) -> AmanResult<MemoryStats> {
        let count = self
            .memories
            .read()
            .expect("memories lock")
            .values()
            .map(|v| v.len() as u64)
            .sum();
        Ok(MemoryStats {
            total_entries: count,
            index_size_bytes: 0,
            graph_nodes: 0,
            graph_edges: 0,
            pending_conflicts: 0,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kernel::memory::MemoryRetrieval;

    #[test]
    fn test_store_and_retrieve() {
        let store = MemoryStore::new();
        store.store("agent-1", "The user likes Python programming", vec!["language".to_owned()]);
        store.store("agent-1", "The user's favorite color is blue", vec!["preference".to_owned()]);
        store.store("agent-1", "Working on a Rust project", vec!["project".to_owned()]);

        let results = pollster::block_on(store.retrieve("agent-1", "What programming language do I like?"));
        assert!(!results.is_empty());
        assert!(results[0].content.contains("Python"));
    }

    #[test]
    fn test_no_results_for_unrelated_query() {
        let store = MemoryStore::new();
        store.store("agent-1", "The user likes Python programming", vec![]);

        let results = pollster::block_on(store.retrieve("agent-1", "weather in Tokyo"));
        assert!(results.is_empty());
    }

    #[test]
    fn test_max_results() {
        let store = MemoryStore::with_max_results(2);
        for i in 0..5 {
            store.store("agent-1", &format!("memory about topic {i}"), vec!["topic".to_owned()]);
        }

        let results = pollster::block_on(store.retrieve("agent-1", "topic memory about"));
        assert!(results.len() <= 2);
    }

    #[test]
    fn test_agent_namespacing() {
        let store = MemoryStore::new();
        store.store("agent-1", "Alice likes Python", vec![]);
        store.store("agent-2", "Bob likes Rust", vec![]);

        let a1 = pollster::block_on(store.retrieve("agent-1", "Python"));
        let a2 = pollster::block_on(store.retrieve("agent-2", "Python"));

        assert!(!a1.is_empty());
        assert!(a2.is_empty());
    }

    #[test]
    fn test_delete() {
        let store = MemoryStore::new();
        let id = store.store("agent-1", "test memory", vec![]);
        assert_eq!(store.list("agent-1").len(), 1);
        assert!(store.delete("agent-1", id));
        assert_eq!(store.list("agent-1").len(), 0);
    }

    #[test]
    fn test_tokenize() {
        let tokens = tokenize("Hello, World! This is a test.");
        assert!(tokens.contains("hello"));
        assert!(tokens.contains("world"));
        assert!(tokens.contains("this"));
        assert!(tokens.contains("test"));
        // Single-char words are filtered
        assert!(!tokens.contains("a"));
    }
}
