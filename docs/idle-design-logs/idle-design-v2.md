# Idle State System — Architecture Design

> 将 Windows "空闲进程"的隐喻落地为 Aman Agent 框架的正式子系统。
> 空闲不是无事可做，而是 Agent 在内省、维护、探索、复盘——用未被使用的周期做有价值的事。
>
> **八种空闲状态**：其中七种由 IdleDetector 根据空闲深度产生，一种（Reflection）由 Dispatcher
> 在队列清空时通过 QueueDrained 事件触发——不属于深度驱动的空闲序列。
>
> **审计状态**：已通过 R1 业务逻辑审计（idle-design-r1.md），11 项发现全部在此版本中处理。
> P0 时序竞态、P1 Reflection 不可打断、P1 聊天场景误触发——均已修复。
> P2 配额/深度/arousal/熔断/线程/对话隔离——均已明确或约束。

---

## 1. Problem Statement

### 要解决的问题

Aman 是事件响应式框架——一切行为由事件驱动。但事件队列为空时，Agent 陷入"真空"：什么都不做，也什么都没学到。这不是设计缺陷，而是**设计空白**。

更重要的是：事件刚处理完、队列刚清空的那一刻，Agent 的 arousal 还高、上下文还热——应该立刻复盘刚完成的任务、检查是否有连锁任务。这是 Dispatcher 的责任，不是 IdleDetector 的责任。

类比：Windows 内核中 CPU 永远不会"什么也不做"。当没有用户进程需要调度时，系统切换到 `System Idle Process`（PID 0）——一个专门捕获空闲周期的特殊进程。Aman 需要同等级别的"空闲进程"。

### 核心约束

| 约束类型 | 内容 |
|---------|------|
| **不可变（框架哲学）** | 一切行为仍是事件驱动。空闲不引入新的执行模型，只是产生新类型的事件 |
| **可变（业务策略）** | 哪些空闲类型启用、阈值、每个 Agent 的"空闲人格"；聊天场景 vs 系统场景 |
| **技术限制** | 空闲检测必须在事件循环内部，不能依赖外部 cron |
| **性能约束** | 空闲检测本身不能成为 CPU 热点。在空闲状态下，检测逻辑本身消耗应 <1% CPU |
| **时序约束** | Reflection 必须在事件处理完成后、真正空闲开始前执行；Reflection 可被真实事件打断 |
| **聊天场景约束** | 对话轮次之间不应触发完整空闲序列——用户随时可能继续对话，深度空闲会污染上下文 |

---

## 2. Design Philosophy

```
空闲不是"没有事件"，而是一类特殊的事件。
它们定义了 Agent 在没有外部输入时如何与自己相处。
```

五条设计原则：

1. **IdleEvent 是 Event 的合法子类型** — 空闲事件通过 Event Bus 路由，与其他事件一视同仁。
2. **QueueDrained 是空闲入口** — Dispatcher 在队列清空时产生 QueueDrained 事件，触发 Reflection 复盘。Reflection 是 Active 态的尾巴，可被新事件抢先打断。
3. **空闲深度决定空闲类型** — Reflection 完成后，Agent 进入真正的空闲序列。连续空闲的时间（深度）驱动状态变迁：Daze → Boredom → Sleep → deeper。
4. **配置驱动人格** — 不同 Agent 实例可以有不同的"空闲人格"：喜欢探索 vs 喜欢发呆，阈值不同，优先级不同。
5. **上下文感知空闲** — 空闲策略根据最后活跃的事件源自适应。聊天场景下只允许无副作用的浅层空闲（Daze/Boredom），系统场景下允许完整序列。

---

## 3. Type System

### 3.1 IdleKind — 七种深度驱动空闲子类型

```rust
/// 由 IdleDetector 产生的空闲子类型。
/// 对应 Agent 在队列持续为空时，随深度递增的行为模式。
///
/// 每种类型具有预定义的 arousal 行为：
/// - Passive：正常 arousal 衰减（Daze, Boredom, Waiting）
/// - Engaged：减缓或暂停 arousal 衰减（Sleep, Exploration, Meditation, Incubation）
///
/// Reflection 不在此枚举中——由 Dispatcher 的 QueueDrained 事件触发。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IdleKind {
    /// 真正的"空转"，最低功耗状态。
    /// Arousal: Passive（正常衰减）
    Daze,

    /// 无特定目标，低成本随机漫游。
    /// Arousal: Passive（正常衰减）
    Boredom,

    /// 维护性后台处理——记忆整理、缓存清理、短期→长期记忆转移。
    /// Arousal: Engaged（衰减速率 ×0.5）
    Sleep,

    /// 基于特定兴趣或假设的主动信息收集。
    /// Arousal: Engaged（衰减暂停——exploration 本身证明了 arousal 仍高）
    Exploration,

    /// 内省性处理——重评目标函数权重、检测策略矛盾、生成元认知报告。
    /// Arousal: Engaged（衰减暂停）
    Meditation,

    /// 条件性等待——期待某个特定事件，处于低功耗但持续检查条件。
    /// Arousal: Passive（正常衰减）
    Waiting,

    /// 后台低优先级进程——不主动搜索但允许潜意识式关联。
    /// Arousal: Engaged（衰减速率 ×0.1）
    Incubation,
}

impl IdleKind {
    /// 该空闲类型的 arousal 衰减行为
    pub fn arousal_behavior(&self) -> ArousalBehavior {
        match self {
            Self::Daze | Self::Boredom | Self::Waiting => ArousalBehavior::Passive,
            Self::Sleep => ArousalBehavior::Engaged { decay_multiplier: 0.5 },
            Self::Exploration | Self::Meditation => ArousalBehavior::Engaged { decay_multiplier: 0.0 },
            Self::Incubation => ArousalBehavior::Engaged { decay_multiplier: 0.1 },
        }
    }
}
```

