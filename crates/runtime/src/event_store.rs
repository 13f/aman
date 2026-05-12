use kernel::event::Event;
use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;

pub struct EventStore {
    inner: Mutex<EventStoreInner>,
    cap: usize,
    per_trace_cap: usize,
}

struct EventStoreInner {
    order: VecDeque<String>,
    by_id: HashMap<String, Event>,
    trace_index: HashMap<String, VecDeque<String>>,
}

impl EventStore {
    #[must_use]
    pub fn new(cap: usize, per_trace_cap: usize) -> Self {
        Self {
            inner: Mutex::new(EventStoreInner {
                order: VecDeque::new(),
                by_id: HashMap::new(),
                trace_index: HashMap::new(),
            }),
            cap: cap.max(1),
            per_trace_cap: per_trace_cap.max(1),
        }
    }

    pub fn record(&self, event: Event) {
        let id = event.id.to_string();
        let trace_id = event.metadata.trace_id.to_string();

        let mut inner = self.inner.lock().expect("event store mutex");
        inner.order.push_back(id.clone());
        inner.by_id.insert(id.clone(), event);
        inner
            .trace_index
            .entry(trace_id)
            .or_default()
            .push_back(id);

        while inner.order.len() > self.cap {
            if let Some(evicted) = inner.order.pop_front() {
                inner.by_id.remove(&evicted);
            }
        }

        for ids in inner.trace_index.values_mut() {
            while ids.len() > self.per_trace_cap {
                ids.pop_front();
            }
        }
    }

    #[must_use]
    pub fn get(&self, id: &str) -> Option<Event> {
        self.inner
            .lock()
            .expect("event store mutex")
            .by_id
            .get(id)
            .cloned()
    }

    #[must_use]
    pub fn trace(&self, trace_id: &str) -> Vec<Event> {
        let inner = self.inner.lock().expect("event store mutex");
        let Some(ids) = inner.trace_index.get(trace_id) else {
            return Vec::new();
        };
        ids.iter()
            .filter_map(|id| inner.by_id.get(id).cloned())
            .collect()
    }

    /// Returns the most recent events, up to `count`.
    #[must_use]
    pub fn recent(&self, count: usize) -> Vec<Event> {
        let inner = self.inner.lock().expect("event store mutex");
        inner
            .order
            .iter()
            .rev()
            .take(count)
            .filter_map(|id| inner.by_id.get(id).cloned())
            .collect()
    }
}

