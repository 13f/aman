// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! Context loader — reads and caches shared documentation for team agents.
//!
//! Architecture ref: docs/team-architect.md §6.1 (context_files)

use crate::store::TeamStore;
use std::path::{Path, PathBuf};
use tokio::fs;
use tracing::{debug, warn};

/// Loads context files from disk and caches them in the TeamStore.
pub struct ContextLoader {
    work_dir: PathBuf,
    context_files: Vec<String>,
    store: TeamStore,
}

impl ContextLoader {
    /// Create a new context loader.
    pub fn new(work_dir: PathBuf, context_files: Vec<String>, store: TeamStore) -> Self {
        Self {
            work_dir,
            context_files,
            store,
        }
    }

    /// Load (or reload) all context files from disk into the store.
    ///
    /// Each file is read, categorized by path prefix, and upserted.
    pub async fn load_all(&self) -> Result<usize, String> {
        let mut loaded = 0;
        for file_rel in &self.context_files {
            let abs_path = self.work_dir.join(file_rel);
            match fs::read_to_string(&abs_path).await {
                Ok(content) => {
                    let title = extract_title(&abs_path, &content);
                    let category = categorize(file_rel);
                    self.store
                        .upsert_context(&title, file_rel, &content, category)
                        .map_err(|e| format!("upsert context '{file_rel}': {e}"))?;
                    loaded += 1;
                }
                Err(e) => {
                    warn!(
                        path = %abs_path.display(),
                        error = %e,
                        "ContextLoader: failed to read context file"
                    );
                }
            }
        }
        debug!(loaded, total = self.context_files.len(), "ContextLoader: load complete");
        Ok(loaded)
    }

    /// Get a formatted context string suitable for injection into an agent's
    /// system prompt (or work-item context).
    pub fn context_summary(&self) -> Result<String, String> {
        let entries = self.store.list_context(None)?;
        if entries.is_empty() {
            return Ok(String::new());
        }
        let mut summary = String::from("## Team Context Documents\n\n");
        for entry in &entries {
            summary.push_str(&format!(
                "### {}\n*Category: {} | Path: {}*\n\n{}\n\n---\n\n",
                entry.title,
                entry.category,
                entry.file_path,
                // Truncate to first 2000 chars for prompt injection
                &entry.content.chars().take(2000).collect::<String>()
            ));
        }
        Ok(summary)
    }
}

/// Extract a title from a markdown file (first `# heading`) or the filename.
fn extract_title(path: &Path, content: &str) -> String {
    for line in content.lines() {
        let trimmed = line.trim();
        if let Some(stripped) = trimmed.strip_prefix("# ") {
            return stripped.trim().to_string();
        }
    }
    // Fall back to filename stem
    path.file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("Untitled")
        .to_string()
}

/// Categorize a context file by its path prefix.
fn categorize(path: &str) -> &'static str {
    if path.contains("architecture") || path.contains("architect") {
        "architecture"
    } else if path.contains("standard") || path.contains("coding") || path.contains("style") {
        "standard"
    } else if path.contains("decision") || path.contains("adr") || path.contains("log") {
        "decision"
    } else {
        "general"
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn extract_title_from_heading() {
        let content = "# Architecture Overview\n\nThis is the doc.";
        assert_eq!(extract_title(Path::new("docs/arch.md"), content), "Architecture Overview");
    }

    #[test]
    fn extract_title_fallback_to_filename() {
        let content = "No heading here.";
        assert_eq!(extract_title(Path::new("docs/readme.md"), content), "readme");
    }

    #[test]
    fn categorize_by_path() {
        assert_eq!(categorize("docs/architecture.md"), "architecture");
        assert_eq!(categorize("docs/architect-design.md"), "architecture");
        assert_eq!(categorize("docs/coding-standards.md"), "standard");
        assert_eq!(categorize("docs/decision-log.md"), "decision");
        assert_eq!(categorize("docs/adr/001-use-rust.md"), "decision");
        assert_eq!(categorize("README.md"), "general");
    }

    #[tokio::test]
    async fn load_and_cache_context_files() {
        let dir = tempdir().unwrap();
        let work_dir = dir.path().to_path_buf();

        // Create test context files
        let docs_dir = work_dir.join("docs");
        std::fs::create_dir_all(&docs_dir).unwrap();
        std::fs::write(docs_dir.join("arch.md"), "# Architecture\n\nTest arch doc.").unwrap();
        std::fs::write(docs_dir.join("style.md"), "# Style Guide\n\nUse 4 spaces.").unwrap();

        let db_path = dir.path().join("test.db");
        let store = TeamStore::open(&db_path).unwrap();

        let loader = ContextLoader::new(
            work_dir,
            vec!["docs/arch.md".into(), "docs/style.md".into()],
            store,
        );

        let loaded = loader.load_all().await.unwrap();
        assert_eq!(loaded, 2);

        let summary = loader.context_summary().unwrap();
        assert!(summary.contains("Architecture"));
        assert!(summary.contains("Style Guide"));
    }
}
