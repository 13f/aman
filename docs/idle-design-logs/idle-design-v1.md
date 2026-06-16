# Idle State System — Architecture Design

> 将 Windows "空闲进程"的隐喻落地为 aman Agent 框架的正式子系统。
> 空闲不是无事可做，而是 Agent 在内省、维护、探索、复盘——用未被使用的周期做有价值的事。
>
> **八种空闲状态**：其中七种由 IdleDetector 根据空闲深度产生，一种（Reflection）由 Dispatcher
> 在队列清空时通过 QueueDrained 事件触发——不属于深度驱动的空闲序列。

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
| **可变（业务策略）** | 哪些空闲类型启用、阈值、每个 Agent 的"空闲人格" |
| **技术限制** | 空闲检测必须在事件循环内部，不能依赖外部 cron（不符合 aman 的 Phase 4 后自主运行原则） |
| **性能约束** | 空闲检测本身不能成为 CPU 热点。在空闲状态下，检测逻辑本身消耗应 <1% CPU |
| **时序约束** | Reflection（复盘）必须在事件处理完成后、真正空闲开始前执行，不能等到 IdleDetector 的下一轮 poll |

---

## 2. Design Philosophy

```
空闲不是"没有事件"，而是一类特殊的事件。
它们定义了 Agent 在没有外部输入时如何与自己相处。
```

四条设计原则：

1. **IdleEvent 是 Event 的合法子类型** — 空闲事件通过 Event Bus 路由，与其他事件一视同仁。
2. **QueueDrained 是空闲入口** — Dispatcher 在队列清空时产生 QueueDrained 事件，触发 Reflection 复盘。Reflection 不是深度驱动的空闲，而是 Active 态的尾巴。
3. **空闲深度决定空闲类型** — Reflection 完成后，Agent 进入真正的空闲序列。连续空闲的时间（深度）驱动状态变迁：Daze → Boredom → Sleep → deeper。
4. **配置驱动人格** — 不同 Agent 实例可以有不同的"空闲人格"：喜欢探索 vs 喜欢发呆，阈值不同，优先级不同。

---

## 3. Type System

### 3.1 IdleKind — 七种深度驱动空闲子类型

```rust
/// 由 IdleDetector 产生的空闲子类型。
/// 对应 Agent 在队列持续为空时，随深度递增的行为模式。
///
/// 注意：Reflection（复盘）不在此枚举中。
/// Reflection 由 Dispatcher 的 QueueDrained 事件触发，不属于深度系统。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IdleKind {
    /// 真正的"空转"，最低功耗状态。
    /// 只监听事件，不执行任何主动行为。
    /// 资源：极低 CPU，最小内存占用。
    Daze,

    /// 无特定目标，低成本随机漫游。
    /// 典型行为：随机浏览 memory、轻微变更内部状态。
    /// 资源：低 CPU，可随时打断，不保留进度。
    Boredom,

    /// 维护性后台处理——记忆整理、缓存清理、短期→长期记忆转移。
    /// 有内部状态变化，打断需保存进度。
    /// 资源：低-中 CPU，正常内存。
    Sleep,

    /// 基于特定兴趣或假设的主动信息收集。
    /// 调用外部 API、搜索记忆模式、向其他 Agent 询问。
    /// 资源：高 CPU/IO，可断点续传。
    Exploration,

    /// 内省性处理——重评目标函数权重、检测策略矛盾、生成元认知报告。
    /// 不对外的"自我重构"。
    /// 资源：中 CPU，打断损失高。
    Meditation,

    /// 条件性等待——期待某个特定事件，处于低功耗但持续检查条件。
    /// 不同于 Daze 的开放等待。
    /// 资源：极低 CPU，最小内存。
    Waiting,

    /// 后台低优先级进程——不主动搜索但允许潜意识式关联。
    /// 用于"灵感涌现"式的问题解决。
    /// 资源：极低 CPU（后台线程），低内存。
    Incubation,
}
```

### 3.2 IdleEvent — IdleDetector 产生的空闲事件

