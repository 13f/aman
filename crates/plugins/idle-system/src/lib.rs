//! Idle System Plugin — handles all 7 IdleKind states.
//!
//! Each IdleKind (daze, boredom, sleep, exploration, meditation, waiting, incubation)
//! has a dedicated skill that fires when the idle detector produces a matching event.
//! Skills filter by the `kind` field in the event payload so only the correct skill
//! executes for each idle tick.
//!
//! This plugin is loaded as an InProcess plugin (no WASM/subprocess overhead).

use kernel::prelude::*;
use semver::Version;
use std::sync::Arc;

// ---------------------------------------------------------------------------
// IdleKindSkill — generic skill parameterized by idle kind string
// ---------------------------------------------------------------------------

struct IdleKindSkill {
    name: &'static str,
    kind_str: &'static str,
    version: Version,
    triggers: Vec<TriggerCondition>,
}

impl IdleKindSkill {
    fn new(name: &'static str, kind_str: &'static str) -> Self {
        Self {
            name,
            kind_str,
            version: Version::new(1, 0, 0),
            triggers: vec![TriggerCondition {
                event_types: vec![EventType::Idle],
                sources: vec![],
                priorities: vec![],
                match_all: false,
            }],
        }
    }
}

#[async_trait::async_trait]
impl Skill for IdleKindSkill {
    fn name(&self) -> &str {
        self.name
    }

    fn version(&self) -> &Version {
        &self.version
    }

    fn description(&self) -> &str {
        match self.kind_str {
            "daze" => "First idle state (depth 0). Passive arousal decay — the agent enters quiet baseline.",
            "boredom" => "Idle state (depth 1). Passive arousal decay — agent senses inactivity.",
            "sleep" => "Idle state (depth 3). Engaged arousal decay (0.5×) — memory consolidation.",
            "exploration" => "Idle state (depth 5). Engaged arousal decay (0.0×) — active exploration.",
            "meditation" => "Idle state (depth 10). Engaged arousal decay (0.0×) — deep introspection.",
            "waiting" => "Intermediate idle state. Passive arousal decay — waiting for input.",
            "incubation" => "Deep idle state. Engaged arousal decay (0.1×) — creative incubation.",
            _ => "Idle personality skill",
        }
    }

    fn triggers(&self) -> &[TriggerCondition] {
        &self.triggers
    }

    async fn execute(&self, event: Event, _ctx: SkillContext) -> AmanResult<()> {
        // Filter by kind from payload — only execute if this is our IdleKind
        let Some(event_kind) = event.payload["kind"].as_str() else {
            return Ok(());
        };
        if event_kind != self.kind_str {
            return Ok(());
        }

        let depth = event.payload["depth"].as_u64().unwrap_or(0);
        let duration = event.payload["duration_secs"].as_f64().unwrap_or(0.0);
        let arousal = event.payload["context"]["arousal_level"].as_f64().unwrap_or(0.0);

        tracing::info!(
            depth = depth,
            duration_secs = duration,
            arousal_level = arousal,
            event_id = %event.id,
            idle_kind = self.kind_str,
            "idle personality activated",
        );

        Ok(())
    }
}

// ---------------------------------------------------------------------------
// IdleSystemPlugin
// ---------------------------------------------------------------------------

pub struct IdleSystemPlugin {
    version: Version,
}

impl IdleSystemPlugin {
    pub fn new() -> Self {
        Self {
            version: Version::new(1, 0, 0),
        }
    }
}

impl Default for IdleSystemPlugin {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl Plugin for IdleSystemPlugin {
    fn name(&self) -> &str {
        "idle-system"
    }

    fn version(&self) -> &Version {
        &self.version
    }

    fn dependencies(&self) -> &[PluginDependency] {
        &[]
    }

    async fn on_load(&mut self, _ctx: PluginContext) -> AmanResult<()> {
        tracing::info!("idle-system plugin loaded");
        Ok(())
    }

    async fn on_unload(&mut self) -> AmanResult<()> {
        tracing::info!("idle-system plugin unloaded");
        Ok(())
    }

