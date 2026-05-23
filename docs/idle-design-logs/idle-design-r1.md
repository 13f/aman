# Idle State System — 业务逻辑审计 R1

> 审计目标：idle-design.md 中的"chat"设计——即事件流、状态转换、与真实对话的交互路径。
> 审计范围：事件时钟竞态、状态机盲区、聊天场景适应性、资源生命周期、回滚补偿。
> 审计方法：业务逻辑审计清单 + 边界案例推理。

---

## 按严重性排列

| # | 严重性 | 关注点 | 核心问题 |
|---|--------|--------|---------|
| 1 | P0 💥 | IdleDetector 与 Reflection 的时序竞态 | Reflection 执行期间队列为空，IdleDetector 误判为空闲并产生 IdleEvent |
| 2 | P1 🚨 | Reflection 不可被真实事件打断 | 高优先级事件被 Reflection（最长 60s）阻塞，与打断策略矩阵矛盾 |
| 3 | P1 🚨 | 聊天场景空闲误触发 | 每次 Chat Turn 后队列短暂清空，触发完整空闲序列，造成资源浪费与行为抖动 |
| 4 | P2 ⚠️ | Exploration API 配额作用域未定义 | quota 在周期性/会话级/全局作用域未确定，可能导致超配或永不消耗 |
| 5 | P2 ⚠️ | deep_sleep 与 depth_schedule 交互未定义 | deep 深度下 idle kind 是继续执行 schedule 还是被 deep_sleep 覆盖 |
| 6 | P2 ⚠️ | Arousal 在深度空闲中的不合理衰减 | 执行探索/冥想等高 CPU 活动时 arousal 依然衰减，唤醒后 agent 异常被动 |
| 7 | P2 ⚠️ | Reflection 连锁任务无限循环缺少熔断 | 若每个事件都有 lessons_learned，Reflection 持续产出新事件，agent 永不空闲 |
| 8 | P2 ⚠️ | Incubation 后台线程生命周期未管理 | 关闭/重启时的线程泄漏与共享状态竞争 |
| 9 | P2 ⚠️ | 多轮对话中空闲中断语义未定义 | 用户正在打字但消息未发出时，agent 进入空闲并开始探索，用户回来发现上下文变了 |
| 10 | P3 🔍 | IdleContext.last_idle_output 单槽丢失 | .take() 丢弃上一轮输出，深度空闲时跨 tick 信息不累积 |
| 11 | P3 🔍 | Meditation 中断导致不完整报告文件 | 不保存策略使 shutdown 时写一半的文件残留 |

---

## P0 — 必须修

---

### 🎯 关注点 1：IdleDetector 在 Reflection 执行期间误判为空闲

📐 场景：

Dispatcher 从 Event Bus 取出 QueueDrained 事件，开始执行 Reflection Pipeline（异步，有 60s timeout）。此时 Event Bus 队列为空（QueueDrained 已被取出）。IdleDetector 按 poll_interval 执行 poll() → 看到 `pending_count() == 0` → 认为队列空 → 产生 IdleEvent(kind=Daze, depth=0) 注入 Event Bus → depth 自增。

Reflection 完成后，Dispatcher 回到循环顶部，看到队列中已有 IdleEvent（由 IdleDetector 在 Reflection 期间产生的）。于是 Dispatcher 先处理 IdleEvent 而非从 Daze 开始真正的空闲序列。

💥 可能后果：

- **空闲序列提前开始**：IdleEvent 在 Reflection 完成前就入队。Reflection 产出连锁任务时，队列中同时有 IdleEvent + 真实事件，路由顺序不确定。
- **depth 在 Reflection 期间递增**：如果 Reflection 执行了多个 poll 周期（poll_interval=5s, Reflection_timeout=60s → 最多 12 次 poll），IdleDetector 的 depth 在 Reflection 期间从 0 增长到 12。Reflection 完成后 IdleDetector 直接产生 Boredom/Sleep 级别的事件，跳过了 Daze。
- **Reflection 产出连锁任务后，IdleEvent 已入队**：反射产出的真实事件和提前产生的 IdleEvent 同时竞争，可能造成 IdleEvent 在真实事件之后被处理，而真实事件的处理又触发新一轮 QueueDrained，使空闲序列一直无法正确启动。

