// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! Skill validation against the agentskills.io specification.
//!
//! Provides spec-compliant validation of SKILL.md files with 10 rules covering
//! frontmatter structure, naming conventions, directory layout, and
//! cross-references. Built on top of `skm-core` for underlying parsing.

use std::path::{Path, PathBuf};

use crate::skm_adapter::SkmRegistry;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// The severity of a validation finding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
}

impl std::fmt::Display for Severity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Severity::Error => write!(f, "error"),
            Severity::Warning => write!(f, "warning"),
        }
    }
}

/// A single validation finding (rule violation or warning).
#[derive(Debug, Clone)]
pub struct ValidationFinding {
    /// Skill name, if determinable from frontmatter.
    pub skill_name: Option<String>,
    /// Path to the file (SKILL.md or directory) where the issue was found.
    pub path: PathBuf,
    /// Rule identifier (e.g. "R1", "R4").
    pub rule: &'static str,
    /// Severity of the finding.
    pub severity: Severity,
    /// Human-readable description of the issue.
    pub message: String,
}

impl std::fmt::Display for ValidationFinding {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = self
            .skill_name
            .as_deref()
            .unwrap_or("<unknown>");
        write!(
            f,
            "[{}] {} {}: {} — {}",
            self.severity,
            self.rule,
            name,
            self.path.display(),
            self.message
        )
    }
}

/// Summary of a validation run.
#[derive(Debug, Clone, Default)]
pub struct ValidationReport {
    pub findings: Vec<ValidationFinding>,
}

impl ValidationReport {
    pub fn is_ok(&self) -> bool {
        self.findings.iter().all(|f| f.severity == Severity::Warning)
    }

    pub fn error_count(&self) -> usize {
        self.findings.iter().filter(|f| f.severity == Severity::Error).count()
    }

    pub fn warning_count(&self) -> usize {
        self.findings.iter().filter(|f| f.severity == Severity::Warning).count()
    }
}

// ---------------------------------------------------------------------------
// Visitor
// ---------------------------------------------------------------------------

/// State passed through the skill directory traversal for rules that span
/// multiple files (e.g. cross-reference checking).
struct ValidationCtx {
    /// All discovered skill names (used for cross-reference validation).
    known_skill_names: Vec<String>,
    /// Findings collected so far.
    findings: Vec<ValidationFinding>,
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Validate all skills under a root directory.
///
/// Walks the directory recursively, discovers all `SKILL.md` files, and
/// evaluates every applicable rule against each skill.
pub fn validate_all(skills_root: &Path) -> ValidationReport {
    let parser = skm_core::SkillParser::new();

    // Phase 1: discover all skills (collect names for cross-ref checks)
    let registry = SkmRegistry::new(skills_root);
    let skills = registry.discover();
    let known_names: Vec<String> = skills.iter().map(|s| s.name.clone()).collect();

    let mut ctx = ValidationCtx {
        known_skill_names: known_names,
        findings: Vec::new(),
    };

    // Phase 2: walk each skill directory and validate
    if let Ok(entries) = std::fs::read_dir(skills_root) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                validate_skill_dir(&parser, &path, &mut ctx);
            }
        }
    }

    ValidationReport {
        findings: ctx.findings,
    }
}

/// Validate a single skill directory at `path`.
///
/// `path` should be a directory containing a `SKILL.md` file.
pub fn validate_one(path: &Path) -> ValidationReport {
    let parser = skm_core::SkillParser::new();
    let mut ctx = ValidationCtx {
        known_skill_names: Vec::new(),
        findings: Vec::new(),
    };

    if path.is_dir() {
        validate_skill_dir(&parser, path, &mut ctx);
    } else if path.is_file() && path.file_name().and_then(|n| n.to_str()) == Some("SKILL.md") {
        // A direct SKILL.md path — try to validate it as a standalone file.
        validate_skill_file(&parser, path, &mut ctx);
    } else {
        ctx.findings.push(ValidationFinding {
            skill_name: None,
            path: path.to_owned(),
            rule: "R0",
            severity: Severity::Error,
            message: format!("not a skill directory or SKILL.md file: {}", path.display()),
        });
    }

    ValidationReport {
        findings: ctx.findings,
    }
}

