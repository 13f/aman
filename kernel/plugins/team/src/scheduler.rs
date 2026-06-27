// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! Team scheduler — capability-based work-item dispatch to agents.
//!
//! Architecture ref: docs/team-architect.md §8

use crate::config::{Autonomy, DispatchStrategy, Stage, TeamConfig};
use tracing::{debug, info};
use work::{WorkItem, WorkItemSource};

/// Scheduler that matches work items to agents by capability tags.
///
/// Uses the agent's WorkSystem queue via `push_work_item()` — agents
/// consume work passively; the scheduler only pushes.
pub struct TeamScheduler {
    config: TeamConfig,
}

impl TeamScheduler {
    pub fn new(config: TeamConfig) -> Self {
        Self { config }
    }

    /// Dispatch a work item to the best-matching agent for the given stage.
    ///
    /// Returns the assigned agent id on success, or a human-readable reason on failure.
    pub async fn dispatch(
        &self,
        item: &WorkItem,
        stage: &Stage,
        agents: &[AgentDispatchInfo<'_>],
    ) -> Result<String, String> {
        let policy = stage
            .assignment_policy
            .as_ref()
            .ok_or_else(|| format!("stage '{}' has no assignment_policy", stage.id))?;

        if !policy.auto_assign {
            return Err(format!("stage '{}' does not auto_assign", stage.id));
        }

        // 1. Filter eligible agents
        let candidates: Vec<&AgentDispatchInfo> = agents
            .iter()
            .filter(|a| {
                // Must be at least Autonomous (not OnMention)
                if matches!(a.autonomy, Autonomy::OnMention) {
                    return false;
                }
                // Must be allowed to work on this stage
                if !a.allowed_stages.is_empty() && !a.allowed_stages.iter().any(|s| s == &stage.id) {
                    return false;
                }
                // Must have queue capacity
                if a.queue_length >= a.queue_max_size {
                    debug!(
                        agent_id = %a.agent_id,
                        queue_length = a.queue_length,
                        queue_max = a.queue_max_size,
                        "TeamScheduler: agent queue full — skipping"
                    );
                    return false;
                }
                // Must match at least one required capability
                if !policy.required_capabilities.is_empty()
                    && !a.capabilities.iter().any(|c| policy.required_capabilities.contains(c))
                {
                    return false;
                }
                true
            })
            .collect();

        if candidates.is_empty() {
            return Err(format!(
                "no eligible agent for stage '{}' (required_capabilities: {:?})",
                stage.id, policy.required_capabilities
            ));
        }

        // 2. Apply dispatch strategy
        let target = match policy.dispatch_strategy {
            DispatchStrategy::BestMatch => candidates
                .iter()
                .max_by_key(|a| {
                    a.capabilities
                        .iter()
                        .filter(|c| policy.required_capabilities.contains(c))
                        .count()
                })
                .unwrap(),
            DispatchStrategy::LeastLoaded => candidates
                .iter()
                .min_by_key(|a| a.queue_length)
                .unwrap(),
            DispatchStrategy::RandomIdle => {
                let idle: Vec<_> = candidates.iter().filter(|a| a.queue_length == 0).collect();
                if idle.is_empty() {
                    // Fall back to least loaded
                    candidates.iter().min_by_key(|a| a.queue_length).unwrap()
                } else {
                    // Deterministic pseudo-random: pick based on item id hash
                    let idx = (item.id.to_string().bytes().fold(0u64, |acc, b| {
                        acc.wrapping_mul(31).wrapping_add(b as u64)
                    }) as usize)
                        % idle.len();
                    idle[idx]
                }
            }
        };

        info!(
            agent_id = %target.agent_id,
            work_item_id = %item.id,
            stage = %stage.id,
            strategy = ?policy.dispatch_strategy,
            "TeamScheduler: dispatching work item"
        );

        // 3. Push to agent WorkSystem
        target
            .work_system
            .push_work_item(
                item.clone(),
                WorkItemSource::Kanban {
                    board_id: self.config.team.name.clone(),
                    scheduler: "team".into(),
                },
            )
            .await
            .map_err(|e| format!("push to agent {}: {e:?}", target.agent_id))?;

        Ok(target.agent_id.clone())
    }

    /// Find agents that are eligible for a given stage (without dispatching).
    /// Useful for UI pre-flight checks.
    pub fn eligible_agents<'a>(
        &self,
        stage: &Stage,
        agents: &'a [AgentDispatchInfo<'_>],
    ) -> Vec<&'a AgentDispatchInfo<'a>> {
        let policy = match &stage.assignment_policy {
            Some(p) => p,
            None => return Vec::new(),
        };

        agents
            .iter()
            .filter(|a| {
                if matches!(a.autonomy, Autonomy::OnMention) {
                    return false;
                }
                if !a.allowed_stages.is_empty() && !a.allowed_stages.iter().any(|s| s == &stage.id) {
                    return false;
                }
                if a.queue_length >= a.queue_max_size {
                    return false;
                }
                if !policy.required_capabilities.is_empty()
                    && !a.capabilities.iter().any(|c| policy.required_capabilities.contains(c))
                {
                    return false;
                }
                true
            })
            .collect()
    }
}