```rust
/// 当事件队列持续为空（Reflection 已经完成之后），
/// IdleDetector 在每次 poll 时产生此事件注入 Event Bus。
///
/// 与其他 Event 的区别：
/// - priority 始终为 Low（空闲事件不抢占正常事件）
/// - 携带 depth 和 duration 供 Dispatcher 路由决策
/// - 不包含 Reflection——Reflection 由 QueueDrained 事件触发
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IdleEvent {
    /// 当前空闲子类型（来自 IdleKind）
    pub kind: IdleKind,

    /// 连续空闲的轮次计数。
    /// depth=0 表示第一个空闲轮次（Daze），depth 递增驱动状态变迁。
    pub depth: u32,

    /// 自上次非空闲事件以来的墙上时钟时长（秒）
    pub duration_secs: f64,

    /// 触发此次空闲的上下文快照
    pub context: Option<IdleContext>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IdleContext {
    /// 进入空闲前最后处理的事件类型
    pub last_event_type: String,

    /// 上一轮空闲产生的摘要（如果有）
    pub last_idle_output: Option<String>,

    /// 当前 arousal 水平（来自 ArousalTracker）
    pub arousal_level: f64,
}
```

### 3.3 QueueDrained — Dispatcher 产生的队列清空事件

```rust
/// 由 Dispatcher 在以下条件同时满足时产生：
/// 1. 刚处理完一个真实事件（非 QueueDrained、非 IdleEvent）
/// 2. Event Bus 队列已空（无待处理事件）
///
/// 此事件触发 Reflection Pipeline——对刚完成任务的轻量复盘。
/// Reflection 不是深度驱动的空闲，不计入 idle depth。
///
/// 防止无限循环：QueueDrained 本身不算"真实事件"。
/// Dispatcher 处理完 QueueDrained 后若队列仍空，不产生新的 QueueDrained。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueueDrained {
    /// 刚处理完的事件类型（用于 Reflection 的上下文）
    pub last_event_type: String,

    /// 刚处理完的事件的 trace_id
    pub last_trace_id: String,

    /// 该事件的处理结果摘要
    pub last_result_summary: Option<String>,

    /// 当前 arousal 水平
    pub arousal_level: f64,
}

impl EventKind {
    /// QueueDrained 不是 IdleEvent，是独立的系统事件
    pub const QUEUE_DRAINED: &'static str = "system.queue_drained";
}
```

### 3.4 IdlePersonality — 每个 Agent 的空闲人格

```rust
/// Agent 的空闲人格——控制空闲行为的可配置参数。
///
/// 注意：Reflection 不在此配置中——它是 Dispatcher 的系统行为，
/// 仅通过 reflection.timeout_secs 控制超时。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IdlePersonality {
    /// 各空闲类型的启用标志（不包含 Reflection）
    pub enabled_kinds: HashSet<IdleKind>,

    /// 空闲深度 → 空闲类型的映射
    ///
    /// 格式：[(depth_threshold, kind), ...]
    /// 按 depth_threshold 升序排列。depth=0 固定为 Daze。
    ///
    /// 例：[(1, Boredom), (3, Sleep), (5, Exploration)]
    /// 表示：发呆1轮后→无聊，3轮后→睡眠，5轮后→探索
    pub depth_schedule: Vec<(u32, IdleKind)>,

    /// IdleDetector 的 poll 间隔（秒）。
    pub poll_interval: PollInterval,

    /// 深度休眠的阈值和间隔
    pub deep_sleep: Option<DeepSleepConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PollInterval {
    /// 固定间隔
    Fixed(f64),
    /// 随深度线性增长：base + depth * multiplier
    Linear { base: f64, multiplier: f64 },
    /// 随深度指数衰减频率：base * 2^(depth / doubling_depth)
    Exponential { base: f64, doubling_depth: u32 },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeepSleepConfig {
    /// 进入深度休眠的深度阈值
    pub depth_threshold: u32,
    /// 深度休眠时的 poll 间隔（秒），建议 >= 60
    pub interval_secs: f64,
}
```

---

## 4. State Machine

### 4.1 完整状态转移

Reflection 是 Active 的尾巴——事件处理完成、队列清空的瞬间执行。
Daze 是 Idle 的起点——Reflection 无连锁任务后，Agent 进入真正的空闲序列。

