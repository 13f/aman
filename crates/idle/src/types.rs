//! Core idle system types: IdleKind, IdleEvent, QueueDrained, IdlePersonality, ChatMode.
//!
//! Architecture ref: idle-design.md §3

use serde::{Deserialize, Serialize};

use kernel::event::{Event, EventType};
use kernel::types::Priority;

// ---------------------------------------------------------------------------
// §3.1 IdleKind — 七种深度驱动空闲子类型
// ---------------------------------------------------------------------------

/// 由 IdleDetector 产生的空闲子类型。
///
/// 每种类型具有预定义的 arousal 行为：
/// - Passive：正常 arousal 衰减（Daze, Boredom, Waiting）
/// - Engaged：减缓或暂停 arousal 衰减（Sleep, Exploration, Meditation, Incubation）
///
/// Reflection 不在此枚举中——由 Dispatcher 的 QueueDrained 事件触发。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IdleKind {
    Daze,
    Boredom,
    Sleep,
    Exploration,
    Meditation,
    Waiting,
    Incubation,
}

/// 控制空闲状态对 arousal 衰减的影响。
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum ArousalBehavior {
    /// 标准衰减（decay_multiplier = 1.0）
    Passive,
    /// 衰减乘以 multiplier（0.0 = 不衰减, 0.5 = 半速）
    Engaged { decay_multiplier: f64 },
}

impl IdleKind {
    /// 返回此空闲类型对应的 arousal 行为。
    #[must_use]
    pub fn arousal_behavior(self) -> ArousalBehavior {
        match self {
            Self::Daze | Self::Boredom | Self::Waiting => ArousalBehavior::Passive,
            Self::Sleep => ArousalBehavior::Engaged { decay_multiplier: 0.5 },
            Self::Exploration | Self::Meditation => ArousalBehavior::Engaged { decay_multiplier: 0.0 },
            Self::Incubation => ArousalBehavior::Engaged { decay_multiplier: 0.1 },
        }
    }
}

// ---------------------------------------------------------------------------
// §3.2 IdleEvent
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IdleContext {
    pub last_event_type: String,
    /// 定容 Vec（最多保留最近 N 条空闲输出，默认 10）
    pub last_idle_outputs: Vec<String>,
    pub arousal_level: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IdleEvent {
    pub kind: IdleKind,
    pub depth: u32,
    pub duration_secs: f64,
    pub context: Option<IdleContext>,
    pub from_chat_mode: bool,
}

// ---------------------------------------------------------------------------
// §3.3 QueueDrained
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueueDrained {
    pub last_event_type: String,
    pub last_trace_id: String,
    pub last_result_summary: String,
    pub arousal_level: f64,
    pub reflection_consecutive_count: u32,
}

// ---------------------------------------------------------------------------
// Into<Event> conversions
// ---------------------------------------------------------------------------

impl From<IdleEvent> for Event {
    fn from(value: IdleEvent) -> Self {
        let payload = serde_json::to_value(value).unwrap_or_default();
        let mut event = Event::new("idle.system", EventType::Idle, payload);
        event.priority = Priority::Low;
        event
    }
}

impl From<QueueDrained> for Event {
    fn from(value: QueueDrained) -> Self {
        let payload = serde_json::to_value(value).unwrap_or_default();
        Event::new("system.dispatcher", EventType::QueueDrained, payload)
    }
}

// ---------------------------------------------------------------------------
// §3.4 IdlePersonality + ChatMode
// ---------------------------------------------------------------------------

/// 配置驱动的人格定义，决定空闲深度 → 空闲类型的映射。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IdlePersonality {
    /// 启用的空闲类型列表
    pub enabled_kinds: Vec<IdleKind>,
    /// 深度 → 空闲类型映射（按深度升序）
    pub depth_schedule: Vec<(u32, IdleKind)>,
    /// poll 间隔策略
    pub poll_interval: PollInterval,
    /// poll 间隔松弛系数
    pub poll_relaxation: PollRelaxation,
    /// 聊天模式配置
    pub chat_mode: ChatMode,
    /// Reflection 熔断配置
    pub reflection_breaker: ReflectionBreaker,
    /// 上下文隔离配置
    pub context_isolation: ContextIsolation,
}

