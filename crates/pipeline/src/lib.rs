#![forbid(unsafe_code)]
#![doc = "Pipeline engine primitives for the Aman agent framework."]

use kernel::context::{BaseContext, PipelineContext, ToolContext};
use kernel::event::Event;
use kernel::pipeline::{PipelineStep, StepType};
use kernel::retry::{CompensationContract, RetryBackoff};
use kernel::types::{ConcurrencyModel, TraceId};
use kernel::{AmanResult, Error};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};
use uuid::Uuid;

#[derive(Debug, Clone)]
pub struct PipelineDefinition {
    pub id: String,
    pub concurrency: ConcurrencyModel,
    pub steps: Vec<PipelineStep>,
    pub compensation_contract: CompensationContract,
}

impl PipelineDefinition {
    #[must_use]
    pub fn new(id: impl Into<String>, concurrency: ConcurrencyModel, steps: Vec<PipelineStep>) -> Self {
        Self {
            id: id.into(),
            concurrency,
            steps,
            compensation_contract: CompensationContract::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PipelineFailureReason {
    PipelineFailed,
    CompensationFailed,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeadLetterRecord {
    pub pipeline_id: String,
    pub event: Event,
    pub failed_step_id: String,
    pub reason: PipelineFailureReason,
    pub message: String,
}

#[derive(Debug, Clone)]
pub struct PipelineInstance {
    pub id: String,
    pub pipeline_id: String,
    pub compensation_stack: Vec<CompensationTask>,
    pub temp_dir: PathBuf,
    pub optimistic_lock_token: Option<String>,
}

impl PipelineInstance {
    fn new(pipeline_id: &str, concurrency: &ConcurrencyModel) -> Self {
        let id = Uuid::now_v7().to_string();
        let temp_dir = std::env::temp_dir().join(format!("aman-pipeline-{pipeline_id}-{id}"));
        let optimistic_lock_token = if matches!(concurrency, ConcurrencyModel::Parallel) {
            Some(Uuid::now_v7().to_string())
        } else {
            None
        };
        Self {
            id,
            pipeline_id: pipeline_id.to_owned(),
            compensation_stack: Vec::new(),
            temp_dir,
            optimistic_lock_token,
        }
    }
}

#[derive(Clone)]
pub struct CompensationTask {
    pub step_id: String,
    pub tool: Arc<dyn kernel::tool::Tool>,
    pub event: Event,
}

impl std::fmt::Debug for CompensationTask {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CompensationTask")
            .field("step_id", &self.step_id)
            .field("event_id", &self.event.id)
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompensationResult {
    FullyCompensated,
    PartiallyCompensated { failed_steps: Vec<String> },
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CompensationStatusLog {
    pub pipeline_id: String,
    pub instance_id: String,
    pub compensated_steps: Vec<String>,
    pub failed_steps: Vec<String>,
    pub timed_out: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompensationExecutionReport {
    pub result: CompensationResult,
    pub status_log: CompensationStatusLog,
}

#[derive(Default)]
pub struct CompensationEngine;

impl CompensationEngine {
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    pub async fn compensate(
        &self,
        mut stack: Vec<CompensationTask>,
        contract: &CompensationContract,
        pipeline_id: &str,
        instance_id: &str,
        trace_id: TraceId,
    ) -> CompensationExecutionReport {
        let mut failed_steps = Vec::new();
        let mut compensated_steps = Vec::new();
        let mut timed_out = false;
        let started_at = Instant::now();
        let timeout = Duration::from_secs(contract.timeout_sec);

        while let Some(task) = stack.pop() {
            if started_at.elapsed() >= timeout {
                timed_out = true;
                failed_steps.push(task.step_id);
                while let Some(remaining) = stack.pop() {
                    failed_steps.push(remaining.step_id);
                }
                break;
            }

            let mut attempt = 1;
            let max_attempts = contract.retry_count.max(1);
            let mut success = false;
            while attempt <= max_attempts {
                if started_at.elapsed() >= timeout {
                    timed_out = true;
                    break;
                }
                let mut base = BaseContext::new(trace_id);
                base.labels
                    .insert("pipeline_id".to_owned(), pipeline_id.to_owned());
                base.labels
                    .insert("instance_id".to_owned(), instance_id.to_owned());
                base.labels
                    .insert("compensation_scope".to_owned(), "instance".to_owned());
                let context = ToolContext {
                    base,
                    tool_name: Some(task.tool.name().to_owned()),
                    working_directory: Some(format!(
                        "pipeline://{pipeline_id}/{instance_id}/compensation"
                    )),
                };
                let params = json!({
                    "event": task.event.clone(),
                    "pipeline_id": pipeline_id,
                    "instance_id": instance_id,
                    "compensate_step_id": task.step_id,
                    "attempt": attempt,
                });
                if task.tool.execute(params, context).await.is_ok() {
                    success = true;
                    break;
                }
                wait_backoff(&contract.retry_backoff, attempt);
                attempt += 1;
            }
            if success {
                compensated_steps.push(task.step_id);
            } else {
                failed_steps.push(task.step_id);
            }
        }

        let result = if failed_steps.is_empty() {
            CompensationResult::FullyCompensated
        } else {
            CompensationResult::PartiallyCompensated { failed_steps }
        };
        let failed_steps = match &result {
            CompensationResult::FullyCompensated => Vec::new(),
            CompensationResult::PartiallyCompensated { failed_steps } => failed_steps.clone(),
        };

        CompensationExecutionReport {
            result,
            status_log: CompensationStatusLog {
                pipeline_id: pipeline_id.to_owned(),
                instance_id: instance_id.to_owned(),
                compensated_steps,
                failed_steps,
                timed_out,
            },
        }
    }
}

#[derive(Default)]
pub struct ConcurrencyController {
    states: Mutex<HashMap<String, Arc<PipelineConcurrencyState>>>,
}

impl ConcurrencyController {
    pub fn enter(&self, pipeline_id: &str, model: &ConcurrencyModel) -> ConcurrencyGuard {
        let state = {
            let mut states = self.states.lock().expect("concurrency states mutex");
            Arc::clone(
                states
                    .entry(pipeline_id.to_owned())
                    .or_insert_with(|| Arc::new(PipelineConcurrencyState::default())),
            )
        };

        let mut running = state.running.lock().expect("concurrency running mutex");
        match model {
            ConcurrencyModel::Serial => {
                while *running > 0 {
                    running = state
                        .wakeup
                        .wait(running)
                        .expect("concurrency wait should not fail");
                }
                *running = 1;
            }
            ConcurrencyModel::Limited(limit) => {
                let limit = (*limit).max(1);
                while *running >= limit {
                    running = state
                        .wakeup
                        .wait(running)
                        .expect("concurrency wait should not fail");
                }
                *running += 1;
            }
            ConcurrencyModel::Parallel => {
                *running += 1;
            }
        }
        drop(running);

        ConcurrencyGuard {
            state,
        }
    }
}

#[derive(Default)]
struct PipelineConcurrencyState {
    running: Mutex<usize>,
    wakeup: Condvar,
}

pub struct ConcurrencyGuard {
    state: Arc<PipelineConcurrencyState>,
}

impl Drop for ConcurrencyGuard {
    fn drop(&mut self) {
        let mut running = self
            .state
            .running
            .lock()
            .expect("concurrency running mutex");
        *running = running.saturating_sub(1);
        if *running == 0 {
            self.state.wakeup.notify_all();
        } else {
            self.state.wakeup.notify_one();
        }
    }
}

#[derive(Default)]
pub struct PipelineEngine {
    dead_letters: Arc<Mutex<VecDeque<DeadLetterRecord>>>,
    compensation_logs: Arc<Mutex<VecDeque<CompensationStatusLog>>>,
    compensation_alerts: Arc<Mutex<VecDeque<String>>>,
    compensation_engine: CompensationEngine,
    concurrency_controller: ConcurrencyController,
}

impl PipelineEngine {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn dead_letters(&self) -> Vec<DeadLetterRecord> {
        self.dead_letters
            .lock()
            .expect("dead letter mutex should not be poisoned")
            .iter()
            .cloned()
            .collect()
    }

    #[must_use]
    pub fn compensation_logs(&self) -> Vec<CompensationStatusLog> {
        self.compensation_logs
            .lock()
            .expect("compensation log mutex should not be poisoned")
            .iter()
            .cloned()
            .collect()
    }

    #[must_use]
    pub fn compensation_alerts(&self) -> Vec<String> {
        self.compensation_alerts
            .lock()
            .expect("compensation alert mutex should not be poisoned")
            .iter()
            .cloned()
            .collect()
    }

    pub async fn execute(
        &self,
        pipeline: &PipelineDefinition,
        event: Event,
    ) -> AmanResult<Vec<Event>> {
        let _guard = self
            .concurrency_controller
            .enter(&pipeline.id, &pipeline.concurrency);

        let trace_id = event.metadata.trace_id;
        let mut instance = PipelineInstance::new(&pipeline.id, &pipeline.concurrency);
        let mut events = vec![event];

        for step in &pipeline.steps {
            let failed_event = events.first().cloned().unwrap_or_else(|| {
                Event::new("pipeline:internal", "pipeline_error".into(), json!({}))
            });
            match self
                .execute_step(pipeline, step, events, trace_id, &mut instance)
                .await
            {
                Ok(next_events) => events = next_events,
                Err(error) => {
                    let compensation_report = self
                        .compensation_engine
                        .compensate(
                            instance.compensation_stack.clone(),
                            &pipeline.compensation_contract,
                            &pipeline.id,
                            &instance.id,
                            trace_id,
                        )
                        .await;
                    let compensation_result = compensation_report.result.clone();
                    self.compensation_logs
                        .lock()
                        .expect("compensation log mutex should not be poisoned")
                        .push_back(compensation_report.status_log.clone());

                    let reason = match compensation_result {
                        CompensationResult::FullyCompensated => PipelineFailureReason::PipelineFailed,
                        CompensationResult::PartiallyCompensated { .. } => {
                            PipelineFailureReason::CompensationFailed
                        }
                    };

                    self.record_dead_letter(
                        &pipeline.id,
                        failed_event,
                        step.id.clone(),
                        reason,
                        format!("{error}"),
                    );

                    if let CompensationResult::PartiallyCompensated { failed_steps } = compensation_result {
                        self.compensation_alerts
                            .lock()
                            .expect("compensation alert mutex should not be poisoned")
                            .push_back(format!(
                                "COMPENSATION_FAILED pipeline={} instance={} failed_steps={}",
                                pipeline.id,
                                instance.id,
                                failed_steps.join(",")
                            ));
                        return Err(Error::CompensationFailed {
                            message: format!(
                                "pipeline `{}` compensation failed for steps: {}",
                                pipeline.id,
                                failed_steps.join(",")
                            ),
                        });
                    }
                    return Err(error);
                }
            }

            if events.is_empty() {
                break;
            }
        }

        Ok(events)
    }

    fn record_dead_letter(
        &self,
        pipeline_id: &str,
        event: Event,
        failed_step_id: String,
        reason: PipelineFailureReason,
        message: String,
    ) {
        let mut dead_letters = self
            .dead_letters
            .lock()
            .expect("dead letter mutex should not be poisoned");
        dead_letters.push_back(DeadLetterRecord {
            pipeline_id: pipeline_id.to_owned(),
            event,
            failed_step_id,
            reason,
            message,
        });
    }

    async fn execute_step(
        &self,
        pipeline: &PipelineDefinition,
        step: &PipelineStep,
        input_events: Vec<Event>,
        trace_id: TraceId,
        instance: &mut PipelineInstance,
    ) -> AmanResult<Vec<Event>> {
        match step.step_type {
            StepType::Filter => {
                let mut output = Vec::new();
                for event in input_events {
                    let compensation_event = event.clone();
                    let tool_output = self
                        .execute_tool_with_retry(pipeline, step, &event, trace_id, instance)
                        .await?;
                    if extract_filter_result(&tool_output) {
                        output.push(event);
                    }
                    if let Some(compensate_tool) = &step.compensate {
                        instance.compensation_stack.push(CompensationTask {
                            step_id: step.id.clone(),
                            tool: Arc::clone(compensate_tool),
                            event: compensation_event,
                        });
                    }
                }
                Ok(output)
            }
            StepType::Transform => {
                let mut output = Vec::new();
                for event in input_events {
                    let tool_output = self
                        .execute_tool_with_retry(pipeline, step, &event, trace_id, instance)
                        .await?;
                    let transformed = extract_transform_events(&event, tool_output)?;
                    if let Some(compensate_tool) = &step.compensate {
                        for compensation_event in &transformed {
                            instance.compensation_stack.push(CompensationTask {
                                step_id: step.id.clone(),
                                tool: Arc::clone(compensate_tool),
                                event: compensation_event.clone(),
                            });
                        }
                    }
                    output.extend(transformed);
                }
                Ok(output)
            }
            StepType::Action => {
                for event in &input_events {
                    self.execute_tool_with_retry(pipeline, step, event, trace_id, instance)
                        .await?;
                    if let Some(compensate_tool) = &step.compensate {
                        instance.compensation_stack.push(CompensationTask {
                            step_id: step.id.clone(),
                            tool: Arc::clone(compensate_tool),
                            event: event.clone(),
                        });
                    }
                }
                Ok(input_events)
            }
        }
    }

    async fn execute_tool_with_retry(
        &self,
        pipeline: &PipelineDefinition,
        step: &PipelineStep,
        event: &Event,
        trace_id: TraceId,
        instance: &PipelineInstance,
    ) -> AmanResult<Value> {
        let max_attempts = step.retry.max_attempts.max(1);
        let mut attempt = 1;
        loop {
            let mut base = BaseContext::new(trace_id);
            base.labels
                .insert("pipeline_id".to_owned(), pipeline.id.clone());
            base.labels
                .insert("instance_id".to_owned(), instance.id.clone());
            base.labels.insert(
                "concurrency_model".to_owned(),
                format!("{:?}", pipeline.concurrency).to_lowercase(),
            );
            if let Some(token) = &instance.optimistic_lock_token {
                base.labels
                    .insert("optimistic_lock_token".to_owned(), token.clone());
            }
            base.extensions.insert(
                "instance_temp_dir".to_owned(),
                Value::String(instance.temp_dir.display().to_string()),
            );

            let context = ToolContext {
                base,
                tool_name: Some(step.tool.name().to_owned()),
                working_directory: Some(
                    instance
                        .temp_dir
                        .join(format!("step-{}", step.id))
                        .display()
                        .to_string(),
                ),
            };
            let params = json!({
                "event": event,
                "pipeline_id": pipeline.id,
                "instance_id": instance.id,
                "step_id": step.id,
                "optimistic_lock_token": instance.optimistic_lock_token,
                "attempt": attempt,
            });

            match step.tool.execute(params, context).await {
                Ok(value) => return Ok(value),
                Err(error) if attempt < max_attempts => {
                    wait_backoff(&step.retry.retry_backoff, attempt);
                    attempt += 1;
                    let _ = error;
                }
                Err(error) => return Err(error),
            }
        }
    }
}

fn wait_backoff(backoff: &RetryBackoff, attempt: u32) {
    let delay = match backoff {
        RetryBackoff::Immediate => 0,
        RetryBackoff::Fixed(ms) => *ms,
        RetryBackoff::Exponential => 100_u64.saturating_mul(2_u64.saturating_pow(attempt - 1)),
        RetryBackoff::Sequence(steps) => {
            let index = usize::try_from(attempt.saturating_sub(1)).unwrap_or(usize::MAX);
            *steps
                .get(index)
                .or_else(|| steps.last())
                .unwrap_or(&0_u64)
        }
    };

    if delay > 0 {
        std::thread::sleep(std::time::Duration::from_millis(delay.min(5)));
    }
}

fn extract_filter_result(output: &Value) -> bool {
    if let Some(pass) = output.get("pass").and_then(Value::as_bool) {
        return pass;
    }
    output.as_bool().unwrap_or(true)
}

fn extract_transform_events(event: &Event, output: Value) -> AmanResult<Vec<Event>> {
    if let Some(events_value) = output.get("events") {
        let events = serde_json::from_value::<Vec<Event>>(events_value.clone())?;
        return Ok(events);
    }
    if let Some(event_value) = output.get("event") {
        let transformed = serde_json::from_value::<Event>(event_value.clone())?;
        return Ok(vec![transformed]);
    }

    let mut transformed = event.clone();
    transformed.payload = output;
    Ok(vec![transformed])
}

#[must_use]
pub fn pipeline_context_for(pipeline_id: impl Into<String>, trace_id: TraceId) -> PipelineContext {
    PipelineContext {
        base: BaseContext::new(trace_id),
        pipeline_id: Some(pipeline_id.into()),
        instance_id: Some(Uuid::now_v7().to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::{pipeline_context_for, PipelineDefinition, PipelineEngine};
    use kernel::context::{BaseContext, ToolContext};
    use kernel::event::{Event, EventType};
    use kernel::pipeline::{PipelineStep, StepType};
    use kernel::retry::{CompensationContract, RetryBackoff, RetryPolicy};
    use kernel::schema::JsonSchema;
    use kernel::tool::Tool;
    use kernel::types::{ConcurrencyModel, ToolMode, TraceId};
    use kernel::{AmanResult, Error};
    use serde_json::{json, Value};
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Barrier, Mutex};
    use std::thread;
    use std::time::Duration;

    #[derive(Debug)]
    struct RecordingTool {
        name: String,
        results: Mutex<VecDeque<AmanResult<Value>>>,
        calls: Arc<Mutex<Vec<String>>>,
    }

    impl RecordingTool {
        fn new(
            name: impl Into<String>,
            results: Vec<AmanResult<Value>>,
            calls: Arc<Mutex<Vec<String>>>,
        ) -> Self {
            Self {
                name: name.into(),
                results: Mutex::new(VecDeque::from(results)),
                calls,
            }
        }
    }

    #[async_trait::async_trait]
    impl Tool for RecordingTool {
        fn name(&self) -> &str {
            &self.name
        }

        fn mode(&self) -> ToolMode {
            ToolMode::Local
        }

        fn parameters(&self) -> &JsonSchema {
            static PARAMS: std::sync::LazyLock<JsonSchema> = std::sync::LazyLock::new(|| {
                JsonSchema::from(json!({"type": "object"}))
            });
            &PARAMS
        }

        fn returns(&self) -> &JsonSchema {
            static RETURNS: std::sync::LazyLock<JsonSchema> = std::sync::LazyLock::new(|| {
                JsonSchema::from(json!({"type": "object"}))
            });
            &RETURNS
        }

        async fn execute(&self, params: Value, _ctx: ToolContext) -> AmanResult<Value> {
            let step_ref = params["step_id"]
                .as_str()
                .or_else(|| params["compensate_step_id"].as_str())
                .unwrap_or("");
            self.calls
                .lock()
                .expect("calls mutex")
                .push(format!("{}:{step_ref}", self.name));
            self.results
                .lock()
                .expect("results mutex")
                .pop_front()
                .unwrap_or_else(|| Ok(json!({})))
        }
    }

    #[derive(Debug)]
    struct ConcurrencyProbeTool {
        active: Arc<AtomicUsize>,
        max_active: Arc<AtomicUsize>,
        sleep_ms: u64,
        captures: Arc<Mutex<Vec<(String, String, Option<String>, String)>>>,
    }

    #[async_trait::async_trait]
    impl Tool for ConcurrencyProbeTool {
        fn name(&self) -> &str {
            "concurrency-probe"
        }

        fn mode(&self) -> ToolMode {
            ToolMode::Local
        }

        fn parameters(&self) -> &JsonSchema {
            static PARAMS: std::sync::LazyLock<JsonSchema> = std::sync::LazyLock::new(|| {
                JsonSchema::from(json!({"type": "object"}))
            });
            &PARAMS
        }

        fn returns(&self) -> &JsonSchema {
            static RETURNS: std::sync::LazyLock<JsonSchema> = std::sync::LazyLock::new(|| {
                JsonSchema::from(json!({"type": "object"}))
            });
            &RETURNS
        }

        async fn execute(&self, params: Value, ctx: ToolContext) -> AmanResult<Value> {
            let current = self.active.fetch_add(1, Ordering::SeqCst) + 1;
            let mut observed = self.max_active.load(Ordering::SeqCst);
            while current > observed
                && self
                    .max_active
                    .compare_exchange(observed, current, Ordering::SeqCst, Ordering::SeqCst)
                    .is_err()
            {
                observed = self.max_active.load(Ordering::SeqCst);
            }

            self.captures
                .lock()
                .expect("captures mutex")
                .push((
                    ctx.base
                        .labels
                        .get("instance_id")
                        .cloned()
                        .unwrap_or_default(),
                    ctx.base
                        .extensions
                        .get("instance_temp_dir")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                    params
                        .get("optimistic_lock_token")
                        .and_then(Value::as_str)
                        .map(ToOwned::to_owned),
                    ctx.base
                        .labels
                        .get("concurrency_model")
                        .cloned()
                        .unwrap_or_default(),
                ));

            thread::sleep(Duration::from_millis(self.sleep_ms));
            self.active.fetch_sub(1, Ordering::SeqCst);
            Ok(json!({"ok": true}))
        }
    }

    fn test_event() -> Event {
        Event::new("timer:test", EventType::TimerTick, json!({"value": 1}))
    }

    #[test]
    fn pipeline_runs_filter_transform_action_serially() {
        pollster::block_on(async {
            let calls = Arc::new(Mutex::new(Vec::new()));
            let filter_tool = Arc::new(RecordingTool::new(
                "filter",
                vec![Ok(json!({"pass": true}))],
                Arc::clone(&calls),
            ));
            let transform_tool = Arc::new(RecordingTool::new(
                "transform",
                vec![Ok(json!({"event": {
                    "id": test_event().id,
                    "source": "timer:test",
                    "event_type": "timer_tick",
                    "timestamp": test_event().timestamp,
                    "priority": "normal",
                    "delivery": "at_least_once",
                    "dedup_key": test_event().dedup_key,
                    "payload": {"value": 2},
                    "metadata": test_event().metadata,
                }}))],
                Arc::clone(&calls),
            ));
            let action_tool = Arc::new(RecordingTool::new(
                "action",
                vec![Ok(json!({"ok": true}))],
                Arc::clone(&calls),
            ));

            let pipeline = PipelineDefinition {
                id: "pipe-a".to_owned(),
                concurrency: ConcurrencyModel::Serial,
                steps: vec![
                    PipelineStep {
                        id: "s1".to_owned(),
                        step_type: StepType::Filter,
                        tool: filter_tool,
                        compensate: None,
                        retry: RetryPolicy::default(),
                    },
                    PipelineStep {
                        id: "s2".to_owned(),
                        step_type: StepType::Transform,
                        tool: transform_tool,
                        compensate: None,
                        retry: RetryPolicy::default(),
                    },
                    PipelineStep {
                        id: "s3".to_owned(),
                        step_type: StepType::Action,
                        tool: action_tool,
                        compensate: None,
                        retry: RetryPolicy::default(),
                    },
                ],
                compensation_contract: CompensationContract::default(),
            };

            let output = PipelineEngine::new()
                .execute(&pipeline, test_event())
                .await
                .expect("pipeline execution succeeds");

            assert_eq!(output.len(), 1);
            assert_eq!(output[0].payload, json!({"value": 2}));
            assert_eq!(
                calls.lock().expect("calls mutex").clone(),
                vec![
                    "filter:s1".to_owned(),
                    "transform:s2".to_owned(),
                    "action:s3".to_owned()
                ]
            );
        });
    }

    #[test]
    fn step_retry_reaches_success_before_max_attempts() {
        pollster::block_on(async {
            let calls = Arc::new(Mutex::new(Vec::new()));
            let flaky_tool = Arc::new(RecordingTool::new(
                "action",
                vec![
                    Err(Error::Unrecoverable {
                        message: "first fail".to_owned(),
                    }),
                    Ok(json!({"ok": true})),
                ],
                Arc::clone(&calls),
            ));

            let pipeline = PipelineDefinition {
                id: "pipe-retry".to_owned(),
                concurrency: ConcurrencyModel::Serial,
                steps: vec![PipelineStep {
                    id: "retry-step".to_owned(),
                    step_type: StepType::Action,
                    tool: flaky_tool,
                    compensate: None,
                    retry: RetryPolicy {
                        max_attempts: 2,
                        retry_backoff: RetryBackoff::Immediate,
                    },
                }],
                compensation_contract: CompensationContract::default(),
            };

            let output = PipelineEngine::new()
                .execute(&pipeline, test_event())
                .await
                .expect("retry should recover");
            assert_eq!(output.len(), 1);
            assert_eq!(calls.lock().expect("calls mutex").len(), 2);
        });
    }

    #[test]
    fn failed_step_is_recorded_to_dead_letter() {
        pollster::block_on(async {
            let calls = Arc::new(Mutex::new(Vec::new()));
            let failing_tool = Arc::new(RecordingTool::new(
                "action",
                vec![Err(Error::Unrecoverable {
                    message: "boom".to_owned(),
                })],
                Arc::clone(&calls),
            ));

            let pipeline = PipelineDefinition {
                id: "pipe-dlq".to_owned(),
                concurrency: ConcurrencyModel::Serial,
                steps: vec![PipelineStep {
                    id: "failed-step".to_owned(),
                    step_type: StepType::Action,
                    tool: failing_tool,
                    compensate: None,
                    retry: RetryPolicy {
                        max_attempts: 1,
                        retry_backoff: RetryBackoff::Immediate,
                    },
                }],
                compensation_contract: CompensationContract::default(),
            };

            let engine = PipelineEngine::new();
            let error = engine.execute(&pipeline, test_event()).await.expect_err("fails");
            assert!(matches!(error, Error::Unrecoverable { .. }));

            let dead_letters = engine.dead_letters();
            assert_eq!(dead_letters.len(), 1);
            assert_eq!(dead_letters[0].pipeline_id, "pipe-dlq");
            assert_eq!(dead_letters[0].failed_step_id, "failed-step");
        });
    }

    #[test]
    fn pipeline_context_contains_pipeline_and_instance_id() {
        let context = pipeline_context_for("pipe-x", TraceId::new());
        assert_eq!(context.pipeline_id.as_deref(), Some("pipe-x"));
        assert!(context.instance_id.is_some());
        assert_eq!(context.base, BaseContext::new(context.base.trace_id));
    }

    #[test]
    fn compensation_runs_in_reverse_order_on_failure() {
        pollster::block_on(async {
            let calls = Arc::new(Mutex::new(Vec::new()));
            let action_1 = Arc::new(RecordingTool::new(
                "a1",
                vec![Ok(json!({"ok": true}))],
                Arc::clone(&calls),
            ));
            let action_2 = Arc::new(RecordingTool::new(
                "a2",
                vec![Ok(json!({"ok": true}))],
                Arc::clone(&calls),
            ));
            let action_3 = Arc::new(RecordingTool::new(
                "a3",
                vec![Err(Error::Unrecoverable {
                    message: "break".to_owned(),
                })],
                Arc::clone(&calls),
            ));
            let compensate_1 = Arc::new(RecordingTool::new(
                "c1",
                vec![Ok(json!({"ok": true}))],
                Arc::clone(&calls),
            ));
            let compensate_2 = Arc::new(RecordingTool::new(
                "c2",
                vec![Ok(json!({"ok": true}))],
                Arc::clone(&calls),
            ));

            let mut pipeline = PipelineDefinition::new(
                "pipe-comp",
                ConcurrencyModel::Serial,
                vec![
                    PipelineStep {
                        id: "s1".to_owned(),
                        step_type: StepType::Action,
                        tool: action_1,
                        compensate: Some(compensate_1),
                        retry: RetryPolicy::default(),
                    },
                    PipelineStep {
                        id: "s2".to_owned(),
                        step_type: StepType::Action,
                        tool: action_2,
                        compensate: Some(compensate_2),
                        retry: RetryPolicy::default(),
                    },
                    PipelineStep {
                        id: "s3".to_owned(),
                        step_type: StepType::Action,
                        tool: action_3,
                        compensate: None,
                        retry: RetryPolicy {
                            max_attempts: 1,
                            retry_backoff: RetryBackoff::Immediate,
                        },
                    },
                ],
            );
            pipeline.compensation_contract.retry_count = 1;
            pipeline.compensation_contract.retry_backoff = RetryBackoff::Immediate;

            let error = PipelineEngine::new()
                .execute(&pipeline, test_event())
                .await
                .expect_err("pipeline should fail");
            assert!(matches!(error, Error::Unrecoverable { .. }));

            assert_eq!(
                calls.lock().expect("calls mutex").clone(),
                vec![
                    "a1:s1".to_owned(),
                    "a2:s2".to_owned(),
                    "a3:s3".to_owned(),
                    "c2:s2".to_owned(),
                    "c1:s1".to_owned(),
                ]
            );
        });
    }

    #[test]
    fn compensation_failure_returns_compensation_error() {
        pollster::block_on(async {
            let calls = Arc::new(Mutex::new(Vec::new()));
            let action = Arc::new(RecordingTool::new(
                "a1",
                vec![Ok(json!({"ok": true}))],
                Arc::clone(&calls),
            ));
            let fail = Arc::new(RecordingTool::new(
                "a2",
                vec![Err(Error::Unrecoverable {
                    message: "break".to_owned(),
                })],
                Arc::clone(&calls),
            ));
            let compensate = Arc::new(RecordingTool::new(
                "c1",
                vec![
                    Err(Error::Unrecoverable {
                        message: "comp fail".to_owned(),
                    }),
                    Err(Error::Unrecoverable {
                        message: "comp fail".to_owned(),
                    }),
                ],
                Arc::clone(&calls),
            ));

            let mut pipeline = PipelineDefinition::new(
                "pipe-comp-fail",
                ConcurrencyModel::Serial,
                vec![
                    PipelineStep {
                        id: "s1".to_owned(),
                        step_type: StepType::Action,
                        tool: action,
                        compensate: Some(compensate),
                        retry: RetryPolicy::default(),
                    },
                    PipelineStep {
                        id: "s2".to_owned(),
                        step_type: StepType::Action,
                        tool: fail,
                        compensate: None,
                        retry: RetryPolicy {
                            max_attempts: 1,
                            retry_backoff: RetryBackoff::Immediate,
                        },
                    },
                ],
            );
            pipeline.compensation_contract.retry_count = 2;
            pipeline.compensation_contract.retry_backoff = RetryBackoff::Immediate;

            let engine = PipelineEngine::new();
            let error = engine
                .execute(&pipeline, test_event())
                .await
                .expect_err("should surface compensation failure");
            assert!(matches!(error, Error::CompensationFailed { .. }));

            let dlq = engine.dead_letters();
            assert_eq!(dlq.len(), 1);
            assert_eq!(dlq[0].reason, super::PipelineFailureReason::CompensationFailed);
            let logs = engine.compensation_logs();
            assert_eq!(logs.len(), 1);
            assert_eq!(logs[0].failed_steps, vec!["s1".to_owned()]);
            let alerts = engine.compensation_alerts();
            assert_eq!(alerts.len(), 1);
            assert!(alerts[0].contains("COMPENSATION_FAILED"));
        });
    }

    #[test]
    fn limited_concurrency_model_is_runnable() {
        pollster::block_on(async {
            let calls = Arc::new(Mutex::new(Vec::new()));
            let action = Arc::new(RecordingTool::new(
                "action",
                vec![Ok(json!({"ok": true}))],
                Arc::clone(&calls),
            ));
            let pipeline = PipelineDefinition::new(
                "pipe-limited",
                ConcurrencyModel::Limited(2),
                vec![PipelineStep {
                    id: "s1".to_owned(),
                    step_type: StepType::Action,
                    tool: action,
                    compensate: None,
                    retry: RetryPolicy::default(),
                }],
            );

            let output = PipelineEngine::new()
                .execute(&pipeline, test_event())
                .await
                .expect("limited model should run");
            assert_eq!(output.len(), 1);
        });
    }

    #[test]
    fn parallel_concurrency_model_is_runnable() {
        pollster::block_on(async {
            let calls = Arc::new(Mutex::new(Vec::new()));
            let action = Arc::new(RecordingTool::new(
                "action",
                vec![Ok(json!({"ok": true}))],
                Arc::clone(&calls),
            ));
            let pipeline = PipelineDefinition::new(
                "pipe-parallel",
                ConcurrencyModel::Parallel,
                vec![PipelineStep {
                    id: "s1".to_owned(),
                    step_type: StepType::Action,
                    tool: action,
                    compensate: None,
                    retry: RetryPolicy::default(),
                }],
            );

            let output = PipelineEngine::new()
                .execute(&pipeline, test_event())
                .await
                .expect("parallel model should run");
            assert_eq!(output.len(), 1);
        });
    }

    #[test]
    fn serial_mode_enforces_single_inflight_instance() {
        let active = Arc::new(AtomicUsize::new(0));
        let max_active = Arc::new(AtomicUsize::new(0));
        let captures = Arc::new(Mutex::new(Vec::new()));
        let tool = Arc::new(ConcurrencyProbeTool {
            active: Arc::clone(&active),
            max_active: Arc::clone(&max_active),
            sleep_ms: 20,
            captures,
        });

        let pipeline = Arc::new(PipelineDefinition::new(
            "pipe-serial-queue",
            ConcurrencyModel::Serial,
            vec![PipelineStep {
                id: "s1".to_owned(),
                step_type: StepType::Action,
                tool,
                compensate: None,
                retry: RetryPolicy::default(),
            }],
        ));
        let engine = Arc::new(PipelineEngine::new());
        let start = Arc::new(Barrier::new(3));

        let mut handles = Vec::new();
        for i in 0..3 {
            let engine = Arc::clone(&engine);
            let pipeline = Arc::clone(&pipeline);
            let start = Arc::clone(&start);
            handles.push(thread::spawn(move || {
                start.wait();
                pollster::block_on(engine.execute(
                    &pipeline,
                    Event::new("timer:test", EventType::TimerTick, json!({"i": i})),
                ))
                .expect("serial execution should succeed");
            }));
        }
        for handle in handles {
            handle.join().expect("thread should join");
        }
        assert_eq!(max_active.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn limited_mode_limits_parallelism_to_n() {
        let active = Arc::new(AtomicUsize::new(0));
        let max_active = Arc::new(AtomicUsize::new(0));
        let captures = Arc::new(Mutex::new(Vec::new()));
        let tool = Arc::new(ConcurrencyProbeTool {
            active: Arc::clone(&active),
            max_active: Arc::clone(&max_active),
            sleep_ms: 20,
            captures,
        });

        let pipeline = Arc::new(PipelineDefinition::new(
            "pipe-limited-n",
            ConcurrencyModel::Limited(2),
            vec![PipelineStep {
                id: "s1".to_owned(),
                step_type: StepType::Action,
                tool,
                compensate: None,
                retry: RetryPolicy::default(),
            }],
        ));
        let engine = Arc::new(PipelineEngine::new());
        let start = Arc::new(Barrier::new(4));

        let mut handles = Vec::new();
        for i in 0..4 {
            let engine = Arc::clone(&engine);
            let pipeline = Arc::clone(&pipeline);
            let start = Arc::clone(&start);
            handles.push(thread::spawn(move || {
                start.wait();
                pollster::block_on(engine.execute(
                    &pipeline,
                    Event::new("timer:test", EventType::TimerTick, json!({"i": i})),
                ))
                .expect("limited execution should succeed");
            }));
        }
        for handle in handles {
            handle.join().expect("thread should join");
        }
        assert!(max_active.load(Ordering::SeqCst) <= 2);
    }

    #[test]
    fn parallel_mode_injects_isolation_context() {
        let active = Arc::new(AtomicUsize::new(0));
        let max_active = Arc::new(AtomicUsize::new(0));
        let captures = Arc::new(Mutex::new(Vec::new()));
        let tool = Arc::new(ConcurrencyProbeTool {
            active: Arc::clone(&active),
            max_active: Arc::clone(&max_active),
            sleep_ms: 10,
            captures: Arc::clone(&captures),
        });

        let pipeline = Arc::new(PipelineDefinition::new(
            "pipe-parallel-safe",
            ConcurrencyModel::Parallel,
            vec![PipelineStep {
                id: "s1".to_owned(),
                step_type: StepType::Action,
                tool,
                compensate: None,
                retry: RetryPolicy::default(),
            }],
        ));
        let engine = Arc::new(PipelineEngine::new());
        let start = Arc::new(Barrier::new(2));

        let mut handles = Vec::new();
        for i in 0..2 {
            let engine = Arc::clone(&engine);
            let pipeline = Arc::clone(&pipeline);
            let start = Arc::clone(&start);
            handles.push(thread::spawn(move || {
                start.wait();
                pollster::block_on(engine.execute(
                    &pipeline,
                    Event::new("timer:test", EventType::TimerTick, json!({"i": i})),
                ))
                .expect("parallel execution should succeed");
            }));
        }
        for handle in handles {
            handle.join().expect("thread should join");
        }

        assert!(max_active.load(Ordering::SeqCst) >= 2);
        let records = captures.lock().expect("captures mutex").clone();
        assert_eq!(records.len(), 2);
        assert_ne!(records[0].0, records[1].0);
        assert_ne!(records[0].1, records[1].1);
        assert!(records.iter().all(|(_, _, token, model)| token.is_some() && model == "parallel"));
    }

    #[test]
    fn compensation_timeout_marks_failed_and_emits_alert() {
        pollster::block_on(async {
            let calls = Arc::new(Mutex::new(Vec::new()));
            let action = Arc::new(RecordingTool::new(
                "a1",
                vec![Ok(json!({"ok": true}))],
                Arc::clone(&calls),
            ));
            let fail = Arc::new(RecordingTool::new(
                "a2",
                vec![Err(Error::Unrecoverable {
                    message: "break".to_owned(),
                })],
                Arc::clone(&calls),
            ));
            let compensate = Arc::new(RecordingTool::new(
                "c1",
                vec![Ok(json!({"ok": true}))],
                Arc::clone(&calls),
            ));

            let mut pipeline = PipelineDefinition::new(
                "pipe-timeout",
                ConcurrencyModel::Serial,
                vec![
                    PipelineStep {
                        id: "s1".to_owned(),
                        step_type: StepType::Action,
                        tool: action,
                        compensate: Some(compensate),
                        retry: RetryPolicy::default(),
                    },
                    PipelineStep {
                        id: "s2".to_owned(),
                        step_type: StepType::Action,
                        tool: fail,
                        compensate: None,
                        retry: RetryPolicy {
                            max_attempts: 1,
                            retry_backoff: RetryBackoff::Immediate,
                        },
                    },
                ],
            );
            pipeline.compensation_contract.timeout_sec = 0;

            let engine = PipelineEngine::new();
            let error = engine.execute(&pipeline, test_event()).await.expect_err("fails");
            assert!(matches!(error, Error::CompensationFailed { .. }));
            let logs = engine.compensation_logs();
            assert_eq!(logs.len(), 1);
            assert!(logs[0].timed_out);
            assert_eq!(logs[0].failed_steps, vec!["s1".to_owned()]);
            assert_eq!(engine.compensation_alerts().len(), 1);
        });
    }
}