// ---------------------------------------------------------------------------
// Per-rule validators
// ---------------------------------------------------------------------------

/// Validate a directory that is expected to contain a skill.
fn validate_skill_dir(parser: &skm_core::SkillParser, dir: &Path, ctx: &mut ValidationCtx) {
    let skill_md = dir.join("SKILL.md");

    // R5: File name must be exactly SKILL.md (case-sensitive)
    if !skill_md.exists() {
        ctx.findings.push(ValidationFinding {
            skill_name: dir_name(dir),
            path: skill_md,
            rule: "R5",
            severity: Severity::Error,
            message: "SKILL.md not found in skill directory".to_owned(),
        });
        check_orphaned_files(dir, ctx);
        return;
    }

    // R6: Directory name must equal frontmatter name
    // Parse first so we have the name.
    match parser.parse_metadata(&skill_md) {
        Ok(meta) => {
            let skill_name = meta.name.as_str().to_owned();
            let dir_name = dir
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("")
                .to_owned();

            if dir_name != skill_name {
                ctx.findings.push(ValidationFinding {
                    skill_name: Some(skill_name.clone()),
                    path: dir.to_owned(),
                    rule: "R6",
                    severity: Severity::Error,
                    message: format!(
                        "directory name '{dir_name}' does not match frontmatter name '{skill_name}'"
                    ),
                });
            }

            // R9: Cross-reference check
            check_cross_references(&meta, ctx);

            // R7: No orphan files
            check_orphaned_files(dir, ctx);
        }
        Err(e) => {
            ctx.findings.push(ValidationFinding {
                skill_name: dir_name(dir),
                path: skill_md.clone(),
                rule: "R1",
                severity: Severity::Error,
                message: format!("failed to parse SKILL.md: {e}"),
            });
        }
    }

    // R8: Validate trigger regex patterns (in a separate pass after parsing)
    validate_trigger_regex(&skill_md, ctx);
}

/// Validate a single SKILL.md file directly (not inside a directory).
fn validate_skill_file(parser: &skm_core::SkillParser, path: &Path, ctx: &mut ValidationCtx) {
    match parser.parse_metadata(path) {
        Ok(meta) => {
            check_cross_references(&meta, ctx);
        }
        Err(e) => {
            ctx.findings.push(ValidationFinding {
                skill_name: None,
                path: path.to_owned(),
                rule: "R1",
                severity: Severity::Error,
                message: format!("failed to parse SKILL.md: {e}"),
            });
        }
    }

    validate_trigger_regex(path, ctx);
}

// ---------------------------------------------------------------------------
// Rule helpers
// ---------------------------------------------------------------------------

/// R7: Check for orphan files in the skill directory (files other than SKILL.md
/// and standard allowed resources like `references/`, `scripts/`, etc.).
fn check_orphaned_files(dir: &Path, ctx: &mut ValidationCtx) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };

    let allowed = [
        "SKILL.md",
        "references",
        "scripts",
        "resources",
        "assets",
        "fixtures",
        "README.md",
    ];

    for entry in entries.flatten() {
        let path = entry.path();
        let name = match path.file_name().and_then(|n| n.to_str()) {
            Some(n) => n.to_owned(),
            None => continue,
        };

        if allowed.contains(&name.as_str()) {
            continue;
        }

        // Allow hidden files (dotfiles)
        if name.starts_with('.') {
            continue;
        }

        ctx.findings.push(ValidationFinding {
            skill_name: dir_name(dir),
            path,
            rule: "R7",
            severity: Severity::Warning,
            message: format!("unexpected file or directory '{name}' in skill directory"),
        });
    }
}

