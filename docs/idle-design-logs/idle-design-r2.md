# Idle State System — 业务逻辑审计 R2

> 审计目标：R1 修复验证 + 新一轮盲区发现
> 审计范围：事件流、chat 交互、状态转换、边界场景
> R1 修正状态：文档标记 11 项发现"全部处理"——本报告验证每项修复的正确性，并寻找 R1 遗漏的问题

---

## 第一部分：R1 修复验证

| R1 # | 严重性 | 核心问题 | 修复方式 | 验证状态 |
|------|--------|---------|---------|:-------:|
| 1 | P0 💥 | IdleDetector 在 Reflection 期间产 IdleEvent | `IdleCoordination.busy_reflecting` 标志阻断 | ✅ 正确 |
| 2 | P1 🚨 | Reflection 不可被打断 | Dispatcher 改为 `select!` 模式 | ✅ 正确 |
| 3 | P1 🚨 | 聊天场景空闲误触发 | `ChatMode` + `grace_period` + `context_isolation` | ⚠️ 结构对，但有传播链断裂 |
| 4 | P2 ⚠️ | API 配额作用域模糊 | `api_rate_per_minute: 10` + `on_quota_exhausted` | ✅ 正确 |
| 5 | P2 ⚠️ | deep_sleep 语义混淆 | 重命名 `poll_relaxation`，明确不改变 idle kind | ✅ 正确 |
| 6 | P2 ⚠️ | Arousal 在活跃空闲中衰减 | `IdleKind.arousal_behavior()` 分 Passive/Engaged | ✅ 正确 |
| 7 | P2 ⚠️ | Reflection 连锁任务无限循环 | `ReflectionBreaker` max_consecutive=5 + cooldown | ✅ 逻辑正确 |
| 8 | P2 ⚠️ | Incubation 线程泄漏 | `CancellationToken` + `IncubationManager` | ✅ 结构正确 |
| 9 | P2 ⚠️ | 多轮对话空闲上下文污染 | `ContextIsolation` 配置项 | ⚠️ 意图明确但实现机制未定义 |
| 10 | P3 🔍 | last_idle_output 单槽丢失 | 改为 `Vec<String>` 环形缓冲 | ✅ 正确 |
| 11 | P3 🔍 | Meditation 文件完整性 | `atomic_write: true` + tmp+rename | ✅ 正确 |

**修复质量总评**：10/11 项修复在结构上是正确的。但 #3 和 #9 存在实现层面的断裂点——设计的"是什么"已经明确，但"怎么连接"还未定义。

---

## 第二部分：R2 新发现

| # | 严重性 | 关注点 | 核心问题 |
|---|--------|--------|---------|
| **R2-1** | **P1 🚨** | **`last_event_from_chat` 传播链断裂** | ChatMode 的激活依赖此标志，但 IdleDetector 无法获知最新事件是否来自 Chat Source |
| **R2-2** | **P1 🚨** | **空闲 Workflow 不可自动打断** | Sleep/Exploration/Meditation 运行时无中断机制——真实事件到达后它们继续在后台执行 |
| R2-3 | P2 ⚠️ | `chat.as_personality()` 未定义 | ChatMode 转 IdlePersonality 的映射缺失，深度不匹配时的 fallback 未定义 |
| R2-4 | P2 ⚠️ | Depth 跨人格边界不重置 | 聊天模式 60s 积累深度后切换到完整人格，直接跳入深度空闲（Exploration at 60s interval） |
| R2-5 | P2 ⚠️ | 聊天模式 Boredom 每 2s 触发一次 | 30 次/分钟的 Boredom Pipeline 调用的开销 |
| R2-6 | P2 ⚠️ | `context_isolation.suspend_on_user_input` 机制未定义 | "挂起空闲上下文"在代码层面没有对应实现 |
| R2-7 | P2 ⚠️ | `allowed_kinds` 与 `enabled_kinds` 交互未定义 | 交集？并集？kind 不在 depth_schedule 中时的 resolve 行为？ |
| R2-8 | P2 ⚠️ | Reflection 被抢先时熔断计数不重置 | preempt 后 `reflection_consecutive_count` 维持原值，后续计数偏移 |
| R2-9 | P3 🔍 | `reflection.abort()` 在 `busy_reflecting=false` 之后的窗口 | 先清标志再 abort，IdleDetector 可能在 abort 完成前 poll |
| R2-10 | P3 🔍 | Reflection 5 次熔断阈值的 pipeline 指令传递 | queue_drained.event.reflection_consecutive_count 需要 pipeline 层理解该字段 |
| R2-11 | P3 🔍 | `wait_for_event()` 假唤醒风险 | select! 分支无 timeout，spurious wakeup 导致 innocent abort |

