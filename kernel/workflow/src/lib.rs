#![forbid(unsafe_code)]
#![doc = "Workflow state machine engine for the aman agent framework."]
// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0


use async_trait::async_trait;
use kernel::event::{Event, EventType};
use kernel::retry::RetryBackoff;
use kernel::types::Timestamp;
use kernel::{AmanResult, Error};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StateDef {
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TransitionFrom {
    Specific(String),
    Any,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TransitionTo {
    Specific(String),
    LastActiveState,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TransitionAction {
    Pipeline(String),
    Skill(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Transition {
    pub from: TransitionFrom,
    pub event: String,
    pub to: TransitionTo,
    pub guard: Option<String>,
    pub on_fail: Option<TransitionTo>,
    pub action: Option<TransitionAction>,
    pub on_action_failure: Option<TransitionTo>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StateTimeout {
    pub state: String,
    pub timeout_ms: u64,
    pub on_timeout: TransitionTo,
    pub on_timeout_alert: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RetryFailurePolicy {
    Archive,
    ManualOnly,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorRecovery {
    pub auto_retry_count: u32,
    pub max_retry_count: u32,
    pub retry_backoff: RetryBackoff,
    pub on_retry_failure: RetryFailurePolicy,
}

impl Default for ErrorRecovery {
    fn default() -> Self {
        Self {
            auto_retry_count: 0,
            max_retry_count: 3,
            retry_backoff: RetryBackoff::Immediate,
            on_retry_failure: RetryFailurePolicy::ManualOnly,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowDef {
    pub name: String,
    pub states: Vec<StateDef>,
    pub initial_state: String,
    pub final_states: Vec<String>,
    pub error_state: String,
    pub transitions: Vec<Transition>,
    pub state_timeouts: Vec<StateTimeout>,
    pub error_recovery: ErrorRecovery,
}

impl WorkflowDef {
    pub fn validate(&self) -> AmanResult<()> {
        if self.name.trim().is_empty() {
            return Err(Error::ConfigInvalid {
                message: "workflow name cannot be empty".to_owned(),
            });
        }

        if self.states.is_empty() {
            return Err(Error::ConfigInvalid {
                message: format!("workflow `{}` must declare at least one state", self.name),
            });
        }

        let known = self
            .states
            .iter()
            .map(|state| normalize_token(&state.name))
            .collect::<Vec<_>>();
        let has_state = |name: &str| known.iter().any(|item| item == &normalize_token(name));

        if !has_state(&self.initial_state) {
            return Err(Error::ConfigInvalid {
                message: format!(
                    "workflow `{}` initial_state `{}` not found in states",
                    self.name, self.initial_state
                ),
            });
        }
        if !has_state(&self.error_state) {
            return Err(Error::ConfigInvalid {
                message: format!(
                    "workflow `{}` error_state `{}` not found in states",
                    self.name, self.error_state
                ),
            });
        }
        for state in &self.final_states {
            if !has_state(state) {
                return Err(Error::ConfigInvalid {
                    message: format!(
                        "workflow `{}` final_state `{}` not found in states",
                        self.name, state
                    ),
                });
            }
        }
        for timeout in &self.state_timeouts {
            if !has_state(&timeout.state) {
                return Err(Error::ConfigInvalid {
                    message: format!(
                        "workflow `{}` timeout state `{}` not found in states",
                        self.name, timeout.state
                    ),
                });
            }
        }
        Ok(())
    }

    #[must_use]
    pub fn normalized_state(&self, state: &str) -> String {
        normalize_token(state)
    }

    #[must_use]
    pub fn is_error_state(&self, state: &str) -> bool {
        self.normalized_state(state) == self.normalized_state(&self.error_state)
    }

    #[must_use]
    pub fn is_final_state(&self, state: &str) -> bool {
        let needle = self.normalized_state(state);
        self.final_states
            .iter()
            .any(|final_state| self.normalized_state(final_state) == needle)
    }

    #[must_use]
    pub fn find_transition(&self, current_state: &str, event: &str) -> Option<&Transition> {
        let current = self.normalized_state(current_state);
        let normalized_event = normalize_token(event);
        self.transitions.iter().find(|transition| {
            let from_match = match &transition.from {
                TransitionFrom::Any => true,
                TransitionFrom::Specific(expected) => self.normalized_state(expected) == current,
            };
            from_match && normalize_token(&transition.event) == normalized_event
        })
    }

    #[must_use]
    pub fn timeout_for(&self, state: &str) -> Option<&StateTimeout> {
        let normalized = self.normalized_state(state);
        self.state_timeouts
            .iter()
            .find(|timeout| self.normalized_state(&timeout.state) == normalized)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct TimeoutClock {
    active_state: Option<String>,
    active_started_at: Option<Timestamp>,
    remaining_ms: HashMap<String, u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum TimeoutExitMode {
    #[default]
    Pause,
    Reset,
    Continue,
}

impl TimeoutClock {
    pub fn on_state_enter(&mut self, state: &str, timeout_ms: u64, now: Timestamp) {
        let key = normalize_token(state);
        self.remaining_ms.entry(key.clone()).or_insert(timeout_ms);
        self.active_state = Some(key);
        self.active_started_at = Some(now);
    }

    pub fn on_state_exit(
        &mut self,
        state: &str,
        timeout_ms: u64,
        mode: TimeoutExitMode,
        now: Timestamp,
    ) {
        let key = normalize_token(state);
        if self.active_state.as_deref() != Some(key.as_str()) {
            return;
        }

        let elapsed = self.active_started_at.map_or(0, |start| elapsed_ms(start, now));
        let current_remaining = self.remaining_ms.get(&key).copied().unwrap_or(timeout_ms);
        let updated = current_remaining.saturating_sub(elapsed);
        let next_remaining = match mode {
            TimeoutExitMode::Pause => updated,
            TimeoutExitMode::Reset => timeout_ms,
            TimeoutExitMode::Continue => current_remaining,
        };
        self.remaining_ms.insert(key, next_remaining);
        self.active_state = None;
        self.active_started_at = None;
    }

    #[must_use]
    pub fn is_timeout_due(&self, state: &str, timeout_ms: u64, now: Timestamp) -> bool {
        let key = normalize_token(state);
        if self.active_state.as_deref() != Some(key.as_str()) {
            return false;
        }
        let remaining = self.remaining_ms.get(&key).copied().unwrap_or(timeout_ms);
        self.active_started_at
            .is_some_and(|started| elapsed_ms(started, now) >= remaining)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkflowInstance {
    pub id: String,
    pub workflow_name: String,
    pub current_state: String,
    pub last_active_state: Option<String>,
    pub total_retry_count: u32,
    pub session_retry_count: u32,
    pub state_entered_at: Timestamp,
    pub timeout_clock: TimeoutClock,
    pub last_user_event_at: Option<Timestamp>,
    pub data: Value,
    pub partial_rollback: bool,
    pub has_pending_retry: bool,
}

impl WorkflowInstance {
    #[must_use]
    pub fn new(id: String, workflow_name: String, initial_state: String, data: Value) -> Self {
        let now = Timestamp::now();
        Self {
            id,
            workflow_name,
            current_state: normalize_token(&initial_state),
            last_active_state: None,
            total_retry_count: 0,
            session_retry_count: 0,
            state_entered_at: now,
            timeout_clock: TimeoutClock::default(),
            last_user_event_at: None,
            data,
            partial_rollback: false,
            has_pending_retry: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HandleEventResult {
    pub instance_id: String,
    pub from_state: String,
    pub to_state: String,
    pub transitioned: bool,
    pub reason: TransitionReason,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransitionReason {
    Event,
    GuardRejected,
    ActionFailed,
    Timeout,
    RetryExceeded,
}

#[async_trait]
pub trait Guard: Send + Sync {
    fn name(&self) -> &str;
    fn evaluate(&self, instance: &WorkflowInstance, event: &Event) -> bool;
}

#[derive(Debug, Default)]
pub struct HasPermissionGuard;

#[async_trait]
impl Guard for HasPermissionGuard {
    fn name(&self) -> &str {
        "has_permission"
    }

    fn evaluate(&self, _instance: &WorkflowInstance, event: &Event) -> bool {
        event
            .payload
            .get("has_permission")
            .and_then(Value::as_bool)
            .unwrap_or(false)
    }
}

#[derive(Debug, Clone)]
pub struct MaxRetryGuard {
    pub max_retry_count: u32,
}

#[async_trait]
impl Guard for MaxRetryGuard {
    fn name(&self) -> &str {
        "max_retry"
    }

    fn evaluate(&self, instance: &WorkflowInstance, _event: &Event) -> bool {
        instance.total_retry_count < self.max_retry_count
    }
}

#[async_trait]
pub trait ActionRunner: Send + Sync {
    async fn run_pipeline(
        &self,
        _pipeline_id: &str,
        _instance: &WorkflowInstance,
        _event: &Event,
    ) -> AmanResult<()> {
        Ok(())
    }

    async fn run_skill(
        &self,
        _skill_name: &str,
        _instance: &WorkflowInstance,
        _event: &Event,
    ) -> AmanResult<()> {
        Ok(())
    }

    fn inflight_pipeline_count(&self, _instance_id: &str) -> usize {
        0
    }
}

pub struct NoopActionRunner;

#[async_trait]
impl ActionRunner for NoopActionRunner {}

pub trait WorkflowStateStore: Send + Sync {
    fn load(&self, instance_id: &str) -> AmanResult<Option<WorkflowInstance>>;
    fn save(&self, instance: &WorkflowInstance) -> AmanResult<()>;
    /// Remove an instance from the store. Default no-op; in-memory stores
    /// should override this.
    fn delete(&self, _instance_id: &str) -> AmanResult<()> {
        Ok(())
    }
}

#[derive(Default)]
pub struct InMemoryStateStore {
    instances: Mutex<HashMap<String, WorkflowInstance>>,
}

impl WorkflowStateStore for InMemoryStateStore {
    fn load(&self, instance_id: &str) -> AmanResult<Option<WorkflowInstance>> {
        Ok(self
            .instances
            .lock()
            .expect("workflow state store lock")
            .get(instance_id)
            .cloned())
    }

    fn save(&self, instance: &WorkflowInstance) -> AmanResult<()> {
        self.instances
            .lock()
            .expect("workflow state store lock")
            .insert(instance.id.clone(), instance.clone());
        Ok(())
    }

    fn delete(&self, instance_id: &str) -> AmanResult<()> {
        self.instances
            .lock()
            .expect("workflow state store lock")
            .remove(instance_id);
        Ok(())
    }
}

#[derive(Default)]
pub struct TimeoutManager;

impl TimeoutManager {
    pub fn on_state_enter(
        &self,
        workflow: &WorkflowDef,
        instance: &mut WorkflowInstance,
        now: Timestamp,
    ) {
        if let Some(timeout) = workflow.timeout_for(&instance.current_state) {
            instance
                .timeout_clock
                .on_state_enter(&instance.current_state, timeout.timeout_ms, now);
        }
    }

    pub fn on_state_exit(
        &self,
        workflow: &WorkflowDef,
        instance: &mut WorkflowInstance,
        now: Timestamp,
        mode: TimeoutExitMode,
    ) {
        if let Some(timeout) = workflow.timeout_for(&instance.current_state) {
            instance.timeout_clock.on_state_exit(
                &instance.current_state,
                timeout.timeout_ms,
                mode,
                now,
            );
        }
    }

    #[must_use]
    pub fn is_timeout_due(
        &self,
        workflow: &WorkflowDef,
        instance: &WorkflowInstance,
        now: Timestamp,
    ) -> bool {
        workflow.timeout_for(&instance.current_state).is_some_and(|timeout| {
            instance
                .timeout_clock
                .is_timeout_due(&instance.current_state, timeout.timeout_ms, now)
        })
    }

}

pub struct WorkflowEngine {
    workflows: Mutex<HashMap<String, WorkflowDef>>,
    instances: Mutex<HashMap<String, WorkflowInstance>>,
    guards: Mutex<HashMap<String, Arc<dyn Guard>>>,
    store: Arc<dyn WorkflowStateStore>,
    action_runner: Arc<dyn ActionRunner>,
    timeout_manager: TimeoutManager,
    state_change_events: Mutex<Vec<Event>>,
    error_alerts: Mutex<Vec<String>>,
    runtime_config: WorkflowRuntimeConfig,
}

#[derive(Debug, Clone, Copy)]
pub struct WorkflowRuntimeConfig {
    pub timeout_defer_ms: u64,
    pub retry_cancel_conflict_defer_ms: u64,
    pub terminal_archive_after_ms: u64,
    pub archived_retention_ms: u64,
    pub cancel_wait_timeout_ms: u64,
}

impl Default for WorkflowRuntimeConfig {
    fn default() -> Self {
        Self {
            timeout_defer_ms: 5_000,
            retry_cancel_conflict_defer_ms: 5_000,
            terminal_archive_after_ms: 30 * 24 * 60 * 60 * 1_000,
            archived_retention_ms: 30 * 24 * 60 * 60 * 1_000,
            cancel_wait_timeout_ms: 1_000,
        }
    }
}

impl WorkflowEngine {
    #[must_use]
    pub fn new() -> Self {
        let mut guard_map: HashMap<String, Arc<dyn Guard>> = HashMap::new();
        let has_permission: Arc<dyn Guard> = Arc::new(HasPermissionGuard);
        let max_retry: Arc<dyn Guard> = Arc::new(MaxRetryGuard { max_retry_count: 3 });
        guard_map.insert("has_permission".to_owned(), has_permission);
        guard_map.insert("max_retry".to_owned(), max_retry);
        Self {
            workflows: Mutex::new(HashMap::new()),
            instances: Mutex::new(HashMap::new()),
            guards: Mutex::new(guard_map),
            store: Arc::new(InMemoryStateStore::default()),
            action_runner: Arc::new(NoopActionRunner),
            timeout_manager: TimeoutManager,
            state_change_events: Mutex::new(Vec::new()),
            error_alerts: Mutex::new(Vec::new()),
            runtime_config: WorkflowRuntimeConfig::default(),
        }
    }

    #[must_use]
    pub fn with_store(mut self, store: Arc<dyn WorkflowStateStore>) -> Self {
        self.store = store;
        self
    }

    #[must_use]
    pub fn with_action_runner(mut self, action_runner: Arc<dyn ActionRunner>) -> Self {
        self.action_runner = action_runner;
        self
    }

    #[must_use]
    pub fn with_runtime_config(mut self, runtime_config: WorkflowRuntimeConfig) -> Self {
        self.runtime_config = runtime_config;
        self
    }

    pub fn register_workflow(&self, workflow: WorkflowDef) -> AmanResult<()> {
        workflow.validate()?;
        let name = workflow.name.clone();
        let mut workflows = self.workflows.lock().expect("workflows lock");
        if workflows.contains_key(&name) {
            return Err(Error::AlreadyExists {
                name: format!("workflow:{name}"),
            });
        }
        workflows.insert(name, workflow);
        Ok(())
    }

    #[must_use]
    pub fn list_workflows(&self) -> Vec<String> {
        let mut names = self
            .workflows
            .lock()
            .expect("workflows lock")
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        names.sort();
        names
    }

    #[must_use]
    pub fn get_workflow(&self, name: &str) -> Option<WorkflowDef> {
        self.workflows
            .lock()
            .expect("workflows lock")
            .get(name)
            .cloned()
    }

    #[must_use]
    pub fn list_instances(&self) -> Vec<WorkflowInstance> {
        let mut items = self
            .instances
            .lock()
            .expect("instances lock")
            .values()
            .cloned()
            .collect::<Vec<_>>();
        items.sort_by(|a, b| a.id.cmp(&b.id));
        items
    }

    pub fn register_guard(&self, guard: Arc<dyn Guard>) {
        self.guards
            .lock()
            .expect("guards lock")
            .insert(guard.name().to_owned(), guard);
    }

    pub fn create_instance(&self, workflow_name: &str, data: Value) -> AmanResult<WorkflowInstance> {
        let workflow = self.workflow(workflow_name)?;
        let id = xid::new().to_string();
        let mut instance = WorkflowInstance::new(
            id,
            workflow.name.clone(),
            workflow.initial_state.clone(),
            data,
        );
        self.timeout_manager
            .on_state_enter(&workflow, &mut instance, Timestamp::now());
        self.store.save(&instance)?;
        self.instances
            .lock()
            .expect("instances lock")
            .insert(instance.id.clone(), instance.clone());
        Ok(instance)
    }

    /// Restore a previously-persisted workflow instance by its existing ID.
    ///
    /// Unlike `create_instance`, this does not generate a new ID — it uses
    /// the provided `id` directly. Used when resuming a chat session that
    /// was persisted across gateway restarts.
    pub fn restore_instance(&self, id: &str, workflow_name: &str, data: Value) -> AmanResult<WorkflowInstance> {
        let workflow = self.workflow(workflow_name)?;
        let mut instance = WorkflowInstance::new(
            id.to_owned(),
            workflow.name.clone(),
            workflow.initial_state.clone(),
            data,
        );
        self.timeout_manager
            .on_state_enter(&workflow, &mut instance, Timestamp::now());
        self.store.save(&instance)?;
        self.instances
            .lock()
            .expect("instances lock")
            .insert(instance.id.clone(), instance.clone());
        Ok(instance)
    }

    #[must_use]
    pub fn get_instance(&self, instance_id: &str) -> Option<WorkflowInstance> {
        self.instances
            .lock()
            .expect("instances lock")
            .get(instance_id)
            .cloned()
    }

    /// Update the `data` field of a workflow instance in-place.
    ///
    /// The `updater` closure receives a mutable reference to the instance's
    /// `data` (`serde_json::Value`), allowing callers to add or modify fields.
    /// Returns `Ok(())` if the instance exists, `Err` otherwise.
    pub fn update_instance_data(
        &self,
        instance_id: &str,
        updater: impl FnOnce(&mut serde_json::Value),
    ) -> AmanResult<()> {
        let mut instances = self.instances.lock().expect("instances lock");
        let instance = instances.get_mut(instance_id).ok_or_else(|| {
            Error::NotFound {
                name: format!("workflow_instance:{}", instance_id),
            }
        })?;
        updater(&mut instance.data);
        self.store.save(instance)?;
        Ok(())
    }

    /// Remove a workflow instance from both the in-memory map and the
    /// persistent store. Returns `Ok(true)` if the instance existed and was
    /// removed, `Ok(false)` if it did not exist.
    pub fn delete_instance(&self, instance_id: &str) -> AmanResult<bool> {
        let existed = self
            .instances
            .lock()
            .expect("instances lock")
            .remove(instance_id)
            .is_some();
        if existed {
            // Best-effort removal from the persistent store.
            let _ = self.store.delete(instance_id);
        }
        Ok(existed)
    }

    #[must_use]
    pub fn state_change_events(&self) -> Vec<Event> {
        self.state_change_events
            .lock()
            .expect("state change events lock")
            .clone()
    }

    #[must_use]
    pub fn error_alerts(&self) -> Vec<String> {
        self.error_alerts
            .lock()
            .expect("error alerts lock")
            .clone()
    }

    pub async fn handle_event(
        &self,
        instance_id: &str,
        event: Event,
    ) -> AmanResult<HandleEventResult> {
        let mut instance = self.load_instance(instance_id)?;
        let workflow = self.workflow(&instance.workflow_name)?;
        let from_state = instance.current_state.clone();
        instance.last_user_event_at = Some(event.timestamp);

        if workflow.is_error_state(&from_state) && normalize_token(event.event_type.as_str()) == "RETRY" {
            return self.retry_from_error(&workflow, instance, event).await;
        }
        if workflow.is_error_state(&from_state) && normalize_token(event.event_type.as_str()) == "CANCEL" {
            return self.cancel_from_error(&workflow, instance).await;
        }

        let transition = workflow
            .find_transition(&instance.current_state, event.event_type.as_str())
            .ok_or_else(|| Error::InvalidStateTransition {
                message: format!(
                    "workflow `{}` has no transition from `{}` on event `{}`",
                    workflow.name,
                    instance.current_state,
                    event.event_type
                ),
            })?
            .clone();

        if let Some(guard_name) = &transition.guard {
            let guard_ok = self
                .guards
                .lock()
                .expect("guards lock")
                .get(guard_name)
                .is_some_and(|guard| guard.evaluate(&instance, &event));
            if !guard_ok {
                if let Some(on_fail) = &transition.on_fail {
                    let target = resolve_transition_target(on_fail, &instance)?;
                    return self
                        .apply_transition(
                            workflow,
                            instance,
                            from_state,
                            target,
                            TransitionReason::GuardRejected,
                        )
                        .await;
                }
                return Ok(HandleEventResult {
                    instance_id: instance.id.clone(),
                    from_state: from_state.clone(),
                    to_state: from_state,
                    transitioned: false,
                    reason: TransitionReason::GuardRejected,
                });
            }
        }

        if let Some(action) = &transition.action {
            if matches!(action, TransitionAction::Pipeline(_))
                && instance
                    .data
                    .get("retry_pipeline_idempotency_required")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
                && event
                    .payload
                    .get("idempotency_key")
                    .and_then(Value::as_str)
                    .is_none()
            {
                return Err(Error::InvalidStateTransition {
                    message: format!(
                        "workflow `{}` requires `idempotency_key` before retrying pipeline action",
                        workflow.name
                    ),
                });
            }
            let action_result = match action {
                TransitionAction::Pipeline(pipeline_id) => {
                    self.action_runner
                        .run_pipeline(pipeline_id, &instance, &event)
                        .await
                }
                TransitionAction::Skill(skill_name) => {
                    self.action_runner.run_skill(skill_name, &instance, &event).await
                }
            };
            if let Err(action_error) = action_result {
                instance.partial_rollback = matches!(action, TransitionAction::Pipeline(_));
                let fallback = transition
                    .on_action_failure
                    .as_ref()
                    .map_or_else(|| TransitionTo::Specific(workflow.error_state.clone()), Clone::clone);
                let target = resolve_transition_target(&fallback, &instance)?;
                let mut result = self
                    .apply_transition(
                            workflow.clone(),
                        instance,
                        from_state,
                        target,
                        TransitionReason::ActionFailed,
                    )
                    .await?;
                let entered_error = result.transitioned && workflow.is_error_state(&result.to_state);
                if entered_error && workflow.error_recovery.auto_retry_count > 0 {
                    let auto_result = self
                        .run_auto_retry_if_enabled(&workflow, &result.instance_id)
                        .await?;
                    if auto_result.transitioned {
                        result = auto_result;
                    }
                }
                if result.transitioned {
                    result.reason = TransitionReason::ActionFailed;
                }
                if !entered_error && !result.transitioned {
                    return Err(action_error);
                }
                return Ok(result);
            }
            if matches!(action, TransitionAction::Pipeline(_)) {
                let idempotency_key = event
                    .payload
                    .get("idempotency_key")
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned);
                if let Some(object) = instance.data.as_object_mut() {
                    object.remove("retry_pipeline_idempotency_required");
                    if let Some(key) = idempotency_key {
                        object.insert("last_idempotency_key".to_owned(), Value::String(key));
                    }
                }
            }
        }

        let target = resolve_transition_target(&transition.to, &instance)?;
        self.apply_transition(workflow, instance, from_state, target, TransitionReason::Event)
            .await
    }

    pub async fn handle_timeouts(&self, now: Timestamp) -> AmanResult<Vec<HandleEventResult>> {
        let ids = self
            .instances
            .lock()
            .expect("instances lock")
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        let mut results = Vec::new();

        for id in ids {
            let instance = self.load_instance(&id)?;
            let workflow = self.workflow(&instance.workflow_name)?;
            if !self.timeout_manager.is_timeout_due(&workflow, &instance, now) {
                continue;
            }
            if let Some(timeout) = workflow.timeout_for(&instance.current_state) {
                if let Some(last_user_event_at) = instance.last_user_event_at {
                    let elapsed = elapsed_ms(last_user_event_at, now);
                    if elapsed <= self.runtime_config.timeout_defer_ms {
                        continue;
                    }
                }
                let from_state = instance.current_state.clone();
                let target = resolve_transition_target(&timeout.on_timeout, &instance)?;
                let result = self
                    .apply_transition(workflow, instance, from_state, target, TransitionReason::Timeout)
                    .await?;
                results.push(result);
            }
        }

        Ok(results)
    }

    pub async fn run_terminal_recovery(&self, now: Timestamp) -> AmanResult<Vec<HandleEventResult>> {
        let ids = self
            .instances
            .lock()
            .expect("instances lock")
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        let mut results = Vec::new();
        for id in ids {
            let instance = self.load_instance(&id)?;
            let workflow = self.workflow(&instance.workflow_name)?;
            let state = normalize_token(&instance.current_state);
            if state == "ARCHIVED" {
                continue;
            }
            if !matches!(state.as_str(), "APPROVED" | "REJECTED" | "CANCELLED") {
                continue;
            }
            if elapsed_ms(instance.state_entered_at, now) < self.runtime_config.terminal_archive_after_ms {
                continue;
            }
            if !workflow.states.iter().any(|state| normalize_token(&state.name) == "ARCHIVED") {
                continue;
            }
            let result = self
                .apply_transition(
                    workflow,
                    instance.clone(),
                    instance.current_state.clone(),
                    "ARCHIVED".to_owned(),
                    TransitionReason::Timeout,
                )
                .await?;
            results.push(result);
        }
        Ok(results)
    }

    pub fn cleanup_archived_instances(&self, now: Timestamp) -> usize {
        let mut instances = self.instances.lock().expect("instances lock");
        let before = instances.len();
        instances.retain(|_, instance| {
            !(normalize_token(&instance.current_state) == "ARCHIVED"
                && elapsed_ms(instance.state_entered_at, now) >= self.runtime_config.archived_retention_ms)
        });
        before.saturating_sub(instances.len())
    }

    fn workflow(&self, workflow_name: &str) -> AmanResult<WorkflowDef> {
        self.workflows
            .lock()
            .expect("workflows lock")
            .get(workflow_name)
            .cloned()
            .ok_or_else(|| Error::NotFound {
                name: format!("workflow:{workflow_name}"),
            })
    }

    fn load_instance(&self, instance_id: &str) -> AmanResult<WorkflowInstance> {
        if let Some(instance) = self
            .instances
            .lock()
            .expect("instances lock")
            .get(instance_id)
            .cloned()
        {
            return Ok(instance);
        }
        let instance = self
            .store
            .load(instance_id)?
            .ok_or_else(|| Error::NotFound {
                name: format!("workflow_instance:{instance_id}"),
            })?;
        self.instances
            .lock()
            .expect("instances lock")
            .insert(instance.id.clone(), instance.clone());
        Ok(instance)
    }

    async fn retry_from_error(
        &self,
        workflow: &WorkflowDef,
        mut instance: WorkflowInstance,
        _event: Event,
    ) -> AmanResult<HandleEventResult> {
        let from_state = instance.current_state.clone();
        if instance.total_retry_count >= workflow.error_recovery.max_retry_count {
            let target = match workflow.error_recovery.on_retry_failure {
                RetryFailurePolicy::Archive => "ARCHIVED".to_owned(),
                RetryFailurePolicy::ManualOnly => workflow.error_state.clone(),
            };
            return self
                .apply_transition(
                    workflow.clone(),
                    instance,
                    from_state,
                    target,
                    TransitionReason::RetryExceeded,
                )
                .await;
        }

        let last_active = instance
            .last_active_state
            .clone()
            .ok_or_else(|| Error::InvalidStateTransition {
                message: format!(
                    "workflow `{}` cannot retry because last_active_state is missing",
                    workflow.name
                ),
            })?;
        instance.total_retry_count = instance.total_retry_count.saturating_add(1);
        instance.session_retry_count = instance.session_retry_count.saturating_add(1);
        instance.has_pending_retry = false;
        if instance.partial_rollback
            && let Some(object) = instance.data.as_object_mut() {
                object.insert(
                    "retry_pipeline_idempotency_required".to_owned(),
                    Value::Bool(true),
                );
            }
        self.apply_transition(
            workflow.clone(),
            instance,
            from_state,
            normalize_token(&last_active),
            TransitionReason::Event,
        )
        .await
    }

    async fn cancel_from_error(
        &self,
        workflow: &WorkflowDef,
        mut instance: WorkflowInstance,
    ) -> AmanResult<HandleEventResult> {
        let from_state = instance.current_state.clone();
        if !instance.has_pending_retry {
            return Ok(HandleEventResult {
                instance_id: instance.id.clone(),
                from_state: from_state.clone(),
                to_state: from_state,
                transitioned: false,
                reason: TransitionReason::GuardRejected,
            });
        }
        if elapsed_ms(instance.state_entered_at, Timestamp::now())
            < self.runtime_config.retry_cancel_conflict_defer_ms
        {
            return Ok(HandleEventResult {
                instance_id: instance.id.clone(),
                from_state: from_state.clone(),
                to_state: from_state,
                transitioned: false,
                reason: TransitionReason::GuardRejected,
            });
        }
        self.wait_inflight_pipeline_drained(&instance.id);
        instance.has_pending_retry = false;
        let target = if workflow
            .states
            .iter()
            .any(|state| normalize_token(&state.name) == "CANCELLED")
        {
            "CANCELLED".to_owned()
        } else {
            workflow.error_state.clone()
        };
        self.apply_transition(
            workflow.clone(),
            instance,
            from_state,
            target,
            TransitionReason::Event,
        )
        .await
    }

    async fn run_auto_retry_if_enabled(
        &self,
        workflow: &WorkflowDef,
        instance_id: &str,
    ) -> AmanResult<HandleEventResult> {
        let mut last = HandleEventResult {
            instance_id: instance_id.to_owned(),
            from_state: workflow.normalized_state(&workflow.error_state),
            to_state: workflow.normalized_state(&workflow.error_state),
            transitioned: false,
            reason: TransitionReason::ActionFailed,
        };
        for attempt in 1..=workflow.error_recovery.auto_retry_count {
            let instance = self.load_instance(instance_id)?;
            if !workflow.is_error_state(&instance.current_state) {
                break;
            }
            wait_backoff(&workflow.error_recovery.retry_backoff, attempt);
            let retry_event = Event::new(
                "workflow:auto_retry",
                EventType::Custom("retry".to_owned()),
                json!({
                    "auto_retry": true,
                    "attempt": attempt,
                }),
            );
            let result = self.retry_from_error(workflow, instance, retry_event).await?;
            let left_error = !workflow.is_error_state(&result.to_state);
            last = result;
            if left_error {
                break;
            }
        }
        Ok(last)
    }

    async fn apply_transition(
        &self,
        workflow: WorkflowDef,
        mut instance: WorkflowInstance,
        from_state: String,
        target_state: String,
        reason: TransitionReason,
    ) -> AmanResult<HandleEventResult> {
        let now = Timestamp::now();
        self.timeout_manager.on_state_exit(
            &workflow,
            &mut instance,
            now,
            TimeoutExitMode::Pause,
        );

        let normalized_target = normalize_token(&target_state);
        if normalized_target != normalize_token(&from_state) {
            if workflow.is_error_state(&normalized_target) && !workflow.is_error_state(&from_state) {
                instance.last_active_state = Some(normalize_token(&from_state));
                instance.session_retry_count = 0;
                instance.has_pending_retry = true;
                self.error_alerts
                    .lock()
                    .expect("error alerts lock")
                    .push(format!(
                        "WORKFLOW_ENTER_ERROR workflow={} instance={} from={}",
                        workflow.name,
                        instance.id,
                        normalize_token(&from_state)
                    ));
            }
            if !workflow.is_error_state(&normalized_target) {
                instance.partial_rollback = false;
            }
            instance.current_state = normalized_target.clone();
            instance.state_entered_at = now;
            self.timeout_manager.on_state_enter(&workflow, &mut instance, now);
            self.record_state_change_event(
                &instance,
                &from_state,
                &normalized_target,
                reason,
                workflow.is_final_state(&normalized_target),
            );
        }
        self.store.save(&instance)?;
        self.instances
            .lock()
            .expect("instances lock")
            .insert(instance.id.clone(), instance.clone());
        let transitioned = normalize_token(&from_state) != normalized_target;
        Ok(HandleEventResult {
            instance_id: instance.id,
            from_state,
            to_state: normalized_target.clone(),
            transitioned,
            reason,
        })
    }

    fn record_state_change_event(
        &self,
        instance: &WorkflowInstance,
        from_state: &str,
        to_state: &str,
        reason: TransitionReason,
        is_final: bool,
    ) {
        let event = Event::new(
            "workflow:engine",
            EventType::WorkflowStateChanged,
            json!({
                "instance_id": instance.id,
                "workflow_name": instance.workflow_name,
                "from_state": normalize_token(from_state),
                "to_state": normalize_token(to_state),
                "reason": format!("{reason:?}").to_lowercase(),
                "is_final": is_final,
            }),
        );
        self.state_change_events
            .lock()
            .expect("state change events lock")
            .push(event);
    }

    fn wait_inflight_pipeline_drained(&self, instance_id: &str) {
        let started = Timestamp::now();
        loop {
            if self.action_runner.inflight_pipeline_count(instance_id) == 0 {
                return;
            }
            if elapsed_ms(started, Timestamp::now()) >= self.runtime_config.cancel_wait_timeout_ms {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }
}

impl Default for WorkflowEngine {
    fn default() -> Self {
        Self::new()
    }
}

fn resolve_transition_target(
    target: &TransitionTo,
    instance: &WorkflowInstance,
) -> AmanResult<String> {
    match target {
        TransitionTo::Specific(state) => Ok(normalize_token(state)),
        TransitionTo::LastActiveState => instance
            .last_active_state
            .clone()
            .map(|state| normalize_token(&state))
            .ok_or_else(|| Error::InvalidStateTransition {
                message: format!(
                    "workflow instance `{}` has no last_active_state for transition",
                    instance.id
                ),
            }),
    }
}

fn elapsed_ms(started_at: Timestamp, now: Timestamp) -> u64 {
    let start = started_at.as_millis();
    let end = now.as_millis();
    if end <= start {
        0
    } else {
        (end - start) as u64
    }
}

#[must_use]
pub fn normalize_token(value: &str) -> String {
    value.trim().to_ascii_uppercase()
}

fn wait_backoff(backoff: &RetryBackoff, attempt: u32) {
    let delay_ms = match backoff {
        RetryBackoff::Immediate => 0,
        RetryBackoff::Fixed(ms) => *ms,
        RetryBackoff::Exponential => 100_u64.saturating_mul(2_u64.saturating_pow(attempt - 1)),
        RetryBackoff::Sequence(steps) => {
            let index = usize::try_from(attempt.saturating_sub(1)).unwrap_or(usize::MAX);
            *steps.get(index).or_else(|| steps.last()).unwrap_or(&0)
        }
    };
    if delay_ms > 0 {
        std::thread::sleep(std::time::Duration::from_millis(delay_ms.min(5)));
    }
}

#[cfg(test)]
mod tests {
    use super::{
        normalize_token, ActionRunner, HandleEventResult, RetryFailurePolicy, StateDef,
        StateTimeout, Transition, TransitionAction, TransitionFrom, TransitionReason, TransitionTo,
        WorkflowDef, WorkflowEngine, WorkflowRuntimeConfig,
    };
    use async_trait::async_trait;
    use kernel::context::ToolContext;
    use kernel::event::{Event, EventType};
    use kernel::pipeline::{PipelineStep, StepType};
    use kernel::retry::RetryBackoff;
    use kernel::retry::RetryPolicy;
    use kernel::schema::JsonSchema;
    use kernel::tool::Tool;
    use kernel::types::{ConcurrencyModel, ToolMode};
    use kernel::{AmanResult, Error};
    use pipeline::{PipelineDefinition, PipelineEngine};
    use serde_json::json;
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use uuid::Uuid;

    fn event(name: &str, payload: serde_json::Value) -> Event {
        Event::new(
            "workflow:test",
            EventType::Custom(name.to_owned()),
            payload,
        )
    }

    fn base_workflow() -> WorkflowDef {
        WorkflowDef {
            name: "approval".to_owned(),
            states: vec![
                StateDef {
                    name: "pending".to_owned(),
                },
                StateDef {
                    name: "reviewing".to_owned(),
                },
                StateDef {
                    name: "approved".to_owned(),
                },
                StateDef {
                    name: "rejected".to_owned(),
                },
                StateDef {
                    name: "error".to_owned(),
                },
                StateDef {
                    name: "archived".to_owned(),
                },
                StateDef {
                    name: "cancelled".to_owned(),
                },
            ],
            initial_state: "pending".to_owned(),
            final_states: vec![
                "approved".to_owned(),
                "rejected".to_owned(),
                "archived".to_owned(),
                "cancelled".to_owned(),
            ],
            error_state: "error".to_owned(),
            transitions: vec![
                Transition {
                    from: TransitionFrom::Specific("pending".to_owned()),
                    event: "submit".to_owned(),
                    to: TransitionTo::Specific("reviewing".to_owned()),
                    guard: None,
                    on_fail: None,
                    action: None,
                    on_action_failure: None,
                },
                Transition {
                    from: TransitionFrom::Specific("reviewing".to_owned()),
                    event: "approve".to_owned(),
                    to: TransitionTo::Specific("approved".to_owned()),
                    guard: None,
                    on_fail: None,
                    action: None,
                    on_action_failure: None,
                },
            ],
            state_timeouts: Vec::new(),
            error_recovery: super::ErrorRecovery {
                auto_retry_count: 0,
                max_retry_count: 2,
                retry_backoff: RetryBackoff::Immediate,
                on_retry_failure: RetryFailurePolicy::Archive,
            },
        }
    }

    #[test]
    fn state_name_is_normalized_case_insensitive() {
        let workflow = base_workflow();
        assert_eq!(workflow.normalized_state("pending"), "PENDING");
        assert_eq!(workflow.normalized_state("PeNdInG"), "PENDING");
        assert_eq!(normalize_token("  reviewing "), "REVIEWING");
    }

    #[test]
    fn workflow_supports_basic_state_transition_flow() {
        pollster::block_on(async {
            let engine = WorkflowEngine::new();
            engine
                .register_workflow(base_workflow())
                .expect("register workflow");
            let instance = engine
                .create_instance("approval", json!({"ticket": "A-1"}))
                .expect("create instance");

            let submit = engine
                .handle_event(&instance.id, event("submit", json!({})))
                .await
                .expect("submit transition");
            assert_eq!(submit.from_state, "PENDING");
            assert_eq!(submit.to_state, "REVIEWING");

            let approve = engine
                .handle_event(&instance.id, event("approve", json!({})))
                .await
                .expect("approve transition");
            assert_eq!(approve.from_state, "REVIEWING");
            assert_eq!(approve.to_state, "APPROVED");
        });
    }

    #[test]
    fn guard_failure_uses_on_fail_transition() {
        pollster::block_on(async {
            let mut workflow = base_workflow();
            workflow.transitions = vec![Transition {
                from: TransitionFrom::Specific("pending".to_owned()),
                event: "submit".to_owned(),
                to: TransitionTo::Specific("reviewing".to_owned()),
                guard: Some("has_permission".to_owned()),
                on_fail: Some(TransitionTo::Specific("rejected".to_owned())),
                action: None,
                on_action_failure: None,
            }];
            let engine = WorkflowEngine::new();
            engine
                .register_workflow(workflow)
                .expect("register workflow");
            let instance = engine
                .create_instance("approval", json!({}))
                .expect("create instance");

            let result = engine
                .handle_event(
                    &instance.id,
                    event("submit", json!({"has_permission": false})),
                )
                .await
                .expect("guard should use on_fail");
            assert_eq!(result.reason, TransitionReason::GuardRejected);
            assert_eq!(result.to_state, "REJECTED");
        });
    }

    #[test]
    fn guard_failure_without_on_fail_stays_in_original_state() {
        pollster::block_on(async {
            let mut workflow = base_workflow();
            workflow.transitions = vec![Transition {
                from: TransitionFrom::Specific("pending".to_owned()),
                event: "submit".to_owned(),
                to: TransitionTo::Specific("reviewing".to_owned()),
                guard: Some("has_permission".to_owned()),
                on_fail: None,
                action: None,
                on_action_failure: None,
            }];
            let engine = WorkflowEngine::new();
            engine
                .register_workflow(workflow)
                .expect("register workflow");
            let instance = engine
                .create_instance("approval", json!({}))
                .expect("create instance");

            let result = engine
                .handle_event(
                    &instance.id,
                    event("submit", json!({"has_permission": false})),
                )
                .await
                .expect("guard should reject transition");
            assert!(!result.transitioned);
            assert_eq!(result.to_state, "PENDING");
            assert_eq!(
                engine
                    .get_instance(&instance.id)
                    .expect("instance should exist")
                    .current_state,
                "PENDING"
            );
        });
    }

    #[derive(Default)]
    struct RecordingRunner {
        fail_pipeline: Mutex<bool>,
    }

    #[async_trait]
    impl ActionRunner for RecordingRunner {
        async fn run_pipeline(
            &self,
            _pipeline_id: &str,
            _instance: &super::WorkflowInstance,
            _event: &Event,
        ) -> AmanResult<()> {
            if *self.fail_pipeline.lock().expect("runner lock") {
                return Err(Error::Unrecoverable {
                    message: "pipeline failed".to_owned(),
                });
            }
            Ok(())
        }
    }

    #[test]
    fn action_failure_enters_error_then_retry_recovers() {
        pollster::block_on(async {
            let mut workflow = base_workflow();
            workflow.transitions = vec![Transition {
                from: TransitionFrom::Specific("reviewing".to_owned()),
                event: "approve".to_owned(),
                to: TransitionTo::Specific("approved".to_owned()),
                guard: None,
                on_fail: None,
                action: Some(TransitionAction::Pipeline("review-pipe".to_owned())),
                on_action_failure: None,
            }];

            let runner = Arc::new(RecordingRunner {
                fail_pipeline: Mutex::new(true),
            });
            let engine = WorkflowEngine::new().with_action_runner(runner.clone());
            engine
                .register_workflow(workflow)
                .expect("register workflow");

            let mut instance = engine
                .create_instance("approval", json!({}))
                .expect("create instance");
            instance.current_state = "REVIEWING".to_owned();
            engine
                .instances
                .lock()
                .expect("instance lock")
                .insert(instance.id.clone(), instance.clone());
            engine
                .handle_event(&instance.id, event("approve", json!({})))
                .await
                .expect("action failure should move to error");
            let errored = engine.get_instance(&instance.id).expect("instance should exist");
            assert_eq!(errored.current_state, "ERROR");
            assert_eq!(errored.last_active_state.as_deref(), Some("REVIEWING"));

            *runner.fail_pipeline.lock().expect("runner lock") = false;
            let retry = engine
                .handle_event(&instance.id, event("retry", json!({})))
                .await
                .expect("retry should recover");
            assert_eq!(retry.to_state, "REVIEWING");
            let recovered = engine.get_instance(&instance.id).expect("instance should exist");
            assert_eq!(recovered.total_retry_count, 1);
        });
    }

    #[test]
    fn timeout_can_auto_transition_state() {
        pollster::block_on(async {
            let mut workflow = base_workflow();
            workflow.initial_state = "reviewing".to_owned();
            workflow.state_timeouts = vec![StateTimeout {
                state: "reviewing".to_owned(),
                timeout_ms: 1,
                on_timeout: TransitionTo::Specific("rejected".to_owned()),
                on_timeout_alert: None,
            }];
            let engine = WorkflowEngine::new().with_runtime_config(WorkflowRuntimeConfig {
                timeout_defer_ms: 0,
                ..WorkflowRuntimeConfig::default()
            });
            engine
                .register_workflow(workflow)
                .expect("register workflow");
            let instance = engine
                .create_instance("approval", json!({}))
                .expect("create instance");

            let now = kernel::types::Timestamp::from_millis(
                kernel::types::Timestamp::now().as_millis() + 10,
            );
            let timeout_results = engine
                .handle_timeouts(now)
                .await
                .expect("handle timeouts");
            assert_eq!(timeout_results.len(), 1);
            assert_eq!(timeout_results[0].reason, TransitionReason::Timeout);
            assert_eq!(timeout_results[0].to_state, "REJECTED");
            assert_eq!(
                engine
                    .get_instance(&instance.id)
                    .expect("instance should exist")
                    .current_state,
                "REJECTED"
            );
        });
    }

    #[test]
    fn retry_can_exceed_limit_and_archive_instance() {
        pollster::block_on(async {
            let mut workflow = base_workflow();
            workflow.error_recovery.max_retry_count = 1;
            workflow.error_recovery.on_retry_failure = RetryFailurePolicy::Archive;
            workflow.transitions = vec![Transition {
                from: TransitionFrom::Specific("reviewing".to_owned()),
                event: "approve".to_owned(),
                to: TransitionTo::Specific("approved".to_owned()),
                guard: None,
                on_fail: None,
                action: Some(TransitionAction::Pipeline("review-pipe".to_owned())),
                on_action_failure: Some(TransitionTo::Specific("error".to_owned())),
            }];

            let runner = Arc::new(RecordingRunner {
                fail_pipeline: Mutex::new(true),
            });
            let engine = WorkflowEngine::new().with_action_runner(runner.clone());
            engine
                .register_workflow(workflow)
                .expect("register workflow");

            let mut instance = engine
                .create_instance("approval", json!({}))
                .expect("create instance");
            instance.current_state = "REVIEWING".to_owned();
            engine
                .instances
                .lock()
                .expect("instance lock")
                .insert(instance.id.clone(), instance.clone());

            engine
                .handle_event(&instance.id, event("approve", json!({})))
                .await
                .expect("first failure to error");
            *runner.fail_pipeline.lock().expect("runner lock") = false;
            engine
                .handle_event(&instance.id, event("retry", json!({})))
                .await
                .expect("first retry should recover");
            *runner.fail_pipeline.lock().expect("runner lock") = true;
            engine
                .handle_event(&instance.id, event("approve", json!({"idempotency_key":"k-1"})))
                .await
                .expect("second failure to error");

            let exceeded = engine
                .handle_event(&instance.id, event("retry", json!({})))
                .await
                .expect("retry should exceed and archive");
            assert_eq!(exceeded.reason, TransitionReason::RetryExceeded);
            assert_eq!(exceeded.to_state, "ARCHIVED");
        });
    }

    #[test]
    fn timeout_clock_pause_semantics_continue_remaining_time_after_retry() {
        pollster::block_on(async {
            let mut workflow = base_workflow();
            workflow.initial_state = "reviewing".to_owned();
            workflow.state_timeouts = vec![StateTimeout {
                state: "reviewing".to_owned(),
                timeout_ms: 100,
                on_timeout: TransitionTo::Specific("rejected".to_owned()),
                on_timeout_alert: None,
            }];
            workflow.transitions = vec![Transition {
                from: TransitionFrom::Specific("reviewing".to_owned()),
                event: "approve".to_owned(),
                to: TransitionTo::Specific("approved".to_owned()),
                guard: None,
                on_fail: None,
                action: Some(TransitionAction::Pipeline("review-pipe".to_owned())),
                on_action_failure: Some(TransitionTo::Specific("error".to_owned())),
            }];

            let runner = Arc::new(RecordingRunner {
                fail_pipeline: Mutex::new(true),
            });
            let engine = WorkflowEngine::new()
                .with_action_runner(runner.clone())
                .with_runtime_config(WorkflowRuntimeConfig {
                    timeout_defer_ms: 0,
                    ..WorkflowRuntimeConfig::default()
                });
            engine
                .register_workflow(workflow)
                .expect("register workflow");
            let instance = engine
                .create_instance("approval", json!({}))
                .expect("create instance");

            std::thread::sleep(std::time::Duration::from_millis(60));
            engine
                .handle_event(&instance.id, event("approve", json!({})))
                .await
                .expect("action failure should enter error");
            *runner.fail_pipeline.lock().expect("runner lock") = false;
            engine
                .handle_event(&instance.id, event("retry", json!({})))
                .await
                .expect("retry recovers to reviewing");
            std::thread::sleep(std::time::Duration::from_millis(50));

            let results = engine
                .handle_timeouts(kernel::types::Timestamp::now())
                .await
                .expect("timeout evaluation");
            assert_eq!(results.len(), 1);
            assert_eq!(results[0].to_state, "REJECTED");
        });
    }

    struct PipelineBackedRunner {
        engine: PipelineEngine,
        pipelines: HashMap<String, PipelineDefinition>,
    }

    #[async_trait]
    impl ActionRunner for PipelineBackedRunner {
        async fn run_pipeline(
            &self,
            pipeline_id: &str,
            _instance: &super::WorkflowInstance,
            event: &Event,
        ) -> AmanResult<()> {
            let pipeline = self
                .pipelines
                .get(pipeline_id)
                .ok_or_else(|| Error::NotFound {
                    name: format!("pipeline:{pipeline_id}"),
                })?;
            let _ = self.engine.execute(pipeline, event.clone()).await?;
            Ok(())
        }
    }

    struct FlakyApproveTool {
        failures_left: Mutex<u32>,
    }

    #[async_trait]
    impl Tool for FlakyApproveTool {
        fn name(&self) -> &str {
            "flaky-approve"
        }

        fn mode(&self) -> ToolMode {
            ToolMode::Local
        }

        fn parameters(&self) -> &JsonSchema {
            static PARAMS: std::sync::LazyLock<JsonSchema> =
                std::sync::LazyLock::new(|| JsonSchema::from(json!({"type": "object"})));
            &PARAMS
        }

        fn returns(&self) -> &JsonSchema {
            static RETURNS: std::sync::LazyLock<JsonSchema> =
                std::sync::LazyLock::new(|| JsonSchema::from(json!({"type": "object"})));
            &RETURNS
        }

        async fn execute(&self, _params: serde_json::Value, _ctx: ToolContext) -> AmanResult<serde_json::Value> {
            let mut failures_left = self.failures_left.lock().expect("tool lock");
            if *failures_left > 0 {
                *failures_left -= 1;
                return Err(Error::Unrecoverable {
                    message: "pipeline action failed".to_owned(),
                });
            }
            Ok(json!({"ok": true}))
        }
    }

    #[test]
    fn workflow_and_pipeline_chain_supports_error_retry_recovery() {
        pollster::block_on(async {
            let mut workflow = base_workflow();
            workflow.transitions = vec![Transition {
                from: TransitionFrom::Specific("reviewing".to_owned()),
                event: "approve".to_owned(),
                to: TransitionTo::Specific("approved".to_owned()),
                guard: None,
                on_fail: None,
                action: Some(TransitionAction::Pipeline("p-review".to_owned())),
                on_action_failure: Some(TransitionTo::Specific("error".to_owned())),
            }];

            let flaky_tool = Arc::new(FlakyApproveTool {
                failures_left: Mutex::new(1),
            });
            let pipeline = PipelineDefinition::new(
                "p-review",
                ConcurrencyModel::Serial,
                vec![PipelineStep {
                    id: "approve-step".to_owned(),
                    step_type: StepType::Action,
                    tool: flaky_tool,
                    compensate: None,
                    retry: RetryPolicy {
                        max_attempts: 1,
                        retry_backoff: RetryBackoff::Immediate,
                    },
                }],
            );
            let runner = Arc::new(PipelineBackedRunner {
                engine: PipelineEngine::new(),
                pipelines: HashMap::from([("p-review".to_owned(), pipeline)]),
            });
            let engine = WorkflowEngine::new().with_action_runner(runner);
            engine
                .register_workflow(workflow)
                .expect("register workflow");

            let mut instance = engine
                .create_instance("approval", json!({"req_id": Uuid::now_v7()}))
                .expect("create instance");
            instance.current_state = "REVIEWING".to_owned();
            engine
                .instances
                .lock()
                .expect("instance lock")
                .insert(instance.id.clone(), instance.clone());

            let failed: HandleEventResult = engine
                .handle_event(&instance.id, event("approve", json!({})))
                .await
                .expect("first pipeline run should fail and enter error");
            assert_eq!(failed.to_state, "ERROR");

            let retried = engine
                .handle_event(&instance.id, event("retry", json!({})))
                .await
                .expect("retry should recover to last active state");
            assert_eq!(retried.to_state, "REVIEWING");

            let succeeded = engine
                .handle_event(&instance.id, event("approve", json!({"idempotency_key":"k-2"})))
                .await
                .expect("second pipeline run should succeed");
            assert_eq!(succeeded.to_state, "APPROVED");
        });
    }

    #[test]
    fn error_cancel_requires_pending_retry_and_defer_window() {
        pollster::block_on(async {
            let mut workflow = base_workflow();
            workflow.transitions = vec![Transition {
                from: TransitionFrom::Specific("reviewing".to_owned()),
                event: "approve".to_owned(),
                to: TransitionTo::Specific("approved".to_owned()),
                guard: None,
                on_fail: None,
                action: Some(TransitionAction::Pipeline("review-pipe".to_owned())),
                on_action_failure: Some(TransitionTo::Specific("error".to_owned())),
            }];
            let runner = Arc::new(RecordingRunner {
                fail_pipeline: Mutex::new(true),
            });
            let engine = WorkflowEngine::new()
                .with_action_runner(runner)
                .with_runtime_config(WorkflowRuntimeConfig {
                    retry_cancel_conflict_defer_ms: 30,
                    ..WorkflowRuntimeConfig::default()
                });
            engine.register_workflow(workflow).expect("register workflow");
            let mut instance = engine
                .create_instance("approval", json!({}))
                .expect("create instance");
            instance.current_state = "REVIEWING".to_owned();
            engine
                .instances
                .lock()
                .expect("instance lock")
                .insert(instance.id.clone(), instance.clone());

            engine
                .handle_event(&instance.id, event("approve", json!({})))
                .await
                .expect("move to error");

            let early_cancel = engine
                .handle_event(&instance.id, event("cancel", json!({})))
                .await
                .expect("early cancel should be guard rejected");
            assert!(!early_cancel.transitioned);
            assert_eq!(early_cancel.to_state, "ERROR");

            std::thread::sleep(std::time::Duration::from_millis(35));
            let late_cancel = engine
                .handle_event(&instance.id, event("cancel", json!({})))
                .await
                .expect("cancel after defer should work");
            assert!(late_cancel.transitioned);
            assert_eq!(late_cancel.to_state, "CANCELLED");
        });
    }

    struct InflightRunner {
        inflight: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl ActionRunner for InflightRunner {
        fn inflight_pipeline_count(&self, _instance_id: &str) -> usize {
            self.inflight.load(Ordering::SeqCst)
        }
    }

    #[test]
    fn cancel_waits_inflight_pipeline_completion() {
        pollster::block_on(async {
            let mut workflow = base_workflow();
            workflow.transitions = Vec::new();
            let inflight = Arc::new(AtomicUsize::new(1));
            let runner = Arc::new(InflightRunner {
                inflight: Arc::clone(&inflight),
            });
            let engine = WorkflowEngine::new()
                .with_action_runner(runner)
                .with_runtime_config(WorkflowRuntimeConfig {
                    retry_cancel_conflict_defer_ms: 0,
                    cancel_wait_timeout_ms: 120,
                    ..WorkflowRuntimeConfig::default()
                });
            engine.register_workflow(workflow).expect("register workflow");
            let mut instance = engine
                .create_instance("approval", json!({}))
                .expect("create instance");
            instance.current_state = "ERROR".to_owned();
            instance.has_pending_retry = true;
            instance.state_entered_at = kernel::types::Timestamp::from_millis(
                kernel::types::Timestamp::now().as_millis() - 100,
            );
            engine
                .instances
                .lock()
                .expect("instance lock")
                .insert(instance.id.clone(), instance.clone());
            let inflight_handle = Arc::clone(&inflight);
            std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_millis(25));
                inflight_handle.store(0, Ordering::SeqCst);
            });
            let started = std::time::Instant::now();
            let cancelled = engine
                .handle_event(&instance.id, event("cancel", json!({})))
                .await
                .expect("cancel should wait then pass");
            assert_eq!(cancelled.to_state, "CANCELLED");
            assert!(started.elapsed().as_millis() >= 20);
        });
    }

    #[test]
    fn retry_requires_idempotency_key_for_pipeline_rerun() {
        pollster::block_on(async {
            let mut workflow = base_workflow();
            workflow.transitions = vec![Transition {
                from: TransitionFrom::Specific("reviewing".to_owned()),
                event: "approve".to_owned(),
                to: TransitionTo::Specific("approved".to_owned()),
                guard: None,
                on_fail: None,
                action: Some(TransitionAction::Pipeline("review-pipe".to_owned())),
                on_action_failure: Some(TransitionTo::Specific("error".to_owned())),
            }];
            let runner = Arc::new(RecordingRunner {
                fail_pipeline: Mutex::new(true),
            });
            let engine = WorkflowEngine::new().with_action_runner(runner.clone());
            engine.register_workflow(workflow).expect("register workflow");
            let mut instance = engine
                .create_instance("approval", json!({}))
                .expect("create instance");
            instance.current_state = "REVIEWING".to_owned();
            engine
                .instances
                .lock()
                .expect("instance lock")
                .insert(instance.id.clone(), instance.clone());

            engine
                .handle_event(&instance.id, event("approve", json!({})))
                .await
                .expect("first run fails to error");
            *runner.fail_pipeline.lock().expect("runner lock") = false;
            engine
                .handle_event(&instance.id, event("retry", json!({})))
                .await
                .expect("retry recovers");

            let missing = engine
                .handle_event(&instance.id, event("approve", json!({})))
                .await
                .expect_err("missing idempotency key should be rejected");
            assert!(matches!(missing, Error::InvalidStateTransition { .. }));

            let success = engine
                .handle_event(
                    &instance.id,
                    event("approve", json!({"idempotency_key":"k-1"})),
                )
                .await
                .expect("idempotent rerun should succeed");
            assert_eq!(success.to_state, "APPROVED");
        });
    }

    #[test]
    fn terminal_states_archive_and_archived_are_cleaned_up() {
        pollster::block_on(async {
            let workflow = base_workflow();
            let engine = WorkflowEngine::new().with_runtime_config(WorkflowRuntimeConfig {
                terminal_archive_after_ms: 10,
                archived_retention_ms: 10,
                ..WorkflowRuntimeConfig::default()
            });
            engine.register_workflow(workflow).expect("register workflow");
            let mut instance = engine
                .create_instance("approval", json!({}))
                .expect("create instance");
            instance.current_state = "APPROVED".to_owned();
            instance.state_entered_at = kernel::types::Timestamp::from_millis(
                kernel::types::Timestamp::now().as_millis() - 20,
            );
            engine
                .instances
                .lock()
                .expect("instance lock")
                .insert(instance.id.clone(), instance.clone());

            let archived = engine
                .run_terminal_recovery(kernel::types::Timestamp::now())
                .await
                .expect("terminal recovery");
            assert_eq!(archived.len(), 1);
            assert_eq!(archived[0].to_state, "ARCHIVED");

            {
                let mut map = engine.instances.lock().expect("instance lock");
                let item = map.get_mut(&instance.id).expect("instance exists");
                item.state_entered_at = kernel::types::Timestamp::from_millis(
                    kernel::types::Timestamp::now().as_millis() - 20,
                );
            }
            let removed = engine.cleanup_archived_instances(kernel::types::Timestamp::now());
            assert_eq!(removed, 1);
            assert!(engine.get_instance(&instance.id).is_none());
        });
    }

    #[test]
    fn update_instance_data_modifies_data_field() {
        let engine = WorkflowEngine::new();
        let def = base_workflow();
        engine.workflows.lock().expect("lock").insert(def.name.clone(), def);
        let instance = engine
            .create_instance("approval", json!({"version": 0, "key": "value"}))
            .expect("create instance");
        let id = instance.id.clone();

        // Update the data field.
        engine
            .update_instance_data(&id, |data| {
                data["version"] = json!(5);
                data["modified"] = json!(true);
            })
            .expect("update should succeed");

        let updated = engine.get_instance(&id).expect("instance exists");
        assert_eq!(updated.data["version"].as_u64(), Some(5));
        assert_eq!(updated.data["modified"].as_bool(), Some(true));
        assert_eq!(updated.data["key"].as_str(), Some("value"));
    }

    #[test]
    fn update_instance_data_returns_error_for_missing_id() {
        let engine = WorkflowEngine::new();
        let err = engine
            .update_instance_data("nonexistent", |data| {
                data["foo"] = json!("bar");
            })
            .expect_err("should fail for missing instance");
        assert!(matches!(err, Error::NotFound { .. }));
    }
}
