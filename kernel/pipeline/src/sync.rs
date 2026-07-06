// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! Built-in pipeline YAML syncing.
//!
//! Syncs built-in pipeline definitions (YAML) from the repo to the user's
//! local `~/.aman/pipelines/` directory. Tracks content hashes so user
//! modifications are detected and preserved across updates.
//!
//! Sync semantics (same as skill_sync):
//! - **New** (not in manifest) → created.
//! - **Unmodified** (user copy hash matches manifest) → overwritten with latest.
//! - **Modified** (user changed their copy) → preserved, logged at WARN level.

use std::collections::HashMap;
use std::path::Path;

/// Unified manifest file at `~/.aman/.manifest.json`.
///
/// Shared by skill, plugin, self, config, and pipeline sync. Each group maps
/// `rel_path` → blake3 hex hash of the last-synced built-in content.
#[derive(serde::Serialize, serde::Deserialize, Default)]
pub struct UnifiedManifest {
    #[serde(default)]
    version: u32,
    #[serde(default)]
    pub hashes: GroupedHashes,
}

/// Grouped hashes for different built-in asset types.
#[derive(serde::Serialize, serde::Deserialize, Default)]
pub struct GroupedHashes {
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub skills: HashMap<String, String>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub plugins: HashMap<String, String>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub configs: HashMap<String, String>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    #[serde(rename = "self")]
    pub self_files: HashMap<String, String>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub pipelines: HashMap<String, String>,
}

/// A single built-in pipeline YAML: (relative_path, content).
pub struct BuiltinPipeline {
    pub name: &'static str,
    /// (rel_path, content) — one entry per file.
    pub files: Vec<(&'static str, &'static str)>,
}

/// Built-in pipeline definitions. Embedded at compile time from `predefined/pipelines/`.
fn builtin_pipelines() -> Vec<BuiltinPipeline> {
    vec![BuiltinPipeline {
        name: "complex-plan",
        files: vec![(
            "01-complex-plan.yaml",
            include_str!("../../../predefined/pipelines/01-complex-plan.yaml"),
        )],
    }]
}

/// Compute blake3 hex hash of content.
pub fn content_hash(content: &str) -> String {
    blake3::hash(content.as_bytes()).to_hex().to_string()
}

/// Load the unified manifest, or return a default if the file doesn't exist.
pub fn load_unified_manifest(path: &Path) -> UnifiedManifest {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

/// Write the unified manifest to disk.
pub fn save_unified_manifest(path: &Path, manifest: &UnifiedManifest) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, serde_json::to_string_pretty(manifest)?)
}

/// Returns the aman user data directory (`~/.aman`).
pub fn aman_data_dir() -> std::path::PathBuf {
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .unwrap_or_else(|_| "/tmp".to_owned());
    std::path::PathBuf::from(home).join(".aman")
}

/// Sync built-in pipeline YAMLs to `~/.aman/pipelines/`.
pub fn sync_builtin_pipelines() -> Result<(), Box<dyn std::error::Error>> {
    sync_builtin_pipelines_to(&aman_data_dir())
}

/// Sync built-in pipeline YAMLs to a given data directory.
///
/// For each built-in pipeline file:
/// - **New** (not in manifest) → created.
/// - **Unmodified** (user copy hash matches manifest) → overwritten with latest.
/// - **Modified** (user changed their copy) → preserved, logged at WARN level.
pub fn sync_builtin_pipelines_to(data_dir: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let pipelines_dir = data_dir.join("pipelines");
    std::fs::create_dir_all(&pipelines_dir)?;

    let manifest_path = data_dir.join(".manifest.json");
    let mut manifest = load_unified_manifest(&manifest_path);

    let pipelines = builtin_pipelines();
    let mut new_hashes = HashMap::new();

    for pipeline in &pipelines {
        for &(rel_path, content) in &pipeline.files {
            let dest = pipelines_dir.join(rel_path);

            if let Some(parent) = dest.parent() {
                std::fs::create_dir_all(parent)?;
            }

            let user_modified = manifest
                .hashes
                .pipelines
                .get(rel_path)
                .map(|prev_hash| {
                    dest.exists()
                        && std::fs::read_to_string(&dest)
                            .ok()
                            .map(|current| content_hash(&current) != *prev_hash)
                            .unwrap_or(false)
                })
                .unwrap_or(false);

            if user_modified {
                tracing::warn!(
                    pipeline = %pipeline.name,
                    file = rel_path,
                    path = %dest.display(),
                    "built-in pipeline file has local modifications — preserving user changes"
                );
                new_hashes.insert(rel_path.to_owned(), content_hash(content));
                continue;
            }

            std::fs::write(&dest, content)?;
            tracing::info!(
                pipeline = %pipeline.name,
                file = rel_path,
                path = %dest.display(),
                "synced built-in pipeline file"
            );
            new_hashes.insert(rel_path.to_owned(), content_hash(content));
        }
    }

    manifest.hashes.pipelines = new_hashes;
    save_unified_manifest(&manifest_path, &manifest)?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_hash_is_deterministic() {
        let a = content_hash("hello");
        let b = content_hash("hello");
        assert_eq!(a, b);
    }

    #[test]
    fn sync_creates_pipelines_in_dir() {
        let tmp = std::env::temp_dir().join(format!("aman-pipeline-sync-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let pipelines_dir = tmp.join("pipelines");

        sync_builtin_pipelines_to(&tmp).expect("sync_to should succeed");

        for pipeline in builtin_pipelines() {
            for &(rel_path, _) in &pipeline.files {
                let path = pipelines_dir.join(rel_path);
                assert!(path.exists(), "{} should exist", path.display());
            }
        }

        let manifest = load_unified_manifest(&tmp.join(".manifest.json"));
        for pipeline in builtin_pipelines() {
            for &(rel_path, _) in &pipeline.files {
                assert!(
                    manifest.hashes.pipelines.contains_key(rel_path),
                    "manifest should contain hash for {}",
                    rel_path
                );
            }
        }

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn sync_preserves_user_modified_pipeline() {
        let tmp = std::env::temp_dir().join(format!("aman-pipeline-sync-preserve-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let pipelines_dir = tmp.join("pipelines");

        sync_builtin_pipelines_to(&tmp).expect("first sync");

        let target_path = pipelines_dir.join("01-complex-plan.yaml");
        let modified = "name: my-custom-pipeline\n# Customized by user\n";
        std::fs::write(&target_path, modified).expect("write modification");

        sync_builtin_pipelines_to(&tmp).expect("second sync");
        let after = std::fs::read_to_string(&target_path).unwrap();
        assert_eq!(after, modified, "user modification should be preserved");

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn sync_replaces_reverted_pipeline() {
        let tmp = std::env::temp_dir().join(format!("aman-pipeline-sync-revert-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let pipelines_dir = tmp.join("pipelines");

        let original_content = builtin_pipelines()[0].files[0].1;
        let target_path = pipelines_dir.join(builtin_pipelines()[0].files[0].0);

        sync_builtin_pipelines_to(&tmp).expect("first sync");
        std::fs::write(&target_path, "name: modified\n").expect("write modification");
        sync_builtin_pipelines_to(&tmp).expect("second sync preserves");
        std::fs::write(&target_path, original_content).expect("revert");
        sync_builtin_pipelines_to(&tmp).expect("third sync");

        let after = std::fs::read_to_string(&target_path).unwrap();
        assert_eq!(after, original_content);

        let _ = std::fs::remove_dir_all(&tmp);
    }
}
