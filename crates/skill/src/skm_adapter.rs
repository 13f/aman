//! Adapter that wraps skm-core's `SkillParser` for SKILL.md discovery and loading.
//!
//! Produces the same [`SkillInfo`] type so all callers remain unchanged.
//!
//! Phase 4 will upgrade this to use [`skm_core::SkillRegistry`] (async, with
//! filesystem watching and lazy loading) for cascade selection support.

use std::path::{Path, PathBuf};

use skm_core::SkillParser;

use crate::SkillInfo;

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// A SKILL.md discovery and loading engine backed by skm-core.
///
/// # Example
///
/// ```ignore
/// let registry = SkmRegistry::new(&skills_dir);
/// let skills = registry.discover();
/// let content = registry.load_content("ipo-research", &skills);
/// ```
pub struct SkmRegistry {
    root: PathBuf,
    parser: SkillParser,
}

impl SkmRegistry {
    /// Create a new registry rooted at `root` (typically `~/.aman/skills/`).
    #[must_use]
    pub fn new(root: &Path) -> Self {
        Self {
            root: root.to_owned(),
            parser: SkillParser::new(),
        }
    }

    /// Walk the root directory recursively and parse all `SKILL.md` files.
    ///
    /// Returns a `Vec<SkillInfo>` compatible with all existing callers.
    /// Silently skips files that fail to parse (logged via skm-core errors).
    pub fn discover(&self) -> Vec<SkillInfo> {
        let mut skills = Vec::new();
        collect_skills(&self.parser, &self.root, &mut skills);
        skills
    }

    /// Read the full `SKILL.md` content for a skill by name.
    ///
    /// Returns `None` if no skill with that name exists or the file is unreadable.
    #[must_use]
    pub fn load_content(&self, name: &str, known_skills: &[SkillInfo]) -> Option<String> {
        let path = known_skills.iter().find(|s| s.name == name)?.path.clone();
        std::fs::read_to_string(&path).ok()
    }

    /// Parse a single `SKILL.md` file at `path` into an `SkillInfo`.
    ///
    /// Returns `None` if the file is missing, has invalid frontmatter, or fails
    /// validation (name too long, empty description, etc.).
    #[must_use]
    pub fn parse_one(&self, path: &Path) -> Option<SkillInfo> {
        let meta = self.parser.parse_metadata(path).ok()?;
        // Extract aman-specific fields (category, array-form triggers) from raw YAML
        // since skm-core only exposes standard agentskills.io fields.
        let (category, triggers) = extract_raw_fields(path);
        Some(SkillInfo {
            name: meta.name.as_str().to_owned(),
            description: meta.description,
            category,
            triggers,
            path: meta.source_path,
        })
    }
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

fn collect_skills(parser: &SkillParser, dir: &Path, skills: &mut Vec<SkillInfo>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_skills(parser, &path, skills);
        } else if path.file_name().and_then(|n| n.to_str()) == Some("SKILL.md") {
            let (category, triggers) = extract_raw_fields(&path);
            if let Ok(meta) = parser.parse_metadata(&path) {
                skills.push(SkillInfo {
                    name: meta.name.as_str().to_owned(),
                    description: meta.description,
                    category,
                    triggers,
                    path: meta.source_path,
                });
            } else if let Some(skill) = fallback_parse_skill(&path, category, triggers) {
                // skm-core's RawFrontmatter uses HashMap<String, String> for metadata,
                // which can't parse nested map values (e.g., metadata.hermes.tags).
                // Fallback to flexible serde_yaml::Value parsing for these cases.
                skills.push(skill);
            }
        }
    }
}

/// Fallback parser for SKILL.md files with non-string metadata values.
///
/// skm-core's `RawFrontmatter::metadata` uses `HashMap<String, String>`, which
/// fails when metadata values are maps or arrays (e.g., `metadata.hermes.tags`).
/// This parser uses flexible `serde_yaml::Value`-based extraction to handle
/// those files without being strict about the metadata field type.
fn fallback_parse_skill(path: &Path, category: String, triggers: Vec<String>) -> Option<SkillInfo> {
    let content = std::fs::read_to_string(path).ok()?;

    // Extract YAML frontmatter between --- delimiters
    let content = content.trim_start();
    if !content.starts_with("---") {
        return None;
    }
    let end = content[3..].find("\n---")?;
    let yaml_str = &content[3..3 + end];

    let value: serde_yaml::Value = serde_yaml::from_str(yaml_str).ok()?;
    let mapping = value.as_mapping()?;

    let name = mapping
        .get(serde_yaml::Value::String("name".to_owned()))?
        .as_str()?
        .to_owned();

    let description = mapping
        .get(serde_yaml::Value::String("description".to_owned()))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_owned();

    Some(SkillInfo {
        name,
        description,
        category,
        triggers,
        path: path.to_owned(),
    })
}

