// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

use kernel::react::ToolDescriptor;

/// A section of the assembled context window.
///
/// Each section has an independent token budget and eviction policy,
/// enabling the context manager to make fine-grained decisions about
/// what enters the model window.
#[derive(Debug, Clone)]
pub enum ContextSection {
    /// System prompt (SOUL) — highest priority, never evicted.
    System {
        content: String,
        /// Token count (estimated).
        token_count: usize,
    },
    /// Tool schema descriptions — high priority, rarely evicted.
    Tools {
        schemas: Vec<ToolDescriptor>,
        /// Rendered schema text.
        rendered: String,
        token_count: usize,
    },
    /// Current task description / goal — high priority.
    Task {
        description: String,
        token_count: usize,
    },
    /// Retrieved memory context — medium priority, can be trimmed.
    Memory {
        entries: Vec<MemorySectionEntry>,
        token_count: usize,
    },
    /// Conversation history — lowest priority, primary compression target.
    Conversation {
        message_count: usize,
        token_count: usize,
    },
}

/// A single memory entry in the context window.
#[derive(Debug, Clone)]
pub struct MemorySectionEntry {
    pub content: String,
    pub tags: Vec<String>,
    /// Token count for this entry.
    pub token_count: usize,
}

/// Per-section token budget allocation.
#[derive(Debug, Clone)]
pub struct SectionBudget {
    /// Maximum tokens for the system section.
    pub system_max: usize,
    /// Maximum tokens for the tools section.
    pub tools_max: usize,
    /// Maximum tokens for the task section.
    pub task_max: usize,
    /// Maximum tokens for the memory section.
    pub memory_max: usize,
    /// Maximum tokens for the conversation section (the remainder).
    pub conversation_max: usize,
}

impl Default for SectionBudget {
    fn default() -> Self {
        Self {
            system_max: 8_000,
            tools_max: 4_000,
            task_max: 1_000,
            memory_max: 3_000,
            conversation_max: 80_000,
        }
    }
}

/// An assembled context window ready for an LLM call.
///
/// Contains each section with its token count, enabling
/// the harness to understand exactly what's in the window.
#[derive(Debug, Clone)]
pub struct ContextWindow {
    pub sections: Vec<ContextSection>,
    /// Total estimated token count of all sections.
    pub total_tokens: usize,
    /// Maximum tokens available for this model.
    pub max_tokens: usize,
    /// Usage as a percentage (0–100).
    pub usage_percent: f64,
}

impl ContextWindow {
    /// Create an empty context window.
    pub fn new(max_tokens: usize) -> Self {
        Self {
            sections: Vec::new(),
            total_tokens: 0,
            max_tokens,
            usage_percent: 0.0,
        }
    }

    /// Add a section and recalculate totals.
    pub fn add_section(&mut self, section: ContextSection) {
        let tokens = section_token_count(&section);
        self.total_tokens += tokens;
        self.sections.push(section);
        self.usage_percent = if self.max_tokens > 0 {
            (self.total_tokens as f64 / self.max_tokens as f64) * 100.0
        } else {
            0.0
        };
    }

    /// Total sections in the window.
    pub fn section_count(&self) -> usize {
        self.sections.len()
    }
}

/// Get the token count for a context section.
fn section_token_count(section: &ContextSection) -> usize {
    match section {
        ContextSection::System { token_count, .. }
        | ContextSection::Tools { token_count, .. }
        | ContextSection::Task { token_count, .. }
        | ContextSection::Memory { token_count, .. }
        | ContextSection::Conversation { token_count, .. } => *token_count,
    }
}
