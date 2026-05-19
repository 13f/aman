use std::collections::{HashMap, HashSet};
use std::sync::RwLock;
use std::time::{SystemTime, UNIX_EPOCH};

/// A single memory entry.
#[derive(Debug, Clone)]
pub struct MemoryEntry {
    pub id: u64,
    pub content: String,
    pub tags: Vec<String>,
    pub created_at_ms: u64,
}

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

    /// Store a memory entry for the given agent.
    pub fn store(&self, agent_id: &str, content: &str, tags: Vec<String>) -> u64 {
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

    /// Retrieve memories relevant to the query for the given agent.
    ///
    /// Uses simple keyword overlap scoring:
    /// 1. Tokenize query and each memory into words
    /// 2. Score by number of common keywords
    /// 3. Return top-N results
    pub fn retrieve(&self, agent_id: &str, query: &str) -> Vec<MemoryEntry> {
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

        // Sort by score descending, then by newest first
        scored.sort_by(|a, b| {
            b.0.cmp(&a.0)
                .then_with(|| b.1.created_at_ms.cmp(&a.1.created_at_ms))
        });

        // Filter out zero-score results, take top N
        scored
            .into_iter()
            .filter(|(score, _)| *score > 0)
            .take(self.max_results)
            .map(|(_, entry)| entry.clone())
            .collect()
    }

    /// List all memories for an agent.
    pub fn list(&self, agent_id: &str) -> Vec<MemoryEntry> {
        let memories = self.memories.read().expect("memories lock");
        memories
            .get(agent_id)
            .cloned()
            .unwrap_or_default()
    }

    /// Delete a memory entry by ID.
    pub fn delete(&self, agent_id: &str, entry_id: u64) -> bool {
        let mut memories = self.memories.write().expect("memories lock");
        if let Some(entries) = memories.get_mut(agent_id) {
            let before = entries.len();
            entries.retain(|e| e.id != entry_id);
            entries.len() < before
        } else {
            false
        }
    }

    /// Get the configured max results.
    pub fn max_results(&self) -> usize {
        self.max_results
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_store_and_retrieve() {
        let store = MemoryStore::new();
        store.store("agent-1", "The user likes Python programming", vec!["language".to_owned()]);
        store.store("agent-1", "The user's favorite color is blue", vec!["preference".to_owned()]);
        store.store("agent-1", "Working on a Rust project", vec!["project".to_owned()]);

        let results = store.retrieve("agent-1", "What programming language do I like?");
        assert!(!results.is_empty());
        assert!(results[0].content.contains("Python"));
    }

    #[test]
    fn test_no_results_for_unrelated_query() {
        let store = MemoryStore::new();
        store.store("agent-1", "The user likes Python programming", vec![]);

        let results = store.retrieve("agent-1", "weather in Tokyo");
        assert!(results.is_empty());
    }

    #[test]
    fn test_max_results() {
        let store = MemoryStore::with_max_results(2);
        for i in 0..5 {
            store.store("agent-1", &format!("memory about topic {i}"), vec!["topic".to_owned()]);
        }

        let results = store.retrieve("agent-1", "topic memory about");
        assert!(results.len() <= 2);
    }

    #[test]
    fn test_agent_namespacing() {
        let store = MemoryStore::new();
        store.store("agent-1", "Alice likes Python", vec![]);
        store.store("agent-2", "Bob likes Rust", vec![]);

        let a1 = store.retrieve("agent-1", "Python");
        let a2 = store.retrieve("agent-2", "Python");

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