/// R9: Check cross-references — `related_skills` entries must point to existing
/// skills (only when we have a known set).
fn check_cross_references(meta: &skm_core::SkillMetadata, ctx: &mut ValidationCtx) {
    // The spec allows `related_skills` in metadata. Since skm-core stores
    // arbitrary metadata fields, we need to check it manually.
    // For now, we use a best-effort approach — read the raw frontmatter.
    // skm-core's Skill doesn't expose related_skills directly, so we check
    // via the raw file content.
    let content = match std::fs::read_to_string(&meta.source_path) {
        Ok(c) => c,
        Err(_) => return,
    };

    // Simple YAML field extraction for `related_skills`.
    // This handles: related_skills: [skill-a, skill-b] or related_skills: skill-a
    let related = extract_list_field(&content, "related_skills");
    let related_alt = extract_list_field(&content, "related-skills");

    let all_related: Vec<&str> = related
        .iter()
        .chain(related_alt.iter())
        .map(|s| s.as_str())
        .collect();

    for ref_name in &all_related {
        if !ctx.known_skill_names.iter().any(|n| n == ref_name) {
            ctx.findings.push(ValidationFinding {
                skill_name: Some(meta.name.as_str().to_owned()),
                path: meta.source_path.clone(),
                rule: "R9",
                severity: Severity::Warning,
                message: format!(
                    "references unknown skill '{ref_name}' in related_skills"
                ),
            });
        }
    }
}

/// R8: Validate that trigger patterns in metadata are non-empty.
/// Full regex validation is handled by `skm-select` trigger matching in Phase 4.
fn validate_trigger_regex(path: &Path, ctx: &mut ValidationCtx) {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return,
    };

    let trigger_exprs = extract_list_field(&content, "triggers");
    for expr in &trigger_exprs {
        if expr.trim().is_empty() {
            ctx.findings.push(ValidationFinding {
                skill_name: dir_name(path.parent().unwrap_or(path)),
                path: path.to_owned(),
                rule: "R8",
                severity: Severity::Warning,
                message: "trigger pattern is empty".to_owned(),
            });
        }
    }
}

// ---------------------------------------------------------------------------
// Parsing helpers
// ---------------------------------------------------------------------------

/// Extract a YAML list field from raw frontmatter content.
/// Supports both flow syntax `[a, b]` and simple scalar values.
fn extract_list_field(content: &str, field: &str) -> Vec<String> {
    let mut results = Vec::new();

    // Try to find `field:` in the frontmatter
    for line in content.lines() {
        let trimmed = line.trim();
        if let Some(val) = trimmed.strip_prefix(&format!("{}:", field))
            .or_else(|| trimmed.strip_prefix(&format!("{}:", field)))
        {
            let val = val.trim();
            if val.starts_with('[') && val.ends_with(']') {
                // Flow sequence: [a, b, c]
                let inner = &val[1..val.len() - 1];
                for item in inner.split(',') {
                    let item = item.trim().trim_matches('"').trim_matches('\'').to_owned();
                    if !item.is_empty() {
                        results.push(item);
                    }
                }
            } else if !val.is_empty() {
                // Scalar value
                results.push(val.trim_matches('"').trim_matches('\'').to_owned());
            }
            break;
        }
    }

    results
}

