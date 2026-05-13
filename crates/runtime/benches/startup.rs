use criterion::{black_box, criterion_group, criterion_main, Criterion};
use config::AgentConfig;
use runtime::AgentRuntimeBuilder;
use std::path::PathBuf;
use std::time::Duration;
use tokio::runtime::Runtime;

fn bench_startup(c: &mut Criterion) {
    let rt = Runtime::new().expect("tokio runtime");

    let mut group = c.benchmark_group("startup");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(60));

    group.bench_function("empty_config", |b| {
        b.iter(|| {
            let dir: PathBuf = std::env::temp_dir().join(format!(
                "aman-startup-bench-{}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&dir);

            let runtime = AgentRuntimeBuilder::new(AgentConfig::default())
                .with_runtime_dir(&dir)
                .build()
                .expect("runtime build");

            rt.block_on(async {
                runtime.start().await.expect("runtime start");
            });

            rt.block_on(async {
                runtime.shutdown().await.expect("runtime shutdown");
            });

            let _ = std::fs::remove_dir_all(&dir);
            black_box(runtime.phase());
        });
    });

    group.finish();
}

criterion_group!(name = startup; config = Criterion::default().sample_size(10); targets = bench_startup);
criterion_main!(startup);
