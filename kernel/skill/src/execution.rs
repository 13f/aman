// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! Decoupled skill execution context builder.
//!
//! Provides [`prepare_skill_execution`] — a single function that any module
//! (chat pipeline, CLI, workflows, API handlers) can call to resolve a skill
//! by name, load its full body, and build an LLM-ready augmented prompt.
//!
//! Also provides [`discover_supporting_files`] and
//! [`build_skill_directory_context`] for tool implementations that need to
//! expose the skill's directory layout to the LLM.

use std::fs;
use std::path::{Path, PathBuf};

use crate::formatting;
use crate::SkillInfo;

/// The result of resolving and preparing a skill for LLM execution.
#[derive(Debug, Clone)]
pub struct SkillExecution {
    pub skill_name: String,
    /// Full SKILL.md body with YAML frontmatter stripped.
    pub skill_body: String,
    /// User-provided parameters after the skill name.
    pub user_input: String,
    /// Ready-to-inject message that includes the full skill methodology
    /// and user input. Prepend this as a system/user turn in the LLM context.
    pub augmented_message: String,
}

/// A supporting file discovered in a skill directory (everything except SKILL.md).
#[derive(Debug, Clone)]
pub struct SupportingFile {
    /// Path relative to the skill directory (e.g. `scripts/run.ts`).
    pub relative_path: String,
    /// Canonical absolute path on disk.
    pub absolute_path: PathBuf,
}

/// Discover supporting files in a skill directory.
///
/// Walks the directory tree rooted at `skill_dir` and returns every file
/// **except** `SKILL.md`. Each entry carries a relative path (for display)
/// and the canonical absolute path (for execution).
///
/// Returns an empty vec if the directory cannot be read.
#[must_use]
pub fn discover_supporting_files(skill_dir: &Path) -> Vec<SupportingFile> {
    let mut files = Vec::new();
    let Ok(canonical_skill_dir) = skill_dir.canonicalize() else {
        return files;
    };
    walk_supporting_files(&canonical_skill_dir, &canonical_skill_dir, &mut files);
    files
}

/// Directory names that are skipped when walking a skill folder for supporting
/// files. These typically hold reference material (examples, tests, cached
/// output) that should not be injected into the LLM prompt.
const EXCLUDED_SKILL_DIRS: &[&str] = &[
    "examples",
    "example",
    "testcase",
    "testcases",
    "test",
    "tests",
    "output",
    "outputs",
    "__pycache__",
    "node_modules",
    "fixtures",
];

/// Recursively walk `dir`, collecting every file except `SKILL.md`.
///
/// Hidden files (e.g. `.DS_Store`) and directories listed in
/// [`EXCLUDED_SKILL_DIRS`] are skipped.
fn walk_supporting_files(base: &Path, dir: &Path, files: &mut Vec<SupportingFile>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            // Skip excluded directories (examples, tests, output, …).
            if path
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|name| EXCLUDED_SKILL_DIRS.contains(&name))
            {
                continue;
            }
            walk_supporting_files(base, &path, files);
        } else if path.is_file()
            && path.file_name().and_then(|n| n.to_str()) != Some("SKILL.md")
        {
            // Skip hidden files (e.g. .DS_Store, .gitignore).
            if path
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|name| name.starts_with('.'))
            {
                continue;
            }
            let relative_path = path
                .strip_prefix(base)
                .ok()
                .and_then(|p| p.to_str())
                .map(String::from);
            if let Some(rel) = relative_path {
                files.push(SupportingFile {
                    relative_path: rel,
                    absolute_path: path.clone(),
                });
            }
        }
    }
}

/// Build the `[Skill directory: ...]` header and `[This skill has supporting files:]`
/// footer block (if any supporting files exist).
///
/// The returned string is ready to prepend/append to the skill content.
#[must_use]
pub fn build_skill_directory_context(skill_dir: &Path) -> (String, String) {
    let dir_display = skill_dir.display();
    let header = format!("[Skill directory: {dir_display}]\n");

    let supporting_files = discover_supporting_files(skill_dir);
    let footer = if supporting_files.is_empty() {
        String::new()
    } else {
        let mut s = String::from("\n\n[This skill has supporting files:]\n");
        for sf in &supporting_files {
            s.push_str(&format!(
                "- {}  ->  {}\n",
                sf.relative_path,
                sf.absolute_path.display()
            ));
        }
        s
    };

    (header, footer)
}

/// Resolve a file path relative to a skill directory, with path-traversal protection.
///
/// Returns `None` when `file_path` is empty, contains `..`, is absolute, or resolves
/// outside the skill directory.
#[must_use]
pub fn resolve_skill_file_path(skill_dir: &Path, file_path: &str) -> Option<PathBuf> {
    if file_path.is_empty() || file_path.contains("..") {
        return None;
    }
    let p = Path::new(file_path);
    if p.is_absolute() {
        return None;
    }
    let resolved = skill_dir.join(p);
    // If the file exists, canonicalize and verify it's still within the skill dir.
    if let (Ok(canon), Ok(skill_canon)) = (resolved.canonicalize(), skill_dir.canonicalize()) {
        if !canon.starts_with(&skill_canon) {
            return None;
        }
        return Some(canon);
    }
    // File doesn't exist yet — accept the joined path.
    Some(resolved)
}

