// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! Export skills to a spec-compliant directory tree consumable by Claude Code,
//! Cursor, Codex, or any agent that follows the agentskills.io specification.
//!
//! Output structure:
//!
//! ```text
//! ./out/
//! ├── <skill-name>/
//! │   └── SKILL.md
//! └── ...
//! ```

use std::path::Path;

use crate::skm_adapter::SkmRegistry;
use crate::SkillInfo;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// Report of an export operation.
#[derive(Debug, Clone, Default)]
pub struct ExportReport {
    pub exported: Vec<String>,
    pub skipped: Vec<(String, String)>,
    pub errors: Vec<(String, String)>,
}

impl ExportReport {
    pub fn is_ok(&self) -> bool {
        self.errors.is_empty()
    }
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Export all skills under `skills_root` to `out_dir`.
///
/// Each skill gets its own subdirectory named after the skill:
/// `{out_dir}/{skill-name}/SKILL.md`
pub fn export_all(skills_root: &Path, out_dir: &Path) -> ExportReport {
    let registry = SkmRegistry::new(skills_root);
    let skills = registry.discover();
    export_skills(&skills, out_dir)
}

/// Export a specific list of skills to `out_dir`.
///
/// Each skill is written as `{out_dir}/{skill.name}/SKILL.md`.
/// Silently skips skills whose source file is missing.
pub fn export_skills(skills: &[SkillInfo], out_dir: &Path) -> ExportReport {
    let mut report = ExportReport::default();

    for skill in skills {
        let dest_dir = out_dir.join(&skill.name);
        let dest = dest_dir.join("SKILL.md");

        // Read the source content
        let content = match std::fs::read_to_string(&skill.path) {
            Ok(c) => c,
            Err(e) => {
                report.errors.push((
                    skill.name.clone(),
                    format!("failed to read {}: {e}", skill.path.display()),
                ));
                continue;
            }
        };

        // Create destination directory
        if let Err(e) = std::fs::create_dir_all(&dest_dir) {
            report.errors.push((
                skill.name.clone(),
                format!("failed to create directory {}: {e}", dest_dir.display()),
            ));
            continue;
        }

        // Write SKILL.md
        if let Err(e) = std::fs::write(&dest, &content) {
            report.errors.push((
                skill.name.clone(),
                format!("failed to write {}: {e}", dest.display()),
            ));
            continue;
        }

        report.exported.push(skill.name.clone());
    }

    report
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn create_test_skill(dir: &Path, name: &str) -> SkillInfo {
        let skill_dir = dir.join(name);
        fs::create_dir_all(&skill_dir).unwrap();
        let content = format!(
            "---\nname: {name}\ndescription: \"{name} description\"\n---\n\n# {name}\n\nBody here.\n"
        );
        let path = skill_dir.join("SKILL.md");
        fs::write(&path, &content).unwrap();
        SkillInfo {
            name: name.to_owned(),
            description: format!("{name} description"),
            category: String::new(),
            triggers: vec![],
            react_mode: ReactMode::default(),
            path,
        }
    }

    #[test]
    fn export_skills_creates_directory_structure() {
        let tmp = std::env::temp_dir().join(format!("export-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();

        let out = tmp.join("out");
        let src = tmp.join("src");
        fs::create_dir_all(&src).unwrap();

        let skills = vec![
            create_test_skill(&src, "skill-alpha"),
            create_test_skill(&src, "skill-beta"),
        ];

        let report = export_skills(&skills, &out);

        assert_eq!(report.exported.len(), 2);
        assert!(report.is_ok());

        // Verify file contents
        let alpha_content = fs::read_to_string(out.join("skill-alpha/SKILL.md")).unwrap();
        assert!(alpha_content.contains("skill-alpha"));

        let beta_content = fs::read_to_string(out.join("skill-beta/SKILL.md")).unwrap();
        assert!(beta_content.contains("skill-beta"));

        // Verify structure: each skill in its own directory
        assert!(out.join("skill-alpha").is_dir());
        assert!(out.join("skill-beta").is_dir());

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn export_skills_reports_missing_source() {
        let tmp = std::env::temp_dir().join(format!("export-test-missing-{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();

        let out = tmp.join("out");
        let skills = vec![SkillInfo {
            name: "ghost".to_owned(),
            description: "missing".to_owned(),
            category: String::new(),
            triggers: vec![],
            react_mode: ReactMode::default(),
            path: tmp.join("nonexistent/SKILL.md"),
        }];

        let report = export_skills(&skills, &out);
        assert_eq!(report.exported.len(), 0);
        assert_eq!(report.errors.len(), 1);
    }

    #[test]
    fn export_all_discovers_and_exports() {
        let tmp = std::env::temp_dir().join(format!("export-all-{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();

        let out = tmp.join("out");
        let src = tmp.join("src");
        fs::create_dir_all(&src).unwrap();

        create_test_skill(&src.join("alpha-dir"), "alpha");
        create_test_skill(&src.join("beta-dir"), "beta");

        let report = export_all(&src, &out);
        assert_eq!(report.exported.len(), 2);
        assert!(out.join("alpha/SKILL.md").exists());
        assert!(out.join("beta/SKILL.md").exists());

        let _ = fs::remove_dir_all(&tmp);
    }
}