🛠 建议：

IdleDetector 需要知道"Reflection 是否正在进行"。方案：

1. **显式信号**：Dispatcher 在开始 Reflection 前设置一个 `reflection_in_progress` 标志（AtomicBool）。IdleDetector 的 poll() 检查此标志，若为 true 则跳过本轮产生事件（只更新计时）。Reflection 完成后清除标志。
2. **或 Event Bus 标记**：让 Event Bus 能区分"队列真正空"和"队列空但 Reflection 在跑"。
3. **时序约束**：IdleDetector 的首次 poll 必须在 Reflection 的 `OnComplete` 回调之后触发，而非基于固定时钟间隔。

---

## P1 — 强烈建议修

---

### 🎯 关注点 2：Reflection 不可被打断

📐 场景：

Dispatcher 的 `run_loop` 伪代码中：
```rust
self.dispatch(event).await;  // 阻塞等待 Pipeline 完成
```
当 event 是 QueueDrained 时，`self.dispatch(event).await` 执行 Reflection Pipeline（最长 60s）。此时：
1. 外部 Source 产生了一个高优先级真实事件（例如用户消息或监控告警），入队 Event Bus
2. 但 Dispatcher 阻塞在 Reflection 的 pipeline 执行上，不会去取出新事件
3. 该高优先级事件在队列中等待，最多 60s 后才被处理

💥 可能后果：

- **打断策略矩阵声称**"打断损失：无"——实际上 Reflection **完全不可打断**
- 用户消息延迟最多 60s 才被响应（如果 Reflection timeout=60s）
- 监控告警等时效性事件可能被延迟到失去意义
- 这个延迟的累积效应：如果 agent 频繁处理短任务（每次触发 Reflection），用户的每个消息都可能被拖延

🛠 建议：

1. **Reflection 应可被真实事件打断**：Dispatcher 处理 QueueDrained 时，应该使用 `select!` 或类似机制同时等待 Reflection 完成和新的入队信号：
   ```rust
   select! {
       _ = reflection_pipeline.run() => { /* Reflection 完成 */ }
       _ = event_bus.wait_for_event() => { /* 新事件到达，中断 Reflection */ }
   }
   ```
2. **Reflection 应做成分段式**：每 1-2 秒检查一次队列，如果有新事件到达则提前结束 Reflection。
3. **或 timeout 大幅度缩短**：在聊天场景下，Reflection 的 60s timeout 不合理，应为 5-10s 甚至更短。

---

### 🎯 关注点 3：聊天场景中的空闲误触发

📐 场景：

aman 是"万物皆事件"框架。一次典型的多轮对话：
1. 用户消息 → 事件 A 入队 → Dispatcher 处理 A
2. Agent 回复（可能产生多个内部事件）
3. 队列清空 → Dispatcher 产生 QueueDrained → Reflection
4. Reflection 完成，无连锁任务
5. IdleDetector 开始空闲序列：Daze(5s) → Boredom(10s) → Sleep(15s) → ...
6. 用户开始打字（还未发送），15-30s 无消息
7. IdleDetector 到达 Exploration depth → agent 开始主动搜索外部 API
8. 用户发送下一条消息 → 打断空闲 → agent 从探索中切换回来

💥 可能后果：

- **用户感知到"agent 在后台做事"**：如果 exploration 调用了外部 API 或改变了 agent 状态，当用户下一条消息到来时，agent 的上下文可能"偏离"了对话
- **资源浪费**：每次对话轮次之间都走一次 idle 序列。如果对话密集（每 2 分钟一条消息），agent 在"启动空闲→被打断→启动空闲→被打断"中循环，CPU 在 Daze/Boredom/Sleep 的切换中浪费
- **状态抖动**：Sleep 的记忆整理如果每次只整理了部分内容就被打断（损失"中"，WAL 写出），频繁的 checkpoint 读写增加 I/O
- **用户感知不一致**：A 场景（3 秒内回复）和 B 场景（30 秒后回复）下，agent 的行为状态不同——B 场景 agent 可能已经进入了探索模式

