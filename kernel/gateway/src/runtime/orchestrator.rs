// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! Autonomous plan orchestrator — iterates over a structured plan,
//! spawns sub-agents for each task, evaluates results, detects
//! stagnation, and pivots directions when needed.
//!
//! Triggered by `plan:created` and `plan:resumed` events from the
//! Planner tool.  Runs as a background actor per agent.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use cognitive_llm::subagent::SubAgentSpawner;
use kernel::agent::{AgentDescriptor, AgentSystemState};
use kernel::context::ToolContext;
use kernel::event::{Event, EventType};
use kernel::react::SoulSnapshot;
use kernel::{AmanResult, Error};
use serde_json::{json, Value};
use tokio::sync::Mutex;
use tool::ToolRegistry;

use super::agent_registry::AgentRegistry;
use super::subagent_spawner::GatewaySubAgentSpawner;

// ── Thresholds ──────────────────────────────────────────────────────

/// stale_count at which we generate a fresh direction (pivot).
const PIVOT_THRESHOLD: u32 = 3;
/// stale_count at which we escalate to human.
const ESCALATE_THRESHOLD: u32 = 6;
/// Maximum cycles per plan execution to prevent infinite loops.
const MAX_CYCLES: u32 = 50;

// ── Orchestrator ────────────────────────────────────────────────────

pub struct Orchestrator {
    agent_id: String,
    registry: Arc<AgentRegistry>,
    tool_registry: Arc<ToolRegistry>,
    subagent_spawner: Arc<GatewaySubAgentSpawner>,
    /// Prevents concurrent cycle loops for the same agent.
    running: AtomicBool,
    /// Guards the run_cycles loop (one plan at a time).
    cycle_lock: Mutex<()>,
}

impl Orchestrator {
    pub fn new(
        agent_id: String,
        registry: Arc<AgentRegistry>,
        tool_registry: Arc<ToolRegistry>,
        subagent_spawner: Arc<GatewaySubAgentSpawner>,
    ) -> Self {
        Self {
            agent_id,
            registry,
            tool_registry,
            subagent_spawner,
            running: AtomicBool::new(false),
            cycle_lock: Mutex::new(()),
        }
    }

    /// Called when a `plan:created` or `plan:resumed` event is received.
    /// Spawns a background task to run the orchestration loop.
    pub fn on_plan_event(self: &Arc<Self>, plan_id: String) {
        if plan_id.is_empty() {
            return;
        }
        let orchestrator = Arc::clone(self);
        tokio::spawn(async move {
            if let Err(e) = orchestrator.run_cycles(&plan_id).await {
                tracing::warn!(
                    agent_id = %orchestrator.agent_id,
                    plan_id = %plan_id,
                    error = %e,
                    "orchestrator cycle loop failed"
                );
            }
        });
    }

    // ── Cycle loop ──────────────────────────────────────────────────

    /// Run orchestration cycles until the plan completes, escalates,
    /// or the agent becomes busy.
    async fn run_cycles(&self, plan_id: &str) -> AmanResult<()> {
        // Guard: only one cycle loop per orchestrator at a time.
        let _lock = self.cycle_lock.lock().await;

        if self.running.swap(true, Ordering::SeqCst) {
            return Ok(()); // Already running
        }

        let _guard = RunningGuard { orchestrator: self };

        for cycle in 0..MAX_CYCLES {
            // Pause if agent is busy (e.g. user is chatting).
            self.wait_until_idle().await;

            match self.run_one_cycle(plan_id, cycle).await {
                Ok(CycleOutcome::Continue) => continue,
                Ok(CycleOutcome::PlanComplete) => {
                    tracing::info!(
                        agent_id = %self.agent_id,
                        plan_id = %plan_id,
                        cycles = cycle + 1,
                        "orchestrator: plan complete"
                    );
                    return Ok(());
                }
                Ok(CycleOutcome::Escalated { reason }) => {
                    tracing::warn!(
                        agent_id = %self.agent_id,
                        plan_id = %plan_id,
                        reason = %reason,
                        "orchestrator: escalated to human"
                    );
                    return Ok(());
                }
                Err(e) => {
                    tracing::error!(
                        agent_id = %self.agent_id,
                        plan_id = %plan_id,
                        cycle,
                        error = %e,
                        "orchestrator: cycle error"
                    );
                    // Continue to next cycle on non-fatal errors.
                    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                    continue;
                }
            }
        }

        tracing::warn!(
            agent_id = %self.agent_id,
            plan_id = %plan_id,
            max_cycles = MAX_CYCLES,
            "orchestrator: max cycles reached"
        );
        Ok(())
    }