/// Parse a slash-command string into (skill_name, user_input).
///
/// Supports two forms:
/// - `/skill skillName args...` — explicit command prefix
/// - `/skillName args...` — direct skill invocation
///
/// Returns `None` when the input does not start with `/` or no skill name is found.
#[must_use]
pub fn parse_skill_command(input: &str) -> Option<(String, String)> {
    let trimmed = input.trim();
    if !trimmed.starts_with('/') {
        return None;
    }
    let inner = &trimmed[1..]; // strip leading '/'
    let parts: Vec<&str> = inner.splitn(3, |c: char| c.is_whitespace()).collect();
    let first = *parts.first()?;
    if first.is_empty() {
        return None;
    }
    // "/skill skillName args..."
    if first == "skill" && parts.len() >= 2 {
        let skill_name = parts[1].trim().to_string();
        let user_input = parts.get(2).map(|s| s.trim().to_string()).unwrap_or_default();
        return Some((skill_name, user_input));
    }
    // "/skillName args..." (direct invocation)
    let skill_name = first.trim().to_string();
    let user_input = parts.get(1).map(|s| s.trim().to_string()).unwrap_or_default();
    Some((skill_name, user_input))
}

/// Resolve a skill by name, read its full SKILL.md body, strip frontmatter,
/// and build an augmented message for injection into the LLM context.
///
/// The message includes:
/// 1. `[Skill directory: ...]` — the skill's base directory for resolving
///    relative paths.
/// 2. The user input (if provided).
/// 3. The full skill methodology.
/// 4. `[This skill has supporting files:]` — every file in the skill directory
///    (except SKILL.md) listed with its absolute path, so the LLM can run
///    scripts, read templates, etc. without guessing file locations.
///
/// Returns `None` when the skill is not found in `skills` or the file cannot
/// be read.
#[must_use]
pub fn prepare_skill_execution(
    skill_name: &str,
    user_input: &str,
    skills: &[SkillInfo],
) -> Option<SkillExecution> {
    let info = skills.iter().find(|s| s.name == skill_name)?;

    let raw = fs::read_to_string(&info.path).ok()?;
    let body = formatting::strip_frontmatter(&raw).trim().to_owned();

    let skill_dir = info.path.parent().unwrap_or_else(|| Path::new("."));
    let (dir_header, supporting_files_footer) = build_skill_directory_context(skill_dir);

    let augmented_message = if user_input.is_empty() {
        format!(
            "{dir_header}\n\
             [SKILL MODE] The user invoked skill \"{skill_name}\".\n\n\
             --- SKILL METHODOLOGY ---\n\
             {body}\n\
             --- END SKILL ---\n\n\
             Follow the skill's methodology, analysis framework, and output \
             template exactly. Execute each step in order. Do not skip or \
             abbreviate any prescribed stage.\
             {supporting_files_footer}"
        )
    } else {
        format!(
            "{dir_header}\n\
             [SKILL MODE] The user invoked skill \"{skill_name}\" with the \
             following input:\n\n\
             > {user_input}\n\n\
             --- SKILL METHODOLOGY ---\n\
             {body}\n\
             --- END SKILL ---\n\n\
             Process the user's input above using the skill's methodology, \
             analysis framework, and output template. Execute each step in \
             order. Do not skip or abbreviate any prescribed stage.\
             {supporting_files_footer}"
        )
    };

    Some(SkillExecution {
        skill_name: skill_name.to_string(),
        skill_body: body,
        user_input: user_input.to_string(),
        augmented_message,
    })
}

/// Check whether a slash-command prefix (e.g. `/btc`) matches any installed
/// skill. Useful for autocomplete filtering.
#[must_use]
pub fn match_skill_prefix<'a>(prefix: &str, skills: &'a [SkillInfo]) -> Vec<&'a SkillInfo> {
    let prefix = prefix.trim_start_matches('/').to_lowercase();
    if prefix.is_empty() {
        return skills.iter().collect();
    }
    skills
        .iter()
        .filter(|s| {
            s.name.to_lowercase().contains(&prefix)
                || s.description.to_lowercase().contains(&prefix)
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Convenience: discover + prepare in one call (for callers without a registry)
// ---------------------------------------------------------------------------

/// Discover skills from `skills_root`, then prepare execution for `skill_name`.
///
/// This is a convenience wrapper that calls [`crate::discover_llm_skills`] and
/// then [`prepare_skill_execution`]. Prefer [`prepare_skill_execution`] directly
/// when you already have a `&[SkillInfo]` slice cached.
#[must_use]
pub fn prepare_skill_execution_from_dir(
    skill_name: &str,
    user_input: &str,
    skills_root: &Path,
) -> Option<SkillExecution> {
    let skills = crate::discover_llm_skills(skills_root);
    prepare_skill_execution(skill_name, user_input, &skills)
}
