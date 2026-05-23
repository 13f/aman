// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! Decoupled skill execution context builder.
//!
//! Provides [`prepare_skill_execution`] — a single function that any module
//! (chat pipeline, CLI, workflows, API handlers) can call to resolve a skill
//! by name, load its full body, and build an LLM-ready augmented prompt.

use std::fs;
use std::path::Path;

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
/// The returned [`SkillExecution`] can be used by any caller to inject a
/// skill into a conversation — the `augmented_message` is ready to prepend
/// as a system or user turn.
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

    let augmented_message = if user_input.is_empty() {
        format!(
            "[SKILL MODE] The user invoked skill \"{skill_name}\".\n\n\
             --- SKILL METHODOLOGY ---\n\
             {body}\n\
             --- END SKILL ---\n\n\
             Follow the skill's methodology, analysis framework, and output \
             template exactly. Execute each step in order. Do not skip or \
             abbreviate any prescribed stage."
        )
    } else {
        format!(
            "[SKILL MODE] The user invoked skill \"{skill_name}\" with the \
             following input:\n\n\
             > {user_input}\n\n\
             --- SKILL METHODOLOGY ---\n\
             {body}\n\
             --- END SKILL ---\n\n\
             Process the user's input above using the skill's methodology, \
             analysis framework, and output template. Execute each step in \
             order. Do not skip or abbreviate any prescribed stage."
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