impl Default for IdlePersonality {
    fn default() -> Self {
        Self {
            enabled_kinds: vec![
                IdleKind::Daze,
                IdleKind::Boredom,
                IdleKind::Sleep,
                IdleKind::Exploration,
                IdleKind::Meditation,
                IdleKind::Incubation,
            ],
            depth_schedule: vec![
                (0, IdleKind::Daze),
                (5, IdleKind::Boredom),
                (20, IdleKind::Sleep),
                (50, IdleKind::Exploration),
                (100, IdleKind::Meditation),
                (200, IdleKind::Incubation),
            ],
            poll_interval: PollInterval::Fixed { interval_secs: 5.0 },
            poll_relaxation: PollRelaxation::None,
            chat_mode: ChatMode::default(),
            reflection_breaker: ReflectionBreaker::default(),
            context_isolation: ContextIsolation::default(),
        }
    }
}

impl IdlePersonality {
    /// 给定 depth，返回对应的 IdleKind（阈值匹配）。
    #[must_use]
    pub fn resolve(&self, depth: u32) -> IdleKind {
        self.depth_schedule
            .iter()
            .copied()
            .rfind(|&(d, _)| d <= depth)
            .map(|(_, kind)| kind)
            .unwrap_or(IdleKind::Daze)
    }

    /// 双轴模型：depth 解锁范围 + arousal 精调具体选择。
    ///
    /// depth 决定最大可到达的 idle kind（阈值匹配），
    /// arousal 在已解锁范围内选择最合适的子类：
    /// - arousal 高 → 活跃/浅层状态（Daze, Boredom）
    /// - arousal 中 → 中层状态（Sleep, Exploration）
    /// - arousal 低 → 深层状态（Meditation, Incubation）
    ///
    /// 这形成了自然的反馈循环：浅层状态使用 Passive 衰减使 arousal 下降，
    /// 降到阈值后自动滑入深层状态；深层状态的 Engaged 衰减（multiplier 接近 0）
    /// 则维持低 arousal，使 agent 在深层状态停留更久。
    #[must_use]
    pub fn resolve_with_arousal(&self, depth: u32, arousal: f64) -> IdleKind {
        let base = self.resolve(depth);

        match base {
            IdleKind::Boredom if arousal < 0.3 => IdleKind::Sleep,
            IdleKind::Sleep if arousal > 0.6 => IdleKind::Boredom,
            IdleKind::Sleep if arousal < 0.2 => IdleKind::Exploration,
            IdleKind::Exploration if arousal > 0.5 => IdleKind::Sleep,
            IdleKind::Exploration if arousal < 0.15 => IdleKind::Meditation,
            IdleKind::Meditation if arousal > 0.4 => IdleKind::Exploration,
            IdleKind::Meditation if arousal < 0.1 => IdleKind::Incubation,
            IdleKind::Incubation if arousal > 0.3 => IdleKind::Meditation,
            _ => base,
        }
    }
}

/// 聊天模式——用户对话期间限制空闲行为。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChatMode {
    /// 聊天期间允许的空闲类型
    pub allowed_kinds: Vec<IdleKind>,
    /// 从聊天模式切换到完整模式的 grace period（秒）
    pub grace_period_secs: u64,
    /// 聊天模式下的 poll 间隔
    pub poll_interval: PollInterval,
}

impl Default for ChatMode {
    fn default() -> Self {
        Self {
            allowed_kinds: vec![IdleKind::Daze, IdleKind::Boredom],
            grace_period_secs: 60,
            poll_interval: PollInterval::Fixed { interval_secs: 2.0 },
        }
    }
}