### 3.2 IdleEvent — IdleDetector 产生的空闲事件

```rust
/// 当事件队列持续为空（Reflection 已经完成之后），
/// IdleDetector 在每次 poll 时产生此事件注入 Event Bus。
///
/// - priority 始终为 Low（空闲事件不抢占正常事件）
/// - 携带 depth 和 duration 供 Dispatcher 路由决策
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IdleEvent {
    pub kind: IdleKind,
    pub depth: u32,
    pub duration_secs: f64,
    pub context: Option<IdleContext>,
}

/// 空闲上下文——累积最近 N 轮空闲的产出。
/// last_idle_outputs 是定容环形缓冲，默认保留最近 5 条产出。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IdleContext {
    pub last_event_type: String,

    /// 最近 N 轮空闲产出的摘要（定容 Vec，新产出 push + 旧产出 pop_front）
    pub last_idle_outputs: Vec<String>,

    pub arousal_level: f64,

    /// 最后活跃事件是否来自 Chat Source（影响空闲人格选择）
    pub last_event_from_chat: bool,
}
```

### 3.3 QueueDrained — Dispatcher 产生的队列清空事件

```rust
/// 由 Dispatcher 在以下条件同时满足时产生：
/// 1. 刚处理完一个真实事件（非 QueueDrained、非 IdleEvent）
/// 2. Event Bus 队列已空
/// 3. Reflection 熔断未激活（参见 IdlePersonality.reflection_breaker）
///
/// 此事件触发 Reflection Pipeline——对刚完成任务的轻量复盘。
///
/// Reflection 在 Dispatcher 主循环中通过 select! 执行：
/// - 优先等待新事件到达（新事件抢先 Reflection）
/// - Reflection 超时或被抢先 → 提前结束
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueueDrained {
    pub last_event_type: String,
    pub last_trace_id: String,
    pub last_result_summary: Option<String>,
    pub arousal_level: f64,

    /// 连续 Reflection 的触发次数（用于熔断检测）
    /// 每次真实事件→QueueDrained 加 1；真正空闲或超熔断阈值后重置
    pub reflection_consecutive_count: u32,
}

impl EventKind {
    pub const QUEUE_DRAINED: &'static str = "system.queue_drained";
}
```

### 3.4 IdlePersonality — 每个 Agent 的空闲人格

```rust
/// Agent 的空闲人格——控制空闲行为的可配置参数。
///
/// 支持"聊天模式"：当 last_event_from_chat == true 时，
/// 自动切换到 chat_personality 子配置，限制可进入的空闲类型。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IdlePersonality {
    /// 各空闲类型的启用标志（不包含 Reflection）
    pub enabled_kinds: HashSet<IdleKind>,

    /// 空闲深度 → 空闲类型的映射。depth=0 固定为 Daze。
    pub depth_schedule: Vec<(u32, IdleKind)>,

    /// IdleDetector 的 poll 间隔（秒）。
    pub poll_interval: PollInterval,

    /// Poll 频率松弛（原 deep_sleep）：仅调整 poll 频率，不影响 idle kind。
    /// 达到 depth_threshold 后，poll 间隔切换到 interval_secs。
    pub poll_relaxation: Option<PollRelaxation>,

    /// 聊天场景子人格（当 last_event_from_chat == true 时生效）
    pub chat_mode: Option<ChatMode>,

    /// Reflection 熔断配置
    pub reflection_breaker: ReflectionBreaker,

    /// 空闲上下文隔离策略
    pub context_isolation: ContextIsolation,
}

/// Poll 频率松弛：仅影响 IdleDetector 的 poll 间隔，不改变 idle kind。
/// idle kind 始终由 depth_schedule 决定。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PollRelaxation {
    pub depth_threshold: u32,
    pub interval_secs: f64,
}

/// 聊天模式：当最近一次事件来自 Chat Source 时生效。
/// grace_period_secs 后若无新聊天事件，退出聊天模式，恢复完整空闲人格。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMode {
    /// 聊天模式下的空闲类型白名单（建议仅 Daze + Boredom）
    pub allowed_kinds: HashSet<IdleKind>,

    /// 退出聊天模式的宽限期（秒）。在此期间仅允许浅层空闲。
    pub grace_period_secs: f64,

    /// 聊天模式下的 poll 间隔（建议 1-2s，更快感知用户回复）
    pub poll_interval: PollInterval,
}

/// Reflection 熔断：防止连锁任务形成无限循环。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReflectionBreaker {
    /// 连续 Reflection 触发次数上限
    pub max_consecutive: u32,          // 默认 5

    /// 超过上限后的熔断冷却时间（秒）
    pub cooldown_secs: f64,            // 默认 30

    /// 超过上限后，Reflection 跳过 lessons_learned（只检查 chain_tasks + errors）
    /// 如果再超 2×max_consecutive → 跳过所有检查，直接进入 Daze
    pub escalate_on_double: bool,      // 默认 true
}

/// 空闲上下文隔离：控制空闲操作对活跃对话上下文的污染程度。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextIsolation {
    /// 空闲操作是否自动进入对话历史（默认 false）
    pub pollute_chat_history: bool,

    /// 用户消息到达时是否"挂起"空闲上下文而非合并（默认 true）
    pub suspend_on_user_input: bool,
}
```

### 3.5 Shared State — 跨组件协调标志