// ---------------------------------------------------------------------------
// AgentDispatchInfo
// ---------------------------------------------------------------------------

/// Snapshot of an agent's dispatch-relevant state at scheduling time.
///
/// Callers construct this from the AgentRegistry + WorkSystem before
/// passing to the scheduler.
pub struct AgentDispatchInfo<'a> {
    pub agent_id: String,
    pub capabilities: &'a [String],
    pub autonomy: Autonomy,
    pub allowed_stages: &'a [String],
    pub queue_max_size: usize,
    pub queue_length: usize,
    pub work_system: &'a work::WorkSystem,
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{AssignmentPolicy, Autonomy, Stage, TeamConfig, TeamMeta};
    use std::sync::Arc;
    use work::{WorkItem, WorkItemId};
    use event_bus::{InMemoryBus, InMemoryBusConfig};

    fn make_work_item(title: &str) -> WorkItem {
        WorkItem {
            id: WorkItemId::new(),
            title: title.to_string(),
            description: String::new(),
            steps: None,
            priority: work::Priority::Normal,
            timeout: None,
            context: Default::default(),
            notify_on_complete: true,
            created_at: kernel::types::Timestamp::now(),
        }
    }

    fn make_config() -> TeamConfig {
        TeamConfig {
            team: TeamMeta { name: "test".into(), description: String::new() },
            members: Vec::new(),
            stages: vec![Stage {
                id: "wip".into(),
                name: "WIP".into(),
                order: 1,
                allowed_next: vec![],
                description: None,
                assignment_policy: Some(AssignmentPolicy {
                    auto_assign: true,
                    required_capabilities: vec!["code".into()],
                    execution_timeout_minutes: 60,
                    dispatch_strategy: DispatchStrategy::BestMatch,
                }),
            }],
            safety_gates: Default::default(),
            initial_stage: "backlog".into(),
            context_files: Vec::new(),
            work_dir: None,
        }
    }

    fn make_bus() -> Arc<dyn event_bus::EventBus> {
        Arc::new(InMemoryBus::new(InMemoryBusConfig::default()))
    }

    #[tokio::test]
    async fn dispatch_best_match() {
        let config = make_config();
        let bus = make_bus();
        let ws1 = work::WorkSystem::new("coder", Default::default(), Arc::clone(&bus), Arc::clone(&bus), None);
        let ws2 = work::WorkSystem::new("reviewer", Default::default(), Arc::clone(&bus), Arc::clone(&bus), None);

        let coder_caps = ["code".to_string(), "refactor".to_string()];
        let coder_stages = ["wip".to_string()];
        let reviewer_caps = ["review".to_string()];
        let reviewer_stages = ["wip".to_string()];

        let agents = vec![
            AgentDispatchInfo {
                agent_id: "coder".into(),
                capabilities: &coder_caps,
                autonomy: Autonomy::Autonomous,
                allowed_stages: &coder_stages,
                queue_max_size: 5,
                queue_length: 0,
                work_system: &ws1,
            },
            AgentDispatchInfo {
                agent_id: "reviewer".into(),
                capabilities: &reviewer_caps,
                autonomy: Autonomy::Autonomous,
                allowed_stages: &reviewer_stages,
                queue_max_size: 5,
                queue_length: 0,
                work_system: &ws2,
            },
        ];

        let scheduler = TeamScheduler::new(config);
        let stage = scheduler.config.find_stage("wip").unwrap();
        let item = make_work_item("Fix bug");

        let result = scheduler.dispatch(&item, stage, &agents).await;
        assert!(result.is_ok(), "dispatch failed: {:?}", result.err());
        assert_eq!(result.unwrap(), "coder"); // coder has best match for "code"
    }

    #[tokio::test]
    async fn dispatch_rejects_full_queue() {
        let config = make_config();
        let bus = make_bus();
        let ws = work::WorkSystem::new("coder", Default::default(), Arc::clone(&bus), Arc::clone(&bus), None);

        let caps = ["code".to_string()];
        let no_stages: [String; 0] = [];

        let agents = vec![AgentDispatchInfo {
            agent_id: "coder".into(),
            capabilities: &caps,
            autonomy: Autonomy::Autonomous,
            allowed_stages: &no_stages,
            queue_max_size: 1,
            queue_length: 1, // queue is full
            work_system: &ws,
        }];

        let scheduler = TeamScheduler::new(config);
        let stage = scheduler.config.find_stage("wip").unwrap();
        let item = make_work_item("Fix bug");

        let result = scheduler.dispatch(&item, stage, &agents).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("no eligible agent"));
    }

    #[tokio::test]
    async fn dispatch_rejects_on_mention_agent() {
        let config = make_config();
        let bus = make_bus();
        let ws = work::WorkSystem::new("helper", Default::default(), Arc::clone(&bus), Arc::clone(&bus), None);

        let caps = ["code".to_string()];
        let no_stages: [String; 0] = [];

        let agents = vec![AgentDispatchInfo {
            agent_id: "helper".into(),
            capabilities: &caps,
            autonomy: Autonomy::OnMention,
            allowed_stages: &no_stages,
            queue_max_size: 5,
            queue_length: 0,
            work_system: &ws,
        }];

        let scheduler = TeamScheduler::new(config);
        let stage = scheduler.config.find_stage("wip").unwrap();
        let item = make_work_item("Fix bug");

        let result = scheduler.dispatch(&item, stage, &agents).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("no eligible agent"));
    }

    #[test]
    fn eligible_agents_filters_correctly() {
        let config = make_config();
        let bus = make_bus();
        let ws = work::WorkSystem::new("coder", Default::default(), Arc::clone(&bus), Arc::clone(&bus), None);

        let coder_caps = ["code".to_string()];
        let designer_caps = ["design".to_string()];
        let no_stages: [String; 0] = [];

        let agents = vec![
            AgentDispatchInfo {
                agent_id: "coder".into(),
                capabilities: &coder_caps,
                autonomy: Autonomy::Autonomous,
                allowed_stages: &no_stages,
                queue_max_size: 5,
                queue_length: 0,
                work_system: &ws,
            },
            AgentDispatchInfo {
                agent_id: "designer".into(),
                capabilities: &designer_caps,
                autonomy: Autonomy::Autonomous,
                allowed_stages: &no_stages,
                queue_max_size: 5,
                queue_length: 0,
                work_system: &ws,
            },
        ];

        let scheduler = TeamScheduler::new(config);
        let stage = scheduler.config.find_stage("wip").unwrap();
        let eligible = scheduler.eligible_agents(stage, &agents);
        assert_eq!(eligible.len(), 1);
        assert_eq!(eligible[0].agent_id, "coder");
    }
}
