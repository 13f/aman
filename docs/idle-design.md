# Idle State System — Architecture Design

> 将 Windows "空闲进程"的隐喻落地为 aman Agent 框架的正式子系统。
> 空闲不是无事可做，而是 Agent 在内省、维护、探索、复盘——用未被使用的周期做有价值的事。
>
> **八种空闲状态**：七种由 AgentIdleManager 根据空闲深度产生，一种（Reflection）由
> 两种事件触发：(1) Dispatcher 队列清空时的 QueueDrained，或 (2) AgentIdleManager
> 冷启动（启动后 3–5s 内队列持续为空时的合成 QueueDrained）。
>
> **Per-Agent 架构（R8）**：每个 Agent 拥有独立的 idle 系统——自己的 IdleCoordination、
> IdleDetector（通过 AgentIdleManager）、IncubationManager。Idle 事件发布到 Agent 的
> Local EventBus，与全局 Bus 隔离。
>
> **审计状态**：
> - R1–R6：类型系统、select!/ChatMode/熔断/配额/arousal/线程/隔离 —— 全部修复，设计成熟度 ★★★★★
> - R8（per-agent-idle）：全局 IdleDetector+SourceRegistry 模式替换为 per-agent AgentIdleManager。
>   每个 Agent 的 idle 系统只监控该 Agent 的 Local EventBus，实现 Agent 间 idle 隔离。

---

## 1. Problem Statement

### 要解决的问题

aman 是事件响应式框架——一切行为由事件驱动。但事件队列为空时，Agent 陷入"真空"：什么都不做，也什么都没学到。这不是设计缺陷，而是**设计空白**。

更重要的是：事件刚处理完、队列刚清空的那一刻，Agent 的 arousal 还高、上下文还热——应该立刻复盘刚完成的任务、检查是否有连锁任务。这是 Dispatcher 的责任，不是 IdleDetector 的责任。

类比：Windows 内核中 CPU 永远不会"什么也不做"。当没有用户进程需要调度时，系统切换到 `System Idle Process`（PID 0）——一个专门捕获空闲周期的特殊进程。aman 需要同等级别的"空闲进程"。

### 核心约束

| 约束类型 | 内容 |
|---------|------|
| **不可变（框架哲学）** | 一切行为仍是事件驱动。空闲不引入新的执行模型，只是产生新类型的事件 |
| **可变（业务策略）** | 哪些空闲类型启用、阈值、每个 Agent 的"空闲人格"；聊天场景 vs 系统场景 |
| **技术限制** | 空闲检测必须在事件循环内部，不能依赖外部 cron |
| **性能约束** | 空闲检测本身不能成为 CPU 热点。在空闲状态下，检测逻辑本身消耗应 <1% CPU |
| **时序约束** | Reflection 必须在事件处理完成后、真正空闲开始前执行；Reflection 可被真实事件打断 |
| **安全约束** | 空闲 Workflow（Sleep/Exploration/Meditation）运行时，真实事件到达后必须能中断它们——避免后台状态污染 |
| **聊天场景约束** | 对话轮次之间不应触发完整空闲序列——用户随时可能继续对话。聊天→完整人格切换时重置 idle depth |

---

## 2. Design Philosophy

```
空闲不是"没有事件"，而是一类特殊的事件。
它们定义了 Agent 在没有外部输入时如何与自己相处。
```

六条设计原则：

1. **IdleEvent 是 Event 的合法子类型** — 空闲事件通过 Event Bus 路由，与其他事件一视同仁。
2. **QueueDrained 是空闲入口** — Dispatcher 在队列清空时产生 QueueDrained，触发 Reflection。Reflection 可被新事件 select! 抢先。
3. **空闲深度决定空闲类型** — Reflection 完成后进入空闲序列：Daze → Boredom → Sleep → deeper。
4. **Per-Agent 中断令牌** — 每个 Agent 的 IdleCoordination 持有独立的 `CancellationToken`。真实事件到达时 AgentHarness 调用 `coord.reset_idle_signal()` 取消令牌，Workflow 在下一个 checkpoint 优雅退出。
5. **配置驱动 + 源类型感知** — IdleCoordination 传递 `last_source_type`，闲聊场景自适应切换 ChatMode。
6. **上下文隔离** — 空闲操作不污染对话历史。实现位置：Pipeline 层的 ContextBuilder 根据 `IdleEvent.source` 标记隔离。

---

## 3. Type System

### 3.1 IdleKind — 七种深度驱动空闲子类型

```rust
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
    Daze,         // Passive
    Boredom,      // Passive
    Sleep,        // Engaged { decay_multiplier: 0.5 }
    Exploration,  // Engaged { decay_multiplier: 0.0 }
    Meditation,   // Engaged { decay_multiplier: 0.0 }
    Waiting,      // Passive
    Incubation,   // Engaged { decay_multiplier: 0.1 }
}

#[derive(Debug, Clone, Copy)]
pub enum ArousalBehavior {
    Passive,
    Engaged { decay_multiplier: f64 },
}

impl IdleKind {
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
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IdleEvent {
    pub kind: IdleKind,
    pub depth: u32,
    pub duration_secs: f64,
    pub context: Option<IdleContext>,

    /// R3-2: 此事件是否在聊天模式下产生。
    /// Pipeline/Workflow 层通过此字段决定行为差异
    /// （如聊天模式 Boredom 为纯 no-op，完整模式 Boredom 执行随机浏览）。
    pub from_chat_mode: bool,
}

/// R4-2: IdleEvent 的序列化约束。
/// IdleEvent（priority=Low）在 Event Bus 背压 overflow 时应丢弃，不持久化。
/// 原因：IdleEvent 携带的 `from_chat_mode` 等上下文标记在 Agent 重启后的新会话中
/// 已失去意义（无活跃聊天上下文）。持久化后恢复的 IdleEvent 可能造成行为偏差。
/// 实现：Event Bus 的 overflow_to_disk 在检查事件优先级的 drop 规则时，
/// LOW priority 事件在注入溢出缓冲区前即丢弃并记录日志。

/// 空闲上下文——累积最近 N 轮空闲的产出。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IdleContext {
    pub last_event_type: String,
    /// 最近 N 轮空闲产出的摘要（定容 Vec，新产出 push + 旧产出 pop_front）
    pub last_idle_outputs: Vec<String>,
    pub arousal_level: f64,
}
```

### 3.3 QueueDrained — Dispatcher 产生的队列清空事件

```rust
/// 由 Dispatcher 在以下条件同时满足时产生：
/// 1. 刚处理完一个真实事件
/// 2. Event Bus 队列已空
/// 3. Reflection 熔断未激活
///
/// Reflection 在 Dispatcher 中通过 select! 执行——新事件可抢先取消。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueueDrained {
    pub last_event_type: String,
    pub last_trace_id: String,
    pub last_result_summary: Option<String>,
    pub arousal_level: f64,

    /// 连续 Reflection 的触发次数（用于熔断检测）。
    /// 注意：被抢先取消时重置为 0（没有产出不算连续）。
    pub reflection_consecutive_count: u32,
}

impl EventKind {
    pub const QUEUE_DRAINED: &'static str = "system.queue_drained";
}
```

### 3.4 IdlePersonality — 每个 Agent 的空闲人格

双轴模型：**depth 决定范围，arousal 决定选择**。

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IdlePersonality {
    pub enabled_kinds: Vec<IdleKind>,
    /// 深度 → 空闲类型映射（按深度升序）
    pub depth_schedule: Vec<(u32, IdleKind)>,
    pub poll_interval: PollInterval,
    pub poll_relaxation: PollRelaxation,
    pub chat_mode: ChatMode,
    pub reflection_breaker: ReflectionBreaker,
    pub context_isolation: ContextIsolation,
    /// Boredom 随机行动配置（R9）— 加权随机选择技能执行。
    /// 当 work_pressure 配置时，work tag 权重根据队列深度动态调整。
    pub boredom: Option<BoredomConfig>,
}