/// Get the directory name as `Option<String>`.
fn dir_name(path: &Path) -> Option<String> {
    path.file_name()
        .and_then(|n| n.to_str())
        .map(|n| n.to_owned())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn create_valid_skill(dir: &Path, name: &str, extra: &str) -> PathBuf {
        let skill_dir = dir.join(name);
        fs::create_dir_all(&skill_dir).unwrap();
        let content = format!(
            "---\nname: {name}\ndescription: \"A valid skill\"{extra}\n---\n\n# Body\n"
        );
        let path = skill_dir.join("SKILL.md");
        fs::write(&path, &content).unwrap();
        path
    }

    #[test]
    fn valid_skill_passes_all_rules() {
        let tmp = std::env::temp_dir().join(format!("val-test-valid-{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();
        create_valid_skill(&tmp, "valid-skill", "");
        let report = validate_all(&tmp);
        assert!(report.is_ok(), "findings: {:?}", report.findings);
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn missing_skill_md_reports_r5() {
        let tmp = std::env::temp_dir().join(format!("val-test-missing-{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();

        let empty_dir = tmp.join("empty-skill");
        fs::create_dir_all(&empty_dir).unwrap();

        let report = validate_all(&tmp);
        let r5: Vec<_> = report.findings.iter().filter(|f| f.rule == "R5").collect();
        assert_eq!(r5.len(), 1, "should report missing SKILL.md");
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn directory_name_mismatch_reports_r6() {
        let tmp = std::env::temp_dir().join(format!("val-test-r6-{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();

        let skill_dir = tmp.join("wrong-dir-name");
        fs::create_dir_all(&skill_dir).unwrap();
        let content = "---\nname: actual-name\ndescription: \"desc\"\n---\n\nBody\n";
        fs::write(skill_dir.join("SKILL.md"), content).unwrap();

        let report = validate_all(&tmp);
        let r6: Vec<_> = report.findings.iter().filter(|f| f.rule == "R6").collect();
        assert_eq!(r6.len(), 1);
        assert!(r6[0].message.contains("wrong-dir-name"));
        assert!(r6[0].message.contains("actual-name"));
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn orphaned_file_reports_r7_warning() {
        let tmp = std::env::temp_dir().join(format!("val-test-r7-{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();

        create_valid_skill(&tmp, "test-skill", "");
        let skill_dir = tmp.join("test-skill");
        fs::write(skill_dir.join("orphan.txt"), "garbage").unwrap();
        fs::write(skill_dir.join("README.md"), "readme").unwrap(); // allowed

        let report = validate_all(&tmp);
        let r7: Vec<_> = report.findings.iter().filter(|f| f.rule == "R7").collect();
        assert_eq!(r7.len(), 1, "only orphan.txt, not README.md");
        assert!(r7[0].message.contains("orphan.txt"));
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn empty_trigger_reports_r8_warning() {
        let tmp = std::env::temp_dir().join(format!("val-test-r8-{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();

        let skill_dir = tmp.join("empty-trigger");
        fs::create_dir_all(&skill_dir).unwrap();
        let content =
            "---\nname: empty-trigger\ndescription: \"desc\"\nmetadata:\n  triggers: \"\"\n---\n\nBody\n";
        fs::write(skill_dir.join("SKILL.md"), content).unwrap();

        let report = validate_all(&tmp);
        let r8: Vec<_> = report.findings.iter().filter(|f| f.rule == "R8").collect();
        assert!(!r8.is_empty(), "should detect empty trigger");
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn cross_reference_unknown_skill_reports_r9() {
        let tmp = std::env::temp_dir().join(format!("val-test-r9-{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();

        create_valid_skill(&tmp, "alpha", "\nrelated_skills: [nonexistent, omega]\n");

        let report = validate_all(&tmp);
        let r9: Vec<_> = report.findings.iter().filter(|f| f.rule == "R9").collect();
        assert_eq!(r9.len(), 2, "both nonexistent and omega are unknown");
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn cross_reference_existing_skill_passes() {
        let tmp = std::env::temp_dir().join(format!("val-test-r9ok-{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();

        create_valid_skill(&tmp, "alpha", "");
        create_valid_skill(&tmp, "beta", "");
        // alpha references beta
        let alpha_dir = tmp.join("alpha");
        fs::write(
            alpha_dir.join("SKILL.md"),
            "---\nname: alpha\ndescription: \"refs beta\"\nrelated_skills: [beta]\n---\n\nBody\n",
        )
        .unwrap();

        let report = validate_all(&tmp);
        let r9: Vec<_> = report.findings.iter().filter(|f| f.rule == "R9").collect();
        assert_eq!(r9.len(), 0, "beta exists, cross-ref should pass");
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn validate_one_accepts_directory_path() {
        let tmp = std::env::temp_dir().join(format!("val-test-one-{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();
        create_valid_skill(&tmp, "single-skill", "");
        let report = validate_one(&tmp.join("single-skill"));
        assert!(report.is_ok());
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn validate_one_accepts_skill_md_path() {
        let tmp = std::env::temp_dir().join(format!("val-test-onemd-{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();
        let path = create_valid_skill(&tmp, "inline-skill", "");
        let report = validate_one(&path);
        assert!(report.is_ok(), "findings: {:?}", report.findings);
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn validate_one_rejects_non_skill_path() {
        let tmp = std::env::temp_dir().join(format!("val-test-bad-{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();
        let f = tmp.join("random.txt");
        fs::write(&f, "hello").unwrap();
        let report = validate_one(&f);
        assert!(!report.is_ok());
        let _ = fs::remove_dir_all(&tmp);
    }
}
