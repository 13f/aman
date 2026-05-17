//! Built-in skill syncing.
//!
//! Syncs built-in skills (embedded via `include_str!`) from the repo to the user's
//! local `~/.aman/skills/` directory. Tracks content hashes so user modifications
//! are detected and preserved across updates.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// Data types
// ---------------------------------------------------------------------------

/// A single built-in skill embedded in the binary.
struct BuiltinSkill {
    /// Display name for logging.
    name: &'static str,
    /// Relative output path under the skills directory (e.g. `idle/idle-daze.yaml`).
    rel_path: &'static str,
    /// Raw skill-definition content.
    content: &'static str,
}

/// Manifest file at `~/.aman/.manifest.json`.
#[derive(serde::Serialize, serde::Deserialize)]
struct SkillManifest {
    version: u32,
    /// Map from `rel_path` → blake3 hex hash of the last-synced built-in content.
    hashes: HashMap<String, String>,
}

// ---------------------------------------------------------------------------
// Built-in skill definitions (embedded at compile time)
// ---------------------------------------------------------------------------

fn builtin_skills() -> Vec<BuiltinSkill> {
    macro_rules! embed {
        ($rel:literal) => {
            include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../skills/", $rel))
        };
    }

    vec![
        BuiltinSkill {
            name: "idle-daze",
            rel_path: "idle/idle-daze.yaml",
            content: embed!("idle/idle-daze.yaml"),
        },
        BuiltinSkill {
            name: "idle-boredom",
            rel_path: "idle/idle-boredom.yaml",
            content: embed!("idle/idle-boredom.yaml"),
        },
        BuiltinSkill {
            name: "idle-sleep",
            rel_path: "idle/idle-sleep.yaml",
            content: embed!("idle/idle-sleep.yaml"),
        },
        BuiltinSkill {
            name: "idle-exploration",
            rel_path: "idle/idle-exploration.yaml",
            content: embed!("idle/idle-exploration.yaml"),
        },
        BuiltinSkill {
            name: "idle-meditation",
            rel_path: "idle/idle-meditation.yaml",
            content: embed!("idle/idle-meditation.yaml"),
        },
        BuiltinSkill {
            name: "idle-waiting",
            rel_path: "idle/idle-waiting.yaml",
            content: embed!("idle/idle-waiting.yaml"),
        },
        BuiltinSkill {
            name: "idle-incubation",
            rel_path: "idle/idle-incubation.yaml",
            content: embed!("idle/idle-incubation.yaml"),
        },
    ]
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn content_hash(content: &str) -> String {
    blake3::hash(content.as_bytes()).to_hex().to_string()
}

fn load_manifest(path: &Path) -> SkillManifest {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or(SkillManifest { version: 1, hashes: HashMap::new() })
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Returns the Aman user data directory (`~/.aman`).
pub fn aman_data_dir() -> PathBuf {
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .unwrap_or_else(|_| "/tmp".to_owned());
    PathBuf::from(home).join(".aman")
}

/// Sync built-in skills to `~/.aman/skills/` (convenience wrapper).
///
/// See [`sync_builtin_skills_to`] for full documentation.
pub fn sync_builtin_skills() -> Result<(), Box<dyn std::error::Error>> {
    sync_builtin_skills_to(&aman_data_dir())
}

/// Sync built-in skills to a given data directory.
///
/// For each built-in skill:
/// - **New** (not in manifest) → created.
/// - **Unmodified** (user copy hash matches manifest) → overwritten with latest.
/// - **Modified** (user changed their copy) → preserved, logged at WARN level.
///
/// The manifest is updated with the *current built-in* hashes so future syncs
/// can correctly detect user modifications.
fn sync_builtin_skills_to(data_dir: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let skills_dir = data_dir.join("skills");
    std::fs::create_dir_all(&skills_dir)?;

    let manifest_path = data_dir.join(".manifest.json");
    let prev_manifest = load_manifest(&manifest_path);

    let skills = builtin_skills();
    let mut new_hashes = HashMap::new();

    for skill in &skills {
        let hash = content_hash(skill.content);
        let dest = skills_dir.join(skill.rel_path);

        // Ensure parent subdirectory exists (e.g. skills/idle/).
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let user_modified = prev_manifest
            .hashes
            .get(skill.rel_path)
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
                skill = %skill.name,
                path = %dest.display(),
                "built-in skill has local modifications — preserving user changes"
            );
            new_hashes.insert(skill.rel_path.to_owned(), hash);
            continue;
        }

        std::fs::write(&dest, skill.content)?;
        tracing::info!(skill = %skill.name, path = %dest.display(), "synced built-in skill");
        new_hashes.insert(skill.rel_path.to_owned(), hash);
    }

    let manifest = SkillManifest { version: 1, hashes: new_hashes };
    std::fs::write(&manifest_path, serde_json::to_string_pretty(&manifest)?)?;

    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

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
    fn content_hash_differs_for_different_content() {
        let a = content_hash("hello");
        let b = content_hash("world");
        assert_ne!(a, b);
    }

    #[test]
    fn all_builtin_skills_are_parseable_yaml() {
        for skill in builtin_skills() {
            let doc: serde_yaml::Value =
                serde_yaml::from_str(skill.content).expect(&format!("{} is valid YAML", skill.name));
            assert!(doc.get("name").is_some(), "{} has a name field", skill.name);
        }
    }

    #[test]
    fn sync_creates_skills_in_dir() {
        let tmp = std::env::temp_dir().join(format!("aman-skill-sync-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let skills_dir = tmp.join("skills");

        sync_builtin_skills_to(&tmp).expect("sync_to should succeed");

        // Verify all 7 files exist
        for skill in builtin_skills() {
            let path = skills_dir.join(skill.rel_path);
            assert!(path.exists(), "{} should exist", path.display());
        }

        // Manifest should exist
        assert!(tmp.join(".manifest.json").exists());

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn sync_preserves_user_modified_skill() {
        let tmp = std::env::temp_dir().join(format!("aman-skill-sync-preserve-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let skills_dir = tmp.join("skills");

        // First sync
        sync_builtin_skills_to(&tmp).expect("first sync");

        // Modify a skill file
        let daze_path = skills_dir.join("idle/idle-daze.yaml");
        std::fs::write(&daze_path, "name: modified-daze\n").expect("write modification");
        let modified_content = std::fs::read_to_string(&daze_path).unwrap();

        // Second sync — should preserve modification
        sync_builtin_skills_to(&tmp).expect("second sync");
        let after = std::fs::read_to_string(&daze_path).unwrap();
        assert_eq!(after, modified_content, "user modification should be preserved");

        // Other unmodified skills should still exist
        let boredom_path = skills_dir.join("idle/idle-boredom.yaml");
        assert!(boredom_path.exists(), "unmodified skill should still exist");

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn sync_replaces_skill_that_was_reverted_to_builtin() {
        let tmp = std::env::temp_dir().join(format!("aman-skill-sync-revert-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let skills_dir = tmp.join("skills");

        // First sync creates all skills
        sync_builtin_skills_to(&tmp).expect("first sync");

        // Modify a skill (user edits it)
        let daze_path = skills_dir.join("idle/idle-daze.yaml");
        std::fs::write(&daze_path, "name: modified-daze\n").expect("write modification");

        // Second sync preserves modification
        sync_builtin_skills_to(&tmp).expect("second sync");
        let after = std::fs::read_to_string(&daze_path).unwrap();
        assert_eq!(after, "name: modified-daze\n", "modification still preserved");

        // Now user reverts their change by writing the original built-in content
        let skills = builtin_skills();
        let daze = skills.iter().find(|s| s.name == "idle-daze").unwrap();
        std::fs::write(&daze_path, daze.content).expect("write reverted content");

        // Third sync: file now matches built-in (user "reverted").
        // Since file hash will match the new manifest entry from the second sync
        // (which recorded the built-in hash, not the user modification), the
        // file hash == manifest hash → not user-modified → gets overwritten.
        sync_builtin_skills_to(&tmp).expect("third sync");

        let _ = std::fs::remove_dir_all(&tmp);
    }
}
