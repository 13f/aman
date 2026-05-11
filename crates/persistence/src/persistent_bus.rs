use crate::WriteAheadLog;
use event_bus::{EventBus, InMemoryBus};
use kernel::event::Event;
use kernel::retry::RetryBackoff;
use kernel::AmanResult;
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone, PartialEq)]
pub struct PersistentBusConfig {
    pub wal_replay_buffer_max: usize,
    pub wal_retry_backoff: RetryBackoff,
}

impl Default for PersistentBusConfig {
    fn default() -> Self {
        Self {
            wal_replay_buffer_max: 5_000,
            wal_retry_backoff: RetryBackoff::Sequence(vec![100, 500, 2_000]),
        }
    }
}

pub struct PersistentBus {
    bus: Arc<InMemoryBus>,
    wal: Arc<Mutex<WriteAheadLog>>,
    config: PersistentBusConfig,
}

impl PersistentBus {
    #[must_use]
    pub fn new(bus: Arc<InMemoryBus>, wal: WriteAheadLog) -> Self {
        Self::with_config(bus, wal, PersistentBusConfig::default())
    }

    #[must_use]
    pub fn with_config(
        bus: Arc<InMemoryBus>,
        wal: WriteAheadLog,
        config: PersistentBusConfig,
    ) -> Self {
        Self {
            bus,
            wal: Arc::new(Mutex::new(wal)),
            config,
        }
    }

    pub async fn publish(&self, event: Event) -> AmanResult<()> {
        {
            let mut wal = self
                .wal
                .lock()
                .expect("persistent bus wal mutex should not be poisoned");
            let _ = wal.append(event.clone())?;
        }

        match self.bus.publish(event.clone()).await {
            Ok(()) => Ok(()),
            Err(error) => {
                let _ = self
                    .bus
                    .enqueue_for_retry(event, 0, Some(error.to_string()))?;
                Err(error)
            }
        }
    }

    pub fn checkpoint(&self, offset: u64) -> AmanResult<()> {
        self.wal
            .lock()
            .expect("persistent bus wal mutex should not be poisoned")
            .checkpoint(offset)
    }

    #[must_use]
    pub fn checkpoint_offset(&self) -> u64 {
        self.wal
            .lock()
            .expect("persistent bus wal mutex should not be poisoned")
            .checkpoint_offset()
    }

    pub async fn recover_from_wal(&self) -> AmanResult<usize> {
        let events = self
            .wal
            .lock()
            .expect("persistent bus wal mutex should not be poisoned")
            .replay_from_checkpoint_with_limit(self.config.wal_replay_buffer_max)?;
        let mut recovered = 0;
        for event in events {
            match self.bus.publish(event.clone()).await {
                Ok(()) => recovered += 1,
                Err(error) => {
                    let _result = self
                        .bus
                        .enqueue_for_retry(event, 0, Some(error.to_string()))?;
                }
            }
        }
        Ok(recovered)
    }

    pub fn recover_from_overflow(&self) -> AmanResult<usize> {
        self.bus.recover_overflow()
    }

    #[must_use]
    pub fn retry_queue_depth(&self) -> usize {
        self.bus.metrics().retry_queue_depth
    }

    #[must_use]
    pub fn bus(&self) -> Arc<InMemoryBus> {
        Arc::clone(&self.bus)
    }

    #[must_use]
    pub fn wal(&self) -> Arc<Mutex<WriteAheadLog>> {
        Arc::clone(&self.wal)
    }
}

#[cfg(test)]
mod tests {
    use super::PersistentBus;
    use crate::{OverflowDir, PersistentBusConfig, WalSync, WriteAheadLog};
    use event_bus::{EventBus, EventHandler, InMemoryBus, InMemoryBusConfig, SubscriptionFilter};
    use kernel::event::{Event, EventType};
    use kernel::{AmanResult, Error};
    use serde_json::json;
    use std::fs;
    use std::sync::{Arc, Mutex};
    use std::time::{SystemTime, UNIX_EPOCH};

    #[derive(Default)]
    struct OkHandler;

    #[async_trait::async_trait]
    impl EventHandler for OkHandler {
        async fn handle(&self, _event: Event) -> AmanResult<()> {
            Ok(())
        }
    }

    #[derive(Default)]
    struct FailHandler;

    #[async_trait::async_trait]
    impl EventHandler for FailHandler {
        async fn handle(&self, _event: Event) -> AmanResult<()> {
            Err(Error::Unrecoverable {
                message: "forced failure".to_owned(),
            })
        }
    }

    #[derive(Default)]
    struct RecordingHandler {
        events: Arc<Mutex<Vec<Event>>>,
    }

    #[async_trait::async_trait]
    impl EventHandler for RecordingHandler {
        async fn handle(&self, event: Event) -> AmanResult<()> {
            self.events
                .lock()
                .expect("recording handler lock")
                .push(event);
            Ok(())
        }
    }