```rust
/// IdleDetector 和 Dispatcher 之间的共享状态。
/// 解决 P0 时序竞态：Reflection 执行期间 IdleDetector 不应产生 IdleEvent。
#[derive(Clone)]
pub struct IdleCoordination {
    /// Dispatcher 设置：Reflection 执行中 → true
    /// IdleDetector 读取：true → 跳过本轮，不产生 IdleEvent，不递增 depth
    pub busy_reflecting: Arc<AtomicBool>,

    /// 共享的 ArousalTracker 实例
    pub arousal: Arc<ArousalTracker>,
}

impl IdleCoordination {
    pub fn new() -> Self {
        Self {
            busy_reflecting: Arc::new(AtomicBool::new(false)),
            arousal: Arc::new(ArousalTracker::default()),
        }
    }
}
```

---

## 4. State Machine

### 4.1 完整状态转移

Reflection 是 Active 的尾巴——事件处理完成、队列清空的瞬间执行。**可被新到达的真实事件抢先打断**。
Daze 是 Idle 的起点——Reflection 无连锁任务后，Agent 进入真正的空闲序列。
聊天场景下（last_event_from_chat），空闲人格受限——只允许 Daze/Boredom。

```
                        ┌──────────────────────────────────────────────────────┐
                        │      (任何真实事件到达 → 立即回到 Active)                  │
                        │      (Reflection 运行中 → select! 抢先 → 取消 Reflection) │
                        │                                                      │
                        ▼                                                      │
    ┌────────┐  queue drained  ┌────────────┐                                  │
    │ Active │────────────────▶│ Reflection │ (不计深度，可被真实事件抢先打断)       │
    └────────┘   (QueueDrained)└──────┬─────┘                                  │
         ▲                  │         │                                        │
         │    真实事件抢先    │  无连锁  │  有连锁任务                              │
         │    取消Reflection │  任务    │  产生新事件                              │
         │         │         │         │                                        │
         │         │         ▼         │                                        │
         │         │    ┌──────────────────────────────────────┐                │
         │         │    │      空闲人格选择（自适应）              │                │
         │         │    │  last_event_from_chat?               │                │
         │         │    │    ├─ true → ChatMode (仅Daze/Boredom)│               │
         │         │    │    └─ false → 完整 depth_schedule     │               │
         │         │    └──────────────┬───────────────────────┘                │
         │         │                   │                                        │
         │         │                   ▼                                        │
         │         │           ┌──────────┐  depth=1  ┌──────────┐             │
         │         │           │   DAZE   │──────────▶│ BOREDOM  │             │
         │         │           └──────────┘           └──────────┘             │
         │         │            depth=0                  │                      │
         │         │                              (chat_mode → 到此为止)         │
         │         │                                                    │       │
         │         │                              (完整模式 → 继续)  depth=3│    │
         │         │                                                    ▼       │
         │         │                                              ┌──────────┐  │
         │         │                                              │  SLEEP   │  │
         │         │                                              └──────────┘  │
         │         │                                             depth=5   │    │
         │         │                                 ┌──────────────────────┘    │
         │         │                                 ▼                           │
         │         │                 ┌──────────┐  ┌──────────┐  ┌──────────┐   │
         │         │                 │EXPLORATION│  │MEDITATION│  │INCUBATION│   │
         │         │                 └──────────┘  └──────────┘  └──────────┘   │
         │         │                      │              │              │        │
         │         └──────────────────────┴──────────────┴──────────────┘        │
         │                      (连锁任务事件)                                     │
         └───────────────────────────────────────────────────────────────────────┘
```

**规则**：

1. **QueueDrained 由 Dispatcher 产生** — 非 IdleDetector。每次处理完一个真实事件且队列为空时触发一次 Reflection。
2. **Reflection 可被抢先** — Dispatcher 用 select! 同时等待 Reflection 完成和新事件到达。新事件到达 → 取消 Reflection → 处理新事件。
3. **Reflection 有 timeout 和熔断** — timeout=60s（可配）；连续 5 次 Reflection 后触发熔断。
4. **Reflection 不占深度** — depth 从 Daze 开始计数（depth=0）。
5. **聊天模式人格切换** — last_event_from_chat 为 true 时，仅允许 Daze 和 Boredom，grace_period_secs 后恢复。
6. **深度递增** — 连续空闲轮次驱动。空闲类型之间切换不重置深度。
7. **事件打断** — 任何 `priority > Low` 的事件到达 → 立即回到 Active。
8. **Poll 松弛** — `poll_relaxation` 仅调整 poll 频率，不改变 idle kind。

### 4.2 完整的事件处理→空闲→再唤醒流程

```
时间轴:
──────────────────────────────────────────────────────────────▶

[事件 A 到达]
  │
  ├─ Dispatcher: 取出 A, 路由到 Pipeline, Pipeline 执行完毕
  │
  ├─ Dispatcher: try_dequeue() → None
  │   recently_processed_real_event == true
  │   reflection_consecutive_count < max_consecutive (熔断未激活)
  │   → 设置 busy_reflecting = true
  │   → 发布 QueueDrained 到 Event Bus
  │
  ├─ Dispatcher: 取出 QueueDrained
  │   → select! {
  │         reflection_pipeline.run() => { /* Reflection 完成或超时 */ }
  │         event_bus.wait_for_event() => { /* 新事件到达 → 取消 Reflection */ }
  │      }
  │
  ├─ 情况 A: 新事件在 Reflection 期间到达
  │   → 取消 Reflection → busy_reflecting = false
  │   → 处理新事件（真实事件）→ 回到 Active
  │
  ├─ 情况 B: Reflection 完成，有连锁任务产出
  │   → busy_reflecting = false
  │   → 注入新事件 → 取出 → 处理 → reflection_consecutive_count++
  │   → 再次 QueueDrained（如果未达熔断阈值）
  │
  ├─ 情况 C: Reflection 完成，无产出
  │   → busy_reflecting = false
  │   → reflection_consecutive_count = 0
  │   → 真正空闲开始
  │
  ├─ [真正空闲]
  │   ├─ 检查 last_event_from_chat?
  │   │   ├─ true → 聊天模式: 仅 Daze → Boredom (graced_period_secs 后恢复)
  │   │   └─ false → 完整模式: Daze → Boredom → Sleep → Exploration/Meditation
  │   │
  │   ├─ IdleDetector poll 1: queue empty, busy_reflecting=false
  │   │   → 产生 IdleEvent(kind=Daze, depth=0)
  │   │
  │   ├─ IdleDetector poll N: queue empty, busy_reflecting=false
  │   │   → depth_schedule 映射 → Sleep/Exploration...
  │   │   → Arousal: Engaged 类型暂停衰减
  │   │
  │   ├─ [事件 B 到达] ← 外部 Source 产生
  │   │
  │   └─ Dispatcher: 取出 B
  │       recently_processed_real_event = true
  │       IdleDetector.next_poll: pending_count > 0 → depth = 0
  │       → 回到 Active，处理 B
```