---

## P1 — 新发现（严重）

---

### R2-1 🎯: `last_event_from_chat` 传播链断裂

📐 场景：

IdleDetector.poll() 中调用 `effective_personality()`，其逻辑是：
```rust
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
```

但 `self.last_event_from_chat` 是 `IdleDetector` 的内部字段（代码中假设在 `IdleDetector` struct 上有此字段）。问题是：**Dispatcher 处理完一个 Chat Source 事件后，如何将这个信息传递到 IdleDetector？**

现有机制分析：
- `IdleCoordination` 包含 `busy_reflecting` 和 `arousal_tracker`，但缺少 `last_event_from_chat`
- `IdleContext` 中有 `last_event_from_chat: bool`，但这是 IdleDetector **产出**给消费方的，不是接收外部输入的
- 文档 6.3 节写"Chat Source 产生的用户消息事件带有源类型标记"，但没有任何代码路径展示 Dispatcher 如何将这个消息写到 IdleDetector 能读的位置

💥 可能后果：

- **ChatMode 永远不会激活**：如果 `last_event_from_chat` 默认 false 且没有外部写入路径，聊天场景下的空闲人格与系统场景完全一致
- **R1 P1#3 的修复实际上不生效**：文档声称已修复，但关键传播路径是断的
- **热修复方向**：可能由 `IdleDetector` 在 poll 时主动检查 `Event Bus` 中最近的 1-2 个事件的 metadata，但这增加了 poll 的开销

🛠 建议：

1. **扩展 IdleCoordination**：添加 `last_source_type: Arc<AtomicU32>`（或一个 SourceType 的原子标识）。Dispatcher 在处理事件时设置此标志，IdleDetector 在 poll 时读取。
2. **或利用 Event Bus metadata**：IdleDetector 在 poll 时检查队列中最近一次被取出的事件类型（如果 Event Bus 保留此信息）。
3. **或通过事件 pipeline**：每个事件的处理结果中包含 source_type 信息，IdleDetector 通过共享状态获取。
4. **明确写入伪代码**：在 Dispatcher 伪代码中添加 `coord.last_source_type.store(event.source_type(), Ordering::Relaxed)`。

---

### R2-2 🎯: 空闲 Workflow 不可自动打断

📐 场景：

IdleDetector 在深度空闲时产生 `IdleEvent(kind=Sleep, depth=3)`，路由到 Workflow `idle-sleep`。Workflow 开始执行记忆整理（预计 5-10s，分阶段：scan→classify→archive→verify）。

此时一个真实事件到达 Event Bus。Dispatcher 的 `try_dequeue()` 返回该事件，开始处理。但 `idle-sleep` Workflow 是一个独立的任务/线程——Dispatcher 处理真实事件时，Sleep 仍在后台运行。

💥 可能后果：

- **后台污染的断言**：Sleep 正在整理记忆、修改 agent 内部状态。同时 Dispatcher 在处理真实事件。两个任务同时对 agent 状态进行操作——除非所有状态都是 `Arc<RwLock<>>`，否则存在数据竞争。
- **干扰真实事件响应**：Sleep 的 CPU 预算显示 "<10% + 临时索引"。但即使 10%，也在与真实事件争抢 CPU。
- **WAL checkpoint 的无效化**：Sleep 被打断时写出 checkpoint。但如果是在真实事件处理的中途被打断，checkpoint 可能捕获了一个与真实事件响应不一致的中间状态。
- **Exploration 更严重**：Exploration 可能正在调用外部 API。API 调用的结果可能与当前真实事件响应产生意外的交互。

**关键矛盾**：打断策略矩阵声称 Sleep/Exploration/Meditation 的可打断性为"是"，但没有任何机制实现这个"打断"。

对比 Incubation：Incubation 有明确的 `CancellationToken`，shutdown 时会调用 `IncubationManager.shutdown_all()`。但 Sleep/Exploration/Meditation 没有类似机制。

