// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! EXP.md parsing and serialization.

use crate::model::{ExpMd, ExperienceEntry, ExperienceKind, ExperienceTag};
use kernel::error::AmanResult;
use std::path::Path;

/// Parse EXP.md from a file path.
pub fn parse_file(path: &Path) -> AmanResult<ExpMd> {
    let content = std::fs::read_to_string(path)?;
    parse_content(&content)
}

/// Parse EXP.md content from a string.
pub fn parse_content(content: &str) -> AmanResult<ExpMd> {
    let mut exp = ExpMd::empty();
    let mut current_kind: Option<ExperienceKind> = None;
    let mut current_entry: Option<ExperienceEntry> = None;

    for line in content.lines() {
        let line = line.trim_end();

        // Section headers
        if line.starts_with("## Tool Strategies") {
            current_kind = Some(ExperienceKind::ToolStrategy);
            continue;
        } else if line.starts_with("## Judgment Patterns") {
            current_kind = Some(ExperienceKind::JudgmentPattern);
            continue;
        } else if line.starts_with("## Anti-Patterns") {
            current_kind = Some(ExperienceKind::AntiPattern);
            continue;
        } else if line.starts_with("## Gotchas") {
            current_kind = Some(ExperienceKind::Gotcha);
            continue;
        }

        // Entry header: ### [tag] description
        if let Some(stripped) = line.strip_prefix("### ") {
            // Save previous entry
            if let Some(entry) = current_entry.take() {
                push_entry(&mut exp, entry);
            }
            let (tag, desc) = parse_entry_header(stripped);
            if let Some(category) = current_kind {
                current_entry = Some(ExperienceEntry {
                    category,
                    tag,
                    description: desc,
                    content: String::new(),
                    confidence: 0.5,
                    uses: 0,
                    successes: 0,
                    needs_verification: false,
                    learned_from: Vec::new(),
                });
            }
            continue;
        }

        // Key-value fields: "- **fieldname**: value"
        if let Some(ref mut entry) = current_entry
            && let Some((key, val)) = line.split_once(':')
        {
            // Strip "- **" prefix and "**" suffix from key
            let key = key
                .trim()
                .trim_start_matches("- **")
                .trim_end_matches("**");
            let val = val.trim();
            match key {
                "Strategy" | "Pattern" | "Anti-pattern" | "Gotcha" | "Workaround"
                | "Content" => {
                    entry.content = val.to_string();
                }
                "confidence" => {
                    entry.confidence = val.parse().unwrap_or(0.5);
                }
                "uses" => {
                    entry.uses = val.parse().unwrap_or(0);
                }
                "successes" => {
                    entry.successes = val.parse().unwrap_or(0);
                }
                "needs_verification" => {
                    entry.needs_verification = val.eq_ignore_ascii_case("true");
                }
                "last_verified" | "last_hit" | "deprecated" => {
                    // Date fields — could parse if needed
                }
                "learned_from" => {
                    entry.learned_from = val
                        .trim_matches(|c| c == '[' || c == ']')
                        .split(',')
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .collect();
                }
                "boundary_ref" => {
                    // Reference to SOUL.md — metadata
                }
                _ => {}
            }
        }
    }

    // Don't forget the last entry
    if let Some(entry) = current_entry {
        push_entry(&mut exp, entry);
    }

    Ok(exp)
}

fn parse_entry_header(s: &str) -> (ExperienceTag, String) {
    // Format: [tag] description
    if s.starts_with('[')
        && let Some(end) = s.find(']')
    {
        let tag = ExperienceTag::new(&s[1..end]);
        let desc = s[end + 1..].trim().to_string();
        return (tag, desc);
    }
    // Fallback: no tag
    (ExperienceTag::new("misc"), s.to_string())
}

fn push_entry(exp: &mut ExpMd, entry: ExperienceEntry) {
    match entry.category {
        ExperienceKind::ToolStrategy => exp.strategies.push(entry),
        ExperienceKind::JudgmentPattern => exp.patterns.push(entry),
        ExperienceKind::AntiPattern => exp.anti_patterns.push(entry),
        ExperienceKind::Gotcha => exp.gotchas.push(entry),
    }
}