### 4.3 深度→类型映射的默认配置

```yaml
idle:
  personality:
    enabled_kinds: [daze, boredom, sleep, exploration, meditation]
    depth_schedule:
      - [1, boredom]
      - [3, sleep]
      - [5, exploration]
    poll_interval:
      linear: { base: 5.0, multiplier: 2.0 }
    poll_relaxation:
      depth_threshold: 15
      interval_secs: 60

    chat_mode:
      allowed_kinds: [daze, boredom]     # 聊天时最多到无聊
      grace_period_secs: 60              # 60s 无消息后恢复完整模式
      poll_interval:
        fixed: 2.0                       # 聊天时更频繁地检查

    reflection_breaker:
      max_consecutive: 5
      cooldown_secs: 30
      escalate_on_double: true

    context_isolation:
      pollute_chat_history: false        # 空闲操作不进对话历史
      suspend_on_user_input: true        # 用户消息到达时挂起空闲上下文

  reflection:
    enabled: true
    timeout_secs: 60
    check_items:
      - chain_tasks
      - immediate_errors
      - lessons_learned
```

### 4.4 打断策略矩阵

| 空闲/过渡状态 | 触发者 | 可被真实事件打断？ | 打断损失 | 进度保存 | 打断后行为 |
|-------------|--------|:----------------:|---------|---------|-----------|
| Reflection | Dispatcher (QueueDrained) | **是（select! 抢先）** | 无 | 不需要 | Reflection 取消，新事件立即处理 |
| Daze | IdleDetector | 是 | 无 | 不需要 | 立即唤醒 |
| Boredom | IdleDetector | 是 | 无 | 不需要 | 立即丢弃，回到 Active |
| Sleep | IdleDetector | 是 | 中 | WAL checkpoint | 恢复时从 checkpoint 继续或丢弃 |
| Exploration | IdleDetector | 是 | 低 | 断点保存 | 恢复时从断点继续 |
| Meditation | IdleDetector | 是 | 高 | 不保存（见下文） | 丢弃，下次空闲重新触发 |
| Waiting | IdleDetector | 是 | 无 | 不需要 | 条件满足→Active，否则继续等待 |
| Incubation | IdleDetector | 是 | 低 | 关联状态列表 | 后台线程通过 CancellationToken 取消 |

**Meditation 文件安全保障**：报告写入使用临时文件 + 原子 rename。写入至 `.tmp` 文件 → 完成后 rename 为目标文件名。即使 shutdown 中断，残留的也是 `.tmp` 文件，不会损坏正式报告。

### 4.5 Reflection 熔断机制

```
事件处理 → QueueDrained → Reflection
    ├─ 产出事件 → 处理 → QueueDrained → Reflection  (consecutive_count++)
    ├─ 产出事件 → 处理 → QueueDrained → Reflection  (consecutive_count++)
    ├─ ...
    │
    ├─ consecutive_count == max_consecutive (5)
    │   → 熔断激活
    │   → Reflection 跳过 lessons_learned（只查 chain_tasks + immediate_errors）
    │   → 如果仍连续触发到 2×max_consecutive (10)
    │       → 完全跳过 Reflection，直接进入 Daze
    │       → cooldown_secs (30s) 内禁止任何 Reflection
    │
    └─ 某次 Reflection 无产出
        → consecutive_count 重置 = 0
        → 熔断解除
```

---

## 5. Integration with Aman Runtime

### 5.1 架构位置

IdleDetector 作为 **Event Source** 插入 Source 层。
QueueDrained 由 **Dispatcher** 产生，经 Event Bus 流动。
**IdleCoordination** 共享状态连接 Dispatcher 和 IdleDetector，解决时序竞态。

