use async_trait::async_trait;
use event_bus::{EventBus, EventHandler, InMemoryBus, SubscriptionFilter};
use kernel::event::{Event, EventType};
use kernel::types::Priority;
use kernel::AmanResult;
use serde_json::json;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};

#[derive(Default)]
struct BlockingRecordingHandler {
    events: Mutex<Vec<Event>>,
    blocked_once: AtomicBool,
    gate: Mutex<GateState>,
    gate_changed: Condvar,
}

#[derive(Default)]
struct GateState {
    waiting: bool,
    released: bool,
}

impl BlockingRecordingHandler {
    fn snapshot(&self) -> Vec<Event> {
        self.events.lock().expect("handler mutex").clone()
    }

    fn wait_until_first_delivery_is_blocked(&self) {
        let mut gate = self.gate.lock().expect("gate mutex");
        while !gate.waiting {
            gate = self.gate_changed.wait(gate).expect("wait on gate");
        }
    }

    fn release_first_delivery(&self) {
        let mut gate = self.gate.lock().expect("gate mutex");
        gate.released = true;
        self.gate_changed.notify_all();
    }
}

struct SharedHandler(Arc<BlockingRecordingHandler>);

#[async_trait]
impl EventHandler for SharedHandler {
    async fn handle(&self, event: Event) -> AmanResult<()> {
        self.0.events.lock().expect("handler mutex").push(event);
        if !self.0.blocked_once.swap(true, Ordering::AcqRel) {
            let mut gate = self.0.gate.lock().expect("gate mutex");
            gate.waiting = true;
            self.0.gate_changed.notify_all();
            while !gate.released {
                gate = self.0.gate_changed.wait(gate).expect("wait on gate");
            }
        }
        Ok(())
    }
}

#[test]
fn preserves_fifo_for_same_source_via_public_api() {
    pollster::block_on(async {
        let bus = Arc::new(InMemoryBus::default());
        let handler = Arc::new(BlockingRecordingHandler::default());

        bus.subscribe(
            SubscriptionFilter::default(),
            Box::new(SharedHandler(Arc::clone(&handler))),
        )
        .await
        .expect("subscribe");

        let mut first = Event::new(
            "source:shared",
            EventType::MessageReceived,
            json!({"seq": 1}),
        );
        first.priority = Priority::Low;
        let mut second = Event::new(
            "source:shared",
            EventType::MessageReceived,
            json!({"seq": 2}),
        );
        second.priority = Priority::High;
        let mut third = Event::new(
            "source:shared",
            EventType::MessageReceived,
            json!({"seq": 3}),
        );
        third.priority = Priority::Normal;

        let bus_for_first = Arc::clone(&bus);
        let first_publish =
            std::thread::spawn(move || pollster::block_on(bus_for_first.publish(first)));

        handler.wait_until_first_delivery_is_blocked();

        bus.publish(second).await.expect("publish second");
        bus.publish(third).await.expect("publish third");
        handler.release_first_delivery();

        first_publish
            .join()
            .expect("first publish thread should join")
            .expect("publish first");

        let payloads = handler
            .snapshot()
            .into_iter()
            .map(|event| event.payload)
            .collect::<Vec<_>>();

        assert_eq!(
            payloads,
            vec![json!({"seq": 1}), json!({"seq": 2}), json!({"seq": 3})]
        );
    });
}

#[test]
fn lets_higher_priority_other_source_compete_without_breaking_same_source_fifo() {
    pollster::block_on(async {
        let bus = Arc::new(InMemoryBus::default());
        let handler = Arc::new(BlockingRecordingHandler::default());

        bus.subscribe(
            SubscriptionFilter::default(),
            Box::new(SharedHandler(Arc::clone(&handler))),
        )
        .await
        .expect("subscribe");

        let mut source_a_first =
            Event::new("source:a", EventType::MessageReceived, json!({"id": "a1"}));
        source_a_first.priority = Priority::Low;
        let mut source_b = Event::new("source:b", EventType::MessageReceived, json!({"id": "b1"}));
        source_b.priority = Priority::High;
        let mut source_a_second =
            Event::new("source:a", EventType::MessageReceived, json!({"id": "a2"}));
        source_a_second.priority = Priority::High;

        let bus_for_first = Arc::clone(&bus);
        let first_publish =
            std::thread::spawn(move || pollster::block_on(bus_for_first.publish(source_a_first)));

        handler.wait_until_first_delivery_is_blocked();

        bus.publish(source_b).await.expect("publish b1");
        bus.publish(source_a_second).await.expect("publish a2");
        handler.release_first_delivery();

        first_publish
            .join()
            .expect("first publish thread should join")
            .expect("publish a1");

        let ids = handler
            .snapshot()
            .into_iter()
            .map(|event| event.payload["id"].as_str().expect("payload id").to_owned())
            .collect::<Vec<_>>();

        assert_eq!(ids, vec!["a1".to_owned(), "b1".to_owned(), "a2".to_owned()]);
    });
}
