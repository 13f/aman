#![forbid(unsafe_code)]
#![doc = "Event bus primitives for the aman agent framework."]
// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0


mod backpressure;
mod dedup;
mod ordering;
mod overflow;
pub mod rate_limiter;
mod retry_queue;

use async_trait::async_trait;
use backpressure::{BackpressureController, BackpressureEventLog, BackpressureSignal};
use tracing::instrument;
use tracing::Instrument;
pub use backpressure::{BackpressureEventKind, BackpressureEventRecord};
use dedup::{DedupOutcome, DedupWindow};
use kernel::event::{Event, EventType};
use kernel::retry::RetryBackoff;
use kernel::types::{BackpressureLevel, Priority, SourceId, TrustLevel};
use kernel::{AmanResult, Error};
use rate_limiter::{EventRateLimiter, RateLimiterConfig};
use ordering::OrderedQueue;
use overflow::OverflowDir;
pub use overflow::OverflowDir as PublicOverflowDir;
pub use retry_queue::{RetryQueue, RetryQueueConfig, RetryQueueItem, RetryScheduleResult};
use serde_json::Value;
use std::collections::HashMap;
use std::fmt;
use std::path::PathBuf;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;
use tokio::sync::Notify;

/// Type of callback invoked when the bus discards an event due to backpressure.
pub type DiscardHook = Arc<dyn Fn(&Event, BackpressureLevel, &str) + Send + Sync>;

/// Error returned when [`InMemoryBus::wait_for_event`] times out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WaitForEventTimeout;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SubscriptionId(u64);

impl SubscriptionId {
    #[must_use]
    pub const fn into_inner(self) -> u64 {
        self.0
    }
}

impl fmt::Display for SubscriptionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SubscriptionFilter {
    pub event_types: Option<Vec<EventType>>,
    pub sources: Option<Vec<SourceId>>,
    pub priorities: Option<Vec<Priority>>,
    pub payload_match: Option<Value>,
}

impl SubscriptionFilter {
    #[must_use]
    pub fn matches(&self, event: &Event) -> bool {
        self.matches_event_type(event)
            && self.matches_source(event)
            && self.matches_priority(event)
            && self.matches_payload(event)
    }

    fn matches_event_type(&self, event: &Event) -> bool {
        self.event_types
            .as_ref()
            .is_none_or(|types| types.iter().any(|kind| kind == &event.event_type))
    }

    fn matches_source(&self, event: &Event) -> bool {
        self.sources
            .as_ref()
            .is_none_or(|sources| sources.iter().any(|source| source == &event.source))
    }

    fn matches_priority(&self, event: &Event) -> bool {
        self.priorities.as_ref().is_none_or(|priorities| {
            priorities
                .iter()
                .any(|priority| priority == &event.priority)
        })
    }

    fn matches_payload(&self, event: &Event) -> bool {
        self.payload_match
            .as_ref()
            .is_none_or(|needle| payload_matches(needle, &event.payload))
    }
}