impl IdlePersonality {
    /// 阈值匹配：找到 schedule 中 d <= depth 的最大 d 对应的 kind。
    pub fn resolve(&self, depth: u32) -> IdleKind { ... }

    /// 双轴精调：depth 确定最大可到达的 kind，arousal 在已解锁范围内选择：
    ///
    /// - arousal 高（> 0.6）→ 浅层活跃状态（Daze, Boredom）
    /// - arousal 中（0.2–0.6）→ 中层状态（Sleep, Exploration）
    /// - arousal 低（< 0.2）→ 深层状态（Meditation, Incubation）
    ///
    /// 形成自然反馈循环：浅层状态（Passive 衰减）使 arousal 下降 →
    /// 降到阈值后自动滑入深层 → 深层状态（Engaged, multiplier≈0）
    /// 维持低 arousal → agent 在深层状态自然停留更久。
    pub fn resolve_with_arousal(&self, depth: u32, arousal: f64) -> IdleKind { ... }
}
```

### 3.5 IdleCoordination — 跨组件共享状态

```rust
/// IdleDetector、Dispatcher、Idle Workflow 三者之间的共享协调状态。
///
/// 新增字段（R2）：
/// - last_source_type：Dispatcher 写入，IdleDetector 读取，解决 ChatMode 传播断裂
/// - idle_cancel_token：Dispatcher 取消，Idle Workflow 监控，解决后台污染问题
#[derive(Clone)]
pub struct IdleCoordination {
    /// Dispatcher 设置：Reflection 执行中 → true
    pub busy_reflecting: Arc<AtomicBool>,

    /// 共享的 ArousalTracker
    pub arousal: Arc<ArousalTracker>,

    /// R2-1 fix: 最近一次被 Dispatch 的事件的 SourceType。
    /// Dispatcher 写入（store），IdleDetector 读取（load）。
    /// IdleDetector 据此判断是否激活 ChatMode。
    pub last_source_type: Arc<AtomicU8>,  // SourceType 的 u8 编码

    /// R2-2 fix: Per-Agent 空闲取消令牌。
    /// 真实事件到达时 Dispatcher 调用 cancel() + 替换为新 token。
    /// Sleep/Exploration/Meditation Workflow 在每个 checkpoint 检查此 token。
    pub idle_cancel_token: Arc<RwLock<CancellationToken>>,

    /// R7: pending_depth_reset — depth 重置不由 reset_idle_signal 触发，
    /// 而是由 Dispatcher 在产生 QueueDrained 时单独设置（signal_queue_drained）。
    /// 避免 idle poll 在事件仍在处理中时提前消费 depth 重置信号。
    pub pending_depth_reset: Arc<AtomicBool>,
}

impl IdleCoordination {
    pub fn new() -> Self {
        Self {
            busy_reflecting: Arc::new(AtomicBool::new(false)),
            arousal: Arc::new(ArousalTracker::default()),
            last_source_type: Arc::new(AtomicU8::new(SourceType::Unknown as u8)),
            idle_cancel_token: Arc::new(RwLock::new(CancellationToken::new())),
            pending_depth_reset: Arc::new(AtomicBool::new(false)),
        }
    }

    /// 取消运行中的空闲 Workflow。
    /// 注意：depth 重置不由这里触发，而是在 Dispatcher 产生 QueueDrained 时
    /// 通过 signal_queue_drained() 设置。
    pub fn reset_idle_signal(&self) {
        let mut token = self.idle_cancel_token.write().unwrap();
        token.cancel();
        *token = CancellationToken::new();
    }

    /// 标记 depth 需要在下次 idle poll 时重置。
    /// Dispatcher 在队列清空（产生 QueueDrained）时调用。
    pub fn signal_queue_drained(&self) {
        self.pending_depth_reset.store(true, Ordering::SeqCst);
    }
}
```

### 3.6 BoredomConfig — Boredom 状态下的随机行动配置（R9）

当 Agent 连续 `trigger_poll` 次处于 Boredom 状态时，`BoredomActor` 按加权随机选择一个
activity tag，然后从 SkillRegistry 中筛选同时带有该 tag 和 `idle_run` 标记的技能执行。

当 `work_pressure` 配置时，目标 tag（通常为 "work"）的权重会根据当前队列深度动态调整——
积压越多，agent 在空闲时越倾向于选 work 技能，形成自然的背压闭环。

```rust
/// Boredom 随机行动配置。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BoredomConfig {
    /// 触发 boredom 行动所需的连续 Boredom poll 次数（1-indexed）。
    pub trigger_poll: u32,
    /// 活动类别及其相对权重（内部归一化，无需总和为 1.0）。
    pub activities: Vec<BoredomActivity>,
    /// 可选：根据工作队列深度动态调整某 tag 的权重。
    pub work_pressure: Option<WorkPressureConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BoredomActivity {
    /// 用于匹配技能（含 idle_run 标记）的 tag。"idle" 是特殊哨兵——不做任何事。
    pub tag: String,
    pub weight: f64,
}

/// 工作压力配置——基于队列深度的动态权重调整。
pub struct WorkPressureConfig {
    /// 要施加压力的 tag（如 "work"）。
    pub target_tag: String,
    /// 队列深度 → 权重乘数的映射函数。
    pub mapping: PressureMapping,
}

pub enum PressureMapping {
    /// Linear: multiplier = clamp(1.0 + slope × depth, 1.0, max)
    /// 例：slope=0.3, max=10 → depth=0 时 multiplier=1.0,
    ///     depth=10 → 4.0, depth=30 → 10.0(capped)
    Linear { slope: f64, max_multiplier: f64 },
    /// Sigmoid: 在 midpoint 附近平滑过渡
    /// multiplier = 1.0 + (max-1.0) / (1 + exp(-steepness×(depth-midpoint)))
    Sigmoid { midpoint: f64, steepness: f64, max_multiplier: f64 },
}
```

**系统状态映射**（BoredomActor 选中的 tag → AgentSystemState）：

| tag | AgentSystemState |
|-----|-----------------|
| `"work"` | `Working` |
| `"study"` | `Studying` |
| `"internet"` \| `"entertainment"` \| `"fun"` | `DailyLife` |
| 其他 / `"idle"` | `Idle` |

---

## 4. State Machine

### 4.1 完整状态转移

```
                        ┌──────────────────────────────────────────────────────┐
                        │      (任何真实事件到达 → reset_idle_signal → Active) │
                        │      (同时 arousal.boost(0.3) → 提升 engagement)   │
                        │      (Reflection 运行中 → select! 抢先 → 取消 Reflection) │
                        │                                                      │
                        ▼                                                      │
    ┌────────┐  queue drained  ┌────────────┐                                  │
    │ Active │────────────────▶│ Reflection │ (不计深度，可被 select! 抢先)        │
    └────────┘   (QueueDrained)└──────┬─────┘                                  │
         ▲                  │         │                                        │
         │    真实事件抢先    │  无连锁  │  有连锁任务                              │
         │    取消Reflection │  任务    │  产生新事件                              │
         │         │         │         │                                        │
         │         │         ▼         │                                        │
         │         │    ┌──────────────────────────────────────┐                │
         │         │    │      空闲人格选择（自适应）              │                │
         │         │    │  coord.last_source_type == Chat?     │                │
         │         │    │    ├─ true & elapsed < grace → Chat  │                │
         │         │    │    └─ false → 完整 depth_schedule     │                │
         │         │    │  (从 ChatMode 退出时 depth 重置为 0)   │                │
         │         │    └──────────────┬───────────────────────┘                │
         │         │                   │                                        │
         │         │                   ▼                                        │
         │         │           ┌──────────┐  depth=1  ┌──────────┐             │
         │         │           │   DAZE   │──────────▶│ BOREDOM  │             │
         │         │           └──────────┘           └──────────┘             │
         │         │            depth=0                  │                      │
         │         │                              (chat → 到此为止)              │
         │         │                              Boredom 为纯 no-op             │
         │         │                                                    │       │
         │         │              (完整 → BoredomActor 加权随机挑技能)     │       │
         │         │              含 work_pressure 动态权重调整    depth=3│       │
         │         │                                                    ▼       │
         │         │                                              ┌──────────┐  │
         │         │                                              │  SLEEP   │  │
         │         │                                              └──────────┘  │
         │         │                                             depth=5   │    │
         │         │                     ┌──────────────────────────────────┘    │
         │         │                     ▼                                       │
         │         │     ┌──────────┐  ┌──────────┐  ┌──────────┐              │
         │         │     │EXPLORATION│  │MEDITATION│  │INCUBATION│              │
         │         │     └──────────┘  └──────────┘  └──────────┘              │
         │         │          │              │              │                   │
         │         │   (所有 Workflow 监控 coord.idle_cancel_token)              │
         │         └──────────────────────────────────────────────────────────┘
