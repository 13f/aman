use kernel::event::Event;
use kernel::types::{BackpressureLevel, DeliveryGuarantee, Priority};
use std::collections::VecDeque;

/// Returns `true` if the event should be routed to overflow disk at the
/// given backpressure level (Level 4A: 98% queue capacity for guaranteed
/// delivery events).
#[must_use]
pub fn should_overflow_to_disk(event: &Event, level: BackpressureLevel) -> bool {
    matches!(level, BackpressureLevel::L4A)
        && matches!(
            event.delivery,
            DeliveryGuarantee::AtLeastOnce | DeliveryGuarantee::ExactlyOnce
        )
}

/// Returns `true` when the overflow directory usage ratio exceeds the
/// configured threshold, triggering Level 4B emergency fallback.
#[must_use]
pub fn is_overflow_dir_emergency(overflow_usage_ratio: f32, threshold: f32) -> bool {
    overflow_usage_ratio >= threshold
}

/// Returns `true` when Level 5 (Critical / 100% full) is active and the
/// event has low priority — indicating the event source should be paused
/// or the event discarded.
#[must_use]
pub fn should_stop_low_priority(event: &Event, level: BackpressureLevel) -> bool {
    matches!(level, BackpressureLevel::Critical) && matches!(event.priority, Priority::Low)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackpressureEventKind {
    LevelChanged {
        from: BackpressureLevel,
        to: BackpressureLevel,
    },
    DroppedAtMostOnce,
    BlockedForRetryLater,
    PublishersPaused,
    PublishersResumed,
    OverflowedToDisk { event_id: Option<String>, source: Option<String> },
    OverflowDirEmergency,
    StoppedLowPriority { event_id: Option<String>, source: Option<String> },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackpressureEventRecord {
    pub kind: BackpressureEventKind,
    pub level: BackpressureLevel,
    pub queue_depth: usize,
    pub event_id: Option<String>,
    pub source: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BackpressureSignal {
    pub level: BackpressureLevel,
    pub pause_publishers: bool,
}

impl Default for BackpressureSignal {
    fn default() -> Self {
        Self {
            level: BackpressureLevel::Normal,
            pause_publishers: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BackpressureController {
    max_queue_size: usize,
    level1_threshold: f32,
    level2_threshold: f32,
    level3_threshold: f32,
    level4_threshold: f32,
}

impl BackpressureController {
    #[must_use]
    pub const fn new(
        max_queue_size: usize,
        level1_threshold: f32,
        level2_threshold: f32,
        level3_threshold: f32,
        level4_threshold: f32,
    ) -> Self {
        Self {
            max_queue_size,
            level1_threshold,
            level2_threshold,
            level3_threshold,
            level4_threshold,
        }
    }

    #[must_use]
    pub fn signal_for_depth(&self, depth: usize) -> BackpressureSignal {
        let level = self.level_for_depth(depth);
        BackpressureSignal {
            level,
            pause_publishers: matches!(
                level,
                BackpressureLevel::L3
                    | BackpressureLevel::L4A
                    | BackpressureLevel::L4B
                    | BackpressureLevel::Critical
            ),
        }
    }

    #[must_use]
    pub fn level_for_depth(&self, depth: usize) -> BackpressureLevel {
        if self.max_queue_size == 0 {
            return BackpressureLevel::Critical;
        }

        let usage = depth as f32 / self.max_queue_size as f32;
        if usage >= 1.0 {
            BackpressureLevel::Critical
        } else if usage >= self.level4_threshold {
            BackpressureLevel::L4A
        } else if usage >= self.level3_threshold {
            BackpressureLevel::L3
        } else if usage >= self.level2_threshold {
            BackpressureLevel::L2
        } else if usage >= self.level1_threshold {
            BackpressureLevel::L1
        } else {
            BackpressureLevel::Normal
        }
    }

    #[must_use]
    pub fn apply_degradation(&self, mut event: Event, level: BackpressureLevel) -> Event {
        if matches!(event.delivery, DeliveryGuarantee::AtMostOnce)
            && matches!(level, BackpressureLevel::L1)
        {
            event.priority = downgrade_priority(event.priority);
        }

        event
    }

    #[must_use]
    pub fn should_drop(&self, event: &Event, level: BackpressureLevel) -> bool {
        matches!(event.delivery, DeliveryGuarantee::AtMostOnce)
            && matches!(
                level,
                BackpressureLevel::L2
                    | BackpressureLevel::L3
                    | BackpressureLevel::L4A
                    | BackpressureLevel::L4B
                    | BackpressureLevel::Critical
            )
    }

    #[must_use]
    pub fn should_block(&self, event: &Event, level: BackpressureLevel) -> bool {
        !matches!(event.delivery, DeliveryGuarantee::AtMostOnce)
            && matches!(
                level,
                BackpressureLevel::L3
                    | BackpressureLevel::L4A
                    | BackpressureLevel::L4B
                    | BackpressureLevel::Critical
            )
    }
}

fn downgrade_priority(priority: Priority) -> Priority {
    match priority {
        Priority::High => Priority::Normal,
        Priority::Normal | Priority::Low => Priority::Low,
    }
}

#[derive(Debug)]
pub struct BackpressureEventLog {
    limit: usize,
    entries: VecDeque<BackpressureEventRecord>,
}

impl BackpressureEventLog {
    #[must_use]
    pub fn new(limit: usize) -> Self {
        Self {
            limit: limit.max(1),
            entries: VecDeque::new(),
        }
    }

    pub fn push(&mut self, record: BackpressureEventRecord) {
        if self.entries.len() >= self.limit {
            self.entries.pop_front();
        }
        self.entries.push_back(record);
    }

    #[must_use]
    pub fn snapshot(&self) -> Vec<BackpressureEventRecord> {
        self.entries.iter().cloned().collect()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }
}
