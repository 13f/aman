// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

use crate::context::SkillContext;
use crate::error::AmanResult;
use crate::event::{Event, EventType};
use crate::types::{Priority, SourceId};
use async_trait::async_trait;
use semver::Version;
use serde::{Deserialize, Deserializer, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Default)]
#[serde(default)]
pub struct TriggerCondition {
    pub event_types: Vec<EventType>,
    pub sources: Vec<SourceId>,
    pub priorities: Vec<Priority>,
    pub match_all: bool,
}

/// Helper for deserializing a `TriggerCondition` from a map (struct form).
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct TriggerConditionHelper {
    event_types: Vec<EventType>,
    sources: Vec<SourceId>,
    priorities: Vec<Priority>,
    match_all: bool,
}

impl<'de> Deserialize<'de> for TriggerCondition {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        use serde::de;

        struct TriggerConditionVisitor;

        impl<'de> de::Visitor<'de> for TriggerConditionVisitor {
            type Value = TriggerCondition;

            fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                formatter.write_str("a trigger condition struct or a plain string")
            }

            fn visit_str<E: de::Error>(self, _v: &str) -> Result<Self::Value, E> {
                Ok(TriggerCondition::default())
            }

            fn visit_map<M: de::MapAccess<'de>>(self, map: M) -> Result<Self::Value, M::Error> {
                let helper = TriggerConditionHelper::deserialize(
                    de::value::MapAccessDeserializer::new(map),
                )?;
                Ok(TriggerCondition {
                    event_types: helper.event_types,
                    sources: helper.sources,
                    priorities: helper.priorities,
                    match_all: helper.match_all,
                })
            }
        }

        deserializer.deserialize_any(TriggerConditionVisitor)
    }
}

#[async_trait]
pub trait Skill: Send + Sync {
    fn name(&self) -> &str;
    fn version(&self) -> &Version;
    fn description(&self) -> &str;
    fn triggers(&self) -> &[TriggerCondition];

    async fn execute(&self, event: Event, ctx: SkillContext) -> AmanResult<()>;

    async fn on_load(&mut self) -> AmanResult<()> {
        Ok(())
    }

    async fn on_unload(&mut self) -> AmanResult<()> {
        Ok(())
    }

    /// Drain any in-flight work (per-session queues, open channels, etc.).
    /// Returns the number of drained items (sessions, tasks, etc.).
    /// Called during plugin hot-unload (Phase 4.5) before capability removal.
    /// Default implementation is a no-op returning 0.
    fn drain(&self) -> usize {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::{Skill, TriggerCondition};
    use crate::context::{BaseContext, SkillContext};
    use crate::event::{Event, EventType};
    use crate::types::{Priority, SourceId, TraceId};
    use pollster::block_on;
    use semver::Version;
    use serde_json::json;

    struct DummySkill {
        version: Version,
        triggers: Vec<TriggerCondition>,
    }

    #[async_trait::async_trait]
    impl Skill for DummySkill {
        fn name(&self) -> &str {
            "dummy-skill"
        }

        fn version(&self) -> &Version {
            &self.version
        }

        fn description(&self) -> &str {
            "dummy description"
        }

        fn triggers(&self) -> &[TriggerCondition] {
            &self.triggers
        }

        async fn execute(&self, _event: Event, _ctx: SkillContext) -> crate::error::AmanResult<()> {
            Ok(())
        }
    }

    #[test]
    fn trigger_condition_defaults_to_empty_matcher() {
        let trigger = TriggerCondition::default();
        assert!(trigger.event_types.is_empty());
        assert!(trigger.sources.is_empty());
        assert!(trigger.priorities.is_empty());
        assert!(!trigger.match_all);
    }

    #[test]
    fn skill_default_lifecycle_hooks_succeed() {
        let mut skill = DummySkill {
            version: Version::new(0, 1, 0),
            triggers: vec![TriggerCondition {
                event_types: vec![EventType::TimerTick],
                sources: vec![SourceId::new("timer:test")],
                priorities: vec![Priority::Normal],
                match_all: true,
            }],
        };

        let event = Event::new("timer:test", EventType::TimerTick, json!({}));
        let ctx = SkillContext {
            base: BaseContext::new(TraceId::new()),
            skill_name: Some("dummy-skill".to_owned()),
            soul_name: None,
        };

        block_on(skill.on_load()).expect("on_load succeeds");
        block_on(skill.execute(event, ctx)).expect("execute succeeds");
        block_on(skill.on_unload()).expect("on_unload succeeds");

        assert_eq!(skill.name(), "dummy-skill");
        assert_eq!(skill.version(), &Version::new(0, 1, 0));
        assert_eq!(skill.description(), "dummy description");
        assert_eq!(skill.triggers().len(), 1);
    }
}