```

**规则**：

1. **QueueDrained 由两个来源产生** —
   (a) **正常路径**: AgentIdleManager 检测到 busy→empty 转换（Dispatcher 刚处理完事件）。
   (b) **冷启动路径**: Agent 启动后事件队列持续为空超过 3–5s——AgentIdleManager 产生合成 QueueDrained，
   确保 Reflection 在进入 idle 深度序列前至少执行一次。两种路径共享同一熔断器。
2. **Reflection 可被 select! 抢先** — 新事件到达 → cancel Reflection → 处理新事件。抢先时熔断计数重置。
3. **Reflection 有 timeout 和熔断** — timeout=60s；连续 5 次→跳过 lessons_learned；10 次→完全跳过+cooldown。
4. **Per-Agent Cancel Token** — 真实事件到达时 AgentHarness 通过 `coord.reset_idle_signal()` 取消令牌。该 Agent 的所有空闲 Workflow 监控此令牌并在 checkpoint 退出。
5. **Reflection 不占深度** — depth 从 Daze 开始（depth=0）。
6. **聊天模式切换 + depth 重置** — 从聊天模式退出到完整人格时，idle_depth 重置为 0。
7. **深度递增** — 连续空闲轮次驱动。空闲类型之间切换不重置深度。
8. **allowed_kinds ⊆ enabled_kinds** — 配置验证强制。resolve() fallback = Daze。

### 4.2 完整的事件处理→空闲→再唤醒流程

```
[事件 A 到达]
  │
  ├─ Dispatcher: 取出 A
  │   coord.last_source_type.store(A.source_type)  ← R2-1+R5-1: 仅外部事件写入源类型
  │   coord.reset_idle_signal()                ← R2-2: 取消正在运行的 idle Workflow
  │   dispatch(A).await
  │
  ├─ try_dequeue() → None, recently_processed_real_event == true
  │   → 熔断检查通过
  │   → busy_reflecting = true
  │   → 发布 QueueDrained
  │
  ├─ 取出 QueueDrained → select! {
  │       reflection_pipeline.run() => {
  │           // 完成或超时
  │           if has_output: 注入新事件, count 不清零
  │           else: count = 0
  │           busy_reflecting = false
  │       }
  │       _ = event_bus.wait_for_event() => {
  │           // ↑ 二次确认 pending_count()>0 防假唤醒 (R2-11)
  │           // R2-9: 先 abort 再清标志
  │           reflection.abort()
  │           busy_reflecting = false
  │           // R2-8: 被抢先 → 熔断计数重置（无产出不算连续）
  │           reflection_consecutive_count = 0
  │       }
  │   }
  │
  ├─ [真正空闲]
  │   检查 coord.last_source_type → ChatMode? 完整?
  │   完整模式: Daze → Boredom → Sleep → Exploration/Meditation
  │   聊天模式: Daze → Boredom(no-op) → grace_period 到期 → depth=0 → 完整模式
  │
  │   每个 idle Workflow 运行时监控 coord.idle_cancel_token
  │
  ├─ [事件 B 到达]
  │   coord.reset_idle_signal()  ← 中断所有 idle Workflow
  │   count 重置 → 回到 Active
```

### 4.3 深度→类型映射的默认配置

depth_schedule 使用渐宽阈值——越深入的 idle 状态，需要越长的累积空闲时间才能触及。

```yaml
idle:
  personality:
    enabled_kinds: [daze, boredom, sleep, exploration, meditation, incubation]
    depth_schedule:
      - [5, boredom]
      - [20, sleep]
      - [50, exploration]
      - [100, meditation]
      - [200, incubation]
    poll_interval:
      linear: { base: 5.0, multiplier: 2.0 }
    poll_relaxation:
      depth_threshold: 15
      interval_secs: 60

    chat_mode:
      allowed_kinds: [daze, boredom]     # 必须 ⊆ enabled_kinds
      grace_period_secs: 60
      poll_interval:
        linear: { base: 2.0, multiplier: 0.5 }  # 2s→3s→4s... 而非固定 2s

    reflection_breaker:
      max_consecutive: 5
      cooldown_secs: 30
      escalate_on_double: true

    context_isolation:
      pollute_chat_history: false
      suspend_on_user_input: true

  reflection:
    enabled: true
    timeout_secs: 60
```

对应实际时间（以 Fixed 5s/tick 计算）：

| depth | kind | 累积时间（约） | poll 次数 |
|-------|------|---------------|----------|
| 0–4 | Daze | 0–20s | 0–4 |
| 5–19 | Boredom | 25–95s | 5–19 |
| 20–49 | Sleep | 100–245s | 20–49 |
| 50–99 | Exploration | 250–495s | 50–99 |
| 100–199 | Meditation | 500–995s | 100–199 |
| 200+ | Incubation | 1000s+ | 200+ |

### 4.4 打断策略矩阵

| 状态 | 触发者 | 可被真实事件打断？ | 打断机制 | 打断损失 | 打断后行为 |
|------|--------|:----------------:|---------|---------|-----------|
| Reflection | Dispatcher | **是** | select! 抢先 | 无 | Reflection 取消，新事件立即处理 |
| Daze | IdleDetector | **否** | 同步 Pipeline 执行至完成（空 Pipeline，<1ms） | 无 | 下一个 poll 感知新事件 |
| Boredom | IdleDetector | **否** | 同步 Pipeline 执行至完成（聊天 no-op <1ms） | 无 | 下一个 poll 感知新事件 |
| Sleep | IdleDetector | **是** | **idle_cancel_token** — Workflow.run_with_cancel() 每步检查 | 中 | WAL checkpoint → 退出 |
| Exploration | IdleDetector | **是** | **idle_cancel_token** — Workflow.run_with_cancel() 每步检查 | 低 | 断点保存 → 退出 |
| Meditation | IdleDetector | **是** | **idle_cancel_token** — Workflow.run_with_cancel() 每步检查 | 高 | 丢弃，temp+rename 文件安全 |
| Waiting | IdleDetector | **否** | 同步 Pipeline 执行至完成（条件检查，极短） | 无 | 条件满足→Active |
| Incubation | IdleDetector | **否** | 独立 CT（仅 Phase 4.5 关闭。纯后台，不因真实事件中断） | 低 | 关联状态保存 → 线程退出 |

> **关键语义**：Pipeline 类型的空闲状态（Daze/Boredom/Waiting）通过 `dispatch(event).await` 同步执行。
> 在此期间 Dispatcher 阻塞在 `await` 上，无法取出 Event Bus 中的新事件。
> 新事件在队列中等待，直到 Pipeline 完成（Pipeline 为空或极短时 <1ms）。
> 这是 Pipeline 设计的固有特征，不是缺陷——Dispatcher 的 select! 仅用于 Reflection。

### 4.5 Reflection 熔断机制（含 Pipeline 指令传递）

```
QueueDrained 事件携带 reflection_consecutive_count 字段。
Reflection Pipeline 的第一个 step 读取此字段决定执行哪些 check_items：

  count < max_consecutive (5)     → 执行全部 check_items
  count >= 5, < 10                → 跳过 lessons_learned
  count >= 10                     → 跳过所有 check_items，直接进入 Daze
                                    + cooldown_secs 内禁止任何 Reflection

