// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

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
use tracing::{debug, info};

use event_bus::EventBus;
use kernel::agent::AgentSystemState;
use kernel::AmanResult;

use crate::boredom::BoredomActor;
use crate::coordination::IdleCoordination;
use crate::detector::IdleDetector;
use crate::incubation::IncubationManager;
use crate::types::{IdleContext, IdleEvent, IdleKind, IdlePersonality, QueueDrained};

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
    /// Optional global event bus — idle events are also published here so the
    /// UI (Tauri event bridge) can observe per-agent idle state.
    global_bus: Option<Arc<dyn EventBus>>,
    /// Shared system state for UI visibility — set to Idle when idle depth > 0.
    system_state: Option<Arc<std::sync::Mutex<AgentSystemState>>>,
    /// Per-agent incubation manager for background idle threads
    incubation: Arc<IncubationManager>,
    /// Optional boredom actor for random tag selection
    boredom_actor: Option<Arc<BoredomActor>>,
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
        global_bus: Option<Arc<dyn EventBus>>,
        personality: IdlePersonality,
        arousal_initial: f64,
        arousal_half_life_secs: f64,
        system_state: Option<Arc<std::sync::Mutex<AgentSystemState>>>,
        boredom_actor: Option<Arc<BoredomActor>>,
    ) -> Self {
        let agent_id = agent_id.into();
        let coord = Arc::new(IdleCoordination::new(arousal_initial, arousal_half_life_secs));

        Self {
            agent_id,
            coord,
            personality,
            local_bus,
            global_bus,
            system_state,
            incubation: Arc::new(IncubationManager::new()),
            boredom_actor,
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
        let global_bus = self.global_bus.clone();
        let system_state = self.system_state.clone();
        let stop_token = self.stop_token.clone();
        let boredom_actor = self.boredom_actor.clone();

        *task_slot = Some(tokio::spawn(async move {
            let mut detector = IdleDetector::new(
                format!("idle:detector:{agent_id}"),
                Arc::clone(&coord),
                personality,
            );

            // Track busy→empty transitions for QueueDrained production.
            let mut was_busy = false;
            let mut reflection_count: u32 = 0;
            // Circuit breaker: skip QueueDrained when count exceeds threshold.
            const BREAKER_THRESHOLD: u32 = 20;
            // Cold-start: produce a QueueDrained if the bus stays empty for this
            // long after startup with no prior QueueDrained (see idle-design.md §4.1).
            // Permanently disabled after any QueueDrained is produced (cold-start
            // or busy→empty), since subsequent transitions are handled normally.
            let mut cold_start_done = false;
            let mut cold_start_deadline: Option<Instant> = None;
            const COLD_START_DELAY_SECS: u64 = 5;

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
                if let Some(last) = detector.last_poll
                    && last.elapsed().as_secs_f64() < delay_secs
                {
                    sleep(Duration::from_millis(50)).await;
                    continue;
                }

                // Check if agent's local bus has pending (non-idle) events
                let metrics = local_bus.metrics();
                let pending = metrics.queue_depth.high
                    + metrics.queue_depth.normal
                    + metrics.queue_depth.low;

                if pending > 0 {
                    // Bus is busy — reset idle depth, note that we were busy
                    was_busy = true;
                    reflection_count = 0; // reset circuit breaker on real activity
                    detector.idle_depth = 0;
                    sleep(Duration::from_millis(100)).await;
                    continue;
                }

                // Bus is empty. If we were previously busy, produce QueueDrained.
                if was_busy {
                    was_busy = false;
                    cold_start_done = true; // no longer need cold-start QD
                    detector.idle_depth = 0;
                    detector.last_poll = Some(Instant::now());

                    // Circuit breaker: skip if too many consecutive reflections
                    if reflection_count < BREAKER_THRESHOLD {
                        let qd = QueueDrained {
                            last_event_type: String::new(),
                            last_trace_id: String::new(),
                            last_result_summary: String::new(),
                            arousal_level: coord.arousal.current(),
                            reflection_consecutive_count: reflection_count,
                            agent_id: Some(agent_id.clone()),
                        };
                        reflection_count += 1;

                        let qd_event: kernel::event::Event = qd.into();
                        debug!(
                            agent_id = %agent_id,
                            reflection_count,
                            arousal = coord.arousal.current(),
                            "Producing QueueDrained event"
                        );
                        let _ = local_bus.publish(qd_event.clone()).await;
                        if let Some(ref global) = global_bus {
                            let _ = global.publish(qd_event).await;
                        }
                        // Agent has entered idle domain
                        if let Some(ref ss) = system_state {
                            let mut guard: std::sync::MutexGuard<'_, AgentSystemState> =
                                ss.lock().expect("system_state lock");
                            *guard = AgentSystemState::Idle;
                        }
                    } else {
                        info!(
                            agent_id = %agent_id,
                            reflection_count,
                            "QueueDrained circuit breaker: cooldown (skip)"
                        );
                        // Reset count after cooldown — next real event will also reset it
                    }

                    sleep(Duration::from_millis(100)).await;
                    continue;
                }

                // ── Cold-start QueueDrained ──────────────────────────────
                // If the agent starts with an empty queue (no prior busy→empty
                // transition), wait up to COLD_START_DELAY_SECS then produce a
                // synthetic QueueDrained so Reflection runs at least once before
                // entering the idle depth sequence. After the first QueueDrained
                // (cold-start or busy→empty), this branch is permanently disabled.
                if !cold_start_done {
                    let deadline = *cold_start_deadline.get_or_insert_with(|| {
                        Instant::now() + Duration::from_secs(COLD_START_DELAY_SECS)
                    });
                    if Instant::now() < deadline {
                        sleep(Duration::from_millis(500)).await;
                        continue;
                    }
                    // Deadline reached — produce cold-start QueueDrained.
                    cold_start_done = true;
                    cold_start_deadline = None;
                    if reflection_count < BREAKER_THRESHOLD {
                        let qd = QueueDrained {
                            last_event_type: String::new(),
                            last_trace_id: String::new(),
                            last_result_summary: String::new(),
                            arousal_level: coord.arousal.current(),
                            reflection_consecutive_count: reflection_count,
                            agent_id: Some(agent_id.clone()),
                        };
                        reflection_count += 1;
                        let qd_event: kernel::event::Event = qd.into();
                        info!(
                            agent_id = %agent_id,
                            "Cold-start QueueDrained (bus empty for {}s)",
                            COLD_START_DELAY_SECS
                        );
                        let _ = local_bus.publish(qd_event.clone()).await;
                        if let Some(ref global) = global_bus {
                            let _ = global.publish(qd_event).await;
                        }
                        // Agent has entered idle domain
                        if let Some(ref ss) = system_state {
                            let mut guard: std::sync::MutexGuard<'_, AgentSystemState> =
                                ss.lock().expect("system_state lock");
                            *guard = AgentSystemState::Idle;
                        }
                    }
                    sleep(Duration::from_millis(100)).await;
                    continue;
                }

                // Bus is empty, no recent activity — progress idle state
                let kind = if detector.idle_depth == 0 {
                    IdleKind::Daze
                } else {
                    let arousal = coord.arousal.current();
                    effective.resolve_with_arousal(detector.idle_depth, arousal)
                };

                // Cooldown check: skip publish entirely while kind is cooling down
                if coord.is_kind_on_cooldown(kind).await {
                    debug!(
                        agent_id = %agent_id,
                        ?kind,
                        depth = detector.idle_depth,
                        delay_secs,
                        "kind on cooldown, sleeping before next poll",
                    );
                    sleep(Duration::from_secs_f64(delay_secs)).await;
                    detector.last_poll = Some(Instant::now());
                    continue;
                }

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
                    agent_id: Some(agent_id.clone()),
                };

                // Apply arousal behavior for this idle kind
                coord.arousal.apply_behavior(kind.arousal_behavior());

                // Track boredom poll count
                if kind == IdleKind::Boredom {
                    detector.boredom_poll_count = detector.boredom_poll_count.saturating_add(1);
                } else {
                    detector.boredom_poll_count = 0;
                }

                let event: kernel::event::Event = idle_event.into();
                detector.idle_depth = detector.idle_depth.saturating_add(1);
                detector.last_poll = Some(Instant::now());

                debug!(
                    agent_id = %agent_id,
                    depth = detector.idle_depth - 1,
                    kind = ?kind,
                    boredom_poll = detector.boredom_poll_count,
                    "AgentIdleManager produced idle event"
                );

                // Publish to the agent's local bus for skill matching
                let _ = local_bus.publish(event.clone()).await;
                // Also publish to the global bus so the Tauri UI event bridge
                // can observe per-agent idle state
                if let Some(ref global) = global_bus {
                    let _ = global.publish(event).await;
                }

                // Boredom action: on trigger poll, pick and execute a skill
                if kind == IdleKind::Boredom {
                    if let Some(ref actor) = boredom_actor {
                        if let Some(tag) =
                            actor.try_act(detector.boredom_poll_count, &agent_id).await
                        {
                            // Notify the corresponding system state so the UI
                            // reflects what the agent is doing.
                            if let Some(ref ss) = system_state {
                                let state = match tag.as_str() {
                                    "work" => AgentSystemState::Working,
                                    "study" => AgentSystemState::Studying,
                                    "internet" | "entertainment" | "fun" => {
                                        AgentSystemState::DailyLife
                                    }
                                    _ => AgentSystemState::Idle,
                                };
                                *ss.lock().expect("system_state lock") = state;
                            }
                        }
                    }
                }
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