impl ChatMode {
    /// 将聊天模式转换为一个 IdlePersonality（用于聊天期间替代完整 personality）。
    #[must_use]
    pub fn as_personality(&self) -> IdlePersonality {
        IdlePersonality {
            enabled_kinds: self.allowed_kinds.clone(),
            depth_schedule: vec![(0, IdleKind::Daze), (1, IdleKind::Boredom)],
            poll_interval: self.poll_interval,
            poll_relaxation: PollRelaxation::None,
            chat_mode: ChatMode {
                allowed_kinds: self.allowed_kinds.clone(),
                grace_period_secs: self.grace_period_secs,
                poll_interval: self.poll_interval,
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
}

// ---------------------------------------------------------------------------
// 辅助类型
// ---------------------------------------------------------------------------

/// Poll 间隔配置。
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum PollInterval {
    Fixed { interval_secs: f64 },
    Linear { base: f64, multiplier: f64 },
}

impl Default for PollInterval {
    fn default() -> Self {
        Self::Fixed { interval_secs: 5.0 }
    }
}

impl PollInterval {
    /// 给定 depth，返回下一次 poll 的延迟（秒）。
    #[must_use]
    pub fn next_delay(&self, depth: u32) -> f64 {
        match self {
            Self::Fixed { interval_secs } => *interval_secs,
            Self::Linear { base, multiplier } => base + multiplier * f64::from(depth),
        }
    }
}

/// Poll 间隔松弛系数——空闲越久，poll 越不频繁。
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PollRelaxation {
    None,
    Linear { slope: f64 },
    Exponential { factor: f64 },
}

impl Default for PollRelaxation {
    fn default() -> Self {
        Self::None
    }
}

/// Reflection 熔断器配置。
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ReflectionBreaker {
    /// 触发熔断的连续无产出 Reflection 次数
    pub max_consecutive: u32,
    /// 完全熔断后的冷却时间（秒）
    pub cooldown_secs: u64,
}

impl Default for ReflectionBreaker {
    fn default() -> Self {
        Self {
            max_consecutive: 5,
            cooldown_secs: 300,
        }
    }
}

/// 上下文隔离配置。
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ContextIsolation {
    /// 空闲输出是否污染聊天历史
    pub pollute_chat_history: bool,
    /// 用户输入到达时是否丢弃当前 idle context
    pub suspend_on_user_input: bool,
}

impl Default for ContextIsolation {
    fn default() -> Self {
        Self {
            pollute_chat_history: false,
            suspend_on_user_input: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // T1.2 — IdleKind + ArousalBehavior 测试
    // -----------------------------------------------------------------------

    #[test]
    fn idle_kind_serde_roundtrip() {
        for kind in &[
            IdleKind::Daze,
            IdleKind::Boredom,
            IdleKind::Sleep,
            IdleKind::Exploration,
            IdleKind::Meditation,
            IdleKind::Waiting,
            IdleKind::Incubation,
        ] {
            let json = serde_json::to_string(kind).expect("serialize");
            let deserialized: IdleKind = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(*kind, deserialized);
        }
    }

    #[test]
    fn idle_kind_from_str() {
        assert_eq!(
            serde_json::from_str::<IdleKind>("\"sleep\"").unwrap(),
            IdleKind::Sleep,
        );
    }

    #[test]
    fn arousal_behavior_mapping() {
        assert_eq!(IdleKind::Daze.arousal_behavior(), ArousalBehavior::Passive);
        assert_eq!(IdleKind::Boredom.arousal_behavior(), ArousalBehavior::Passive);
        assert_eq!(IdleKind::Waiting.arousal_behavior(), ArousalBehavior::Passive);
        assert_eq!(
            IdleKind::Sleep.arousal_behavior(),
            ArousalBehavior::Engaged { decay_multiplier: 0.5 },
        );
        assert_eq!(
            IdleKind::Exploration.arousal_behavior(),
            ArousalBehavior::Engaged { decay_multiplier: 0.0 },
        );
        assert_eq!(
            IdleKind::Meditation.arousal_behavior(),
            ArousalBehavior::Engaged { decay_multiplier: 0.0 },
        );
        assert_eq!(
            IdleKind::Incubation.arousal_behavior(),
            ArousalBehavior::Engaged { decay_multiplier: 0.1 },
        );
    }

    // -----------------------------------------------------------------------
    // T1.4 — Personality + ChatMode 测试
    // -----------------------------------------------------------------------

    #[test]
    fn resolve_returns_daze_for_undefined_depth() {
        let p = IdlePersonality {
            enabled_kinds: vec![IdleKind::Daze, IdleKind::Boredom],
            depth_schedule: vec![(0, IdleKind::Daze), (1, IdleKind::Boredom)],
            poll_interval: PollInterval::Fixed { interval_secs: 5.0 },
            poll_relaxation: PollRelaxation::None,
            chat_mode: ChatMode {
                allowed_kinds: vec![],
                grace_period_secs: 60,
                poll_interval: PollInterval::Fixed { interval_secs: 2.0 },
            },
            reflection_breaker: ReflectionBreaker {
                max_consecutive: 5,
                cooldown_secs: 300,
            },
            context_isolation: ContextIsolation {
                pollute_chat_history: false,
                suspend_on_user_input: true,
            },
        };
        assert_eq!(p.resolve(0), IdleKind::Daze);
        assert_eq!(p.resolve(1), IdleKind::Boredom);
        // Uses threshold match: d=1 (Boredom) is the largest d <= 2
        assert_eq!(p.resolve(2), IdleKind::Boredom);
        assert_eq!(p.resolve(5), IdleKind::Boredom);
    }

    #[test]
    fn resolve_uses_threshold_for_deep_depths() {
        let p = IdlePersonality::default();
        assert_eq!(p.resolve(0), IdleKind::Daze);
        assert_eq!(p.resolve(4), IdleKind::Daze);       // d=0 ≤ 4
        assert_eq!(p.resolve(5), IdleKind::Boredom);    // d=5 ≤ 5
        assert_eq!(p.resolve(19), IdleKind::Boredom);   // d=5 ≤ 19
        assert_eq!(p.resolve(20), IdleKind::Sleep);     // d=20 ≤ 20
        assert_eq!(p.resolve(49), IdleKind::Sleep);     // d=20 ≤ 49
        assert_eq!(p.resolve(50), IdleKind::Exploration);
        assert_eq!(p.resolve(99), IdleKind::Exploration);
        assert_eq!(p.resolve(100), IdleKind::Meditation);
        assert_eq!(p.resolve(199), IdleKind::Meditation);
        assert_eq!(p.resolve(200), IdleKind::Incubation);
        assert_eq!(p.resolve(999), IdleKind::Incubation);
    }

    #[test]
    fn resolve_with_arousal_selects_within_unlocked_range() {
        let p = IdlePersonality::default();

        // Depth 5 → Boredom unlocked. High arousal stays at Boredom.
        assert_eq!(p.resolve_with_arousal(5, 0.9), IdleKind::Boredom);
        // Low arousal drops into Sleep.
        assert_eq!(p.resolve_with_arousal(5, 0.2), IdleKind::Sleep);

        // Depth 20 → Sleep unlocked. Very high arousal pushes back to Boredom.
        assert_eq!(p.resolve_with_arousal(20, 0.8), IdleKind::Boredom);
        // Very low arousal pushes into Exploration.
        assert_eq!(p.resolve_with_arousal(20, 0.1), IdleKind::Exploration);

        // Depth 50 → Exploration unlocked. High arousal pulls back to Sleep.
        assert_eq!(p.resolve_with_arousal(50, 0.7), IdleKind::Sleep);
        // Very low arousal pushes into Meditation.
        assert_eq!(p.resolve_with_arousal(50, 0.1), IdleKind::Meditation);

        // Depth 100 → Meditation unlocked. Still high arousal → Exploration.
        assert_eq!(p.resolve_with_arousal(100, 0.6), IdleKind::Exploration);
        // Very low → Incubation.
        assert_eq!(p.resolve_with_arousal(100, 0.05), IdleKind::Incubation);

        // Depth 200 → Incubation unlocked. High arousal → Meditation.
        assert_eq!(p.resolve_with_arousal(200, 0.5), IdleKind::Meditation);

        // Depth 0 → always Daze regardless of arousal.
        assert_eq!(p.resolve_with_arousal(0, 0.9), IdleKind::Daze);
        assert_eq!(p.resolve_with_arousal(0, 0.1), IdleKind::Daze);
    }

    #[test]
    fn chat_mode_as_personality_depth_one_always_boredom() {
        let chat = ChatMode {
            allowed_kinds: vec![IdleKind::Daze, IdleKind::Boredom],
            grace_period_secs: 30,
            poll_interval: PollInterval::Fixed { interval_secs: 2.0 },
        };
        let p = chat.as_personality();
        assert_eq!(p.resolve(0), IdleKind::Daze);
        assert_eq!(p.resolve(1), IdleKind::Boredom);
        // Chat mode schedule only has (0,Daze),(1,Boredom), so depth>=2 stays at Boredom
        assert_eq!(p.resolve(2), IdleKind::Boredom);
        assert!(!p.context_isolation.pollute_chat_history);
    }

    #[test]
    fn poll_interval_linear_computation() {
        let linear = PollInterval::Linear {
            base: 2.0,
            multiplier: 0.5,
        };
        assert!((linear.next_delay(0) - 2.0).abs() < f64::EPSILON);
        assert!((linear.next_delay(5) - 4.5).abs() < f64::EPSILON);
        assert!((linear.next_delay(10) - 7.0).abs() < f64::EPSILON);
    }

    #[test]
    fn poll_interval_fixed() {
        let fixed = PollInterval::Fixed { interval_secs: 10.0 };
        assert!((fixed.next_delay(0) - 10.0).abs() < f64::EPSILON);
        assert!((fixed.next_delay(100) - 10.0).abs() < f64::EPSILON);
    }

    // -----------------------------------------------------------------------
    // T1.3 — IdleEvent + QueueDrained Into<Event> 测试
    // -----------------------------------------------------------------------

    #[test]
    fn idle_event_into_event_has_low_priority() {
        let idle = IdleEvent {
            kind: IdleKind::Daze,
            depth: 0,
            duration_secs: 0.0,
            context: None,
            from_chat_mode: false,
        };
        let event: Event = idle.into();
        assert_eq!(event.priority, Priority::Low);
        assert_eq!(event.event_type, EventType::Idle);
        assert_eq!(event.source.as_str(), "idle.system");
    }

    #[test]
    fn queue_drained_into_event() {
        let qd = QueueDrained {
            last_event_type: "timer:heartbeat".into(),
            last_trace_id: "t1".into(),
            last_result_summary: "ok".into(),
            arousal_level: 0.8,
            reflection_consecutive_count: 0,
        };
        let event: Event = qd.into();
        assert_eq!(event.event_type, EventType::QueueDrained);
        assert_ne!(event.priority, Priority::Low);
    }

    #[test]
    fn idle_event_is_not_external_source() {
        let idle = IdleEvent {
            kind: IdleKind::Daze,
            depth: 0,
            duration_secs: 0.0,
            context: None,
            from_chat_mode: false,
        };
        let event: Event = idle.into();
        assert!(!event.is_from_external_source());
        assert!(event.is_idle_event());
    }

    #[test]
    fn queue_drained_event_is_not_external_source() {
        let qd = QueueDrained {
            last_event_type: "test".into(),
            last_trace_id: "t1".into(),
            last_result_summary: "ok".into(),
            arousal_level: 0.0,
            reflection_consecutive_count: 0,
        };
        let event: Event = qd.into();
        assert!(!event.is_from_external_source());
        assert!(event.is_queue_drained());
    }

    #[test]
    fn queue_drained_serializes_camel_case() {
        let qd = QueueDrained {
            last_event_type: "timer:test".into(),
            last_trace_id: "abc".into(),
            last_result_summary: "done".into(),
            arousal_level: 0.5,
            reflection_consecutive_count: 3,
        };
        let json = serde_json::to_value(&qd).expect("serialize");
        // Verify camelCase field names
        assert!(json.get("lastEventType").is_some(), "expected camelCase 'lastEventType'");
        assert!(json.get("lastTraceId").is_some(), "expected camelCase 'lastTraceId'");
        assert!(json.get("lastResultSummary").is_some(), "expected camelCase 'lastResultSummary'");
        assert!(json.get("arousalLevel").is_some(), "expected camelCase 'arousalLevel'");
        assert!(json.get("reflectionConsecutiveCount").is_some(), "expected camelCase 'reflectionConsecutiveCount'");
    }
}