🛠 建议：

1. **全局 Cancel Token**：`IdleCoordination` 添加一个 `idle_cancel_token: CancellationToken`。每当真实事件到达：
   - Dispatcher 设置 `token.cancel()`
   - 正在运行的 idle Workflow（Sleep/Exploration/Meditation）监控此 token，在下一个 checkpoint 点优雅退出
   - 退出时保存进度（如有）
   - 新的 idle tick 开始时，替换为新的 `CancellationToken`

2. **或 Workflow 层支持中断**：将 WaitGroup 或类似机制集成到 Workflow 引擎中，让 Workflow 执行时能响应中断信号。

3. **或使用单独的 Runtime**：Idle Workflow 跑在 `tokio::task::spawn` 上，Dispatcher 持有它们的 `JoinHandle`。真实事件到达时，Dispatcher 直接 abort 这些任务。

---

## P2 — 结构性问题

---

### R2-3 🎯: `chat.as_personality()` 未定义

📐 `IdleDetector.effective_personality()` 调用 `chat.as_personality()`，但 `ChatMode` 的定义是：
```rust
pub struct ChatMode {
    pub allowed_kinds: HashSet<IdleKind>,
    pub grace_period_secs: f64,
    pub poll_interval: PollInterval,
}
```

**缺少的信息**：
- `depth_schedule`——ChatMode 没有深度映射表，只有 allowed_kinds 白名单
- `enabled_kinds`——ChatMode 没有自己的启用列表
- `poll_relaxation`——ChatMode 有独立的 poll_interval，但 poll_relaxation 从父人格继承？
- `reflection_breaker`——从父人格继承还是有自己的？
- `context_isolation`——从父人格继承还是有自己的？

`as_personality()` 需要将 ChatMode 的结构映射到 `IdlePersonality`：
- 它必须生成一个 `depth_schedule`，其中只包含 allowed_kinds 对应的深度
- 如果没有定义，`resolve(depth, agent_state)` 可能在深度找不到匹配时返回 undefined 的 IdleKind

💥 可能后果：

- **运行时 panic 或默认行为错误**：如果 `resolve()` 在给定深度找不到合法的 idle kind（比如 chat 模式只允许 daze/boredom，depth=3 没有映射），返回 `None` 或一个未初始化的 IdleKind
- **聊天模式意外允许深度空闲**：如果 `as_personality()` 返回父人格的 depth_schedule 但只 override enabled_kinds，Sleep 事件仍然可能按 depth_schedule 产生

🛠 建议：

1. **明确定义 `as_personality()` 的转换规则**：
   - `depth_schedule` 只包含 `[(0, Daze), (1, Boredom)]`，depth>=1 后始终返回 Boredom
   - `poll_relaxation` 从父人格继承
   - `reflection_breaker` 从父人格继承
   - `enabled_kinds` 取 `allowed_kinds`
   - `context_isolation` 从父人格继承但强制 `pollute_chat_history = false`
2. **或改为 `ChatMode` 直接包含一个独立的 `IdlePersonality` 配置块**（配置中嵌套），这样语义清晰，不需要转换。

---

### R2-4 🎯: Depth 跨人格边界不重置

📐 配置中空闲的人格切换流程：

```
t=0s   聊天模式激活，depth=0，Daze
t=2s   聊天模式，depth=1，Boredom
t=4s   聊天模式，depth=2，Boredom
...
t=58s  聊天模式，depth=29，Boredom
t=60s  grace_period_secs 到期
t=61s  切换到完整人格，depth=30
        idle_kind = resolve(30) → depth_schedule 最后匹配是 Exploration (depth=5)
        poll_relaxation: depth_threshold=15 → interval=60s
```

💥 可能后果：

- **直接跳到深度空闲**：用户谈话间隙 60s 后，agent 不经过 Daze/Boredom/Sleep 的渐进，直接到达 Exploration/Meditation 的高成本状态。这个跳跃是用户的预期吗？
- **完全跳过了记忆整理（Sleep）**：如果用户每天都有几次 60s+ 的思考间隙，agent 永远不会在 idle 中做记忆整理（Sleep depth=3），因为聊天模式积累的深度已经超过它了
- **Poll interval 突变**：从 2s 直接跳到 60s（poll_relaxation）。如果下一个真实事件在 30s 后到达，IdleDetector 还没 poll，agent 在 30s 内"没反应"（深层空闲但 no poll）

