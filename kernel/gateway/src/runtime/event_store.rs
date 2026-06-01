// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

use kernel::event::Event;
use std::collections::{HashMap, HashSet, VecDeque};
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
    /// trace_prev → set of trace_ids that reference it as predecessor.
    /// Built on the fly from event payloads; used by trace_chain()
    /// to find descendant traces.
    trace_children: HashMap<String, HashSet<String>>,
}

impl EventStore {
    #[must_use]
    pub fn new(cap: usize, per_trace_cap: usize) -> Self {
        Self {
            inner: Mutex::new(EventStoreInner {
                order: VecDeque::new(),
                by_id: HashMap::new(),
                trace_index: HashMap::new(),
                trace_children: HashMap::new(),
            }),
            cap: cap.max(1),
            per_trace_cap: per_trace_cap.max(1),
        }
    }

    pub fn record(&self, event: Event) {
        let id = event.id.to_string();
        let trace_id = event.metadata.trace_id.to_string();
        let trace_prev = event
            .payload
            .get("trace_prev")
            .and_then(|v| v.as_str())
            .map(|s| s.to_owned());

        let mut inner = self.inner.lock().expect("event store mutex");
        inner.order.push_back(id.clone());
        inner.by_id.insert(id.clone(), event);
        inner
            .trace_index
            .entry(trace_id.clone())
            .or_default()
            .push_back(id.clone());

        // Index child traces for trace_chain queries.
        if let Some(prev) = trace_prev {
            inner
                .trace_children
                .entry(prev)
                .or_default()
                .insert(trace_id);
        }

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
    /// Return all events in the trace chain for a given trace ID.
    ///
    /// A trace chain includes:
    /// - All events with the given `trace_id`
    /// - All events reachable by following `trace_prev` backward (ancestors)
    /// - All events reachable by following `trace_children` forward (descendants)
    ///
    /// The result is unordered and deduplicated. Returns an empty `Vec` when
    /// the trace_id has no events and no known relations in the store.
    pub fn trace_chain(&self, trace_id: &str) -> Vec<Event> {
        let inner = self.inner.lock().expect("event store mutex");

        let mut seen: HashSet<String> = HashSet::new();
        let mut result: Vec<Event> = Vec::new();
        let mut queue: VecDeque<String> = VecDeque::new();
        queue.push_back(trace_id.to_owned());

        // BFS over trace_id nodes following prev/children links.
        while let Some(current) = queue.pop_front() {
            if !seen.insert(current.clone()) {
                continue;
            }

            // Add events with this trace_id.
            if let Some(ids) = inner.trace_index.get(&current) {
                for event_id in ids.iter() {
                    if let Some(event) = inner.by_id.get(event_id) {
                        result.push(event.clone());
                    }
                }
            }

            // Follow trace_prev backward (ancestor).
            if let Some(ids) = inner.trace_index.get(&current) {
                for event_id in ids.iter() {
                    if let Some(event) = inner.by_id.get(event_id)
                        && let Some(prev) = event
                            .payload
                            .get("trace_prev")
                            .and_then(|v| v.as_str())
                        {
                            queue.push_back(prev.to_owned());
                        }
                }
            }

            // Follow trace_children forward (descendants).
            if let Some(children) = inner.trace_children.get(&current) {
                for child in children.iter() {
                    queue.push_back(child.clone());
                }
            }
        }

        result
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