```
                        ┌──────────────────────────────────────────────────┐
                        │      (任何非空闲事件到达 → 立即回到 Active)          │
                        │                                                  │
                        ▼                                                  │
    ┌────────┐  queue drained  ┌────────────┐                              │
    │ Active │────────────────▶│ Reflection │ (不计深度，每次任务完成后一次)   │
    └────────┘   (QueueDrained)└──────┬─────┘                              │
         ▲                            │                                    │
         │              有连锁任务？    │  无连锁任务                          │
         │              产生新事件      │  冷却完成                            │
         │                  │         │                                    │
         │     ┌────────────┘         ▼                                    │
         │     │                ┌──────────┐  depth=1  ┌──────────┐        │
         │     │                │   DAZE   │──────────▶│ BOREDOM  │        │
         │     │                └──────────┘           └──────────┘        │
         │     │                 depth=0                  │                │
         │     │                                   depth=3│                │
         │     │                                          ▼                │
         │     │                                    ┌──────────┐          │
         │     │                                    │  SLEEP   │          │
         │     │                                    └──────────┘          │
         │     │                                   depth=5   │            │
         │     │                       ┌──────────────────────┘            │
         │     │                       ▼                                   │
         │     │       ┌──────────┐  ┌──────────┐  ┌──────────┐           │
         │     │       │EXPLORATION│  │MEDITATION│  │INCUBATION│           │
         │     │       └──────────┘  └──────────┘  └──────────┘           │
         │     │            │              │              │                │
         │     └────────────┴──────────────┴──────────────┘                │
         │                  (连锁任务事件)                                   │
         └──────────────────────────────────────────────────────────────────┘
```

**规则**：

1. **QueueDrained 由 Dispatcher 产生** — 非 IdleDetector。每次处理完一个真实事件且队列为空时触发一次 Reflection。
2. **Reflection 有 timeout** — 最长 60 秒（可配置）。超时视为无连锁任务，进入 Daze。
3. **Reflection 不占深度** — depth 从 Daze 开始计数（depth=0）。
4. **深度递增** — 空闲时间越长 → 进入更深层的空闲状态。深度由连续空闲轮次计数决定。
5. **深度不减** — 除非被非空闲事件打断回到 Active。空闲类型之间切换不重置深度。
6. **可跳过** — 如果某个空闲类型被禁用，直接跳到下一个匹配的深度阈值。
7. **事件打断** — 任何 `priority > Low` 的事件到达 → 立即回到 Active。
8. **条件分支** — 深度达到 5 后，根据 Agent 当前状态选择 Exploration / Meditation / Incubation。

### 4.2 完整的单次事件处理→空闲→再唤醒流程

```
时间轴:
──────────────────────────────────────────────────────────────▶

[事件 A 到达]
  │
  ├─ Dispatcher: 取出 A, 路由到 Pipeline, Pipeline 执行完毕
  │
  ├─ Dispatcher: try_dequeue() → None
  │   recently_processed_real_event == true
  │   → 发布 QueueDrained 到 Event Bus
  │
  ├─ Dispatcher: 取出 QueueDrained, 路由到 Reflection Pipeline
  │   Reflection 检查三项：
  │     1. 刚完成的任务有连锁任务吗？
  │     2. 有需要立即关注的错误吗？
  │     3. 有值得记录的经验吗？
  │   timeout = 60s
  │
  ├─ 情况 A: Reflection 产出新事件 → 注入 Event Bus
  │   → Dispatcher 取出新事件 → 回到 Active
  │
  ├─ 情况 B: Reflection 无产出
  │   → Dispatcher: try_dequeue() → None
  │   → recently_processed_real_event == false
  │   → 不发布新的 QueueDrained（终止循环）
  │
  ├─ [真正空闲开始]
  │
  ├─ IdleDetector poll 1: queue empty → 产生 IdleEvent(kind=Daze, depth=0)
  │   → 路由到 idle-daze Pipeline (空操作，记录 metrics)
  │
  ├─ IdleDetector poll 2: queue empty → 产生 IdleEvent(kind=Boredom, depth=1)
  │   → 路由到 idle-boredom Pipeline (随机漫游)
  │
  ├─ IdleDetector poll N: queue empty → 产生 IdleEvent(kind=Sleep, depth=3)
  │   → 路由到 idle-sleep Workflow (记忆整理)
  │
  ├─ [事件 B 到达] ← 外部 Source 产生
  │
  └─ Dispatcher: 取出 B, 是真实事件
      recently_processed_real_event = true
      IdleDetector.next_poll: queue 非空 → 重置 idle_depth = 0
      → 回到 Active，处理 B
```

### 4.3 深度→类型映射的默认配置

