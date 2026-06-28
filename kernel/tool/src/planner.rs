#![forbid(unsafe_code)]
// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! Planner tool — structured, persistent plan state for long-horizon agent tasks.
//!
//! Provides create / update / query operations on task plans with progress
//! tracking, stall detection primitives, and cross-session resume support.
//!
//! ## File layout
//!
//! ```text
//! ~/.aman/plans/
//!   {plan_id}.plan            ← task DAG + goal + milestones + directions
//!   {plan_id}.progress        ← execution checkpoint (iteration, stale_count, …)
//!   {plan_id}.findings.jsonl  ← append-only intermediate findings
//! ```

use kernel::context::ToolContext;
use kernel::event::{Event, EventType};
use kernel::schema::JsonSchema;
use kernel::tool::Tool;
use kernel::types::ToolMode;
use kernel::{AmanResult, Error};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::sync::LazyLock;

// ---------------------------------------------------------------------------
// Domain types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum TaskStatus {
    #[default]
    Pending,
    InProgress,
    Completed,
    Failed,
    Blocked,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Milestone {
    /// Short machine-readable id, e.g. "m1", "root-cause"
    id: String,
    /// Human description of what this milestone means.
    description: String,
    /// How to verify this milestone has been met (test command, observable behaviour, …).
    verification: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Direction {
    /// Short machine-readable id, e.g. "d1", "heap-profile"
    id: String,
    /// Human description of the approach.
    description: String,
    /// Optional key-value parameters that parameterise the direction.
    /// Each value describes what the parameter means (for LLM and human readers).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    parameters: Option<HashMap<String, String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct TaskDef {
    /// Short machine-readable id, e.g. "1", "profile-memory"
    id: String,
    /// Human-readable title.
    title: String,
    /// What this task should accomplish.
    description: String,
    /// Task ids that must complete before this one can start.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    depends_on: Vec<String>,
    /// Optional link to a milestone this task contributes to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    milestone_id: Option<String>,
    #[serde(default)]
    status: TaskStatus,
    /// Summary written when the task completes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    result: Option<String>,
    /// Directions already explored for this task (anti-loop).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    directions_tried: Vec<Direction>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PlanState {
    plan_id: String,
    goal: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    milestones: Vec<Milestone>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    success_criteria: Option<String>,
    #[serde(default = "default_round_cap")]
    round_cap: u32,
    #[serde(default)]
    tasks: Vec<TaskDef>,
    created_at: String,
    updated_at: String,
}

fn default_round_cap() -> u32 {
    15
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct ProgressState {
    plan_id: String,
    #[serde(default)]
    iteration: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    current_task_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    current_milestone_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    current_direction_id: Option<String>,
    #[serde(default)]
    stale_count: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_progress_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_session_id: Option<String>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    retry_counts: HashMap<String, u32>,
}

// Thread-local override set by tests so each test thread has its own temp directory.
// Checked before `$HOME` in `plans_dir()`.
thread_local! {
    static PLANS_DIR_OVERRIDE: std::cell::RefCell<Option<PathBuf>> = const { std::cell::RefCell::new(None) };
}

pub struct PlannerTool;

impl PlannerTool {
    /// Set an override plans directory for the current thread (used in tests).
    #[cfg(test)]
    pub fn set_test_plans_dir(dir: &std::path::Path) {
        PLANS_DIR_OVERRIDE.with(|cell| *cell.borrow_mut() = Some(dir.to_owned()));
    }

    /// Clear the current thread's override.
    #[cfg(test)]
    pub fn clear_test_plans_dir() {
        PLANS_DIR_OVERRIDE.with(|cell| *cell.borrow_mut() = None);
    }
}

/// Operations supported by the planner tool.
const OP_CREATE: &str = "create";
const OP_SET_TASKS: &str = "set_tasks";
const OP_START: &str = "start";
const OP_COMPLETE: &str = "complete";
const OP_FAIL: &str = "fail";
const OP_APPEND_FINDING: &str = "append_finding";
const OP_RECORD_DIRECTION: &str = "record_direction";
const OP_INCREMENT_STALE: &str = "increment_stale";
const OP_STATUS: &str = "status";
const OP_RESUME: &str = "resume";

const VALID_OPERATIONS: &[&str] = &[
    OP_CREATE,
    OP_SET_TASKS,
    OP_START,
    OP_COMPLETE,
    OP_FAIL,
    OP_APPEND_FINDING,
    OP_RECORD_DIRECTION,
    OP_INCREMENT_STALE,
    OP_STATUS,
    OP_RESUME,
];

#[async_trait::async_trait]
impl Tool for PlannerTool {
    fn name(&self) -> &str {
        "planner"
    }

    fn description(&self) -> &str {
        "Create, update, and query structured task plans with progress tracking. \
         Operations: create (initialize a plan with goal), set_tasks (write decomposed \
         task list), start/complete/fail (task lifecycle), append_finding (save \
         intermediate results), record_direction (anti-loop tracking), increment_stale \
         (stall signal), status (full view), resume (status + explicit next action)."
    }

    fn mode(&self) -> ToolMode {
        ToolMode::Local
    }

    fn execution_model(&self) -> kernel::types::ExecutionModel {
        kernel::types::ExecutionModel::Stateful
    }

    fn parameters(&self) -> &JsonSchema {
        static PARAMS: LazyLock<JsonSchema> = LazyLock::new(|| {
            JsonSchema::from(json!({
                "type": "object",
                "required": ["operation"],
                "properties": {
                    "operation": {
                        "type": "string",
                        "description": "Which planner action to perform.",
                        "enum": VALID_OPERATIONS
                    },
                    "plan_id": {
                        "type": "string",
                        "description": "Plan identifier. Required for all operations except create (where it is auto-generated)."
                    },
                    "goal": {
                        "type": "string",
                        "description": "create: one-sentence description of what the plan aims to achieve."
                    },
                    "milestones": {
                        "type": "array",
                        "description": "create: milestone objects, each with {id, description, verification}.",
                        "items": {
                            "type": "object",
                            "required": ["id", "description", "verification"],
                            "properties": {
                                "id": {"type": "string", "description": "Short machine-readable id, e.g. 'm1'"},
                                "description": {"type": "string", "description": "What this milestone means in human terms"},
                                "verification": {"type": "string", "description": "How to verify this milestone is met"}
                            }
                        }
                    },
                    "success_criteria": {
                        "type": "string",
                        "description": "create: how we know the plan is done (overall acceptance criteria)."
                    },
                    "round_cap": {
                        "type": "integer",
                        "description": "create: max rounds per session before forcing a pause (default 15)."
                    },
                    "tasks": {
                        "type": "array",
                        "description": "set_tasks: task objects, each with {id, title, description, depends_on?, milestone_id?}. Replaces the entire task list.",
                        "items": {
                            "type": "object",
                            "required": ["id", "title", "description"],
                            "properties": {
                                "id": {"type": "string", "description": "Short machine-readable id, e.g. '1'"},
                                "title": {"type": "string", "description": "Human-readable title"},
                                "description": {"type": "string", "description": "What this task should accomplish"},
                                "depends_on": {
                                    "type": "array",
                                    "items": {"type": "string"},
                                    "description": "Task ids that must complete before this one"
                                },
                                "milestone_id": {
                                    "type": "string",
                                    "description": "Optional milestone this task contributes to. References a milestone id from the plan. If the referenced milestone doesn't exist, the reference is silently dropped."
                                }
                            }
                        }
                    },
                    "task_id": {
                        "type": "string",
                        "description": "Target task id for start, complete, fail, append_finding, and record_direction."
                    },
                    "result": {
                        "type": "string",
                        "description": "complete: summary of what was accomplished."
                    },
                    "reason": {
                        "type": "string",
                        "description": "fail: why the task failed."
                    },
                    "retryable": {
                        "type": "boolean",
                        "description": "fail: whether the task can be retried. If false, task is marked blocked."
                    },
                    "finding": {
                        "type": "string",
                        "description": "append_finding: the intermediate finding text."
                    },
                    "confidence": {
                        "type": "number",
                        "description": "append_finding: confidence level 0.0-1.0."
                    },
                    "direction": {
                        "type": "object",
                        "description": "record_direction: direction object with {id, description, parameters?}. Parameters is a map of param_name→description.",
                        "required": ["id", "description"],
                        "properties": {
                            "id": {"type": "string", "description": "Short machine-readable id"},
                            "description": {"type": "string", "description": "Human description of the approach"},
                            "parameters": {
                                "type": "object",
                                "description": "Optional parameters describing the direction"
                            }
                        }
                    },
                    "current_milestone_id": {
                        "type": "string",
                        "description": "start: set the current milestone in progress tracking."
                    },
                    "current_direction_id": {
                        "type": "string",
                        "description": "start: set the current direction in progress tracking."
                    }
                }
            }))
        });
        &PARAMS
    }

    fn returns(&self) -> &JsonSchema {
        static RETURNS: LazyLock<JsonSchema> = LazyLock::new(|| {
            JsonSchema::from(json!({
                "type": "object",
                "properties": {
                    "ok": {"type": "boolean"},
                    "operation": {"type": "string", "description": "The operation that was executed."},
                    "plan_id": {"type": "string", "description": "The plan identifier."},
                    "plan_path": {"type": "string", "description": "create: path to the .plan file."},
                    "goal": {"type": "string", "description": "status/resume: the plan goal."},
                    "milestones": {
                        "type": "array",
                        "description": "status/resume: milestone list."
                    },
                    "tasks": {
                        "type": "array",
                        "description": "status/resume: task list with statuses."
                    },
                    "progress": {
                        "type": "object",
                        "description": "status/resume: current progress state (iteration, stale_count, current_task_id, …)."
                    },
                    "next_task": {
                        "type": "object",
                        "description": "resume: the next task to execute (first pending, unblocked task)."
                    },
                    "unblocked_tasks": {
                        "type": "array",
                        "description": "complete: tasks that became unblocked as a result of this completion."
                    },
                    "findings_count": {
                        "type": "integer",
                        "description": "status: number of findings accumulated so far."
                    }
                }
            }))
        });
        &RETURNS
    }

    async fn execute(&self, params: Value, ctx: ToolContext) -> AmanResult<Value> {
        let operation = params
            .get("operation")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::ConfigInvalid {
                message: "planner requires an \"operation\" field. \
                          Valid operations: create, set_tasks, start, complete, fail, \
                          append_finding, record_direction, increment_stale, status, resume."
                    .to_owned(),
            })?;

        let result = match operation {
            OP_CREATE => self.op_create(&params),
            OP_SET_TASKS => self.op_set_tasks(&params),
            OP_START => self.op_start(&params),
            OP_COMPLETE => self.op_complete(&params),
            OP_FAIL => self.op_fail(&params),
            OP_APPEND_FINDING => self.op_append_finding(&params),
            OP_RECORD_DIRECTION => self.op_record_direction(&params),
            OP_INCREMENT_STALE => self.op_increment_stale(&params),
            OP_STATUS => self.op_status(&params),
            OP_RESUME => self.op_resume(&params),
            other => Err(Error::ConfigInvalid {
                message: format!(
                    "unknown planner operation: {other}. Valid: {}",
                    VALID_OPERATIONS.join(", ")
                ),
            }),
        };

        // Publish plan lifecycle events so the Orchestrator can react.
        if let Ok(ref val) = result
            && let Some(bus) = &ctx.base.event_bus
        {
            match operation {
                    OP_CREATE => {
                        let plan_id = val["plan_id"].as_str().unwrap_or("");
                        let goal = params.get("goal").and_then(|v| v.as_str()).unwrap_or("");
                        let _ = bus.publish(Event::new(
                            "tool:planner",
                            EventType::Custom("plan:created".to_owned()),
                            json!({"plan_id": plan_id, "goal": goal}),
                        )).await;
                    }
                    OP_RESUME => {
                        let plan_id = val["plan_id"].as_str().unwrap_or("");
                        let _ = bus.publish(Event::new(
                            "tool:planner",
                            EventType::Custom("plan:resumed".to_owned()),
                            json!({
                                "plan_id": plan_id,
                                "next_task": val["next_task"],
                                "stale_count": val["stale_count"],
                                "iteration": val["iteration"],
                            }),
                        )).await;
                    }
                    _ => {}
                }
        }

        result
    }
}

// ---------------------------------------------------------------------------
// Path helpers
// ---------------------------------------------------------------------------

impl PlannerTool {
    fn plans_dir() -> PathBuf {
        // Thread-local test override.
        let override_dir = PLANS_DIR_OVERRIDE.with(|cell| cell.borrow().clone());
        if let Some(ref dir) = override_dir {
            return dir.clone();
        }
        let home = std::env::var("HOME")
            .or_else(|_| std::env::var("USERPROFILE"))
            .unwrap_or_else(|_| "/tmp".to_owned());
        PathBuf::from(home).join(".aman").join("plans")
    }

    fn plan_path(plan_id: &str, extension: &str) -> PathBuf {
        Self::plans_dir().join(format!("{plan_id}.{extension}"))
    }

    fn ensure_dir() -> AmanResult<()> {
        let dir = Self::plans_dir();
        fs::create_dir_all(&dir).map_err(|e| Error::Unrecoverable {
            message: format!("failed to create plans directory {}: {e}", dir.display()),
        })
    }

    // -----------------------------------------------------------------------
    // Plan file read/write
    // -----------------------------------------------------------------------

    fn read_plan(plan_id: &str) -> AmanResult<PlanState> {
        let path = Self::plan_path(plan_id, "plan");
        let raw = fs::read_to_string(&path).map_err(|e| Error::Unrecoverable {
            message: format!("failed to read plan file {}: {e}", path.display()),
        })?;
        serde_json::from_str(&raw).map_err(|e| Error::Unrecoverable {
            message: format!("failed to parse plan file {}: {e}", path.display()),
        })
    }

    fn write_plan(plan_id: &str, state: &PlanState) -> AmanResult<()> {
        Self::ensure_dir()?;
        let path = Self::plan_path(plan_id, "plan");
        let raw = serde_json::to_string_pretty(state).map_err(|e| Error::Unrecoverable {
            message: format!("failed to serialize plan: {e}"),
        })?;
        fs::write(&path, &raw).map_err(|e| Error::Unrecoverable {
            message: format!("failed to write plan file {}: {e}", path.display()),
        })
    }

    // -----------------------------------------------------------------------
    // Progress file read/write
    // -----------------------------------------------------------------------

    fn read_progress(plan_id: &str) -> AmanResult<ProgressState> {
        let path = Self::plan_path(plan_id, "progress");
        match fs::read_to_string(&path) {
            Ok(raw) => serde_json::from_str(&raw).map_err(|e| Error::Unrecoverable {
                message: format!("failed to parse progress file {}: {e}", path.display()),
            }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                // Return a default progress state for the given plan.
                Ok(ProgressState {
                    plan_id: plan_id.to_owned(),
                    iteration: 0,
                    current_task_id: None,
                    current_milestone_id: None,
                    current_direction_id: None,
                    stale_count: 0,
                    last_progress_at: None,
                    last_session_id: None,
                    retry_counts: HashMap::new(),
                })
            }
            Err(e) => Err(Error::Unrecoverable {
                message: format!("failed to read progress file {}: {e}", path.display()),
            }),
        }
    }

    fn write_progress(plan_id: &str, state: &ProgressState) -> AmanResult<()> {
        Self::ensure_dir()?;
        let path = Self::plan_path(plan_id, "progress");
        let raw =
            serde_json::to_string_pretty(state).map_err(|e| Error::Unrecoverable {
                message: format!("failed to serialize progress: {e}"),
            })?;
        fs::write(&path, &raw).map_err(|e| Error::Unrecoverable {
            message: format!("failed to write progress file {}: {e}", path.display()),
        })
    }

    // -----------------------------------------------------------------------
    // Findings file (append-only JSONL)
    // -----------------------------------------------------------------------

    fn count_findings(plan_id: &str) -> AmanResult<usize> {
        let path = Self::plan_path(plan_id, "findings.jsonl");
        match fs::File::open(&path) {
            Ok(file) => {
                let reader = BufReader::new(file);
                Ok(reader.lines().count())
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(0),
            Err(e) => Err(Error::Unrecoverable {
                message: format!("failed to open findings file {}: {e}", path.display()),
            }),
        }
    }

    #[allow(dead_code)]
    fn read_findings(plan_id: &str) -> AmanResult<Vec<Value>> {
        let path = Self::plan_path(plan_id, "findings.jsonl");
        match fs::File::open(&path) {
            Ok(file) => {
                let reader = BufReader::new(file);
                let mut findings = Vec::new();
                for line in reader.lines() {
                    let line = line.map_err(|e| Error::Unrecoverable {
                        message: format!("failed to read findings line: {e}"),
                    })?;
                    if line.trim().is_empty() {
                        continue;
                    }
                    let value: Value = serde_json::from_str(&line).unwrap_or_else(|_| {
                        json!({"raw": line})
                    });
                    findings.push(value);
                }
                Ok(findings)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(e) => Err(Error::Unrecoverable {
                message: format!("failed to open findings file {}: {e}", path.display()),
            }),
        }
    }

    fn append_finding_inner(plan_id: &str, entry: &Value) -> AmanResult<()> {
        Self::ensure_dir()?;
        let path = Self::plan_path(plan_id, "findings.jsonl");
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(|e| Error::Unrecoverable {
                message: format!("failed to open findings file for append {}: {e}", path.display()),
            })?;
        let line = serde_json::to_string(entry).map_err(|e| Error::Unrecoverable {
            message: format!("failed to serialize finding: {e}"),
        })?;
        writeln!(file, "{line}").map_err(|e| Error::Unrecoverable {
            message: format!("failed to write finding: {e}"),
        })
    }

    // -----------------------------------------------------------------------
    // Operation implementations
    // -----------------------------------------------------------------------

    fn op_create(&self, params: &Value) -> AmanResult<Value> {
        let plan_id = params
            .get("plan_id")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| format!("plan_{}", uuid::Uuid::now_v7()));

        // Fail if this plan already exists.
        let plan_path = Self::plan_path(&plan_id, "plan");
        if plan_path.exists() {
            return Err(Error::AlreadyExists {
                name: format!("plan:{plan_id}"),
            });
        }

        let goal = params
            .get("goal")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::ConfigInvalid {
                message: "create requires a \"goal\" field (non-empty string).".to_owned(),
            })?;

        let milestones: Vec<Milestone> = params
            .get("milestones")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or_default();

        let success_criteria = params
            .get("success_criteria")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(ToOwned::to_owned);

        let round_cap = params
            .get("round_cap")
            .and_then(Value::as_u64)
            .map(|v| v as u32)
            .unwrap_or_else(default_round_cap);

        let now = chrono_now();

        let state = PlanState {
            plan_id: plan_id.clone(),
            goal: goal.to_owned(),
            milestones,
            success_criteria,
            round_cap,
            tasks: Vec::new(),
            created_at: now.clone(),
            updated_at: now,
        };

        Self::write_plan(&plan_id, &state)?;

        // Initialise progress.
        let progress = ProgressState {
            plan_id: plan_id.clone(),
            ..Default::default()
        };
        Self::write_progress(&plan_id, &progress)?;

        Ok(json!({
            "ok": true,
            "operation": OP_CREATE,
            "plan_id": plan_id,
            "plan_path": Self::plan_path(&plan_id, "plan").to_string_lossy(),
        }))
    }

    fn op_set_tasks(&self, params: &Value) -> AmanResult<Value> {
        let plan_id = extract_plan_id(params)?;
        let mut state = Self::read_plan(&plan_id)?;

        let tasks_raw: Vec<Value> = params
            .get("tasks")
            .and_then(Value::as_array)
            .cloned()
            .ok_or_else(|| Error::ConfigInvalid {
                message: "set_tasks requires a \"tasks\" array.".to_owned(),
            })?;

        let mut tasks: Vec<TaskDef> = tasks_raw
            .into_iter()
            .map(serde_json::from_value)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| Error::ConfigInvalid {
                message: format!("invalid task in tasks array: {e}"),
            })?;

        // Validate depends_on references.
        let task_ids: Vec<&str> = tasks.iter().map(|t| t.id.as_str()).collect();
        for task in &tasks {
            for dep in &task.depends_on {
                if !task_ids.contains(&dep.as_str()) {
                    return Err(Error::ConfigInvalid {
                        message: format!(
                            "task '{}' depends on '{}' which is not in the task list",
                            task.id, dep
                        ),
                    });
                }
            }
        }

        // Clean milestone references: if a task references a milestone that
        // doesn't exist in the plan (or the plan has no milestones at all),
        // silently strip the reference rather than erroring. The LLM may not
        // always know the milestone list — especially when milestones were
        // omitted during create.
        let milestone_ids: Vec<&str> = state.milestones.iter().map(|m| m.id.as_str()).collect();
        for task in &mut tasks {
            if let Some(ref mid) = task.milestone_id
                && !milestone_ids.contains(&mid.as_str())
            {
                task.milestone_id = None;
            }
        }

        state.tasks = tasks;
        state.updated_at = chrono_now();

        Self::write_plan(&plan_id, &state)?;

        // Reset progress tracking (new task list = fresh execution).
        let mut progress = Self::read_progress(&plan_id)?;
        progress.iteration = 0;
        progress.stale_count = 0;
        progress.current_task_id = None;
        progress.current_milestone_id = None;
        progress.current_direction_id = None;
        progress.retry_counts.clear();
        Self::write_progress(&plan_id, &progress)?;

        Ok(json!({
            "ok": true,
            "operation": OP_SET_TASKS,
            "plan_id": plan_id,
            "task_count": state.tasks.len(),
        }))
    }

    fn op_start(&self, params: &Value) -> AmanResult<Value> {
        let plan_id = extract_plan_id(params)?;
        let task_id = extract_task_id(params)?;
        let mut state = Self::read_plan(&plan_id)?;
        let mut progress = Self::read_progress(&plan_id)?;

        // Phase 1: immutable checks — verify task exists, deps are met, status allows start.
        let task_idx = state
            .tasks
            .iter()
            .position(|t| t.id == task_id)
            .ok_or_else(|| Error::ConfigInvalid {
                message: format!("task '{task_id}' not found in plan '{plan_id}'"),
            })?;

        let task_ref = &state.tasks[task_idx];

        // Check dependencies are met.
        let unmet: Vec<&str> = task_ref
            .depends_on
            .iter()
            .filter(|dep| {
                !state
                    .tasks
                    .iter()
                    .any(|t| t.id == **dep && matches!(t.status, TaskStatus::Completed))
            })
            .map(String::as_str)
            .collect();
        if !unmet.is_empty() {
            return Err(Error::ConfigInvalid {
                message: format!(
                    "task '{task_id}' has unmet dependencies: {}",
                    unmet.join(", ")
                ),
            });
        }

        // Only allow starting pending or previously-failed tasks.
        match task_ref.status {
            TaskStatus::Pending | TaskStatus::Failed => {}
            TaskStatus::InProgress => {
                return Ok(json!({
                    "ok": true,
                    "operation": OP_START,
                    "plan_id": plan_id,
                    "task_id": task_id,
                    "status": "in_progress",
                    "note": "task was already in_progress"
                }));
            }
            TaskStatus::Completed => {
                return Err(Error::ConfigInvalid {
                    message: format!("task '{task_id}' is already completed"),
                });
            }
            TaskStatus::Blocked => {
                return Err(Error::ConfigInvalid {
                    message: format!("task '{task_id}' is blocked and cannot be started"),
                });
            }
        }

        // Phase 2: mutable update.
        state.tasks[task_idx].status = TaskStatus::InProgress;
        state.updated_at = chrono_now();
        Self::write_plan(&plan_id, &state)?;

        // Update progress.
        progress.current_task_id = Some(task_id.clone());
        progress.last_progress_at = Some(chrono_now());

        if let Some(ref mid) = params
            .get("current_milestone_id")
            .and_then(Value::as_str)
        {
            progress.current_milestone_id = Some(mid.to_string());
        }
        if let Some(ref did) = params
            .get("current_direction_id")
            .and_then(Value::as_str)
        {
            progress.current_direction_id = Some(did.to_string());
        }

        Self::write_progress(&plan_id, &progress)?;

        Ok(json!({
            "ok": true,
            "operation": OP_START,
            "plan_id": plan_id,
            "task_id": task_id,
            "status": "in_progress"
        }))
    }

    fn op_complete(&self, params: &Value) -> AmanResult<Value> {
        let plan_id = extract_plan_id(params)?;
        let task_id = extract_task_id(params)?;
        let result = params
            .get("result")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::ConfigInvalid {
                message: "complete requires a \"result\" field (summary of what was accomplished)."
                    .to_owned(),
            })?;

        let mut state = Self::read_plan(&plan_id)?;
        let mut progress = Self::read_progress(&plan_id)?;

        let task = state
            .tasks
            .iter_mut()
            .find(|t| t.id == task_id)
            .ok_or_else(|| Error::ConfigInvalid {
                message: format!("task '{task_id}' not found in plan '{plan_id}'"),
            })?;

        task.status = TaskStatus::Completed;
        task.result = Some(result.to_owned());
        state.updated_at = chrono_now();
        Self::write_plan(&plan_id, &state)?;

        // Find newly unblocked tasks.
        let unblocked: Vec<&TaskDef> = state
            .tasks
            .iter()
            .filter(|t| {
                matches!(t.status, TaskStatus::Pending)
                    && t.depends_on.iter().all(|dep| {
                        state
                            .tasks
                            .iter()
                            .any(|other| other.id == *dep && matches!(other.status, TaskStatus::Completed))
                    })
                    && !t.depends_on.is_empty()
            })
            .collect();

        // Update progress.
        progress.iteration += 1;
        progress.stale_count = 0;
        progress.last_progress_at = Some(chrono_now());
        progress.current_task_id = None;
        // Retry count cleanup for completed task.
        progress.retry_counts.remove(&task_id);
        Self::write_progress(&plan_id, &progress)?;

        Ok(json!({
            "ok": true,
            "operation": OP_COMPLETE,
            "plan_id": plan_id,
            "task_id": task_id,
            "status": "completed",
            "unblocked_tasks": unblocked.iter().map(|t| json!({
                "id": t.id,
                "title": t.title,
            })).collect::<Vec<_>>(),
        }))
    }

    fn op_fail(&self, params: &Value) -> AmanResult<Value> {
        let plan_id = extract_plan_id(params)?;
        let task_id = extract_task_id(params)?;
        let reason = params
            .get("reason")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::ConfigInvalid {
                message: "fail requires a \"reason\" field.".to_owned(),
            })?;
        let retryable = params
            .get("retryable")
            .and_then(Value::as_bool)
            .unwrap_or(true);

        let mut state = Self::read_plan(&plan_id)?;
        let mut progress = Self::read_progress(&plan_id)?;

        let task = state
            .tasks
            .iter_mut()
            .find(|t| t.id == task_id)
            .ok_or_else(|| Error::ConfigInvalid {
                message: format!("task '{task_id}' not found in plan '{plan_id}'"),
            })?;

        if retryable {
            // Check retry limit (default 3).
            let count = progress.retry_counts.entry(task_id.clone()).or_insert(0);
            *count += 1;
            if *count > 3 {
                task.status = TaskStatus::Blocked;
                task.result = Some(format!("exceeded retry limit (3). Last reason: {reason}"));
            } else {
                task.status = TaskStatus::Failed;
                task.result = Some(format!("attempt {count}/3: {reason}"));
            }
        } else {
            task.status = TaskStatus::Blocked;
            task.result = Some(reason.to_owned());
        }

        state.updated_at = chrono_now();
        Self::write_plan(&plan_id, &state)?;

        progress.last_progress_at = Some(chrono_now());
        progress.current_task_id = None;
        Self::write_progress(&plan_id, &progress)?;

        let effective_status = if retryable && *progress.retry_counts.get(&task_id).unwrap_or(&0) <= 3
        {
            "failed"
        } else {
            "blocked"
        };

        Ok(json!({
            "ok": true,
            "operation": OP_FAIL,
            "plan_id": plan_id,
            "task_id": task_id,
            "status": effective_status,
        }))
    }

    fn op_append_finding(&self, params: &Value) -> AmanResult<Value> {
        let plan_id = extract_plan_id(params)?;
        let task_id = extract_task_id(params)?;
        let finding = params
            .get("finding")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::ConfigInvalid {
                message: "append_finding requires a \"finding\" field.".to_owned(),
            })?;
        let confidence = params
            .get("confidence")
            .and_then(Value::as_f64)
            .unwrap_or(1.0);

        // Verify the plan exists.
        let _state = Self::read_plan(&plan_id)?;

        // Determine the next sequence number by counting existing findings.
        let existing_count = Self::count_findings(&plan_id)?;

        let entry = json!({
            "task_id": task_id,
            "seq": existing_count,
            "ts": chrono_now(),
            "finding": finding,
            "confidence": confidence,
        });

        Self::append_finding_inner(&plan_id, &entry)?;

        Ok(json!({
            "ok": true,
            "operation": OP_APPEND_FINDING,
            "plan_id": plan_id,
            "task_id": task_id,
            "seq": existing_count,
        }))
    }

    fn op_record_direction(&self, params: &Value) -> AmanResult<Value> {
        let plan_id = extract_plan_id(params)?;
        let task_id = extract_task_id(params)?;
        let direction_value = params
            .get("direction")
            .ok_or_else(|| Error::ConfigInvalid {
                message: "record_direction requires a \"direction\" object with {id, description, parameters?}."
                    .to_owned(),
            })?;

        let direction: Direction = serde_json::from_value(direction_value.clone()).map_err(|e| {
            Error::ConfigInvalid {
                message: format!("invalid direction: {e}"),
            }
        })?;

        let mut state = Self::read_plan(&plan_id)?;

        // Mutate in a block so the mutable borrow is released before write_plan.
        let directions_count = {
            let task = state
                .tasks
                .iter_mut()
                .find(|t| t.id == task_id)
                .ok_or_else(|| Error::ConfigInvalid {
                    message: format!("task '{task_id}' not found in plan '{plan_id}'"),
                })?;

            // Replace if direction with same id already exists; otherwise append.
            if let Some(existing) = task
                .directions_tried
                .iter_mut()
                .find(|d| d.id == direction.id)
            {
                *existing = direction;
            } else {
                task.directions_tried.push(direction);
            }

            task.directions_tried.len()
        };

        state.updated_at = chrono_now();
        Self::write_plan(&plan_id, &state)?;

        Ok(json!({
            "ok": true,
            "operation": OP_RECORD_DIRECTION,
            "plan_id": plan_id,
            "task_id": task_id,
            "directions_count": directions_count,
        }))
    }

    fn op_increment_stale(&self, params: &Value) -> AmanResult<Value> {
        let plan_id = extract_plan_id(params)?;
        let mut progress = Self::read_progress(&plan_id)?;

        progress.stale_count += 1;

        Self::write_progress(&plan_id, &progress)?;

        Ok(json!({
            "ok": true,
            "operation": OP_INCREMENT_STALE,
            "plan_id": plan_id,
            "stale_count": progress.stale_count,
        }))
    }

    fn op_status(&self, params: &Value) -> AmanResult<Value> {
        let plan_id = extract_plan_id_or_guess(params)?;
        let state = Self::read_plan(&plan_id)?;
        let progress = Self::read_progress(&plan_id)?;
        let findings_count = Self::count_findings(&plan_id).unwrap_or(0);

        Ok(json!({
            "ok": true,
            "operation": OP_STATUS,
            "plan_id": state.plan_id,
            "goal": state.goal,
            "milestones": state.milestones,
            "success_criteria": state.success_criteria,
            "round_cap": state.round_cap,
            "tasks": state.tasks,
            "progress": {
                "iteration": progress.iteration,
                "current_task_id": progress.current_task_id,
                "current_milestone_id": progress.current_milestone_id,
                "current_direction_id": progress.current_direction_id,
                "stale_count": progress.stale_count,
                "last_progress_at": progress.last_progress_at,
            },
            "findings_count": findings_count,
            "created_at": state.created_at,
            "updated_at": state.updated_at,
        }))
    }

    fn op_resume(&self, params: &Value) -> AmanResult<Value> {
        let plan_id = extract_plan_id_or_guess(params)?;
        let state = Self::read_plan(&plan_id)?;
        let mut progress = Self::read_progress(&plan_id)?;
        let findings_count = Self::count_findings(&plan_id).unwrap_or(0);

        // Find the next task to execute:
        // 1. Look for an in_progress task first (resume after interruption).
        // 2. Otherwise find the first pending task with all deps met.
        let in_progress: Vec<&TaskDef> = state
            .tasks
            .iter()
            .filter(|t| matches!(t.status, TaskStatus::InProgress))
            .collect();

        let next_pending = state
            .tasks
            .iter()
            .find(|t| {
                matches!(t.status, TaskStatus::Pending)
                    && t.depends_on.iter().all(|dep| {
                        state
                            .tasks
                            .iter()
                            .any(|other| other.id == *dep && matches!(other.status, TaskStatus::Completed))
                    })
            });

        let next_failed = state
            .tasks
            .iter()
            .find(|t| matches!(t.status, TaskStatus::Failed));

        // Update progress to reflect current session.
        progress.last_session_id = params
            .get("session_id")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned);
        progress.last_progress_at = Some(chrono_now());
        Self::write_progress(&plan_id, &progress)?;

        // Determine next task.
        let next_task = if !in_progress.is_empty() {
            // Prefer the in-progress task (interrupted).
            Some(json!({
                "id": in_progress[0].id,
                "title": in_progress[0].title,
                "status": "in_progress",
                "note": "task was interrupted — resume from where you left off"
            }))
        } else if let Some(task) = next_pending {
            Some(json!({
                "id": task.id,
                "title": task.title,
                "status": "pending",
                "depends_on": task.depends_on,
            }))
        } else {
            next_failed.map(|task| json!({
                "id": task.id,
                "title": task.title,
                "status": "failed",
                "note": "this task failed previously — consider retrying or pivoting"
            }))
        };

        Ok(json!({
            "ok": true,
            "operation": OP_RESUME,
            "plan_id": state.plan_id,
            "goal": state.goal,
            "milestones": state.milestones,
            "success_criteria": state.success_criteria,
            "round_cap": state.round_cap,
            "tasks": state.tasks,
            "progress": {
                "iteration": progress.iteration,
                "current_task_id": progress.current_task_id,
                "current_milestone_id": progress.current_milestone_id,
                "current_direction_id": progress.current_direction_id,
                "stale_count": progress.stale_count,
                "last_progress_at": progress.last_progress_at,
            },
            "next_task": next_task,
            "next_milestone": state.milestones.iter().find(|m| {
                progress.current_milestone_id.as_deref() == Some(m.id.as_str())
            }).or_else(|| {
                // Auto-detect: first milestone with incomplete tasks.
                state.milestones.iter().find(|m| {
                    state.tasks.iter().any(|t| {
                        t.milestone_id.as_deref() == Some(m.id.as_str())
                            && !matches!(t.status, TaskStatus::Completed)
                    })
                })
            }),
            "findings_count": findings_count,
            "created_at": state.created_at,
            "updated_at": state.updated_at,
        }))
    }
}

