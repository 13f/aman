// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! DailyLifeConfig — passive-queue configuration for the daily-life system.
//!
//! Architecture ref: daily-life-design.md v2 §9

use lifecycle::{ExecutionConfig, HooksConfig, QueueConfig, RetryConfig};
use serde::{Deserialize, Serialize};

use crate::types::Routine;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DailyLifeConfig {
    #[serde(default)]
    pub execution: ExecutionConfig,

    #[serde(default)]
    pub hooks: HooksConfig,

    #[serde(default)]
    pub queue: QueueConfig,

    #[serde(default)]
    pub retry: RetryConfig,

    /// 时区。
    #[serde(default = "default_timezone")]
    pub timezone: String,

    /// 每个时间窗的默认例行事项。
    #[serde(default)]
    pub routines: RoutinesPerWindow,
}

fn default_timezone() -> String {
    "Asia/Shanghai".into()
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct RoutinesPerWindow {
    #[serde(default)]
    pub morning: Vec<Routine>,
    #[serde(default)]
    pub midday: Vec<Routine>,
    #[serde(default)]
    pub afternoon: Vec<Routine>,
    #[serde(default)]
    pub evening: Vec<Routine>,
    #[serde(default)]
    pub night: Vec<Routine>,
}

impl Default for DailyLifeConfig {
    fn default() -> Self {
        Self {
            execution: ExecutionConfig::default(),
            hooks: HooksConfig::default(),
            queue: QueueConfig::default(),
            retry: RetryConfig::default(),
            timezone: "Asia/Shanghai".into(),
            routines: RoutinesPerWindow::default(),
        }
    }
}

impl DailyLifeConfig {
    pub fn validate(&self) -> Result<(), String> {
        self.execution.validate()?;
        self.queue.validate()?;
        self.retry.validate()?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_is_valid() {
        let config = DailyLifeConfig::default();
        assert!(config.validate().is_ok());
        assert_eq!(config.timezone, "Asia/Shanghai");
    }

    #[test]
    fn config_serde_roundtrip() {
        let config = DailyLifeConfig::default();
        let json = serde_json::to_string(&config).expect("serialize");
        let deser: DailyLifeConfig = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(config.timezone, deser.timezone);
    }
}
