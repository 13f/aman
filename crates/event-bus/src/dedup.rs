// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

use kernel::event::Event;
use kernel::types::DedupKey;
use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DedupOutcome {
    Accepted,
    Duplicate,
}

#[derive(Debug)]
pub struct DedupWindow {
    bloom: BloomFilter,
    recent: LruCache,
    window: Duration,
}

impl DedupWindow {
    #[must_use]
    pub fn new(window_ms: u64, recent_capacity: usize) -> Self {
        Self {
            bloom: BloomFilter::new(recent_capacity.max(64) * 8),
            recent: LruCache::new(recent_capacity.max(64)),
            window: Duration::from_millis(window_ms),
        }
    }

    pub fn check(&mut self, event: &Event) -> DedupOutcome {
        let Some(key) = event.dedup_key.clone() else {
            return DedupOutcome::Accepted;
        };

        let now = Instant::now();
        self.recent.evict_expired(now, self.window);

        if self.bloom.may_contain(&key) && self.recent.contains(&key, now, self.window) {
            return DedupOutcome::Duplicate;
        }

        self.bloom.insert(&key);
        self.recent.insert(key, now);
        DedupOutcome::Accepted
    }
}

#[derive(Debug)]
struct BloomFilter {
    words: Vec<u64>,
}

impl BloomFilter {
    fn new(bit_count: usize) -> Self {
        let word_count = bit_count.div_ceil(64);
        Self {
            words: vec![0; word_count.max(1)],
        }
    }

    fn insert(&mut self, key: &DedupKey) {
        for index in bloom_indexes(key, self.bit_count()) {
            let word_index = index / 64;
            let bit_index = index % 64;
            self.words[word_index] |= 1_u64 << bit_index;
        }
    }

    fn may_contain(&self, key: &DedupKey) -> bool {
        bloom_indexes(key, self.bit_count())
            .into_iter()
            .all(|index| {
                let word_index = index / 64;
                let bit_index = index % 64;
                (self.words[word_index] & (1_u64 << bit_index)) != 0
            })
    }

    fn bit_count(&self) -> usize {
        self.words.len() * 64
    }
}

#[derive(Debug)]
struct LruCache {
    capacity: usize,
    entries: HashMap<DedupKey, RecentEntry>,
    order: VecDeque<DedupKey>,
    next_touch: u64,
}

impl LruCache {
    fn new(capacity: usize) -> Self {
        Self {
            capacity: capacity.max(1),
            entries: HashMap::new(),
            order: VecDeque::new(),
            next_touch: 0,
        }
    }

    fn contains(&mut self, key: &DedupKey, now: Instant, window: Duration) -> bool {
        self.evict_expired(now, window);
        let touch = self.bump_touch();

        let Some(entry) = self.entries.get_mut(key) else {
            return false;
        };

        if now.duration_since(entry.inserted_at) >= window {
            self.entries.remove(key);
            return false;
        }

        entry.last_touch = touch;
        self.order.push_back(key.clone());
        true
    }

    fn insert(&mut self, key: DedupKey, now: Instant) {
        let touch = self.bump_touch();
        self.entries.insert(
            key.clone(),
            RecentEntry {
                inserted_at: now,
                last_touch: touch,
            },
        );
        self.order.push_back(key);
        self.evict_over_capacity();
    }

    fn evict_expired(&mut self, now: Instant, window: Duration) {
        while let Some(key) = self.order.front().cloned() {
            let Some(entry) = self.entries.get(&key) else {
                self.order.pop_front();
                continue;
            };

            if now.duration_since(entry.inserted_at) < window && self.entries.len() <= self.capacity
            {
                break;
            }

            let entry = *entry;
            self.order.pop_front();
            if self
                .entries
                .get(&key)
                .is_some_and(|current| current.last_touch == entry.last_touch)
                && (now.duration_since(entry.inserted_at) >= window
                    || self.entries.len() > self.capacity)
            {
                self.entries.remove(&key);
            }
        }
    }

    fn evict_over_capacity(&mut self) {
        while self.entries.len() > self.capacity {
            let Some(key) = self.order.pop_front() else {
                break;
            };
            let Some(entry) = self.entries.get(&key).copied() else {
                continue;
            };
            if self
                .entries
                .get(&key)
                .is_some_and(|current| current.last_touch == entry.last_touch)
            {
                self.entries.remove(&key);
            }
        }
    }

    fn bump_touch(&mut self) -> u64 {
        let value = self.next_touch;
        self.next_touch = self.next_touch.wrapping_add(1);
        value
    }
}

#[derive(Debug, Clone, Copy)]
struct RecentEntry {
    inserted_at: Instant,
    last_touch: u64,
}

fn bloom_indexes(key: &DedupKey, bit_count: usize) -> [usize; 3] {
    let hash = blake3::hash(key.as_str().as_bytes());
    let bytes = hash.as_bytes();

    [
        usize::from(u16::from_le_bytes([bytes[0], bytes[1]])) % bit_count,
        usize::from(u16::from_le_bytes([bytes[10], bytes[11]])) % bit_count,
        usize::from(u16::from_le_bytes([bytes[20], bytes[21]])) % bit_count,
    ]
}

#[cfg(test)]
mod tests {
    use super::{DedupOutcome, DedupWindow};
    use kernel::event::{Event, EventType};
    use kernel::types::{DedupKey, DeliveryGuarantee};
    use serde_json::json;
    use std::thread;
    use std::time::Duration;

    #[test]
    fn duplicate_key_is_rejected_inside_window() {
        let mut dedup = DedupWindow::new(1_000, 64);
        let mut first = Event::new("watch:a", EventType::FileCreated, json!({"name": "a.txt"}));
        first.dedup_key = Some(DedupKey::new("file:a"));
        let mut duplicate = Event::new("watch:a", EventType::FileCreated, json!({"name": "a.txt"}));
        duplicate.dedup_key = Some(DedupKey::new("file:a"));

        assert_eq!(dedup.check(&first), DedupOutcome::Accepted);
        assert_eq!(dedup.check(&duplicate), DedupOutcome::Duplicate);
    }

    #[test]
    fn distinct_key_is_not_rejected_when_bloom_matches() {
        let mut dedup = DedupWindow::new(1_000, 1);
        let mut first = Event::new("watch:a", EventType::FileCreated, json!({"name": "a.txt"}));
        first.dedup_key = Some(DedupKey::new("file:a"));
        let mut second = Event::new("watch:a", EventType::FileCreated, json!({"name": "b.txt"}));
        second.dedup_key = Some(DedupKey::new("file:b"));

        assert_eq!(dedup.check(&first), DedupOutcome::Accepted);
        assert_eq!(dedup.check(&second), DedupOutcome::Accepted);
    }

    #[test]
    fn key_expires_after_window() {
        let mut dedup = DedupWindow::new(10, 64);
        let mut event = Event::new("watch:a", EventType::FileCreated, json!({"name": "a.txt"}));
        event.dedup_key = Some(DedupKey::new("file:a"));

        assert_eq!(dedup.check(&event), DedupOutcome::Accepted);
        thread::sleep(Duration::from_millis(20));
        assert_eq!(dedup.check(&event), DedupOutcome::Accepted);
    }

    #[test]
    fn events_without_dedup_key_are_accepted() {
        let mut dedup = DedupWindow::new(1_000, 64);
        let event = Event::new("timer:a", EventType::Heartbeat, json!({}))
            .with_delivery(DeliveryGuarantee::AtMostOnce);

        assert_eq!(dedup.check(&event), DedupOutcome::Accepted);
        assert_eq!(dedup.check(&event), DedupOutcome::Accepted);
    }
}
