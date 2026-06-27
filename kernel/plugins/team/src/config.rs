// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! Team configuration types — YAML deserialization for team.yaml.
//!
//! Architecture ref: docs/team-architect.md §6

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

// ---------------------------------------------------------------------------
// TeamConfig
// ---------------------------------------------------------------------------

/// Root configuration for a team.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeamConfig {
    pub team: TeamMeta,
    #[serde(default)]
    pub members: Vec<TeamMember>,
    #[serde(default)]
    pub stages: Vec<Stage>,
    #[serde(default)]
    pub safety_gates: SafetyGateConfig,
    #[serde(default)]
    pub initial_stage: String,
    #[serde(default)]
    pub context_files: Vec<String>,
    #[serde(default)]
    pub work_dir: Option<PathBuf>,
}

/// Team metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeamMeta {
    pub name: String,
    #[serde(default)]
    pub description: String,
}

// ---------------------------------------------------------------------------
// TeamMember
// ---------------------------------------------------------------------------

/// A member of the team — human or agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeamMember {
    pub id: String,
    #[serde(rename = "type", default)]
    pub member_type: MemberType,
    pub name: String,
    /// Agent profile id (agent members only).
    #[serde(default)]
    pub profile: Option<String>,
    /// Roles: owner, admin (human members only).
    #[serde(default)]
    pub roles: Vec<String>,
    /// Skill tags for work-item capability matching.
    #[serde(default)]
    pub capabilities: Vec<String>,
    /// Autonomy level: autonomous, supervised, or on_mention.
    #[serde(default)]
    pub autonomy: Autonomy,
    /// Stages this member is allowed to work on.
    #[serde(default)]
    pub allowed_stages: Vec<String>,
    /// Maximum queued work items before the scheduler stops dispatching.
    #[serde(default = "default_queue_max")]
    pub queue_max_size: usize,
    /// Optional system-prompt hint injected during execution.
    #[serde(default)]
    pub context_hint: Option<String>,
}