被 select! 抢先取消时 reflection_consecutive_count 重置为 0。
```

---

## 5. Integration with aman Runtime

### 5.1 架构位置（R8：Per-Agent）

```
┌──────────────────────────────────────────────────────────────────────┐
│                          Agent Runtime                                │
│                                                                       │
│  ┌──────────┐  ┌──────────┐  ┌──────────┐                            │
│  │  Timer   │  │ FileWatch│  │ Webhook  │                            │
│  │ Source   │  │ Source   │  │ Source   │                            │
│  └────┬─────┘  └────┬─────┘  └────┬─────┘                            │
│       └──────────────┴──────────────┘                                 │
│                      │                                                │
│           ┌──────────┼──────────┐                                     │
│           ▼          │          ▼                                     │
│     Dispatcher  ◄── 出队  │  ── 入队 ──▶ Dispatcher                   │
│           │          │                                               │
│           │  ┌───────▼──────────────────────────┐                    │
│           │  │       AgentRegistry               │                    │
│           │  │                                   │                    │
│           │  │  Per-Agent (×N):                  │                    │
│           │  │  ┌─────────────────────────────┐  │                    │
│           │  │  │ AgentIdleManager            │  │                    │
│           │  │  │ ├─ IdleCoordination         │  │                    │
│           │  │  │ │  busy_reflecting          │  │                    │
│           │  │  │ │  last_source_type  (R2)   │  │                    │
│           │  │  │ │  idle_cancel_token (R2)   │  │                    │
│           │  │  │ │  arousal_tracker          │  │                    │
│           │  │  │ │  pending_depth_reset (R7) │  │                    │
│           │  │  │ ├─ IdleDetector (内部)       │  │                    │
│           │  │  │ ├─ IncubationManager        │  │                    │
│           │  │  │ └─ background task           │  │                    │
│           │  │  │    监控 Local EventBus ──────┼──┼──► Agent Local Bus │
│           │  │  └─────────────────────────────┘  │                    │
│           │  └───────────────────────────────────┘                    │
│           │                                                           │
│  route: system.queue_drained → pipeline:reflection                    │
│  route: idle.*               → pipeline/workflow:idle-*               │
│           │         │          │                                      │
│  ┌────────┼─────────┼──────────┼────────┐                            │
│  ▼        ▼         │          ▼        ▼                            │
│ Reflection  Idle    │    Idle Workflow  │                            │
│ Pipeline    Pipeline│    (Sleep,Explor, │                            │
│ (select!)   (Daze,  │     Meditation)   │                            │
│             Boredom)│    监控 cancel    │                            │
│                     │    token          │                            │
│           ┌─────────▼────────┐          │                            │
│           │   Tool Runner    │          │                            │
│           └──────────────────┘          │                            │
└──────────────────────────────────────────────────────────────────────┘
```

> **R8 关键变更**：IdleDetector 不再作为全局 EventSource 注册到 SourceRegistry。
> 改为每个 Agent 拥有独立的 `AgentIdleManager`（存储在 `AgentRegistry` 中），
> 其后台 tokio task 监控该 Agent 的 Local EventBus 队列深度，idle 事件发布到
> Local EventBus。Agent 间 idle 完全隔离。

### 5.2 Dispatcher 主循环（R2 重写；R8：per-agent 上下文通过 AgentHarness 注入）

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

                        // R2-1 + R5-1: 仅外部事件写入源类型。
                        // 内部产出的连锁任务（如 Reflection 的 lessons_learned）
                        // 不覆盖 last_source_type——防止 ChatMode 被静默停用。
                        if event.is_from_external_source() {
                            coord.last_source_type.store(
                                event.source_type().to_u8(), Ordering::Relaxed
                            );
                        }

                        // R2-2: 取消所有正在运行的 idle Workflow
                        coord.reset_idle_signal();

                        // R7: 真实事件提升 arousal
                        coord.arousal.boost(0.3);

                        self.dispatch(event).await;
                    } else if event.is_queue_drained() {
                        coord.busy_reflecting.store(true, Ordering::SeqCst);
                        let reflection = self.resolve_pipeline("pipeline:reflection");

                        select! {
                            result = reflection.execute(&event) => {
                                coord.busy_reflecting.store(false, Ordering::SeqCst);
                                if result.has_output() {
                                    for new_event in result.output_events() {
                                        self.event_bus.publish(new_event).await;
                                    }
                                } else {
                                    reflection_consecutive_count = 0;
                                }
                            }
                            _ = self.event_bus.wait_for_event() => {
                                // R2-11: 二次确认防止假唤醒
                                if self.event_bus.pending_count() == 0 {
                                    continue;  // 假唤醒，重新进入 select!
                                }
                                // R2-9: 先 abort 再清标志
                                reflection.abort();
                                coord.busy_reflecting.store(false, Ordering::SeqCst);
                                // R2-8: 被抢先 → 重置熔断计数
                                reflection_consecutive_count = 0;
                            }
                        }
                    } else {
                        // IdleEvent → 正常 dispatch
                        self.dispatch(event).await;
                    }
                }
                None => {
                    if recently_processed_real_event {
                        recently_processed_real_event = false;
                        let breaker = &self.idle_config.personality.reflection_breaker;

                        if reflection_consecutive_count >= breaker.max_consecutive * 2 {
                            reflection_consecutive_count = 0;
                            tokio::time::sleep(
                                Duration::from_secs(breaker.cooldown_secs)
                            ).await;
                            continue;
                        }

                        // R7: 队列清空 → 标记 depth 重置（不再依赖 real_event_seen 的 race-prone 机制）
                        coord.signal_queue_drained();

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
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            }
        }
    }
}
```

### 5.3 AgentIdleManager — Per-Agent 后台空闲循环（R8：替代 EventSource 模式）

R8 用 `AgentIdleManager` 替代了原先注册到 SourceRegistry 的全局 `IdleDetector`。
每个 Agent 拥有独立的 Manager 实例，其后台 tokio task 直接监控该 Agent 的 Local EventBus，
不再依赖 SourceRegistry 的 poll 循环。

```rust
/// AgentIdleManager：per-agent 空闲生命周期管理。
///
/// 内部持有：
/// - IdleCoordination（共享状态）
/// - IdleDetector（空闲状态机，pub(crate) 字段供后台 task 读写）
/// - IncubationManager（后台灵感线程）
/// - CancellationToken（shutdown 信号）
pub struct AgentIdleManager {
    agent_id: String,
    coord: Arc<IdleCoordination>,
    personality: IdlePersonality,
    local_bus: Arc<dyn EventBus>,
    incubation: Arc<IncubationManager>,
    stop_token: CancellationToken,
    task: tokio::sync::Mutex<Option<JoinHandle<()>>>,
}

impl AgentIdleManager {
    /// 创建实例（不启动后台循环）。
    pub fn new(
        agent_id: String,
        local_bus: Arc<dyn EventBus>,
        personality: IdlePersonality,
        arousal_initial: f64,
        arousal_half_life: f64,
    ) -> Self { /* ... */ }

    /// 启动后台 tokio task（在 Phase 4 调用）。
    pub async fn start(&self) { /* ... */ }

    /// 获取共享协调状态的引用（供 AgentHarness 使用）。
    pub fn coordination(&self) -> &Arc<IdleCoordination> { /* ... */ }

    /// 关闭：取消 incubation 线程 → reset idle signal → stop token。
    pub async fn shutdown(&self) -> amanResult<()> { /* ... */ }
}
```

**后台 task 主循环逻辑**（替代原 `EventSource::poll`）：

