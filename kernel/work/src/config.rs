// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! WorkConfig — v2 passive-queue configuration.
//!
//! Architecture ref: work-design.md v2 §9

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::Duration;

// ---------------------------------------------------------------------------
// Serde helpers — serialize Duration as f64 seconds
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
// WorkConfig
// ---------------------------------------------------------------------------

/// Top-level work system configuration (v2).
///
/// No board connection, no claim strategy, no capabilities — external systems
/// push work items; the Work System is a passive FIFO consumer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkConfig {
    /// Execution tuning.
    #[serde(default)]
    pub execution: ExecutionConfig,

    /// Hook definitions (loaded but executed by external scripts, not Rust).
    #[serde(default)]
    pub hooks: HooksConfig,

    /// Queue limits.
    #[serde(default)]
    pub queue: QueueConfig,

    /// Retry behaviour.
    #[serde(default)]
    pub retry: RetryConfig,
}

impl Default for WorkConfig {
    fn default() -> Self {
        Self {
            execution: ExecutionConfig::default(),
            hooks: HooksConfig::default(),
            queue: QueueConfig::default(),
            retry: RetryConfig::default(),
        }
    }
}

impl WorkConfig {
    /// Validate the configuration.
    pub fn validate(&self) -> Result<(), String> {
        self.execution.validate()?;
        self.queue.validate()?;
        self.retry.validate()?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// ExecutionConfig
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExecutionConfig {
    /// Whether to auto-decompose items without predefined steps via LLM.
    #[serde(default = "default_true")]
    pub auto_decompose: bool,

    /// Per-step timeout.
    #[serde(
        default = "default_step_timeout",
        with = "duration_secs"
    )]
    pub step_timeout: Duration,

    /// Optional cooldown between work items (0 = none).
    #[serde(
        default,
        with = "duration_secs"
    )]
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
    fn validate(&self) -> Result<(), String> {
        if self.step_timeout.is_zero() {
            return Err("work.execution.step_timeout must be > 0".into());
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// HooksConfig
// ---------------------------------------------------------------------------

/// Hook definitions for each lifecycle point.
///
/// Hooks are loaded from config but **not executed by Rust** — external
/// scripting (Python, shell, etc.) reads these definitions and runs them.
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
    /// Whether to abort the work item when this hook fails.
    #[serde(default)]
    pub abort_on_failure: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HookAction {
    /// Call a built-in tool.
    Tool {
        tool_name: String,
        #[serde(default)]
        params: HashMap<String, serde_json::Value>,
    },
    /// Call LLM with context.
    Llm {
        system_prompt: String,
        #[serde(default = "default_max_tokens")]
        max_tokens: u32,
    },
    /// Emit an event to the global bus.
    EmitEvent {
        event_type: String,
        payload_template: String,
    },
}

fn default_max_tokens() -> u32 {
    1024
}

// ---------------------------------------------------------------------------
// QueueConfig
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QueueConfig {
    /// Maximum queue length (excess items are rejected).
    #[serde(default = "default_max_queue_size")]
    pub max_size: usize,

    /// Whether to use priority ordering (false = pure FIFO).
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
    fn validate(&self) -> Result<(), String> {
        if self.max_size == 0 {
            return Err("work.queue.max_size must be >= 1".into());
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// RetryConfig
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RetryConfig {
    /// Maximum step-level retries before declaring failure.
    #[serde(default = "default_max_step_retries")]
    pub max_step_retries: u32,

    /// Delay between retry attempts.
    #[serde(
        default = "default_retry_delay",
        with = "duration_secs"
    )]
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
    fn validate(&self) -> Result<(), String> {
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
        let config = WorkConfig::default();
        assert!(config.validate().is_ok());
        assert!(config.execution.auto_decompose);
        assert_eq!(config.execution.step_timeout, Duration::from_secs(120));
        assert_eq!(config.execution.inter_item_cooldown, Duration::ZERO);
        assert_eq!(config.queue.max_size, 100);
        assert!(!config.queue.priority_queue);
        assert_eq!(config.retry.max_step_retries, 3);
        assert_eq!(config.retry.retry_delay, Duration::from_secs(5));
    }

    #[test]
    fn zero_step_timeout_is_invalid() {
        let mut config = WorkConfig::default();
        config.execution.step_timeout = Duration::ZERO;
        assert!(config.validate().is_err());
    }

    #[test]
    fn zero_queue_size_is_invalid() {
        let mut config = WorkConfig::default();
        config.queue.max_size = 0;
        assert!(config.validate().is_err());
    }

    #[test]
    fn hooks_default_empty() {
        let config = WorkConfig::default();
        assert!(config.hooks.before_execution.is_empty());
        assert!(config.hooks.before_step.is_empty());
        assert!(config.hooks.after_step.is_empty());
        assert!(config.hooks.after_execution.is_empty());
        assert!(config.hooks.on_success.is_empty());
        assert!(config.hooks.on_failure.is_empty());
    }

    #[test]
    fn config_serde_roundtrip() {
        let config = WorkConfig::default();
        let json = serde_json::to_string(&config).expect("serialize");
        let deser: WorkConfig = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(config.execution.step_timeout, deser.execution.step_timeout);
        assert_eq!(config.queue.max_size, deser.queue.max_size);
    }

    #[test]
    fn hook_definition_serde() {
        let hook = HookDefinition {
            name: "log_start".into(),
            action: HookAction::Tool {
                tool_name: "trace.record".into(),
                params: {
                    let mut m = HashMap::new();
                    m.insert("event".into(), serde_json::json!("work.item.started"));
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
