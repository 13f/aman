//! Pure formatting helpers for skill activation messages, system prompts,
//! and LLM reinforcement messages.

use kernel::react::ChatMessage;

use crate::SkillInfo;

/// Build a lightweight skill activation message (Level 2 of Progressive Disclosure).
///
/// Unlike the previous version that embedded the full SKILL.md body, this version
/// only signals the skill name and tells the LLM to call `read_skill` to load
/// the methodology and output template. This makes the skill-loading step visible
/// in the tool call stream so users see the skill being activated.
pub fn build_skill_activation_message(skill: &SkillInfo) -> Option<String> {
    let name = &skill.name;
    Some(format!(
        "[ACTIVATED SKILL: \"{name}\"]\n\
         The skill \"{name}\" matches your query. Call `read_skill(skill: \"{name}\")` \n\
         now to load its full methodology, analysis framework, and output template.\n\
         You MUST load the skill with read_skill before proceeding — do not skip this step.\n\
         Begin your response by stating \"[Skill: {name}]\" to confirm activation."
    ))
}

/// Strip YAML frontmatter (`---\n...\n---`) from SKILL.md content.
///
/// Returns only the body after the frontmatter block. If no frontmatter is
/// found, returns the entire content unchanged.
#[must_use]
pub fn strip_frontmatter(raw: &str) -> &str {
    let s = raw.trim_start();
    if s.starts_with("---") {
        // Find the closing `---` (first `\n---` after the opening delimiter)
        if let Some(end) = s[3..].find("\n---") {
            return s[3 + end + 4..].trim_start();
        }
    }
    s
}

/// Build the lightweight skill index for the LLM system prompt (Level 1 of
/// Progressive Disclosure — see agentskills.io).  Produces a categorized list
/// of available skills with names and short descriptions.
pub fn build_skills_system_prompt(skills: &[SkillInfo]) -> String {
    if skills.is_empty() {
        return String::new();
    }
    let mut out = String::from("\n\n## Skills (mandatory)\n\n");
    out.push_str(
        "Before replying, scan the skills below. If a skill matches or is even \
         partially relevant to your task, load it with `read_skill(skill: \"...\")` \
         and follow its instructions. Err on the side of loading — it is always \
         better to have context you don't need than to miss critical steps, pitfalls, \
         or established workflows. Skills contain specialized knowledge, methodologies, \
         and output templates that your default approach cannot replicate.\n\n\
         Always start by calling read_skill for the matching skill before doing any \
         other work — this ensures you have the full methodology and output format \
         before gathering data or producing results.\n\n",
    );

    // Group skills by category
    let mut grouped: std::collections::BTreeMap<&str, Vec<&SkillInfo>> =
        std::collections::BTreeMap::new();
    for s in skills.iter() {
        let cat = if s.category.is_empty() {
            "General"
        } else {
            s.category.as_str()
        };
        grouped.entry(cat).or_default().push(s);
    }

    for (category, cat_skills) in &grouped {
        out.push_str(&format!("### {category}\n"));
        for s in cat_skills {
            out.push_str(&format!("- {}: {}\n", s.name, s.description));
        }
        out.push('\n');
    }

    out.push_str(
        "Only proceed without loading a skill if you have checked and genuinely \
         none are relevant to the task.\n",
    );
    out.push_str(
        "After completing a difficult or iterative task, consider offering to save \
         the approach as a skill for future reuse by asking the user to create a new \
         SKILL.md file.\n",
    );
    out
}

/// Build a reinforcement message to inject after `read_skill` is invoked,
/// telling the LLM that the loaded skill is authoritative guidance.
pub fn build_read_skill_reinforcement(skill_name: &str) -> ChatMessage {
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

    // Re-inject the skill's scoring methodology and report template so the LLM
    // has them fresh in context after many turns of information gathering.
    if let Some(body) = skill_body.filter(|b| !b.is_empty()) {
        let relevant = extract_scoring_and_template_sections(body);
        if !relevant.is_empty() {
            msg.push_str("\n\n---\n## Skill Methodology (re-injected)\n\n");
            msg.push_str(&relevant);
            msg.push_str("\n\n---\n");
            msg.push_str("Follow the sections, scoring dimensions, weights, and template exactly as shown above.");
        }
    }

    ChatMessage::user(msg)
}

/// Extract the scoring criteria and report template sections from a skill body.
/// Looks for common section headers to identify relevant portions.
fn extract_scoring_and_template_sections(body: &str) -> String {
    let mut parts = Vec::new();
    let mut in_relevant = false;
    let mut current = String::new();

    for line in body.lines() {
        let trimmed = line.trim();
        // Detect scoring sections, output templates, report formats
        if trimmed.starts_with("### ") || trimmed.starts_with("## ") {
            let header_lower = trimmed.to_lowercase();
            let is_score_section = header_lower.contains("评分")
                || header_lower.contains("score")
                || header_lower.contains("scoring")
                || header_lower.contains("analys")
                || header_lower.contains("分析")
                || header_lower.contains("维度")
                || header_lower.contains("dimension")
                || header_lower.contains("阶段 2")
                || header_lower.contains("stage 2")
                || header_lower.contains("阶段 3")
                || header_lower.contains("stage 3")
                || header_lower.contains("输出")
                || header_lower.contains("output")
                || header_lower.contains("report")
                || header_lower.contains("template")
                || header_lower.contains("模板")
                || header_lower.contains("格式");

            if is_score_section {
                // Flush previous section
                if !current.is_empty() {
                    parts.push(std::mem::take(&mut current));
                }
                in_relevant = true;
                current.push_str(trimmed);
                current.push('\n');
                continue;
            }
        }

        if in_relevant {
            current.push_str(line);
            current.push('\n');
        }
    }

    // Flush last section
    if !current.is_empty() {
        parts.push(current);
    }

    parts.join("\n")
}
