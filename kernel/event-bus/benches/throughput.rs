// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

use criterion::{black_box, criterion_group, criterion_main, Criterion};
use event_bus::{EventBus, InMemoryBus, InMemoryBusConfig, SubscriptionFilter};
use kernel::event::{Event, EventType};
use kernel::AmanResult;

struct CountingHandler {
    count: std::sync::atomic::AtomicU64,
}

#[async_trait::async_trait]
impl event_bus::EventHandler for CountingHandler {
    async fn handle(&self, _event: Event) -> AmanResult<()> {
        self.count.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Ok(())
    }
}

fn bench_event_bus_throughput(c: &mut Criterion) {
    let config = InMemoryBusConfig {
        max_queue_size: 100_000,
        ..InMemoryBusConfig::default()
    };

    c.bench_function("event_bus_publish_10k", |b| {
        b.iter(|| {
            pollster::block_on(async {
                let bus = InMemoryBus::new(config.clone());
                for i in 0..10_000i32 {
                    let event = Event::new(
                        "bench:source",
                        EventType::Custom(format!("evt_{i}")),
                        serde_json::json!({"seq": i}),
                    );
                    bus.publish(event).await.expect("publish");
                }
                black_box(bus);
            });
        });
    });
}

fn bench_event_bus_publish_single(c: &mut Criterion) {
    let config = InMemoryBusConfig {
        max_queue_size: 100_000,
        ..InMemoryBusConfig::default()
    };

    c.bench_function("event_bus_publish_single", |b| {
        let bus = InMemoryBus::new(config.clone());
        let event = Event::new(
            "bench:source",
            EventType::Custom("single".into()),
            serde_json::json!({"key": "value"}),
        );
        b.iter(|| {
            pollster::block_on(async {
                bus.publish(event.clone()).await.expect("publish");
                black_box(&bus);
            });
        });
    });
}

fn bench_event_bus_with_subscribers(c: &mut Criterion) {
    let config = InMemoryBusConfig {
        max_queue_size: 100_000,
        ..InMemoryBusConfig::default()
    };

    c.bench_function("event_bus_10_subscribers", |b| {
        let bus = InMemoryBus::new(config.clone());
        // Subscribe 10 handlers
        pollster::block_on(async {
            for _ in 0..10 {
                bus.subscribe(
                    SubscriptionFilter::default(),
                    Box::new(CountingHandler {
                        count: std::sync::atomic::AtomicU64::new(0),
                    }),
                )
                .await
                .expect("subscribe");
            }
        });

        b.iter(|| {
            pollster::block_on(async {
                let event = Event::new(
                    "bench:source",
                    EventType::Custom("msg".into()),
                    serde_json::json!({"n": 1}),
                );
                bus.publish(event).await.expect("publish");
                black_box(&bus);
            });
        });
    });
}

criterion_group!(
    name = event_bus;
    config = Criterion::default().sample_size(50);
    targets = bench_event_bus_throughput, bench_event_bus_publish_single, bench_event_bus_with_subscribers
);
criterion_main!(event_bus);