🛠 建议：

1. **对话空闲 vs 系统空闲分离**：引入 `ChatIdleTimeout`——在最近一次人机交互后的 N 秒内（建议 60s），IdleDetector 只允许 Daze/Boredom，不允许 Exploration/Sleep/Meditation 等有副作用的空闲状态。因为用户大概率会继续对话。
2. **对话窗口标志**：当有 Chat Source 连接时，IdlePersonality 应自动切换到一个"对话友好"配置，禁用 Exploration/Meditation 等高干扰状态。只有当 Chat Session 断开后才启用完整空闲序列。
3. **Boredom 在聊天场景中改为纯记录**：不执行外部操作，只记录"已有 X 秒无消息，记录时间戳到 context"。
4. **空闲深度在聊天场景下使用不同的 poll_interval**：建议 chat 场景的 poll_interval 更短（1-2s），但限制可进入的 idle kind。

---

## P2 — 建议修/可延迟

---

### 🎯 关注点 4：Exploration API 配额作用域未定义

📐 配置中：
```yaml
exploration:
    api_rate_limit: 10
```
未定义：**10 每什么？**

可能的解读：
- 10 请求 / 每秒 → 太宽松
- 10 请求 / 每分钟 → 合理但未写
- 10 请求 / 每次 Exploration cycle → 如果 exploration 持续 5 轮（每轮 60s），每轮 10 次
- 10 请求 / 整个 agent 生命周期 → 太严格

💥 可能后果：

- **实现时选择了错误作用域**：比如实现为"每 tick 10 次"，如果 poll_interval 很短，API 配额瞬间耗尽
- **如果配额是全局（agent 级别）**：多个 Exploration cycle 间共用一个计数器，第一次探索就用完了所有配额，后面的探索饥饿
- **如果配额定在 idle 模块内**：而别的模块也调用了同一个外部 API（比如主动搜索），两个配额互不影响，但实际上超过 API 提供者的限制
- **跨 idle 周期的配额无重置逻辑**：如果 quota 按周期重置，没有定义周期长度

🛠 建议：

1. 明确定义 `api_rate_limit` 的时间窗口：`api_rate_per_minute: 10` 或 `api_rate_per_session: 50`
2. 明确 quota 的 scope：per idle cycle / per agent instance / per time window
3. 考虑全局共享配额池：将配额计数器放在 ArousalTracker 或一个独立的 RateLimiter 模块中，所有模块共享
4. 超出配额的 behavior：静默 fallback 到 Boredom？记录日志？通知 operator？

---

### 🎯 关注点 5：deep_sleep 与 depth_schedule 的交互未定义

📐 配置示例：
```yaml
depth_schedule:
  - [1, boredom]
  - [3, sleep]
  - [5, exploration]
deep_sleep:
  depth_threshold: 15
  interval_secs: 60
```

💥 可能后果：

当 depth=20 时：
- idle_kind 是什么？继续按 depth_schedule 返回 Exploration（最后一个匹配）？
- 还是 deep_sleep 覆盖了 kind，实际上 agent 在"深度睡眠"？
- 如果 poll_interval 切换到 60s，但每 tick 仍然产生 Exploration 事件，探索间隔变成了 60s 一次——这是设计意图吗？
- 如果 Exploration 有断点续传（进度保存），60s 的间隔导致每次 resume 的开销占比很高

🛠 建议：

1. 明确 `deep_sleep` 是否为 idle_kind 的覆盖：如果是，它应该出现在 `enabled_kinds` 中；如果不是，它只是 poll 频率的优化。
2. 如果 deep_sleep 只是频率调整，重命名以免暗示"睡眠状态"：如 `poll_relaxation` 或 `extended_poll_interval`。
3. 或者，将 deep_sleep 设计为 IdleKind::Waiting 的一个特殊子模式，语义清晰。

---

### 🎯 关注点 6：Arousal 在深度空闲中的不合理衰减

📐 配置：
```yaml
arousal:
    initial_value: 1.0
    half_life_secs: 900   # 15 分钟
    boredom_threshold: 0.3
```