```
┌──────────────────────────────────────────────────────────────────┐
│                         Agent Runtime                             │
│                                                                   │
│  ┌──────────┐  ┌──────────┐  ┌──────────┐  ┌────────────┐       │
│  │  Timer   │  │ FileWatch│  │ Webhook  │  │IdleDetector│       │
│  │ Source   │  │ Source   │  │ Source   │  │  Source    │       │
│  └────┬─────┘  └────┬─────┘  └────┬─────┘  └─────┬──────┘       │
│       └──────────────┴──────────────┴──────────────┘              │
│                             │                                      │
│              ┌──────────────┼──────────────┐                       │
│              │              │              │                       │
│              ▼              ▼              ▼                       │
│        Dispatcher  ◄── 出队  │  ── 入队 ──▶ Dispatcher             │
│        (消费者)    │         │             (QueueDrained 生产者)    │
│              │     │         │                                     │
│              │     │    ┌────▼────────────┐                        │
│              │     │    │IdleCoordination │ ← 共享状态              │
│              │     │    │ busy_reflecting │                        │
│              │     │    │ arousal_tracker │                        │
│              │     │    └─────────────────┘                        │
│              │                                                    │
│     route: system.queue_drained → pipeline:reflection              │
│     route: idle.*               → pipeline/workflow:idle-*         │
│              │                                                     │
│     ┌────────┼────────┐                                            │
│     ▼        ▼        ▼                                            │
│  ┌──────────┐  ┌──────────┐  ┌──────────┐                        │
│  │Reflection│  │  Idle    │  │  Idle    │                        │
│  │ Pipeline │  │ Pipeline │  │ Workflow │                        │
│  │(select!  │  │(Boredom, │  │(Sleep,   │                        │
│  │ + timeout│  │ Daze)    │  │Explor...)│                        │
│  │ 60s)     │  │          │  │          │                        │
│  └──────────┘  └──────────┘  └──────────┘                        │
│        │              │              │                             │
│        └──────────────┼──────────────┘                             │
│                       │                                            │
│              ┌────────▼────────┐                                   │
│              │   Tool Runner   │                                   │
│              └─────────────────┘                                   │
└──────────────────────────────────────────────────────────────────┘
```

### 5.2 Dispatcher 主循环（含 select! 和 BusyReflecting）

```rust
impl Dispatcher {
    async fn run_loop(&mut self, coord: IdleCoordination) {
        let mut recently_processed_real_event = false;
        let mut reflection_consecutive_count: u32 = 0;

        loop {
            match self.event_bus.try_dequeue().await {
                Some(event) => {
                    let is_real = !event.is_queue_drained() && !event.is_idle_event();

                    if is_real {
                        recently_processed_real_event = true;
                    }

                    if event.is_queue_drained() {
                        // Reflection Pipeline —— 通过 select! 可被新事件抢先
                        coord.busy_reflecting.store(true, Ordering::SeqCst);

                        let reflection = self.resolve_pipeline("pipeline:reflection");

                        select! {
                            result = reflection.execute(&event) => {
                                // Reflection 完成或超时
                                coord.busy_reflecting.store(false, Ordering::SeqCst);

                                if result.has_output() {
                                    // 有连锁任务 → 注入 Event Bus
                                    for new_event in result.output_events() {
                                        self.event_bus.publish(new_event).await;
                                    }
                                } else {
                                    // 无产出 → 熔断计数重置
                                    reflection_consecutive_count = 0;
                                }
                            }
                            _ = self.event_bus.wait_for_event() => {
                                // 新事件到达 → 抢先取消 Reflection
                                coord.busy_reflecting.store(false, Ordering::SeqCst);
                                reflection.abort();
                                // continue → 下一轮取出新事件
                            }
                        }
                    } else {
                        // 非 QueueDrained 事件 → 正常 dispatch（阻塞等待完成）
                        self.dispatch(event).await;
                    }
                }
                None => {
                    // 队列空
                    if recently_processed_real_event {
                        recently_processed_real_event = false;

                        // 熔断检查
                        let breaker = &self.idle_config.personality.reflection_breaker;
                        if reflection_consecutive_count >= breaker.max_consecutive * 2 {
                            // 双倍阈值 → 完全跳过 Reflection，直接空闲
                            reflection_consecutive_count = 0;
                            tokio::time::sleep(Duration::from_secs(breaker.cooldown_secs)).await;
                            continue;
                        }

                        let drained = QueueDrained {
                            last_event_type: self.last_event_type.clone(),
                            last_trace_id: self.last_trace_id.clone(),
                            last_result_summary: self.last_result_summary.take(),
                            arousal_level: coord.arousal.current(),
                            reflection_consecutive_count,
                        };

                        self.event_bus.publish(drained.into_event()).await;
                        reflection_consecutive_count += 1;
                    }
                    // 队列空且无待复盘 → 真正空闲
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            }
        }
    }
}
```

### 5.3 IdleDetector（含 BusyReflecting 检查）

```rust
/// IdleDetector: 感知事件队列持续为空。
///
/// 与 QueueDrained 的分工：
/// - QueueDrained（Dispatcher）：队列刚清空的瞬间，触发 Reflection
/// - IdleDetector：Reflection 之后队列仍为空，按深度产生 IdleEvent
///
/// 关键：通过 IdleCoordination.busy_reflecting 避免在 Reflection 期间误判为空闲。
#[async_trait]
impl EventSource for IdleDetector {
    async fn poll(&mut self, ctx: &SourceContext) -> Result<Vec<Event>> {
        // P0 fix: Reflection 执行中不产生 IdleEvent
        if self.coord.busy_reflecting.load(Ordering::SeqCst) {
            return Ok(vec![]);
        }

        let queue_empty = ctx.event_bus.pending_count() == 0;

        if !queue_empty {
            self.idle_depth = 0;
            self.last_non_idle = Instant::now();
            return Ok(vec![]);
        }

        // 队列空 → 真实的空闲
        let personality = self.effective_personality();

        let kind = if self.idle_depth == 0 {
            IdleKind::Daze
        } else {
            personality.resolve(self.idle_depth, &self.agent_state)
        };

        // Arousal: 根据 idle kind 的 arousal_behavior 调整衰减速率
        self.coord.arousal.apply_behavior(kind.arousal_behavior());

        let event = IdleEvent {
            kind,
            depth: self.idle_depth,
            duration_secs: self.last_non_idle.elapsed().as_secs_f64(),
            context: Some(IdleContext {
                last_event_type: self.last_event_type.clone(),
                last_idle_outputs: self.last_idle_outputs.clone(),
                arousal_level: self.coord.arousal.current(),
                last_event_from_chat: self.last_event_from_chat,
            }),
        };

        self.idle_depth += 1;

        let mut event: Event = event.into();
        event.metadata.priority = Priority::Low;
        event.metadata.source = self.id().into();
        Ok(vec![event])
    }

    /// 根据 last_event_from_chat 选择有效人格
    fn effective_personality(&self) -> &IdlePersonality {
        if self.last_event_from_chat {
            if let Some(ref chat) = self.personality.chat_mode {
                let elapsed = self.last_non_idle.elapsed().as_secs_f64();
                if elapsed < chat.grace_period_secs {
                    return chat.as_personality();
                }
            }
        }
        &self.personality
    }
}
```

