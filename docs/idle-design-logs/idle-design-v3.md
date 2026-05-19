# Idle State System — Architecture Design

> 将 Windows "空闲进程"的隐喻落地为 Aman Agent 框架的正式子系统。
> 空闲不是无事可做，而是 Agent 在内省、维护、探索、复盘——用未被使用的周期做有价值的事。
>
> **八种空闲状态**：七种由 IdleDetector 根据空闲深度产生，一种（Reflection）由 Dispatcher
> 在队列清空时通过 QueueDrained 事件触发。
>
> **审计状态**：
> - R1（idle-design-r1.md）：11 项发现全部处理——类型系统、select! 抢占、ChatMode、熔断/配额/arousal/线程/隔离
> - R2（idle-design-r2.md）：R1 修复 9/11 ✅ 验证通过。2 个 P1 传播断裂（last_event_from_chat 传播、Workflow 中断）+
>   9 个 P2/P3 细节——全部在此版本修复

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
4. **全局中断令牌** — 所有空闲 Workflow 共享一个 `CancellationToken`。真实事件到达时 Dispatcher 取消令牌，Workflow 在下一个 checkpoint 优雅退出。
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
}

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

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IdlePersonality {
    pub enabled_kinds: HashSet<IdleKind>,
    pub depth_schedule: Vec<(u32, IdleKind)>,
    pub poll_interval: PollInterval,
    /// 仅调整 poll 频率，不改变 idle kind。idle kind 始终由 depth_schedule 决定。
    pub poll_relaxation: Option<PollRelaxation>,
    /// 聊天场景子人格（当 last_source_type 为 Chat 时生效）
    pub chat_mode: Option<ChatMode>,
    pub reflection_breaker: ReflectionBreaker,
    pub context_isolation: ContextIsolation,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PollRelaxation {
    pub depth_threshold: u32,
    pub interval_secs: f64,
}

/// 聊天模式。ChatMode 转换为 IdlePersonality 的规则（as_personality()）：
/// - depth_schedule：固定 [(0, Daze), (1, Boredom)]，depth>=1 始终返回 Boredom
/// - enabled_kinds：取 allowed_kinds（配置验证保证 allowed_kinds ⊆ 父人格 enabled_kinds）
/// - poll_relaxation、reflection_breaker、context_isolation：继承自父人格
/// - context_isolation.pollute_chat_history 强制为 false
/// - resolve() fallback：给定深度无合法 kind 时返回 Daze
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMode {
    /// 白名单——必须是 enabled_kinds 的子集（配置验证强制）
    pub allowed_kinds: HashSet<IdleKind>,
    pub grace_period_secs: f64,
    /// 建议使用 Linear 而非 Fixed——避免 Boredom 每 2s 触发 30 次/分钟
    pub poll_interval: PollInterval,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReflectionBreaker {
    pub max_consecutive: u32,       // 默认 5
    pub cooldown_secs: f64,         // 默认 30
    pub escalate_on_double: bool,   // 默认 true
}

/// 空闲上下文隔离：在 Pipeline 层的 ContextBuilder 中实现。
/// - pollute_chat_history=false：IdleEvent 不进入对话历史 context builder
/// - suspend_on_user_input=true：用户消息到达时，ContextBuilder 丢弃当前 idle 上下文，
///   仅使用对话上下文组装 LLM prompt
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextIsolation {
    pub pollute_chat_history: bool,
    pub suspend_on_user_input: bool,
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

    /// R2-2 fix: 全局空闲取消令牌。
    /// 真实事件到达时 Dispatcher 调用 cancel() + 替换为新 token。
    /// Sleep/Exploration/Meditation Workflow 在每个 checkpoint 检查此 token。
    pub idle_cancel_token: Arc<RwLock<CancellationToken>>,
}

impl IdleCoordination {
    pub fn new() -> Self {
        Self {
            busy_reflecting: Arc::new(AtomicBool::new(false)),
            arousal: Arc::new(ArousalTracker::default()),
            last_source_type: Arc::new(AtomicU8::new(SourceType::Unknown as u8)),
            idle_cancel_token: Arc::new(RwLock::new(CancellationToken::new())),
        }
    }

    /// 真实事件到达时：取消所有正在运行的 idle Workflow，
    /// 然后替换为一个新的 CancellationToken 供下一轮空闲使用。
    pub fn cancel_idle_workflows(&self) {
        let mut token = self.idle_cancel_token.write().unwrap();
        token.cancel();
        *token = CancellationToken::new();
    }
}
```

---

## 4. State Machine

### 4.1 完整状态转移

```
                        ┌──────────────────────────────────────────────────────┐
                        │      (任何真实事件到达 → cancel_idle_workflows → Active) │
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
         │         │                              (完整 → 继续)   depth=3│       │
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

1. **QueueDrained 由 Dispatcher 产生** — 每次处理完一个真实事件且队列为空时触发一次 Reflection。
2. **Reflection 可被 select! 抢先** — 新事件到达 → cancel Reflection → 处理新事件。抢先时熔断计数重置。
3. **Reflection 有 timeout 和熔断** — timeout=60s；连续 5 次→跳过 lessons_learned；10 次→完全跳过+cooldown。
4. **全局 Cancel Token** — 真实事件到达时 Dispatcher 取消令牌。所有空闲 Workflow 监控此令牌并在 checkpoint 退出。
5. **Reflection 不占深度** — depth 从 Daze 开始（depth=0）。
6. **聊天模式切换 + depth 重置** — 从聊天模式退出到完整人格时，idle_depth 重置为 0。
7. **深度递增** — 连续空闲轮次驱动。空闲类型之间切换不重置深度。
8. **allowed_kinds ⊆ enabled_kinds** — 配置验证强制。resolve() fallback = Daze。

### 4.2 完整的事件处理→空闲→再唤醒流程

```
[事件 A 到达]
  │
  ├─ Dispatcher: 取出 A
  │   coord.last_source_type.store(A.source_type)  ← R2-1: 写入源类型
  │   coord.cancel_idle_workflows()                ← R2-2: 取消正在运行的 idle Workflow
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
  │   coord.cancel_idle_workflows()  ← 中断所有 idle Workflow
  │   count 重置 → 回到 Active
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

### 4.4 打断策略矩阵

| 状态 | 触发者 | 打断机制 | 打断损失 | 打断后行为 |
|------|--------|---------|---------|-----------|
| Reflection | Dispatcher | select! 抢先 | 无 | Reflection 取消，新事件立即处理 |
| Daze | IdleDetector | —（仅在 poll 间存在） | 无 | 立即唤醒 |
| Boredom | IdleDetector | cancel_idle_workflows() | 无 | Pipeline 丢弃 |
| Sleep | IdleDetector | **idle_cancel_token** | 中 | WAL checkpoint → 退出 |
| Exploration | IdleDetector | **idle_cancel_token** | 低 | 断点保存 → 退出 |
| Meditation | IdleDetector | **idle_cancel_token** | 高 | 丢弃，temp+rename 文件安全 |
| Waiting | IdleDetector | — | 无 | 条件满足→Active |
| Incubation | IdleDetector | CancellationToken（独立） | 低 | 关联状态保存 → 线程退出 |

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

## 5. Integration with Aman Runtime

### 5.1 架构位置

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
│              ▼              │              ▼                       │
│        Dispatcher  ◄── 出队  │  ── 入队 ──▶ Dispatcher             │
│              │              │                                     │
│              │    ┌─────────▼──────────────┐                      │
│              │    │    IdleCoordination    │                      │
│              │    │ busy_reflecting        │                      │
│              │    │ last_source_type  (R2) │                      │
│              │    │ idle_cancel_token (R2) │                      │
│              │    │ arousal_tracker        │                      │
│              │    └────────────────────────┘                      │
│              │         ▲          ▲                               │
│     route: system.queue_drained → pipeline:reflection              │
│     route: idle.*               → pipeline/workflow:idle-*         │
│              │         │          │                                │
│     ┌────────┼─────────┼──────────┼────────┐                      │
│     ▼        ▼         │          ▼        ▼                      │
│  Reflection  Idle      │    Idle Workflow  │                      │
│  Pipeline    Pipeline  │    (Sleep,Explor, │                      │
│  (select!)   (Daze,    │     Meditation)   │                      │
│              Boredom)  │    监控 cancel    │                      │
│                        │    token          │                      │
│              ┌─────────▼────────┐          │                      │
│              │   Tool Runner    │          │                      │
│              └──────────────────┘          │                      │
└──────────────────────────────────────────────────────────────────┘
```

### 5.2 Dispatcher 主循环（R2 重写：last_source_type + cancel + 抢先复位 + abort 顺序 + 假唤醒）

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

                        // R2-1: 写入源类型 → IdleDetector 读取
                        coord.last_source_type.store(
                            event.source_type().to_u8(), Ordering::Relaxed
                        );

                        // R2-2: 取消所有正在运行的 idle Workflow
                        coord.cancel_idle_workflows();

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

### 5.3 IdleDetector（R2：使用 coord.last_source_type 替代本地字段）

```rust
/// IdleDetector: 感知事件队列持续为空。
///
/// R2 关键变更：
/// - 通过 coord.last_source_type 判断 ChatMode（而非本地字段）
/// - 从聊天模式退出到完整人格时，idle_depth 重置为 0
/// - resolve() 的 fallback 返回 Daze
#[async_trait]
impl EventSource for IdleDetector {
    async fn poll(&mut self, ctx: &SourceContext) -> Result<Vec<Event>> {
        if self.coord.busy_reflecting.load(Ordering::SeqCst) {
            return Ok(vec![]);
        }

        if ctx.event_bus.pending_count() > 0 {
            self.idle_depth = 0;
            self.last_non_idle = Instant::now();
            return Ok(vec![]);
        }

        // 队列空 → 真实空闲
        let personality = self.effective_personality();

        let kind = if self.idle_depth == 0 {
            IdleKind::Daze
        } else {
            // resolve: 给定深度无合法 kind → fallback = Daze
            personality.resolve(self.idle_depth, &self.agent_state)
                .unwrap_or(IdleKind::Daze)
        };

        self.coord.arousal.apply_behavior(kind.arousal_behavior());

        let event = IdleEvent {
            kind,
            depth: self.idle_depth,
            duration_secs: self.last_non_idle.elapsed().as_secs_f64(),
            context: Some(IdleContext {
                last_event_type: self.last_event_type.clone(),
                last_idle_outputs: self.last_idle_outputs.clone(),
                arousal_level: self.coord.arousal.current(),
            }),
        };

        self.idle_depth += 1;
        let mut event: Event = event.into();
        event.metadata.priority = Priority::Low;
        event.metadata.source = self.id().into();
        Ok(vec![event])
    }

    /// R2: 通过 coord.last_source_type 判断，而非本地字段。
    /// 从聊天模式切换到完整人格时重置 depth 为 0。
    fn effective_personality(&mut self) -> &IdlePersonality {
        let is_chat = SourceType::from_u8(
            self.coord.last_source_type.load(Ordering::Relaxed)
        ).is_chat();

        let was_chat = self.was_in_chat_mode;

        if is_chat {
            if let Some(ref chat) = self.personality.chat_mode {
                let elapsed = self.last_non_idle.elapsed().as_secs_f64();
                if elapsed < chat.grace_period_secs {
                    self.was_in_chat_mode = true;
                    return chat.as_personality(&self.personality);
                }
            }
        }

        // 聊天→完整人格切换：重置 depth (R2-4)
        if was_chat && !is_chat {
            self.idle_depth = 0;
        }
        self.was_in_chat_mode = false;
        &self.personality
    }
}
```

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

### 5.5 Incubation 后台线程生命周期

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
| Boredom | IdleDetector | Pipeline（无状态） | cancel_idle_workflows | 聊天模式下纯 no-op |
| Sleep | IdleDetector | Workflow + cancel token | idle_cancel_token | checkpoint 保存进度 |
| Exploration | IdleDetector | Workflow + cancel token | idle_cancel_token | 断点续传 |
| Meditation | IdleDetector | Workflow + cancel token | idle_cancel_token | temp+rename 文件安全 |
| Waiting | IdleDetector | Pipeline | — | |
| Incubation | IdleDetector | Pipeline + 独立 CT | 独立 CancellationToken | max_concurrent=1 |

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
├── detector.rs     # IdleDetector: EventSource 实现
├── personality.rs  # 人格解析：depth→kind + ChatMode.as_personality() + resolve()
├── coordination.rs # IdleCoordination: cancel_idle_workflows()
├── workflow.rs     # IdleWorkflowRunner: run_with_cancel()
├── arousal.rs      # ArousalTracker: decay + Engaged/Passive
├── incubation.rs   # IncubationManager: CancellationToken + 线程
└── config.rs       # 配置验证（含 allowed_kinds ⊆ enabled_kinds）
```

### 7.2 修改的现有 crate

| Crate | 变更 |
|-------|------|
| `core` | `EventKind::Idle` + `EventKind::QueueDrained` 变体；`SourceType` 新增 `to_u8()`/`from_u8()`/`is_chat()` |
| `dispatcher` | select! 模式 + QueueDrained 生产 + last_source_type 写入 + cancel_idle_workflows 调用 |
| `event-bus` | `wait_for_event()` — 仅队列从空→非空时触发通知，保证无假唤醒 |
| `config` | `IdleConfig` section + 验证：`allowed_kinds ⊆ enabled_kinds` |
| `runtime` | Phase 4 注册 IdleDetector + IdleCoordination；Phase 4.5 关停 + Incubation 清理 |

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

### 9.1 启动

```
Phase 0: IdleCoordination 初始化（包含 last_source_type, idle_cancel_token）
Phase 2: Dispatcher 路由注入
Phase 4: IdleDetector 注册为 Event Source
```

### 9.2 关闭

```
Phase 4.5:
  1. IdleDetector 停止
  2. IncubationManager.shutdown_all() — CancellationToken → 5s 超时
  3. cancel_idle_workflows() — 中断所有运行中的 idle Workflow
  4. 其他 Source 停止
```

### 9.3 防循环/防竞态机制总览

```
层级 1: recently_processed_real_event     — QueueDrained 不算真实事件
层级 2: reflection_consecutive_count      — 5 次后熔断
层级 3: escalate_on_double                — 10 次后 cooldown
层级 4: BusyReflecting                    — Reflection 期间 IdleDetector 不 poll
层级 5: select! + abort 先于 store(false)  — 竞态窗口关闭 (R2-9)
层级 6: wait_for_event + 二次确认          — 假唤醒防护 (R2-11)
层级 7: cancel_idle_workflows             — 真实事件时取消所有 idle Workflow (R2-2)
```

---

## 10. Known Risks & Mitigations (R2 Updated)

| 风险 | 来源 | 严重性 | 应对策略 |
|------|------|--------|---------|
| **IdleDetector 在 Reflection 期间产 IdleEvent** | R1-P0 | ~~高~~ 已修复 | BusyReflecting 标志阻断 |
| **Reflection 阻塞真实事件** | R1-P1#2 | ~~高~~ 已修复 | select! 模式 |
| **last_event_from_chat 传播链断裂** | **R2-1 P1** | ~~高~~ **已修复** | IdleCoordination.last_source_type；Dispatcher 写入→IdleDetector 读取 |
| **空闲 Workflow 运行时不可打断** | **R2-2 P1** | ~~高~~ **已修复** | idle_cancel_token；Workflow 每步检查；Dispatcher 真实事件时取消 |
| **聊天场景空闲误触发** | R1-P1#3 | ~~高~~ 已缓解 | ChatMode + grace_period + 上下文隔离 |
| **chat.as_personality() 未定义** | **R2-3 P2** | **已修复** | 明确转换规则：固定 depth_schedule + 继承父人格 |
| **Depth 跨人格边界不重置** | **R2-4 P2** | **已修复** | 聊天→完整切换时 idle_depth=0 |
| **聊天 Boredom 高频触发** | **R2-5 P2** | **已修复** | Linear poll + Boredom 聊天模式 no-op |
| **context_isolation 机制未定义** | **R2-6 P2** | **已修复** | 明确为 Pipeline ContextBuilder 层实现 |
| **allowed_kinds 交互未定义** | **R2-7 P2** | **已修复** | config 验证：allowed_kinds ⊆ enabled_kinds；resolve fallback=Daze |
| **Reflection 抢先时熔断不重置** | **R2-8 P2** | **已修复** | 抢先分支中 count=0 |
| **abort 在 busy_reflecting=false 之后** | **R2-9 P3** | **已修复** | 先 abort 再 store(false) |
| **Pipeline 阈值指令传递** | **R2-10 P3** | **已修复** | Pipeline step 1 读取 count 字段 |
| **wait_for_event 假唤醒** | **R2-11 P3** | **已修复** | 二次确认 pending_count()>0；Event Bus 保证 edge-triggered |
| Reflection 连锁任务无限循环 | R1-P2#7 | 已缓解 | reflection_breaker |
| Exploration API 配额模糊 | R1-P2#4 | 已修复 | api_rate_per_minute |
| poll_relaxation 语义混淆 | R1-P2#5 | 已修复 | 重命名 + 文档 |
| Arousal 活跃空闲中衰减 | R1-P2#6 | 已修复 | IdleKind.arousal_behavior() |
| Incubation 线程泄漏 | R1-P2#8 | 已修复 | CancellationToken |
| 多轮对话上下文污染 | R1-P2#9 | 已缓解 | ContextIsolation |
| last_idle_output 单槽 | R1-P3#10 | 已修复 | Vec<String> 环形缓冲 |
| Meditation 文件残留 | R1-P3#11 | 已修复 | temp+rename |

---

## 11. Migration & Compatibility

| 旧模型 | 新模型 | 迁移方式 |
|--------|--------|---------|
| `arousal < 阈值 → 无聊` | depth_schedule 驱动 | arousal 降级为 context |
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
5. **多 Agent 的空闲互相影响？** 不在本次范围。

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
> 2. `coord.cancel_idle_workflows()` → `idle_cancel_token.cancel()` → `Workflow.checkpoint()` — 中断机制闭合
