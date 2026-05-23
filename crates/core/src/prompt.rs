// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

use crate::react::{SoulSnapshot, ToolDescriptor};
use async_trait::async_trait;

/// Date string to inject into the system prompt so the LLM can
/// interpret relative time references like "recent" or "today".
///
/// Uses pure arithmetic (no chrono dependency) based on Unix epoch.
pub fn current_date_string() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or(std::time::Duration::ZERO)
        .as_secs();

    let days = secs / 86400;

    fn leap_days_since_1970(year: u64) -> u64 {
        let y = year - 1;
        y / 4 - y / 100 + y / 400 - 469
    }

    let mut year = 1970 + days / 365;
    loop {
        let days_in_year = if is_leap(year) { 366 } else { 365 };
        let from_1970 = (year - 1970) * 365 + leap_days_since_1970(year);
        if from_1970 * 86400 > secs {
            year -= 1;
            break;
        }
        if (from_1970 + days_in_year) * 86400 > secs {
            break;
        }
        year += 1;
    }

    let from_1970 = (year - 1970) * 365 + leap_days_since_1970(year);
    let day_of_year = days - from_1970;
    let is_leap_yr = is_leap(year);
    let month_days: [u64; 12] = if is_leap_yr {
        [31, 29, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    } else {
        [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    };

    let mut month = 0u64;
    let mut accum = 0u64;
    for (i, &md) in month_days.iter().enumerate() {
        if accum + md > day_of_year {
            month = i as u64;
            break;
        }
        accum += md;
    }

    let day = day_of_year - accum + 1;

    format!("{year:04}-{month:02}-{day:02}")
}

fn is_leap(year: u64) -> bool {
    (year.is_multiple_of(4) && !year.is_multiple_of(100)) || year.is_multiple_of(400)
}

/// Prompt pipeline — builds the system prompt for the LLM.
///
/// Implementations define how the system prompt is assembled from
/// the agent's SOUL, available tools, and any retrieved memory context.
#[async_trait]
pub trait PromptPipeline: Send + Sync {
    /// Build the full system prompt string.
    async fn build_system_prompt(
        &self,
        soul: &SoulSnapshot,
        tools: &[ToolDescriptor],
        memory: Option<&str>,
    ) -> String;
}

/// Default prompt pipeline matching the original ContextAssembler logic.
///
/// Assembles: SOUL prompt → current date → available tools (with formatting
/// instructions) → retrieved memories.
pub struct DefaultPromptPipeline;

#[async_trait]
impl PromptPipeline for DefaultPromptPipeline {
    async fn build_system_prompt(
        &self,
        soul: &SoulSnapshot,
        tools: &[ToolDescriptor],
        memory: Option<&str>,
    ) -> String {
        let mut parts: Vec<String> = Vec::new();
        parts.push(soul.system_prompt.clone());
        parts.push(format!("Current date: {}", current_date_string()));

        if !tools.is_empty() {
            let tool_list: Vec<String> = tools
                .iter()
                .map(|t| {
                    format!(
                        "- {}: {} (parameters: {})",
                        t.name, t.description, t.parameters
                    )
                })
                .collect();
            parts.push(format!(
                "\n## Available Tools\nYou can use these tools when responding:\n{}",
                tool_list.join("\n")
            ));
            parts.push(
                "\n## File Operations (safe, no shell)\n\
                 - read(path): read file contents\n\
                 - write(path, content): write file (auto-creates parent dirs)\n\
                 - edit(file_path, old_string, new_string): replace exact matching text in file\n\
                 - list(path): list directory entries\n\
                 - find(pattern, base): search files by name (recursive, case-insensitive)\n\
                 - grep(pattern, path, glob?): search file contents via ripgrep (multi-threaded)"
                    .to_owned(),
            );
            parts.push(
                "\nWhen you need to use a tool, respond with a JSON tool call in the format:\
                 \n```tool_call\n{\"name\": \"tool_name\", \"arguments\": {...}}\n```"
                    .to_owned(),
            );
            parts.push(
                "\nImportant: If the user asks about current events, recent data, prices, dates, \
                 or any time-sensitive information, use the web_search tool first rather than \
                 relying on your training data. For example, search for \"recent\" or \"today\" \
                 queries instead of answering from memory."
                    .to_owned(),
            );
        }

        if let Some(mem) = memory
            && !mem.is_empty()
        {
            parts.push(format!("\n## Retrieved Memories\n{mem}"));
        }

        parts.join("\n\n")
    }
}