    /// Execute a single orchestration cycle:
    /// resume → check stale → (pivot|escalate) → start task →
    /// spawn sub-agent → evaluate → complete/increment_stale.
    async fn run_one_cycle(
        &self,
        plan_id: &str,
        cycle: u32,
    ) -> AmanResult<CycleOutcome> {
        tracing::info!(
            agent_id = %self.agent_id,
            plan_id = %plan_id,
            cycle,
            "orchestrator: starting cycle"
        );

        // 1. Resume plan state.
        let status = self
            .call_planner("resume", json!({"plan_id": plan_id}))
            .await?;

        let stale_count = status["stale_count"].as_u64().unwrap_or(0) as u32;

        // 2. Check escalation.
        if stale_count >= ESCALATE_THRESHOLD {
            self.publish_plan_event("plan:escalated", json!({
                "plan_id": plan_id,
                "stale_count": stale_count,
                "message": "Plan has stalled repeatedly. Human intervention required.",
            }))
            .await;
            return Ok(CycleOutcome::Escalated {
                reason: format!("stale_count {stale_count} >= {ESCALATE_THRESHOLD}"),
            });
        }

        // 3. Check pivot.
        if (PIVOT_THRESHOLD..ESCALATE_THRESHOLD).contains(&stale_count) {
            self.do_pivot(plan_id, &status).await?;
        }

        // 4. Get next task.
        let next_task = &status["next_task"];
        if next_task.is_null() {
            self.publish_plan_event("plan:completed", json!({
                "plan_id": plan_id,
                "iteration": status["iteration"],
            }))
            .await;
            return Ok(CycleOutcome::PlanComplete);
        }

        let task_id = next_task["id"].as_str().unwrap_or("");
        let task_title = next_task["title"].as_str().unwrap_or("");
        let task_desc = next_task["description"].as_str().unwrap_or("");
        let directions = next_task["directions_tried"].clone();

        if task_id.is_empty() {
            return Err(Error::ConfigInvalid {
                message: format!("orchestrator: next_task has no id in plan '{plan_id}'"),
            });
        }

        // 5. Start the task.
        self.call_planner(
            "start",
            json!({"plan_id": plan_id, "task_id": task_id}),
        )
        .await?;

        // 6. Build prompt with direction context.
        let prompt = build_task_prompt(task_title, task_desc, &directions);

        // 7. Spawn sub-agent.
        let descriptor = AgentDescriptor {
            agent_id: String::new(),
            display_name: format!("orch-{}", uuid::Uuid::new_v4()),
            provider: String::new(),
            model: String::new(),
            soul_path: None,
            allowed_tools: None,
            denied_tools: Vec::new(),
            allowed_skills: None,
            enabled: true,
            capabilities: Vec::new(),
            queue_max_size: 5,
            max_context_tokens: None,
            max_output_tokens: None,
        };

        let soul = SoulSnapshot::new(
            "orchestrator-worker",
            "You are an autonomous task executor working on a structured research plan. \
             Read the task description carefully. Execute the task using available tools. \
             When done, provide a clear summary of what you found, including any errors \
             or obstacles encountered. Be honest about what worked and what didn't.",
        );

        let result = self
            .subagent_spawner
            .spawn(descriptor, soul, prompt, false)
            .await?;

        let reply = result.reply.clone();

        // 8. Evaluate result.
        let made_progress = evaluate_reply(&reply);

        // 9. Record findings if there's content.
        if !reply.is_empty() && made_progress {
            let summary: String = if reply.len() > 500 {
                format!("{}… (truncated)", &reply[..500])
            } else {
                reply.clone()
            };
            let _ = self
                .call_planner(
                    "append_finding",
                    json!({
                        "plan_id": plan_id,
                        "task_id": task_id,
                        "finding": summary,
                        "confidence": 0.7,
                    }),
                )
                .await;
        }

        // 10. Complete or mark stale.
        if made_progress {
            self.call_planner(
                "complete",
                json!({
                    "plan_id": plan_id,
                    "task_id": task_id,
                    "result": reply,
                }),
            )
            .await?;
        } else {
            self.call_planner(
                "increment_stale",
                json!({"plan_id": plan_id}),
            )
            .await?;
        }

        self.publish_plan_event("plan:iteration_completed", json!({
            "plan_id": plan_id,
            "cycle": cycle,
            "task_id": task_id,
            "made_progress": made_progress,
            "stale_count": if made_progress { 0 } else { stale_count + 1 },
        }))
        .await;

        Ok(CycleOutcome::Continue)
    }

    // ── Helpers ──────────────────────────────────────────────────────

    /// Call a planner operation and return the JSON result.
    async fn call_planner(&self, operation: &str, params: Value) -> AmanResult<Value> {
        let tool = self
            .tool_registry
            .get("planner")
            .ok_or_else(|| Error::NotFound {
                name: "tool:planner".to_owned(),
            })?;

        let mut params = params;
        if let Some(obj) = params.as_object_mut() {
            obj.insert("operation".to_owned(), json!(operation));
        }

        let ctx = ToolContext::default();
        tool.execute(params, ctx).await
    }

