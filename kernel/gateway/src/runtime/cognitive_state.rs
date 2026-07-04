// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! Agent 认知能力状态机——"大脑还能不能转"。
//!
//! 本模块基于 [`super::backend_health::BackendStatus`]（基础设施层的客观诊断），
//! 映射出 Agent 的主观体验状态 [`CognitiveState`]。
//!
//! # 两层的关系
//!
//! - `BackendStatus` 是医生的诊断（客观）
//! - `CognitiveState` 是患者的主观体验
//! - `BackendStatus::Down` 持续一段时间才会把 `CognitiveState` 从 Catatonic 推到 Coma
//!   ——给 Agent "缓刑期"，避免短暂抖动就深度昏迷

#![forbid(unsafe_code)]

use std::sync::atomic::{AtomicI64, AtomicU8, Ordering};
use tokio::sync::watch;

use super::backend_health::BackendStatus;

/// Agent 的认知能力状态——"大脑还能不能转"。
///
/// 与 `AgentSystemState`（谁在干活）正交：
/// - `AgentSystemState::Working` + `CognitiveState::Lucid` = 正常工作中
/// - `AgentSystemState::Idle` + `CognitiveState::Lucid` = 清醒待命
/// - `AgentSystemState::Idle` + `CognitiveState::Catatonic` = 木僵（大脑离线，身体在呼吸）
/// - `AgentSystemState::Idle` + `CognitiveState::Coma` = 昏迷（完全无感知）
///
/// # 拟人化类比
///
/// | 状态 | 类比 |
/// |------|------|
/// | `Lucid` | 清醒、健康 |
/// | `Groggy` | 发烧 39°C——能听到但思维模糊 |
/// | `Catatonic` | 闭锁综合征——清醒但无法行动 |
/// | `Coma` | 深度麻醉——无感知无反应 |
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize)]
#[repr(u8)]
pub enum CognitiveState {
    /// 清醒——LLM 后端正常，Agent 可以思考。
    Lucid = 0,
    /// 迷糊——LLM 后端 Degraded，偶尔能响应但延迟高、错误多。
    Groggy = 1,
    /// 木僵——LLM 后端 Down，能感知事件流但无法调用 CognitiveEngine。
    Catatonic = 2,
    /// 昏迷——LLM 后端长时间不可用，连"感知"都关闭。
    Coma = 3,
}

impl CognitiveState {
    /// 从 u8 转换为 CognitiveState。
    pub fn from_u8(v: u8) -> Self {
        match v {
            0 => Self::Lucid,
            1 => Self::Groggy,
            2 => Self::Catatonic,
            3 => Self::Coma,
            _ => Self::Lucid,
        }
    }
}

impl From<BackendStatus> for CognitiveState {
    fn from(s: BackendStatus) -> Self {
        match s {
            BackendStatus::Unknown => Self::Groggy,
            BackendStatus::Ok => Self::Lucid,
            BackendStatus::Degraded => Self::Groggy,
            BackendStatus::Down => Self::Catatonic,
        }
    }
}

/// 认知状态机配置。
#[derive(Debug, Clone)]
pub struct CognitiveStateConfig {
    /// Catatonic 持续多少秒后进入 Coma。默认 900（15 分钟）。
    pub coma_threshold_secs: u64,
    /// Resurrection 后的 self_check 事件数量。默认 5。
    pub resurrection_self_check_depth: usize,
}

impl Default for CognitiveStateConfig {
    fn default() -> Self {
        Self {
            coma_threshold_secs: 900,
            resurrection_self_check_depth: 5,
        }
    }
}

/// Agent 的认知能力状态机。
///
/// 基于 BackendHealth 的翻转事件驱动，加入时间累积产生 Coma 状态。
/// 通过 `watch::channel` 通知所有订阅者（idle、emotion、arousal）。
pub struct CognitiveStateMachine {
    state: AtomicU8,
    /// 进入 Catatonic 的时间戳（Unix epoch 毫秒）。0 = 不在 Catatonic。
    catatonic_since: AtomicI64,
    tx: watch::Sender<CognitiveState>,
    config: CognitiveStateConfig,
}

impl CognitiveStateMachine {
    /// 创建一个新的 CognitiveStateMachine，初始状态为 Lucid。
    pub fn new(config: CognitiveStateConfig) -> (Self, watch::Receiver<CognitiveState>) {
        let (tx, rx) = watch::channel(CognitiveState::Lucid);
        (
            Self {
                state: AtomicU8::new(CognitiveState::Lucid as u8),
                catatonic_since: AtomicI64::new(0),
                tx,
                config,
            },
            rx,
        )
    }

    /// 获取当前认知状态。
    pub fn state(&self) -> CognitiveState {
        CognitiveState::from_u8(self.state.load(Ordering::Relaxed))
    }