```yaml
idle:
  personality:
    enabled_kinds: [daze, boredom, sleep, exploration, meditation]
    depth_schedule:
      - [1, boredom]           # 发呆1轮后开始无聊
      - [3, sleep]             # 3轮后触发记忆整理
      - [5, exploration]       # 5轮后主动探索（或 meditation，看条件）
    poll_interval:
      linear: { base: 5.0, multiplier: 2.0 }
    deep_sleep:
      depth_threshold: 15
      interval_secs: 60

  # Reflection 独立于空闲人格
  reflection:
    enabled: true
    timeout_secs: 60           # 单次 Reflection 的最长执行时间
```

### 4.4 打断策略矩阵

| 空闲/过渡状态 | 触发者 | 打断损失 | 进度保存 | 打断后行为 |
|-------------|--------|---------|---------|-----------|
| Reflection | Dispatcher (QueueDrained) | 无 | 不需要 | 超时视为完成，进入 Daze |
| Daze | IdleDetector | 无 | 不需要 | 立即唤醒 |
| Boredom | IdleDetector | 无 | 不需要 | 立即丢弃，回到 Active |
| Sleep | IdleDetector | 中 | WAL 写入 checkpoint | 恢复时从 checkpoint 继续或丢弃 |
| Exploration | IdleDetector | 低 | 断点保存 | 恢复时从断点继续 |
| Meditation | IdleDetector | 高 | 不保存 | 丢弃，下次空闲重新触发 |
| Waiting | IdleDetector | 无 | 不需要 | 条件满足→Active，否则继续等待 |
| Incubation | IdleDetector | 低 | 关联状态列表 | 后台线程继续运行，不打断 |

---

## 5. Integration with aman Runtime

### 5.1 架构位置

IdleDetector 作为 **Event Source** 插入 aman 的 Source 层。
QueueDrained 由 **Dispatcher** 产生，经 Event Bus 流动——Dispatcher 既是生产者也是消费者。

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
│                    ┌────────▼────────┐                             │
│                    │   Event Bus     │                             │
│                    └────────┬────────┘                             │
│                             │                                      │
│              ┌──────────────┼──────────────┐                       │
│              │              │              │                       │
│              ▼              ▼              ▼                       │
│        Dispatcher  ◄── 出队  │  ── 入队 ──▶ Dispatcher             │
│        (消费者)              │             (QueueDrained 生产者)    │
│              │               │                                     │
│     route: system.queue_drained → pipeline:reflection              │
│     route: idle.*               → pipeline/workflow:idle-*         │
│              │                                                     │
│     ┌────────┼────────┐                                            │
│     ▼        ▼        ▼                                            │
│  ┌──────────┐  ┌──────────┐  ┌──────────┐                        │
│  │Reflection│  │  Idle    │  │  Idle    │                        │
│  │ Pipeline │  │ Pipeline │  │ Workflow │                        │
│  │(timeout= │  │(Boredom, │  │(Sleep,   │                        │
│  │  60s)    │  │ Daze)    │  │Explor...)│                        │
│  └──────────┘  └──────────┘  └──────────┘                        │
│        │              │              │                             │
│        └──────────────┼──────────────┘                             │
│                       │                                            │
│              ┌────────▼────────┐                                   │
│              │   Tool Runner   │                                   │
│              └─────────────────┘                                   │
└──────────────────────────────────────────────────────────────────┘
```

### 5.2 QueueDrained 在 Dispatcher 中的生产逻辑

```rust
/// Dispatcher 主循环中的 QueueDrained 生产逻辑
impl Dispatcher {
    async fn run_loop(&mut self) {
        let mut recently_processed_real_event = false;

        loop {
            match self.event_bus.try_dequeue().await {
                Some(event) => {
                    let is_real = !event.is_queue_drained() && !event.is_idle_event();

                    if is_real {
                        recently_processed_real_event = true;
                    }

                    // 路由并等待处理完成
                    self.dispatch(event).await;

                    // 继续下一轮
                }
                None => {
                    // 队列空
                    if recently_processed_real_event {
                        // 刚处理完一个真实事件 → 触发 Reflection
                        recently_processed_real_event = false;

                        let drained = QueueDrained {
                            last_event_type: self.last_event_type.clone(),
                            last_trace_id: self.last_trace_id.clone(),
                            last_result_summary: self.last_result_summary.take(),
                            arousal_level: self.arousal.current(),
                        };

                        self.event_bus.publish(drained.into_event()).await;
                        // 继续循环 → 下一轮取出 QueueDrained → 路由到 Reflection
                    }
                    // recently_processed_real_event == false:
                    // 队列空且无待复盘 → 真正空闲
                    // IdleDetector 的下一次 poll 会检测到
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            }
        }
    }
}
```

### 5.3 IdleDetector 伪代码

```rust
/// IdleDetector: 感知事件队列持续为空。
///
/// 与 QueueDrained 的分工：
/// - QueueDrained（Dispatcher 产生）：队列刚清空的瞬间，触发 Reflection
/// - IdleDetector（本组件）：Reflection 之后队列仍为空，按深度产生 IdleEvent
#[async_trait]
impl EventSource for IdleDetector {
    fn source_type(&self) -> SourceType {
        SourceType::InternalTimer
    }

