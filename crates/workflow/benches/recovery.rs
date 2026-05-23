// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

use criterion::{black_box, criterion_group, criterion_main, Criterion};
use serde_json::json;
use workflow::{ErrorRecovery, StateDef, WorkflowDef, WorkflowEngine};

fn bench_workflow_recovery(c: &mut Criterion) {
    let mut group = c.benchmark_group("workflow_recovery");
    group.sample_size(10);

    // Set up engine with one workflow definition
    let engine = WorkflowEngine::new();
    let workflow = WorkflowDef {
        name: "approval".to_owned(),
        states: vec![
            StateDef { name: "pending".to_owned() },
            StateDef { name: "approved".to_owned() },
            StateDef { name: "rejected".to_owned() },
            StateDef { name: "error".to_owned() },
        ],
        initial_state: "pending".to_owned(),
        final_states: vec!["approved".to_owned(), "rejected".to_owned()],
        error_state: "error".to_owned(),
        transitions: Vec::new(),
        state_timeouts: Vec::new(),
        error_recovery: ErrorRecovery::default(),
    };
    engine.register_workflow(workflow).expect("register workflow");

    // Create 10K instances
    for i in 0..10_000i32 {
        engine
            .create_instance("approval", json!({"seq": i, "ticket": format!("T-{i}")}))
            .expect("create instance");
    }

    group.bench_function("list_10k_instances", |b| {
        b.iter(|| {
            let instances = engine.list_instances();
            black_box(instances.len());
        });
    });

    group.bench_function("create_and_list_10k", |b| {
        b.iter(|| {
            let eng = WorkflowEngine::new();
            let wf = WorkflowDef {
                name: "bench".to_owned(),
                states: vec![
                    StateDef { name: "pending".to_owned() },
                    StateDef { name: "done".to_owned() },
                    StateDef { name: "error".to_owned() },
                ],
                initial_state: "pending".to_owned(),
                final_states: vec!["done".to_owned()],
                error_state: "error".to_owned(),
                transitions: Vec::new(),
                state_timeouts: Vec::new(),
                error_recovery: ErrorRecovery::default(),
            };
            eng.register_workflow(wf).expect("register");
            for j in 0..10_000i32 {
                eng.create_instance("bench", json!({"seq": j}))
                    .expect("create instance");
            }
            let instances = eng.list_instances();
            black_box(instances.len());
        });
    });

    group.finish();
}

criterion_group!(name = recovery; config = Criterion::default().sample_size(10); targets = bench_workflow_recovery);
criterion_main!(recovery);
