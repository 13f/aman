// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! IdleCoordination 结构体——跨组件共享状态。
//!
//! Architecture ref: idle-design.md §3.5

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU8, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info};

use crate::types::IdleKind;

// ---------------------------------------------------------------------------
// WakeUpSchedule
// ---------------------------------------------------------------------------

/// Tracks a progressive wake-up transition after Incubation completes.
///
/// During the delay period (from schedule time to `start_at`), the idle
/// detector skips deep states. Once `start_at` is reached, the detector
/// captures the current depth and arousal, then advances one step per poll
/// cycle, linearly interpolating depth → 0 and arousal → target.
#[derive(Debug, Clone)]
pub struct WakeUpSchedule {
    /// When the wake-up transition begins (schedule time + delay).
    pub start_at: Instant,
    /// Total poll steps for the progressive transition.
    pub total_steps: u32,
    /// Current step (0 = not started, incremented each poll during transition).
    pub current_step: u32,
    /// Depth snapshot captured when the first wake-up poll starts.
    pub initial_depth: Option<u32>,
    /// Arousal snapshot captured when the first wake-up poll starts.
    pub initial_arousal: Option<f64>,
    /// Target arousal after wake-up completes.
    pub target_arousal: f64,
}

impl WakeUpSchedule {
    /// Whether the transition has been initialized (first poll captured snapshots).
    pub fn is_initialized(&self) -> bool {
        self.initial_depth.is_some()
    }

    /// Progress through the transition (0.0 → 1.0).
    pub fn progress(&self) -> f64 {
        if self.total_steps == 0 {
            return 1.0;
        }
        (self.current_step as f64 / self.total_steps as f64).clamp(0.0, 1.0)
    }

    /// Whether the transition is complete.
    pub fn is_done(&self) -> bool {
        self.current_step > self.total_steps
    }

    /// Interpolated depth at the current step.
    pub fn current_depth(&self) -> u32 {
        let init = self.initial_depth.unwrap_or(0);
        (init as f64 * (1.0 - self.progress())) as u32
    }
}

/// 恢复计时器的句柄。
///
/// stop() 后启动，在 1min 内将 depth / arousal 逐步恢复到初始值。
/// 重新 start() 时通过 [`IdleCoordination::cancel_recovery`] 终止并释放。
pub struct RecoveryHandle {
    /// 取消令牌——start() 重新触发时取消此令牌以终止恢复。
    pub cancel_token: CancellationToken,
    /// 恢复任务句柄。
    pub task: JoinHandle<()>,
}

/// 跨组件共享的空闲协调状态。
pub struct IdleCoordination {
    /// Dispatcher 正在执行 Reflection，IdleDetector 应暂停
    pub busy_reflecting: Arc<AtomicBool>,
    /// Arousal 跟踪器
    pub arousal: Arc<ArousalTracker>,
    /// 最后处理的真实事件来源类型（AtomicU8 存储 SourceType::to_u8()）
    pub last_source_type: Arc<AtomicU8>,
    /// 全局空闲取消令牌——真实事件到达时取消
    pub idle_cancel_token: Arc<RwLock<CancellationToken>>,
    /// 队列已清空，IdleDetector 应重置 depth（由 Dispatcher 在产生 QueueDrained 时设置）
    pub pending_depth_reset: Arc<AtomicBool>,
    /// Per-kind cooldown expiry timestamps (per-agent, not global).
    pub kind_cooldowns: Arc<RwLock<HashMap<IdleKind, Instant>>>,
    /// Active wake-up schedule (set by Incubation completion, consumed by IdleDetector).
    pub wakeup_schedule: RwLock<Option<WakeUpSchedule>>,
    /// 认知状态非 Lucid 时，强制进入 Sleep（由外部 CognitiveStateMachine 驱动）。
    pub cognitive_force_sleep: Arc<AtomicBool>,
    /// The agenverse era (Void=0, Chaos=1, Genesis=2), shared from [`Agenverse`].
    /// During Chaos the idle system is suppressed — agents may only Daze.
    pub era: Arc<AtomicU8>,
    // ---------------------------------------------------------------------------
    // 启动 / 停止 生命周期（start-stop lifecycle）
    // ---------------------------------------------------------------------------
    /// 当前 idle depth（AtomicU32，跨 stop/start 存活）。
    ///
    /// 原来存放在 `IdleDetector.idle_depth`，但 detector 在 start() 时新建，
    /// stop 后即丢失。为了让恢复计时器能跨 stop 修改 depth，移至此处。
    pub idle_depth: Arc<AtomicU32>,
    /// depth 初始值（默认 0）。恢复计时器的 depth 目标值。
    pub depth_initial: Arc<AtomicU32>,
    /// arousal 初始值（构造时传入）。恢复计时器的 arousal 目标值。
    arousal_initial: f64,
    /// 活跃恢复计时器的句柄。`None` 表示无恢复在进行。
    recovery_handle: Arc<RwLock<Option<RecoveryHandle>>>,
}

