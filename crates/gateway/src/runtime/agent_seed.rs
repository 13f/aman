// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! Built-in agent seeding.
//!
//! Seeds predefined agents from `predefined/agents/` into the user's
//! `~/.aman/agents/` data directory on first run (when no agents exist).
//! Does NOT overwrite existing agents.

use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// Embedded predefined agent SOUL.md content
// ---------------------------------------------------------------------------

const PREDEFINED_AGENTS: &[PredefinedAgent] = &[
    PredefinedAgent {
        key: "aman",
        display_name: "aman",
        soul_md: include_str!("../../../../predefined/agents/aman/SOUL.md"),
    },
    PredefinedAgent {
        key: "health",
        display_name: "健康顾问",
        soul_md: include_str!("../../../../predefined/agents/health/SOUL.md"),
    },
    PredefinedAgent {
        key: "money",
        display_name: "投资顾问",
        soul_md: include_str!("../../../../predefined/agents/money/SOUL.md"),
    },
];

struct PredefinedAgent {
    key: &'static str,
    display_name: &'static str,
    soul_md: &'static str,
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Returns the aman user data directory (`~/.aman`).
pub fn aman_data_dir() -> PathBuf {
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .unwrap_or_else(|_| "/tmp".to_owned());
    PathBuf::from(home).join(".aman")
}

fn agents_data_dir() -> PathBuf {
    aman_data_dir().join("agents")
}

/// Count existing agent subdirectories under `~/.aman/agents/`.
fn existing_agent_count(dir: &Path) -> usize {
    if !dir.exists() {
        return 0;
    }
    std::fs::read_dir(dir)
        .map(|entries| {
            entries
                .filter_map(|e| e.ok())
                .filter(|e| e.file_type().map_or(false, |t| t.is_dir()))
                .count()
        })
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Scan `~/.aman/agents/` for subdirectories containing `SOUL.md` that are
/// not yet registered in config.yaml, and auto-register them with empty
/// provider (disabled).  This allows users to copy agent directories into
/// `~/.aman/agents/` and have them discovered automatically on next restart.
///
/// Returns the keys of newly discovered agents.
pub fn discover_filesystem_agents() -> Vec<String> {
    let config_path = aman_data_dir().join("config.yaml");
    let agents_dir = agents_data_dir();

    // Nothing to discover if the agents directory doesn't exist yet.
    if !agents_dir.exists() {
        return Vec::new();
    }

    let mut config: serde_yaml::Value = if config_path.exists() {
        let raw = match std::fs::read_to_string(&config_path) {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
        serde_yaml::from_str(&raw).unwrap_or(serde_yaml::Value::Mapping(
            serde_yaml::Mapping::new(),
        ))
    } else {
        return Vec::new(); // no config yet — seed_builtin_agents handles that
    };

    let entries = match std::fs::read_dir(&agents_dir) {
        Ok(e) => e,
        Err(_) => return Vec::new(),
    };

    let agents_map = match config.as_mapping_mut() {
        Some(m) => m,
        None => return Vec::new(),
    };

    let agents_entry = agents_map
        .entry(serde_yaml::Value::String("agents".to_string()))
        .or_insert_with(|| {
            serde_yaml::Value::Mapping(serde_yaml::Mapping::new())
        });

    let agents = match agents_entry.as_mapping_mut() {
        Some(m) => m,
        None => return Vec::new(),
    };

    let mut discovered = Vec::new();

    for entry in entries.flatten() {
        if !entry.file_type().map_or(false, |t| t.is_dir()) {
            continue;
        }
        let key = entry.file_name().to_string_lossy().to_string();
        let key_val = serde_yaml::Value::String(key.clone());

        // Skip agents already registered in config.
        if agents.contains_key(&key_val) {
            continue;
        }

        // Must have a SOUL.md to be a valid agent directory.
        if !entry.path().join("SOUL.md").exists() {
            continue;
        }

        let display_name = extract_display_name(&entry.path()).unwrap_or_else(|| key.clone());

        let mut entry_map = serde_yaml::Mapping::new();
        entry_map.insert(
            serde_yaml::Value::String("display_name".to_string()),
            serde_yaml::Value::String(display_name),
        );
        // Always register user-discovered agents with no provider — the user
        // must explicitly configure a provider before the agent can be used.
        entry_map.insert(
            serde_yaml::Value::String("provider".to_string()),
            serde_yaml::Value::String(String::new()),
        );
        entry_map.insert(
            serde_yaml::Value::String("model".to_string()),
            serde_yaml::Value::String(String::new()),
        );
        entry_map.insert(
            serde_yaml::Value::String("enabled".to_string()),
            serde_yaml::Value::Bool(false),
        );

        agents.insert(key_val, serde_yaml::Value::Mapping(entry_map));
        discovered.push(key.clone());
        tracing::info!(agent = %key, "discovered filesystem agent");
    }

    if discovered.is_empty() {
        return Vec::new();
    }

    if let Some(parent) = config_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    match serde_yaml::to_string(&config) {
        Ok(yaml) => {
            if let Err(e) = std::fs::write(&config_path, yaml) {
                tracing::warn!(path = %config_path.display(), error = %e, "failed to write config after discovering agents");
            } else {
                tracing::info!(
                    path = %config_path.display(),
                    count = discovered.len(),
                    "updated config.yaml with discovered agents"
                );
            }
        }
        Err(e) => {
            tracing::warn!(error = %e, "failed to serialize config after discovering agents");
        }
    }

    discovered
}

/// Try to extract a display name from an agent directory's SOUL.md.
/// Looks for the first `# Title` line.
fn extract_display_name(agent_dir: &std::path::Path) -> Option<String> {
    let soul_path = agent_dir.join("SOUL.md");
    let content = std::fs::read_to_string(&soul_path).ok()?;
    for line in content.lines() {
        let trimmed = line.trim();
        if let Some(stripped) = trimmed.strip_prefix("# ") {
            let title = stripped.trim();
            if !title.is_empty() {
                return Some(title.to_string());
            }
        }
    }
    None
}

/// Seed predefined agents into `~/.aman/agents/` if no agents exist yet.
///
/// Also updates `~/.aman/config.yaml` to register the agents if the config
/// exists but has no agents, or creates a minimal config if none exists.
///
/// Returns the list of agent keys that were seeded.
pub fn seed_builtin_agents() -> Vec<String> {
    let agents_dir = agents_data_dir();
    let count = existing_agent_count(&agents_dir);
    if count > 0 {
        tracing::info!(
            existing = count,
            "agents directory already has agents — skipping seed"
        );
        return Vec::new();
    }

    let mut seeded = Vec::new();
    for agent in PREDEFINED_AGENTS {
        let agent_dir = agents_dir.join(agent.key);
        if let Err(e) = seed_one_agent(agent_dir, agent) {
            tracing::warn!(agent = agent.key, error = %e, "failed to seed agent");
        } else {
            seeded.push(agent.key.to_string());
            tracing::info!(agent = agent.key, "seeded predefined agent");
        }
    }

    // Update config.yaml with the seeded agents.
    if !seeded.is_empty() {
        if let Err(e) = update_config_with_agents(&seeded) {
            tracing::warn!(error = %e, "failed to update config.yaml with seeded agents");
        }
    }

    seeded
}

fn seed_one_agent(agent_dir: PathBuf, agent: &PredefinedAgent) -> Result<(), String> {
    std::fs::create_dir_all(agent_dir.join("memory"))
        .map_err(|e| format!("create memory dir: {e}"))?;
    std::fs::create_dir_all(agent_dir.join("sessions"))
        .map_err(|e| format!("create sessions dir: {e}"))?;
    // Substitute {name} placeholder if present
    let content = agent.soul_md.replace("{name}", agent.display_name);
    std::fs::write(agent_dir.join("SOUL.md"), &content)
        .map_err(|e| format!("write SOUL.md: {e}"))?;
    Ok(())
}

/// Update `config.yaml` to include the seeded agents.
///
/// If the config has no providers, agents are added with empty provider
/// and model strings. The UI handles this gracefully: unconfigured agents
/// are shown but disabled for chat and idle.
fn update_config_with_agents(agent_keys: &[String]) -> Result<(), String> {
    let config_path = aman_data_dir().join("config.yaml");

    // Load existing config or create a minimal default.
    let mut config: serde_yaml::Value = if config_path.exists() {
        let raw = std::fs::read_to_string(&config_path)
            .map_err(|e| format!("read config: {e}"))?;
        serde_yaml::from_str(&raw).unwrap_or(serde_yaml::Value::Mapping(
            serde_yaml::Mapping::new(),
        ))
    } else {
        serde_yaml::Value::Mapping(serde_yaml::Mapping::new())
    };

    // Find the first available provider (or empty string for unconfigured agents).
    let default_provider = config
        .get("providers")
        .and_then(|p| p.as_mapping())
        .and_then(|m| m.keys().next())
        .and_then(|k| k.as_str())
        .map(|s| s.to_string())
        .unwrap_or_default();

    // Build the agents mapping.
    let agents_map = config
        .as_mapping_mut()
        .ok_or("config is not a mapping")?;

    let agents_entry = agents_map
        .entry(serde_yaml::Value::String("agents".to_string()))
        .or_insert_with(|| {
            serde_yaml::Value::Mapping(serde_yaml::Mapping::new())
        });

    let agents = agents_entry
        .as_mapping_mut()
        .ok_or("agents is not a mapping")?;

    for key in agent_keys {
        // Find matching predefined agent for display_name
        let display_name = PREDEFINED_AGENTS
            .iter()
            .find(|a| a.key == key.as_str())
            .map(|a| a.display_name)
            .unwrap_or(key.as_str());

        let mut entry = serde_yaml::Mapping::new();
        entry.insert(
            serde_yaml::Value::String("display_name".to_string()),
            serde_yaml::Value::String(display_name.to_string()),
        );
        entry.insert(
            serde_yaml::Value::String("provider".to_string()),
            serde_yaml::Value::String(default_provider.clone()),
        );
        let has_provider = !default_provider.is_empty();
        entry.insert(
            serde_yaml::Value::String("model".to_string()),
            serde_yaml::Value::String(if has_provider { "default" } else { "" }.to_string()),
        );
        // Disable agents without a configured provider so the idle system
        // and chat don't try to use them. The user enables them by configuring.
        entry.insert(
            serde_yaml::Value::String("enabled".to_string()),
            serde_yaml::Value::Bool(has_provider),
        );

        agents.insert(
            serde_yaml::Value::String(key.clone()),
            serde_yaml::Value::Mapping(entry),
        );
    }

    // Write updated config.
    if let Some(parent) = config_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("create config dir: {e}"))?;
    }
    std::fs::write(
        &config_path,
        serde_yaml::to_string(&config).map_err(|e| format!("serialize config: {e}"))?,
    )
    .map_err(|e| format!("write config: {e}"))?;

    tracing::info!(
        path = %config_path.display(),
        "updated config.yaml with seeded agents"
    );
    Ok(())
}
