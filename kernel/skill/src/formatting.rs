// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! Pure formatting helpers for skill activation messages, system prompts,
//! and LLM reinforcement messages.

use kernel::react::ChatMessage;

use crate::SkillInfo;

/// Strip YAML frontmatter (`---\n...\n---`) from SKILL.md content.
///
/// Returns only the body after the frontmatter block. If no frontmatter is
/// found, returns the entire content unchanged.
#[must_use]
pub fn strip_frontmatter(raw: &str) -> &str {
    let s = raw.trim_start();
    if let Some(stripped) = s.strip_prefix("---") {
        // Find the closing `---` (first `\n---` after the opening delimiter)
        if let Some(end) = stripped.find("\n---") {
            return stripped[end + 4..].trim_start();
        }
    }
    s
}

/// Build the categorized skill index for the LLM system prompt.
///
/// Produces a `## Skills` section with an `<available_skills>` XML-style block
/// grouping skills by category. Skill matching is 100% LLM-driven — the model
/// reads each description and decides which skills to load via `skill_view(name)`.
pub fn build_skills_system_prompt(skills: &[SkillInfo]) -> String {
    if skills.is_empty() {
        return String::new();
    }

    let mut out = String::from("\n\n## Skills\n\n");
    out.push_str(
        "Before replying, scan the skills below. If a skill matches or is even partially \
         relevant to your task, you MUST load it with skill_view(name) and follow its \
         instructions.\n\n",
    );

    out.push_str("<available_skills>\n");

    // Group skills by category (sorted for deterministic output)
    let mut grouped: std::collections::BTreeMap<&str, Vec<&SkillInfo>> =
        std::collections::BTreeMap::new();
    for s in skills.iter() {
        let cat = if s.category.is_empty() {
            "general"
        } else {
            s.category.as_str()
        };
        grouped.entry(cat).or_default().push(s);
    }

    for (category, cat_skills) in &grouped {
        out.push_str(&format!("  {}:\n", category));
        for s in cat_skills {
            out.push_str(&format!("    - {}: {}\n", s.name, s.description));
        }
    }

    out.push_str("</available_skills>\n");
    out
}

/// Build a reinforcement message to inject after `skill_view` is invoked,
/// telling the LLM that the loaded skill is authoritative guidance.
pub fn build_skill_view_reinforcement(skill_name: &str) -> ChatMessage {
    ChatMessage::user(format!(
        "[The skill \"{skill_name}\" has been loaded and is now active. \
         Its instructions in the tool result above are authoritative \
         for this task. You MUST follow its prescribed methodology, \
         analysis framework, and output format completely. Do not skip \
         or abbreviate any prescribed stage — execute each step in order.]"
    ))
}

/// Build a format reminder for when a skill was previously loaded and the
/// LLM has finished collecting data, prompting it to use the skill's template.
///
/// `skill_body` should contain the full skill body content (frontmatter-stripped).
/// When provided, the scoring methodology and report template are re-injected
/// so the LLM has them fresh in context after many ReAct turns of data collection.
pub fn build_format_reminder(skill_body: Option<&str>) -> ChatMessage {
    let mut msg = String::from(
        "[FORMAT INSTRUCTION] Data collection is complete. Now produce \
         the final report using the skill's prescribed template. Fill ALL \
         scoring sections — do not leave anything blank or marked \"TBD\". \
         Output the report now in a single message, using the exact section \
         headers and template layout from the skill.",
    );

    // Re-inject the FULL skill body so the LLM has the complete methodology,
    // scoring rubrics, traps, and output template fresh in context after many
    // turns of information gathering. Partial extraction misses critical
    // sections (e.g. traps, sub-dimensions, market-specific rules).
    if let Some(body) = skill_body.filter(|b| !b.is_empty()) {
        msg.push_str("\n\n---\n## Skill Methodology (re-injected, FULL)\n\n");
        msg.push_str(body);
        msg.push_str("\n\n---\n");
        msg.push_str("Follow ALL sections, scoring dimensions, weights, sub-dimensions, traps, and template exactly as shown above. Fill every section completely.");
    }

    ChatMessage::user(msg)
}
