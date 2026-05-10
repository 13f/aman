use crate::context::PipelineContext;
use crate::error::AmanResult;
use crate::event::Event;
use crate::retry::RetryPolicy;
use crate::tool::Tool;
use crate::types::ConcurrencyModel;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::sync::Arc;

pub type PipelineResult = AmanResult<Vec<Event>>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepType {
    Filter,
    Transform,
    Action,
}

#[derive(Clone)]
pub struct PipelineStep {
    pub id: String,
    pub step_type: StepType,
    pub tool: Arc<dyn Tool>,
    pub compensate: Option<Arc<dyn Tool>>,
    pub retry: RetryPolicy,
}

impl fmt::Debug for PipelineStep {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PipelineStep")
            .field("id", &self.id)
            .field("step_type", &self.step_type)
            .field("retry", &self.retry)
            .finish()
    }
}

#[async_trait]
pub trait Pipeline: Send + Sync {
    fn id(&self) -> &str;
    fn concurrency(&self) -> ConcurrencyModel;
    fn steps(&self) -> &[PipelineStep];

    async fn execute(&self, event: Event, ctx: PipelineContext) -> PipelineResult;
}

#[cfg(test)]
mod tests {
    use super::{Pipeline, PipelineResult, PipelineStep, StepType};
    use crate::context::{BaseContext, PipelineContext, ToolContext};
    use crate::event::{Event, EventType};
    use crate::retry::RetryPolicy;
    use crate::schema::JsonSchema;
    use crate::tool::{Tool, ToolResult};
    use crate::types::{ConcurrencyModel, ToolMode, TraceId};
    use pollster::block_on;
    use serde_json::{json, Value};
    use std::sync::Arc;

    struct DummyTool {
        parameters: JsonSchema,
        returns: JsonSchema,
    }

    #[async_trait::async_trait]
    impl Tool for DummyTool {
        fn name(&self) -> &str {
            "dummy-tool"
        }

        fn mode(&self) -> ToolMode {
            ToolMode::Local
        }

        fn parameters(&self) -> &JsonSchema {
            &self.parameters
        }

        fn returns(&self) -> &JsonSchema {
            &self.returns
        }

        async fn execute(&self, _params: Value, _ctx: ToolContext) -> ToolResult {
            Ok(json!({"ok": true}))
        }
    }

    struct DummyPipeline {
        steps: Vec<PipelineStep>,
    }

    #[async_trait::async_trait]
    impl Pipeline for DummyPipeline {
        fn id(&self) -> &str {
            "dummy-pipeline"
        }

        fn concurrency(&self) -> ConcurrencyModel {
            ConcurrencyModel::Serial
        }

        fn steps(&self) -> &[PipelineStep] {
            &self.steps
        }

        async fn execute(&self, event: Event, _ctx: PipelineContext) -> PipelineResult {
            Ok(vec![event])
        }
    }

    #[test]
    fn pipeline_step_debug_omits_trait_object_details() {
        let tool = Arc::new(DummyTool {
            parameters: JsonSchema::default(),
            returns: JsonSchema::default(),
        });
        let step = PipelineStep {
            id: "step-1".to_owned(),
            step_type: StepType::Action,
            tool: tool.clone(),
            compensate: Some(tool),
            retry: RetryPolicy::default(),
        };

        let debug = format!("{step:?}");
        assert!(debug.contains("step-1"));
        assert!(debug.contains("Action"));
    }

    #[test]
    fn pipeline_trait_exposes_steps_and_executes() {
        let tool = Arc::new(DummyTool {
            parameters: JsonSchema::from(json!({"type": "object"})),
            returns: JsonSchema::from(json!({"type": "object"})),
        });
        let pipeline = DummyPipeline {
            steps: vec![PipelineStep {
                id: "step-1".to_owned(),
                step_type: StepType::Transform,
                tool,
                compensate: None,
                retry: RetryPolicy::default(),
            }],
        };
        let ctx = PipelineContext {
            base: BaseContext::new(TraceId::new()),
            pipeline_id: Some("dummy-pipeline".to_owned()),
            instance_id: Some("instance-1".to_owned()),
        };
        let event = Event::new("timer:test", EventType::TimerTick, json!({"ok": true}));

        let output = block_on(pipeline.execute(event.clone(), ctx)).expect("execute succeeds");

        assert_eq!(pipeline.id(), "dummy-pipeline");
        assert_eq!(pipeline.concurrency(), ConcurrencyModel::Serial);
        assert_eq!(pipeline.steps().len(), 1);
        assert_eq!(output, vec![event]);
    }
}