fn default_queue_max() -> usize {
    5
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum MemberType {
    #[default]
    Human,
    Agent,
}

impl MemberType {
    pub fn is_agent(&self) -> bool {
        matches!(self, Self::Agent)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Autonomy {
    /// Agent automatically accepts dispatched work items.
    #[default]
    Autonomous,
    /// Dispatched work items require human approval before execution.
    Supervised,
    /// Agent only responds to explicit @mentions (not used in scheduler v1).
    OnMention,
}

// ---------------------------------------------------------------------------
// Stage
// ---------------------------------------------------------------------------

/// A kanban stage / column in the workflow.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Stage {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub order: u32,
    #[serde(default)]
    pub allowed_next: Vec<String>,
    #[serde(default)]
    pub description: Option<String>,
    /// If present and auto_assign is true, the scheduler dispatches
    /// work items entering this stage to matching agents.
    #[serde(default)]
    pub assignment_policy: Option<AssignmentPolicy>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssignmentPolicy {
    #[serde(default)]
    pub auto_assign: bool,
    #[serde(default)]
    pub required_capabilities: Vec<String>,
    /// Timeout in minutes. Defaults to 120 (2 hours).
    #[serde(default = "default_timeout")]
    pub execution_timeout_minutes: u64,
    #[serde(default)]
    pub dispatch_strategy: DispatchStrategy,
}

fn default_timeout() -> u64 {
    120
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum DispatchStrategy {
    /// Pick the agent with the most overlapping capabilities.
    #[default]
    BestMatch,
    /// Pick the agent with the shortest work queue.
    LeastLoaded,
    /// Pick a random idle agent (queue_length == 0).
    RandomIdle,
}

// ---------------------------------------------------------------------------
// SafetyGateConfig
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SafetyGateConfig {
    /// Regex patterns for dangerous actions that require human confirmation.
    #[serde(default)]
    pub dangerous_actions: Vec<DangerousActionPattern>,
    /// Minimum confidence threshold (0.0–1.0). Agent completions below
    /// this value trigger a safety gate.
    #[serde(default = "default_min_confidence")]
    pub min_confidence: f64,
    /// Maximum autonomous actions an agent can take while no human is online.
    #[serde(default = "default_max_autonomous")]
    pub max_autonomous_actions_without_human: u64,
}

fn default_min_confidence() -> f64 {
    0.7
}

fn default_max_autonomous() -> u64 {
    20
}

impl Default for SafetyGateConfig {
    fn default() -> Self {
        Self {
            dangerous_actions: Vec::new(),
            min_confidence: default_min_confidence(),
            max_autonomous_actions_without_human: default_max_autonomous(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DangerousActionPattern {
    pub pattern: String,
    #[serde(default = "default_true")]
    pub require_human: bool,
}

fn default_true() -> bool {
    true
}

// ---------------------------------------------------------------------------
// TeamConfig helpers
// ---------------------------------------------------------------------------

impl TeamConfig {
    /// Load a TeamConfig from a YAML file.
    pub fn from_file(path: &PathBuf) -> Result<Self, String> {
        let content =
            std::fs::read_to_string(path).map_err(|e| format!("read team.yaml: {e}"))?;
        serde_yaml::from_str(&content).map_err(|e| format!("parse team.yaml: {e}"))
    }

    /// Find a stage by id.
    pub fn find_stage(&self, stage_id: &str) -> Option<&Stage> {
        self.stages.iter().find(|s| s.id == stage_id)
    }

    /// Find a member by id.
    pub fn find_member(&self, member_id: &str) -> Option<&TeamMember> {
        self.members.iter().find(|m| m.id == member_id)
    }

    /// Return all agent members.
    pub fn agents(&self) -> impl Iterator<Item = &TeamMember> {
        self.members.iter().filter(|m| m.member_type.is_agent())
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_minimal_team_config() {
        let yaml = r#"
team:
  name: "Test Team"
"#;
        let config: TeamConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(config.team.name, "Test Team");
        assert!(config.members.is_empty());
        assert!(config.stages.is_empty());
    }

    #[test]
    fn parse_full_team_config() {
        let yaml = r#"
team:
  name: "Aman Core Team"
  description: "Development team"

members:
  - id: "jerin"
    type: human
    name: "Jerin"
    roles: [owner]

  - id: "coder"
    type: agent
    name: "Coder"
    profile: "coder"
    capabilities: [code, refactor, fix]
    autonomy: autonomous
    allowed_stages: ["wip"]
    queue_max_size: 5

stages:
  - id: "backlog"
    name: "待办"
    order: 1
    allowed_next: ["wip"]

  - id: "wip"
    name: "处理中"
    order: 2
    allowed_next: ["review"]
    assignment_policy:
      auto_assign: true
      required_capabilities: [code, refactor]
      execution_timeout_minutes: 120
      dispatch_strategy: best_match

  - id: "review"
    name: "审核"
    order: 3
    allowed_next: ["done"]

  - id: "done"
    name: "完成"
    order: 4
    allowed_next: []

safety_gates:
  dangerous_actions:
    - pattern: "rm -rf"
    - pattern: "git push --force"
  min_confidence: 0.7
  max_autonomous_actions_without_human: 20

initial_stage: "backlog"
context_files:
  - "docs/architecture.md"
"#;
        let config: TeamConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(config.team.name, "Aman Core Team");
        assert_eq!(config.members.len(), 2);
        assert_eq!(config.stages.len(), 4);
        assert_eq!(config.initial_stage, "backlog");

        // Check agent member
        let coder = config.find_member("coder").unwrap();
        assert!(coder.member_type.is_agent());
        assert_eq!(coder.capabilities, vec!["code", "refactor", "fix"]);
        assert_eq!(coder.autonomy, Autonomy::Autonomous);
        assert_eq!(coder.allowed_stages, vec!["wip"]);

        // Check stage with assignment policy
        let wip = config.find_stage("wip").unwrap();
        let policy = wip.assignment_policy.as_ref().unwrap();
        assert!(policy.auto_assign);
        assert_eq!(policy.required_capabilities, vec!["code", "refactor"]);
        assert_eq!(policy.dispatch_strategy, DispatchStrategy::BestMatch);

        // Check safety gates
        assert_eq!(config.safety_gates.dangerous_actions.len(), 2);
        assert_eq!(config.safety_gates.min_confidence, 0.7);

        // Check final stages
        let done = config.find_stage("done").unwrap();
        assert!(done.allowed_next.is_empty());
    }

    #[test]
    fn default_member_type_is_human() {
        let yaml = r#"
team:
  name: "Test"
members:
  - id: "alice"
    name: "Alice"
"#;
        let config: TeamConfig = serde_yaml::from_str(yaml).unwrap();
        let alice = config.find_member("alice").unwrap();
        assert_eq!(alice.member_type, MemberType::Human);
    }

    #[test]
    fn default_autonomy_is_autonomous() {
        let yaml = r#"
team:
  name: "Test"
members:
  - id: "bot"
    type: agent
    name: "Bot"
"#;
        let config: TeamConfig = serde_yaml::from_str(yaml).unwrap();
        let bot = config.find_member("bot").unwrap();
        assert_eq!(bot.autonomy, Autonomy::Autonomous);
    }
}