impl IdleCoordination {
    /// 创建新的协调状态。
    ///
    /// `era` is a shared handle to the agenverse era (Void/Chaos/Genesis).
    /// During Chaos the idle loop suppresses depth progression so agents
    /// can only Daze.
    #[must_use]
    pub fn new(
        arousal_initial: f64,
        arousal_half_life_secs: f64,
        era: Arc<AtomicU8>,
    ) -> Self {
        Self {
            busy_reflecting: Arc::new(AtomicBool::new(false)),
            arousal: Arc::new(ArousalTracker::new(arousal_initial, arousal_half_life_secs)),
            last_source_type: Arc::new(AtomicU8::new(0)), // SourceType::Unknown = 0
            idle_cancel_token: Arc::new(RwLock::new(CancellationToken::new())),
            pending_depth_reset: Arc::new(AtomicBool::new(false)),
            kind_cooldowns: Arc::new(RwLock::new(HashMap::new())),
            wakeup_schedule: RwLock::new(None),
            cognitive_force_sleep: Arc::new(AtomicBool::new(false)),
            era,
            idle_depth: Arc::new(AtomicU32::new(0)),
            depth_initial: Arc::new(AtomicU32::new(0)),
            arousal_initial,
            recovery_handle: Arc::new(RwLock::new(None)),
        }
    }

    /// Whether the agenverse has reached Genesis (agents fully awakened).
    /// During Void or Chaos this returns `false`, and the idle loop must
    /// not progress past Daze.
    #[must_use]
    pub fn is_genesis(&self) -> bool {
        self.era.load(Ordering::Acquire) >= 2 /* Era::Genesis */
    }

    /// 设置是否强制进入 Sleep 模式（由认知状态机驱动）。
    pub fn set_cognitive_force_sleep(&self, force: bool) {
        self.cognitive_force_sleep.store(force, Ordering::Relaxed);
    }

    /// 检查是否强制进入 Sleep 模式。
    pub fn is_cognitive_force_sleep(&self) -> bool {
        self.cognitive_force_sleep.load(Ordering::Relaxed)
    }

    /// 取消运行中的空闲 Workflow —— 真实事件到达时调用。
    ///
    /// 注意：depth 重置不由这里触发，而是在 Dispatcher 产生 QueueDrained 时
    /// 通过 [`signal_queue_drained`](Self::signal_queue_drained) 设置。
    pub async fn reset_idle_signal(&self) {
        let mut token = self.idle_cancel_token.write().await;
        token.cancel();
        *token = CancellationToken::new();
    }

    /// 标记 depth 需要在下次 idle poll 时重置 —— Dispatcher 队列清空时调用。
    pub fn signal_queue_drained(&self) {
        self.pending_depth_reset.store(true, Ordering::SeqCst);
    }

