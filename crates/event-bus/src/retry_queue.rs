// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

use kernel::event::Event;
use kernel::retry::RetryBackoff;
use kernel::{AmanResult, Error};
use std::cmp::Ordering;
use std::collections::BinaryHeap;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, PartialEq)]
pub struct RetryQueueConfig {
    pub max_entries: usize,
    pub max_attempts: u32,
    pub retry_backoff: RetryBackoff,
}

impl Default for RetryQueueConfig {
    fn default() -> Self {
        Self {
            max_entries: 1_000,
            max_attempts: 5,
            retry_backoff: RetryBackoff::Sequence(vec![100, 500, 2_000]),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct RetryQueueItem {
    pub event: Event,
    pub attempt: u32,
    pub ready_at: Instant,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum RetryScheduleResult {
    Queued { attempt: u32, ready_at: Instant },
    Exhausted { attempt: u32, event: Box<Event> },
}

#[derive(Debug, Default)]
pub struct RetryQueue {
    config: RetryQueueConfig,
    next_seq: u64,
    items: BinaryHeap<ScheduledItem>,
}

impl RetryQueue {
    #[must_use]
    pub fn new(config: RetryQueueConfig) -> Self {
        Self {
            config,
            next_seq: 0,
            items: BinaryHeap::new(),
        }
    }

    pub fn schedule_retry(
        &mut self,
        event: Event,
        previous_attempts: u32,
        last_error: Option<String>,
    ) -> AmanResult<RetryScheduleResult> {
        let attempt = previous_attempts.saturating_add(1);
        if attempt > self.config.max_attempts {
            return Ok(RetryScheduleResult::Exhausted {
                attempt,
                event: Box::new(event),
            });
        }

        if self.len() >= self.config.max_entries {
            return Err(Error::BusFull);
        }

        let delay = retry_delay(&self.config.retry_backoff, attempt);
        let ready_at = Instant::now() + delay;
        let item = RetryQueueItem {
            event,
            attempt,
            ready_at,
            last_error,
        };
        self.push(item);

        Ok(RetryScheduleResult::Queued { attempt, ready_at })
    }

    pub fn push(&mut self, item: RetryQueueItem) {
        let scheduled = ScheduledItem {
            ready_at: item.ready_at,
            seq: self.next_seq,
            item,
        };
        self.next_seq = self.next_seq.wrapping_add(1);
        self.items.push(scheduled);
    }

    #[must_use]
    pub fn pop_ready(&mut self) -> Option<RetryQueueItem> {
        let now = Instant::now();
        if self.items.peek().is_some_and(|item| item.ready_at <= now) {
            return self.items.pop().map(|scheduled| scheduled.item);
        }

        None
    }

    #[must_use]
    pub fn peek_ready_at(&self) -> Option<Instant> {
        self.items.peek().map(|item| item.ready_at)
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.items.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    #[must_use]
    pub fn max_entries(&self) -> usize {
        self.config.max_entries
    }
}

#[derive(Debug)]
struct ScheduledItem {
    ready_at: Instant,
    seq: u64,
    item: RetryQueueItem,
}

impl PartialEq for ScheduledItem {
    fn eq(&self, other: &Self) -> bool {
        self.ready_at == other.ready_at && self.seq == other.seq
    }
}

impl Eq for ScheduledItem {}

impl PartialOrd for ScheduledItem {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for ScheduledItem {
    fn cmp(&self, other: &Self) -> Ordering {
        other
            .ready_at
            .cmp(&self.ready_at)
            .then_with(|| other.seq.cmp(&self.seq))
    }
}

fn retry_delay(backoff: &RetryBackoff, attempt: u32) -> Duration {
    match backoff {
        RetryBackoff::Immediate => Duration::ZERO,
        RetryBackoff::Fixed(delay_ms) => Duration::from_millis(*delay_ms),
        RetryBackoff::Exponential => {
            let shift = attempt.saturating_sub(1).min(8);
            let multiplier = 1_u64 << shift;
            Duration::from_millis((100 * multiplier).min(30_000))
        }
        RetryBackoff::Sequence(delays) => {
            let index = usize::try_from(attempt.saturating_sub(1)).unwrap_or(usize::MAX);
            let delay_ms = delays
                .get(index)
                .copied()
                .unwrap_or_else(|| *delays.last().unwrap_or(&0));
            Duration::from_millis(delay_ms)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{RetryQueue, RetryQueueConfig, RetryQueueItem, RetryScheduleResult};
    use kernel::event::{Event, EventType};
    use kernel::retry::RetryBackoff;
    use kernel::Error;
    use serde_json::json;
    use std::thread;
    use std::time::{Duration, Instant};

    #[test]
    fn schedules_default_sequence_backoff() {
        let mut queue = RetryQueue::new(RetryQueueConfig::default());
        let event = Event::new("wal:a", EventType::MessageReceived, json!({"seq": 1}));
        let started_at = Instant::now();

        let result = queue
            .schedule_retry(event, 0, Some("publish failed".to_owned()))
            .expect("schedule retry");

        match result {
            RetryScheduleResult::Queued { attempt, ready_at } => {
                assert_eq!(attempt, 1);
                assert!(ready_at >= started_at + Duration::from_millis(100));
            }
            RetryScheduleResult::Exhausted { .. } => panic!("first retry should be queued"),
        }
        assert_eq!(queue.len(), 1);
    }

    #[test]
    fn sequence_reuses_last_delay_for_later_attempts() {
        let mut queue = RetryQueue::new(RetryQueueConfig::default());
        let event = Event::new("wal:a", EventType::MessageReceived, json!({"seq": 1}));
        let started_at = Instant::now();

        let result = queue
            .schedule_retry(event, 4, Some("still failing".to_owned()))
            .expect("schedule retry");

        match result {
            RetryScheduleResult::Queued { attempt, ready_at } => {
                assert_eq!(attempt, 5);
                assert!(ready_at >= started_at + Duration::from_millis(2_000));
            }
            RetryScheduleResult::Exhausted { .. } => panic!("fifth retry should still be queued"),
        }
    }

    #[test]
    fn returns_exhausted_after_max_attempts() {
        let mut queue = RetryQueue::new(RetryQueueConfig::default());
        let event = Event::new("wal:a", EventType::MessageReceived, json!({"seq": 1}));

        let result = queue
            .schedule_retry(event.clone(), 5, Some("still failing".to_owned()))
            .expect("schedule retry");

        assert_eq!(
            result,
            RetryScheduleResult::Exhausted {
                attempt: 6,
                event: Box::new(event),
            }
        );
        assert!(queue.is_empty());
    }

    #[test]
    fn rejects_when_queue_is_full() {
        let mut queue = RetryQueue::new(RetryQueueConfig {
            max_entries: 1,
            ..RetryQueueConfig::default()
        });
        queue.push(RetryQueueItem {
            event: Event::new("wal:a", EventType::MessageReceived, json!({"seq": 1})),
            attempt: 1,
            ready_at: Instant::now(),
            last_error: None,
        });

        let error = queue
            .schedule_retry(
                Event::new("wal:b", EventType::MessageReceived, json!({"seq": 2})),
                0,
                Some("queue full".to_owned()),
            )
            .expect_err("queue should reject when full");

        assert!(matches!(error, Error::BusFull));
    }

    #[test]
    fn pop_ready_only_returns_due_items() {
        let mut queue = RetryQueue::new(RetryQueueConfig {
            retry_backoff: RetryBackoff::Immediate,
            ..RetryQueueConfig::default()
        });

        queue
            .schedule_retry(
                Event::new("wal:a", EventType::MessageReceived, json!({"seq": 1})),
                0,
                Some("retry now".to_owned()),
            )
            .expect("schedule immediate retry");
        assert!(queue.pop_ready().is_some());
        assert!(queue.pop_ready().is_none());

        queue.push(RetryQueueItem {
            event: Event::new("wal:b", EventType::MessageReceived, json!({"seq": 2})),
            attempt: 1,
            ready_at: Instant::now() + Duration::from_millis(20),
            last_error: None,
        });
        assert!(queue.pop_ready().is_none());
        thread::sleep(Duration::from_millis(25));
        assert!(queue.pop_ready().is_some());
    }
}