    /// Generate a pivot direction when the current approach is stale.
    async fn do_pivot(&self, plan_id: &str, status: &Value) -> AmanResult<()> {
        let next_task = &status["next_task"];
        let task_id = next_task["id"].as_str().unwrap_or("");
        if task_id.is_empty() {
            return Ok(());
        }

        let directions = &next_task["directions_tried"];
        let tried_count = directions.as_array().map(|a| a.len()).unwrap_or(0);
        let pivot_id = format!("pivot-{}", tried_count + 1);

        let previous: Vec<String> = directions
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|d| d["description"].as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();

        let previous_summary = if previous.is_empty() {
            "none".to_owned()
        } else {
            previous.join("; ")
        };

        let _ = self
            .call_planner(
                "record_direction",
                json!({
                    "plan_id": plan_id,
                    "task_id": task_id,
                    "direction": {
                        "id": pivot_id,
                        "description": format!(
                            "Pivot #{}: try a substantively different approach. \
                             Previously tried: [{}]",
                            tried_count + 1, previous_summary
                        ),
                    },
                }),
            )
            .await;

        tracing::info!(
            agent_id = %self.agent_id,
            plan_id = %plan_id,
            task_id = %task_id,
            pivot = %pivot_id,
            previous_count = tried_count,
            "orchestrator: pivot direction generated"
        );

        Ok(())
    }

    /// Publish a plan lifecycle event to the agent's local bus.
    async fn publish_plan_event(&self, event_type: &str, payload: Value) {
        if let Some(bus) = self.registry.get_local_bus(&self.agent_id).await {
            let _ = bus
                .publish(Event::new(
                    "orchestrator",
                    EventType::Custom(event_type.to_owned()),
                    payload,
                ))
                .await;
        }
    }

    /// Wait until the agent is idle before proceeding.
    async fn wait_until_idle(&self) {
        loop {
            let is_idle = self
                .registry
                .get_system_state(&self.agent_id)
                .await
                .map(|ss| {
                    *ss.lock().expect("system_state lock") == AgentSystemState::Idle
                })
                .unwrap_or(true); // No state = assume idle
            if is_idle {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        }
    }
}

// ── Running guard ────────────────────────────────────────────────────

/// Resets `running` to false on drop.
struct RunningGuard<'a> {
    orchestrator: &'a Orchestrator,
}

impl Drop for RunningGuard<'_> {
    fn drop(&mut self) {
        self.orchestrator.running.store(false, Ordering::SeqCst);
    }
}

// ── Cycle outcome ────────────────────────────────────────────────────

enum CycleOutcome {
    Continue,
    PlanComplete,
    Escalated { reason: String },
}

// ── Reply evaluation ─────────────────────────────────────────────────

/// Evaluate a sub-agent's reply to determine if progress was made.
fn evaluate_reply(reply: &str) -> bool {
    if reply.is_empty() {
        return false;
    }
    let lower = reply.to_lowercase();
    // Fatal error markers — treat as no progress.
    let fatal_markers = [
        "permission_denied",
        "tool not found:",
        "hardline_blocked",
        "security_denied",
    ];
    if fatal_markers.iter().any(|m| lower.contains(m)) {
        return false;
    }
    // Generic error prefix — likely a tool failure.
    if lower.starts_with("error:") || lower.starts_with("failed:") {
        return false;
    }
    true
}

// ── Prompt builder ───────────────────────────────────────────────────

/// Build a task prompt for the sub-agent, including direction history
/// so the LLM knows which approaches have already been tried.
fn build_task_prompt(title: &str, description: &str, directions: &Value) -> String {
    let directions_text = directions
        .as_array()
        .map(|arr| {
            if arr.is_empty() {
                String::new()
            } else {
                let items: Vec<String> = arr
                    .iter()
                    .enumerate()
                    .map(|(i, d)| {
                        let desc = d["description"].as_str().unwrap_or("unknown");
                        format!("  {}. {}", i + 1, desc)
                    })
                    .collect();
                format!(
                    "\n\n## Previously Tried Directions (DO NOT repeat these)\n{}",
                    items.join("\n")
                )
            }
        })
        .unwrap_or_default();

    format!(
        "## Task: {title}\n\n{description}{directions_text}\n\n\
         ## Instructions\n\
         - Complete the task described above.\n\
         - If previous directions were tried, use a DIFFERENT approach.\n\
         - Report your findings clearly. Include what worked and what didn't.\n\
         - If you encounter errors, describe them so the next attempt can adjust."
    )
}

// ── Event handler for plan lifecycle events ──────────────────────────

use async_trait::async_trait;
use event_bus::EventHandler;

pub struct PlanEventHandler {
    orchestrator: Arc<Orchestrator>,
}

impl PlanEventHandler {
    pub fn new(orchestrator: Arc<Orchestrator>) -> Self {
        Self { orchestrator }
    }
}

#[async_trait]
impl EventHandler for PlanEventHandler {
    async fn handle(&self, event: Event) -> kernel::AmanResult<()> {
        let plan_id = event
            .payload
            .get("plan_id")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if !plan_id.is_empty() {
            self.orchestrator.on_plan_event(plan_id.to_owned());
        }
        Ok(())
    }
}