/// Extract aman-specific frontmatter fields (category, array-form triggers)
/// that skm-core's standard schema doesn't support.
///
/// Checks both top-level `triggers` (aman format) and `metadata.triggers`
/// (agentskills.io standard format). Falls back from top-level to metadata
/// for compatibility with skm-core-using tools (e.g. cascade selector).
fn extract_raw_fields(path: &Path) -> (String, Vec<String>) {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return (String::new(), vec![]),
    };
    let content = content.trim_start();
    if !content.starts_with("---") {
        return (String::new(), vec![]);
    }
    let end = match content[3..].find("\n---") {
        Some(pos) => pos,
        None => return (String::new(), vec![]),
    };
    let yaml_str = &content[3..3 + end];
    let value: serde_yaml::Value = match serde_yaml::from_str(yaml_str) {
        Ok(v) => v,
        Err(_) => return (String::new(), vec![]),
    };
    let mapping = match value.as_mapping() {
        Some(m) => m,
        None => return (String::new(), vec![]),
    };

    let category = mapping
        .get(serde_yaml::Value::String("category".to_owned()))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_owned();

    // Try top-level `triggers` first (aman format: YAML array), fall back to
    // `metadata.triggers` (agentskills.io standard: comma-separated string).
    let triggers = extract_triggers_value(
        mapping.get(serde_yaml::Value::String("triggers".to_owned())),
    )
    .or_else(|| {
        mapping
            .get(serde_yaml::Value::String("metadata".to_owned()))
            .and_then(|v| v.as_mapping())
            .and_then(|meta| meta.get(serde_yaml::Value::String("triggers".to_owned())))
            .and_then(|v| match v {
                serde_yaml::Value::String(s) => Some(
                    s.split(',')
                        .map(|t| t.trim().to_owned())
                        .filter(|t| !t.is_empty())
                        .collect(),
                ),
                _ => None,
            })
    })
    .unwrap_or_default();

    (category, triggers)
}