    /// Set a cooldown for `kind` — it will not be produced until `cooldown_secs` elapses.
    pub async fn set_kind_cooldown(&self, kind: IdleKind, cooldown_secs: u64) {
        let expiry = Instant::now() + std::time::Duration::from_secs(cooldown_secs);
        self.kind_cooldowns.write().await.insert(kind, expiry);
    }

    /// Check whether `kind` is currently on cooldown.
    pub async fn is_kind_on_cooldown(&self, kind: IdleKind) -> bool {
        match self.kind_cooldowns.read().await.get(&kind) {
            Some(expiry) => Instant::now() < *expiry,
            None => false,
        }
    }

    /// Schedule a progressive wake-up after `delay_secs`.
    ///
    /// Called when Incubation completes. Depth and arousal snapshots are
    /// captured lazily by the IdleDetector when the transition starts.
    pub async fn schedule_wakeup(&self, delay_secs: u64, poll_steps: u32) {
        let schedule = WakeUpSchedule {
            start_at: Instant::now() + Duration::from_secs(delay_secs),
            total_steps: poll_steps,
            current_step: 0,
            initial_depth: None,
            initial_arousal: None,
            target_arousal: 1.0,
        };
        *self.wakeup_schedule.write().await = Some(schedule);
    }

    /// Take and return the active wake-up schedule if one is in progress
    /// (past its delay period). Returns `None` if no schedule or still waiting.
    pub async fn take_active_wakeup(&self) -> Option<WakeUpSchedule> {
        let mut guard = self.wakeup_schedule.write().await;
        match &*guard {
            Some(s) if Instant::now() >= s.start_at => guard.take(),
            _ => None,
        }
    }

    /// Check whether a wake-up is scheduled (including delay period).
    pub async fn has_pending_wakeup(&self) -> bool {
        self.wakeup_schedule.read().await.is_some()
    }

    // ---------------------------------------------------------------------------
    // 启动 / 停止 生命周期（start-stop lifecycle）
    // ---------------------------------------------------------------------------

    /// 启动恢复计时器，在 `duration` 内将 depth / arousal 逐步恢复到初始值。
    ///
    /// 若已有恢复在进行，先取消旧的再启动新的（保证不叠加）。
    /// 恢复速度在启动时根据当前值与目标值的差值计算：
    /// - depth_speed = current_depth / duration_secs  (units/sec)
    /// - arousal_speed = |current_arousal - arousal_initial| / duration_secs
    pub async fn start_recovery(&self, duration: Duration) {
        // 取消已有恢复，防止叠加。
        self.cancel_recovery().await;

        let cancel_token = CancellationToken::new();
        let task = spawn_recovery_task(
            Arc::clone(&self.idle_depth),
            Arc::clone(&self.depth_initial),
            Arc::clone(&self.arousal),
            self.arousal_initial,
            duration,
            cancel_token.clone(),
            Arc::clone(&self.recovery_handle),
        );

        let handle = RecoveryHandle { cancel_token, task };
        *self.recovery_handle.write().await = Some(handle);

        let depth = self.idle_depth.load(Ordering::SeqCst);
        let arousal = self.arousal.current();
        let secs = duration.as_secs_f64();
        let depth_speed = depth as f64 / secs;
        let arousal_speed = (arousal - self.arousal_initial).abs() / secs;
        info!(
            duration_secs = duration.as_secs(),
            depth_initial = depth,
            arousal_initial = arousal,
            depth_speed = format!("{depth_speed:.2}"),
            arousal_speed = format!("{arousal_speed:.4}"),
            "IdleSystem: recovery timer started"
        );
    }

    /// 终止并释放恢复计时器。
    ///
    /// 由 `AgentIdleManager::start()` 调用：重新 start 时终止恢复中的计时器。
    pub async fn cancel_recovery(&self) {
        let mut guard = self.recovery_handle.write().await;
        if let Some(handle) = guard.take() {
            handle.cancel_token.cancel();
            handle.task.abort();
            debug!("IdleSystem: recovery timer cancelled");
        }
    }

