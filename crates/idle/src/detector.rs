//! IdleDetector — EventSource 实现。
//!
//! Architecture ref: idle-design.md §5.3

use async_trait::async_trait;
use kernel::context::SourceContext;
use kernel::event::Event;
use kernel::source::EventSource;
use kernel::types::{HealthStatus, SourceType};
use kernel::AmanResult;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Instant;
use tracing::debug;

use crate::coordination::IdleCoordination;
use crate::types::{
    IdleContext, IdleEvent, IdleKind, IdlePersonality,
};

/// IdleDetector — EventSource that produces IdleEvents when the queue is idle.
///
/// Produces a depth-driven sequence: Daze → Boredom → Sleep → Exploration → Meditation.
/// The personality switches between full and chat-modes based on
/// [`IdleCoordination::last_source_type`].
pub struct IdleDetector {
    id: String,
    coord: Arc<IdleCoordination>,
    personality: IdlePersonality,
    /// Current idle depth (0 = just entered idle, incremented each poll)
    pub(crate) idle_depth: u32,
    /// True if the last effective_personality call was in chat mode
    pub(crate) was_in_chat_mode: bool,
    /// Timestamp of the last chat event (for grace period tracking)
    pub(crate) last_chat_seen: Option<Instant>,
    /// Timestamp of the last event produced by poll()
    pub(crate) last_poll: Option<Instant>,
    /// Ring buffer of recent idle outputs (max 10)
    pub(crate) last_idle_outputs: Vec<String>,
}

impl IdleDetector {
    #[must_use]
    pub fn new(
        id: impl Into<String>,
        coord: Arc<IdleCoordination>,
        personality: IdlePersonality,
    ) -> Self {
        Self {
            id: id.into(),
            coord,
            personality,
            idle_depth: 0,
            was_in_chat_mode: false,
            last_chat_seen: None,
            last_poll: None,
            last_idle_outputs: Vec::new(),
        }
    }

    /// Returns a clone of the configured personality.
    #[must_use]
    pub fn personality(&self) -> IdlePersonality {
        self.personality.clone()
    }

    /// Determine which personality to use (T5.2).
    ///
    /// If the last source was Chat and we're within the grace period,
    /// use the ChatMode-restricted personality. When leaving chat mode,
    /// reset idle_depth to 0 (R3-1 correction).
    pub(crate) fn effective_personality(&mut self) -> IdlePersonality {
        let source_u8 = self.coord.last_source_type.load(Ordering::Relaxed);
        let is_chat = source_u8 == SourceType::Chat.to_u8();

        if is_chat {
            let now = Instant::now();
            let elapsed_since_chat = self
                .last_chat_seen
                .map(|t| now.duration_since(t).as_secs())
                .unwrap_or(0);

            self.last_chat_seen = Some(now);
            self.was_in_chat_mode = true;

            if elapsed_since_chat < self.personality.chat_mode.grace_period_secs {
                return self.personality.chat_mode.as_personality();
            }

            // Grace period expired — use full personality
            return self.personality.clone();
        }

        // Not in chat mode
        if self.was_in_chat_mode {
            // Just left chat mode — reset depth (R3-1)
            self.idle_depth = 0;
            self.was_in_chat_mode = false;
        }

        self.personality.clone()
    }
}

#[async_trait]
impl EventSource for IdleDetector {
    fn id(&self) -> &str {
        &self.id
    }

    fn source_type(&self) -> SourceType {
        SourceType::Custom
    }

    async fn init(&mut self, _ctx: SourceContext) -> AmanResult<()> {
        debug!(id = %self.id, "IdleDetector initialized");
        Ok(())
    }

    async fn shutdown(&mut self) -> AmanResult<()> {
        debug!(id = %self.id, "IdleDetector shut down");
        Ok(())
    }

