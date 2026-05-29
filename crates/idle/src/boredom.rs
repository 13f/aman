// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! BoredomActor — weighted random tag selection, filtered skill lookup,
//! random idle-prompt pick, and MessageReceived event publication so the
//! agent harness processes the skill through the ReAct loop.
//!
//! When the agent has been in Boredom for `trigger_poll` consecutive polls,
//! a weighted random tag is selected. Skills matching the tag AND the
//! `idle_run` marker tag are filtered, one is picked at random, an
//! `idle_prompt` is selected from the skill's SKILL.md frontmatter (with
//! `{agent_id}` substitution), and a `MessageReceived` event is published
//! to the global bus so the agent harness picks it up.

use std::sync::Arc;

use event_bus::EventBus;
use kernel::event::{Event, EventType};
use rand::Rng;
use serde_json::json;
use skill::{SkillRegistry, SkillSearch};
use tracing::{info, warn};

use crate::types::BoredomConfig;

/// Sentinel tag — skills must also carry this tag to be eligible for
/// boredom-triggered execution.
const IDLE_RUN_TAG: &str = "idle_run";

/// Picks and executes a random skill based on boredom configuration.
pub struct BoredomActor {
    config: BoredomConfig,
    skill_index: Arc<SkillSearch>,
    skill_registry: Arc<SkillRegistry>,
    global_bus: Option<Arc<dyn EventBus>>,
}

impl BoredomActor {
    /// Create a new BoredomActor.
    #[must_use]
    pub fn new(
        config: BoredomConfig,
        skill_index: Arc<SkillSearch>,
        skill_registry: Arc<SkillRegistry>,
        global_bus: Option<Arc<dyn EventBus>>,
    ) -> Self {
        Self {
            config,
            skill_index,
            skill_registry,
            global_bus,
        }
    }

    /// Try to pick a skill and publish a MessageReceived event for the
    /// agent harness to process. Returns `Some(tag)` if a skill was
    /// selected (the caller should use the tag to update system state).
    /// Returns `None` when:
    /// - `poll_count` != `trigger_poll`
    /// - Weighted pick lands on "idle"
    /// - No skills match the tag + `idle_run` filter
    /// - Skill is not found in the registry
    pub async fn try_act(&self, poll_count: u32, agent_id: &str) -> Option<String> {
        if poll_count != self.config.trigger_poll {
            return None;
        }

        let Some(tag) = self.weighted_pick_tag() else {
            return None;
        };
        info!("random_hit:tag: {}", tag);

        if tag == "idle" {
            return None;
        }

        // Filter: skills must carry both the selected tag AND idle_run.
        let candidates: Vec<_> = self
            .skill_index
            .search_by_tag(IDLE_RUN_TAG)
            .into_iter()
            .filter(|s| s.tags.iter().any(|t| *t == tag))
            .collect();

        if candidates.is_empty() {
            return None;
        }

        let idx = rand::thread_rng().gen_range(0..candidates.len());
        let skill_name = candidates[idx].name.clone();

        let Some(skill) = self.skill_registry.get(&skill_name) else {
            return None;
        };

        info!(
            "random_hit:skill: {} tag={} agent={} poll={}",
            skill_name, tag, agent_id, poll_count
        );

        // Pick a random idle_prompt and substitute agent_id.
        let idle_prompt = self
            .skill_registry
            .idle_prompts(&skill_name)
            .and_then(|prompts| {
                let i = rand::thread_rng().gen_range(0..prompts.len());
                Some(prompts[i].replace("{agent_id}", agent_id))
            });

        let text = match idle_prompt {
            Some(prompt) => {
                let body = self.skill_registry.skill_body(&skill_name);
                match body {
                    Some(b) => format!(
                        "[IDLE ACTION] {prompt}\n\n\
                         --- SKILL METHODOLOGY ---\n\
                         {b}\n\
                         --- END SKILL ---\n\n\
                         Execute the action above using the skill's methodology. \
                         Do not skip or abbreviate any prescribed stage."
                    ),
                    None => format!(
                        "[IDLE ACTION] {prompt}\n\n\
                         Execute the action above using your available tools and \
                         knowledge. Be thorough and complete the task."
                    ),
                }
            }
            None => {
                // No idle_prompt configured — fall back to a generic action
                // based on the skill's description.
                format!(
                    "[IDLE ACTION] Execute the skill \"{skill_name}\": {}.\n\
                     Use your available tools and follow your standard methodology.",
                    skill.description()
                )
            }
        };

        // Publish MessageReceived event so the agent harness picks it up
        // and runs it through the ReAct loop — same path as "/skill name prompt".
        if let Some(ref bus) = self.global_bus {
            let session_id = format!("{agent_id}:idle");
            let event = Event::new(
                "idle.boredom",
                EventType::MessageReceived,
                json!({
                    "session_id": session_id,
                    "agent_id": agent_id,
                    "text": text,
                    "skill_name": skill_name,
                    "tag": tag,
                }),
            );
            if let Err(e) = bus.publish(event).await {
                warn!("boredom: failed to publish MessageReceived event: {e}");
            }
        }

        Some(tag)
    }