```rust
async fn idle_loop(
    agent_id: String,
    coord: Arc<IdleCoordination>,
    personality: IdlePersonality,
    local_bus: Arc<dyn EventBus>,
    incubation: Arc<IncubationManager>,
    stop_token: CancellationToken,
) {
    let mut detector = IdleDetector::new(personality, Arc::clone(&coord));

    // Busy→empty tracking for QueueDrained production.
    let mut was_busy = false;
    let mut reflection_count: u32 = 0;
    const BREAKER_THRESHOLD: u32 = 20;

    // Cold-start: produce QueueDrained if bus stays empty for this long
    // after startup with no prior QueueDrained.
    let mut cold_start_done = false;
    let mut cold_start_deadline: Option<Instant> = None;
    const COLD_START_DELAY_SECS: u64 = 5;

    loop {
        // 1. 检查 shutdown 信号
        if stop_token.is_cancelled() {
            break;
        }

        // 2. 检查 Dispatcher 是否正在执行 Reflection
        if coord.busy_reflecting.load(Ordering::SeqCst) {
            tokio::time::sleep(poll_interval).await;
            continue;
        }

        // 3. R7: 检查 depth 重置信号（Dispatcher 在 QueueDrained 时设置）
        if coord.pending_depth_reset.swap(false, Ordering::SeqCst) {
            detector.idle_depth = 0;
            detector.last_non_idle = Instant::now();
            tokio::time::sleep(poll_interval).await;
            continue;
        }

        // 4. 检查 Agent Local EventBus 队列深度
        let metrics = local_bus.metrics().await;
        if metrics.queue_depth > 0 {
            // 有待处理事件 → 重置深度，标记曾 busy
            was_busy = true;
            reflection_count = 0;
            detector.idle_depth = 0;
            detector.last_non_idle = Instant::now();
            tokio::time::sleep(poll_interval).await;
            continue;
        }

        // 5. Busy→empty transition → produce QueueDrained (subject to circuit breaker)
        if was_busy {
            was_busy = false;
            cold_start_done = true; // no longer need cold-start
            detector.idle_depth = 0;

            if reflection_count < BREAKER_THRESHOLD {
                let qd = QueueDrained {
                    reflection_consecutive_count: reflection_count,
                    agent_id: Some(agent_id.clone()),
                    ..
                };
                reflection_count += 1;
                let _ = local_bus.publish(qd.into()).await;
            }
            continue;
        }

        // 6. Cold-start: if bus never became busy, wait grace period then
        //    produce a synthetic QueueDrained before entering idle states.
        //    Permanently disabled after any QueueDrained is produced.
        if !cold_start_done {
            let deadline = *cold_start_deadline.get_or_insert_with(|| {
                Instant::now() + Duration::from_secs(COLD_START_DELAY_SECS)
            });
            if Instant::now() < deadline {
                tokio::time::sleep(Duration::from_millis(500)).await;
                continue;
            }
            cold_start_done = true;
            if reflection_count < BREAKER_THRESHOLD {
                let qd = QueueDrained {
                    reflection_consecutive_count: reflection_count,
                    agent_id: Some(agent_id.clone()),
                    ..
                };
                reflection_count += 1;
                let _ = local_bus.publish(qd.into()).await;
            }
            continue;
        }

        // 7. 队列空 → 真实空闲，推进 idle 状态
        let personality = detector.effective_personality();
        let kind = if detector.idle_depth == 0 {
            IdleKind::Daze
        } else {
            let arousal = coord.arousal.current();
            personality.resolve_with_arousal(detector.idle_depth, arousal)
                .unwrap_or(IdleKind::Daze)
        };

        coord.arousal.apply_behavior(kind.arousal_behavior());

        let event = IdleEvent { kind, depth: detector.idle_depth, /* ... */ };
        detector.idle_depth += 1;

        // 8. 发布 idle 事件到 Agent 的 Local EventBus
        let mut event: Event = event.into();
        event.metadata.priority = Priority::Low;
        let _ = local_bus.publish(event).await;
    }
}
```

> **与旧架构的关键区别**：
> - 旧：`IdleDetector` 实现 `EventSource` trait，由 `SourceRegistry::poll_loop` 统一调度，
>   事件发布到全局 EventBus。
> - 新：`AgentIdleManager` 自管理后台 task，直接读取 Local EventBus 的 `metrics().queue_depth`，
>   事件发布到 Agent 的 Local EventBus。Agent 间完全隔离。
> - `effective_personality()` 和双轴模型（depth + arousal）逻辑保持不变，仅运行载体改变。

### 5.4 Idle Workflow 取消机制（R2 新增）

```rust
/// Idle Workflow 在执行过程中监控 coord.idle_cancel_token。
/// 每个 checkpoint 点检查令牌状态，被取消时优雅退出。
///
/// 此模式适用于 Sleep、Exploration、Meditation 三种 Workflow。
impl IdleWorkflowRunner {
    async fn run_with_cancel<T>(
        workflow: &mut WorkflowInstance,
        cancel_token: CancellationToken,
    ) -> WorkflowResult<T> {
        loop {
            // 每个步骤执行前检查取消信号
            if cancel_token.is_cancelled() {
                // 保存当前进度
                workflow.checkpoint().await;
                return WorkflowResult::Cancelled {
                    saved_checkpoint: workflow.checkpoint_id(),
                };
            }

            match workflow.step().await {
                StepResult::Done(output) => return WorkflowResult::Completed(output),
                StepResult::Continue => continue,
                StepResult::Error(e) => {
                    workflow.checkpoint().await;
                    return WorkflowResult::Error(e);
                }
            }
        }
    }
}

/// Sleep Workflow 的典型使用
async fn execute_sleep_workflow(
    mut workflow: SleepWorkflow,
    coord: &IdleCoordination,
) {
    let token = coord.idle_cancel_token.read().unwrap().clone();
    let result = IdleWorkflowRunner::run_with_cancel(&mut workflow, token).await;
    match result {
        WorkflowResult::Cancelled { saved_checkpoint } => {
            log::info!("Sleep interrupted at checkpoint {}", saved_checkpoint);
        }
        WorkflowResult::Completed(_) => { /* 正常完成 */ }
        WorkflowResult::Error(_) => { /* 错误处理 */ }
    }
}
```

### 5.5 Incubation 后台线程生命周期（R8：Per-Agent）

每个 Agent 的 `AgentIdleManager` 持有独立的 `IncubationManager` 实例。
Incubation 线程不因真实事件中断（纯后台），仅在 Agent shutdown 时通过 `AgentIdleManager::shutdown()` 取消。

```rust
struct IncubationManager {
    max_concurrent: usize,          // 默认 1
    active_handles: Vec<IncubationHandle>,
}

impl IncubationManager {
    fn shutdown_all(&mut self) {
        for handle in self.active_handles.drain(..) {
            handle.cancel_token.cancel();
            // timeout=5s 等待线程退出
        }
    }
}
```

---

## 6. Event Flow

### 6.1 路由配置

```yaml
routes:
  - match: { event_type: "system.queue_drained" } → pipeline:reflection
  - match: { event_type: "idle.daze" }            → pipeline:idle-daze
  - match: { event_type: "idle.boredom" }         → pipeline:idle-boredom
  - match: { event_type: "idle.sleep" }           → workflow:idle-sleep
  - match: { event_type: "idle.exploration" }     → workflow:idle-exploration
  - match: { event_type: "idle.meditation" }      → workflow:idle-meditation
  - match: { event_type: "idle.waiting" }         → pipeline:idle-waiting
  - match: { event_type: "idle.incubation" }      → pipeline:idle-incubation
```

### 6.2 各空闲状态的 Pipeline / Workflow

| 状态 | 触发者 | 处理方式 | 取消机制 | 备注 |
|------|--------|---------|---------|------|
| Reflection | Dispatcher | Pipeline + select! | select! 抢先 | 可被新事件抢先取消 |
| Daze | IdleDetector | Pipeline（空） | — | 仅记录 metrics |
| Boredom | IdleDetector | BoredomActor 加权随机选技能 + MessageReceived 事件 | 否（同步执行） | 聊天模式纯 no-op；完整模式随机挑选 idle_run 技能并通过 ReAct loop 执行。work_pressure 根据队列深度动态调整 work tag 权重 |
| Sleep | IdleDetector | Workflow + cancel token | idle_cancel_token | checkpoint 保存进度 |
| Exploration | IdleDetector | Workflow + cancel token | idle_cancel_token | 断点续传 |
| Meditation | IdleDetector | Workflow + cancel token | idle_cancel_token | temp+rename 文件安全 |
| Waiting | IdleDetector | Pipeline | — | |
| Incubation | IdleDetector | Pipeline + 独立 CT | 否（纯后台，不因真实事件中断） | max_concurrent=1 |

