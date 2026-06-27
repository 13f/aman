// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! Workflow compiler — converts team stages into a WorkflowDef.
//!
//! Architecture ref: docs/team-architect.md §10

use crate::config::TeamConfig;
use workflow::{ErrorRecovery, StateDef, StateTimeout, Transition, TransitionFrom, TransitionTo, WorkflowDef};

/// Compile a TeamConfig into a WorkflowDef suitable for the WorkflowEngine.
pub fn compile_team_workflow(config: &TeamConfig) -> WorkflowDef {
    let mut states = Vec::new();
    let mut transitions = Vec::new();
    let mut state_timeouts = Vec::new();

    for stage in &config.stages {
        states.push(StateDef {
            name: stage.id.clone(),
        });

        // Build state timeout if the stage has an assignment policy with a timeout
        if let Some(ref policy) = stage.assignment_policy
            && policy.execution_timeout_minutes > 0
        {
            state_timeouts.push(StateTimeout {
                state: stage.id.clone(),
                timeout_ms: policy.execution_timeout_minutes * 60 * 1000,
                on_timeout: TransitionTo::Specific("team:work_item.failed".to_string()),
                on_timeout_alert: Some(format!(
                    "work item in stage '{}' timed out after {} minutes",
                    stage.id, policy.execution_timeout_minutes
                )),
            });
            }

        // Build transitions
        for next_id in &stage.allowed_next {
            transitions.push(Transition {
                from: TransitionFrom::Specific(stage.id.clone()),
                event: format!("team:stage.{}.{}", stage.id, next_id),
                to: TransitionTo::Specific(next_id.clone()),
                guard: None, // Safety gate guards are applied at runtime by the handler
                on_fail: None,
                action: None,
                on_action_failure: None,
            });
        }
    }

    let final_states: Vec<String> = config
        .stages
        .iter()
        .filter(|s| s.allowed_next.is_empty())
        .map(|s| s.id.clone())
        .collect();

    let initial_state = if config.initial_stage.is_empty() {
        config
            .stages
            .first()
            .map(|s| s.id.clone())
            .unwrap_or_default()
    } else {
        config.initial_stage.clone()
    };

    // Use the last stage with no transitions as the error state, fall back to initial
    let error_state = final_states
        .first()
        .cloned()
        .unwrap_or_else(|| initial_state.clone());

    WorkflowDef {
        name: format!("team-{}", config.team.name),
        states,
        initial_state,
        final_states,
        error_state,
        transitions,
        state_timeouts,
        error_recovery: ErrorRecovery::default(),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{AssignmentPolicy, Stage, TeamConfig, TeamMeta};

    fn test_config() -> TeamConfig {
        TeamConfig {
            team: TeamMeta {
                name: "Test".into(),
                description: String::new(),
            },
            members: Vec::new(),
            stages: vec![
                Stage {
                    id: "backlog".into(),
                    name: "待办".into(),
                    order: 1,
                    allowed_next: vec!["wip".into()],
                    description: None,
                    assignment_policy: None,
                },
                Stage {
                    id: "wip".into(),
                    name: "处理中".into(),
                    order: 2,
                    allowed_next: vec!["review".into(), "backlog".into()],
                    description: None,
                    assignment_policy: Some(AssignmentPolicy {
                        auto_assign: true,
                        required_capabilities: vec!["code".into()],
                        execution_timeout_minutes: 120,
                        dispatch_strategy: Default::default(),
                    }),
                },
                Stage {
                    id: "review".into(),
                    name: "审核".into(),
                    order: 3,
                    allowed_next: vec!["done".into(), "wip".into()],
                    description: None,
                    assignment_policy: None,
                },
                Stage {
                    id: "done".into(),
                    name: "完成".into(),
                    order: 4,
                    allowed_next: vec![],
                    description: None,
                    assignment_policy: None,
                },
            ],
            safety_gates: Default::default(),
            initial_stage: "backlog".into(),
            context_files: Vec::new(),
            work_dir: None,
        }
    }

    #[test]
    fn compile_basic_workflow() {
        let config = test_config();
        let def = compile_team_workflow(&config);

        assert_eq!(def.name, "team-Test");
        assert_eq!(def.states.len(), 4);
        assert_eq!(def.initial_state, "backlog");
        assert_eq!(def.final_states, vec!["done"]);

        // transitions: backlog→wip, wip→review, wip→backlog, review→done, review→wip
        assert_eq!(def.transitions.len(), 5);
    }

    #[test]
    fn stages_with_timeout_policy_get_state_timeout() {
        let config = test_config();
        let def = compile_team_workflow(&config);

        // wip has timeout because it has assignment_policy with execution_timeout_minutes
        let wip_timeout = def
            .state_timeouts
            .iter()
            .find(|t| t.state == "wip")
            .unwrap();
        assert_eq!(wip_timeout.timeout_ms, 120 * 60 * 1000);
        assert!(wip_timeout.on_timeout_alert.is_some());
    }

    #[test]
    fn stage_without_assignment_policy_has_no_timeout() {
        let config = test_config();
        let def = compile_team_workflow(&config);

        // backlog has no assignment_policy, so no timeout entry
        let backlog_timeout = def.state_timeouts.iter().find(|t| t.state == "backlog");
        assert!(backlog_timeout.is_none());
    }
}