### 5.4 Incubation 后台线程生命周期

```rust
/// Incubation 后台线程——通过 CancellationToken 管理生命周期。
/// shutdown/热重载时发送取消信号，线程在下一个检查点退出。
struct IncubationHandle {
    cancel_token: CancellationToken,
    join_handle: JoinHandle<()>,
}

impl IncubationHandle {
    /// 启动后台孵化线程
    fn spawn(association_graph: Arc<AssociationGraph>, cancel_token: CancellationToken) -> Self {
        let token = cancel_token.clone();
        let join_handle = tokio::spawn(async move {
            loop {
                // 分段检查取消信号
                if token.is_cancelled() {
                    break;
                }
                // 执行一轮关联匹配
                association_graph.tick().await;
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
            // 退出前保存关联状态
            association_graph.checkpoint().await;
        });
        Self { cancel_token, join_handle }
    }

    /// 优雅关闭：发送取消 + 等待线程退出（timeout=5s）
    async fn shutdown(self) {
        self.cancel_token.cancel();
        tokio::time::timeout(Duration::from_secs(5), self.join_handle).await.ok();
    }
}

/// 全局 Incubation 管理器——限制最大并发线程数（默认 1）
struct IncubationManager {
    max_concurrent: usize,          // 默认 1
    active_handles: Vec<IncubationHandle>,
}
```

---

## 6. Event Flow

### 6.1 QueueDrained 和 IdleEvent 的 Dispatcher 路由

```yaml
routes:
  - match:
      event_type: "system.queue_drained"
    target: "pipeline:reflection"

  - match:
      event_type: "idle.daze"
    target: "pipeline:idle-daze"

  - match:
      event_type: "idle.boredom"
    target: "pipeline:idle-boredom"

  - match:
      event_type: "idle.sleep"
    target: "workflow:idle-sleep"

  - match:
      event_type: "idle.exploration"
    target: "workflow:idle-exploration"

  - match:
      event_type: "idle.meditation"
    target: "workflow:idle-meditation"

  - match:
      event_type: "idle.waiting"
    target: "pipeline:idle-waiting"

  - match:
      event_type: "idle.incubation"
    target: "pipeline:idle-incubation"
```

### 6.2 各空闲状态的 Pipeline / Workflow

| 状态 | 触发者 | 处理方式 | 备注 |
|------|--------|---------|------|
| Reflection | Dispatcher (QueueDrained) | Pipeline + select! + 熔断 | 可被新事件抢先取消 |
| Daze | IdleDetector | Pipeline（空） | 仅记录 metrics |
| Boredom | IdleDetector | Pipeline（无状态） | 聊天模式下的最深层状态 |
| Sleep | IdleDetector | Workflow（有状态） | 聊天模式下禁用 |
| Exploration | IdleDetector | Workflow（有状态） | 聊天模式下禁用；API 配额按分钟窗口 |
| Meditation | IdleDetector | Workflow（有状态） | 报告写入用 temp+rename |
| Waiting | IdleDetector | Pipeline | |
| Incubation | IdleDetector | Pipeline + 后台 CancellationToken | 线程数上限=1 |

### 6.3 聊天场景适配策略

聊天场景是 Aman 最常见的使用场景，也是 idle 系统最容易出错的地方。核心原则：

> 对话轮次之间的空闲应该是"轻量且无副作用"的。用户随时可能继续对话。

具体策略：

1. **事件源检测** — Dispatcher 在处理事件时标记 `last_event_from_chat`。Chat Source 产生的用户消息事件带有源类型标记。
2. **人格切换** — 聊天模式下，IdleDetector 使用 chat_mode 限制：仅允许 Daze 和 Boredom。
3. **宽限期** — chat_mode.grace_period_secs（默认 60s）。60s 内无新聊天事件→退出聊天模式→恢复完整空闲人格。
4. **上下文隔离** — 空闲期间的操作（Boredom 的随机浏览等）不自动写入对话历史。用户消息到达时，agent 切换到"对话上下文"，之前空闲期间的输出被挂起（不影响当前回复的上下文窗口）。
5. **"正在输入"信号** — 如果 Chat Source 支持 typing indicator（如 Telegram），可以作为提前退出空闲的触发器。不在本次设计范围，作为 Chat Source 的未来扩展。

---

## 7. Crate Assignment

### 7.1 新增 crate: `idle`

```
crates/idle/
├── Cargo.toml
├── src/
│   ├── lib.rs               # 公开 API
│   ├── types.rs              # IdleKind, IdleEvent, QueueDrained, IdlePersonality,
│   │                         #   IdleCoordination, IdleContext, ChatMode,
│   │                         #   ReflectionBreaker, PollRelaxation
│   ├── detector.rs           # IdleDetector: EventSource 实现
│   ├── personality.rs        # 人格解析：深度→类型映射 + 聊天模式切换
│   ├── arousal.rs            # ArousalTracker: arousal decay + Engaged/Passive 行为
│   ├── incubation.rs         # IncubationManager: CancellationToken + 线程管理
│   └── config.rs             # 配置验证
```

