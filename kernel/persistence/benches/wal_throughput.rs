// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

use criterion::{black_box, criterion_group, criterion_main, Criterion};
use kernel::event::{Event, EventType};
use persistence::{WalSync, WriteAheadLog};
use serde_json::json;
use std::path::PathBuf;

fn bench_wal_append_batch(c: &mut Criterion) {
    let dir: PathBuf = std::env::temp_dir().join(format!(
        "aman-wal-bench-{}",
        std::process::id()
    ));

    let mut group = c.benchmark_group("wal_throughput");
    group.sample_size(10);

    // Prepare events in advance — smaller set for fsync (slow)
    let events_100: Vec<Event> = (0..100i32)
        .map(|i| {
            Event::new(
                "bench:source",
                EventType::Custom(format!("evt_{i}")),
                json!({"seq": i, "data": "payload to measure serialize overhead for wal benchmark purposes"}),
            )
        })
        .collect();
    let events_1k: Vec<Event> = (0..1_000i32)
        .map(|i| {
            Event::new(
                "bench:source",
                EventType::Custom(format!("evt_{i}")),
                json!({"seq": i, "data": "payload to measure serialize overhead for wal benchmark purposes"}),
            )
        })
        .collect();

    group.bench_function("wal_append_fsync_100", |b| {
        b.iter(|| {
            let _ = std::fs::remove_dir_all(&dir);
            let mut wal = WriteAheadLog::new(&dir, 1024 * 1024 * 1024, WalSync::Fsync)
                .expect("wal init");

            for event in &events_100 {
                wal.append(event.clone()).expect("wal append");
            }

            black_box(wal);
        });
    });

    group.bench_function("wal_append_batch_1k", |b| {
        b.iter(|| {
            let _ = std::fs::remove_dir_all(&dir);
            let mut wal = WriteAheadLog::new(&dir, 1024 * 1024 * 1024, WalSync::Batch)
                .expect("wal init");

            for event in &events_1k {
                wal.append(event.clone()).expect("wal append");
            }

            black_box(wal);
        });
    });

    // Cleanup
    let _ = std::fs::remove_dir_all(&dir);
    group.finish();
}

criterion_group!(name = wal; config = Criterion::default().sample_size(30); targets = bench_wal_append_batch);
criterion_main!(wal);