// ---------------------------------------------------------------------------
// Parameter extraction helpers
// ---------------------------------------------------------------------------

fn extract_plan_id(params: &Value) -> AmanResult<String> {
    params
        .get("plan_id")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(ToOwned::to_owned)
        .ok_or_else(|| Error::ConfigInvalid {
            message: "planner requires a \"plan_id\" field (non-empty string).".to_owned(),
        })
}

/// Like extract_plan_id but falls back to scanning ~/.aman/plans/ for the
/// most recently modified plan file. Only used by status/resume — the soft
/// "tell me about my active plan" operations.
fn extract_plan_id_or_guess(params: &Value) -> AmanResult<String> {
    if let Some(id) = params
        .get("plan_id")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
    {
        return Ok(id.to_owned());
    }

    // Fallback: find the most recently modified .plan file.
    let dir = PlannerTool::plans_dir();
    if !dir.exists() {
        return Err(Error::ConfigInvalid {
            message: "no plan_id provided and no plans directory exists yet. \
                      Create a plan first with planner.create."
                .to_owned(),
        });
    }

    let mut best: Option<(String, std::time::SystemTime)> = None;
    if let Ok(entries) = fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("plan")
                && let Ok(meta) = path.metadata()
                && let Ok(modified) = meta.modified()
                && best.as_ref().is_none_or(|(_, t)| modified > *t)
                && let Some(stem) = path.file_stem().and_then(|s| s.to_str())
            {
                best = Some((stem.to_owned(), modified));
            }
        }
    }

    best.map(|(id, _)| id).ok_or_else(|| Error::ConfigInvalid {
        message: "no plan_id provided and no .plan files found in ~/.aman/plans/. \
                  Create a plan first with planner.create."
            .to_owned(),
    })
}