### 6.3 聊天场景适配策略

核心原则：对话轮次之间的空闲是"轻量且无副作用的"。用户随时可能继续对话。

具体策略：

1. **源类型传播（R2-1 fix）** — Dispatcher 处理后通过 `coord.last_source_type` 写入源类型。IdleDetector 在 poll 时读取，不依赖本地字段。
2. **人格切换** — Chat Source → ChatMode 激活：仅 Daze + Boredom。
3. **Boredom 降频（R2-5 fix）** — 聊天模式 poll_interval 使用 Linear(2s, +0.5s)，而非 Fixed(2s)。Boredom Pipeline 在聊天模式下为纯 no-op。
4. **宽限期 + depth 重置（R2-4 fix）** — grace_period_secs 后退出聊天模式，idle_depth 重置为 0。
5. **配置验证（R2-7 fix）** — allowed_kinds 必须在 enabled_kinds 中存在，否则配置加载时拒绝。
6. **上下文隔离（R2-6 明确）** — 在 Pipeline 层的 ContextBuilder 中实现：IdleEvent 不进入对话 context builder。用户消息到达时丢弃 idle context，仅使用对话上下文。
7. **"正在输入"信号** — 未来扩展。Chat Source 支持 typing indicator 时可设置 `user_active` 标志抑制 IdleDetector。

---

## 7. Crate Assignment

### 7.1 新增 crate: `idle`

```
crates/idle/src/
├── types.rs        # IdleKind, IdleEvent, QueueDrained, IdlePersonality,
│                   #   IdleCoordination, IdleContext, ChatMode,
│                   #   ReflectionBreaker, PollRelaxation, ContextIsolation,
│                   #   ArousalBehavior, PollInterval
├── detector.rs     # IdleDetector: 空闲状态机（pub(crate) 字段供 manager 读写）
├── manager.rs      # AgentIdleManager: per-agent 后台 task + 生命周期（R8）
├── personality.rs  # 人格解析：depth→kind + ChatMode.as_personality() + resolve()
├── coordination.rs # IdleCoordination: reset_idle_signal()
├── workflow.rs     # IdleWorkflowRunner: run_with_cancel()
├── arousal.rs      # ArousalTracker: decay + Engaged/Passive + boost(factor)
├── incubation.rs   # IncubationManager: CancellationToken + 线程
└── config.rs       # 配置验证（含 allowed_kinds ⊆ enabled_kinds）
```

### 7.2 修改的现有 crate

| Crate | 变更 |
|-------|------|
| `core` | `EventKind::Idle` + `EventKind::QueueDrained` 变体；`SourceType` 新增 `to_u8()`/`from_u8()`/`is_chat()`；`Event` 新增 `is_from_external_source()` |
| `dispatcher` | select! 模式 + QueueDrained 生产 + last_source_type 写入 + reset_idle_signal 调用 |
| `event-bus` | `wait_for_event()` — 仅队列从空→非空时触发通知，保证无假唤醒；新增 `metrics()` 返回队列深度（供 AgentIdleManager 后台 task 使用） |
| `config` | `IdleConfig` section + 验证：`allowed_kinds ⊆ enabled_kinds` |
| `runtime` | **R8**：不再注册全局 IdleDetector 到 SourceRegistry。AgentRegistry 在 `load_from_config()` 中为每个 Agent 创建 `AgentIdleManager`（含 Local EventBus + IdleCoordination）。Phase 4 通过 `start_all_idle_loops()` 启动所有后台 task。Shutdown 时 `agent_registry.clear()` 统一停止。AgentHarness 通过 `get_idle_coordination()` 获取 per-agent 协调状态 |

---

## 8. Configuration Surface

```yaml
idle:
  enabled: true

  reflection:
    enabled: true
    timeout_secs: 60
    check_items: [chain_tasks, immediate_errors, lessons_learned]

  personality:
    enabled_kinds: [daze, boredom, sleep, exploration, meditation, incubation]
    depth_schedule:
      - [5, boredom]
      - [20, sleep]
      - [50, exploration]
      - [100, meditation]
      - [200, incubation]
    poll_interval:
      linear: { base: 5.0, multiplier: 2.0 }
    poll_relaxation:
      depth_threshold: 15
      interval_secs: 60

    chat_mode:
      allowed_kinds: [daze, boredom]            # 必须 ⊆ enabled_kinds（验证强制）
      grace_period_secs: 60
      poll_interval:
        linear: { base: 2.0, multiplier: 0.5 }  # 2s→2.5s→3s... 防止 30次/min

    reflection_breaker:
      max_consecutive: 5
      cooldown_secs: 30
      escalate_on_double: true

    context_isolation:
      pollute_chat_history: false   # Pipeline ContextBuilder 层实现
      suspend_on_user_input: true

  arousal:
    initial_value: 1.0
    half_life_secs: 900

  boredom:
    trigger_poll: 3
    activities:
      - { tag: "idle", weight: 7.5 }
      - { tag: "work", weight: 1.0 }
      - { tag: "study", weight: 0.5 }
      - { tag: "fun", weight: 0.3 }
    # 可选：work 积压越多，越倾向于选 work 技能
    work_pressure:
      target_tag: "work"
      curve: "linear"       # 或 "sigmoid"
      slope: 0.3
      max_multiplier: 10.0

  sleep:
    short_term_retention_days: 7
    cache_expiry_days: 30
    max_cpu_seconds: 60

  exploration:
    curiosity_sources: [memory_gaps, skill_audit, recent_failures]
    max_results: 20
    api_rate_per_minute: 10
    on_quota_exhausted: fallback

  meditation:
    min_interval_ticks: 20
    report_path: "~/.aman/narrative/meditation/"
    atomic_write: true

  incubation:
    max_concurrent_threads: 1
    cancel_timeout_secs: 5

  context:
    max_output_buffer: 5
```

---

## 9. Lifecycle Integration

### 9.1 启动（R8：Per-Agent）

```
Phase 0: 全局 EventBus 初始化
Phase 2: AgentRegistry::load_from_config()
         ├─ 为每个 Agent 创建 Local EventBus
         ├─ 为每个 Agent 创建 IdleCoordination
         └─ 为每个 Agent 创建 AgentIdleManager（存入 registry）
Phase 4: agent_registry.start_all_idle_loops().await
         └─ 每个 AgentIdleManager 启动后台 tokio task
```

### 9.2 关闭（R8：Per-Agent）

```
Phase 4→0 shutdown:
  agent_registry.clear():
    1. 遍历所有 AgentIdleManager，调用 shutdown()
       ├─ IncubationManager.shutdown_all() — CancellationToken → 5s 超时
       ├─ reset_idle_signal() — 中断运行中的 idle Workflow
       └─ stop_token.cancel() — 停止后台 task
    2. 清空 agents、local_buses、idle_managers 三个 map
```

### 9.3 防循环/防竞态机制总览

```
层级 1: recently_processed_real_event     — QueueDrained 不算真实事件
层级 2: reflection_consecutive_count      — 5 次后熔断
层级 3: escalate_on_double                — 10 次后 cooldown
层级 4: BusyReflecting                    — Reflection 期间 IdleDetector 不 poll
层级 5: select! + abort 先于 store(false)  — 竞态窗口关闭 (R2-9)
层级 6: wait_for_event + 二次确认          — 假唤醒防护 (R2-11)
层级 7: reset_idle_signal             — 真实事件时取消所有 idle Workflow (R2-2)
层级 8: signal_queue_drained          — 队列清空时才重置 depth，避免 poll 时序偷跑 (R7)
层级 9: arousal.boost                 — 真实事件提升 arousal，避免只衰减不回复 (R7)
```

---

## 10. Known Risks & Mitigations (R5 Updated)

