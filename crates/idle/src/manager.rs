//! Per-agent idle manager — bundles coordination, detection, and a background loop
//! that monitors the agent's local event bus and publishes idle events to it.
//!
//! Architecture: each agent gets its own AgentIdleManager. The manager spawns a
//! dedicated tokio task that monitors the agent's local bus for activity and
//! progresses through idle depth states (Daze → Boredom → Sleep → …).
//! This replaces the previous global IdleDetector+SourceRegistry pattern.

use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Instant;
use tokio::task::JoinHandle;
use tokio::time::{sleep, Duration};
use tokio_util::sync::CancellationToken;
use tracing::debug;

use event_bus::EventBus;
use kernel::AmanResult;

use crate::coordination::IdleCoordination;
use crate::detector::IdleDetector;
use crate::incubation::IncubationManager;
use crate::types::{IdleContext, IdleEvent, IdleKind, IdlePersonality};

/// Manages the full idle lifecycle for a single agent.
///
/// Spawns a background task that:
/// 1. Monitors the agent's local event bus for activity
/// 2. When the bus is empty, progresses idle depth and publishes idle events
/// 3. When real events arrive (bus non-empty), resets idle depth
pub struct AgentIdleManager {
    agent_id: String,
    /// Per-agent idle coordination (arousal, cancel token, depth reset, etc.)
    coord: Arc<IdleCoordination>,
    /// Configured idle personality
    personality: IdlePersonality,
    /// The agent's local event bus — idle events are published here
    local_bus: Arc<dyn EventBus>,
    /// Per-agent incubation manager for background idle threads
    incubation: Arc<IncubationManager>,
    /// Stop signal for the background idle loop
    stop_token: CancellationToken,
    /// Handle for the background idle loop task
    task: tokio::sync::Mutex<Option<JoinHandle<()>>>,
}

impl AgentIdleManager {
    /// Create a new per-agent idle manager.
    #[must_use]
    pub fn new(
        agent_id: impl Into<String>,
        local_bus: Arc<dyn EventBus>,
        personality: IdlePersonality,
        arousal_initial: f64,
        arousal_half_life_secs: f64,
    ) -> Self {
        let agent_id = agent_id.into();
        let coord = Arc::new(IdleCoordination::new(arousal_initial, arousal_half_life_secs));

        Self {
            agent_id,
            coord,
            personality,
            local_bus,
            incubation: Arc::new(IncubationManager::new()),
            stop_token: CancellationToken::new(),
            task: tokio::sync::Mutex::new(None),
        }
    }

    /// Returns a reference to this agent's idle coordination for cross-component use.
    #[must_use]
    pub fn coordination(&self) -> &Arc<IdleCoordination> {
        &self.coord
    }

    /// Returns a reference to this agent's incubation manager.
    #[must_use]
    pub fn incubation(&self) -> &Arc<IncubationManager> {
        &self.incubation
    }

    /// Start the background idle detection loop.
    ///
    /// The loop runs until `stop()` is called. Safe to call multiple times —
    /// subsequent calls are no-ops if already running.
    pub async fn start(&self) {
        let mut task_slot = self.task.lock().await;
        if task_slot.is_some() {
            return;
        }

        let agent_id = self.agent_id.clone();
        let coord = Arc::clone(&self.coord);
        let personality = self.personality.clone();
        let local_bus = Arc::clone(&self.local_bus);
        let stop_token = self.stop_token.clone();

        *task_slot = Some(tokio::spawn(async move {
            let mut detector = IdleDetector::new(
                format!("idle:detector:{agent_id}"),
                Arc::clone(&coord),
                personality,
            );

            loop {
                if stop_token.is_cancelled() {
                    break;
                }

                // Skip if busy_reflecting is set
                if coord.busy_reflecting.load(Ordering::Relaxed) {
                    sleep(Duration::from_millis(100)).await;
                    continue;
                }

                // Check if depth reset is pending (queue was drained)
                if coord.pending_depth_reset.swap(false, Ordering::SeqCst) {
                    detector.idle_depth = 0;
                    sleep(Duration::from_millis(100)).await;
                    continue;
                }

                // Determine effective personality (chat vs full mode)
                let effective = detector.effective_personality();

                // Throttle: respect poll_interval
                let delay_secs = effective.poll_interval.next_delay(detector.idle_depth);
                if let Some(last) = detector.last_poll {
                    if last.elapsed().as_secs_f64() < delay_secs {
                        sleep(Duration::from_millis(50)).await;
                        continue;
                    }
                }

                // Check if agent's local bus has pending (non-idle) events
                let metrics = local_bus.metrics();
                let pending = metrics.queue_depth.high
                    + metrics.queue_depth.normal
                    + metrics.queue_depth.low;

                if pending > 0 {
                    detector.idle_depth = 0;
                    sleep(Duration::from_millis(100)).await;
                    continue;
                }

                // Bus is empty — progress idle state
                let kind = if detector.idle_depth == 0 {
                    IdleKind::Daze
                } else {
                    let arousal = coord.arousal.current();
                    effective.resolve_with_arousal(detector.idle_depth, arousal)
                };

                let context = IdleContext {
                    last_event_type: String::new(),
                    last_idle_outputs: detector.last_idle_outputs.clone(),
                    arousal_level: coord.arousal.current(),
                };

                let idle_event = IdleEvent {
                    kind,
                    depth: detector.idle_depth,
                    duration_secs: effective.poll_interval.next_delay(detector.idle_depth),
                    context: Some(context),
                    from_chat_mode: detector.was_in_chat_mode,
                };

                // Apply arousal behavior for this idle kind
                coord.arousal.apply_behavior(kind.arousal_behavior());

                let event: kernel::event::Event = idle_event.into();
                detector.idle_depth = detector.idle_depth.saturating_add(1);
                detector.last_poll = Some(Instant::now());

                debug!(
                    agent_id = %agent_id,
                    depth = detector.idle_depth - 1,
                    kind = ?kind,
                    "AgentIdleManager produced idle event"
                );

                let _ = local_bus.publish(event).await;
            }
        }));
    }

    /// Stop the background idle detection loop.
    pub async fn stop(&self) {
        self.stop_token.cancel();
        let mut task_slot = self.task.lock().await;
        if let Some(handle) = task_slot.take() {
            handle.abort();
        }
    }

    /// Full shutdown: cancel idle workflows, stop incubation, stop the idle loop.
    pub async fn shutdown(&self) -> AmanResult<()> {
        let cancelled = self.incubation.shutdown_all().await;
        if cancelled > 0 {
            tracing::info!(
                agent_id = %self.agent_id,
                cancelled,
                "agent idle incubation threads cancelled"
            );
        }
        self.coord.reset_idle_signal().await;
        self.stop().await;
        Ok(())
    }
}