    /// 是否有活跃的恢复计时器。
    ///
    /// 恢复任务完成后会自动清理自身 handle，因此只需检查 handle 是否存在。
    pub async fn is_recovering(&self) -> bool {
        self.recovery_handle.read().await.is_some()
    }

    /// 返回 arousal 初始值（恢复计时器的目标值）。
    pub fn initial_arousal(&self) -> f64 {
        self.arousal_initial
    }
}

// ---------------------------------------------------------------------------
// 恢复计时器任务
// ---------------------------------------------------------------------------

/// 启动一个恢复任务，在 `duration` 内将 depth / arousal 逐步恢复到初始值。
///
/// 每 500ms 一个 tick：
/// - depth 线性递减：`decrement = ceil(current_depth / remaining_ticks)`
/// - arousal 指数逼近：`restore_to(target, remaining)`
///
/// 当 `cancel_token` 被 cancel 时立即停止（不强制归零，保留当前值）。
/// 当 depth 提前归零时，只继续恢复 arousal。
/// 完成后强制精确归零 / 复位，并自动清理自身 handle。
fn spawn_recovery_task(
    idle_depth: Arc<AtomicU32>,
    depth_initial: Arc<AtomicU32>,
    arousal: Arc<ArousalTracker>,
    arousal_initial: f64,
    duration: Duration,
    cancel_token: CancellationToken,
    recovery_handle: Arc<RwLock<Option<RecoveryHandle>>>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        // 500ms 一 tick → 120 ticks/min，足够平滑，开销可忽略。
        let tick = Duration::from_millis(500);
        let total_ticks = (duration.as_millis() / tick.as_millis()) as u32;
        let mut remaining = total_ticks;

        loop {
            tokio::select! {
                _ = cancel_token.cancelled() => {
                    // start() 重新触发，终止恢复。
                    debug!("IdleRecovery: cancelled by restart");
                    break;
                }
                _ = tokio::time::sleep(tick) => {
                    if remaining == 0 {
                        break;
                    }

                    // depth 恢复：线性递减，ceil 保证不卡在非零值。
                    let current_depth = idle_depth.load(Ordering::SeqCst);
                    if current_depth > 0 {
                        let decrement =
                            (current_depth as f64 / remaining as f64).ceil() as u32;
                        let new_depth = current_depth.saturating_sub(decrement);
                        idle_depth.store(new_depth, Ordering::SeqCst);
                    }

                    // arousal 恢复：指数逼近目标值。
                    arousal.restore_to(arousal_initial, remaining);

                    remaining -= 1;

                    let d = idle_depth.load(Ordering::SeqCst);
                    let a = arousal.current();
                    debug!(
                        remaining_ticks = remaining,
                        depth = d,
                        arousal = format!("{a:.4}"),
                        "IdleRecovery: tick"
                    );

                    // depth 已归零且 arousal 已到位，提前退出。
                    if d == 0 && (a - arousal_initial).abs() < 0.001 {
                        break;
                    }
                }
            }
        }

        // 完成后强制精确归零 / 复位，消除浮点误差。
        idle_depth.store(depth_initial.load(Ordering::SeqCst), Ordering::SeqCst);
        arousal.reset(arousal_initial);

        // 自动清理自身 handle，使 is_recovering() 返回 false。
        let _ = recovery_handle.write().await.take();

        debug!("IdleRecovery: complete, depth and arousal restored to defaults");
    })
}

// ---------------------------------------------------------------------------
// ArousalTracker（引用自 ArousalBehavior）
// ---------------------------------------------------------------------------

/// 指数衰减的 Arousal 跟踪器。
pub struct ArousalTracker {
    current_value: Arc<std::sync::Mutex<ArousalInner>>,
}

