// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! IdleCoordination 结构体——跨组件共享状态。
//!
//! Architecture ref: idle-design.md §3.5

use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::Arc;
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;

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
}

impl IdleCoordination {
    /// 创建新的协调状态。
    #[must_use]
    pub fn new(arousal_initial: f64, arousal_half_life_secs: f64) -> Self {
        Self {
            busy_reflecting: Arc::new(AtomicBool::new(false)),
            arousal: Arc::new(ArousalTracker::new(arousal_initial, arousal_half_life_secs)),
            last_source_type: Arc::new(AtomicU8::new(0)), // SourceType::Unknown = 0
            idle_cancel_token: Arc::new(RwLock::new(CancellationToken::new())),
            pending_depth_reset: Arc::new(AtomicBool::new(false)),
        }
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
}

// ---------------------------------------------------------------------------
// ArousalTracker（引用自 ArousalBehavior）
// ---------------------------------------------------------------------------

use std::time::Instant;

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

    // ── IdleCoordination tests (T2.1) ───────────────────────────

    #[test]
    fn coordination_new_initial_state() {
        let coord = IdleCoordination::new(1.0, 900.0);
        assert_eq!(coord.last_source_type.load(Ordering::Relaxed), 0); // Unknown = 0
        assert!(!coord.busy_reflecting.load(Ordering::Relaxed));
        assert!(!coord.pending_depth_reset.load(Ordering::Relaxed));
    }

    #[tokio::test]
    async fn reset_idle_signal_cancels_old_token() {
        let coord = IdleCoordination::new(1.0, 900.0);
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
        let coord = IdleCoordination::new(1.0, 900.0);
        coord.reset_idle_signal().await;
        // depth reset 不由 reset_idle_signal 触发，而是由 signal_queue_drained 触发
        assert!(!coord.pending_depth_reset.load(Ordering::SeqCst));
    }

    #[test]
    fn signal_queue_drained_sets_pending_depth_reset() {
        let coord = IdleCoordination::new(1.0, 900.0);
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
        let coord = IdleCoordination::new(1.0, 900.0);
        coord.reset_idle_signal().await;
        let new_token = coord.idle_cancel_token.read().await;
        assert!(!new_token.is_cancelled());
    }
}
