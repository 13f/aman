// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

use std::collections::HashMap;
use std::sync::Mutex;

/// Per-session system prompt cache.
///
/// The combined soul + skills prompt is built once on the first turn of a session
/// and reused verbatim on subsequent turns so that LLM prompt caching
/// (Anthropic prefix cache, OpenAI prompt caching) stays effective.
pub struct SystemPromptCache {
    cache: Mutex<HashMap<String, String>>,
}

impl SystemPromptCache {
    pub fn new() -> Self {
        Self {
            cache: Mutex::new(HashMap::new()),
        }
    }

    /// Get the cached prompt for `session_id`, or build and cache it via `build_fn`.
    pub fn get_or_build(
        &self,
        session_id: &str,
        build_fn: impl FnOnce() -> String,
    ) -> String {
        let mut cache = self.cache.lock().unwrap();
        if let Some(cached) = cache.get(session_id) {
            cached.clone()
        } else {
            let prompt = build_fn();
            cache.insert(session_id.to_owned(), prompt.clone());
            prompt
        }
    }

    /// Remove a cached prompt (e.g. when a session is deleted).
    pub fn invalidate(&self, session_id: &str) {
        let mut cache = self.cache.lock().unwrap();
        cache.remove(session_id);
    }
}

impl Default for SystemPromptCache {
    fn default() -> Self {
        Self::new()
    }
}