🛠 建议：

1. **人格切换时重置 depth**：从聊天模式退出到完整人格时，`idle_depth = 0`。这样完整人格从 Daze 开始渐进。
2. **或保留 depth 但施加上限**：切换到完整人格时 `idle_depth = min(idle_depth, 1)`，至少从 Boredom 开始。
3. **文档中明确语义决策**：当前文档说"深度递增...空闲类型之间切换不重置深度"——人格切换是否算"空闲类型切换"需要澄清。

---

### R2-5 🎯: 聊天模式 Boredom 每 2s 触发

📐 聊天模式下 `poll_interval: fixed: 2.0`。IdleDetector 每 2s poll 一次。聊天模式允许 Daze（depth=0，仅一次）和 Boredom（depth>=1）。

在 60s grace period 内，Boredom 事件产生次数：29-30 次。

💥 可能后果：

- **Pipeline 开销**：每个 Boredom IdleEvent 都要经过 Event Bus → Dispatcher → Pipeline 路由 → Pipeline 引擎 → metrics 记录。即使 Boredom Pipeline 是空的，也有事件序列化/反序列化、路由匹配、日志记录的开销。
- **如果 Boredom 有操作**（如"随机浏览记忆"），30 次/分钟内的 memory 遍历和随机访问可能造成 I/O 热点
- **Poll、Event、Pipeline 三倍开销**：每 2s = 一次 IdleDetector poll（测量 pending_count + busy_reflecting 标志）+ 一次 IdleEvent 创建 + 一次 Pipeline 调度

🛠 建议：

1. **聊天模式 Boredom 降低频率**：一旦进入 Boredom 深度（depth>=1），poll_interval 可以从 2s 增长到 5-10s（用 Linear 或固定值）。
2. **或聊天模式下 Boredom Pipeline 为纯 no-op**：只记录 metrics，不执行任何操作。ChatMode 下的 Boredom 语义应为"等用户"而非"随便逛逛"。
3. **配置示例建议**：
   ```yaml
   chat_mode:
     poll_interval:
       linear: { base: 2.0, multiplier: 0.5 }  # 2s → 3s → 4s → ...
   ```

---

### R2-6 🎯: `context_isolation.suspend_on_user_input` 无实现

📐 配置：
```yaml
context_isolation:
  pollute_chat_history: false
  suspend_on_user_input: true
```

文档 6.3 说："用户消息到达时，agent 切换到'对话上下文'，之前空闲期间的输出被挂起（不影响当前回复的上下文窗口）"

💥 可能后果：

- **这个"挂起"语义在架构的什么层次落地？**
  - 在 Event Bus 层？——Event Bus 不关心上下文
  - 在 Dispatcher 层？——Dispatcher 路由事件到 Pipeline，不管理上下文
  - 在 Pipeline/Workflow 层？——Pipeline 执行时有上下文，但 Pipeline 实例之间不共享上下文
  - 在 LLM 调用的 Tool Runner 层？——Tool Runner 将 idle 期间的状态注入到 chat 响应中？
- **3 种可能的实现**：
  - **场景 1**：Idle 操作的输出和 Chat 操作的输出使用不同的 memory store（物理隔离）——但文档没这么说
  - **场景 2**：同一 store 但标记 source（idle vs chat），LLM 调用时过滤——但文档没定义标记系统
  - **场景 3**：idle 产出写入暂存区，用户消息到达时清空暂存区——但没有定义暂存区

🛠 建议：

1. **将 `context_isolation` 关联到实际的架构组件**：明确说明这是在 Tool Runner 层的上下文组装逻辑中实现的，还是在 Pipeline 层的 context builder 中实现的。
2. **或定义为"仅 metrics 级别的隔离"**：idle 操作不影响 chat 上下文窗口，不影响 memory——但这与 Sleep 的记忆整理矛盾（Sleep 专门修改 memory）。
3. **在配置注释中增加实现约束**：
   - `pollute_chat_history: false` —— idle 日志不写入对话历史
   - `suspend_on_user_input: true` —— 实现方式：workflow context 为每个可中断状态独立保存，用户事件到达时丢弃 idle context

