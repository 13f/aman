// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! Finance card manager — skill cards displayed on the Home page Finance tab.
//!
//! Cards are defined in `predefined/cards.json` (built-in, kept up-to-date with
//! each release) and `~/.aman/finance_cards.json` (user edits — additions and
//! removals). The built-in set provides the default starting list; the user file
//! stores the full effective list once the user makes any modifications.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use crate::models::FinanceCardEntry;

// ---------------------------------------------------------------------------
// Persisted config types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FinanceCard {
    #[serde(rename = "skillName")]
    pub skill_name: String,
    pub title: String,
    pub subtitle: String,
    pub icon: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct FinanceCardsFile {
    #[serde(default)]
    cards: Vec<FinanceCard>,
}

/// Embedded built-in cards, kept current with each release.
const BUILTIN_JSON: &str = include_str!("../../../predefined/cards.json");

// ---------------------------------------------------------------------------
// Paths
// ---------------------------------------------------------------------------

fn user_cards_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    PathBuf::from(home).join(".aman").join("finance_cards.json")
}

// ---------------------------------------------------------------------------
// Load / save
// ---------------------------------------------------------------------------

/// Load the effective card list: user overrides if available, otherwise builtin.
///
/// On first run (no user file), returns the built-in defaults. Once the user
/// makes any modification (add or remove), the full effective list is persisted
/// to `~/.aman/finance_cards.json` and loaded from there on subsequent runs.
pub fn load_finance_cards() -> Result<Vec<FinanceCardEntry>, String> {
    let user_path = user_cards_path();

    let cards: Vec<FinanceCard> = if user_path.exists() {
        let raw = std::fs::read_to_string(&user_path)
            .map_err(|e| format!("读取 {} 失败: {e}", user_path.display()))?;
        let file: FinanceCardsFile = serde_json::from_str(&raw)
            .map_err(|e| format!("解析 {} 失败: {e}", user_path.display()))?;
        file.cards
    } else {
        let builtin: FinanceCardsFile = serde_json::from_str(BUILTIN_JSON)
            .map_err(|e| format!("解析内置 cards.json 失败: {e}"))?;
        builtin.cards
    };

    Ok(cards
        .into_iter()
        .map(|c| FinanceCardEntry {
            skill_name: c.skill_name,
            title: c.title,
            subtitle: c.subtitle,
            icon: c.icon,
        })
        .collect())
}

/// Persist the full effective card list to the user file.
fn save_cards(cards: &[FinanceCard]) -> Result<(), String> {
    let user_path = user_cards_path();
    if let Some(parent) = user_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("创建 ~/.aman 目录失败: {e}"))?;
    }
    let file = FinanceCardsFile {
        cards: cards.to_vec(),
    };
    let json = serde_json::to_string_pretty(&file)
        .map_err(|e| format!("序列化卡片数据失败: {e}"))?;
    std::fs::write(&user_path, json)
        .map_err(|e| format!("写入 {} 失败: {e}", user_path.display()))
}

/// Add a card. Loads current list, appends, and persists.
pub fn add_finance_card(
    skill_name: &str,
    title: &str,
    subtitle: &str,
    icon: &str,
) -> Result<(), String> {
    let mut cards: Vec<FinanceCard> = load_finance_cards()?
        .into_iter()
        .map(|c| FinanceCard {
            skill_name: c.skill_name,
            title: c.title,
            subtitle: c.subtitle,
            icon: c.icon,
        })
        .collect();

    // Avoid duplicates
    if cards.iter().any(|c| c.skill_name == skill_name) {
        return Err(format!("技能 '{skill_name}' 已存在"));
    }

    cards.push(FinanceCard {
        skill_name: skill_name.to_owned(),
        title: title.to_owned(),
        subtitle: subtitle.to_owned(),
        icon: icon.to_owned(),
    });

    save_cards(&cards)
}

/// Remove a card by skill name. Loads current list, removes, and persists.
pub fn remove_finance_card(skill_name: &str) -> Result<(), String> {
    let mut cards: Vec<FinanceCard> = load_finance_cards()?
        .into_iter()
        .map(|c| FinanceCard {
            skill_name: c.skill_name,
            title: c.title,
            subtitle: c.subtitle,
            icon: c.icon,
        })
        .collect();

    let len_before = cards.len();
    cards.retain(|c| c.skill_name != skill_name);

    if cards.len() == len_before {
        return Err(format!("技能 '{skill_name}' 不存在"));
    }

    save_cards(&cards)
}
