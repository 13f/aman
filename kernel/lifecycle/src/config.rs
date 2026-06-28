// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! Shared configuration types for lifecycle systems.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::Duration;

// ---------------------------------------------------------------------------
// Serde helpers
// ---------------------------------------------------------------------------

mod duration_secs {
    use serde::{Deserialize, Deserializer, Serializer};
    use std::time::Duration;

    pub fn serialize<S>(d: &Duration, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_f64(d.as_secs_f64())
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Duration, D::Error>
    where
        D: Deserializer<'de>,
    {
        let secs = f64::deserialize(deserializer)?;
        Ok(Duration::from_secs_f64(secs))
    }
}

// ---------------------------------------------------------------------------
// ExecutionConfig
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExecutionConfig {
    #[serde(default = "default_true")]
    pub auto_decompose: bool,

    #[serde(default = "default_step_timeout", with = "duration_secs")]
    pub step_timeout: Duration,

    #[serde(default, with = "duration_secs")]
    pub inter_item_cooldown: Duration,
}

fn default_true() -> bool {
    true
}

fn default_step_timeout() -> Duration {
    Duration::from_secs(120)
}

impl Default for ExecutionConfig {
    fn default() -> Self {
        Self {
            auto_decompose: true,
            step_timeout: Duration::from_secs(120),
            inter_item_cooldown: Duration::ZERO,
        }
    }
}

impl ExecutionConfig {
    pub fn validate(&self) -> Result<(), String> {
        if self.step_timeout.is_zero() {
            return Err("execution.step_timeout must be > 0".into());
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// QueueConfig
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QueueConfig {
    #[serde(default = "default_max_queue_size")]
    pub max_size: usize,

    #[serde(default)]
    pub priority_queue: bool,
}

fn default_max_queue_size() -> usize {
    100
}

impl Default for QueueConfig {
    fn default() -> Self {
        Self {
            max_size: 100,
            priority_queue: false,
        }
    }
}

impl QueueConfig {
    pub fn validate(&self) -> Result<(), String> {
        if self.max_size == 0 {
            return Err("queue.max_size must be >= 1".into());
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// RetryConfig
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RetryConfig {
    #[serde(default = "default_max_step_retries")]
    pub max_step_retries: u32,

    #[serde(default = "default_retry_delay", with = "duration_secs")]
    pub retry_delay: Duration,
}

fn default_max_step_retries() -> u32 {
    3
}

fn default_retry_delay() -> Duration {
    Duration::from_secs(5)
}

impl Default for RetryConfig {
    fn default() -> Self {
        Self {
            max_step_retries: 3,
            retry_delay: Duration::from_secs(5),
        }
    }
}

impl RetryConfig {
    pub fn validate(&self) -> Result<(), String> {
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// HooksConfig
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct HooksConfig {
    #[serde(default)]
    pub before_execution: Vec<HookDefinition>,
    #[serde(default)]
    pub before_step: Vec<HookDefinition>,
    #[serde(default)]
    pub after_step: Vec<HookDefinition>,
    #[serde(default)]
    pub after_execution: Vec<HookDefinition>,
    #[serde(default)]
    pub on_success: Vec<HookDefinition>,
    #[serde(default)]
    pub on_failure: Vec<HookDefinition>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HookDefinition {
    pub name: String,
    pub action: HookAction,
    #[serde(default)]
    pub abort_on_failure: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HookAction {
    Tool {
        tool_name: String,
        #[serde(default)]
        params: HashMap<String, serde_json::Value>,
    },
    Llm {
        system_prompt: String,
        #[serde(default = "default_max_tokens")]
        max_tokens: u32,
    },
    EmitEvent {
        event_type: String,
        payload_template: String,
    },
}

fn default_max_tokens() -> u32 {
    1024
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_execution_config_is_valid() {
        let c = ExecutionConfig::default();
        assert!(c.validate().is_ok());
        assert!(c.auto_decompose);
        assert_eq!(c.step_timeout, Duration::from_secs(120));
    }

    #[test]
    fn zero_step_timeout_is_invalid() {
        let c = ExecutionConfig {
            step_timeout: Duration::ZERO,
            ..Default::default()
        };
        assert!(c.validate().is_err());
    }

    #[test]
    fn default_queue_config_is_valid() {
        let c = QueueConfig::default();
        assert!(c.validate().is_ok());
        assert_eq!(c.max_size, 100);
    }

    #[test]
    fn zero_queue_size_is_invalid() {
        let c = QueueConfig {
            max_size: 0,
            ..Default::default()
        };
        assert!(c.validate().is_err());
    }

    #[test]
    fn hooks_default_empty() {
        let c = HooksConfig::default();
        assert!(c.before_execution.is_empty());
        assert!(c.after_execution.is_empty());
    }

    #[test]
    fn hook_definition_serde() {
        let hook = HookDefinition {
            name: "log_start".into(),
            action: HookAction::Tool {
                tool_name: "trace.record".into(),
                params: {
                    let mut m = HashMap::new();
                    m.insert("event".into(), serde_json::json!("item.started"));
                    m
                },
            },
            abort_on_failure: false,
        };
        let json = serde_json::to_string(&hook).expect("serialize");
        assert!(json.contains("log_start"), "{json}");
        let deser: HookDefinition = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(deser.name, "log_start");
    }
}