    async fn poll(&mut self, ctx: &SourceContext) -> Result<Vec<Event>> {
        let queue_empty = ctx.event_bus.pending_count() == 0;

        if !queue_empty {
            // 队列非空 → 有正常事件或 Reflection → 重置
            self.idle_depth = 0;
            self.last_non_idle = Instant::now();
            return Ok(vec![]);
        }

        // 队列空 → 真正的空闲
        // depth=0 时产生 Daze（发呆），depth>=1 按 depth_schedule 映射
        let kind = if self.idle_depth == 0 {
            IdleKind::Daze
        } else {
            self.personality.resolve(self.idle_depth, &self.agent_state)
        };

        let event = IdleEvent {
            kind,
            depth: self.idle_depth,
            duration_secs: self.last_non_idle.elapsed().as_secs_f64(),
            context: Some(IdleContext {
                last_event_type: self.last_event_type.clone(),
                last_idle_output: self.last_idle_output.take(),
                arousal_level: self.arousal.current(),
            }),
        };

        self.idle_depth += 1;

        let mut event: Event = event.into();
        event.metadata.priority = Priority::Low;
        event.metadata.source = self.id().into();
        Ok(vec![event])
    }

    fn poll_interval(&self) -> Duration {
        self.personality.effective_interval(self.idle_depth)
    }
}
```

---

## 6. Event Flow

### 6.1 QueueDrained 的 Dispatcher 路由

```yaml
routes:
  # QueueDrained → Reflection（Dispatcher 自产自消）
  - match:
      event_type: "system.queue_drained"
    target: "pipeline:reflection"