在 deep idle 状态下（depth >= 5），agent 正在执行 Exploration（高 CPU/IO，主动搜索外部信息）或 Meditation（中 CPU，内省处理）。但 arousal 仍然以 15min 半衰期衰减。

💥 可能后果：

- 假设 Exploration 运行了 30 分钟（depth 深度增长，60s interval）。此时 arousal = 1.0 * e^(-1800/900*ln2) ≈ 0.25
- 一个真实事件到达，打断空闲。但 arousal=0.25 < 0.3 阈值 → agent 进入"低 arousal"模式
- 但实际上 agent 刚才在 Exploration 中非常活跃——arousal 应该是高的。arousal 错误地反映了 agent 的状态

🛠 建议：

1. **Engaging idle states 应减缓或暂停 arousal 衰减**：当 agent 在 Exploration/Meditation/Incubation 中时，arousal 衰减速率应乘以一个系数（如 0.1 或完全暂停）
2. **或者 arousal 在 idle 操作中反向增长**：如果 Exploration 产出的结果有实质收获，arousal 应该上升
3. **Boredom 和 Daze 才应该正常衰减**：只有"真正的空闲"才降 arousal

---

### 🎯 关注点 7：Reflection 连锁任务无限循环缺少熔断

📐 文档 says：
> 若 Reflection 每次都有产出且每次都 > 0，说明业务逻辑本身在自产自消——属于正常行为

💥 可能后果：

考虑一个设计不良（或过于勤快）的 Reflection Pipeline：
- 事件 A → Reflection → 产出"检查 A 的影响范围 → 产出事件 B"
- 事件 B → Reflection → 产出"记录 B 的处理经验 → 产出事件 C"
- 事件 C → Reflection → 产出"将经验同步到 memory → 产出事件 D"
- ...无限循环，agent 永远不进入 idle

**即使每个事件本身合理**，这种"每步都复盘"的模式在高吞吐场景下可能形成活锁。尤其是当 check_items 包含 `lessons_learned` 时——几乎所有处理都有值得记录的东西。

🛠 建议：

1. **Reflection Cycle Counter**：在 `IdleContext` 或 Dispatcher 中记录 Reflection 连续触发次数。超过阈值（如 5 次）后，Reflection 跳过 lessons_learned，只检查 chain_tasks 和 immediate_errors。
2. **Minimum Idle Interval**：强制要求每次真实事件处理后，至少经过 N 秒（可配置，建议 3-5s）才能触发下一次 Reflection。在此期间的事件处理不触发 QueueDrained。
3. **或引入"热度"概念**：事件越密集，Reflection 的检查项越少。高频场景下只检查 immediate_errors。

---

### 🎯 关注点 8：Incubation 后台线程生命周期未管理

📐 打断策略：
```
Incubation | IdleDetector | 后台线程继续运行，不打断
```

💥 可能后果：

- **关闭时线程泄漏**：Phase 4.5 关闭时，IdleDetector 停止，但 Incubation 的后台线程被"不打断"原则保留。这些线程访问的共享状态（`Event Bus`、`memory`、`agent_state`）可能已被析构，造成 use-after-free 或 panic。
- **热重载/配置变更时**：如果 operator 修改了 idle 配置并 reload，旧的 Incubation 线程还在运行，新的配置又可能启动新的线程，造成重复。
- **线程 panic 隔离**：后台线程如果 panic，错误难以追踪，且可能留下不一致的中间状态。

🛠 建议：

1. **后台线程必须加入生命周期管理**：使用一个 `CancellationToken` 或类似的协作式取消机制。shutdown/热重载时通知所有后台线程。
2. **Incubation 应被视为"有进度"的空闲状态**：至少需要保存关联状态列表的 checkpoint。
3. **线程计数上限**：最多允许 N 个并发 Incubation 线程（建议 N=1 或 2），防止多个 idle cycle 累积线程。

---

### 🎯 关注点 9：多轮对话中空闲中断语义未定义

📐 当前设计假定"事件到达 → 打断空闲 → 回到 Active"。但在真实的聊天场景中：

- 用户正在打字（输入中），但此状态对 agent 不可见
- Event Bus 队列为空 → IdleDetector 正常启动空闲序列
- 用户完成打字、发送消息 → 事件到达 → 打断空闲

