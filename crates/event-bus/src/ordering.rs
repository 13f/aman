use crate::QueueDepth;
use kernel::event::Event;
use kernel::types::{Priority, SourceId};
use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap, VecDeque};

#[derive(Debug, Clone)]
struct QueuedEvent {
    event: Event,
    enqueue_seq: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct QueueHead {
    source: SourceId,
    priority: Priority,
    enqueue_seq: u64,
}

impl Ord for QueueHead {
    fn cmp(&self, other: &Self) -> Ordering {
        priority_rank(self.priority)
            .cmp(&priority_rank(other.priority))
            .then_with(|| other.enqueue_seq.cmp(&self.enqueue_seq))
            .then_with(|| other.source.as_str().cmp(self.source.as_str()))
    }
}

impl PartialOrd for QueueHead {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

#[derive(Debug, Default)]
pub struct OrderedQueue {
    segments: HashMap<SourceId, VecDeque<QueuedEvent>>,
    heads: BinaryHeap<QueueHead>,
    next_enqueue_seq: u64,
    len: usize,
    depth: QueueDepth,
}

impl OrderedQueue {
    pub fn push(&mut self, event: Event) {
        let source = event.source.clone();
        let queued = QueuedEvent {
            event,
            enqueue_seq: self.next_enqueue_seq,
        };
        let queued_priority = queued.event.priority;
        self.next_enqueue_seq += 1;

        let segment = self.segments.entry(source.clone()).or_default();
        let needs_head = segment.is_empty();
        segment.push_back(queued);

        let head = segment.front().expect("queue segment has head");
        if needs_head {
            self.heads.push(QueueHead {
                source,
                priority: head.event.priority,
                enqueue_seq: head.enqueue_seq,
            });
        }

        increment_depth(&mut self.depth, queued_priority);
        self.len += 1;
    }

    pub fn pop(&mut self) -> Option<Event> {
        let candidate = self.heads.pop()?;
        let mut remove_segment = false;

        let event = {
            let segment = self
                .segments
                .get_mut(&candidate.source)
                .expect("queue head source must exist");
            let queued = segment
                .pop_front()
                .expect("queue head segment must not be empty");
            if let Some(next_head) = segment.front() {
                self.heads.push(QueueHead {
                    source: candidate.source.clone(),
                    priority: next_head.event.priority,
                    enqueue_seq: next_head.enqueue_seq,
                });
            } else {
                remove_segment = true;
            }
            queued.event
        };

        if remove_segment {
            self.segments.remove(&candidate.source);
        }

        decrement_depth(&mut self.depth, event.priority);
        self.len -= 1;
        Some(event)
    }

    #[must_use]
    pub const fn len(&self) -> usize {
        self.len
    }

    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    #[must_use]
    pub const fn depth_by_priority(&self) -> QueueDepth {
        self.depth
    }
}

const fn priority_rank(priority: Priority) -> u8 {
    match priority {
        Priority::High => 2,
        Priority::Normal => 1,
        Priority::Low => 0,
    }
}

fn increment_depth(depth: &mut QueueDepth, priority: Priority) {
    match priority {
        Priority::High => depth.high += 1,
        Priority::Normal => depth.normal += 1,
        Priority::Low => depth.low += 1,
    }
}

fn decrement_depth(depth: &mut QueueDepth, priority: Priority) {
    match priority {
        Priority::High => depth.high -= 1,
        Priority::Normal => depth.normal -= 1,
        Priority::Low => depth.low -= 1,
    }
}