  # IdleEvent 按 kind 路由
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

| 状态 | 触发者 | 处理方式 | 理由 |
|------|--------|---------|------|
| Reflection | Dispatcher (QueueDrained) | Pipeline（有 timeout） | 轻量复盘：三个固定检查项，无需状态 |
| Daze | IdleDetector | Pipeline（空） | 仅记录 metrics（daze_duration, depth） |
| Boredom | IdleDetector | Pipeline（无状态） | 随机行为不可重复，不需要状态管理 |
| Sleep | IdleDetector | Workflow（有状态） | 记忆整理分阶段：扫描→分类→归档→验证 |
| Exploration | IdleDetector | Workflow（有状态） | 搜索结果需要断点续传 |
| Meditation | IdleDetector | Workflow（有状态） | 内省分阶段，中断损失大 |
| Waiting | IdleDetector | Pipeline | 检查一个条件，满足则升级为 Active 事件 |
| Incubation | IdleDetector | Pipeline | 触发后台关联任务，主流程不阻塞 |

---

## 7. Crate Assignment

### 7.1 新增 crate: `idle`

```
kernel/idle/
├── Cargo.toml
├── src/
│   ├── lib.rs               # 公开 API
│   ├── types.rs              # IdleKind, IdleEvent, QueueDrained, IdlePersonality
│   ├── detector.rs           # IdleDetector: EventSource 实现
│   ├── personality.rs        # 人格解析：深度→类型映射
│   ├── arousal.rs            # ArousalTracker: 从 boring-design 迁移
│   └── config.rs             # 配置验证
```

### 7.2 依赖关系

```
idle
  ├── core (Event, Priority, EventKind, Source trait)
  ├── event-bus (pending_count)
  ├── config (配置层)
  └── persistence (WAL checkpoint for Sleep/Exploration 进度)
```

### 7.3 修改的现有 crate

| Crate | 变更 |
|-------|------|
| `core` | 新增 `EventKind::Idle(IdleKind)` 变体；新增 `EventKind::QueueDrained` 变体；`Event` 新增 `is_queue_drained()` 等方法 |
| `dispatcher` | 新增 QueueDrained 生产逻辑 + `recently_processed_real_event` 标志 |
| `source` | 无需修改 trait；IdleDetector 作为新的 Source 实现 |
| `config` | 新增 `IdleConfig` section（含 `reflection.timeout_secs`） |
| `runtime` | Phase 4 注册 IdleDetector；Phase 4.5 关停 |

---

## 8. Configuration Surface

```yaml
# agent.yaml — idle section
idle:
  # 是否启用空闲检测（默认 true）
  enabled: true

  # Reflection 复盘配置（由 QueueDrained 触发，不属于空闲人格）
  reflection:
    enabled: true
    timeout_secs: 60          # 单次 Reflection 的最大执行时间
    check_items:               # Reflection 的检查清单
      - chain_tasks            # 刚完成的任务有无连锁任务？
      - immediate_errors       # 有无需要立即关注的错误？
      - lessons_learned        # 有无值得记录的经验？

  # 空闲人格（仅 IdleDetector 产生的深度驱动空闲）
  personality:
    enabled_kinds: [daze, boredom, sleep, exploration, meditation]
    
    depth_schedule:
      - [1, boredom]           # depth=0 固定为 Daze，此处从 depth=1 起
      - [3, sleep]
      - [5, exploration]       # 或 meditation，看条件分支

    poll_interval:
      linear: { base: 5.0, multiplier: 2.0 }

    deep_sleep:
      depth_threshold: 15
      interval_secs: 60

  # Sleep 类型的行为参数
  sleep:
    short_term_retention_days: 7
    cache_expiry_days: 30
    max_cpu_seconds: 60

  # Exploration 类型的行为参数
  exploration:
    curiosity_sources: [memory_gaps, skill_audit, recent_failures]
    max_results: 20
    api_rate_limit: 10

  # Meditation 类型的行为参数
  meditation:
    min_interval_ticks: 20
    report_path: "~/.aman/narrative/meditation/"

  # Arousal 衰减参数（从原 boring-design 迁移）
  arousal:
    initial_value: 1.0
    half_life_secs: 900
    boredom_threshold: 0.3
```

---

## 9. Lifecycle Integration

### 9.1 启动

```
Phase 2 [组件注册]:
  - Dispatcher 路由注入（含 QueueDrained → Reflection 路由）

Phase 4 [源激活]:
  - IdleDetector 注册为 Event Source
  - IdleDetector 的 poll() 在 Phase 4 首次调用
  - 此时 Event Bus、Dispatcher、Pipeline/Workflow 均已就绪
```

### 9.2 关闭

```
Phase 4.5 [源停止]:
  1. IdleDetector 停止 → 不再产生新 IdleEvent
  2. 其他 Source 停止

Phase 4.5 [排水]:
  - 正在执行的 Reflection (timeout=60s) 被 drain_timeout_sec 覆盖
  - 正在执行的 Sleep/Exploration/Meditation 按已有排水机制处理
```

### 9.3 QueueDrained 防止无限循环的机制

```
Dispatcher 主循环中有标志位: recently_processed_real_event

初始值: false

处理真实事件 → 标志 = true
队列空 + 标志 == true → 发 QueueDrained + 标志 = false
取出 QueueDrained → 不是真实事件 → 标志保持 false
Reflection 完成 → 无产出 → 队列空 + 标志 == false → 不发 QueueDrained ✓

Reflection 有产出 → 注入新事件（真实事件）
取出新事件 → 标志 = true → 处理后队列空 → 发 QueueDrained ✓
  （这是合理的：新事件是个独立任务，完成后应该再复盘）
```

---

## 10. Resources & Performance

| 状态 | CPU 预算 | 内存增量 | 触发者 |
|------|---------|---------|--------|
| Reflection | <5% | 0 | Dispatcher（同步，有 timeout=60s） |
| Daze | <0.1% | 0 | IdleDetector |
| Boredom | <2% | 0 | IdleDetector |
| Sleep | <10% | +临时索引 | IdleDetector |
| Exploration | <30% | +搜索结果 | IdleDetector |
| Meditation | <15% | +内省状态 | IdleDetector |
| Waiting | <0.1% | 0 | IdleDetector |
| Incubation | <5%（后台） | +关联图 | IdleDetector |

---

## 11. Known Risks & Mitigations

| 风险 | 严重性 | 应对策略 |
|------|--------|---------|
| **QueueDrained 无限循环**（Reflection 产出一个 trivial 事件→处理→又 QueueDrained→又 Reflection...） | 高 | `recently_processed_real_event` 标志 + QueueDrained 不算真实事件。若 Reflection 每次都有产出且每次都 > 0，说明业务逻辑本身在自产自消——属于正常行为 |
| **Reflection 超时未完成** | 低 | timeout=60s 硬限制，超时视为无连锁任务，进入 Daze。Pipeline 的 timeout 机制已支持 |
| **Sleep 在 shutdown 时丢失进度** | 低 | WAL checkpoint 保存 Sleep 阶段；恢复时跳过已完成阶段 |
| **Exploration 耗尽 API 配额** | 高 | `exploration.api_rate_limit` 硬限制 + `max_results` 上限 |
| **Meditation 产出错误认知** | 中 | 接受。产出带 `confidence` 分数，低分不自动写入 memory |
| **空闲深度无限增长** | 低 | `deep_sleep` 限制 poll 频率；operator 通过 health check 监控 `idle_depth` metric |
| **IdleDetector 与 QueueDrained 的时序竞争** | 低 | IdleDetector.poll() 取 pending_count() 的瞬时快照。QueueDrained 注入→pending_count 变 1→下个 poll 看到非空→重置 depth。时序是安全的 |

---

## 12. Migration from Existing Arousal Model

现有的 `agent-boredom-narrative-event-driven.md` 中定义了 arousal decay 模型。本设计保留了 arousal 作为 IdleContext 字段，但不作为空闲分类的唯一依据。

### 迁移路径

| 旧模型 | 新模型 | 迁移方式 |
|--------|--------|---------|
| `arousal_level < 阈值 → 无聊` | `depth_schedule` 决定空闲类型 | arousal 降级为 context 字段。如需旧行为，配置 `depth_schedule: [[1,boredom]]` 并使 Pipeline 检查 arousal |
| 三态（忙/刚完成/空闲） | Reflection + Daze + 七态空闲 | 刚完成 → Reflection（QueueDrained 触发）；空闲 → Daze → 深度序列 |
| 硬编码的 5min/30min 间隔 | 配置化的 `poll_interval` + `deep_sleep` | 迁移配置即可 |
| 独立的 arousal 计算 | 合并到 `idle::arousal` 模块 | `ArousalTracker` 作为 `IdleDetector` 的内部组件 |

---

## 13. Metrics & Observability

```rust
struct IdleMetrics {
    idle_depth: u32,
    idle_kind: IdleKind,
    total_idle_seconds: f64,
    kind_durations: HashMap<IdleKind, f64>,
    reflections_completed: u64,      // Reflection 完成次数
    reflections_timeout: u64,        // Reflection 超时次数
    reflections_produced_events: u64, // Reflection 产出连锁任务的次数
    memories_consolidated: u64,
    explorations_completed: u64,
    meditations_completed: u64,
}
```

---

## 14. Open Questions

1. **Reflection 产出的连锁任务如何避免重复触发？** 如果同一个任务每次 Reflection 都产出一个"检查是否完成"的子任务，会形成垃圾循环。建议 Reflection 产出带 `dedup_key`，Event Bus 的去重机制覆盖。
2. **Sleep 产出的长期记忆存哪里？** 建议独立 `memory` crate（未来设计）。
3. **Incubation 的"灵感"机制？** Phase 1 实现跳过，待 Sleep/Exploration 稳定后再设计。
4. **多 Agent 的空闲互相影响？** 不在本次设计范围。

---

> **这份设计能承载几轮业务迭代而不需要重写？**
>
> - 新增空闲类型：`IdleKind` enum 加变体 + `depth_schedule` 加映射 + 一个 Pipeline/Workflow。改动范围：3 处。
> - 修改 Reflection 超时：改 `reflection.timeout_secs`，0 处代码改动。
> - 新增 Reflection 检查项：改 `reflection.check_items` + Pipeline step，1 处改动。
> - 替换 Arousal 模型：`idle::arousal` 模块内部重构，不影响外部接口。
> - QueueDrained 的生产者从 Dispatcher 迁移到别处：改 `recently_processed_real_event` 的位置，风险可控。