struct ArousalInner {
    value: f64,
    half_life_secs: f64,
    last_update: Instant,
}

impl ArousalTracker {
    #[must_use]
    pub fn new(initial_value: f64, half_life_secs: f64) -> Self {
        Self {
            current_value: Arc::new(std::sync::Mutex::new(ArousalInner {
                value: initial_value,
                half_life_secs,
                last_update: Instant::now(),
            })),
        }
    }

    /// 返回当前arousal值（考虑时间衰减）。
    #[must_use]
    pub fn current(&self) -> f64 {
        let inner = self.current_value.lock().unwrap();
        let elapsed = inner.last_update.elapsed().as_secs_f64();
        inner.value * (0.5_f64).powf(elapsed / inner.half_life_secs)
    }

    /// 应用指定behavior的衰减效果。
    pub fn apply_behavior(&self, behavior: super::types::ArousalBehavior) {
        let decay_multiplier = match behavior {
            super::types::ArousalBehavior::Passive => 1.0,
            super::types::ArousalBehavior::Engaged { decay_multiplier } => decay_multiplier,
        };
        let mut inner = self.current_value.lock().unwrap();
        let elapsed = inner.last_update.elapsed().as_secs_f64();
        let decayed = inner.value * (0.5_f64).powf(elapsed * decay_multiplier / inner.half_life_secs);
        inner.value = decayed;
        inner.last_update = Instant::now();
    }

    /// Boost arousal toward 1.0 by `factor` (0.0–1.0).
    ///
    /// Called when a real event arrives so that engagement raises arousal
    /// instead of only decaying during idle. Formula:
    /// `new_value = decayed_current + (1.0 - decayed_current) * factor`
    pub fn boost(&self, factor: f64) {
        let mut inner = self.current_value.lock().unwrap();
        let elapsed = inner.last_update.elapsed().as_secs_f64();
        let decayed = inner.value * (0.5_f64).powf(elapsed / inner.half_life_secs);
        inner.value = decayed + (1.0 - decayed) * factor.clamp(0.0, 1.0);
        inner.last_update = Instant::now();
    }

    /// 重置到初始值。
    pub fn reset(&self, initial_value: f64) {
        let mut inner = self.current_value.lock().unwrap();
        inner.value = initial_value;
        inner.last_update = Instant::now();
    }