    async fn on_dependency_unloading(&self, _dep_name: &str) -> AmanResult<()> {
        Ok(())
    }

    fn event_sources(&self) -> Vec<Arc<dyn EventSource>> {
        vec![]
    }

    fn skills(&self) -> Vec<Arc<dyn Skill>> {
        vec![
            Arc::new(IdleKindSkill::new("idle-daze", "daze")),
            Arc::new(IdleKindSkill::new("idle-boredom", "boredom")),
            Arc::new(IdleKindSkill::new("idle-sleep", "sleep")),
            Arc::new(IdleKindSkill::new("idle-exploration", "exploration")),
            Arc::new(IdleKindSkill::new("idle-meditation", "meditation")),
            Arc::new(IdleKindSkill::new("idle-waiting", "waiting")),
            Arc::new(IdleKindSkill::new("idle-incubation", "incubation")),
        ]
    }

    fn tools(&self) -> Vec<Arc<dyn Tool>> {
        vec![]
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idle_system_plugin_exposes_all_seven_skills() {
        let plugin = IdleSystemPlugin::new();
        assert_eq!(plugin.name(), "idle-system");

        let skills = plugin.skills();
        assert_eq!(skills.len(), 7);

        let skill_names: Vec<&str> = skills.iter().map(|s| s.name()).collect();
        assert!(skill_names.contains(&"idle-daze"));
        assert!(skill_names.contains(&"idle-boredom"));
        assert!(skill_names.contains(&"idle-sleep"));
        assert!(skill_names.contains(&"idle-exploration"));
        assert!(skill_names.contains(&"idle-meditation"));
        assert!(skill_names.contains(&"idle-waiting"));
        assert!(skill_names.contains(&"idle-incubation"));

        assert!(plugin.tools().is_empty());
        assert!(plugin.event_sources().is_empty());
        assert!(plugin.dependencies().is_empty());
    }

    #[tokio::test]
    async fn idle_daze_skill_only_activates_on_daze_kind() {
        let skill = IdleKindSkill::new("idle-daze", "daze");
        let ctx = SkillContext {
            base: BaseContext::new(TraceId::new()),
            skill_name: Some("idle-daze".to_owned()),
            soul_name: None,
        };

        // Should execute for "daze"
        let daze_event = Event::new(
            "idle.system",
            EventType::Idle,
            serde_json::json!({"kind": "daze", "depth": 0, "duration_secs": 5.0, "context": {"arousal_level": 0.8}}),
        );
        skill.execute(daze_event, ctx.clone()).await.expect("daze should execute");

        // Should no-op for "boredom"
        let bored_event = Event::new(
            "idle.system",
            EventType::Idle,
            serde_json::json!({"kind": "boredom", "depth": 1, "duration_secs": 5.0, "context": {"arousal_level": 0.7}}),
        );
        skill.execute(bored_event, ctx).await.expect("boredom should no-op");
    }

    #[tokio::test]
    async fn non_idle_event_is_noop() {
        let skill = IdleKindSkill::new("idle-daze", "daze");
        let ctx = SkillContext {
            base: BaseContext::new(TraceId::new()),
            skill_name: Some("idle-daze".to_owned()),
            soul_name: None,
        };

        let msg_event = Event::new(
            "chat:user",
            EventType::MessageReceived,
            serde_json::json!({"text": "hello"}),
        );
        // Should not panic — trigger matching happens before execute()
        assert!(skill.triggers().iter().any(|t| {
            t.event_types.contains(&EventType::Idle)
        }));
        // Execute should not error even though kind field is missing
        skill.execute(msg_event, ctx).await.expect("non-idle event should not error");
    }

    #[tokio::test]
    async fn idle_system_lifecycle_hooks_succeed() {
        let mut plugin = IdleSystemPlugin::new();
        let ctx = PluginContext {
            base: BaseContext::new(TraceId::new()),
            plugin_name: Some("idle-system".to_owned()),
            resource_tracker: Default::default(),
        };
        plugin.on_load(ctx).await.expect("on_load should succeed");
        plugin.on_unload().await.expect("on_unload should succeed");
    }
}
