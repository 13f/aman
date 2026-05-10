#![forbid(unsafe_code)]
#![doc = "Dispatcher primitives for the Aman agent framework."]

use kernel::event::{Event, EventType};
use kernel::types::{Priority, SourceId};
use kernel::{AmanResult, Error};
use pipeline::{PipelineDefinition, PipelineEngine};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

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

#[derive(Default)]
pub struct Dispatcher {
    rules: Vec<RouteRule>,
    pipelines: HashMap<String, PipelineDefinition>,
    pipeline_engine: PipelineEngine,
    runtime_state: Mutex<DispatcherRuntimeState>,
}

impl Dispatcher {
    #[must_use]
    pub fn new() -> Self {
        Self {
            rules: Vec::new(),
            pipelines: HashMap::new(),
            pipeline_engine: PipelineEngine::new(),
            runtime_state: Mutex::new(DispatcherRuntimeState::default()),
        }
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
                        result.output_events.extend(output_events.iter().cloned());
                        result.pipeline_runs.push(DispatchedPipelineRun {
                            rule_id: rule.id.clone(),
                            pipeline_id: pipeline_id.clone(),
                            output_events,
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
    use event_bus::SubscriptionFilter;
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
}