| 风险 | 来源 | 严重性 | 应对策略 |
|------|------|--------|---------|
| **Reflection 连锁任务覆盖 last_source_type** | **R5-1 P2** | **已修复** | `is_from_external_source()` 守卫——仅外部 Source 事件覆盖源类型 |
| **Pipeline 空闲状态打断矩阵描述不准确** | R4-1 P2 | 已修复 | §4.4 表格新增"可被真实事件打断？"列 |
| **from_chat_mode 序列化跨会话残留** | **R4-2 P3** | **已修复** | 设计约束：IdleEvent（priority=Low）在 overflow 时丢弃不持久化 |
| **idle_depth 事件处理后不保证重置** | **R4-3 P3** | **R7 重修复** | 移除 real_event_seen，改用 pending_depth_reset（队列清空时 signal_queue_drained 设置）。解决 idle poll 提前消费 flag 的问题 |
| **Arousal 只衰减不回复** | **R7 P2** | **已修复** | 新增 ArousalTracker::boost(factor)，Dispatcher 在真实事件处理时调用 arousal.boost(0.3) |
| **Depth 重置条件在纯聊天场景下不触发** | R3-1 P1 | ~~高~~ 已修复 | 条件改为 `was_chat`（含超时+源变更） |
| **聊天 Boredom no-op 信息不可达** | R3-2 P1 | ~~高~~ 已修复 | IdleEvent.from_chat_mode 字段 |
| **Incubation 打断语义不一致** | **R3-3 P2** | **已修复** | 明确 Incubation 为纯后台，不因真实事件中断。仅 Phase 4.5 关闭时取消 |
| **reset_idle_signal RwLock 微阻塞** | **R3-4 P2** | **已接受** | 读锁持有时间为纳秒级（Arc clone），无实际风险。未来优化可用 AtomicPtr+CAS |
| **last_non_idle 时间戳滞后** | **R3-5 P3** | **已接受** | 误差 <3%（2s/60s），业务可接受 |
| **IdleDetector 在 Reflection 期间产 IdleEvent** | R1-P0 | ~~高~~ 已修复 | BusyReflecting 标志阻断 |
| **Reflection 阻塞真实事件** | R1-P1#2 | ~~高~~ 已修复 | select! 模式 |
| **last_event_from_chat 传播链断裂** | R2-1 P1 | ~~高~~ 已修复 | IdleCoordination.last_source_type |
| **空闲 Workflow 运行时不可打断** | R2-2 P1 | ~~高~~ 已修复 | idle_cancel_token |
| 聊天场景空闲误触发 | R1-P1#3 | 已缓解 | ChatMode + grace_period + 上下文隔离 |
| chat.as_personality() 未定义 | R2-3 P2 | 已修复 | 固定 depth_schedule + 继承 |
| 聊天 Boredom 高频触发 | R2-5 P2 | 已修复 | Linear poll + from_chat_mode no-op |
| Reflection 抢先时熔断不重置 | R2-8 P2 | 已修复 | 抢先分支 count=0 |
| reset_idle_signal RwLock 微阻塞 | R3-4 P2 | 已接受 | 纳秒级，无风险 |

---

## 11. Migration & Compatibility

| 旧模型 | 新模型 | 迁移方式 |
|--------|--------|---------|
| `arousal < 阈值 → 无聊` | 双轴模型：depth 解锁范围 + arousal 精调 | arousal 从调度上下文提升为 resolve 主参数 |
| 三态（忙/刚完成/空闲） | Reflection + 八态 | 刚完成→Reflection；空闲→深度序列 |
| 统一衰减 | Engaged/Passive | IdleKind.arousal_behavior() |
| 硬编码间隔 | poll_interval + poll_relaxation | 配置迁移 |
| 无聊天感知 | ChatMode + last_source_type | 新增，向后兼容 |
| 无 Workflow 中断 | idle_cancel_token (R2) | 所有 idle Workflow 迁移到 run_with_cancel |

---

## 12. Metrics

```rust
struct IdleMetrics {
    idle_depth: u32,
    idle_kind: IdleKind,
    total_idle_seconds: f64,
    kind_durations: HashMap<IdleKind, f64>,
    reflections_completed: u64,
    reflections_preempted: u64,
    reflections_timeout: u64,
    reflections_breaker_activated: u64,
    reflections_false_wakeup: u64,           // R2-11: 假唤醒次数
    chat_mode_active_seconds: f64,
    chat_to_full_switches: u64,              // R2-4: 聊天→完整切换次数
    idle_workflows_cancelled: u64,           // R2-2: idle Workflow 被取消次数
    memories_consolidated: u64,
    explorations_completed: u64,
    explorations_quota_exhausted: u64,
    meditations_completed: u64,
    incubation_threads_spawned: u64,
    incubation_threads_cancelled: u64,
}
```

---

## 13. Open Questions

1. **Reflection 产出的连锁任务去重？** 建议产出带 `dedup_key`，Event Bus 去重机制覆盖。
2. **Sleep 产出的长期记忆存哪里？** 建议独立 `memory` crate。
3. **Incubation 的"灵感"机制？** Phase 1 跳过。
4. **"正在输入"信号集成？** Chat Source 未来扩展。
5. **多 Agent 的空闲互相影响？** R8 已解决——每个 Agent 拥有独立的 IdleCoordination、Local EventBus、AgentIdleManager，Agent 间 idle 完全隔离。

---

> **R2 变更量**：
> - 类型系统：IdleCoordination 新增 2 字段（last_source_type, idle_cancel_token）
> - 新增 struct：IdleWorkflowRunner
> - 伪代码重写：Dispatcher 主循环（+40 行变更，R2-1/2/8/9/11）
> - 伪代码重写：IdleDetector.effective_personality（改用 coord.last_source_type + depth 重置）
> - 新增 §5.4：Idle Workflow 取消机制
> - 配置：chat_mode.poll_interval Fixed→Linear；验证规则 added
> - 文档新增：ContextIsolation 实现位置（Pipeline ContextBuilder）
> - 已知风险表：+13 行 R2 条目
> - Event Bus：wait_for_event() 增加 edge-triggered 保证
>
> **R1→R2 的关键修复路径**：
> 1. `Dispatcher.store(source_type)` → `coord.last_source_type` → `IdleDetector.load()` — 源类型传播链闭合
> 2. `coord.reset_idle_signal()` → `idle_cancel_token.cancel()` → `Workflow.checkpoint()` — 中断机制闭合

---

## 14. Idle State Activity Catalog

> 每个空闲子状态「适合做什么」的具体事项清单。
> 信息来源：设计文档 §8 配置暗示 + `~/.aman/skills/idle/*.yaml` 的 description 字段。

### 14.1 总览表

| 状态 | 深度范围 | 累积时间 | Arousal 行为 | 触发者 | 核心意图 |
|------|---------|---------|-------------|--------|---------|
| Reflection | — | — | — | Dispatcher (QueueDrained) | 刚完成任务后的即时复盘 |
| Daze | 0–4 | 0–20s | Passive | IdleDetector | 空闲基线，仅记 metrics |
| Boredom | 5–19 | 25–95s | Passive | IdleDetector | 感知不活跃，寻 pending 任务 |
| Sleep | 20–49 | 100–245s | Engaged (×0.5) | IdleDetector | 长期记忆整合，压缩上下文 |
| Exploration | 50–99 | 250–495s | Engaged (×0.0) | IdleDetector | 主动探索外部信息，发现信号 |
| Meditation | 100–199 | 500–995s | Engaged (×0.0) | IdleDetector | 深度内省，提炼经验更新启发式 |
| Waiting | — | — | Passive | IdleDetector (条件) | 等待外部输入或异步操作完成 |
| Incubation | 200+ | 1000s+ | Engaged (×0.1) | IdleDetector | 创意孵化，跨域联想 |

### 14.2 Reflection — 即时复盘

**触发**：Dispatcher 处理完一个真实事件且 Event Bus 队列为空时，发布 `QueueDrained`。

