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
use tracing::{debug, info, warn};

use event_bus::{try_publish, EventBus};
use kernel::agent::AgentSystemState;
use kernel::event::{Event, EventType};
use kernel::AmanResult;

use kernel::deferred_task::{current_time_ms, DeferredTaskQueue};

use crate::boredom::BoredomActor;
use crate::coordination::IdleCoordination;
use crate::detector::IdleDetector;
use crate::incubation::IncubationManager;
use crate::types::{IdleContext, IdleEvent, IdleKind, IdlePersonality, QueueDrained};

use rand::Rng;

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
    /// Optional deferred task queue (checked before random skill selection)
    deferred_queue: Option<Arc<dyn DeferredTaskQueue>>,
    /// Stop signal for the background idle loop
    stop_token: CancellationToken,
    /// Handle for the background idle loop task
    task: tokio::sync::Mutex<Option<JoinHandle<()>>>,
}

impl AgentIdleManager {
    /// Create a new per-agent idle manager.
    #[must_use]
    #[allow(clippy::too_many_arguments)] // Constructor aggregates distinct per-agent deps.
    pub fn new(
        agent_id: impl Into<String>,
        local_bus: Arc<dyn EventBus>,
        global_bus: Option<Arc<dyn EventBus>>,
        personality: IdlePersonality,
        arousal_initial: f64,
        arousal_half_life_secs: f64,
        system_state: Option<Arc<std::sync::Mutex<AgentSystemState>>>,
        boredom_actor: Option<Arc<BoredomActor>>,
        deferred_queue: Option<Arc<dyn DeferredTaskQueue>>,
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
            deferred_queue,
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

    /// Returns a reference to this agent's deferred task queue, if configured.
    #[must_use]
    pub fn deferred_queue(&self) -> Option<&Arc<dyn DeferredTaskQueue>> {
        self.deferred_queue.as_ref()
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
        let deferred_queue = self.deferred_queue.clone();

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
            // 在首次忙→空转换前，agent 不会产生 QueueDrained，因此也不会
            // 触发 reflection（session 提取 + memory.store → embed）。
            // 这避免了冷启动时多余的 reflection 争抢 embed_lock，
            // 从而防止与新到达的用户消息竞争导致的长时间 hang。
            // 注：曾经有冷启动合成 QueueDrained（bus 空 5 秒后触发），
            // 现已移除——agent 只需在真正忙过后再 reflection。

            // 随机化启动延迟(0.5~3 秒): 7 个 agent 不会完全同步启动,
            // 避免在接第一批消息时产生 embed 竞争的"惊群"效应。
            // 这个延迟发生在发 cold_start_done 之前,延迟 Preparing→Idle,
            // 让 system 有短暂窗口完成初始化,但不影响用户体验(远小于 5 秒)。
            let startup_delay_ms: u64 = rand::thread_rng().gen_range(500..=3000);
            sleep(Duration::from_millis(startup_delay_ms)).await;

            // 在循环开始前通知外部冷启动已完成，使 AgentStatus Preparing → Idle。
            // 与 QueueDrained/reflection 解耦：状态转换不应依赖 reflection 触发。
            publish_cold_start_done(&local_bus, &global_bus, &agent_id).await;

            loop {
                if stop_token.is_cancelled() {
                    break;
                }

                // Skip if busy_reflecting is set
                if coord.busy_reflecting.load(Ordering::Relaxed) {
                    sleep(Duration::from_millis(100)).await;
                    continue;
                }

                // Skip if the agent system state indicates it is doing real work
                // (chatting, working, studying, daily-life, or waiting for detach).
                // Otherwise the idle detector would fire boredom / sleep while a
                // detached process is still running.
                if let Some(ref ss) = system_state {
                    let state = *ss.lock().expect("system_state lock");
                    if state != AgentSystemState::Idle {
                        // Reset idle depth — the agent is busy doing something real.
                        detector.idle_depth = 0;
                        detector.last_poll = Some(Instant::now());
                        sleep(Duration::from_millis(100)).await;
                        continue;
                    }
                }

                // Check if depth reset is pending (queue was drained)
                if coord.pending_depth_reset.swap(false, Ordering::SeqCst) {
                    detector.idle_depth = 0;
                    // Depth reset invalidates any pending wake-up schedule.
                    *coord.wakeup_schedule.write().await = None;
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

                // ── Wake-up transition ────────────────────────────────────
                // If a wake-up schedule is active (past its delay period),
                // drive the progressive transition: each poll advances one
                // step, linearly interpolating depth → 0 and arousal → 1.0.
                if let Some(mut schedule) = coord.take_active_wakeup().await {
                    // Lazily capture depth / arousal on the first wake-up step.
                    if !schedule.is_initialized() {
                        schedule.initial_depth = Some(detector.idle_depth);
                        schedule.initial_arousal = Some(coord.arousal.current());
                    }

                    let init_depth = schedule.initial_depth.unwrap_or(0);
                    let init_arousal = schedule.initial_arousal.unwrap_or(1.0);

                    // Advance one step.
                    schedule.current_step += 1;

                    let progress = schedule.progress();
                    let new_depth = (init_depth as f64 * (1.0 - progress)) as u32;
                    let new_arousal = if schedule.is_done() {
                        schedule.target_arousal
                    } else {
                        init_arousal
                            + (schedule.target_arousal - init_arousal) * progress
                    };

                    // Apply interpolated arousal.
                    coord.arousal.reset(new_arousal);
                    detector.idle_depth = new_depth;
                    detector.last_poll = Some(Instant::now());

                    if schedule.is_done() {
                        // Transition complete — agent is awake.
                        detector.idle_depth = 0;
                        coord.arousal.reset(schedule.target_arousal);
                        info!(
                            agent_id = %agent_id,
                            target_arousal = schedule.target_arousal,
                            "WakeUp: transition complete, agent awake"
                        );
                    } else {
                        // Still transitioning — publish WakeUp event.
                        let wakeup_event = IdleEvent {
                            kind: IdleKind::WakeUp,
                            depth: new_depth,
                            duration_secs: delay_secs,
                            context: Some(IdleContext {
                                last_event_type: String::new(),
                                last_idle_outputs: detector.last_idle_outputs.clone(),
                                arousal_level: new_arousal,
                            }),
                            from_chat_mode: false,
                            agent_id: Some(agent_id.clone()),
                        };
                        let event: kernel::event::Event = wakeup_event.into();
                        try_publish(&*local_bus, event.clone()).await;
                        if let Some(ref global) = global_bus {
                            try_publish(&**global, event).await;
                        }

                        info!(
                            agent_id = %agent_id,
                            step = schedule.current_step,
                            total_steps = schedule.total_steps,
                            depth = new_depth,
                            arousal = new_arousal,
                            "WakeUp: transition step",
                        );

                        // Re-insert schedule with updated step.
                        *coord.wakeup_schedule.write().await = Some(schedule);
                    }

                    sleep(Duration::from_secs_f64(delay_secs)).await;
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
                        try_publish(&*local_bus, qd_event.clone()).await;
                        if let Some(ref global) = global_bus {
                            try_publish(&**global, qd_event).await;
                        }
                        // NOTE: Do NOT write AgentSystemState::Idle here.
                        // The system_state is managed by the agent harness
                        // (Chatting → Idle) and the boredom actor
                        // (Working/Studying/DailyLife). Unconditionally
                        // writing Idle here races with the harness because
                        // the publish().await points above yield to the
                        // Tokio runtime, allowing process_message to set
                        // Chatting before we overwrite it.
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

                // Bus is empty, no recent activity — progress idle state.
                // Override: if cognitive state is not Lucid, force Sleep.
                let kind = if coord.is_cognitive_force_sleep() {
                    IdleKind::Sleep
                } else if detector.idle_depth == 0 {
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
                try_publish(&*local_bus, event.clone()).await;
                // Also publish to the global bus so the Tauri UI event bridge
                // can observe per-agent idle state
                if let Some(ref global) = global_bus {
                    try_publish(&**global, event).await;
                }

                // Boredom action: check deferred tasks first (higher priority
                // than random skill selection), then fall through to the
                // weighted random tag pick.
                if kind == IdleKind::Boredom {
                    // ── Deferred task queue check ────────────────────────────
                    // If there are deferred tasks ready to execute, publish one
                    // as a MessageReceived event and skip random skill selection
                    // this cycle. The agent harness will process it through the
                    // ReAct loop.
                    let mut deferred_acted = false;
                    if let Some(ref queue) = deferred_queue {
                        // Dequeue pending tasks and filter by execute_after_ms.
                        // We use the base trait's dequeue() + manual filtering
                        // because the extension trait's blanket impl requires
                        // Sized (not satisfied for dyn DeferredTaskQueue).
                        match queue.dequeue(5).await {
                            Ok(tasks) => {
                                let now = current_time_ms();
                                let ready: Vec<_> = tasks
                                    .into_iter()
                                    .filter(|t| t.is_ready_at(now))
                                    .collect();
                                if let Some(task) = ready.first() {
                                    info!(
                                        agent_id = %agent_id,
                                        task_id = %task.id,
                                        title = %task.title,
                                        source = %task.source,
                                        "DeferredQueue: executing deferred task",
                                    );
                                    let event = kernel::event::Event::new(
                                        format!("idle:deferred:{agent_id}:{}", task.id),
                                        kernel::event::EventType::MessageReceived,
                                        serde_json::json!({
                                            "session_id": format!("{agent_id}:deferred:{}", task.id),
                                            "agent_id": agent_id,
                                            "text": format!(
                                                "[DEFERRED TASK] {}\n\n{}",
                                                task.title, task.description,
                                            ),
                                            "source": task.source,
                                            "deferred_task_id": task.id,
                                            "session_type": "background",
                                            "background": true,
                                        }),
                                    );
                                    try_publish(&*local_bus, event.clone()).await;
                                    if let Some(ref global) = global_bus {
                                        try_publish(&**global, event).await;
                                    }
                                    deferred_acted = true;
                                }
                            }
                            Err(e) => {
                                warn!(
                                    agent_id = %agent_id,
                                    error = %e,
                                    "DeferredQueue: dequeue failed",
                                );
                            }
                        }
                    }

                    // ── Random skill selection (only if no deferred task) ────
                    if !deferred_acted
                        && let Some(ref actor) = boredom_actor
                        && let Some(tag) =
                            actor.try_act(
                                detector.boredom_poll_count,
                                &agent_id,
                                pending,
                            ).await
                    {
                        // Notify the corresponding system state so the UI
                        // reflects what the agent is doing.
                        if let Some(ref ss) = system_state {
                            let state = match tag.as_str() {
                                "work" => AgentSystemState::Working,
                                "study" => AgentSystemState::Studying,
                                "prize" => AgentSystemState::Prize,
                                "internet" | "entertainment" | "fun" => {
                                    AgentSystemState::DailyLife
                                }
                                _ => AgentSystemState::Waiting,
                            };
                            *ss.lock().expect("system_state lock") = state;
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

/// 冷启动完成事件的 EventType 标识。
/// 订阅者（通常是 AgentRegistry）通过这个字符串过滤事件。
pub const COLD_START_DONE_EVENT: &str = "agent:cold_start_done";

/// 发布冷启动完成事件到 local_bus（和可选的 global_bus）。
/// 在 AgentIdleManager 启动时调用一次，驱动 AgentStatus Preparing → Idle。
/// 与 QueueDrained/reflection 解耦：状态转换不应依赖 reflection 触发。
async fn publish_cold_start_done(
    local_bus: &Arc<dyn EventBus>,
    global_bus: &Option<Arc<dyn EventBus>>,
    agent_id: &str,
) {
    let event = Event::new(
        "idle.manager",
        EventType::Custom(COLD_START_DONE_EVENT.to_owned()),
        serde_json::json!({ "agent_id": agent_id }),
    );
    try_publish(&**local_bus, event.clone()).await;
    if let Some(global) = global_bus {
        try_publish(&**global, event).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use event_bus::SubscriptionFilter;
    use kernel::event::Event;
    use std::sync::Arc;
    use tokio::sync::mpsc;

    /// Adapter: 把 `Fn(Event)` 包装出 EventHandler(走 channel)。
    struct ChanHandler<T: Fn(Event) + Send + Sync>(T);
    #[async_trait::async_trait]
    impl<T: Fn(Event) + Send + Sync> event_bus::EventHandler for ChanHandler<T> {
        async fn handle(&self, event: Event) -> AmanResult<()> {
            (self.0)(event);
            Ok(())
        }
    }

    /// 测试:冷启动后 AgentIdleManager 立即发布 `agent:cold_start_done`,
    /// 但在 agent 真正 busy→empty 之前,**不**产生 QueueDrained。
    /// 这是 Bug B 的回归测试:冷启动时不应触发 reflection(会争抢 embed_lock)。
    #[tokio::test]
    async fn cold_start_publishes_done_without_queue_drained() {
        let bus: Arc<dyn EventBus> = Arc::new(event_bus::InMemoryBus::new(
            Default::default(),
        ));

        // 订阅 QueueDrained 和 cold_start_done — 用 channel 收集
        let (qd_tx, mut qd_rx) = mpsc::unbounded_channel::<Event>();
        let (done_tx, mut done_rx) = mpsc::unbounded_channel::<Event>();

        let _id = bus
            .subscribe(
                SubscriptionFilter {
                    event_types: Some(vec![EventType::QueueDrained]),
                    sources: None,
                    priorities: None,
                    payload_match: None,
                },
                Box::new(ChanHandler(move |e: Event| {
                    let _ = qd_tx.send(e);
                })),
            )
            .await
            .expect("subscribe qd");
        let _id2 = bus
            .subscribe(
                SubscriptionFilter {
                    event_types: Some(vec![EventType::Custom(
                        COLD_START_DONE_EVENT.to_owned(),
                    )]),
                    sources: None,
                    priorities: None,
                    payload_match: None,
                },
                Box::new(ChanHandler(move |e: Event| {
                    let _ = done_tx.send(e);
                })),
            )
            .await
            .expect("subscribe done");
        let _ = (_id, _id2);

        // 启动 idle manager (bus 持续空 — 模拟冷启动)
        let manager = AgentIdleManager::new(
            "test-agent",
            Arc::clone(&bus),
            None,
            IdlePersonality::default(),
            1.0,
            60.0,
            None,
            None,
            None,
        );
        manager.start().await;

        // 等待足够让随机启动延迟(0.5~3 秒)+ cold_start_done 发出
        // 用 4 秒上限,覆盖随机延迟的最大值。
        tokio::time::sleep(Duration::from_millis(4000)).await;

        // cold_start_done 应该已发布
        let done_published = done_rx.try_recv().is_ok();
        assert!(
            done_published,
            "cold_start_done should be published on startup"
        );
        // QueueDrained **不应**在冷启动时产生(只在 busy→empty 后)
        let qd_received = qd_rx.try_recv().is_ok();
        assert!(
            !qd_received,
            "QueueDrained must NOT be produced during cold start (Bug B regression)"
        );

        manager.stop().await;
    }
}