/// Extract triggers from a YAML value (array of strings or comma-separated string).
fn extract_triggers_value(value: Option<&serde_yaml::Value>) -> Option<Vec<String>> {
    match value? {
        serde_yaml::Value::Sequence(seq) => Some(
            seq.iter()
                .filter_map(|item| item.as_str().map(|s| s.to_owned()))
                .collect(),
        ),
        serde_yaml::Value::String(s) => Some(
            s.split(',')
                .map(|t| t.trim().to_owned())
                .filter(|t| !t.is_empty())
                .collect(),
        ),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn create_skill_dir(dir: &Path, name: &str, description: &str) -> PathBuf {
        let skill_dir = dir.join(name);
        fs::create_dir_all(&skill_dir).unwrap();
        let content = format!(
            "---\nname: {name}\ndescription: \"{description}\"\n---\n\n# {name}\n\nInstructions here.\n"
        );
        let path = skill_dir.join("SKILL.md");
        fs::write(&path, &content).unwrap();
        path
    }

    fn create_categorized_skill_dir(
        dir: &Path,
        name: &str,
        description: &str,
        category: &str,
        triggers: &[&str],
    ) -> PathBuf {
        let skill_dir = dir.join(name);
        fs::create_dir_all(&skill_dir).unwrap();
        let trigger_list = triggers
            .iter()
            .map(|t| format!("  - \"{t}\""))
            .collect::<Vec<_>>()
            .join("\n");
        let content = format!(
            "---\nname: {name}\ndescription: \"{description}\"\ncategory: {category}\ntriggers:\n{trigger_list}\n---\n\n# {name}\n\nInstructions here.\n"
        );
        let path = skill_dir.join("SKILL.md");
        fs::write(&path, &content).unwrap();
        path
    }

    #[test]
    fn discover_finds_skills() {
        let tmp = std::env::temp_dir().join(format!("skm-test-discover-{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();

        create_skill_dir(&tmp, "test-skill", "A test skill");
        create_skill_dir(&tmp, "another-skill", "Another test skill");

        let registry = SkmRegistry::new(&tmp);
        let skills = registry.discover();
        assert_eq!(skills.len(), 2);

        let names: Vec<&str> = skills.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"test-skill"));
        assert!(names.contains(&"another-skill"));

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn discover_skips_invalid_files() {
        let tmp = std::env::temp_dir().join(format!("skm-test-invalid-{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();

        // Valid skill
        create_skill_dir(&tmp, "valid-skill", "Valid");

        // Invalid: no frontmatter
        let no_fm_dir = tmp.join("no-frontmatter");
        fs::create_dir_all(&no_fm_dir).unwrap();
        fs::write(no_fm_dir.join("SKILL.md"), "# Just markdown\n").unwrap();

        // Invalid: empty description (strict mode would reject, but non-strict allows it)
        let empty_dir = tmp.join("empty-desc");
        fs::create_dir_all(&empty_dir).unwrap();
        fs::write(
            empty_dir.join("SKILL.md"),
            "---\nname: empty-desc\ndescription: \"\"\n---\n\nBody\n",
        )
        .unwrap();

        let registry = SkmRegistry::new(&tmp);
        let skills = registry.discover();
        let names: Vec<&str> = skills.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"valid-skill"));
        // empty-desc has empty description but non-strict parser still accepts it
        assert!(names.contains(&"empty-desc"));
        // no-frontmatter should be skipped
        assert!(!names.contains(&"no-frontmatter"));

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn parse_one_returns_none_for_missing_file() {
        let tmp = std::env::temp_dir().join(format!("skm-test-missing-{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();

        let registry = SkmRegistry::new(&tmp);
        assert!(registry.parse_one(&tmp.join("nonexistent/SKILL.md")).is_none());
    }

    #[test]
    fn parse_one_returns_skill_for_valid_file() {
        let tmp = std::env::temp_dir().join(format!("skm-test-parseone-{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();

        let path = create_skill_dir(&tmp, "my-skill", "My skill description");

        let registry = SkmRegistry::new(&tmp);
        let skill = registry.parse_one(&path).expect("should parse valid skill");
        assert_eq!(skill.name, "my-skill");
        assert_eq!(skill.description, "My skill description");
        assert_eq!(skill.path, path);
        assert!(skill.category.is_empty());
        assert!(skill.triggers.is_empty());

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn parse_one_extracts_category_and_triggers() {
        let tmp = std::env::temp_dir().join(format!("skm-test-cat-{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();

        let path = create_categorized_skill_dir(
            &tmp,
            "ipo-research",
            "IPO research skill",
            "macro-investment-research",
            &["打新", "IPO", "新股"],
        );

        let registry = SkmRegistry::new(&tmp);
        let skill = registry.parse_one(&path).expect("should parse");
        assert_eq!(skill.name, "ipo-research");
        assert_eq!(skill.category, "macro-investment-research");
        assert!(skill.triggers.contains(&"打新".to_owned()));
        assert!(skill.triggers.contains(&"IPO".to_owned()));
        assert!(skill.triggers.contains(&"新股".to_owned()));

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn load_content_returns_skill_markdown() {
        let tmp = std::env::temp_dir().join(format!("skm-test-content-{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();

        create_skill_dir(&tmp, "content-skill", "Has content");

        let registry = SkmRegistry::new(&tmp);
        let skills = registry.discover();
        let content = registry.load_content("content-skill", &skills);
        assert!(content.is_some());
        assert!(content.unwrap().contains("Has content"));

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn discover_handles_nested_metadata_maps() {
        let tmp = std::env::temp_dir().join(format!("skm-test-nested-{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();

        // Skill with nested map in metadata (like metadata.hermes.tags)
        // skm-core's HashMap<String,String> rejects this, but fallback handles it.
        let skill_dir = tmp.join("nested-meta-skill");
        fs::create_dir_all(&skill_dir).unwrap();
        fs::write(
            skill_dir.join("SKILL.md"),
            r#"---
name: nested-meta-skill
description: "Skill with nested metadata"
metadata:
  hermes:
    tags: [RSS, Blogs]
    homepage: https://example.com
triggers: [blog, rss]
---

# Nested Metadata Skill

Instructions here.
"#,
        )
        .unwrap();

        // Also add a regular skill for comparison
        let skill_dir2 = tmp.join("regular-skill");
        fs::create_dir_all(&skill_dir2).unwrap();
        fs::write(
            skill_dir2.join("SKILL.md"),
            "---\nname: regular-skill\ndescription: \"Regular skill\"\n---\n\nBody\n",
        )
        .unwrap();

        let registry = SkmRegistry::new(&tmp);
        let skills = registry.discover();
        let names: Vec<&str> = skills.iter().map(|s| s.name.as_str()).collect();

        assert!(
            names.contains(&"nested-meta-skill"),
            "fallback should parse skills with nested metadata maps, found: {:?}",
            names
        );
        assert!(names.contains(&"regular-skill"), "regular skills still work");

        // Verify fallback parsed skill gets category and triggers from raw YAML
        let nested = skills.iter().find(|s| s.name == "nested-meta-skill").unwrap();
        assert!(nested.triggers.contains(&"blog".to_owned()));
        assert!(nested.triggers.contains(&"rss".to_owned()));

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn load_content_returns_none_for_unknown_skill() {
        let tmp = std::env::temp_dir().join(format!("skm-test-unknown-{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();

        let registry = SkmRegistry::new(&tmp);
        let content = registry.load_content("nonexistent", &[]);
        assert!(content.is_none());
    }
}