**check_items**（按顺序执行）：
1. `chain_tasks` — 检查刚完成的任务是否产生了新的连锁任务（如 "部署完 → 运行冒烟测试"）
2. `immediate_errors` — 检查任务执行过程中是否有被忽略的即时错误或警告
3. `lessons_learned` — 提取本轮任务的经验教训，写入经验库

**约束**：
- 通过 `select!` 实现：新事件到达时 Reflection 被抢先取消，无产出时熔断计数重置
- timeout=60s，超时视为完成（无产出）
- 连续 5 次无产出 → 跳过 `lessons_learned`；10 次 → 完全跳过 + cooldown 30s

**产出**：连锁任务 Event（注入 Event Bus）、错误报告、经验条目

### 14.3 Daze — 空闲基线

**核心意图**：Agent 处于安静基线状态——认知负荷最低。不执行任何实质性工作，仅记录 idle metrics。

**建议活动**：
- 更新 `IdleMetrics`：idle_depth 递增、kind_durations 计时
- 检查 `pending_depth_reset` 信号（R7）
- 应用 Passive arousal 衰减
- 聊天模式与完整模式行为一致（空 Pipeline，<1ms 完成）

**不适合做的事**：任何 I/O、LLM 调用、外部查询——Daze 应该是零开销的过渡态。

### 14.4 Boredom — 感知不活跃

**核心意图**：Agent 感知到不活跃，开始寻找是否有被遗忘的待处理事项。

**聊天模式**：纯 no-op——不执行任何操作。Pipeline 为同步空操作，<1ms 返回。轮次之间的空闲不应触发实质性工作。

**完整模式建议活动**：
- 扫描 deferred task queue / pending 工作队列，寻找可执行的延迟任务
- 检查是否有链式任务（Reflection 产出的连锁任务）尚未被拾取
- 随机浏览：从知识库中随机抽取条目进行快速回顾（保持认知"温热"）
- 检查定时器/提醒是否到期但未触发
- 评估 "任务饥饿度"——如果发现积压，产出低优先级提醒事件

**不适合做的事**：外部 API 调用（留给 Exploration）、深度思考（留给 Meditation）、记忆整理（留给 Sleep）

### 14.5 Sleep — 长期记忆整合

**核心意图**：将短期/工作记忆压缩到持久化存储。类比人类睡眠中的记忆巩固。

**建议活动**：
- **会话压缩**：将最近完成的对话/任务上下文从完整日志压缩为摘要嵌入
- **短期记忆 → 长期记忆**：short_term_retention_days (7天) 内的记忆进行质量筛选，高质量条目提升到长期存储
- **缓存清理**：过期缓存（>30天）的惰性清理，释放存储空间
- **索引重建**：对记忆/Tantivy 索引执行增量优化（merge segments）
- **去重**：扫描重复或高度相似的记忆条目，合并或标记
- **指标快照**：生成当前 memory store 的健康报告（条目数、大小、碎片率）

**资源约束**：
- CPU 预算：max_cpu_seconds=60（防止记忆整理抢占资源）
- 可被真实事件通过 `idle_cancel_token` 中断，checkpoint 保存进度后退出
- Engaged arousal (×0.5)：衰减速度减半，允许 Agent 在 Sleep 状态停留更久

**不适合做的事**：外部查询（无网络依赖）、实时性要求高的操作

### 14.6 Exploration — 主动探索

**核心意图**：Agent 主动向外探索，获取新信息、评估信号。类比人类无聊时刷信息流。

**好奇心源**（`curiosity_sources`）：
1. **memory_gaps** — 记忆中标记为 "待查证" 或 "信息不完整" 的条目。逐一查询补充。
2. **skill_audit** — 审计已注册 skill 的新鲜度：上游文档是否更新？API 是否变更？最佳实践是否过时？
3. **recent_failures** — 回顾最近的错误/失败，搜索外部解决方案：是否有新的 issue/PR/文章 解决了同样的问题？

**建议活动**：
- 查询外部信息源（web search、RSS feed、API endpoints）
- 对获取的信息做兴趣度评分（相关性、新鲜度、可操作性），筛选 top-N 信号
- 将高价值发现包装为低优先级 Event 注入 Event Bus（Agent 醒来后处理）
- 更新 skill 审计报告

**约束**：
- max_results=20：每轮探索最多保留 20 个信号
- api_rate_per_minute=10：对外 API 调用速率限制
- `on_quota_exhausted: fallback`：配额耗尽时降级为本地探索（扫描本地文件/日志）
- Engaged (×0.0)：arousal 不衰减，agent 在探索中保持"清醒"
- 可被真实事件通过 `idle_cancel_token` 中断，断点保存

**不适合做的事**：写操作（只读探索）、发送消息/通知（idle 不应主动联系用户）

### 14.7 Meditation — 深度内省

**核心意图**：回顾近期经验链，提取教训，更新内部启发式规则。类比人类的深度反思或冥想。

**建议活动**：
- **经验链回顾**：加载最近 N 轮任务的经验链（trace），做端到端复盘
- **教训提取**：从经验链中识别模式——"什么做对了？什么做错了？下次如何改进？"
- **启发式更新**：将提取的教训转化为内部规则/约束（如 "以后遇到 X 类型错误先检查 Y"）
- **知识图谱修剪**：清理过时或矛盾的内部知识条目
- **撰写冥想报告**：生成结构化的叙事报告，输出到 `~/.aman/narrative/meditation/`

**文件安全**：
- `atomic_write: true`：先写 temp 文件，完成后再 rename，防止崩溃损坏
- `min_interval_ticks: 20`：两次 Meditation 之间最少间隔 20 个 tick，防止连续触发

**约束**：
- 中断损失高：被 `idle_cancel_token` 中断时直接丢弃当前产出（temp+rename 保证文件安全，上一个完成的报告不受影响）
- Engaged (×0.0)：arousal 冻结，agent 在冥想中完全沉浸

**不适合做的事**：外部信息查询（那是 Exploration 的职责）、快速响应性任务

### 14.8 Waiting — 等待外部输入

**核心意图**：Agent 在等待某个外部条件满足——异步操作完成、用户回复、定时器到期。

**与 Daze 的关键区别**：
- Daze = "没什么可做的"（被动空闲）
- Waiting = "有事在做但需要等"（预期未来活动）

**建议活动**：
- 轮询或等待条件变量（如异步 HTTP 请求的 response、文件系统事件）
- 检查 timeout：等待超时后产出一个 timeout 事件
- 条件满足时立即产出一个唤醒事件，将 Agent 拉回 Active

**约束**：
- Pipeline 同步执行（条件检查极短，<1ms）
- 不可被真实事件打断（同步 Pipeline 完成后再处理新事件）
- Passive arousal 衰减

**不适合做的事**：长时间阻塞（应在 Workflow 层用 async/await 而非 busy-wait）

### 14.9 Incubation — 创意孵化

**核心意图**：后台潜意识处理——在不相关的记忆之间建立新颖连接。类比人类的 "灵感乍现"。

**建议活动**（Phase 1 跳过，以下为设计方向）：
- **跨域联想**：扫描记忆库中不同领域的条目，寻找意外的关联（如 "医疗 Agent 的错误恢复模式 是否能用于 金融 Agent？"）
- **假设生成**：基于现有知识生成 "如果……会怎样？" 的假设性问题
- **灵感评分**：对新生成的连接做新颖性和可行性评分，高价值灵感包装为 Event
- **种子演进**：对之前 Incubation 产生的灵感种子做进一步的发散推演

**约束**：
- `max_concurrent_threads: 1`：同时最多 1 个孵化线程
- `cancel_timeout_secs: 5`：shutdown 时给 5s 窗口优雅退出
- 纯后台：不因真实事件中断（仅 Agent shutdown 时取消）
- Engaged (×0.1)：arousal 极慢衰减，允许长期驻留在深层状态

**不适合做的事**：任何面向用户的输出（灵感仅为内部消费）

---

*本节的技能定义文件位于 `~/.aman/skills/idle/`。技能 YAML 的 `description` 字段与本节的「核心意图」保持同步。*