/// Serialize EXP.md to a string.
pub fn format_md(exp: &ExpMd) -> String {
    let mut out = String::new();
    out.push_str("# Experience\n\n");

    if !exp.strategies.is_empty() {
        out.push_str("## Tool Strategies\n");
        for entry in &exp.strategies {
            format_entry(&mut out, entry);
        }
        out.push('\n');
    }

    if !exp.patterns.is_empty() {
        out.push_str("## Judgment Patterns\n");
        for entry in &exp.patterns {
            format_entry(&mut out, entry);
        }
        out.push('\n');
    }

    if !exp.anti_patterns.is_empty() {
        out.push_str("## Anti-Patterns\n");
        for entry in &exp.anti_patterns {
            format_entry(&mut out, entry);
        }
        out.push('\n');
    }

    if !exp.gotchas.is_empty() {
        out.push_str("## Gotchas\n");
        for entry in &exp.gotchas {
            format_entry(&mut out, entry);
        }
        out.push('\n');
    }

    out
}

fn format_entry(out: &mut String, entry: &ExperienceEntry) {
    let field_name = match entry.category {
        ExperienceKind::ToolStrategy => "Strategy",
        ExperienceKind::JudgmentPattern => "Pattern",
        ExperienceKind::AntiPattern => "Anti-pattern",
        ExperienceKind::Gotcha => "Gotcha",
    };
    out.push_str(&format!("### [{}] {}\n", entry.tag.as_str(), entry.description));
    if !entry.content.is_empty() {
        out.push_str(&format!("- **{}**: {}\n", field_name, entry.content));
    }
    out.push_str(&format!("- **confidence**: {:.2}\n", entry.confidence));
    out.push_str(&format!("- **uses**: {}\n", entry.uses));
    out.push_str(&format!("- **successes**: {}\n", entry.successes));
    if entry.needs_verification {
        out.push_str("- **needs_verification**: true\n");
    }
    if !entry.learned_from.is_empty() {
        out.push_str(&format!(
            "- **learned_from**: [{}]\n",
            entry.learned_from.join(", ")
        ));
    }
    out.push('\n');
}

/// Write EXP.md to a file.
pub fn write_file(path: &Path, exp: &ExpMd) -> AmanResult<()> {
    let content = format_md(exp);
    std::fs::write(path, content)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_empty() {
        let exp = parse_content("# Experience\n\n").unwrap();
        assert!(exp.is_empty());
    }

    #[test]
    fn test_parse_strategy() {
        let md = r#"# Experience

## Tool Strategies
### [pr] GitHub PR operations
- **Content**: Use `gh` CLI instead of raw API
- **confidence**: 0.92
- **uses**: 47
- **successes**: 43
- **learned_from**: [session_001, session_002]
"#;
        let exp = parse_content(md).unwrap();
        assert_eq!(exp.strategies.len(), 1);
        let s = &exp.strategies[0];
        assert_eq!(s.tag.as_str(), "pr");
        assert_eq!(s.uses, 47);
        assert_eq!(s.successes, 43);
        assert!((s.confidence - 0.92).abs() < 0.001);
    }

    #[test]
    fn test_roundtrip() {
        let md = r#"# Experience

## Tool Strategies
### [deploy] Local k8s deployment
- **Content**: Use kind instead of minikube for faster startup
- **confidence**: 0.85
- **uses**: 12
- **successes**: 10

## Gotchas
### [kind] Port mapping difference
- **Content**: kind doesn't need port-forward to localhost
- **confidence**: 0.95
- **uses**: 5
- **successes**: 5
"#;
        let exp = parse_content(md).unwrap();
        let formatted = format_md(&exp);
        let exp2 = parse_content(&formatted).unwrap();
        assert_eq!(exp.strategies.len(), exp2.strategies.len());
        assert_eq!(exp.gotchas.len(), exp2.gotchas.len());
    }
}