    /// 获取 watch receiver 的克隆，供订阅者使用。
    pub fn subscribe(&self) -> watch::Receiver<CognitiveState> {
        self.tx.subscribe()
    }

    /// 由 BackendHealth 的翻转事件驱动。
    pub fn on_backend_status_change(&self, backend_status: BackendStatus) -> Option<CognitiveState> {
        let new_state = CognitiveState::from(backend_status);
        self.transition(new_state)
    }

    /// 检查 Catatonic 是否超时进入 Coma。由内部定时器调用。
    pub fn maybe_escalate_to_coma(&self) -> Option<CognitiveState> {
        if self.state() != CognitiveState::Catatonic {
            return None;
        }
        let since = self.catatonic_since.load(Ordering::Relaxed);
        if since == 0 {
            return None;
        }
        let elapsed_ms = now_ms() - since;
        if elapsed_ms >= (self.config.coma_threshold_secs as i64) * 1000 {
            self.transition(CognitiveState::Coma)
        } else {
            None
        }
    }

    fn transition(&self, to: CognitiveState) -> Option<CognitiveState> {
        let from = self.state();
        if from == to {
            return None;
        }
        self.state.store(to as u8, Ordering::Relaxed);
        if to == CognitiveState::Catatonic {
            self.catatonic_since.store(now_ms(), Ordering::Relaxed);
        } else {
            self.catatonic_since.store(0, Ordering::Relaxed);
        }
        let _ = self.tx.send(to);
        Some(to)
    }
}

/// 当前 Unix 时间戳（毫秒）。
fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_backend_status_mapping() {
        assert_eq!(
            CognitiveState::from(BackendStatus::Ok),
            CognitiveState::Lucid
        );
        assert_eq!(
            CognitiveState::from(BackendStatus::Unknown),
            CognitiveState::Groggy
        );
        assert_eq!(
            CognitiveState::from(BackendStatus::Degraded),
            CognitiveState::Groggy
        );
        assert_eq!(
            CognitiveState::from(BackendStatus::Down),
            CognitiveState::Catatonic
        );
    }

    #[test]
    fn test_initial_state_is_lucid() {
        let (machine, _rx) = CognitiveStateMachine::new(CognitiveStateConfig::default());
        assert_eq!(machine.state(), CognitiveState::Lucid);
    }

    #[test]
    fn test_transition_emits_state() {
        let (machine, _rx) = CognitiveStateMachine::new(CognitiveStateConfig::default());

        let result = machine.on_backend_status_change(BackendStatus::Down);
        assert_eq!(result, Some(CognitiveState::Catatonic));
        assert_eq!(machine.state(), CognitiveState::Catatonic);
    }

    #[test]
    fn test_no_transition_when_same_state() {
        let (machine, _rx) = CognitiveStateMachine::new(CognitiveStateConfig::default());

        // 已经是 Lucid，再传 Ok 不会触发转换
        let result = machine.on_backend_status_change(BackendStatus::Ok);
        assert_eq!(result, None);
    }

    #[test]
    fn test_watch_channel_delivers_transitions() {
        let (machine, rx) = CognitiveStateMachine::new(CognitiveStateConfig::default());

        machine.on_backend_status_change(BackendStatus::Down);
        // watch channel 应该收到新状态
        assert_eq!(*rx.borrow(), CognitiveState::Catatonic);

        machine.on_backend_status_change(BackendStatus::Ok);
        assert_eq!(*rx.borrow(), CognitiveState::Lucid);
    }

    #[test]
    fn test_catatonic_to_coma_after_threshold() {
        let config = CognitiveStateConfig {
            coma_threshold_secs: 0, // 0 秒阈值，立即进入 Coma
            ..Default::default()
        };
        let (machine, _rx) = CognitiveStateMachine::new(config);

        // 进入 Catatonic
        machine.on_backend_status_change(BackendStatus::Down);
        assert_eq!(machine.state(), CognitiveState::Catatonic);

        // 阈值 0，立即 escalate
        let result = machine.maybe_escalate_to_coma();
        assert_eq!(result, Some(CognitiveState::Coma));
        assert_eq!(machine.state(), CognitiveState::Coma);
    }

    #[test]
    fn test_coma_recovery_resets_timer() {
        let (machine, _rx) = CognitiveStateMachine::new(CognitiveStateConfig::default());

        machine.on_backend_status_change(BackendStatus::Down);
        assert_eq!(machine.state(), CognitiveState::Catatonic);

        // 恢复到 Ok
        machine.on_backend_status_change(BackendStatus::Ok);
        assert_eq!(machine.state(), CognitiveState::Lucid);

        // 不应该再 escalate
        assert_eq!(machine.maybe_escalate_to_coma(), None);
    }
}