    /// Poll for idle events (T5.1, T5.3, T5.4).
    ///
    /// 1. Skip if Dispatcher is reflecting (`busy_reflecting`).
    /// 2. Reset depth if a real event was seen.
    /// 3. Determine effective personality (chat vs full).
    /// 4. Resolve IdleKind from current depth.
    /// 5. Build and return an IdleEvent.
    async fn poll(&mut self, _ctx: &SourceContext) -> AmanResult<Vec<Event>> {
        // T5.1: Skip if Dispatcher is reflecting
        if self.coord.busy_reflecting.load(Ordering::Relaxed) {
            return Ok(Vec::new());
        }

        // T5.4: Check if the queue was recently drained (depth reset pending)
        if self.coord.pending_depth_reset.swap(false, Ordering::SeqCst) {
            self.idle_depth = 0;
            return Ok(Vec::new());
        }

        // Determine effective personality (T5.2)
        let effective = self.effective_personality();

        // Throttle: respect the poll_interval between consecutive events.
        let delay_secs = effective.poll_interval.next_delay(self.idle_depth);
        if let Some(last) = self.last_poll
            && last.elapsed().as_secs_f64() < delay_secs
        {
            return Ok(Vec::new());
        }

        // T5.3: Resolve IdleKind from depth + arousal (two-axis model)
        let kind = if self.idle_depth == 0 {
            IdleKind::Daze
        } else {
            let arousal = self.coord.arousal.current();
            effective.resolve_with_arousal(self.idle_depth, arousal)
        };

        // Build IdleContext
        let context = IdleContext {
            last_event_type: String::new(),
            last_idle_outputs: self.last_idle_outputs.clone(),
            arousal_level: self.coord.arousal.current(),
        };

        let duration_secs = effective.poll_interval.next_delay(self.idle_depth);

        let idle_event = IdleEvent {
            kind,
            depth: self.idle_depth,
            duration_secs,
            context: Some(context),
            from_chat_mode: self.was_in_chat_mode,
            agent_id: None,
        };

        // Apply arousal behavior for this idle kind
        self.coord.arousal.apply_behavior(kind.arousal_behavior());

        let event: Event = idle_event.into();
        self.idle_depth = self.idle_depth.saturating_add(1);
        self.last_poll = Some(Instant::now());

        debug!(
            id = %self.id,
            depth = self.idle_depth - 1,
            kind = ?kind,
            from_chat = self.was_in_chat_mode,
            "IdleDetector produced event"
        );

        Ok(vec![event])
    }