fn extract_task_id(params: &Value) -> AmanResult<String> {
    params
        .get("task_id")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(ToOwned::to_owned)
        .ok_or_else(|| Error::ConfigInvalid {
            message: "this operation requires a \"task_id\" field (non-empty string).".to_owned(),
        })
}

// ---------------------------------------------------------------------------
// Time helper
// ---------------------------------------------------------------------------

fn chrono_now() -> String {
    // Use the system clock. Date/time functions are called in direct response
    // to tool invocation (not inside workflow scripts), so they are safe here.
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = now.as_secs();
    // Format as ISO-8601 UTC.
    let days = secs / 86400;
    let time_secs = secs % 86400;
    let hours = time_secs / 3600;
    let minutes = (time_secs % 3600) / 60;
    let seconds = time_secs % 60;

    // Simple date calculation from UNIX epoch (1970-01-01).
    let (year, month, day) = civil_from_days(days as i64 + 719_468); // days since 0000-03-01

    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        year, month, day, hours, minutes, seconds
    )
}

/// Convert days since 0000-03-01 to (year, month, day). Uses the
/// algorithm from Howard Hinnant's date library.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z - 719_468; // shift to 1970-03-01 epoch
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u32;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    #![allow(unused_variables)]

    use super::*;
    use serde_json::json;
    use std::fs;

    /// Convenience wrapper: execute the tool synchronously via pollster.
    fn exec(params: Value) -> AmanResult<Value> {
        pollster::block_on(async {
            let tool = PlannerTool;
            tool.execute(params, ToolContext::default()).await
        })
    }

    fn temp_plans_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("aman-planner-test-{}", uuid::Uuid::now_v7()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn setup_test_home() -> (PathBuf, PathBuf) {
        let home = temp_plans_dir();
        let plans = home.join(".aman").join("plans");
        PlannerTool::set_test_plans_dir(&plans);
        (home, plans)
    }

    fn teardown_test_home(home: &std::path::Path) {
        PlannerTool::clear_test_plans_dir();
        fs::remove_dir_all(home).ok();
    }

    // -----------------------------------------------------------------------
    // Create
    // -----------------------------------------------------------------------

    #[test]
    fn create_minimal_plan() {
        let (home, plans) = setup_test_home();

        let result = exec(json!({
            "operation": "create",
            "goal": "Test plan for planner tool"
        })).unwrap();

        let plan_id = result["plan_id"].as_str().unwrap();
        assert!(result["ok"].as_bool().unwrap());
        assert!(!plan_id.is_empty());

        let plan_path = plans.join(format!("{plan_id}.plan"));
        assert!(plan_path.exists(), "plan file should exist at {plan_path:?}");

        teardown_test_home(&home);
    }

    #[test]
    fn create_with_milestones() {
        let (home, plans) = setup_test_home();

        let result = exec(json!({
            "operation": "create",
            "goal": "Test with milestones",
            "milestones": [
                {"id": "m1", "description": "First milestone", "verification": "cargo build"},
                {"id": "m2", "description": "Second milestone", "verification": "cargo test"}
            ],
            "success_criteria": "all tests pass",
            "round_cap": 10
        })).unwrap();

        let plan_id = result["plan_id"].as_str().unwrap();

        let state = PlannerTool::read_plan(plan_id).unwrap();
        assert_eq!(state.goal, "Test with milestones");
        assert_eq!(state.milestones.len(), 2);
        assert_eq!(state.milestones[0].id, "m1");
        assert_eq!(state.success_criteria.as_deref(), Some("all tests pass"));
        assert_eq!(state.round_cap, 10);

        teardown_test_home(&home);
    }

    #[test]
    fn create_duplicate_plan_fails() {
        let (home, plans) = setup_test_home();

        let result1 = exec(json!({"operation": "create", "plan_id": "dup-test", "goal": "first"})).unwrap();
        assert!(result1["ok"].as_bool().unwrap());

        let result2 = exec(json!({"operation": "create", "plan_id": "dup-test", "goal": "second"}));
        assert!(result2.is_err());

        teardown_test_home(&home);
    }

    #[test]
    fn create_requires_goal() {
        let result = exec(json!({"operation": "create"}));
        assert!(result.is_err());
        let err = format!("{}", result.unwrap_err());
        assert!(err.contains("goal"), "error should mention goal: {err}");
    }

    // -----------------------------------------------------------------------
    // set_tasks
    // -----------------------------------------------------------------------

    #[test]
    fn set_tasks_and_read_back() {
        let (home, plans) = setup_test_home();

        let create = exec(json!({"operation": "create", "goal": "Task list test"})).unwrap();
        let plan_id = create["plan_id"].as_str().unwrap();

        let set = exec(json!({
            "operation": "set_tasks",
            "plan_id": plan_id,
            "tasks": [
                {"id": "1", "title": "First task", "description": "Do the first thing"},
                {"id": "2", "title": "Second task", "description": "Do the second thing", "depends_on": ["1"]}
            ]
        })).unwrap();
        assert!(set["ok"].as_bool().unwrap());
        assert_eq!(set["task_count"].as_u64().unwrap(), 2);

        let state = PlannerTool::read_plan(plan_id).unwrap();
        assert_eq!(state.tasks.len(), 2);
        assert_eq!(state.tasks[0].id, "1");
        assert!(matches!(state.tasks[0].status, TaskStatus::Pending));
        assert_eq!(state.tasks[1].depends_on, vec!["1"]);

        teardown_test_home(&home);
    }

    #[test]
    fn set_tasks_with_bad_dep_fails() {
        let (home, plans) = setup_test_home();

        let create = exec(json!({"operation": "create", "goal": "Dep validation"})).unwrap();
        let plan_id = create["plan_id"].as_str().unwrap();

        let result = exec(json!({
            "operation": "set_tasks",
            "plan_id": plan_id,
            "tasks": [
                {"id": "1", "title": "Only task", "description": "...", "depends_on": ["99"]}
            ]
        }));
        assert!(result.is_err());

        teardown_test_home(&home);
    }

    #[test]
    fn set_tasks_links_to_milestones() {
        let (home, plans) = setup_test_home();

        let create = exec(json!({
            "operation": "create",
            "goal": "Milestone linking",
            "milestones": [
                {"id": "m1", "description": "Setup", "verification": "ls"}
            ]
        })).unwrap();
        let plan_id = create["plan_id"].as_str().unwrap();

        let set = exec(json!({
            "operation": "set_tasks",
            "plan_id": plan_id,
            "tasks": [
                {"id": "1", "title": "Setup task", "description": "...", "milestone_id": "m1"}
            ]
        })).unwrap();
        assert!(set["ok"].as_bool().unwrap());

        let state = PlannerTool::read_plan(plan_id).unwrap();
        assert_eq!(state.tasks[0].milestone_id.as_deref(), Some("m1"));

        // Bad milestone reference: silently stripped instead of erroring.
        // The LLM may not know the milestone list, so we just drop the ref.
        let set2 = exec(json!({
            "operation": "set_tasks",
            "plan_id": plan_id,
            "tasks": [
                {"id": "1", "title": "Bad ref", "description": "...", "milestone_id": "m99"}
            ]
        })).unwrap();
        assert!(set2["ok"].as_bool().unwrap());

        let state2 = PlannerTool::read_plan(plan_id).unwrap();
        assert_eq!(state2.tasks[0].milestone_id, None,
            "milestone_id 'm99' should have been stripped since it's not in the plan");

        teardown_test_home(&home);
    }

    // -----------------------------------------------------------------------
    // Task lifecycle: start → complete
    // -----------------------------------------------------------------------

    #[test]
    fn start_complete_lifecycle() {
        let (home, plans) = setup_test_home();

        let create = exec(json!({"operation": "create", "goal": "Lifecycle test"})).unwrap();
        let plan_id = create["plan_id"].as_str().unwrap();

        exec(json!({
            "operation": "set_tasks",
            "plan_id": plan_id,
            "tasks": [
                {"id": "1", "title": "Task one", "description": "First"},
                {"id": "2", "title": "Task two", "description": "Second", "depends_on": ["1"]}
            ]
        })).unwrap();

        // Start task 1.
        let start = exec(json!({"operation": "start", "plan_id": plan_id, "task_id": "1"})).unwrap();
        assert!(start["ok"].as_bool().unwrap());

        let state = PlannerTool::read_plan(plan_id).unwrap();
        assert!(matches!(state.tasks[0].status, TaskStatus::InProgress));

        // Cannot start task 2 (depends on 1).
        let blocked_start = exec(json!({"operation": "start", "plan_id": plan_id, "task_id": "2"}));
        assert!(blocked_start.is_err());

        // Complete task 1 → should unblock task 2.
        let complete = exec(json!({
            "operation": "complete",
            "plan_id": plan_id,
            "task_id": "1",
            "result": "All done with task 1"
        })).unwrap();
        assert!(complete["ok"].as_bool().unwrap());

        let unblocked = complete["unblocked_tasks"].as_array().unwrap();
        assert_eq!(unblocked.len(), 1);
        assert_eq!(unblocked[0]["id"], "2");

        let state = PlannerTool::read_plan(plan_id).unwrap();
        assert!(matches!(state.tasks[0].status, TaskStatus::Completed));
        assert_eq!(state.tasks[0].result.as_deref(), Some("All done with task 1"));

        teardown_test_home(&home);
    }

    // -----------------------------------------------------------------------
    // fail
    // -----------------------------------------------------------------------

    #[test]
    fn fail_retryable_and_blocked() {
        let (home, plans) = setup_test_home();

        let create = exec(json!({"operation": "create", "goal": "Fail test"})).unwrap();
        let plan_id = create["plan_id"].as_str().unwrap();

        exec(json!({
            "operation": "set_tasks",
            "plan_id": plan_id,
            "tasks": [{"id": "1", "title": "Flaky task", "description": "May fail"}]
        })).unwrap();
        exec(json!({"operation": "start", "plan_id": plan_id, "task_id": "1"})).unwrap();

        // Fail once (retryable).
        let fail1 = exec(json!({
            "operation": "fail",
            "plan_id": plan_id,
            "task_id": "1",
            "reason": "transient network error",
            "retryable": true
        })).unwrap();
        assert_eq!(fail1["status"], "failed");

        let state = PlannerTool::read_plan(plan_id).unwrap();
        assert!(matches!(state.tasks[0].status, TaskStatus::Failed));

        // Can restart a failed task.
        exec(json!({"operation": "start", "plan_id": plan_id, "task_id": "1"})).unwrap();

        // Fail non-retryable → blocked.
        let fail2 = exec(json!({
            "operation": "fail",
            "plan_id": plan_id,
            "task_id": "1",
            "reason": "permanent failure",
            "retryable": false
        })).unwrap();
        assert_eq!(fail2["status"], "blocked");

        let state = PlannerTool::read_plan(plan_id).unwrap();
        assert!(matches!(state.tasks[0].status, TaskStatus::Blocked));

        teardown_test_home(&home);
    }

    // -----------------------------------------------------------------------
    // append_finding
    // -----------------------------------------------------------------------

    #[test]
    fn append_and_read_findings() {
        let (home, plans) = setup_test_home();

        let create = exec(json!({"operation": "create", "goal": "Findings test"})).unwrap();
        let plan_id = create["plan_id"].as_str().unwrap();

        exec(json!({
            "operation": "set_tasks",
            "plan_id": plan_id,
            "tasks": [{"id": "1", "title": "Research", "description": "Find things"}]
        })).unwrap();

        exec(json!({
            "operation": "append_finding",
            "plan_id": plan_id,
            "task_id": "1",
            "finding": "First discovery",
            "confidence": 0.8
        })).unwrap();

        exec(json!({
            "operation": "append_finding",
            "plan_id": plan_id,
            "task_id": "1",
            "finding": "Second discovery"
        })).unwrap();

        let count = PlannerTool::count_findings(plan_id).unwrap();
        assert_eq!(count, 2);

        let findings = PlannerTool::read_findings(plan_id).unwrap();
        assert_eq!(findings.len(), 2);
        assert_eq!(findings[0]["task_id"], "1");
        assert_eq!(findings[0]["finding"], "First discovery");
        assert_eq!(findings[0]["confidence"], 0.8);

        teardown_test_home(&home);
    }

    // -----------------------------------------------------------------------
    // record_direction
    // -----------------------------------------------------------------------

    #[test]
    fn record_and_update_directions() {
        let (home, plans) = setup_test_home();

        let create = exec(json!({"operation": "create", "goal": "Direction test"})).unwrap();
        let plan_id = create["plan_id"].as_str().unwrap();

        exec(json!({
            "operation": "set_tasks",
            "plan_id": plan_id,
            "tasks": [{"id": "1", "title": "Explore", "description": "Try different things"}]
        })).unwrap();

        exec(json!({
            "operation": "record_direction",
            "plan_id": plan_id,
            "task_id": "1",
            "direction": {
                "id": "d1",
                "description": "Heaptrack profiling",
                "parameters": {"threshold_kb": "Memory threshold in KB to sample at"}
            }
        })).unwrap();

        exec(json!({
            "operation": "record_direction",
            "plan_id": plan_id,
            "task_id": "1",
            "direction": {"id": "d2", "description": "Manual instrumentation"}
        })).unwrap();

        let state = PlannerTool::read_plan(plan_id).unwrap();
        assert_eq!(state.tasks[0].directions_tried.len(), 2);
        assert_eq!(state.tasks[0].directions_tried[0].id, "d1");
        assert_eq!(
            state.tasks[0].directions_tried[0].parameters.as_ref().unwrap()["threshold_kb"],
            "Memory threshold in KB to sample at"
        );

        // Update d1 (same id → replace).
        exec(json!({
            "operation": "record_direction",
            "plan_id": plan_id,
            "task_id": "1",
            "direction": {
                "id": "d1",
                "description": "Heaptrack profiling (revised)",
                "parameters": {"threshold_kb": "Lowered to 64KB for finer granularity"}
            }
        })).unwrap();

        let state = PlannerTool::read_plan(plan_id).unwrap();
        assert_eq!(state.tasks[0].directions_tried.len(), 2,
            "should still be 2 directions (d1 updated, not appended)");
        assert!(state.tasks[0].directions_tried[0].description.contains("revised"));

        teardown_test_home(&home);
    }

    // -----------------------------------------------------------------------
    // increment_stale
    // -----------------------------------------------------------------------

    #[test]
    fn stale_count_accumulates() {
        let (home, plans) = setup_test_home();

        let create = exec(json!({"operation": "create", "goal": "Stale test"})).unwrap();
        let plan_id = create["plan_id"].as_str().unwrap();

        let r1 = exec(json!({"operation": "increment_stale", "plan_id": plan_id})).unwrap();
        assert_eq!(r1["stale_count"], 1);

        let r2 = exec(json!({"operation": "increment_stale", "plan_id": plan_id})).unwrap();
        assert_eq!(r2["stale_count"], 2);

        // Complete a task → stale_count resets.
        exec(json!({
            "operation": "set_tasks",
            "plan_id": plan_id,
            "tasks": [{"id": "1", "title": "A task", "description": "Do it"}]
        })).unwrap();
        exec(json!({"operation": "start", "plan_id": plan_id, "task_id": "1"})).unwrap();
        exec(json!({
            "operation": "complete",
            "plan_id": plan_id,
            "task_id": "1",
            "result": "done"
        })).unwrap();

        let progress = PlannerTool::read_progress(plan_id).unwrap();
        assert_eq!(progress.stale_count, 0);

        teardown_test_home(&home);
    }

    // -----------------------------------------------------------------------
    // status & resume
    // -----------------------------------------------------------------------

    #[test]
    fn status_returns_full_snapshot() {
        let (home, plans) = setup_test_home();

        let create = exec(json!({"operation": "create", "goal": "Status test"})).unwrap();
        let plan_id = create["plan_id"].as_str().unwrap();

        let status = exec(json!({"operation": "status", "plan_id": plan_id})).unwrap();
        assert!(status["ok"].as_bool().unwrap());
        assert_eq!(status["goal"], "Status test");
        assert!(status["tasks"].is_array());
        assert!(status["progress"].is_object());
        assert_eq!(status["findings_count"], 0);

        teardown_test_home(&home);
    }

    #[test]
    fn resume_returns_next_task_and_milestone() {
        let (home, plans) = setup_test_home();

        let create = exec(json!({
            "operation": "create",
            "goal": "Resume test",
            "milestones": [
                {"id": "m1", "description": "Phase 1", "verification": "cargo build"},
                {"id": "m2", "description": "Phase 2", "verification": "cargo test"}
            ]
        })).unwrap();
        let plan_id = create["plan_id"].as_str().unwrap();

        exec(json!({
            "operation": "set_tasks",
            "plan_id": plan_id,
            "tasks": [
                {"id": "1", "title": "Setup project", "description": "...", "milestone_id": "m1"},
                {"id": "2", "title": "Write tests", "description": "...", "depends_on": ["1"], "milestone_id": "m2"}
            ]
        })).unwrap();

        // Resume → should point to task 1.
        let resume = exec(json!({"operation": "resume", "plan_id": plan_id})).unwrap();
        let next = resume["next_task"].as_object().unwrap();
        assert_eq!(next["id"], "1");
        assert_eq!(next["status"], "pending");

        // Complete task 1. Resume → should point to task 2.
        exec(json!({"operation": "start", "plan_id": plan_id, "task_id": "1"})).unwrap();
        exec(json!({
            "operation": "complete",
            "plan_id": plan_id,
            "task_id": "1",
            "result": "Done"
        })).unwrap();

        let resume2 = exec(json!({"operation": "resume", "plan_id": plan_id})).unwrap();
        let next2 = resume2["next_task"].as_object().unwrap();
        assert_eq!(next2["id"], "2");

        teardown_test_home(&home);
    }

    #[test]
    fn resume_finds_interrupted_task() {
        let (home, plans) = setup_test_home();

        let create = exec(json!({"operation": "create", "goal": "Interrupted"})).unwrap();
        let plan_id = create["plan_id"].as_str().unwrap();

        exec(json!({
            "operation": "set_tasks",
            "plan_id": plan_id,
            "tasks": [{"id": "1", "title": "Started but not finished", "description": "..."}]
        })).unwrap();
        exec(json!({"operation": "start", "plan_id": plan_id, "task_id": "1"})).unwrap();

        // Simulate interruption — resume without completing.
        let resume = exec(json!({"operation": "resume", "plan_id": plan_id})).unwrap();
        let next = resume["next_task"].as_object().unwrap();
        assert_eq!(next["id"], "1");
        assert_eq!(next["status"], "in_progress");

        teardown_test_home(&home);
    }

    // -----------------------------------------------------------------------
    // Edge cases
    // -----------------------------------------------------------------------

    #[test]
    fn complete_without_result_fails() {
        let result = exec(json!({"operation": "complete", "plan_id": "nonexistent", "task_id": "1"}));
        assert!(result.is_err());
    }

    #[test]
    fn unknown_operation_fails() {
        let result = exec(json!({"operation": "fly_to_moon"}));
        assert!(result.is_err());
    }

    #[test]
    fn fail_without_reason_fails() {
        let result = exec(json!({"operation": "fail", "plan_id": "x", "task_id": "1"}));
        assert!(result.is_err());
    }

    #[test]
    fn retry_count_caps_at_3() {
        let (home, plans) = setup_test_home();

        let create = exec(json!({"operation": "create", "goal": "Retry cap"})).unwrap();
        let plan_id = create["plan_id"].as_str().unwrap();

        exec(json!({
            "operation": "set_tasks",
            "plan_id": plan_id,
            "tasks": [{"id": "1", "title": "Doomed", "description": "Will fail forever"}]
        })).unwrap();

        for i in 0..4 {
            exec(json!({"operation": "start", "plan_id": plan_id, "task_id": "1"})).unwrap();
            let r = exec(json!({
                "operation": "fail",
                "plan_id": plan_id,
                "task_id": "1",
                "reason": format!("failure {}", i + 1),
                "retryable": true
            })).unwrap();
            if i < 3 {
                assert_eq!(r["status"], "failed", "attempt {} should be 'failed'", i + 1);
            } else {
                assert_eq!(r["status"], "blocked", "attempt {} should be 'blocked'", i + 1);
            }
        }

        let state = PlannerTool::read_plan(plan_id).unwrap();
        assert!(matches!(state.tasks[0].status, TaskStatus::Blocked),
            "task should be blocked after exceeding retry limit");

        teardown_test_home(&home);
    }
}