---

### R2-7 🎯: `allowed_kinds` 与 `enabled_kinds` 交互

📐 配置：
```yaml
personality:
  enabled_kinds: [daze, boredom, sleep, exploration, meditation]
chat_mode:
  allowed_kinds: [daze, boredom]
```

💥 可能后果：

- **交集还是继承？** `allowed_kinds: [daze, waiting]` + `enabled_kinds` 不包含 waiting → waiting 在聊天模式下是否可用？
- **chat_mode 配置缺少深度映射**：如果 `allowed_kinds: [daze]`（只有发呆），IdleDetector 的 `resolve()` 在 depth=1 时找不到合法 kind
- **如果实现者取交集**：`allowed_kinds.intersection(enabled_kinds)` → 预期 OK
- **如果实现者直接替换**：若 `as_personality()` 用 allowed_kinds 覆盖 enabled_kinds 但不改 depth_schedule，某些 kind 产生但被 enabled_kinds 过滤掉，形成半有效的 IdleEvent —→ 资源浪费

🛠 建议：

1. **配置验证逻辑**：在 config 的 validate() 中添加检查——`allowed_kinds` 中的每个 kind 必须在 `enabled_kinds` 中存在，否则配置错误。
2. **`resolve()` 的 fallback 行为**：如果给定深度没有任何合法的 kind，返回 `Daze`（安全的默认状态）。
3. **在配置注释中注明**：`allowed_kinds` 是 `enabled_kinds` 的子集，不会启用父人格未启用的 kind。

---

### R2-8 🎯: Reflection 被抢先时熔断计数不重置

📐 场景：

```
count=4 → QueueDrained(4) → select!
    → wait_for_event() 分支 → 新事件抢先
    → 设置 busy_reflecting=false
    → 不重置 count（count 仍然 =4）
    → 循环继续
    → 新事件被处理
    → 队列空 → QueueDrained 发布
    → count++ → count=5
```

💥 可能后果：

- 一个 Reflection 被抢先，但被抢先的 Reflection 还没有机会产生输出。然而 `reflection_consecutive_count` 没有重置。
- 如果连续的 Reflection 因为高优事件持续被抢先（高负载场景），count 持续上升直到触发熔断——但熔断针对的是"每个 Reflection 都有产出"的场景，不是"Reflection 被抢先"的场景。
- **假阳性熔断**：系统本应正常工作的场景因连续抢先进入了熔断。

🛠 建议：

1. **抢先分支中重置 count 为 0**：因为被抢先的 Reflection 没有产生输出，不应计入连续产出计数。
2. **或使用两种计数器**：`consecutive_with_output` 和 `consecutive_attempted`，熔断只关心前者。

---

## P3 — 细节问题

---

### R2-9 🎯: `reflection.abort()` 在 `busy_reflecting=false` 之后的窗口

📐 select! 的抢先分支：
```rust
_ = self.event_bus.wait_for_event() => {
    coord.busy_reflecting.store(false, Ordering::SeqCst);
    reflection.abort();
}
```

顺序是：先清 `busy_reflecting`，后 abort reflection。

💥 可能后果：

在这个极小窗口（store → abort 之间），IdleDetector 的 poll() 可能执行：
1. 读取 `busy_reflecting == false` → 认为 Reflection 已结束
2. 读取 `pending_count() == 0`（新事件已入队但 IdleDetector 的 snapshot 可能还没看到）

→ 产生一个不必要的 IdleEvent。这个事件在队列中，稍后被 Dispatcher 处理。

🛠 建议：

交换顺序：先 abort，再清标志。这样 store(false) 时 Reflection 至少已被标记为取消，IdleDetector 在 abort 完成后才会看到 false。

---

### R2-10 🎯: Reflection 5 次阈值要向 pipeline 传递指令

📐 文档 4.5 节：
```
consecutive_count == max_consecutive (5)
    → Reflection 跳过 lessons_learned（只查 chain_tasks + immediate_errors）
```

💥 可能后果：

`reflection_consecutive_count` 在 QueueDrained 事件中携带，但 pipeline 需要识别该数值并动态决定执行哪些 check_items。设计没有说明：
- Pipeline 如何接收这个信息（通过事件 metadata？）
- Pipeline 如何根据 count 跳过 lessons_learned（条件 branch？）
- check_items 与 count 阈值的映射表在哪里定义