fn payload_matches(filter: &Value, payload: &Value) -> bool {
    match (filter, payload) {
        (Value::Object(filter_map), Value::Object(payload_map)) => {
            filter_map.iter().all(|(key, filter_value)| {
                payload_map
                    .get(key)
                    .is_some_and(|value| payload_matches(filter_value, value))
            })
        }
        (Value::Array(filter_values), Value::Array(payload_values)) => {
            filter_values.len() == payload_values.len()
                && filter_values
                    .iter()
                    .zip(payload_values)
                    .all(|(left, right)| payload_matches(left, right))
        }
        _ => filter == payload,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct QueueDepth {
    pub high: usize,
    pub normal: usize,
    pub low: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BusMetrics {
    pub queue_depth: QueueDepth,
    pub retry_queue_depth: usize,
    pub throughput: u64,
    pub discarded_count: u64,
    pub duplicate_count: u64,
    pub backpressure_event_count: usize,
    pub subscription_count: usize,
    pub backpressure_level: BackpressureLevel,
    pub pause_publishers: bool,
    /// Number of idle events silently discarded under backpressure.
    pub idle_events_discarded: u64,
    /// Number of successful wait_for_event wakeups (event returned).
    pub wait_for_event_wakeups: u64,
    /// Number of wait_for_event false wakeups (stale notifications).
    pub wait_for_event_false_wakeups: u64,
}

impl Default for BusMetrics {
    fn default() -> Self {
        Self {
            queue_depth: QueueDepth::default(),
            retry_queue_depth: 0,
            throughput: 0,
            discarded_count: 0,
            duplicate_count: 0,
            backpressure_event_count: 0,
            subscription_count: 0,
            backpressure_level: BackpressureLevel::Normal,
            pause_publishers: false,
            idle_events_discarded: 0,
            wait_for_event_wakeups: 0,
            wait_for_event_false_wakeups: 0,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct InMemoryBusConfig {
    pub max_queue_size: usize,
    pub retry_queue_max: usize,
    pub retry_max_attempts: u32,
    pub retry_backoff: RetryBackoff,
    pub dedup_window_ms: u64,
    pub backpressure_event_limit: usize,
    pub level1_threshold: f32,
    pub level2_threshold: f32,
    pub level3_threshold: f32,
    pub level4_threshold: f32,
    /// Optional path to an overflow directory for Level 4A (98% full)
    /// disk-based event spillover.
    pub overflow_dir: Option<PathBuf>,
    /// Maximum bytes the overflow directory may consume before triggering
    /// Level 4B emergency fallback.
    pub overflow_max_bytes: u64,
    /// Optional per-source rate limiter configuration.
    /// When set, events from any source are rate-limited using a token-bucket
    /// algorithm. Default: `None` (no rate limiting).
    pub rate_limiter: Option<RateLimiterConfig>,
    /// When true, reject events from sandboxed sources that target sensitive
    /// event types (ConfigChanged, SecretRotated, InjectionDetected).
    /// Default: true.
    pub reject_sandboxed_sensitive_events: bool,
}

impl Default for InMemoryBusConfig {
    fn default() -> Self {
        Self {
            max_queue_size: 10_000,
            retry_queue_max: 1_000,
            retry_max_attempts: 5,
            retry_backoff: RetryBackoff::Sequence(vec![100, 500, 2_000]),
            dedup_window_ms: 30_000,
            backpressure_event_limit: 128,
            level1_threshold: 0.8097,
            level2_threshold: 0.90109,
            level3_threshold: 0.9597,
            level4_threshold: 0.98110,
            overflow_dir: None,
            overflow_max_bytes: 1_073_741_824, // 1 GB
            rate_limiter: None,
            reject_sandboxed_sensitive_events: true,
        }
    }
}

#[async_trait]
pub trait EventHandler: Send + Sync {
    async fn handle(&self, event: Event) -> AmanResult<()>;
}

#[async_trait]
pub trait EventBus: Send + Sync {
    async fn publish(&self, event: Event) -> AmanResult<()>;
    async fn subscribe(
        &self,
        filter: SubscriptionFilter,
        handler: Box<dyn EventHandler>,
    ) -> AmanResult<SubscriptionId>;
    async fn unsubscribe(&self, id: SubscriptionId);

    /// Non-blocking attempt to dequeue the next event, if any.
    /// Returns `None` if the queue is empty.
    fn try_dequeue(&self) -> Option<Event>;

    /// Block until an event arrives or the timeout elapses.
    ///
    /// Edge-triggered: only wakes when the queue transitions from
    /// empty to non-empty. Returns the first available event.
    async fn wait_for_event(&self, timeout: Duration) -> Result<Event, WaitForEventTimeout>;

    fn metrics(&self) -> BusMetrics;
    fn backpressure_level(&self) -> BackpressureLevel;
    fn can_poll(&self) -> bool;
}

// ── EventPublisher impl ────────────────────────────────────────────
// Newtype wrapper so we can coerce Arc<BusEventPublisher> → Arc<dyn EventPublisher>.
// (Rust won't let us cast Arc<dyn EventBus> to Arc<dyn EventPublisher> directly
// even though Arc<dyn EventBus> implements EventPublisher.)

/// Wraps an `EventBus` so it can be passed as an `Arc<dyn EventPublisher>`.
pub struct BusEventPublisher {
    bus: Arc<dyn EventBus>,
}

impl std::fmt::Debug for BusEventPublisher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BusEventPublisher").finish_non_exhaustive()
    }
}

impl BusEventPublisher {
    /// Wrap an event bus for use as an EventPublisher.
    pub fn new(bus: Arc<dyn EventBus>) -> Self {
        Self { bus }
    }
}

#[async_trait]
impl kernel::hook::EventPublisher for BusEventPublisher {
    async fn publish(&self, event: kernel::event::Event) -> kernel::AmanResult<()> {
        self.bus.publish(event).await
    }
}

#[derive(Clone)]
struct Subscription {
    filter: SubscriptionFilter,
    handler: Arc<dyn EventHandler>,
}

enum PublishAdmission {
    Accept(Event),
    Overflow(Event),
    Drop,
    RetryLater { level: BackpressureLevel },
}

struct BusState {
    queue: OrderedQueue,
    dedup: DedupWindow,
    subscriptions: HashMap<SubscriptionId, Subscription>,
    retry_queue: RetryQueue,
    backpressure_events: BackpressureEventLog,
    next_subscription_id: u64,
    throughput: u64,
    discarded_count: u64,
    duplicate_count: u64,
    idle_discarded_count: u64,
    wait_for_event_wakeups: u64,
    wait_for_event_false_wakeups: u64,
    /// Monotonically increasing counter — incremented each time the queue
    /// transitions from empty to non-empty. Used to detect stale notifications
    /// in `wait_for_event`.
    wait_for_event_generation: u64,
    signal: BackpressureSignal,
}

impl BusState {
    fn new(config: &InMemoryBusConfig) -> Self {
        Self {
            queue: OrderedQueue::default(),
            dedup: DedupWindow::new(config.dedup_window_ms, config.max_queue_size.max(64)),
            subscriptions: HashMap::new(),
            retry_queue: RetryQueue::new(RetryQueueConfig {
                max_entries: config.retry_queue_max,
                max_attempts: config.retry_max_attempts,
                retry_backoff: config.retry_backoff.clone(),
            }),
            backpressure_events: BackpressureEventLog::new(config.backpressure_event_limit),
            next_subscription_id: 1,
            throughput: 0,
            discarded_count: 0,
            duplicate_count: 0,
            idle_discarded_count: 0,
            wait_for_event_wakeups: 0,
            wait_for_event_false_wakeups: 0,
            wait_for_event_generation: 0,
            signal: BackpressureSignal::default(),
        }
    }

    fn metrics(&self) -> BusMetrics {
        BusMetrics {
            queue_depth: self.queue.depth_by_priority(),
            retry_queue_depth: self.retry_queue.len(),
            throughput: self.throughput,
            discarded_count: self.discarded_count,
            duplicate_count: self.duplicate_count,
            backpressure_event_count: self.backpressure_events.len(),
            subscription_count: self.subscriptions.len(),
            backpressure_level: self.signal.level,
            pause_publishers: self.signal.pause_publishers,
            idle_events_discarded: self.idle_discarded_count,
            wait_for_event_wakeups: self.wait_for_event_wakeups,
            wait_for_event_false_wakeups: self.wait_for_event_false_wakeups,
        }
    }

    fn update_signal(&mut self, next_signal: BackpressureSignal, queue_depth: usize) {
        let previous_signal = self.signal;
        if previous_signal.level != next_signal.level {
            self.backpressure_events.push(BackpressureEventRecord {
                kind: BackpressureEventKind::LevelChanged {
                    from: previous_signal.level,
                    to: next_signal.level,
                },
                level: next_signal.level,
                queue_depth,
                event_id: None,
                source: None,
            });
        }

        if previous_signal.pause_publishers != next_signal.pause_publishers {
            let kind = if next_signal.pause_publishers {
                BackpressureEventKind::PublishersPaused
            } else {
                BackpressureEventKind::PublishersResumed
            };
            self.backpressure_events.push(BackpressureEventRecord {
                kind,
                level: next_signal.level,
                queue_depth,
                event_id: None,
                source: None,
            });
        }

        self.signal = next_signal;
    }

    fn record_drop(&mut self, event: &Event, level: BackpressureLevel, queue_depth: usize) {
        self.backpressure_events.push(BackpressureEventRecord {
            kind: BackpressureEventKind::DroppedAtMostOnce,
            level,
            queue_depth,
            event_id: Some(event.id.to_string()),
            source: Some(event.source.to_string()),
        });
    }

    fn record_block(&mut self, event: &Event, level: BackpressureLevel, queue_depth: usize) {
        self.backpressure_events.push(BackpressureEventRecord {
            kind: BackpressureEventKind::BlockedForRetryLater,
            level,
            queue_depth,
            event_id: Some(event.id.to_string()),
            source: Some(event.source.to_string()),
        });
    }

    fn record_overflow(&mut self, event: &Event, level: BackpressureLevel, queue_depth: usize) {
        self.backpressure_events.push(BackpressureEventRecord {
            kind: BackpressureEventKind::OverflowedToDisk {
                event_id: Some(event.id.to_string()),
                source: Some(event.source.to_string()),
            },
            level,
            queue_depth,
            event_id: Some(event.id.to_string()),
            source: Some(event.source.to_string()),
        });
    }

    fn record_stopped_low_priority(
        &mut self,
        event: &Event,
        level: BackpressureLevel,
        queue_depth: usize,
    ) {
        self.backpressure_events.push(BackpressureEventRecord {
            kind: BackpressureEventKind::StoppedLowPriority {
                event_id: Some(event.id.to_string()),
                source: Some(event.source.to_string()),
            },
            level,
            queue_depth,
            event_id: Some(event.id.to_string()),
            source: Some(event.source.to_string()),
        });
    }
}

pub struct InMemoryBus {
    config: InMemoryBusConfig,
    backpressure: BackpressureController,
    state: Mutex<BusState>,
    draining: AtomicBool,
    overflow_dir: Option<OverflowDir>,
    discard_hook: Option<DiscardHook>,
    /// Notifies `wait_for_event` when the queue transitions from empty → non-empty.
    event_notify: Notify,
    /// Per-source rate limiter (token bucket). When `None`, rate limiting is disabled.
    rate_limiter: Mutex<Option<EventRateLimiter>>,
    /// When true, reject events from sandboxed sources that target sensitive types.
    reject_sandboxed_sensitive: bool,
}

impl Default for InMemoryBus {
    fn default() -> Self {
        Self::new(InMemoryBusConfig::default())
    }
}

impl InMemoryBus {
    #[must_use]
    pub fn new(config: InMemoryBusConfig) -> Self {
        let backpressure = BackpressureController::new(
            config.max_queue_size,
            config.level1_threshold,
            config.level2_threshold,
            config.level3_threshold,
            config.level4_threshold,
        );

        let overflow_dir = config.overflow_dir.as_ref().and_then(|dir| {
            OverflowDir::new(dir, config.overflow_max_bytes).ok()
        });

        let rate_limiter_config = config.rate_limiter.clone();
        let reject_sandboxed_sensitive = config.reject_sandboxed_sensitive_events;
        Self {
            state: Mutex::new(BusState::new(&config)),
            config,
            backpressure,
            draining: AtomicBool::new(false),
            overflow_dir,
            discard_hook: None,
            event_notify: Notify::new(),
            rate_limiter: Mutex::new(rate_limiter_config.map(EventRateLimiter::new)),
            reject_sandboxed_sensitive,
        }
    }

    /// Register a callback invoked when the bus drops an event due to backpressure.
    pub fn set_discard_hook(&mut self, hook: DiscardHook) {
        self.discard_hook = Some(hook);
    }

    #[must_use]
    pub fn backpressure_signal(&self) -> BackpressureSignal {
        self.lock_state().signal
    }

    #[must_use]
    pub fn can_poll(&self) -> bool {
        let mut state = self.lock_state();
        self.refresh_signal(&mut state);
        !matches!(
            state.signal.level,
            BackpressureLevel::L3
                | BackpressureLevel::L4A
                | BackpressureLevel::L4B
                | BackpressureLevel::Critical
        )
    }

    #[must_use]
    pub fn backpressure_events(&self) -> Vec<BackpressureEventRecord> {
        self.lock_state().backpressure_events.snapshot()
    }

    fn refresh_signal(&self, state: &mut BusState) {
        let queue_depth = state.queue.len();
        let next_signal = self.backpressure.signal_for_depth(queue_depth);
        state.update_signal(next_signal, queue_depth);
    }

    /// Rate-limit check against the per-source token bucket.
    ///
    /// Called *before* `state` is locked to avoid a nested-lock deadlock (the
    /// rate-limiter mutex must not be acquired while holding `state`). If no
    /// limiter is configured, this is a no-op.
    fn check_rate_limit(&self, source: &crate::SourceId) -> AmanResult<()> {
        let mut guard = self
            .rate_limiter
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        if let Some(ref mut limiter) = *guard {
            limiter.check(source)?;
        }
        Ok(())
    }

    fn lock_state(&self) -> MutexGuard<'_, BusState> {
        self.state
            .lock()
            .unwrap_or_else(|err| err.into_inner())
    }

    fn admit_event(&self, mut event: Event, state: &mut BusState) -> AmanResult<PublishAdmission> {
        // ── Layer 4: Trust-level enforcement ──────────────────────────
        // Reject events from sandboxed sources that target sensitive event types.
        if self.reject_sandboxed_sensitive
            && event.trust_level == Some(TrustLevel::Sandboxed)
            && event.event_type.is_sensitive()
        {
            tracing::warn!(
                event_id = %event.id,
                source = %event.source,
                event_type = %event.event_type.as_str(),
                "rejected sensitive event from sandboxed source"
            );
            return Err(Error::SecurityViolation {
                message: format!(
                    "sandboxed source '{}' cannot publish sensitive event type '{}'",
                    event.source, event.event_type
                ),
            });
        }

        // Note: rate-limit check is performed by `check_rate_limit` *before*
        // `state` is locked (see `publish`). Holding `state` while acquiring a
        // second mutex here was a nested-lock deadlock risk.

        self.refresh_signal(state);
        event = self
            .backpressure
            .apply_degradation(event, state.signal.level);

        // Idle events: silently discard at any backpressure level above Normal.
        // These are internal bookkeeping events and should not consume queue
        // capacity or overflow to disk when the system is under pressure.
        if event.is_idle_event() && state.signal.level != BackpressureLevel::Normal {
            state.idle_discarded_count += 1;
            if let Some(ref hook) = self.discard_hook {
                hook(&event, state.signal.level, "idle_backpressure");
            }
            return Ok(PublishAdmission::Drop);
        }

        // Level 5 (Critical): stop low-priority event sources
        if backpressure::should_stop_low_priority(&event, state.signal.level) {
            state.discarded_count += 1;
            state.record_stopped_low_priority(&event, state.signal.level, state.queue.len());
            if let Some(ref hook) = self.discard_hook {
                hook(&event, state.signal.level, "critical_low_priority");
            }
            return Ok(PublishAdmission::Drop);
        }

        if self.backpressure.should_drop(&event, state.signal.level) {
            state.discarded_count += 1;
            state.record_drop(&event, state.signal.level, state.queue.len());
            if let Some(ref hook) = self.discard_hook {
                let reason = if state.signal.level == BackpressureLevel::L4B { "backpressure_l4b" } else { "backpressure" };
                hook(&event, state.signal.level, reason);
            }
            return Ok(PublishAdmission::Drop);
        }

        // Level 4A: overflow guaranteed-delivery events to disk
        if backpressure::should_overflow_to_disk(&event, state.signal.level) {
            // Overflow dir available? Write to disk instead of queue
            if self.overflow_dir.is_some() {
                let overflow_usage = self
                    .overflow_dir
                    .as_ref()
                    .and_then(|d| d.usage_ratio().ok())
                    .unwrap_or(0.0);

                // Level 4B: overflow dir ≥80% → emergency alert + fallback to block
                if backpressure::is_overflow_dir_emergency(overflow_usage, 0.8) {
                    state.backpressure_events.push(BackpressureEventRecord {
                        kind: BackpressureEventKind::OverflowDirEmergency,
                        level: state.signal.level,
                        queue_depth: state.queue.len(),
                        event_id: None,
                        source: None,
                    });
                    state.record_block(&event, state.signal.level, state.queue.len());
                    return Ok(PublishAdmission::RetryLater {
                        level: BackpressureLevel::L3,
                    });
                }

                // Overflow to disk
                state.record_overflow(&event, state.signal.level, state.queue.len());
                return Ok(PublishAdmission::Overflow(event));
            }

            // No overflow dir configured → fall through to block
        }

        if self.backpressure.should_block(&event, state.signal.level) {
            state.record_block(&event, state.signal.level, state.queue.len());
            return Ok(PublishAdmission::RetryLater {
                level: state.signal.level,
            });
        }

        if self.config.max_queue_size > 0 && state.queue.len() >= self.config.max_queue_size {
            state.discarded_count += 1;
            self.refresh_signal(state);
            return Err(Error::BusFull);
        }

        match state.dedup.check(&event) {
            DedupOutcome::Accepted => Ok(PublishAdmission::Accept(event)),
            DedupOutcome::Duplicate => {
                state.duplicate_count += 1;
                Ok(PublishAdmission::Drop)
            }
        }
    }

    async fn ensure_drained(&self) -> AmanResult<()> {
        if self
            .draining
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return Ok(());
        }

        loop {
            let work = {
                let mut state = self.lock_state();
                match state.queue.pop() {
                    Some(event) => {
                        self.refresh_signal(&mut state);
                        let handlers = state
                            .subscriptions
                            .values()
                            .filter(|subscription| subscription.filter.matches(&event))
                            .map(|subscription| Arc::clone(&subscription.handler))
                            .collect::<Vec<_>>();
                        Some((event, handlers))
                    }
                    None => None,
                }
            };

            let Some((event, handlers)) = work else {
                self.draining.store(false, Ordering::Release);

                let should_continue = {
                    let state = self.lock_state();
                    !state.queue.is_empty()
                };

                if should_continue
                    && self
                        .draining
                        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                        .is_ok()
                {
                    continue;
                }

                return Ok(());
            };

            for handler in handlers {
                handler
                    .handle(event.clone())
                    .instrument(tracing::trace_span!("dispatch_event"))
                    .await?;
            }

            let mut state = self.lock_state();
            state.throughput += 1;
            self.refresh_signal(&mut state);
        }
    }
}

#[async_trait]
impl EventBus for InMemoryBus {
    #[instrument(skip(self, event), fields(event_id = %event.id, source = %event.source))]
    async fn publish(&self, event: Event) -> AmanResult<()> {
        // Rate-limit check must run *before* we take `state`, otherwise we'd
        // acquire two mutexes in nested fashion (deadlock risk).
        self.check_rate_limit(&event.source)?;
        {
            let mut state = self.lock_state();
            match self.admit_event(event, &mut state)? {
                PublishAdmission::Accept(event) => {
                    let was_empty = state.queue.is_empty();
                    state.queue.push(event);
                    if was_empty {
                        state.wait_for_event_generation += 1;
                        self.event_notify.notify_one();
                    }
                    self.refresh_signal(&mut state);
                }
                PublishAdmission::Overflow(event) => {
                    // Write to overflow disk outside the lock scope below
                    if let Some(ref overflow) = self.overflow_dir {
                        overflow.write_event(&event)?;
                        state.throughput += 1;
                    }
                    return Ok(());
                }
                PublishAdmission::Drop => return Ok(()),
                PublishAdmission::RetryLater { level } => {
                    return Err(Error::BackpressureBlocked { level });
                }
            }
        }

        self.ensure_drained().await
    }

    async fn subscribe(
        &self,
        filter: SubscriptionFilter,
        handler: Box<dyn EventHandler>,
    ) -> AmanResult<SubscriptionId> {
        let mut state = self.lock_state();
        let id = SubscriptionId(state.next_subscription_id);
        state.next_subscription_id += 1;
        state.subscriptions.insert(
            id,
            Subscription {
                filter,
                handler: Arc::from(handler),
            },
        );
        Ok(id)
    }

    async fn unsubscribe(&self, id: SubscriptionId) {
        let mut state = self.lock_state();
        state.subscriptions.remove(&id);
    }

    fn metrics(&self) -> BusMetrics {
        self.lock_state().metrics()
    }

    fn backpressure_level(&self) -> BackpressureLevel {
        self.lock_state().signal.level
    }

    fn can_poll(&self) -> bool {
        Self::can_poll(self)
    }

    fn try_dequeue(&self) -> Option<Event> {
        let mut state = self.lock_state();
        let event = state.queue.pop();
        if let Some(ref _event) = event {
            state.throughput += 1;
            self.refresh_signal(&mut state);
        }
        event
    }

    async fn wait_for_event(&self, timeout: Duration) -> Result<Event, WaitForEventTimeout> {
        loop {
            // Fast path: pop immediately if queue has events
            {
                let mut state = self.lock_state();
                if let Some(event) = state.queue.pop() {
                    self.refresh_signal(&mut state);
                    state.throughput += 1;
                    state.wait_for_event_wakeups += 1;
                    return Ok(event);
                }
            }

            // Record generation before waiting
            let generation = {
                let state = self.lock_state();
                state.wait_for_event_generation
            };

            // Wait for notification or timeout
            let notified = self.event_notify.notified();
            tokio::pin!(notified);

            tokio::select! {
                _ = &mut notified => {
                    // Check if generation advanced (real event arrival)
                    // vs. stale notification from a previous cycle
                    let current_gen = {
                        let state = self.lock_state();
                        state.wait_for_event_generation
                    };
                    if current_gen <= generation {
                        let mut state = self.lock_state();
                        state.wait_for_event_false_wakeups += 1;
                        // Stale notification — loop back and wait
                    }
                    // Generation advanced — loop back to try popping
                }
                _ = tokio::time::sleep(timeout) => {
                    return Err(WaitForEventTimeout);
                }
            }
        }
    }
}

impl InMemoryBus {
    /// Enqueue an event for retry (intended for WAL-confirmed delivery failures).
    /// Returns `RetryScheduleResult::Queued` if the event was scheduled, or
    /// `RetryScheduleResult::Exhausted` if the retry limit was exceeded.
    /// Returns `Error::BusFull` if the retry queue is at capacity.
    pub fn enqueue_for_retry(
        &self,
        event: Event,
        previous_attempts: u32,
        last_error: Option<String>,
    ) -> AmanResult<RetryScheduleResult> {
        let mut state = self.lock_state();
        state.retry_queue.schedule_retry(event, previous_attempts, last_error)
    }

    /// Scan the overflow directory and re-inject events into the queue for
    /// replay after a crash recovery. Events are sorted by timestamp and
    /// deduplication is applied during re-injection.
    pub fn recover_overflow(&self) -> AmanResult<usize> {
        let Some(ref overflow) = self.overflow_dir else {
            return Ok(0);
        };

        let events = overflow.scan()?;
        let count = events.len();

        for event in events {
            let event_id = event.id;
            let mut state = self.lock_state();
            match state.dedup.check(&event) {
                DedupOutcome::Accepted => {
                    let was_empty = state.queue.is_empty();
                    state.queue.push(event);
                    if was_empty {
                        state.wait_for_event_generation += 1;
                        self.event_notify.notify_one();
                    }
                    self.refresh_signal(&mut state);
                    if let Some(ref overflow) = self.overflow_dir {
                        let _ = overflow.remove_event(&event_id);
                    }
                }
                DedupOutcome::Duplicate => {
                    // Already processed, just remove from overflow
                    if let Some(ref overflow) = self.overflow_dir {
                        let _ = overflow.remove_event(&event_id);
                    }
                }
            }
        }

        Ok(count)
    }

    /// Returns the current overflow directory usage ratio (0.0–1.0),
    /// or `None` if overflow is not configured.
    pub fn overflow_usage_ratio(&self) -> Option<f32> {
        self.overflow_dir.as_ref().and_then(|d| d.usage_ratio().ok())
    }
}

#[cfg(test)]
mod tests {
    use super::{
        BackpressureEventKind, EventBus, EventHandler, InMemoryBus, InMemoryBusConfig,
        PublishAdmission, QueueDepth, RetryScheduleResult, SubscriptionFilter, WaitForEventTimeout,
    };
    use async_trait::async_trait;
    use kernel::event::{Event, EventType};
    use kernel::types::{BackpressureLevel, DedupKey, DeliveryGuarantee, Priority, SourceId};
    use kernel::{AmanResult, Error};
    use crate::overflow::OverflowDir;
    use serde_json::json;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    #[derive(Default)]
    struct RecordingHandler {
        events: Mutex<Vec<Event>>,
    }

    impl RecordingHandler {
        fn snapshot(&self) -> Vec<Event> {
            self.events.lock().expect("handler mutex").clone()
        }
    }

    #[async_trait]
    impl EventHandler for RecordingHandler {
        async fn handle(&self, event: Event) -> AmanResult<()> {
            self.events.lock().expect("handler mutex").push(event);
            Ok(())
        }
    }

    struct SharedHandler(Arc<RecordingHandler>);

    #[async_trait]
    impl EventHandler for SharedHandler {
        async fn handle(&self, event: Event) -> AmanResult<()> {
            self.0.handle(event).await
        }
    }

    #[derive(Default)]
    struct FailingHandler;

    #[async_trait]
    impl EventHandler for FailingHandler {
        async fn handle(&self, _event: Event) -> AmanResult<()> {
            Err(Error::Unrecoverable {
                message: "boom".to_owned(),
            })
        }
    }

    #[test]
    fn publish_delivers_to_multiple_matching_subscribers() {
        pollster::block_on(async {
            let bus = InMemoryBus::default();
            let first = Arc::new(RecordingHandler::default());
            let second = Arc::new(RecordingHandler::default());

            bus.subscribe(
                SubscriptionFilter {
                    event_types: Some(vec![EventType::TimerTick]),
                    ..SubscriptionFilter::default()
                },
                Box::new(SharedHandler(Arc::clone(&first))),
            )
            .await
            .expect("subscribe first");
            bus.subscribe(
                SubscriptionFilter::default(),
                Box::new(SharedHandler(Arc::clone(&second))),
            )
            .await
            .expect("subscribe second");

            bus.publish(Event::new(
                "timer:a",
                EventType::TimerTick,
                json!({"ok": true}),
            ))
            .await
            .expect("publish event");

            assert_eq!(first.snapshot().len(), 1);
            assert_eq!(second.snapshot().len(), 1);
            assert_eq!(bus.metrics().throughput, 1);
        });
    }

    #[test]
    fn unsubscribe_stops_future_deliveries() {
        pollster::block_on(async {
            let bus = InMemoryBus::default();
            let handler = Arc::new(RecordingHandler::default());
            let subscription = bus
                .subscribe(
                    SubscriptionFilter::default(),
                    Box::new(SharedHandler(Arc::clone(&handler))),
                )
                .await
                .expect("subscribe");

            bus.publish(Event::new(
                "timer:a",
                EventType::TimerTick,
                json!({"step": 1}),
            ))
            .await
            .expect("publish first");
            bus.unsubscribe(subscription).await;
            bus.publish(Event::new(
                "timer:a",
                EventType::TimerTick,
                json!({"step": 2}),
            ))
            .await
            .expect("publish second");

            let events = handler.snapshot();
            assert_eq!(events.len(), 1);
            assert_eq!(events[0].payload, json!({"step": 1}));
        });
    }

    #[test]
    fn filter_matches_event_type_source_priority_and_payload_subset() {
        pollster::block_on(async {
            let bus = InMemoryBus::default();
            let handler = Arc::new(RecordingHandler::default());

            bus.subscribe(
                SubscriptionFilter {
                    event_types: Some(vec![EventType::WebhookReceived]),
                    sources: Some(vec![SourceId::new("webhook:billing")]),
                    priorities: Some(vec![Priority::High]),
                    payload_match: Some(json!({"kind": "invoice", "nested": {"ok": true}})),
                },
                Box::new(SharedHandler(Arc::clone(&handler))),
            )
            .await
            .expect("subscribe");

            let mut miss = Event::new(
                "webhook:billing",
                EventType::WebhookReceived,
                json!({"kind": "invoice"}),
            );
            miss.priority = Priority::High;
            bus.publish(miss).await.expect("publish miss");

            let mut hit = Event::new(
                "webhook:billing",
                EventType::WebhookReceived,
                json!({"kind": "invoice", "nested": {"ok": true, "extra": 1}}),
            );
            hit.priority = Priority::High;
            bus.publish(hit).await.expect("publish hit");

            assert_eq!(handler.snapshot().len(), 1);
        });
    }

    #[test]
    fn keeps_fifo_for_same_source_even_when_later_event_has_higher_priority() {
        pollster::block_on(async {
            let bus = InMemoryBus::new(InMemoryBusConfig {
                max_queue_size: 8,
                ..InMemoryBusConfig::default()
            });
            let gate = Arc::new(RecordingHandler::default());

            bus.subscribe(
                SubscriptionFilter::default(),
                Box::new(SharedHandler(Arc::clone(&gate))),
            )
            .await
            .expect("subscribe");

            let mut first = Event::new("source:a", EventType::MessageReceived, json!({"seq": 1}));
            first.priority = Priority::Normal;
            let mut second = Event::new("source:a", EventType::MessageReceived, json!({"seq": 2}));
            second.priority = Priority::High;

            {
                let mut state = bus.lock_state();
                state.queue.push(first);
                state.queue.push(second);
                bus.refresh_signal(&mut state);
            }

            bus.ensure_drained().await.expect("drain queue");

            let payloads = gate
                .snapshot()
                .into_iter()
                .map(|event| event.payload)
                .collect::<Vec<_>>();
            assert_eq!(payloads, vec![json!({"seq": 1}), json!({"seq": 2})]);
        });
    }

    #[test]
    fn dedup_window_drops_duplicates_without_harming_distinct_events() {
        pollster::block_on(async {
            let bus = InMemoryBus::default();
            let handler = Arc::new(RecordingHandler::default());
            bus.subscribe(
                SubscriptionFilter::default(),
                Box::new(SharedHandler(Arc::clone(&handler))),
            )
            .await
            .expect("subscribe");

            let key = DedupKey::new("watch:file_created:invoice-1001");
            let mut first = Event::new(
                "watch:invoices",
                EventType::FileCreated,
                json!({"name": "a.pdf"}),
            );
            first.dedup_key = Some(key.clone());
            let mut duplicate = Event::new(
                "watch:invoices",
                EventType::FileCreated,
                json!({"name": "a.pdf"}),
            );
            duplicate.dedup_key = Some(key);
            let different = Event::new(
                "watch:invoices",
                EventType::FileCreated,
                json!({"name": "b.pdf"}),
            );

            bus.publish(first).await.expect("publish first");
            bus.publish(duplicate).await.expect("publish duplicate");
            bus.publish(different).await.expect("publish different");

            assert_eq!(handler.snapshot().len(), 2);
            assert_eq!(bus.metrics().duplicate_count, 1);
        });
    }

    #[test]
    fn backpressure_degrades_drops_and_recovers() {
        pollster::block_on(async {
            let bus = InMemoryBus::new(InMemoryBusConfig {
                max_queue_size: 10,
                level1_threshold: 0.80,
                level2_threshold: 0.90,
                level3_threshold: 0.95,
                level4_threshold: 0.98,
                ..InMemoryBusConfig::default()
            });

            {
                let mut state = bus.lock_state();
                for index in 0..8 {
                    let event = Event::new("source:a", EventType::TimerTick, json!({"seq": index}));
                    state.queue.push(event);
                }
                bus.refresh_signal(&mut state);
            }

            let mut degraded = Event::new("source:b", EventType::Heartbeat, json!({"seq": 99}));
            degraded.priority = Priority::High;
            degraded.delivery = DeliveryGuarantee::AtMostOnce;

            {
                let mut state = bus.lock_state();
                let degraded = match bus.admit_event(degraded, &mut state).expect("admit event") {
                    PublishAdmission::Accept(event) => event,
                    PublishAdmission::Drop | PublishAdmission::RetryLater { .. } | PublishAdmission::Overflow(_) => {
                        panic!("event should be accepted at level 1")
                    }
                };
                assert_eq!(degraded.priority, Priority::Normal);
            }

            {
                let mut state = bus.lock_state();
                while state.queue.len() < 9 {
                    let event = Event::new(
                        "source:a",
                        EventType::TimerTick,
                        json!({"fill": state.queue.len()}),
                    );
                    state.queue.push(event);
                }
                bus.refresh_signal(&mut state);
            }

            let mut dropped = Event::new("source:b", EventType::Heartbeat, json!({"drop": true}));
            dropped.delivery = DeliveryGuarantee::AtMostOnce;
            {
                let mut state = bus.lock_state();
                let prepared = bus.admit_event(dropped, &mut state).expect("admit event");
                assert!(matches!(prepared, PublishAdmission::Drop));
                assert_eq!(state.signal.level, BackpressureLevel::L2);
                assert_eq!(state.discarded_count, 1);
            }

            {
                let mut state = bus.lock_state();
                while state.queue.len() < 10 {
                    let event = Event::new(
                        "source:c",
                        EventType::TimerTick,
                        json!({"fill": state.queue.len()}),
                    );
                    state.queue.push(event);
                }
                bus.refresh_signal(&mut state);
            }

            assert_eq!(bus.backpressure_signal().level, BackpressureLevel::Critical);
            assert!(bus.backpressure_signal().pause_publishers);

            let subscribe_handler = Arc::new(RecordingHandler::default());
            bus.subscribe(
                SubscriptionFilter::default(),
                Box::new(SharedHandler(Arc::clone(&subscribe_handler))),
            )
            .await
            .expect("subscribe");
            bus.ensure_drained().await.expect("drain");

            assert_eq!(bus.backpressure_level(), BackpressureLevel::Normal);
            assert!(!bus.backpressure_signal().pause_publishers);
            assert_eq!(bus.metrics().queue_depth, QueueDepth::default());
        });
    }

    #[test]
    fn records_backpressure_drop_events() {
        pollster::block_on(async {
            let bus = InMemoryBus::new(InMemoryBusConfig {
                max_queue_size: 10,
                level1_threshold: 0.80,
                level2_threshold: 0.90,
                level3_threshold: 0.95,
                level4_threshold: 0.98,
                ..InMemoryBusConfig::default()
            });

            {
                let mut state = bus.lock_state();
                while state.queue.len() < 9 {
                    let event = Event::new(
                        "source:a",
                        EventType::TimerTick,
                        json!({"fill": state.queue.len()}),
                    );
                    state.queue.push(event);
                }
                bus.refresh_signal(&mut state);
            }

            let mut dropped = Event::new("source:b", EventType::Heartbeat, json!({"drop": true}));
            dropped.delivery = DeliveryGuarantee::AtMostOnce;
            let dropped_id = dropped.id.to_string();

            {
                let mut state = bus.lock_state();
                let prepared = bus.admit_event(dropped, &mut state).expect("admit event");
                assert!(matches!(prepared, PublishAdmission::Drop));
            }

            let events = bus.backpressure_events();
            let drop_record = events
                .iter()
                .find(|record| matches!(record.kind, BackpressureEventKind::DroppedAtMostOnce))
                .expect("drop record should exist");
            assert_eq!(drop_record.level, BackpressureLevel::L2);
            assert_eq!(drop_record.event_id.as_deref(), Some(dropped_id.as_str()));
            assert_eq!(drop_record.source.as_deref(), Some("source:b"));
        });
    }

    #[test]
    fn records_pause_and_resume_transitions() {
        pollster::block_on(async {
            let bus = InMemoryBus::new(InMemoryBusConfig {
                max_queue_size: 10,
                ..InMemoryBusConfig::default()
            });

            {
                let mut state = bus.lock_state();
                while state.queue.len() < 10 {
                    let fill = state.queue.len();
                    state.queue.push(Event::new(
                        "source:a",
                        EventType::TimerTick,
                        json!({"fill": fill}),
                    ));
                }
                bus.refresh_signal(&mut state);
            }

            bus.ensure_drained().await.expect("drain queue");

            let events = bus.backpressure_events();
            assert!(
                events
                    .iter()
                    .any(|record| matches!(record.kind, BackpressureEventKind::PublishersPaused)),
                "expected a pause record"
            );
            assert!(
                events
                    .iter()
                    .any(|record| matches!(record.kind, BackpressureEventKind::PublishersResumed)),
                "expected a resume record"
            );
            assert!(
                events.iter().any(|record| matches!(
                    record.kind,
                    BackpressureEventKind::LevelChanged {
                        from: _,
                        to: BackpressureLevel::Normal
                    }
                )),
                "expected a transition back to normal"
            );
        });
    }

    #[test]
    fn level3_blocks_guaranteed_delivery_and_polling_until_recovered() {
        pollster::block_on(async {
            let bus = InMemoryBus::new(InMemoryBusConfig {
                max_queue_size: 20,
                level1_threshold: 0.80,
                level2_threshold: 0.90,
                level3_threshold: 0.95,
                level4_threshold: 0.98,
                ..InMemoryBusConfig::default()
            });

            {
                let mut state = bus.lock_state();
                while state.queue.len() < 19 {
                    let fill = state.queue.len();
                    state.queue.push(Event::new(
                        "source:a",
                        EventType::TimerTick,
                        json!({"fill": fill}),
                    ));
                }
                bus.refresh_signal(&mut state);
            }

            assert_eq!(bus.backpressure_level(), BackpressureLevel::L3);
            assert!(bus.backpressure_signal().pause_publishers);
            assert!(!bus.can_poll());

            let blocked = bus
                .publish(Event::new(
                    "source:b",
                    EventType::MessageReceived,
                    json!({"queued": false}),
                ))
                .await
                .expect_err("guaranteed-delivery event should be blocked at level 3");
            assert!(matches!(
                blocked,
                Error::BackpressureBlocked {
                    level: BackpressureLevel::L3
                }
            ));

            bus.ensure_drained().await.expect("drain queue");

            assert_eq!(bus.backpressure_level(), BackpressureLevel::Normal);
            assert!(bus.can_poll());
            bus.publish(Event::new(
                "source:b",
                EventType::MessageReceived,
                json!({"queued": true}),
            ))
            .await
            .expect("publish should succeed after recovery");
        });
    }

    #[test]
    fn records_retry_later_block_events() {
        pollster::block_on(async {
            let bus = InMemoryBus::new(InMemoryBusConfig {
                max_queue_size: 20,
                level1_threshold: 0.80,
                level2_threshold: 0.90,
                level3_threshold: 0.95,
                level4_threshold: 0.98,
                ..InMemoryBusConfig::default()
            });

            {
                let mut state = bus.lock_state();
                while state.queue.len() < 19 {
                    let fill = state.queue.len();
                    state.queue.push(Event::new(
                        "source:a",
                        EventType::TimerTick,
                        json!({"fill": fill}),
                    ));
                }
                bus.refresh_signal(&mut state);
            }

            let blocked = Event::new(
                "source:b",
                EventType::MessageReceived,
                json!({"retry": true}),
            );
            let blocked_id = blocked.id.to_string();
            let error = bus
                .publish(blocked)
                .await
                .expect_err("publish should be blocked at level 3");

            assert!(matches!(
                error,
                Error::BackpressureBlocked {
                    level: BackpressureLevel::L3
                }
            ));

            let events = bus.backpressure_events();
            let block_record = events
                .iter()
                .find(|record| matches!(record.kind, BackpressureEventKind::BlockedForRetryLater))
                .expect("block record should exist");
            assert_eq!(block_record.level, BackpressureLevel::L3);
            assert_eq!(block_record.event_id.as_deref(), Some(blocked_id.as_str()));
            assert_eq!(block_record.source.as_deref(), Some("source:b"));
        });
    }

    #[test]
    fn publish_returns_handler_error() {
        pollster::block_on(async {
            let bus = InMemoryBus::default();
            bus.subscribe(SubscriptionFilter::default(), Box::new(FailingHandler))
                .await
                .expect("subscribe");

            let error = bus
                .publish(Event::new("timer:a", EventType::TimerTick, json!({})))
                .await
                .expect_err("publish should surface handler errors");

            assert!(matches!(error, Error::Unrecoverable { .. }));
        });
    }

    #[test]
    fn full_queue_blocks_guaranteed_delivery_publish() {
        pollster::block_on(async {
            let bus = InMemoryBus::new(InMemoryBusConfig {
                max_queue_size: 1,
                ..InMemoryBusConfig::default()
            });

            {
                let mut state = bus.lock_state();
                state.queue.push(Event::new(
                    "source:a",
                    EventType::MessageReceived,
                    json!({"queued": true}),
                ));
                bus.refresh_signal(&mut state);
            }

            let error = bus
                .publish(Event::new(
                    "source:b",
                    EventType::MessageReceived,
                    json!({"queued": false}),
                ))
                .await
                .expect_err("second event should be rejected");

            assert!(matches!(
                error,
                Error::BackpressureBlocked {
                    level: BackpressureLevel::Critical
                }
            ));
        });
    }

    #[test]
    fn level4a_overflows_guaranteed_delivery_to_disk() {
        use std::fs;

        pollster::block_on(async {
            let overflow_dir = std::env::temp_dir().join(format!(
                "aman-bus-l4a-test-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));

            // Use queue_size=100 so 98 events = 98% = L4A, 99 = 99% = L4A, 100 = 100% = Critical
            let bus = InMemoryBus::new(InMemoryBusConfig {
                max_queue_size: 100,
                overflow_dir: Some(overflow_dir.clone()),
                overflow_max_bytes: 1_048_576, // 1 MB
                level1_threshold: 0.80,
                level2_threshold: 0.90,
                level3_threshold: 0.95,
                level4_threshold: 0.98,
                ..InMemoryBusConfig::default()
            });

            // Fill queue to Level 4A (98%) = 98 events
            {
                let mut state = bus.lock_state();
                for i in 0..98 {
                    state.queue.push(Event::new(
                        "source:a",
                        EventType::TimerTick,
                        json!({"fill": i}),
                    ));
                }
                bus.refresh_signal(&mut state);
            }

            assert_eq!(bus.backpressure_level(), BackpressureLevel::L4A);

            // AtLeastOnce event should be overflowed to disk
            let at_least_once = Event::new(
                "source:b",
                EventType::MessageReceived,
                json!({"overflow": true}),
            )
            .with_delivery(DeliveryGuarantee::AtLeastOnce);

            bus.publish(at_least_once)
                .await
                .expect("publish should succeed via overflow");

            // Verify file written to overflow dir
            let files: Vec<_> = fs::read_dir(&overflow_dir)
                .expect("read overflow dir")
                .filter_map(|e| e.ok())
                .filter(|e| e.path().extension().map_or(false, |ext| ext == "json"))
                .collect();
            assert_eq!(files.len(), 1, "one event should be overflowed to disk");

            // Verify event can be recovered
            let recovered = bus.recover_overflow().expect("recover overflow");
            assert_eq!(recovered, 1);

            let _ = fs::remove_dir_all(&overflow_dir);
        });
    }

    #[test]
    fn level4b_emergency_fallback_when_overflow_dir_near_capacity() {
        use std::fs;

        pollster::block_on(async {
            let overflow_dir = std::env::temp_dir().join(format!(
                "aman-bus-l4b-test-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));

            // max_bytes = 200 bytes — one event JSON will exceed 80%
            let bus = InMemoryBus::new(InMemoryBusConfig {
                max_queue_size: 100,
                overflow_dir: Some(overflow_dir.clone()),
                overflow_max_bytes: 200,
                level1_threshold: 0.80,
                level2_threshold: 0.90,
                level3_threshold: 0.95,
                level4_threshold: 0.98,
                ..InMemoryBusConfig::default()
            });

            // Fill queue to Level 4A
            {
                let mut state = bus.lock_state();
                for i in 0..98 {
                    state.queue.push(Event::new(
                        "source:a",
                        EventType::TimerTick,
                        json!({"fill": i}),
                    ));
                }
                bus.refresh_signal(&mut state);
            }

            assert_eq!(bus.backpressure_level(), BackpressureLevel::L4A);

            // First AtLeastOnce event should succeed (writes to disk)
            let first = Event::new(
                "source:b",
                EventType::MessageReceived,
                json!({"seq": 1}),
            )
            .with_delivery(DeliveryGuarantee::AtLeastOnce);

            bus.publish(first)
                .await
                .expect("first overflow should succeed");

            // Second event: overflow dir should be >80% → L4B emergency fallback
            let second = Event::new(
                "source:c",
                EventType::MessageReceived,
                json!({"seq": 2}),
            )
            .with_delivery(DeliveryGuarantee::AtLeastOnce);

            let error = bus
                .publish(second)
                .await
                .expect_err("second overflow should fail with L3 block");

            assert!(matches!(
                error,
                Error::BackpressureBlocked {
                    level: BackpressureLevel::L3
                }
            ));

            // Verify emergency event recorded
            let events = bus.backpressure_events();
            assert!(
                events
                    .iter()
                    .any(|r| matches!(r.kind, BackpressureEventKind::OverflowDirEmergency)),
                "should record overflow dir emergency"
            );

            let _ = fs::remove_dir_all(&overflow_dir);
        });
    }

    #[test]
    fn level5_stops_low_priority_at_critical() {
        pollster::block_on(async {
            let bus = InMemoryBus::new(InMemoryBusConfig {
                max_queue_size: 100,
                ..InMemoryBusConfig::default()
            });

            // Fill to 100% → Critical
            {
                let mut state = bus.lock_state();
                for i in 0..100 {
                    state.queue.push(Event::new(
                        "source:a",
                        EventType::TimerTick,
                        json!({"fill": i}),
                    ));
                }
                bus.refresh_signal(&mut state);
            }

            assert_eq!(bus.backpressure_level(), BackpressureLevel::Critical);

            // Low priority event should be stopped/discarded
            let mut low = Event::new(
                "source:b",
                EventType::Heartbeat,
                json!({"low": true}),
            );
            low.priority = Priority::Low;

            {
                let mut state = bus.lock_state();
                let admission = bus.admit_event(low, &mut state).expect("admit");
                assert!(
                    matches!(admission, PublishAdmission::Drop),
                    "low priority should be dropped at Critical"
                );
                assert_eq!(state.discarded_count, 1);
            }

            // Verify StoppedLowPriority event recorded
            let events = bus.backpressure_events();
            assert!(
                events
                    .iter()
                    .any(|r| matches!(r.kind, BackpressureEventKind::StoppedLowPriority { .. })),
                "should record stopped low priority"
            );
        });
    }

    #[test]
    fn recover_overflow_reinjects_and_deduplicates() {
        use std::fs;

        pollster::block_on(async {
            let overflow_dir = std::env::temp_dir().join(format!(
                "aman-recover-test-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));

            // Create overflow dir with events manually (simulating crash recovery)
            fs::create_dir_all(&overflow_dir).expect("create dir");

            let overflow =
                OverflowDir::new(&overflow_dir, 1_048_576).expect("create overflow dir");

            let mut event1 = Event::new(
                "source:a",
                EventType::FileCreated,
                json!({"file": "a.txt"}),
            );
            event1.timestamp = kernel::types::Timestamp::from_millis(1000);
            let mut event2 = Event::new(
                "source:a",
                EventType::FileCreated,
                json!({"file": "b.txt"}),
            );
            event2.timestamp = kernel::types::Timestamp::from_millis(2000);
            let mut event3 = Event::new(
                "source:b",
                EventType::MessageReceived,
                json!({"msg": "hello"}),
            );
            event3.timestamp = kernel::types::Timestamp::from_millis(1500);

            overflow.write_event(&event1).expect("write");
            overflow.write_event(&event2).expect("write");
            overflow.write_event(&event3).expect("write");

            // Create bus with same overflow dir
            let bus = InMemoryBus::new(InMemoryBusConfig {
                max_queue_size: 100,
                overflow_dir: Some(overflow_dir.clone()),
                overflow_max_bytes: 1_048_576,
                ..InMemoryBusConfig::default()
            });

            // Pre-mark event2 as already processed (dedup)
            {
                let mut state = bus.lock_state();
                state.dedup.check(&event2);
            }

            let recovered = bus.recover_overflow().expect("recover");
            assert_eq!(recovered, 3, "should find 3 overflow files");

            // Queue should have event1 and event3 (event2 was deduped)
            let metrics = bus.metrics();
            assert_eq!(
                metrics.queue_depth.high + metrics.queue_depth.normal + metrics.queue_depth.low,
                2,
                "only 2 events should be re-enqueued (1 deduped)"
            );

            // Overflow dir should be cleaned up
            let remaining: Vec<_> = fs::read_dir(&overflow_dir)
                .expect("read dir")
                .filter_map(|e| e.ok())
                .filter(|e| e.path().extension().map_or(false, |ext| ext == "json"))
                .collect();
            assert_eq!(remaining.len(), 0, "overflow dir should be empty after recovery");

            let _ = fs::remove_dir_all(&overflow_dir);
        });
    }

    #[test]
    fn enqueue_for_retry_schedules_with_backoff() {
        pollster::block_on(async {
            let bus = InMemoryBus::default();

            let event = Event::new(
                "source:retry",
                EventType::MessageReceived,
                json!({"retry": true}),
            );

            let result = bus
                .enqueue_for_retry(event.clone(), 0, None)
                .expect("enqueue");
            assert!(matches!(result, RetryScheduleResult::Queued { .. }));

            // Verify queue depth
            assert_eq!(bus.metrics().retry_queue_depth, 1);

            // Exhausted after max attempts
            let event2 = Event::new(
                "source:retry",
                EventType::MessageReceived,
                json!({"retry": true}),
            );
            let exhausted = bus
                .enqueue_for_retry(event2, 5, None)
                .expect("enqueue");
            assert!(matches!(exhausted, RetryScheduleResult::Exhausted { .. }));
        });
    }

    // ── M3.1: wait_for_event tests ───────────────────────────────

    #[tokio::test]
    async fn wait_for_event_returns_immediately_if_queue_non_empty() {
        let bus = InMemoryBus::default();
        // Push directly to queue (bypass publish() which would drain it)
        {
            let mut state = bus.lock_state();
            state.queue.push(Event::new(
                "source:a",
                EventType::TimerTick,
                json!({"seq": 1}),
            ));
        }

        let event = bus
            .wait_for_event(Duration::from_millis(5000))
            .await
            .expect("should get event immediately");
        assert_eq!(event.payload, json!({"seq": 1}));
    }

    #[tokio::test]
    async fn wait_for_event_blocks_until_event_arrives() {
        let bus = Arc::new(InMemoryBus::default());
        let bus_clone = Arc::clone(&bus);

        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            let mut state = bus_clone.lock_state();
            state.queue.push(Event::new(
                "source:b",
                EventType::TimerTick,
                json!({"seq": 2}),
            ));
            state.wait_for_event_generation += 1;
            bus_clone.event_notify.notify_one();
        });

        let event = bus
            .wait_for_event(Duration::from_secs(5))
            .await
            .expect("should get event from spawn");
        assert_eq!(event.payload, json!({"seq": 2}));
    }

    #[tokio::test]
    async fn wait_for_event_times_out_when_no_event() {
        let bus = InMemoryBus::default();
        let result = bus.wait_for_event(Duration::from_millis(10)).await;
        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), WaitForEventTimeout);
    }

    #[tokio::test]
    async fn wait_for_event_wakeup_metrics_incremented() {
        let bus = Arc::new(InMemoryBus::default());
        let bus_clone = Arc::clone(&bus);

        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            let mut state = bus_clone.lock_state();
            state.queue.push(Event::new(
                "source:c",
                EventType::MessageReceived,
                json!({"seq": 3}),
            ));
            state.wait_for_event_generation += 1;
            bus_clone.event_notify.notify_one();
        });

        let _event = bus
            .wait_for_event(Duration::from_secs(5))
            .await
            .expect("should get event");

        let metrics = bus.metrics();
        assert_eq!(
            metrics.wait_for_event_wakeups, 1,
            "should record one successful wakeup"
        );
    }

    // ── M3.2: Idle event backpressure tests ──────────────────────

    #[test]
    fn idle_event_discarded_at_l1_backpressure() {
        pollster::block_on(async {
            let bus = InMemoryBus::new(InMemoryBusConfig {
                max_queue_size: 10,
                level1_threshold: 0.80,
                ..InMemoryBusConfig::default()
            });

            // Fill queue to L1 (8/10 = 80%)
            {
                let mut state = bus.lock_state();
                for i in 0..8 {
                    state
                        .queue
                        .push(Event::new("source:a", EventType::TimerTick, json!({"fill": i})));
                }
                bus.refresh_signal(&mut state);
            }

            // Idle event should be discarded at L1
            let idle = Event::new("idle.system", EventType::Idle, json!({}));
            let admission = {
                let mut state = bus.lock_state();
                bus.admit_event(idle, &mut state).expect("admit idle")
            };
            assert!(
                matches!(admission, PublishAdmission::Drop),
                "idle event should be dropped at L1 backpressure"
            );
            assert_eq!(
                bus.metrics().idle_events_discarded, 1,
                "idle_discarded_count should be 1"
            );
        });
    }

    #[test]
    fn idle_event_not_discarded_at_normal_backpressure() {
        pollster::block_on(async {
            let bus = InMemoryBus::default();
            let idle = Event::new("idle.system", EventType::Idle, json!({}));
            let admission = {
                let mut state = bus.lock_state();
                bus.admit_event(idle, &mut state).expect("admit idle")
            };
            assert!(
                matches!(admission, PublishAdmission::Accept(_)),
                "idle event should be accepted at Normal backpressure"
            );
            assert_eq!(
                bus.metrics().idle_events_discarded, 0,
                "no idle events should be discarded at Normal"
            );
        });
    }

    #[test]
    fn non_idle_event_not_affected_by_idle_discard() {
        pollster::block_on(async {
            let bus = InMemoryBus::new(InMemoryBusConfig {
                max_queue_size: 10,
                level1_threshold: 0.80,
                ..InMemoryBusConfig::default()
            });

            // Fill queue to L1
            {
                let mut state = bus.lock_state();
                for i in 0..8 {
                    state
                        .queue
                        .push(Event::new("source:a", EventType::TimerTick, json!({"fill": i})));
                }
                bus.refresh_signal(&mut state);
            }

            // Non-idle, AtMostOnce event should still be dropped by normal backpressure
            let mut normal = Event::new("source:b", EventType::Heartbeat, json!({}));
            normal.delivery = DeliveryGuarantee::AtMostOnce;
            let admission = {
                let mut state = bus.lock_state();
                bus.admit_event(normal, &mut state).expect("admit normal")
            };
            // At L1 with AtMostOnce: gets degraded priority but NOT dropped (L2+ threshold)
            // Actually at L1 the apply_degradation() downgrades priority but doesn't drop
            // Let's check: should_drop only at L2+
            // So at L1, the event should be accepted (just degraded)
            assert!(
                matches!(admission, PublishAdmission::Accept(_)),
                "non-idle event should not be caught by idle discard"
            );
        });
    }

    #[test]
    fn idle_event_discarded_ignores_guaranteed_delivery() {
        pollster::block_on(async {
            let bus = InMemoryBus::new(InMemoryBusConfig {
                max_queue_size: 10,
                level1_threshold: 0.80,
                ..InMemoryBusConfig::default()
            });

            // Fill queue to L2 (9/10 = 90%)
            {
                let mut state = bus.lock_state();
                for i in 0..9 {
                    state
                        .queue
                        .push(Event::new("source:a", EventType::TimerTick, json!({"fill": i})));
                }
                bus.refresh_signal(&mut state);
            }

            // Idle event with AtLeastOnce delivery should still be discarded
            let mut idle = Event::new("idle.system", EventType::Idle, json!({}));
            idle.delivery = DeliveryGuarantee::AtLeastOnce;
            let admission = {
                let mut state = bus.lock_state();
                bus.admit_event(idle, &mut state).expect("admit idle")
            };
            assert!(
                matches!(admission, PublishAdmission::Drop),
                "idle event should be dropped regardless of delivery guarantee"
            );
        });
    }

    #[test]
    fn idle_event_discarded_at_critical() {
        pollster::block_on(async {
            let bus = InMemoryBus::new(InMemoryBusConfig {
                max_queue_size: 10,
                ..InMemoryBusConfig::default()
            });

            // Fill to 100% → Critical
            {
                let mut state = bus.lock_state();
                for i in 0..10 {
                    state
                        .queue
                        .push(Event::new("source:a", EventType::TimerTick, json!({"fill": i})));
                }
                bus.refresh_signal(&mut state);
            }

            let idle = Event::new("idle.system", EventType::Idle, json!({}));
            let admission = {
                let mut state = bus.lock_state();
                bus.admit_event(idle, &mut state).expect("admit idle")
            };
            assert!(
                matches!(admission, PublishAdmission::Drop),
                "idle event should be dropped at Critical"
            );
            assert_eq!(
                bus.metrics().idle_events_discarded, 1,
                "idle_discarded_count should be 1"
            );
        });
    }
}