    fn health(&self) -> HealthStatus {
        if self.idle_depth < 5 {
            HealthStatus::Ok
        } else if self.idle_depth < 20 {
            HealthStatus::Degraded
        } else {
            HealthStatus::Failed
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::coordination::IdleCoordination;
    use crate::types::{
        ChatMode, ContextIsolation, PollInterval, ReflectionBreaker,
    };

    fn test_personality() -> IdlePersonality {
        IdlePersonality {
            enabled_kinds: vec![
                IdleKind::Daze,
                IdleKind::Boredom,
                IdleKind::Sleep,
                IdleKind::Exploration,
            ],
            depth_schedule: vec![
                (0, IdleKind::Daze),
                (1, IdleKind::Boredom),
                (3, IdleKind::Sleep),
                (5, IdleKind::Exploration),
            ],
            poll_interval: PollInterval::Fixed { interval_secs: 0.0 },
            poll_relaxation: crate::types::PollRelaxation::None,
            chat_mode: ChatMode {
                allowed_kinds: vec![IdleKind::Daze, IdleKind::Boredom],
                grace_period_secs: 60,
                poll_interval: PollInterval::Fixed { interval_secs: 0.0 },
            },
            reflection_breaker: ReflectionBreaker {
                max_consecutive: 5,
                cooldown_secs: 300,
            },
            context_isolation: ContextIsolation {
                pollute_chat_history: false,
                suspend_on_user_input: true,
            },
        }
    }

    fn make_source_context() -> SourceContext {
        SourceContext::default()
    }

    fn make_detector() -> IdleDetector {
        let coord = Arc::new(IdleCoordination::new(1.0, 900.0));
        IdleDetector::new("idle:detector", coord, test_personality())
    }

    // ── T5.1: Basic poll and busy_reflecting ──────────────────────

    #[tokio::test]
    async fn poll_returns_idle_event_when_not_busy() {
        let mut detector = make_detector();
        let ctx = make_source_context();
        let events = detector.poll(&ctx).await.expect("poll");
        assert_eq!(events.len(), 1);
        assert!(events[0].is_idle_event());
        assert_eq!(events[0].priority, kernel::types::Priority::Low);
        assert_eq!(events[0].source.as_str(), "idle.system");
    }

    #[tokio::test]
    async fn poll_skips_when_busy_reflecting() {
        let coord = Arc::new(IdleCoordination::new(1.0, 900.0));
        coord.busy_reflecting.store(true, Ordering::Relaxed);
        let mut detector = IdleDetector::new("idle:detector", coord, test_personality());
        let events = detector.poll(&make_source_context()).await.expect("poll");
        assert!(events.is_empty());
    }

    // ── T5.3: Depth progression ──────────────────────────────────

    #[tokio::test]
    async fn depth_0_is_daze() {
        let mut detector = make_detector();
        let ctx = make_source_context();
        let events = detector.poll(&ctx).await.expect("poll");
        assert_eq!(events.len(), 1);
        assert!(events[0].is_idle_event());
    }

    #[tokio::test]
    async fn consecutive_polls_increment_depth() {
        let mut detector = make_detector();
        let ctx = make_source_context();

        for expected_depth in 0..4 {
            let events = detector.poll(&ctx).await.expect("poll");
            assert_eq!(events.len(), 1, "depth {expected_depth} should produce event");
        }
        // After 4 polls at depth 0,1,2,3 → next poll is depth 4 (still Daze with widened schedule)
        let events = detector.poll(&ctx).await.expect("poll");
        assert_eq!(events.len(), 1);
    }

    // ── T5.4: pending_depth_reset (queue drained → reset depth) ──────

    #[tokio::test]
    async fn queue_drained_resets_depth() {
        let coord = Arc::new(IdleCoordination::new(1.0, 900.0));
        let mut detector = IdleDetector::new("idle:detector", coord.clone(), test_personality());
        let ctx = make_source_context();

        // Advance depth to 3
        for _ in 0..3 {
            detector.poll(&ctx).await.expect("poll");
        }
        assert_eq!(detector.idle_depth, 3);

        // Signal queue drained (replaces old real_event_seen mechanism)
        coord.pending_depth_reset.store(true, Ordering::SeqCst);

        // Poll should reset depth and not produce an event
        let events = detector.poll(&ctx).await.expect("poll");
        assert!(events.is_empty());
        assert_eq!(detector.idle_depth, 0);

        // Next poll should produce Daze (depth 0)
        let events = detector.poll(&ctx).await.expect("poll");
        assert_eq!(events.len(), 1);
    }

    // ── T5.2: Chat mode switching ────────────────────────────────

    #[tokio::test]
    async fn effective_personality_chat_mode_in_grace_period() {
        let coord = Arc::new(IdleCoordination::new(1.0, 900.0));
        coord.last_source_type.store(SourceType::Chat.to_u8(), Ordering::Relaxed);
        let mut detector = IdleDetector::new("idle:detector", coord, test_personality());

        let ep = detector.effective_personality();
        // Chat mode: depth 0 → Daze, depth 1+ → Boredom (threshold match)
        assert_eq!(ep.resolve(0), IdleKind::Daze);
        assert_eq!(ep.resolve(1), IdleKind::Boredom);
        assert_eq!(ep.resolve(2), IdleKind::Boredom);
        assert_eq!(ep.resolve(3), IdleKind::Boredom);
    }

    #[tokio::test]
    async fn leaving_chat_mode_resets_depth() {
        let coord = Arc::new(IdleCoordination::new(1.0, 900.0));
        coord.last_source_type.store(SourceType::Chat.to_u8(), Ordering::Relaxed);
        let mut detector = IdleDetector::new("idle:detector", coord.clone(), test_personality());

        // Enter chat mode
        let _ = detector.effective_personality();
        assert!(detector.was_in_chat_mode);
        detector.idle_depth = 5;

        // Leave chat mode (non-chat source)
        coord.last_source_type.store(SourceType::Custom.to_u8(), Ordering::Relaxed);

        let _ = detector.effective_personality();
        assert_eq!(detector.idle_depth, 0, "R3-1: depth reset on chat exit");
        assert!(!detector.was_in_chat_mode);
    }

    // ── Health ───────────────────────────────────────────────────

    #[tokio::test]
    async fn health_reflects_idle_depth() {
        let mut detector = make_detector();
        assert_eq!(detector.health(), HealthStatus::Ok);

        detector.idle_depth = 10;
        assert_eq!(detector.health(), HealthStatus::Degraded);

        detector.idle_depth = 25;
        assert_eq!(detector.health(), HealthStatus::Failed);
    }
}
