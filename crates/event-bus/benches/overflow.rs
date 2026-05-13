use criterion::{black_box, criterion_group, criterion_main, Criterion};
use event_bus::{EventBus, InMemoryBus, InMemoryBusConfig};
use kernel::event::{Event, EventType};
use pollster::block_on;
use serde_json::json;
use std::path::PathBuf;

fn bench_overflow_100k(c: &mut Criterion) {
    let dir: PathBuf = std::env::temp_dir().join(format!(
        "aman-overflow-bench-{}",
        std::process::id()
    ));

    let mut group = c.benchmark_group("overflow");
    group.sample_size(10);

    // Pre-allocate events outside the measured section
    let events_100k: Vec<Event> = (0..100_000i32)
        .map(|i| {
            Event::new(
                "bench:overflow",
                EventType::Custom(format!("evt_{i}")),
                json!({"seq": i, "data": "payload for overflow throughput benchmark"}),
            )
        })
        .collect();

    group.bench_function("overflow_100k_spill_to_disk", |b| {
        b.iter(|| {
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).expect("create overflow dir");

            let config = InMemoryBusConfig {
                max_queue_size: 1_000,
                overflow_dir: Some(dir.clone()),
                overflow_max_bytes: 1_073_741_824, // 1 GB
                ..InMemoryBusConfig::default()
            };
            let bus = InMemoryBus::new(config);

            for event in &events_100k {
                block_on(bus.publish(event.clone())).expect("publish");
            }

            let metrics = bus.metrics();
            black_box(metrics);
        });
    });

    let _ = std::fs::remove_dir_all(&dir);
    group.finish();
}

criterion_group!(name = overflow; config = Criterion::default().sample_size(10); targets = bench_overflow_100k);
criterion_main!(overflow);