### 7.2 依赖关系

```
idle
  ├── core (Event, Priority, EventKind, Source trait)
  ├── event-bus (pending_count)
  ├── config (配置层)
  ├── persistence (WAL checkpoint for Sleep/Exploration 进度)
  └── tokio-util (CancellationToken)
```

### 7.3 修改的现有 crate

| Crate | 变更 |
|-------|------|
| `core` | 新增 `EventKind::Idle(IdleKind)` 变体；新增 `EventKind::QueueDrained` 变体 |
| `dispatcher` | 新增 QueueDrained 生产逻辑 + select! 模式 + `recently_processed_real_event` + 熔断计数 |
| `event-bus` | 新增 `wait_for_event()` 方法（异步通知新事件到达，供 select! 使用） |
| `source` | 无需修改 trait；IdleDetector 作为新的 Source 实现 |
| `config` | 新增 `IdleConfig` section |
| `runtime` | Phase 4 注册 IdleDetector + IdleCoordination 初始化；Phase 4.5 关停 + Incubation 线程清理 |

---

## 8. Configuration Surface

```yaml
# agent.yaml — idle section (R1 审计后版本)
idle:
  enabled: true

  # Reflection 复盘配置（由 QueueDrained 触发）
  reflection:
    enabled: true
    timeout_secs: 60
    check_items:
      - chain_tasks
      - immediate_errors
      - lessons_learned

  # 空闲人格
  personality:
    enabled_kinds: [daze, boredom, sleep, exploration, meditation]
    depth_schedule:
      - [1, boredom]
      - [3, sleep]
      - [5, exploration]

    poll_interval:
      linear: { base: 5.0, multiplier: 2.0 }

    # Poll 松弛：仅调整 poll 频率，不改变 idle kind
    poll_relaxation:
      depth_threshold: 15
      interval_secs: 60

    # 聊天模式
    chat_mode:
      allowed_kinds: [daze, boredom]
      grace_period_secs: 60
      poll_interval:
        fixed: 2.0

    # Reflection 熔断
    reflection_breaker:
      max_consecutive: 5
      cooldown_secs: 30
      escalate_on_double: true

    # 上下文隔离
    context_isolation:
      pollute_chat_history: false
      suspend_on_user_input: true

  # Arousal（由 IdleKind.arousal_behavior() 控制衰减速率）
  arousal:
    initial_value: 1.0
    half_life_secs: 900
    boredom_threshold: 0.3

  # Sleep 参数
  sleep:
    short_term_retention_days: 7
    cache_expiry_days: 30
    max_cpu_seconds: 60

  # Exploration 参数
  exploration:
    curiosity_sources: [memory_gaps, skill_audit, recent_failures]
    max_results: 20
    api_rate_per_minute: 10       # 明确的时间窗口：每分钟 10 次
    on_quota_exhausted: fallback   # fallback → 降级到 Boredom；notify → 通知 operator

  # Meditation 参数
  meditation:
    min_interval_ticks: 20
    report_path: "~/.aman/narrative/meditation/"
    atomic_write: true             # 使用临时文件 + rename

  # Incubation 参数
  incubation:
    max_concurrent_threads: 1
    cancel_timeout_secs: 5

  # IdleContext 缓冲
  context:
    max_output_buffer: 5           # last_idle_outputs 的最大条目数
```

---

## 9. Lifecycle Integration

### 9.1 启动

```
Phase 0 [基础设施]:
  - IdleCoordination 初始化（busy_reflecting, arousal_tracker）

Phase 2 [组件注册]:
  - Dispatcher 路由注入（含 QueueDrained → Reflection 路由）

Phase 4 [源激活]:
  - IdleDetector 注册为 Event Source
```

### 9.2 关闭

```
Phase 4.5 [源停止]:
  1. IdleDetector 停止
  2. IncubationManager.shutdown_all() → 发送 CancellationToken → 等待线程退出（5s 超时）
  3. 其他 Source 停止

Phase 4.5 [排水]:
  - 正在执行的 Reflection 被 select! 取消（busy_reflecting = false）
  - 正在执行的 Sleep/Exploration/Meditation 按已有排水机制处理
```

### 9.3 防循环机制总结

```
层级 1: recently_processed_real_event — QueueDrained 不算"真实事件"
层级 2: reflection_consecutive_count — 连续 5 次后熔断
层级 3: escalate_on_double — 10 次后跳过所有 Reflection，进入 cooldown
层级 4: BusyReflecting — Reflection 期间 IdleDetector 不产 IdleEvent
层级 5: select! — Reflection 可被新事件抢先取消
```

---

## 10. Resources & Performance

| 状态 | CPU 预算 | 内存增量 | Arousal 行为 |
|------|---------|---------|-------------|
| Reflection | <5% | 0 | —（不在arousal系统内） |
| Daze | <0.1% | 0 | Passive（正常衰减） |
| Boredom | <2% | 0 | Passive（正常衰减） |
| Sleep | <10% | +临时索引 | Engaged（×0.5） |
| Exploration | <30% | +搜索结果 | Engaged（暂停） |
| Meditation | <15% | +内省状态 | Engaged（暂停） |
| Waiting | <0.1% | 0 | Passive（正常衰减） |
| Incubation | <5%（后台） | +关联图 | Engaged（×0.1） |

---

## 11. Known Risks & Mitigations (R1 Updated)

