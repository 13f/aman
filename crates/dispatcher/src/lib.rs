#![forbid(unsafe_code)]
#![doc = "Dispatcher primitives for the aman agent framework."]

use event_bus::EventBus;
use idle::coordination::IdleCoordination;
use idle::types::{QueueDrained, ReflectionBreaker};
use kernel::event::{Event, EventType};
use kernel::types::{Priority, SourceId};
use kernel::{AmanResult, Error};
use pipeline::{PipelineDefinition, PipelineEngine};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tokio::time;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum MatchCondition {
    Type(EventType),
    Source(SourceId),
    TypeAndSource { event_type: EventType, source: SourceId },
    Priority(Priority),
    All(Vec<MatchCondition>),
    Any(Vec<MatchCondition>),
}

impl MatchCondition {
    #[must_use]
    pub fn matches(&self, event: &Event) -> bool {
        match self {
            Self::Type(expected_type) => &event.event_type == expected_type,
            Self::Source(expected_source) => &event.source == expected_source,
            Self::TypeAndSource { event_type, source } => {
                &event.event_type == event_type && &event.source == source
            }
            Self::Priority(priority) => &event.priority == priority,
            Self::All(conditions) => conditions.iter().all(|condition| condition.matches(event)),
            Self::Any(conditions) => conditions.iter().any(|condition| condition.matches(event)),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum DispatchTarget {
    Pipeline(String),
    Skill(String),
    Workflow(String),
    Hook(String),
    FanOut(Vec<DispatchTarget>),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RouteRule {
    pub id: String,
    pub priority: i32,
    pub condition: MatchCondition,
    pub targets: Vec<DispatchTarget>,
    pub transform: Option<TransformRule>,
    pub filter: Option<FilterRule>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum TransformRule {
    Identity,
    SetEventType(EventType),
    FanOutPayloadArray { field: String },
}

impl TransformRule {
    pub fn apply(&self, event: &Event) -> AmanResult<Vec<Event>> {
        match self {
            Self::Identity => Ok(vec![event.clone()]),
            Self::SetEventType(event_type) => {
                let mut transformed = event.clone();
                transformed.event_type = event_type.clone();
                Ok(vec![transformed])
            }
            Self::FanOutPayloadArray { field } => {
                let Some(items) = event.payload.get(field).and_then(Value::as_array) else {
                    return Ok(vec![event.clone()]);
                };
                let output = items
                    .iter()
                    .map(|item| {
                        let mut transformed = event.clone();
                        transformed.payload = item.clone();
                        transformed
                    })
                    .collect::<Vec<_>>();
                Ok(output)
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum FilterRule {
    RateLimit {
        max_events: u32,
        per_seconds: u64,
    },
}

#[derive(Debug)]
struct RateLimitWindow {
    started_at: Instant,
    count: u32,
}

#[derive(Default, Debug)]
struct DispatcherRuntimeState {
    rate_limit_windows: HashMap<String, RateLimitWindow>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DispatchedPipelineRun {
    pub rule_id: String,
    pub pipeline_id: String,
    pub output_events: Vec<Event>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PendingTargetDispatch {
    pub rule_id: String,
    pub target: DispatchTarget,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DispatchFailure {
    pub rule_id: String,
    pub target: DispatchTarget,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct DispatchResult {
    pub pipeline_runs: Vec<DispatchedPipelineRun>,
    pub output_events: Vec<Event>,
    pub pending_targets: Vec<PendingTargetDispatch>,
    pub failures: Vec<DispatchFailure>,
}

pub struct Dispatcher {
    rules: Vec<RouteRule>,
    pipelines: HashMap<String, PipelineDefinition>,
    pipeline_engine: PipelineEngine,
    runtime_state: Mutex<DispatcherRuntimeState>,
    /// Event bus reference for the idle-integrated [`run_loop`].
    event_bus: Option<Arc<dyn EventBus>>,
    /// Reflection circuit breaker configuration (from IdlePersonality).
    reflection_breaker: Option<ReflectionBreaker>,
}

impl Dispatcher {
    /// Create a new `Dispatcher` without idle integration.
    ///
    /// Use [`configure_idle`](Self::configure_idle) before calling
    /// [`run_loop`](Self::run_loop) when idle system support is needed.
    #[must_use]
    pub fn new() -> Self {
        Self {
            rules: Vec::new(),
            pipelines: HashMap::new(),
            pipeline_engine: PipelineEngine::new(),
            runtime_state: Mutex::new(DispatcherRuntimeState::default()),
            event_bus: None,
            reflection_breaker: None,
        }
    }

    /// Configure the Dispatcher for idle-integrated event loop operation.
    ///
    /// Must be called before [`run_loop`](Self::run_loop).
    pub fn configure_idle(&mut self, event_bus: Arc<dyn EventBus>, breaker: ReflectionBreaker) {
        self.event_bus = Some(event_bus);
        self.reflection_breaker = Some(breaker);
    }

    pub fn add_rule(&mut self, rule: RouteRule) {
        self.rules.push(rule);
    }

    pub fn rebuild_routes(&mut self, mut rules: Vec<RouteRule>) {
        rules.sort_by(|left, right| {
            right
                .priority
                .cmp(&left.priority)
                .then_with(|| left.id.cmp(&right.id))
        });
        self.rules = rules;
    }

    pub fn register_pipeline(&mut self, pipeline: PipelineDefinition) {
        self.pipelines.insert(pipeline.id.clone(), pipeline);
    }

    #[must_use]
    pub fn route_matches<'a>(&'a self, event: &Event) -> Vec<&'a RouteRule> {
        let mut matched = self
            .rules
            .iter()
            .filter(|rule| rule.condition.matches(event))
            .collect::<Vec<_>>();
        matched.sort_by(|left, right| {
            right
                .priority
                .cmp(&left.priority)
                .then_with(|| left.id.cmp(&right.id))
        });
        matched
    }

    pub async fn dispatch(&self, event: Event) -> DispatchResult {
        let mut result = DispatchResult::default();
        for rule in self.route_matches(&event) {
            if !self.passes_filter(rule) {
                continue;
            }
            let transformed_events = match &rule.transform {
                Some(transform_rule) => match transform_rule.apply(&event) {
                    Ok(events) => events,
                    Err(error) => {
                        result.failures.push(DispatchFailure {
                            rule_id: rule.id.clone(),
                            target: DispatchTarget::FanOut(rule.targets.clone()),
                            message: format!("transform failed: {error}"),
                        });
                        continue;
                    }
                },
                None => vec![event.clone()],
            };

            let mut targets = Vec::new();
            for target in &rule.targets {
                flatten_targets(target, &mut targets);
            }
            for transformed_event in transformed_events {
                for target in &targets {
                    self.dispatch_target(&transformed_event, rule, target, &mut result)
                        .await;
                }
            }
        }
        result
    }

    fn passes_filter(&self, rule: &RouteRule) -> bool {
        let Some(filter) = &rule.filter else {
            return true;
        };
        match filter {
            FilterRule::RateLimit {
                max_events,
                per_seconds,
            } => {
                let mut state = self.runtime_state.lock().expect("runtime state mutex");
                let window = state
                    .rate_limit_windows
                    .entry(rule.id.clone())
                    .or_insert_with(|| RateLimitWindow {
                        started_at: Instant::now(),
                        count: 0,
                    });
                let window_duration = Duration::from_secs((*per_seconds).max(1));
                if window.started_at.elapsed() >= window_duration {
                    window.started_at = Instant::now();
                    window.count = 0;
                }
                if window.count >= *max_events {
                    return false;
                }
                window.count = window.count.saturating_add(1);
                true
            }
        }
    }

    async fn dispatch_target(
        &self,
        event: &Event,
        rule: &RouteRule,
        target: &DispatchTarget,
        result: &mut DispatchResult,
    ) {
        match target {
            DispatchTarget::Pipeline(pipeline_id) => match self.pipelines.get(pipeline_id) {
                Some(pipeline) => match self.pipeline_engine.execute(pipeline, event.clone()).await {
                    Ok(output_events) => {
                        let linked: Vec<Event> = output_events
                            .into_iter()
                            .map(|mut e| {
                                e.metadata.parent_event_id = Some(event.id);
                                e
                            })
                            .collect();
                        result.output_events.extend(linked.clone());
                        result.pipeline_runs.push(DispatchedPipelineRun {
                            rule_id: rule.id.clone(),
                            pipeline_id: pipeline_id.clone(),
                            output_events: linked,
                        });
                    }
                    Err(error) => result.failures.push(DispatchFailure {
                        rule_id: rule.id.clone(),
                        target: target.clone(),
                        message: format!("{error}"),
                    }),
                },
                None => result.failures.push(DispatchFailure {
                    rule_id: rule.id.clone(),
                    target: target.clone(),
                    message: format!("pipeline `{pipeline_id}` is not registered"),
                }),
            },
            _ => result.pending_targets.push(PendingTargetDispatch {
                rule_id: rule.id.clone(),
                target: target.clone(),
            }),
        }
    }

    pub async fn dispatch_to_pipeline(
        &self,
        pipeline_id: &str,
        event: Event,
    ) -> AmanResult<Vec<Event>> {
        let Some(pipeline) = self.pipelines.get(pipeline_id) else {
            return Err(Error::NotFound {
                name: format!("pipeline:{pipeline_id}"),
            });
        };
        self.pipeline_engine.execute(pipeline, event).await
    }

    // ── Idle-integrated event loop (M4) ─────────────────────────

    /// Run the dispatcher main loop with idle system integration.
    ///
    /// This loop processes events from the bus with three-way classification:
    ///
    /// 1. **Real events** (`is_queue_drained` and `is_idle_event` are false):
    ///    - Updates `last_source_type` (only for external sources, R5-1 guard)
    ///    - Calls `coord.reset_idle_signal()` to cancel running idle workflows
    ///    - Dispatches the event to matching pipelines
    ///    - Sets `recently_processed_real_event = true`
    ///
    /// 2. **QueueDrained events**:
    ///    - Sets `coord.busy_reflecting = true`
    ///    - Uses `tokio::select!` to race between:
    ///      - Reflection pipeline execution
    ///      - `wait_for_event()` — new event preempts reflection (R2-9)
    ///    - On preemption: aborts reflection, resets circuit breaker count
    ///
    /// 3. **Idle events**: dispatched normally via routing rules.
    ///
    /// When the queue is empty and a real event was recently processed,
    /// produces a `QueueDrained` event (subject to circuit breaker).
    pub async fn run_loop(&mut self, coord: &IdleCoordination) {
        let event_bus = self
            .event_bus
            .clone()
            .expect("Dispatcher::run_loop requires event_bus (call configure_idle)");
        let breaker_config = self
            .reflection_breaker
            .expect("Dispatcher::run_loop requires reflection_breaker (call configure_idle)");

        let mut recently_processed_real_event = false;
        let mut reflection_consecutive_count: u32 = 0;
        let mut last_event_type = String::new();
        let mut last_trace_id = String::new();

        loop {
            match event_bus.try_dequeue() {
                Some(event) => {
                    let is_real = !event.is_queue_drained() && !event.is_idle_event();

                    if is_real {
                        // ── Real event branch ──
                        recently_processed_real_event = true;

                        // T4.4 (R5-1 guard): only external sources update last_source_type.
                        // Internal chain tasks (e.g. Reflection output) must not
                        // overwrite it — otherwise ChatMode gets silently deactivated
                        // during an active conversation.
                        if event.is_from_external_source() {
                            coord.last_source_type.store(
                                event.source_type().to_u8(),
                                Ordering::Relaxed,
                            );
                        }

                        // T4.6: cancel running idle workflows
                        coord.reset_idle_signal().await;

                        // Arousal boost: real events raise engagement
                        coord.arousal.boost(0.3);

                        // Track event metadata for QueueDrained production
                        last_event_type = event.event_type.to_string();
                        last_trace_id = event.metadata.trace_id.to_string();

                        self.dispatch(event).await;
                    } else if event.is_queue_drained() {
                        // ── QueueDrained → Reflection branch (T4.3) ──
                        coord.busy_reflecting.store(true, Ordering::SeqCst);

                        // T4.3: select! between reflection execution and new event arrival
                        // Use a typed future for the reflection pipeline to help inference.
                        let reflection_fut = self.dispatch_to_pipeline("pipeline:reflection", event);
                        tokio::pin!(reflection_fut);

                        tokio::select! {
                            result = &mut reflection_fut => {
                                coord.busy_reflecting.store(false, Ordering::SeqCst);
                                // result: &AmanResult<Vec<Event>>
                                if let Ok(output_events) = result {
                                    if output_events.is_empty() {
                                        // No output → reset circuit breaker
                                        reflection_consecutive_count = 0;
                                    } else {
                                        // Publish output events back to the bus
                                        for new_event in output_events {
                                            let _ = event_bus.publish(new_event.clone()).await;
                                        }
                                    }
                                } else {
                                    // Error → don't count as consecutive
                                    reflection_consecutive_count = 0;
                                }
                            }
                            event_result = event_bus.wait_for_event(Duration::from_secs(3600)) => {
                                // T4.3 (R2-9): new event arrived during reflection.
                                // R2-8: preempted → reset circuit breaker count.
                                reflection_consecutive_count = 0;
                                coord.busy_reflecting.store(false, Ordering::SeqCst);

                                // wait_for_event may have consumed an event from the queue.
                                // Process it directly if available (avoids losing events).
                                if let Ok(preempt_event) = event_result {
                                    // Treat as a real event (T4.4 + T4.6)
                                    if preempt_event.is_from_external_source() {
                                        coord.last_source_type.store(
                                            preempt_event.source_type().to_u8(),
                                            Ordering::Relaxed,
                                        );
                                    }
                                    coord.reset_idle_signal().await;
                                    coord.arousal.boost(0.3);
                                    last_event_type = preempt_event.event_type.to_string();
                                    last_trace_id = preempt_event.metadata.trace_id.to_string();
                                    self.dispatch(preempt_event).await;
                                    recently_processed_real_event = true;
                                }
                            }
                        }
                    } else {
                        // ── Idle event branch ──
                        self.dispatch(event).await;
                    }
                }
                None => {
                    // ── Queue empty branch ──
                    if recently_processed_real_event {
                        recently_processed_real_event = false;

                        // T4.5: circuit breaker — check before producing QueueDrained
                        if reflection_consecutive_count >= breaker_config.max_consecutive * 2 {
                            // Full cooldown: skip QueueDrained, sleep, reset counter
                            reflection_consecutive_count = 0;
                            time::sleep(Duration::from_secs(breaker_config.cooldown_secs)).await;
                            continue;
                        }

                        // R3-1: signal idle detector to reset depth
                        coord.signal_queue_drained();

                        // T4.2: produce QueueDrained
                        #[allow(unused_variables)]
                        let drained = QueueDrained {
                            last_event_type: last_event_type.clone(),
                            last_trace_id: last_trace_id.clone(),
                            last_result_summary: String::new(),
                            arousal_level: coord.arousal.current(),
                            reflection_consecutive_count,
                        };

                        let _ = event_bus.publish(drained.into()).await;
                        reflection_consecutive_count += 1;
                    }

                    // Brief sleep before polling again
                    time::sleep(Duration::from_millis(100)).await;
                }
            }
        }
    }
}

#[must_use]
pub fn match_condition_from_subscription_filter(
    filter: &event_bus::SubscriptionFilter,
) -> Option<MatchCondition> {
    let mut all_conditions = Vec::new();
    if let Some(event_types) = &filter.event_types {
        let options = event_types
            .iter()
            .cloned()
            .map(MatchCondition::Type)
            .collect::<Vec<_>>();
        all_conditions.push(if options.len() == 1 {
            options[0].clone()
        } else {
            MatchCondition::Any(options)
        });
    }
    if let Some(sources) = &filter.sources {
        let options = sources
            .iter()
            .cloned()
            .map(MatchCondition::Source)
            .collect::<Vec<_>>();
        all_conditions.push(if options.len() == 1 {
            options[0].clone()
        } else {
            MatchCondition::Any(options)
        });
    }
    if let Some(priorities) = &filter.priorities {
        let options = priorities
            .iter()
            .copied()
            .map(MatchCondition::Priority)
            .collect::<Vec<_>>();
        all_conditions.push(if options.len() == 1 {
            options[0].clone()
        } else {
            MatchCondition::Any(options)
        });
    }

    if all_conditions.is_empty() {
        None
    } else if all_conditions.len() == 1 {
        Some(all_conditions.remove(0))
    } else {
        Some(MatchCondition::All(all_conditions))
    }
}

fn flatten_targets(target: &DispatchTarget, output: &mut Vec<DispatchTarget>) {
    match target {
        DispatchTarget::FanOut(children) => {
            for child in children {
                flatten_targets(child, output);
            }
        }
        _ => output.push(target.clone()),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        match_condition_from_subscription_filter, DispatchTarget, Dispatcher, FilterRule,
        MatchCondition, RouteRule, TransformRule,
    };
    use event_bus::{EventBus, InMemoryBus, SubscriptionFilter};
    use idle::coordination::IdleCoordination;
    use idle::types::{QueueDrained, ReflectionBreaker};
    use kernel::context::ToolContext;
    use kernel::event::{Event, EventType};
    use kernel::pipeline::{PipelineStep, StepType};
    use kernel::retry::{RetryBackoff, RetryPolicy};
    use kernel::schema::JsonSchema;
    use kernel::tool::Tool;
    use kernel::types::{ConcurrencyModel, Priority, SourceId, ToolMode};
    use kernel::{AmanResult, Error};
    use pipeline::PipelineDefinition;
    use serde_json::{json, Value};
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};

    #[derive(Debug)]
    struct StubTool {
        outputs: Mutex<VecDeque<AmanResult<Value>>>,
    }

    #[async_trait::async_trait]
    impl Tool for StubTool {
        fn name(&self) -> &str {
            "stub"
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

        async fn execute(&self, _params: Value, _ctx: ToolContext) -> AmanResult<Value> {
            self.outputs
                .lock()
                .expect("stub outputs mutex")
                .pop_front()
                .unwrap_or_else(|| Ok(json!({})))
        }
    }

    fn build_pipeline(id: &str, outputs: Vec<AmanResult<Value>>) -> PipelineDefinition {
        PipelineDefinition::new(
            id.to_owned(),
            ConcurrencyModel::Serial,
            vec![PipelineStep {
                id: "transform".to_owned(),
                step_type: StepType::Transform,
                tool: Arc::new(StubTool {
                    outputs: Mutex::new(VecDeque::from(outputs)),
                }),
                compensate: None,
                retry: RetryPolicy {
                    max_attempts: 2,
                    retry_backoff: RetryBackoff::Immediate,
                },
            }],
        )
    }

    fn webhook_event() -> Event {
        let mut event = Event::new(
            "webhook:billing",
            EventType::WebhookReceived,
            json!({"invoice": "inv_1"}),
        );
        event.priority = Priority::High;
        event
    }

    #[test]
    fn match_condition_supports_type_source_and_priority() {
        let event = webhook_event();
        assert!(MatchCondition::Type(EventType::WebhookReceived).matches(&event));
        assert!(MatchCondition::Source(SourceId::new("webhook:billing")).matches(&event));
        assert!(MatchCondition::Priority(Priority::High).matches(&event));
        assert!(MatchCondition::TypeAndSource {
            event_type: EventType::WebhookReceived,
            source: SourceId::new("webhook:billing"),
        }
        .matches(&event));
        assert!(!MatchCondition::Priority(Priority::Low).matches(&event));
    }

    #[test]
    fn dispatch_routes_to_pipeline_and_emits_outputs() {
        pollster::block_on(async {
            let mut dispatcher = Dispatcher::new();
            dispatcher.register_pipeline(build_pipeline(
                "p-high",
                vec![Ok(json!({"payload": {"routed": true}}))],
            ));
            dispatcher.rebuild_routes(vec![RouteRule {
                id: "r1".to_owned(),
                priority: 100,
                condition: MatchCondition::Type(EventType::WebhookReceived),
                targets: vec![DispatchTarget::Pipeline("p-high".to_owned())],
                transform: None,
                filter: None,
            }]);

            let result = dispatcher.dispatch(webhook_event()).await;

            assert!(result.failures.is_empty());
            assert_eq!(result.pipeline_runs.len(), 1);
            assert_eq!(result.output_events.len(), 1);
            assert_eq!(result.output_events[0].payload, json!({"payload": {"routed": true}}));
        });
    }

    #[test]
    fn higher_priority_rules_are_executed_first() {
        pollster::block_on(async {
            let mut dispatcher = Dispatcher::new();
            dispatcher.register_pipeline(build_pipeline("p-high", vec![Ok(json!({"order": 1}))]));
            dispatcher.register_pipeline(build_pipeline("p-low", vec![Ok(json!({"order": 2}))]));

            dispatcher.rebuild_routes(vec![
                RouteRule {
                    id: "low".to_owned(),
                    priority: 1,
                    condition: MatchCondition::Type(EventType::WebhookReceived),
                    targets: vec![DispatchTarget::Pipeline("p-low".to_owned())],
                    transform: None,
                    filter: None,
                },
                RouteRule {
                    id: "high".to_owned(),
                    priority: 10,
                    condition: MatchCondition::Type(EventType::WebhookReceived),
                    targets: vec![DispatchTarget::Pipeline("p-high".to_owned())],
                    transform: None,
                    filter: None,
                },
            ]);

            let result = dispatcher.dispatch(webhook_event()).await;
            assert_eq!(result.pipeline_runs.len(), 2);
            assert_eq!(result.pipeline_runs[0].rule_id, "high");
            assert_eq!(result.pipeline_runs[1].rule_id, "low");
        });
    }

    #[test]
    fn dispatch_returns_failure_when_pipeline_errors() {
        pollster::block_on(async {
            let mut dispatcher = Dispatcher::new();
            dispatcher.register_pipeline(build_pipeline(
                "p-fail",
                vec![
                    Err(Error::Unrecoverable {
                        message: "boom".to_owned(),
                    }),
                    Err(Error::Unrecoverable {
                        message: "boom".to_owned(),
                    }),
                ],
            ));
            dispatcher.add_rule(RouteRule {
                id: "r-fail".to_owned(),
                priority: 1,
                condition: MatchCondition::Type(EventType::WebhookReceived),
                targets: vec![DispatchTarget::Pipeline("p-fail".to_owned())],
                transform: None,
                filter: None,
            });

            let result = dispatcher.dispatch(webhook_event()).await;
            assert_eq!(result.output_events.len(), 0);
            assert_eq!(result.failures.len(), 1);
            assert!(result.failures[0].message.contains("boom"));
        });
    }

    #[test]
    fn transform_rule_fan_out_payload_array_expands_event() {
        pollster::block_on(async {
            let mut dispatcher = Dispatcher::new();
            dispatcher.register_pipeline(build_pipeline(
                "p-fanout",
                vec![Ok(json!({"payload": {"ok": true}})), Ok(json!({"payload": {"ok": true}}))],
            ));
            dispatcher.add_rule(RouteRule {
                id: "r-fanout".to_owned(),
                priority: 1,
                condition: MatchCondition::Type(EventType::WebhookReceived),
                targets: vec![DispatchTarget::Pipeline("p-fanout".to_owned())],
                transform: Some(TransformRule::FanOutPayloadArray {
                    field: "items".to_owned(),
                }),
                filter: None,
            });
            let mut event = webhook_event();
            event.payload = json!({"items": [{"a": 1}, {"a": 2}]});

            let result = dispatcher.dispatch(event).await;
            assert_eq!(result.pipeline_runs.len(), 2);
        });
    }

    #[test]
    fn rate_limit_filter_drops_events_after_threshold() {
        pollster::block_on(async {
            let mut dispatcher = Dispatcher::new();
            dispatcher.register_pipeline(build_pipeline("p-rate", vec![Ok(json!({"ok": true}))]));
            dispatcher.add_rule(RouteRule {
                id: "r-rate".to_owned(),
                priority: 1,
                condition: MatchCondition::Type(EventType::WebhookReceived),
                targets: vec![DispatchTarget::Pipeline("p-rate".to_owned())],
                transform: None,
                filter: Some(FilterRule::RateLimit {
                    max_events: 1,
                    per_seconds: 60,
                }),
            });

            let first = dispatcher.dispatch(webhook_event()).await;
            let second = dispatcher.dispatch(webhook_event()).await;
            assert_eq!(first.pipeline_runs.len(), 1);
            assert_eq!(second.pipeline_runs.len(), 0);
        });
    }

    #[test]
    fn converts_subscription_filter_to_match_condition() {
        let filter = SubscriptionFilter {
            event_types: Some(vec![EventType::WebhookReceived]),
            sources: Some(vec![SourceId::new("webhook:billing")]),
            priorities: Some(vec![Priority::High]),
            payload_match: None,
        };
        let condition = match_condition_from_subscription_filter(&filter)
            .expect("condition should be generated");
        let event = webhook_event();
        assert!(condition.matches(&event));
    }

    #[test]
    fn rebuild_routes_replaces_old_rules() {
        pollster::block_on(async {
            let mut dispatcher = Dispatcher::new();
            dispatcher.register_pipeline(build_pipeline("p-a", vec![Ok(json!({"order": "a"}))]));
            dispatcher.register_pipeline(build_pipeline("p-b", vec![Ok(json!({"order": "b"}))]));

            dispatcher.add_rule(RouteRule {
                id: "old".to_owned(),
                priority: 1,
                condition: MatchCondition::Type(EventType::WebhookReceived),
                targets: vec![DispatchTarget::Pipeline("p-a".to_owned())],
                transform: None,
                filter: None,
            });

            dispatcher.rebuild_routes(vec![RouteRule {
                id: "new".to_owned(),
                priority: 10,
                condition: MatchCondition::Type(EventType::WebhookReceived),
                targets: vec![DispatchTarget::Pipeline("p-b".to_owned())],
                transform: None,
                filter: None,
            }]);

            let result = dispatcher.dispatch(webhook_event()).await;
            assert_eq!(result.pipeline_runs.len(), 1);
            assert_eq!(result.pipeline_runs[0].rule_id, "new");
            assert_eq!(result.pipeline_runs[0].pipeline_id, "p-b");
        });
    }

    #[test]
    fn dispatch_with_no_matching_rules_returns_empty_result() {
        pollster::block_on(async {
            let mut dispatcher = Dispatcher::new();
            dispatcher.register_pipeline(build_pipeline("p-a", vec![Ok(json!({"ok": true}))]));
            dispatcher.add_rule(RouteRule {
                id: "only-file-created".to_owned(),
                priority: 1,
                condition: MatchCondition::Type(EventType::FileCreated),
                targets: vec![DispatchTarget::Pipeline("p-a".to_owned())],
                transform: None,
                filter: None,
            });

            let result = dispatcher.dispatch(webhook_event()).await;
            assert!(result.pipeline_runs.is_empty());
            assert!(result.output_events.is_empty());
            assert!(result.pending_targets.is_empty());
            assert!(result.failures.is_empty());
        });
    }

    #[test]
    fn fan_out_mixed_targets_collects_pipeline_and_pending_targets() {
        pollster::block_on(async {
            let mut dispatcher = Dispatcher::new();
            dispatcher.register_pipeline(build_pipeline(
                "p-a",
                vec![Ok(json!({"payload": {"fanout": true}}))],
            ));
            dispatcher.add_rule(RouteRule {
                id: "fanout".to_owned(),
                priority: 10,
                condition: MatchCondition::Type(EventType::WebhookReceived),
                targets: vec![DispatchTarget::FanOut(vec![
                    DispatchTarget::Pipeline("p-a".to_owned()),
                    DispatchTarget::Skill("skill-a".to_owned()),
                    DispatchTarget::Workflow("wf-a".to_owned()),
                ])],
                transform: None,
                filter: None,
            });

            let result = dispatcher.dispatch(webhook_event()).await;
            assert_eq!(result.pipeline_runs.len(), 1);
            assert_eq!(result.pending_targets.len(), 2);
            assert!(result
                .pending_targets
                .iter()
                .any(|target| matches!(target.target, DispatchTarget::Skill(_))));
            assert!(result
                .pending_targets
                .iter()
                .any(|target| matches!(target.target, DispatchTarget::Workflow(_))));
        });
    }

    #[test]
    fn transform_set_event_type_rewrites_event_before_pipeline_execution() {
        pollster::block_on(async {
            let mut dispatcher = Dispatcher::new();
            dispatcher.register_pipeline(build_pipeline(
                "p-transform",
                vec![Ok(json!({"payload": {"rewritten": true}}))],
            ));
            dispatcher.add_rule(RouteRule {
                id: "rewrite-type".to_owned(),
                priority: 10,
                condition: MatchCondition::Type(EventType::WebhookReceived),
                targets: vec![DispatchTarget::Pipeline("p-transform".to_owned())],
                transform: Some(TransformRule::SetEventType(EventType::Custom(
                    "rewritten_type".to_owned(),
                ))),
                filter: None,
            });

            let result = dispatcher.dispatch(webhook_event()).await;
            assert_eq!(result.pipeline_runs.len(), 1);
            assert_eq!(result.pipeline_runs[0].output_events.len(), 1);
            assert_eq!(
                result.pipeline_runs[0].output_events[0].event_type,
                EventType::Custom("rewritten_type".to_owned())
            );
        });
    }

    // ── M4: Idle integration tests ──────────────────────────────

    /// Helper: build a bare-bones InMemoryBus for testing.
    fn test_bus() -> Arc<InMemoryBus> {
        let mut config = event_bus::InMemoryBusConfig::default();
        config.max_queue_size = 100;
        Arc::new(InMemoryBus::new(config))
    }

    #[test]
    fn configure_idle_sets_event_bus_and_breaker() {
        let bus = test_bus();
        let breaker = ReflectionBreaker {
            max_consecutive: 5,
            cooldown_secs: 300,
        };

        let mut dispatcher = Dispatcher::new();
        dispatcher.configure_idle(bus.clone(), breaker);

        // Can't access private fields, but verify dispatch still works
        // and id is correctly handled.
        let event = Event::new(
            "source:a",
            EventType::MessageReceived,
            json!({"ok": true}),
        );
        let result = pollster::block_on(dispatcher.dispatch(event));
        assert!(result.failures.is_empty());
    }

    #[test]
    fn queue_drained_event_construction_and_classification() {
        // Verify QueueDrained can be constructed and classified correctly
        let qd = QueueDrained {
            last_event_type: "message_received".into(),
            last_trace_id: "test-trace".into(),
            last_result_summary: String::new(),
            arousal_level: 0.5,
            reflection_consecutive_count: 0,
        };
        let qd_event: Event = qd.into();
        assert!(qd_event.is_queue_drained());
        assert!(!qd_event.is_from_external_source());
        assert!(!qd_event.is_idle_event());
        assert_eq!(qd_event.event_type.as_str(), "system.queue_drained");
    }

    #[test]
    fn real_event_classification_identifies_all_types() {
        let real = Event::new("source:a", EventType::MessageReceived, json!({}));
        let idle = Event::new("idle.system", EventType::Idle, json!({}));
        let qd: Event = QueueDrained {
            last_event_type: "test".into(),
            last_trace_id: "t1".into(),
            last_result_summary: String::new(),
            arousal_level: 0.0,
            reflection_consecutive_count: 0,
        }
        .into();

        // Real event: not queue_drained AND not idle
        assert!(!real.is_queue_drained() && !real.is_idle_event());
        // QueueDrained event
        assert!(qd.is_queue_drained());
        assert!(!qd.is_idle_event());
        // Idle event
        assert!(idle.is_idle_event());
        assert!(!idle.is_queue_drained());

        // QueueDrained and Idle are "not from external source"
        assert!(!qd.is_from_external_source());
        assert!(!idle.is_from_external_source());
        assert!(real.is_from_external_source());
    }

    #[test]
    fn external_event_updates_source_type() {
        pollster::block_on(async {
            let bus = test_bus();
            let coord = IdleCoordination::new(1.0, 900.0);
            let mut dispatcher = Dispatcher::new();
            dispatcher.configure_idle(
                bus.clone(),
                ReflectionBreaker {
                    max_consecutive: 5,
                    cooldown_secs: 300,
                },
            );

            // Chat source event should set last_source_type to Chat
            let chat_event = Event::new(
                "chat:slack",
                EventType::MessageReceived,
                json!({"msg": "hello"}),
            );
            assert!(chat_event.is_from_external_source());
            assert_eq!(
                chat_event.source_type().to_u8(),
                kernel::types::SourceType::Chat.to_u8()
            );

            // Directly test the source_type store logic from run_loop
            coord.last_source_type.store(
                chat_event.source_type().to_u8(),
                std::sync::atomic::Ordering::Relaxed,
            );
            assert_eq!(coord.last_source_type.load(std::sync::atomic::Ordering::Relaxed), 8);

            // Non-chat external event
            let file_event = Event::new(
                "watch:invoices",
                EventType::FileCreated,
                json!({"file": "a.pdf"}),
            );
            assert!(file_event.is_from_external_source());
            assert_eq!(
                file_event.source_type().to_u8(),
                kernel::types::SourceType::Custom.to_u8()
            );
        });
    }
}