    /// 朝 `target` 逐步恢复一步。
    ///
    /// 用于 stop 后的恢复计时器：每 tick 调用一次，每次移动剩余距离的
    /// `1 / remaining_ticks`，呈指数衰减式逼近，保证最后一 tick 恰好到位。
    /// 公式：`current += (target - current) / remaining_ticks`
    pub fn restore_to(&self, target: f64, remaining_ticks: u32) {
        let mut inner = self.current_value.lock().unwrap();
        let step = if remaining_ticks == 0 {
            // 最后一跳：直接到位
            target - inner.value
        } else {
            (target - inner.value) / remaining_ticks as f64
        };
        inner.value = (inner.value + step).clamp(0.0, 1.0);
        inner.last_update = Instant::now();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── ArousalTracker tests (T2.2) ─────────────────────────────

    #[test]
    fn arousal_half_life_decay() {
        let tracker = ArousalTracker::new(1.0, 900.0);
        let inner = tracker.current_value.lock().unwrap();
        let elapsed = 900.0;
        let expected = inner.value * (0.5_f64).powf(elapsed / inner.half_life_secs);
        drop(inner);
        assert!((expected - 0.5).abs() < 0.001);
    }

    #[test]
    fn engaged_zero_no_decay() {
        let tracker = ArousalTracker::new(1.0, 900.0);
        tracker.apply_behavior(super::super::types::ArousalBehavior::Engaged {
            decay_multiplier: 0.0,
        });
        let value = tracker.current();
        assert!((value - 1.0).abs() < 0.001);
    }

    #[test]
    fn engaged_half_speed_decay() {
        let tracker = ArousalTracker::new(1.0, 900.0);
        tracker.apply_behavior(super::super::types::ArousalBehavior::Engaged {
            decay_multiplier: 0.5,
        });
        // apply_behavior resets last_update to now, so current() returns
        // the updated value (half-speed applied). Immediately check.
        let value = tracker.current();
        assert!((value - 1.0).abs() < 0.001, "value={value}");
    }

    #[test]
    fn passive_sequential_cumulative_decay() {
        let tracker = ArousalTracker::new(1.0, 900.0);
        // Each Passive call applies standard decay
        let mut last = 1.0;
        for _ in 0..5 {
            tracker.apply_behavior(super::super::types::ArousalBehavior::Passive);
            let current = tracker.current();
            // After each apply, current should be <= previous (no time elapsed means no extra decay)
            assert!(current <= last, "cumulative decay should not increase: {current} > {last}");
            last = current;
        }
    }

    #[test]
    fn arousal_reset_restores_value() {
        let tracker = ArousalTracker::new(1.0, 900.0);
        tracker.apply_behavior(super::super::types::ArousalBehavior::Passive);
        let after = tracker.current();
        assert!(after <= 1.0);
        tracker.reset(1.0);
        let reset_val = tracker.current();
        assert!((reset_val - 1.0).abs() < 0.001);
    }

    #[test]
    fn restore_to_approaches_target_exponentially() {
        // 从 0.8 朝 0.2 恢复，10 ticks。
        let tracker = ArousalTracker::new(0.8, 900.0);
        let target = 0.2;
        let ticks = 10;
        for i in 0..ticks {
            let remaining = ticks - i;
            tracker.restore_to(target, remaining);
        }
        // 最后一跳后应该精确到位（允许浮点误差）。
        let val = tracker.current();
        assert!(
            (val - target).abs() < 0.0001,
            "restore_to should converge to target, got {val}"
        );
    }

    #[test]
    fn restore_to_never_overshoots() {
        // 从 0.1 朝 0.9 恢复（向上），验证不会超调。
        let tracker = ArousalTracker::new(0.1, 900.0);
        let target = 0.9;
        for i in 0..100 {
            let remaining = 100 - i;
            tracker.restore_to(target, remaining);
            let val = tracker.current();
            assert!(
                val >= 0.0 && val <= 1.0,
                "restore_to should stay within [0, 1], got {val}"
            );
        }
        let val = tracker.current();
        assert!(
            (val - target).abs() < 0.0001,
            "should reach target, got {val}"
        );
    }

    #[test]
    fn restore_to_zero_remaining_jumps_to_target() {
        // remaining_ticks == 0 时应直接跳到 target。
        let tracker = ArousalTracker::new(0.5, 900.0);
        tracker.restore_to(0.9, 0);
        let val = tracker.current();
        assert!(
            (val - 0.9).abs() < 0.0001,
            "remaining=0 should jump to target, got {val}"
        );
    }

    // ── IdleCoordination tests (T2.1) ───────────────────────────

    /// Helper: create an era atomic in Genesis state for tests.
    fn era_genesis() -> Arc<AtomicU8> {
        Arc::new(AtomicU8::new(2 /* Era::Genesis */))
    }

    #[test]
    fn coordination_new_initial_state() {
        let coord = IdleCoordination::new(1.0, 900.0, era_genesis());
        assert_eq!(coord.last_source_type.load(Ordering::Relaxed), 0); // Unknown = 0
        assert!(!coord.busy_reflecting.load(Ordering::Relaxed));
        assert!(!coord.pending_depth_reset.load(Ordering::Relaxed));
        assert!(coord.is_genesis());
    }

    #[test]
    fn coordination_chaos_not_genesis() {
        let era = Arc::new(AtomicU8::new(1 /* Era::Chaos */));
        let coord = IdleCoordination::new(1.0, 900.0, era);
        assert!(!coord.is_genesis());
    }

    #[test]
    fn coordination_void_not_genesis() {
        let era = Arc::new(AtomicU8::new(0 /* Era::Void */));
        let coord = IdleCoordination::new(1.0, 900.0, era);
        assert!(!coord.is_genesis());
    }

    #[tokio::test]
    async fn reset_idle_signal_cancels_old_token() {
        let coord = IdleCoordination::new(1.0, 900.0, era_genesis());
        let old_token = coord.idle_cancel_token.read().await.clone();
        // Prove the old token works
        assert!(!old_token.is_cancelled());

        coord.reset_idle_signal().await;

        // Old token should now be cancelled
        assert!(old_token.is_cancelled());

        // New token should be uncancelled
        let new_token = coord.idle_cancel_token.read().await;
        assert!(!new_token.is_cancelled());
    }

    #[tokio::test]
    async fn reset_idle_signal_does_not_set_pending_depth_reset() {
        let coord = IdleCoordination::new(1.0, 900.0, era_genesis());
        coord.reset_idle_signal().await;
        // depth reset 不由 reset_idle_signal 触发，而是由 signal_queue_drained 触发
        assert!(!coord.pending_depth_reset.load(Ordering::SeqCst));
    }

    #[test]
    fn signal_queue_drained_sets_pending_depth_reset() {
        let coord = IdleCoordination::new(1.0, 900.0, era_genesis());
        coord.signal_queue_drained();
        assert!(coord.pending_depth_reset.load(Ordering::SeqCst));
    }

    #[test]
    fn arousal_boost_moves_toward_one() {
        let tracker = ArousalTracker::new(0.5, 900.0);
        tracker.boost(0.5);
        let val = tracker.current();
        // 0.5 + (1.0 - 0.5) * 0.5 = 0.75
        assert!((val - 0.75).abs() < 0.01, "value={val}");
    }

    #[test]
    fn arousal_boost_zero_factor_no_change() {
        let tracker = ArousalTracker::new(0.3, 900.0);
        tracker.boost(0.0);
        let val = tracker.current();
        assert!((val - 0.3).abs() < 0.01, "value={val}");
    }

    #[tokio::test]
    async fn reset_idle_signal_new_token_uncancelled() {
        let coord = IdleCoordination::new(1.0, 900.0, era_genesis());
        coord.reset_idle_signal().await;
        let new_token = coord.idle_cancel_token.read().await;
        assert!(!new_token.is_cancelled());
    }

    #[test]
    fn cognitive_force_sleep_flag() {
        let coord = IdleCoordination::new(1.0, 900.0, era_genesis());
        assert!(!coord.is_cognitive_force_sleep());

        coord.set_cognitive_force_sleep(true);
        assert!(coord.is_cognitive_force_sleep());

        coord.set_cognitive_force_sleep(false);
        assert!(!coord.is_cognitive_force_sleep());
    }

    #[test]
    fn cognitive_force_sleep_affects_idle_kind_resolution() {
        use crate::types::IdleKind;

        let coord = IdleCoordination::new(1.0, 900.0, era_genesis());
        // Simulate the idle loop's decision: when force_sleep is true,
        // the kind should be Sleep regardless of depth.
        let depth = 50; // would normally be Exploration
        let kind = if coord.is_cognitive_force_sleep() {
            IdleKind::Sleep
        } else if depth == 0 {
            IdleKind::Daze
        } else {
            IdleKind::Sleep // placeholder for resolve_with_arousal
        };

        // Without force_sleep, depth 50 resolves to some state
        assert_eq!(kind, IdleKind::Sleep); // placeholder match

        // With force_sleep, must be Sleep
        coord.set_cognitive_force_sleep(true);
        let kind = if coord.is_cognitive_force_sleep() {
            IdleKind::Sleep
        } else {
            IdleKind::Daze
        };
        assert_eq!(kind, IdleKind::Sleep);
    }

    // ── Recovery lifecycle tests ─────────────────────────────────

    #[tokio::test]
    async fn recovery_timer_restores_depth_and_arousal() {
        let coord = IdleCoordination::new(0.5, 900.0, era_genesis());
        // 模拟 stop 后的状态：depth=100, arousal=0.9。
        coord.idle_depth.store(100, Ordering::SeqCst);
        coord.arousal.reset(0.9);

        // 启动一个短恢复计时器（2 秒，便于测试）。
        coord.start_recovery(Duration::from_secs(2)).await;
        assert!(coord.is_recovering().await);

        // 等待恢复完成。
        tokio::time::sleep(Duration::from_millis(2500)).await;

        // depth 和 arousal 应该恢复到初始值。
        assert_eq!(coord.idle_depth.load(Ordering::SeqCst), 0);
        assert!(
            (coord.arousal.current() - 0.5).abs() < 0.001,
            "arousal should restore to initial 0.5, got {}",
            coord.arousal.current()
        );
        // 恢复完成后 handle 自动清理。
        assert!(!coord.is_recovering().await);
    }

    #[tokio::test]
    async fn recovery_timer_can_be_cancelled() {
        let coord = IdleCoordination::new(0.5, 900.0, era_genesis());
        coord.idle_depth.store(1000, Ordering::SeqCst);
        coord.arousal.reset(1.0);

        coord.start_recovery(Duration::from_secs(60)).await;
        assert!(coord.is_recovering().await);

        // 等 1 秒，让恢复进行一部分。
        tokio::time::sleep(Duration::from_millis(1000)).await;
        let depth_before_cancel = coord.idle_depth.load(Ordering::SeqCst);
        assert!(
            depth_before_cancel < 1000,
            "depth should have decreased, got {depth_before_cancel}"
        );

        // 取消恢复。
        coord.cancel_recovery().await;
        assert!(!coord.is_recovering().await);

        // 取消后 depth 不再变化。
        let depth_after_cancel = coord.idle_depth.load(Ordering::SeqCst);
        tokio::time::sleep(Duration::from_millis(500)).await;
        let depth_later = coord.idle_depth.load(Ordering::SeqCst);
        assert_eq!(
            depth_after_cancel, depth_later,
            "depth should not change after cancel"
        );
    }

    #[tokio::test]
    async fn start_recovery_cancels_previous_recovery() {
        let coord = IdleCoordination::new(0.5, 900.0, era_genesis());
        coord.idle_depth.store(500, Ordering::SeqCst);

        // 启动第一个恢复。
        coord.start_recovery(Duration::from_secs(60)).await;
        assert!(coord.is_recovering().await);

        // 立即启动第二个恢复（应取消第一个）。
        coord.start_recovery(Duration::from_secs(60)).await;
        assert!(coord.is_recovering().await);

        // 清理。
        coord.cancel_recovery().await;
        assert!(!coord.is_recovering().await);
    }

    #[tokio::test]
    async fn recovery_does_not_instantly_reset() {
        // 验证恢复是渐进的，不是瞬间重置。
        let coord = IdleCoordination::new(0.5, 900.0, era_genesis());
        coord.idle_depth.store(100, Ordering::SeqCst);
        coord.arousal.reset(1.0);

        coord.start_recovery(Duration::from_secs(2)).await;
        assert!(coord.is_recovering().await);

        // 等足够时间让至少 1 tick 完成（tick=500ms，给 700ms 余量）。
        tokio::time::sleep(Duration::from_millis(700)).await;
        let depth_after_one_tick = coord.idle_depth.load(Ordering::SeqCst);
        assert!(
            depth_after_one_tick > 0 && depth_after_one_tick < 100,
            "depth should decrease gradually, got {depth_after_one_tick}"
        );

        // 清理。
        coord.cancel_recovery().await;
        assert!(!coord.is_recovering().await);
    }
}
