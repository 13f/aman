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

use crate::types::{BoredomActivity, BoredomConfig};

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
    ///
    /// `queue_depth` is the total pending event count across all priority
    /// levels. When `work_pressure` is configured, it dynamically scales
    /// the weight of the target tag so a growing backlog increases the
    /// probability of selecting work skills.
    pub async fn try_act(
        &self,
        poll_count: u32,
        agent_id: &str,
        queue_depth: usize,
    ) -> Option<String> {
        if poll_count != self.config.trigger_poll {
            return None;
        }

        let Some(tag) = self.weighted_pick_tag(queue_depth) else {
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
            let run_id = format!("{:016x}", rand::random::<u64>());
            let session_id = format!("{agent_id}_idle_{run_id}");
            let event = Event::new(
                "idle.boredom",
                EventType::MessageReceived,
                json!({
                    "session_id": session_id,
                    "agent_id": agent_id,
                    "text": text,
                    "skill_name": skill_name,
                    "tag": tag,
                    "session_type": "background",
                    "background": true,
                }),
            );
            if let Err(e) = bus.publish(event).await {
                warn!("boredom: failed to publish MessageReceived event: {e}");
            }
        }

        Some(tag)
    }

    /// Weighted random tag selection with optional work-pressure scaling.
    ///
    /// When `work_pressure` is configured and `queue_depth > 0`, the
    /// target tag's weight is multiplied by the pressure curve before
    /// the weighted random draw, making it more likely to be selected
    /// as the backlog grows.
    fn weighted_pick_tag(&self, queue_depth: usize) -> Option<String> {
        // Compute effective weights (base weight × pressure multiplier
        // if work_pressure targets this tag).
        let effective = self.effective_activities(queue_depth);

        let total: f64 = effective.iter().map(|(_, w)| w).sum();
        if total <= 0.0 {
            return None;
        }

        let r: f64 = rand::random();
        let target = r * total;

        let mut acc = 0.0;
        for (activity, weight) in &effective {
            acc += weight;
            if target <= acc {
                return Some(activity.tag.clone());
            }
        }

        effective
            .last()
            .map(|(a, _)| a.tag.clone())
    }

    /// Build the list of `(activity, effective_weight)` pairs, applying
    /// the work-pressure multiplier if configured.
    fn effective_activities(
        &self,
        queue_depth: usize,
    ) -> Vec<(&BoredomActivity, f64)> {
        self.config.activities.iter().map(|activity| {
            let effective = if let Some(ref wp) = self.config.work_pressure
                && activity.tag == wp.target_tag
            {
                activity.weight * wp.mapping.multiplier(queue_depth)
            } else {
                activity.weight
            };
            (activity, effective)
        }).collect()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use kernel::skill::TriggerCondition;
    use kernel::AmanResult;
    use semver::Version;

    use super::*;
    use crate::types::{BoredomActivity, PressureMapping, WorkPressureConfig};

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
                BoredomActivity { tag: "exploration".into(), weight: 0.3 },
            ],
            work_pressure: None,
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
        assert!(actor.try_act(1, "a", 0).await.is_none());
        assert!(actor.try_act(2, "a", 0).await.is_none());
    }

    #[tokio::test]
    async fn idle_tag_returns_none() {
        let called = Arc::new(Mutex::new(false));
        let config = BoredomConfig {
            trigger_poll: 3,
            activities: vec![BoredomActivity { tag: "idle".into(), weight: 1.0 }],
            work_pressure: None,
        };
        let actor = setup_actor(config, "s", vec!["idle"], called);
        assert!(actor.try_act(3, "a", 0).await.is_none());
    }

    #[tokio::test]
    async fn executes_skill_with_tag_and_idle_run() {
        let called = Arc::new(Mutex::new(false));
        let config = BoredomConfig {
            trigger_poll: 3,
            activities: vec![BoredomActivity { tag: "work".into(), weight: 1.0 }],
            work_pressure: None,
        };
        let actor = setup_actor(config, "check-inbox", vec!["work", "idle_run"], Arc::clone(&called));

        // Without a global bus, the skill is selected but no event is published.
        // The tag is still returned for system state update.
        assert_eq!(actor.try_act(3, "a", 0).await, Some("work".into()));
        // Note: execute() is no longer called since we publish MessageReceived instead.
    }

    #[tokio::test]
    async fn skips_skill_without_idle_run_tag() {
        let called = Arc::new(Mutex::new(false));
        let config = BoredomConfig {
            trigger_poll: 3,
            activities: vec![BoredomActivity { tag: "work".into(), weight: 1.0 }],
            work_pressure: None,
        };
        // Skill has "work" tag but NOT "idle_run"
        let actor = setup_actor(config, "no-idle-skill", vec!["work"], called);

        assert!(actor.try_act(3, "a", 0).await.is_none());
    }

    #[tokio::test]
    async fn fun_tag_selects_luck_skill() {
        // Simulate: boredom config has "fun" tag at weight 1.0,
        // and the luck skill has tags [fun, idle_run, bitcoin, ...].
        let config = BoredomConfig {
            trigger_poll: 1,
            activities: vec![BoredomActivity { tag: "fun".into(), weight: 1.0 }],
            work_pressure: None,
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
        let result = actor.try_act(1, "test-agent", 0).await;
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
            work_pressure: None,
        };
        let search = Arc::new(SkillSearch::new());
        let registry = Arc::new(SkillRegistry::new());
        let actor = BoredomActor::new(config, search, registry, None);

        for _ in 0..100 {
            let tag = actor.weighted_pick_tag(0).expect("should pick");
            assert_eq!(tag, "b");
        }
    }

    // ── Work pressure tests ─────────────────────────────────────────

    #[test]
    fn pressure_linear_at_zero_depth_is_identity() {
        let config = BoredomConfig {
            trigger_poll: 1,
            activities: vec![
                BoredomActivity { tag: "idle".into(), weight: 5.0 },
                BoredomActivity { tag: "work".into(), weight: 1.0 },
            ],
            work_pressure: Some(WorkPressureConfig {
                target_tag: "work".into(),
                mapping: PressureMapping::Linear {
                    slope: 0.5,
                    max_multiplier: 10.0,
                },
            }),
        };
        let search = Arc::new(SkillSearch::new());
        let registry = Arc::new(SkillRegistry::new());
        let actor = BoredomActor::new(config, search, registry, None);

        // At depth=0, work weight should still be 1.0 (no boost)
        let effective = actor.effective_activities(0);
        let work_w = effective.iter().find(|(a, _)| a.tag == "work").map(|(_, w)| *w);
        let idle_w = effective.iter().find(|(a, _)| a.tag == "idle").map(|(_, w)| *w);
        assert!((work_w.unwrap() - 1.0).abs() < 0.001);
        assert!((idle_w.unwrap() - 5.0).abs() < 0.001);
    }

    #[test]
    fn pressure_linear_scales_with_depth() {
        let config = BoredomConfig {
            trigger_poll: 1,
            activities: vec![
                BoredomActivity { tag: "idle".into(), weight: 5.0 },
                BoredomActivity { tag: "work".into(), weight: 1.0 },
            ],
            work_pressure: Some(WorkPressureConfig {
                target_tag: "work".into(),
                mapping: PressureMapping::Linear {
                    slope: 0.5,
                    max_multiplier: 10.0,
                },
            }),
        };
        let search = Arc::new(SkillSearch::new());
        let registry = Arc::new(SkillRegistry::new());
        let actor = BoredomActor::new(config, search, registry, None);

        // At depth=4: multiplier = 1.0 + 0.5*4 = 3.0, work weight = 3.0
        let effective = actor.effective_activities(4);
        let work_w = effective.iter().find(|(a, _)| a.tag == "work").map(|(_, w)| *w).unwrap();
        assert!((work_w - 3.0).abs() < 0.001);

        // At depth=10: multiplier = 1.0 + 0.5*10 = 6.0, work weight = 6.0
        let effective = actor.effective_activities(10);
        let work_w = effective.iter().find(|(a, _)| a.tag == "work").map(|(_, w)| *w).unwrap();
        assert!((work_w - 6.0).abs() < 0.001);
    }

    #[test]
    fn pressure_linear_respects_max() {
        let config = BoredomConfig {
            trigger_poll: 1,
            activities: vec![
                BoredomActivity { tag: "idle".into(), weight: 5.0 },
                BoredomActivity { tag: "work".into(), weight: 1.0 },
            ],
            work_pressure: Some(WorkPressureConfig {
                target_tag: "work".into(),
                mapping: PressureMapping::Linear {
                    slope: 1.0,
                    max_multiplier: 5.0,
                },
            }),
        };
        let search = Arc::new(SkillSearch::new());
        let registry = Arc::new(SkillRegistry::new());
        let actor = BoredomActor::new(config, search, registry, None);

        // At depth=100: multiplier would be 101, but capped at 5.0
        let effective = actor.effective_activities(100);
        let work_w = effective.iter().find(|(a, _)| a.tag == "work").map(|(_, w)| *w).unwrap();
        assert!((work_w - 5.0).abs() < 0.001);
    }

    #[test]
    fn pressure_sigmoid_midpoint() {
        let config = BoredomConfig {
            trigger_poll: 1,
            activities: vec![
                BoredomActivity { tag: "idle".into(), weight: 5.0 },
                BoredomActivity { tag: "work".into(), weight: 1.0 },
            ],
            work_pressure: Some(WorkPressureConfig {
                target_tag: "work".into(),
                mapping: PressureMapping::Sigmoid {
                    midpoint: 10.0,
                    steepness: 0.5,
                    max_multiplier: 10.0,
                },
            }),
        };
        let search = Arc::new(SkillSearch::new());
        let registry = Arc::new(SkillRegistry::new());
        let actor = BoredomActor::new(config, search, registry, None);

        // At depth=0: should be close to 1.0 (far below midpoint)
        let effective = actor.effective_activities(0);
        let work_w = effective.iter().find(|(a, _)| a.tag == "work").map(|(_, w)| *w).unwrap();
        assert!(work_w < 1.1, "expected near 1.0, got {work_w}");

        // At depth=10 (midpoint): should be ~5.5 (halfway between 1.0 and 10.0)
        let effective = actor.effective_activities(10);
        let work_w = effective.iter().find(|(a, _)| a.tag == "work").map(|(_, w)| *w).unwrap();
        assert!((work_w - 5.5).abs() < 0.01, "expected ~5.5 at midpoint, got {work_w}");
    }

    #[test]
    fn pressure_no_config_no_effect() {
        // Without work_pressure, all weights stay at base values
        let config = BoredomConfig {
            trigger_poll: 1,
            activities: vec![
                BoredomActivity { tag: "idle".into(), weight: 7.5 },
                BoredomActivity { tag: "work".into(), weight: 1.0 },
            ],
            work_pressure: None,
        };
        let search = Arc::new(SkillSearch::new());
        let registry = Arc::new(SkillRegistry::new());
        let actor = BoredomActor::new(config, search, registry, None);

        // Depth should not matter when work_pressure is None
        for depth in [0, 5, 50, 100] {
            let effective = actor.effective_activities(depth);
            let work_w = effective.iter().find(|(a, _)| a.tag == "work").map(|(_, w)| *w).unwrap();
            assert!((work_w - 1.0).abs() < 0.001, "depth={depth}: expected 1.0, got {work_w}");
        }
    }

    #[test]
    fn pressure_wrong_tag_unchanged() {
        // Work pressure on "work" tag should not affect "fun" tag
        let config = BoredomConfig {
            trigger_poll: 1,
            activities: vec![
                BoredomActivity { tag: "idle".into(), weight: 5.0 },
                BoredomActivity { tag: "work".into(), weight: 1.0 },
                BoredomActivity { tag: "fun".into(), weight: 0.5 },
            ],
            work_pressure: Some(WorkPressureConfig {
                target_tag: "work".into(),
                mapping: PressureMapping::Linear {
                    slope: 0.5,
                    max_multiplier: 10.0,
                },
            }),
        };
        let search = Arc::new(SkillSearch::new());
        let registry = Arc::new(SkillRegistry::new());
        let actor = BoredomActor::new(config, search, registry, None);

        // At depth=10: work boosted, fun unchanged
        let effective = actor.effective_activities(10);
        let work_w = effective.iter().find(|(a, _)| a.tag == "work").map(|(_, w)| *w).unwrap();
        let fun_w = effective.iter().find(|(a, _)| a.tag == "fun").map(|(_, w)| *w).unwrap();
        assert!((work_w - 6.0).abs() < 0.001); // 1.0 + 1.0*0.5*10
        assert!((fun_w - 0.5).abs() < 0.001); // unchanged
    }

    #[test]
    fn pressure_biased_random_selection() {
        // With extreme work pressure, "work" should always be picked
        let config = BoredomConfig {
            trigger_poll: 1,
            activities: vec![
                BoredomActivity { tag: "idle".into(), weight: 1.0 },
                BoredomActivity { tag: "work".into(), weight: 1.0 },
            ],
            work_pressure: Some(WorkPressureConfig {
                target_tag: "work".into(),
                mapping: PressureMapping::Linear {
                    slope: 100.0,       // huge boost at any depth
                    max_multiplier: 1000.0,
                },
            }),
        };
        let search = Arc::new(SkillSearch::new());
        let registry = Arc::new(SkillRegistry::new());
        let actor = BoredomActor::new(config, search, registry, None);

        // With depth=5: work multiplier = 1+100*5=501, idle stays at 1.0
        // work: 501, idle: 1.0, total: 502 → work dominates
        for _ in 0..50 {
            let tag = actor.weighted_pick_tag(5).expect("should pick");
            assert_eq!(tag, "work", "work pressure should make work dominate");
        }
    }
}
