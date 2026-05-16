use async_trait::async_trait;
use event_bus::{EventBus, EventHandler, InMemoryBus, InMemoryBusConfig, SubscriptionFilter, SubscriptionId};
use kernel::event::Event;
use kernel::types::BackpressureLevel;
use kernel::AmanResult;
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Mutex};

/// Configuration for FakeEventBus backpressure simulation.
#[derive(Debug, Clone)]
pub struct FakeBusConfig {
    pub l1_threshold: usize,
    pub l2_threshold: usize,
    pub l3_threshold: usize,
}

impl Default for FakeBusConfig {
    fn default() -> Self {
        Self {
            l1_threshold: 5,
            l2_threshold: 10,
            l3_threshold: 20,
        }
    }
}

/// An in-memory fake event bus for testing.
///
/// Delegates to a real `InMemoryBus` for subscription management
/// (since `SubscriptionId` has a private constructor), but records
/// all published events for later retrieval and supports configurable
/// backpressure simulation.
pub struct FakeEventBus {
    inner: InMemoryBus,
    published: Arc<Mutex<Vec<Event>>>,
    config: FakeBusConfig,
    queue_depth: AtomicU64,
}

impl FakeEventBus {
    pub fn new(config: FakeBusConfig) -> Self {
        let inner = InMemoryBus::new(InMemoryBusConfig {
            dedup_window_ms: 0,
            backpressure_event_limit: 100,
            ..Default::default()
        });
        Self {
            inner,
            published: Arc::new(Mutex::new(Vec::new())),
            config,
            queue_depth: AtomicU64::new(0),
        }
    }

    /// Return all events published so far.
    pub fn published_events(&self) -> Vec<Event> {
        self.published.lock().unwrap().clone()
    }

    /// Return events matching a predicate.
    pub fn events_matching<F>(&self, pred: F) -> Vec<Event>
    where
        F: Fn(&Event) -> bool,
    {
        self.published
            .lock()
            .unwrap()
            .iter()
            .filter(|e| pred(e))
            .cloned()
            .collect()
    }

    /// Clear all recorded events.
    pub fn clear_events(&self) {
        self.published.lock().unwrap().clear();
        self.queue_depth.store(0, std::sync::atomic::Ordering::SeqCst);
    }

    /// Inject an event directly (for test setup).
    pub fn inject_event(&self, event: Event) {
        self.published.lock().unwrap().push(event);
    }

    fn compute_backpressure(&self, depth: usize) -> BackpressureLevel {
        if depth >= self.config.l3_threshold {
            BackpressureLevel::L3
        } else if depth >= self.config.l2_threshold {
            BackpressureLevel::L2
        } else if depth >= self.config.l1_threshold {
            BackpressureLevel::L1
        } else {
            BackpressureLevel::Normal
        }
    }
}

#[async_trait]
impl EventBus for FakeEventBus {
    async fn publish(&self, event: Event) -> AmanResult<()> {
        let _depth = self.queue_depth.fetch_add(1, std::sync::atomic::Ordering::SeqCst) as usize + 1;
        self.published.lock().unwrap().push(event.clone());
        self.inner.publish(event).await
    }

    async fn subscribe(
        &self,
        filter: SubscriptionFilter,
        handler: Box<dyn EventHandler>,
    ) -> AmanResult<SubscriptionId> {
        self.inner.subscribe(filter, handler).await
    }

    async fn unsubscribe(&self, id: SubscriptionId) {
        self.inner.unsubscribe(id).await;
    }

    fn metrics(&self) -> event_bus::BusMetrics {
        self.inner.metrics()
    }

    fn backpressure_level(&self) -> BackpressureLevel {
        let depth = self.queue_depth.load(std::sync::atomic::Ordering::SeqCst) as usize;
        self.compute_backpressure(depth)
    }

    fn can_poll(&self) -> bool {
        true
    }

    fn try_dequeue(&self) -> Option<Event> {
        self.inner.try_dequeue()
    }

    async fn wait_for_event(
        &self,
        timeout: std::time::Duration,
    ) -> Result<Event, event_bus::WaitForEventTimeout> {
        self.inner.wait_for_event(timeout).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kernel::event::EventType;
    use uuid::Uuid;

    fn make_event(event_type: &str) -> Event {
        Event::new(
            "test",
            EventType::Custom(event_type.to_owned()),
            serde_json::Value::Null,
        )
    }

    #[tokio::test]
    async fn publish_records_event() {
        let bus = FakeEventBus::new(FakeBusConfig::default());
        let event = make_event("test.event");
        bus.publish(event.clone()).await.unwrap();
        let events = bus.published_events();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].id, event.id);
    }

    #[tokio::test]
    async fn backpressure_levels() {
        let cfg = FakeBusConfig {
            l1_threshold: 2,
            l2_threshold: 4,
            l3_threshold: 6,
        };
        let bus = FakeEventBus::new(cfg);

        assert_eq!(bus.backpressure_level(), BackpressureLevel::Normal);

        for _ in 0..2 {
            bus.publish(make_event("t")).await.unwrap();
        }
        assert_eq!(bus.backpressure_level(), BackpressureLevel::L1);

        // 4 events → L2
        for _ in 2..4 {
            bus.publish(make_event("t")).await.unwrap();
        }
        assert_eq!(bus.backpressure_level(), BackpressureLevel::L2);

        // 7 events → L3
        for _ in 4..7 {
            bus.publish(make_event("t")).await.unwrap();
        }
        assert_eq!(bus.backpressure_level(), BackpressureLevel::L3);
    }

    #[tokio::test]
    async fn subscribe_and_unsubscribe() {
        let bus = FakeEventBus::new(FakeBusConfig::default());
        let filter = SubscriptionFilter::default();
        let handler = Box::new(DummyHandler);
        let id = bus.subscribe(filter, handler).await.unwrap();
        bus.unsubscribe(id).await;
    }

    struct DummyHandler;
    #[async_trait]
    impl EventHandler for DummyHandler {
        async fn handle(&self, _event: Event) -> AmanResult<()> {
            Ok(())
        }
    }
}
