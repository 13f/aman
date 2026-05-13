use criterion::{black_box, criterion_group, criterion_main, Criterion};
use kernel::event::{Event, EventType};
use kernel::pipeline::{PipelineStep, StepType};
use kernel::tool::{Tool, ToolResult};
use kernel::types::{ConcurrencyModel, ToolMode};
use pipeline::{PipelineDefinition, PipelineEngine};
use serde_json::{json, Value};
use std::sync::Arc;

struct NoopTool {
    schema: kernel::schema::JsonSchema,
}

impl NoopTool {
    fn new() -> Self {
        Self {
            schema: kernel::schema::JsonSchema::default(),
        }
    }
}

#[async_trait::async_trait]
impl Tool for NoopTool {
    fn name(&self) -> &str {
        "noop"
    }
    fn mode(&self) -> ToolMode {
        ToolMode::Local
    }
    fn parameters(&self) -> &kernel::schema::JsonSchema {
        &self.schema
    }
    fn returns(&self) -> &kernel::schema::JsonSchema {
        &self.schema
    }
    async fn execute(&self, params: Value, _ctx: kernel::context::ToolContext) -> ToolResult {
        Ok(params)
    }
}

fn bench_pipeline_3_steps(c: &mut Criterion) {
    let tool = Arc::new(NoopTool::new());
    let retry = kernel::retry::RetryPolicy::default();

    let pipeline = PipelineDefinition::new(
        "bench-pipeline",
        ConcurrencyModel::Serial,
        vec![
            PipelineStep {
                id: "filter".into(),
                step_type: StepType::Filter,
                tool: tool.clone(),
                compensate: None,
                retry: retry.clone(),
            },
            PipelineStep {
                id: "transform".into(),
                step_type: StepType::Transform,
                tool: tool.clone(),
                compensate: None,
                retry: retry.clone(),
            },
            PipelineStep {
                id: "action".into(),
                step_type: StepType::Action,
                tool,
                compensate: None,
                retry,
            },
        ],
    );

    let engine = PipelineEngine::new();
    let event = Event::new("bench:source", EventType::FileCreated, json!({"key": "value"}));

    c.bench_function("pipeline_3_steps_serial", |b| {
        b.iter(|| {
            pollster::block_on(async {
                let result = engine
                    .execute(&pipeline, event.clone())
                    .await
                    .expect("pipeline execute");
                black_box(result);
            });
        });
    });
}

fn bench_pipeline_single_step(c: &mut Criterion) {
    let tool = Arc::new(NoopTool::new());

    let pipeline = PipelineDefinition::new(
        "bench-pipeline-single",
        ConcurrencyModel::Serial,
        vec![PipelineStep {
            id: "action".into(),
            step_type: StepType::Action,
            tool,
            compensate: None,
            retry: kernel::retry::RetryPolicy::default(),
        }],
    );

    let engine = PipelineEngine::new();
    let event = Event::new("bench:source", EventType::FileCreated, json!({"key": "value"}));

    c.bench_function("pipeline_1_step", |b| {
        b.iter(|| {
            pollster::block_on(async {
                let result = engine
                    .execute(&pipeline, event.clone())
                    .await
                    .expect("pipeline execute");
                black_box(result);
            });
        });
    });
}

criterion_group!(
    name = pipeline;
    config = Criterion::default().sample_size(50);
    targets = bench_pipeline_3_steps, bench_pipeline_single_step
);
criterion_main!(pipeline);