| 风险 | 严重性 | 应对策略 |
|------|--------|---------|
| **IdleDetector 在 Reflection 期间产生 IdleEvent**（P0） | ~~高~~ **已修复** | `BusyReflecting` 标志阻断；Reflection 期间 IdleDetector 跳过 poll |
| **Reflection 阻塞真实事件**（P1#2） | ~~高~~ **已修复** | select! 模式——新事件到达抢先取消 Reflection |
| **聊天场景空闲误触发**（P1#3） | ~~高~~ **已缓解** | ChatMode 限制 + grace_period + 上下文隔离 |
| **Reflection 连锁任务无限循环**（P2#7） | ~~中~~ **已缓解** | reflection_breaker: max_consecutive=5 + cooldown |
| **Exploration API 配额作用域模糊**（P2#4） | ~~中~~ **已修复** | `api_rate_per_minute: 10` + `on_quota_exhausted: fallback` |
| **poll_relaxation 语义混淆**（P2#5） | ~~中~~ **已修复** | 重命名为 `poll_relaxation`，明确不改变 idle kind |
| **Arousal 在活跃空闲中衰减**（P2#6） | ~~中~~ **已修复** | `IdleKind.arousal_behavior()` —— Engaged 类型暂停或减缓衰减 |
| **Incubation 线程泄漏**（P2#8） | ~~中~~ **已修复** | CancellationToken + shutdown 清理 + max_concurrent=1 |
| **多轮对话空闲上下文污染**（P2#9） | ~~中~~ **已缓解** | `context_isolation: { pollute_chat_history: false, suspend_on_user_input: true }` |
| **IdleContext.last_idle_output 单槽丢失**（P3#10） | **已修复** | 改为 `last_idle_outputs: Vec<String>`，环形缓冲 max_output_buffer=5 |
| **Meditation 中断残留文件**（P3#11） | **已修复** | 临时文件 + 原子 rename |
| **空闲深度无限增长** | 低 | `poll_relaxation` 限制 poll 频率 + health check 监控 |
| **Sleep 在 shutdown 时丢失进度** | 低 | WAL checkpoint |
| **Meditation 产出错误认知** | 中 | 产出带 `confidence` 分数，低分不自动写入 memory |

---

## 12. Migration from Existing Arousal Model

现有的 `agent-boredom-narrative-event-driven.md` 中定义了 arousal decay 模型。本设计保留了 arousal 作为 IdleContext 字段，但增强了衰减语义。

| 旧模型 | 新模型 | 迁移方式 |
|--------|--------|---------|
| `arousal_level < 阈值 → 无聊` | `depth_schedule` 决定空闲类型 | arousal 降级为 context 字段 |
| 三态（忙/刚完成/空闲） | Reflection + 八态空闲 | 刚完成 → Reflection；空闲 → Daze → 深度序列 |
| 统一衰减速率 | Engaged/Passive 分速率 | IdleKind.arousal_behavior() |
| 硬编码 5min/30min 间隔 | 配置化 `poll_interval` + `poll_relaxation` | 迁移配置 |
| 无聊天感知 | ChatMode | 新增，向后兼容（chat_mode 为 None 时行为不变） |

---

## 13. Metrics & Observability

```rust
struct IdleMetrics {
    idle_depth: u32,
    idle_kind: IdleKind,
    total_idle_seconds: f64,
    kind_durations: HashMap<IdleKind, f64>,
    reflections_completed: u64,
    reflections_preempted: u64,          // 被新事件抢先取消的 Reflection 次数
    reflections_timeout: u64,
    reflections_produced_events: u64,
    reflections_breaker_activated: u64,   // 熔断触发次数
    chat_mode_active_seconds: f64,        // 聊天模式累积时长
    memories_consolidated: u64,
    explorations_completed: u64,
    explorations_quota_exhausted: u64,    // API 配额耗尽次数
    meditations_completed: u64,
    incubation_threads_spawned: u64,
    incubation_threads_cancelled: u64,
}
```

---

## 14. Open Questions

1. **Reflection 产出的连锁任务如何避免重复？** 建议产出带 `dedup_key`，Event Bus 的去重机制覆盖。
2. **Sleep 产出的长期记忆存哪里？** 建议独立 `memory` crate（未来设计）。
3. **Incubation 的"灵感"机制？** Phase 1 实现跳过，待 Sleep/Exploration 稳定后再设计。
4. **"正在输入"信号集成？** Chat Source 的未来扩展，需要各平台（Telegram/Discord/Slack）的 typing indicator 支持。当 typing 开始时设置 `user_active` 标志，抑制 IdleDetector poll。
5. **多 Agent 的空闲互相影响？** 不在本次设计范围。

---

> **这份设计能承载几轮业务迭代而不需要重写？**
>
> - 新增空闲类型：`IdleKind` enum 加变体 + `depth_schedule` 加映射 + 一个 Pipeline/Workflow。改动 3 处。
> - 修改 Reflection 超时：改 `reflection.timeout_secs`。0 处代码改动。
> - 新增 Reflection 检查项：改 `reflection.check_items` + Pipeline step。1 处改动。
> - 调整聊天模式策略：改 `chat_mode` yaml。0 处代码改动。
> - 替换 Arousal 模型：`idle::arousal` 模块内部重构，不影响外部接口。
> - 新增 Chat Source 平台：ChatMode 自动适配（只要 Source 标记了 chat 类型）。
>
> **R1 审计变更量**：11 项发现全部处理。类型系统新增 3 个 struct（IdleCoordination, ReflectionBreaker, ChatMode）、1 个 enum（ArousalBehavior）、1 个方法（IdleKind.arousal_behavior）。Dispatcher 重构为 select! 模式。Event Bus 新增 wait_for_event()。配置新增 8 个字段。