    /// Weighted random tag selection.
    fn weighted_pick_tag(&self) -> Option<String> {
        let total: f64 = self.config.activities.iter().map(|a| a.weight).sum();
        if total <= 0.0 {
            return None;
        }

        let r: f64 = rand::random();
        let target = r * total;

        let mut acc = 0.0;
        for activity in &self.config.activities {
            acc += activity.weight;
            if target <= acc {
                return Some(activity.tag.clone());
            }
        }

        self.config.activities.last().map(|a| a.tag.clone())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use kernel::skill::TriggerCondition;
    use kernel::AmanResult;
    use semver::Version;

    use super::*;
    use crate::types::BoredomActivity;

    struct TestSkill {
        name: String,
        called: Arc<Mutex<bool>>,
    }

    impl TestSkill {
        fn new(name: &str, called: Arc<Mutex<bool>>) -> Self {
            Self { name: name.into(), called }
        }
    }

    #[async_trait::async_trait]
    impl kernel::skill::Skill for TestSkill {
        fn name(&self) -> &str { &self.name }
        fn version(&self) -> &Version {
            static V: std::sync::LazyLock<Version> =
                std::sync::LazyLock::new(|| Version::new(0, 1, 0));
            &V
        }
        fn description(&self) -> &str { "test skill" }
        fn triggers(&self) -> &[TriggerCondition] { &[] }

        async fn execute(&self, _event: Event, _ctx: kernel::context::SkillContext) -> AmanResult<()> {
            *self.called.lock().expect("called lock") = true;
            Ok(())
        }
    }

    fn test_config() -> BoredomConfig {
        BoredomConfig {
            trigger_poll: 3,
            activities: vec![
                BoredomActivity { tag: "idle".into(), weight: 7.5 },
                BoredomActivity { tag: "work".into(), weight: 1.0 },
            ],
        }
    }

    fn setup_actor(
        config: BoredomConfig,
        skill_name: &str,
        tags: Vec<&str>,
        called: Arc<Mutex<bool>>,
    ) -> BoredomActor {
        let search = Arc::new(SkillSearch::new());
        search.index_skill(skill::IndexedSkill {
            name: skill_name.into(),
            version: "0.1.0".into(),
            description: "test".into(),
            tags: tags.iter().map(|t| t.to_string()).collect(),
        });

        let registry = Arc::new(SkillRegistry::new());
        registry
            .register(Arc::new(TestSkill::new(skill_name, called)))
            .expect("register");

        BoredomActor::new(config, search, registry, None)
    }

    #[tokio::test]
    async fn returns_none_when_poll_mismatch() {
        let called = Arc::new(Mutex::new(false));
        let actor = setup_actor(test_config(), "s", vec!["work", "idle_run"], called);
        assert!(actor.try_act(1, "a").await.is_none());
        assert!(actor.try_act(2, "a").await.is_none());
    }

    #[tokio::test]
    async fn idle_tag_returns_none() {
        let called = Arc::new(Mutex::new(false));
        let config = BoredomConfig {
            trigger_poll: 3,
            activities: vec![BoredomActivity { tag: "idle".into(), weight: 1.0 }],
        };
        let actor = setup_actor(config, "s", vec!["idle"], called);
        assert!(actor.try_act(3, "a").await.is_none());
    }

    #[tokio::test]
    async fn executes_skill_with_tag_and_idle_run() {
        let called = Arc::new(Mutex::new(false));
        let config = BoredomConfig {
            trigger_poll: 3,
            activities: vec![BoredomActivity { tag: "work".into(), weight: 1.0 }],
        };
        let actor = setup_actor(config, "check-inbox", vec!["work", "idle_run"], Arc::clone(&called));

        // Without a global bus, the skill is selected but no event is published.
        // The tag is still returned for system state update.
        assert_eq!(actor.try_act(3, "a").await, Some("work".into()));
        // Note: execute() is no longer called since we publish MessageReceived instead.
    }

    #[tokio::test]
    async fn skips_skill_without_idle_run_tag() {
        let called = Arc::new(Mutex::new(false));
        let config = BoredomConfig {
            trigger_poll: 3,
            activities: vec![BoredomActivity { tag: "work".into(), weight: 1.0 }],
        };
        // Skill has "work" tag but NOT "idle_run"
        let actor = setup_actor(config, "no-idle-skill", vec!["work"], called);

        assert!(actor.try_act(3, "a").await.is_none());
    }

    #[tokio::test]
    async fn fun_tag_selects_luck_skill() {
        // Simulate: boredom config has "fun" tag at weight 1.0,
        // and the luck skill has tags [fun, idle_run, bitcoin, ...].
        let config = BoredomConfig {
            trigger_poll: 1,
            activities: vec![BoredomActivity { tag: "fun".into(), weight: 1.0 }],
        };
        let search = Arc::new(SkillSearch::new());
        // Mirror the real luck skill tags
        search.index_skill(skill::IndexedSkill {
            name: "lifecycle/luck".into(),
            version: "1.0.0".into(),
            description: "Bitcoin dormant address lottery".into(),
            tags: vec![
                "idle_run".into(), "bitcoin".into(), "btc".into(),
                "fun".into(), "game".into(), "lottery".into(),
                "luck".into(), "crypto".into(),
            ],
        });
        // Also index a non-fun skill to prove filtering works
        search.index_skill(skill::IndexedSkill {
            name: "investment/btc-bottom-model".into(),
            version: "1.0.0".into(),
            description: "BTC bottom model".into(),
            tags: vec!["investment".into(), "btc".into()],
        });

        let registry = Arc::new(SkillRegistry::new());
        // Register the skill so try_act can look it up.
        registry
            .register(Arc::new(TestSkill::new(
                "lifecycle/luck",
                Arc::new(Mutex::new(false)),
            )))
            .expect("register");
        let actor = BoredomActor::new(config, search, registry, None);

        // poll_count == trigger_poll (1), tag "fun" → should find luck skill
        let result = actor.try_act(1, "test-agent").await;
        assert_eq!(result, Some("fun".into()));
    }

    #[test]
    fn weighted_pick_respects_distribution() {
        let config = BoredomConfig {
            trigger_poll: 1,
            activities: vec![
                BoredomActivity { tag: "a".into(), weight: 0.0 },
                BoredomActivity { tag: "b".into(), weight: 1.0 },
            ],
        };
        let search = Arc::new(SkillSearch::new());
        let registry = Arc::new(SkillRegistry::new());
        let actor = BoredomActor::new(config, search, registry, None);

        for _ in 0..100 {
            let tag = actor.weighted_pick_tag().expect("should pick");
            assert_eq!(tag, "b");
        }
    }
}