🛠 建议：

1. 在 `QueueDrained` 或事件 metadata 中明确定义 count 字段
2. 在 Route 配置或 Pipeline 定义中添加 count 条件路由的注释
3. 最简单实现：Reflection Pipeline 的第一个 step 根据 `event.reflection_consecutive_count` 决定跳过哪些检查

---

### R2-11 🎯: `wait_for_event()` 假唤醒风险

📐 Dispatcher 用 `select!` 同时等待 Reflection 完成和新事件到达。`wait_for_event()` 在 Event Bus 层实现为通道通知。

💥 可能后果：

如果 `wait_for_event()` 的实现有假唤醒（spurious wakeup，如 `Condvar` 或 `mpsc::Receiver` 的虚假通知）：
- 没有真实事件到达，但 `wait_for_event()` 返回
- `busy_reflecting = false`，`reflection.abort()`
- 循环继续 → `try_dequeue()` → None
- 进入 None 分支 → `recently_processed_real_event == false` → sleep(100ms)

结果：Reflection 被无辜取消，连锁任务的链条断裂。用户不会感知（因为无状态改变），但潜在业务逻辑丢失。

如果 Reflection 是 `chain_tasks`（检查是否有连锁任务），而这次假唤醒刚好取消了正确的检查——连锁任务不会被触发。

🛠 建议：

1. `wait_for_event()` 应保证：只有队列从空变为非空时触发通知，不产生假唤醒。
2. 或在 select! 中添加 `event_bus.pending_count() > 0` 的二次确认：
   ```rust
   _ = self.event_bus.wait_for_event() => {
       if self.event_bus.pending_count() == 0 {
           continue;  // 假唤醒，继续等待 Reflection
       }
       coord.busy_reflecting.store(false, Ordering::SeqCst);
       reflection.abort();
   }
   ```

---

## 第三部分：跨发现关联风险

### 联动故障场景：聊天→完整人格切换时的深度空闲跳跃 + 不可打断 Workflow

如果 R2-1（last_event_from_chat 传播断裂）、R2-4（depth 跨人格不重置）、R2-2（Workflow 不可打断）同时存在于最终实现中：

1. ChatMode 从未激活（R2-1）——每次对话间隙 agent 使用完整人格
2. 3 次对话轮次，每次间隙 IdleDetector 累积 depth=6
3. Sleep Workflow 启动（depth=3）——正在整理记忆
4. 用户第 4 条消息到达——真实事件入队
5. Dispatcher 处理用户消息（R2-2）——Sleep 仍在后台运行
6. Sleep 修改了 memory 中的分类索引——与用户消息处理的 agent 状态冲突
7. 用户看到不一致的回复

---

## 汇总

| 类别 | 数量 | 严重性分布 |
|------|------|-----------|
| **R1 修复验证** | 11 项 | 9 ✅ 正确, 2 ⚠️ 结构对但实现断裂 |
| **R2 新发现** | 11 项 | 2×P1, 6×P2, 3×P3 |

**R1→R2 的整体评估**：文档团队对 R1 11 项发现做了认真的结构修复。最关键的进展是 chat 场景感知架构（ChatMode、ContextIsolation、grace_period）。但修复集中在类型系统层面，**事件传播路径和运行时中断机制** 仍有两个断裂点：

1. **R2-1 🚨（传播断裂）**：`last_event_from_chat` 如何从 Dispatcher 到达 IdleDetector？没有定义→ChatMode 无法激活。
2. **R2-2 🚨（中断断裂）**：真实事件到达后，正在运行的 idle Workflow 如何被打断？没有定义→数据竞争风险。

建议优先级：
- **在实现前**：先关闭 R2-1 和 R2-2 这两个断裂点——它们的修复不改变类型系统（无需新增 struct/enum），只需在伪代码和架构图中加入传播/中断路径
- **在实现过程中**：处理 P2 项（R2-3 到 R2-8），多数是配置验证和决策澄清
- **在实现后review**：关注 P3 项（R2-9 到 R2-11），细节边界问题

---

*审计人：业务逻辑审计器 R2*
*审计范围：R1 修复验证 + 新盲区发现*
*审计日期：2026-05-16*
