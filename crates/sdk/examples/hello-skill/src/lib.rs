// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! Hello Skill — an example aman plugin demonstrating the Skill, Tool, and Plugin traits.
#![allow(dead_code)]
//!
//! This plugin provides:
//! - An **EchoTool** that echoes back whatever parameters it receives.
//! - An **EchoSkill** triggered by heartbeat events.
//!
//! Use this as a starting template for your own aman plugins.

use sdk::prelude::*;
use semver::Version;
use std::sync::Arc;

// ---------------------------------------------------------------------------
// EchoTool
// ---------------------------------------------------------------------------

struct EchoTool;

#[async_trait::async_trait]
impl Tool for EchoTool {
    fn name(&self) -> &str {
        "echo"
    }

    fn mode(&self) -> ToolMode {
        ToolMode::Local
    }

    fn parameters(&self) -> &JsonSchema {
        static PARAMS: std::sync::LazyLock<JsonSchema> = std::sync::LazyLock::new(|| {
            JsonSchema::from(serde_json::json!({
                "type": "object",
                "properties": {
                    "message": {"type": "string"},
                    "data": {}
                }
            }))
        });
        &PARAMS
    }

    fn returns(&self) -> &JsonSchema {
        static RETURNS: std::sync::LazyLock<JsonSchema> = std::sync::LazyLock::new(|| {
            JsonSchema::from(serde_json::json!({
                "type": "object",
                "properties": {
                    "echo": {"type": "string"},
                    "params": {"type": "object"}
                }
            }))
        });
        &RETURNS
    }

    async fn execute(&self, params: serde_json::Value, _ctx: ToolContext) -> ToolResult {
        let message = params
            .get("message")
            .and_then(|v| v.as_str())
            .unwrap_or("hello from echo tool");
        Ok(serde_json::json!({
            "echo": message,
            "params": params
        }))
    }
}

// ---------------------------------------------------------------------------
// EchoSkill
// ---------------------------------------------------------------------------

struct EchoSkill {
    version: Version,
    triggers: Vec<TriggerCondition>,
}

impl EchoSkill {
    fn new() -> Self {
        Self {
            version: Version::new(1, 0, 0),
            triggers: vec![TriggerCondition {
                event_types: vec![EventType::Heartbeat, EventType::TimerTick],
                sources: vec![],
                priorities: vec![],
                match_all: false,
            }],
        }
    }
}

#[async_trait::async_trait]
impl Skill for EchoSkill {
    fn name(&self) -> &str {
        "echo-skill"
    }

    fn version(&self) -> &Version {
        &self.version
    }

    fn description(&self) -> &str {
        "A skill that echoes heartbeat events"
    }

    fn triggers(&self) -> &[TriggerCondition] {
        &self.triggers
    }

    async fn execute(&self, event: Event, _ctx: SkillContext) -> AmanResult<()> {
        println!(
            "[echo-skill] received event: type={:?}, source={}, payload={}",
            event.event_type,
            event.source,
            event.payload,
        );
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// HelloPlugin
// ---------------------------------------------------------------------------

struct HelloPlugin {
    version: Version,
}

impl HelloPlugin {
    fn new() -> Self {
        Self {
            version: Version::new(1, 0, 0),
        }
    }
}

#[async_trait::async_trait]
impl Plugin for HelloPlugin {
    fn name(&self) -> &str {
        "hello-plugin"
    }

    fn version(&self) -> &Version {
        &self.version
    }

    fn dependencies(&self) -> &[PluginDependency] {
        &[]
    }

    async fn on_load(&mut self, _ctx: PluginContext) -> AmanResult<()> {
        println!("[hello-plugin] loaded");
        Ok(())
    }

    async fn on_unload(&mut self) -> AmanResult<()> {
        println!("[hello-plugin] unloaded");
        Ok(())
    }

    async fn on_dependency_unloading(&self, _dep_name: &str) -> AmanResult<()> {
        Ok(())
    }

    fn event_sources(&self) -> Vec<Arc<dyn EventSource>> {
        vec![]
    }

    fn skills(&self) -> Vec<Arc<dyn Skill>> {
        vec![Arc::new(EchoSkill::new())]
    }

    fn tools(&self) -> Vec<Arc<dyn Tool>> {
        vec![Arc::new(EchoTool)]
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn echo_tool_echos_message() {
        let tool = EchoTool;
        assert_eq!(tool.name(), "echo");
        assert_eq!(tool.mode(), ToolMode::Local);
    }

    #[tokio::test]
    async fn echo_skill_responds_to_heartbeat() {
        let skill = EchoSkill::new();
        assert_eq!(skill.name(), "echo-skill");
        assert_eq!(skill.version(), &Version::new(1, 0, 0));

        let trigger_types: Vec<_> = skill
            .triggers()
            .iter()
            .flat_map(|t| t.event_types.clone())
            .collect();
        assert!(trigger_types.contains(&EventType::Heartbeat));
        assert!(trigger_types.contains(&EventType::TimerTick));

        let event = Event::new("timer:heartbeat", EventType::Heartbeat, serde_json::json!({"beat": 1}));
        let ctx = SkillContext {
            base: BaseContext::new(TraceId::new()),
            skill_name: Some("echo-skill".to_owned()),
            soul_name: None,
        };
        skill.execute(event, ctx).await.expect("execute should succeed");
    }

    #[test]
    fn hello_plugin_exposes_skill_and_tool() {
        let plugin = HelloPlugin::new();
        assert_eq!(plugin.name(), "hello-plugin");

        let skills = plugin.skills();
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].name(), "echo-skill");

        let tools = plugin.tools();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name(), "echo");

        assert!(plugin.event_sources().is_empty());
        assert!(plugin.dependencies().is_empty());
    }

    #[tokio::test]
    async fn plugin_lifecycle_hooks_succeed() {
        let mut plugin = HelloPlugin::new();
        let ctx = PluginContext {
            base: BaseContext::new(TraceId::new()),
            plugin_name: Some("hello-plugin".to_owned()),
            resource_tracker: Default::default(),
        };
        plugin.on_load(ctx).await.expect("on_load should succeed");
        plugin.on_unload().await.expect("on_unload should succeed");
    }
}