    fn test_dir(name: &str) -> std::path::PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock should be after unix epoch")
            .as_nanos();
        std::env::temp_dir().join(format!("aman-persistent-bus-{name}-{nanos}"))
    }

    #[test]
    fn publish_writes_to_wal_before_delivery() {
        pollster::block_on(async {
            let dir = test_dir("publish");
            let bus = Arc::new(InMemoryBus::default());
            bus.subscribe(SubscriptionFilter::default(), Box::new(OkHandler))
                .await
                .expect("subscribe");
            let wal = WriteAheadLog::new(&dir, 1024 * 1024, WalSync::Batch).expect("create wal");
            let persistent = PersistentBus::new(bus, wal);

            persistent
                .publish(Event::new(
                    "persistent:test",
                    EventType::MessageReceived,
                    json!({"seq": 1}),
                ))
                .await
                .expect("publish should succeed");

            let replayed = persistent
                .wal()
                .lock()
                .expect("wal lock")
                .replay_from_checkpoint()
                .expect("replay");
            assert_eq!(replayed.len(), 1);
            let _ = fs::remove_dir_all(&dir);
        });
    }

    #[test]
    fn failed_delivery_is_enqueued_for_retry_after_wal_append() {
        pollster::block_on(async {
            let dir = test_dir("retry");
            let bus = Arc::new(InMemoryBus::new(InMemoryBusConfig {
                max_queue_size: 1,
                ..InMemoryBusConfig::default()
            }));
            bus.subscribe(SubscriptionFilter::default(), Box::new(FailHandler))
                .await
                .expect("subscribe failing handler");

            let wal = WriteAheadLog::new(&dir, 1024 * 1024, WalSync::Batch).expect("create wal");
            let persistent = PersistentBus::new(Arc::clone(&bus), wal);
            let result = persistent
                .publish(Event::new(
                    "persistent:test",
                    EventType::MessageReceived,
                    json!({"seq": 2}),
                ))
                .await;
            assert!(result.is_err());
            assert_eq!(persistent.retry_queue_depth(), 1);
            let _ = fs::remove_dir_all(&dir);
        });
    }

    #[test]
    fn recover_from_wal_uses_replay_buffer_limit() {
        pollster::block_on(async {
            let dir = test_dir("wal-limit");
            let bus = Arc::new(InMemoryBus::default());
            bus.subscribe(SubscriptionFilter::default(), Box::new(OkHandler))
                .await
                .expect("subscribe");
            let mut wal = WriteAheadLog::new(&dir, 1024 * 1024, WalSync::Batch).expect("create wal");
            let _ = wal.append(Event::new("recovery", EventType::MessageReceived, json!({"seq": 1})));
            let _ = wal.append(Event::new("recovery", EventType::MessageReceived, json!({"seq": 2})));
            let _ = wal.append(Event::new("recovery", EventType::MessageReceived, json!({"seq": 3})));
            let persistent = PersistentBus::with_config(
                Arc::clone(&bus),
                wal,
                PersistentBusConfig {
                    wal_replay_buffer_max: 2,
                    ..PersistentBusConfig::default()
                },
            );

            let recovered = persistent.recover_from_wal().await.expect("recover from wal");
            assert_eq!(recovered, 2);
            let _ = fs::remove_dir_all(&dir);
        });
    }

    #[test]
    fn recover_from_overflow_reinjects_events() {
        pollster::block_on(async {
            let overflow_dir = test_dir("overflow-recover");
            let bus = Arc::new(InMemoryBus::new(InMemoryBusConfig {
                max_queue_size: 100,
                overflow_dir: Some(overflow_dir.clone()),
                overflow_max_bytes: 1024 * 1024,
                ..InMemoryBusConfig::default()
            }));
            let wal = WriteAheadLog::new(test_dir("overflow-recover-wal"), 1024 * 1024, WalSync::Batch)
                .expect("create wal");
            let persistent = PersistentBus::new(Arc::clone(&bus), wal);

            let overflow = OverflowDir::new(&overflow_dir, 1024 * 1024).expect("create overflow");
            let overflow_event =
                Event::new("overflow", EventType::MessageReceived, json!({"overflow": true}));
            overflow
                .write_event(&overflow_event)
                .expect("write overflow event");

            let recovered = persistent
                .recover_from_overflow()
                .expect("recover from overflow");
            assert_eq!(recovered, 1);
            let _ = fs::remove_dir_all(&overflow_dir);
        });
    }

    #[test]
    fn crash_recovery_replays_uncheckpointed_wal_events() {
        pollster::block_on(async {
            let wal_dir = test_dir("crash-recovery");
            let bus = Arc::new(InMemoryBus::default());
            let wal = WriteAheadLog::new(&wal_dir, 1024 * 1024, WalSync::Batch).expect("create wal");
            let persistent = PersistentBus::new(Arc::clone(&bus), wal);

            persistent
                .publish(Event::new(
                    "recovery",
                    EventType::MessageReceived,
                    json!({"seq": 1}),
                ))
                .await
                .expect("publish first");
            persistent
                .publish(Event::new(
                    "recovery",
                    EventType::MessageReceived,
                    json!({"seq": 2}),
                ))
                .await
                .expect("publish second");

            // Simulate restart: new in-memory bus + same WAL dir.
            let restarted_bus = Arc::new(InMemoryBus::default());
            let recorder = RecordingHandler::default();
            let recorded = Arc::clone(&recorder.events);
            restarted_bus
                .subscribe(SubscriptionFilter::default(), Box::new(recorder))
                .await
                .expect("subscribe recorder");
            let restarted_wal = WriteAheadLog::new(&wal_dir, 1024 * 1024, WalSync::Batch)
                .expect("reopen wal");
            let restarted = PersistentBus::new(restarted_bus, restarted_wal);

            let recovered = restarted
                .recover_from_wal()
                .await
                .expect("recover from wal");
            assert_eq!(recovered, 2);
            assert_eq!(recorded.lock().expect("recorded lock").len(), 2);
            let _ = fs::remove_dir_all(&wal_dir);
        });
    }
}