💥 可能后果：

- **用户感知不一致**：如果 agent 在空闲期间修改了自己的 memory 或状态（如在 Sleep 中整理了记忆），用户可能发现前一个对话的"记忆"变了
- **Exploration 可能携带对话上下文**：如果 Exploration 在空闲期间调用了外部工具，而工具调用链中混入了未清空的上下文，可能产生 LLM 幻觉
- **用户回来后上下文偏移**：agent 回应用户时，其"当前状态"包含了空闲期间的操作日志，导致回复中"我刚刚在搜索关于 X 的信息，现在你问的是 Y"——不必要的上下文混杂

🛠 建议：

1. **对话上下文隔离**：Chat Source 产生的对话上下文与 Idle 操作的上下文应严格隔离。当真实事件（用户消息）到达时，agent 应"挂起"空闲上下文，切换到对话上下文。
2. **空闲操作不污染对话历史**：Idle 期间的操作不应自动进入对话历史，除非 Reflection 或 Exploration 产出与当前对话相关的信息。
3. **"正在输入"信号**：如果 Chat Source 支持 typing indicator（Telegram 支持），可以在用户开始打字时设置一个"user_active"标志，抑制深度空闲。

---

## P3 — 低优先级，但值得注意

---

### 🎯 关注点 10：IdleContext.last_idle_output 单槽设计丢失信息

📐 `IdleContext` 中：
```rust
pub last_idle_output: Option<String>,  // take() 消费
```

`IdleDetector.poll()` 中用了 `.take()`，意味着：
- tick N 的 output → 被 tick N+1 消费后丢弃
- tick N+1 的 output 覆盖同一槽位

💥 可能后果：

深度空闲场景下，如果 Boredom 连续运行 3 个 tick（depth 1-2，每个 tick 产生随机漫游输出），只有最后一个 tick 的 output 能传递到下一个状态（Sleep/Exploration）。中间的状态变化信息丢失。

🛠 建议：

改为 `Vec<String>`（积累最近 N 个产出）或 `LimitedQueue<String>`（固定容量环形缓冲），让后续的空闲状态能够看到更丰富的上下文。

---

### 🎯 关注点 11：Meditation 中断导致不完整报告文件

📐 打断策略：
```
Meditation | IdleDetector | 高 | 不保存 | 丢弃，下次空闲重新触发
```

`meditation.report_path: "~/.aman/narrative/meditation/"` 目标路径存在文件写入风险。

💥 可能后果：

- 如果 Meditation 在写入报告文件的中途被中断，磁盘上留下一个不完整/损坏的 JSON/YAML 文件。
- 下次 Meditation 启动时，可能读取（或追加到）这个不完整文件，造成解析错误或重复内容。
- 如果 operator 用外部工具查看 `report_path/` 目录，可能看到"当前"报告不完整。

🛠 建议：

1. **写时使用临时文件 + rename**：写入至 `.tmp` 文件，完成后原子 rename 为目标文件名。
2. **或检查中断场景**：在 shutdown 的 Phase 4.5 中，给正在执行的 Meditation 一个"优雅完成"的机会（如 5s 超时），而不是直接丢弃。

---

## 汇总

| 严重性 | 数量 | 立即行动 | 
|--------|------|---------|
| P0 💥 | 1 | 必须修：IdleDetector-Reflection 时序竞态 |
| P1 🚨 | 2 | 强建议：Reflection 不可打断 + 聊天场景空闲误触发 |
| P2 ⚠️ | 6 | 建议修：配额、深度交互、Arousal、熔断、线程、对话隔离 |
| P3 🔍 | 2 | 留意 |

**最需要产品决策的：P1#3（聊天场景空闲策略）** — 这决定了 aman 作为一个"事件响应式 Agent"在实际聊天使用中是否可用。如果每次对话轮次之间都走完整空闲序列，用户会感觉到 agent 行为不一致。建议将"聊天情境感知"作为 idle 系统的一个设计约束（非功能需求）明确写入。

---

*审计人：业务逻辑审计器 R1*
*审计范围：Idle 系统的事件流/状态转换/聊天交互路径*
*审计日期：2026-05-16*
