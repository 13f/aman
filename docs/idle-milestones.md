# Idle State System — 开发里程碑与任务拆分

> 基于 `/Users/jerin/projects/aman/docs/idle-design.md`（R8 per-agent-idle，设计成熟度 ★★★★★）
> 扩展自 `/Users/jerin/projects/aman/docs/agent-design.md` §3.4 Event Dispatcher — Reflection 部分
> 每个里程碑有明确的可交付物和验收标准，开发者可直接领取任务。
> 架构引用格式：`§章节` 指向 idle-design.md 对应章节。

---

## 总体进度

**设计阶段**：R8 per-agent-idle 收敛 ✅ (2025-05-22)

| 里程碑 | 进度 | 任务数 | 已完成 | 估时 |
|--------|------|--------|--------|------|
| M1 核心类型系统 | **100%** | 5 | 5/5 | 5 天 |
| M2 协调与配置 | **100%** | 3 | 3/3 | 3 天 |
| M3 Event Bus 增强 | **100%** | 3 | 3/3 | 2 天 |
| M4 Dispatcher 改造 | **100%** | 6 | 6/6 | 6 天 |
| M5 IdleDetector | **100%** | 4 | 4/4 | 4 天 |
| M6 空闲 Workflow | **100%** | 6 | 6/6 | 5 天 |
| M7 生命周期集成 | **100%** | 3 | 3/3 | 3 天 |
| M8 上下文隔离与指标 | **100%** | 3 | 3/3 | 3 天 |
| **合计** | **100%** | **33** | **33/33** | **31 天** |

**当前阶段**：全部里程碑已完成 ✅

---

## 依赖关系总览

```
M1 核心类型 ──┬── M2 协调与配置 ──┐
              │                    │
              └── M3 Event Bus ────┤
                                   │
              ┌────────────────────┘
              ▼
        M4 Dispatcher 改造 ──┐
              │               │
              ▼               ▼
        M5 IdleDetector   M6 空闲 Workflow
              │               │
              └───────┬───────┘
                      ▼
              M7 生命周期集成
                      │
                      ▼
              M8 上下文隔离与指标
```

- M1/M2 部分并行（M2 需要 M1 的类型定义稳定后可开始）
- M3 依赖 M1（需要 IdleEvent 类型）
- M4/M5/M6 可部分并行（都需要 M1+M2+M3，但互不阻塞）
- M7 必须在 M4/M5/M6 全部完成后开始
- M8 依赖 M7 的集成点

---

## M1：核心类型系统（5 天）

> 目标：创建 `crates/idle/` 新 crate，实现 §3 全部类型定义。所有类型经序列化/反序列化测试。
> 验收：`cargo check -p idle` 通过，所有类型可正确 ser/de。

### [x] T1.1 — 创建 idle crate 骨架

| 属性 | 内容 |
|------|------|
| 估时 | 0.5 天 |
| 涉及 | 新建 `crates/idle/`（Cargo.toml, src/lib.rs, src/types.rs 等） |
| 架构 | §7.1 Crate Assignment |

**子任务：**
1. 创建 `crates/idle/Cargo.toml`，依赖 `core`、`serde`、`tokio`
2. 在 workspace `Cargo.toml` 中注册 `idle` crate
3. 创建模块骨架：`types.rs`、`detector.rs`、`personality.rs`、`coordination.rs`、`workflow.rs`、`arousal.rs`、`incubation.rs`、`config.rs`
4. `lib.rs` 中声明所有模块并 re-export 公共类型
5. 添加 `#![forbid(unsafe_code)]`

**验收：**
- `cargo check -p idle` 通过
- 模块骨架编译无警告

---

### [x] T1.2 — 实现 IdleKind + ArousalBehavior

| 属性 | 内容 |
|------|------|
| 估时 | 1 天 |
| 涉及 | `crates/idle/src/types.rs` |
| 架构 | §3.1 IdleKind — 七种深度驱动空闲子类型 |

**子任务：**
1. 实现 `IdleKind` 枚举：Daze, Boredom, Sleep, Exploration, Meditation, Waiting, Incubation（7 个变体）
2. 每个变体需 `#[serde(rename_all = "snake_case")]`
3. 实现 `ArousalBehavior` 枚举：Passive, Engaged { decay_multiplier: f64 }
4. 实现 `IdleKind::arousal_behavior()` 方法：
   - Daze/Boredom/Waiting → Passive
   - Sleep → Engaged(0.5)
   - Exploration/Meditation → Engaged(0.0)
   - Incubation → Engaged(0.1)
5. 单元测试验证所有 7 种映射

**验收：**
- `cargo test -p idle` 通过
- `serde_json::from_str::<IdleKind>("\"sleep\"")` → IdleKind::Sleep
- 序列化往返不丢信息

---

### [x] T1.3 — 实现 IdleEvent + IdleContext + QueueDrained

| 属性 | 内容 |
|------|------|
| 估时 | 1 天 |
| 涉及 | `crates/idle/src/types.rs` |
| 架构 | §3.2 IdleEvent、§3.3 QueueDrained |

**子任务：**
1. 实现 `IdleEvent` 结构体：kind, depth, duration_secs, context: Option<IdleContext>, from_chat_mode: bool
2. 实现 `IdleContext` 结构体：last_event_type, last_idle_outputs: Vec<String>（定容 Vec 语义需文档化）, arousal_level
3. 实现 `QueueDrained` 结构体：last_event_type, last_trace_id, last_result_summary, arousal_level, reflection_consecutive_count
4. 为 `IdleEvent` 和 `QueueDrained` 实现 `Into<Event>` 转换
5. 在 `core` crate 的 `EventKind` 中新增 `Idle` 和 `QueueDrained` 变体常量：
   - `EventKind::IDLE` → `"idle"`
   - `EventKind::QUEUE_DRAINED` → `"system.queue_drained"`
6. 为 `Event` 实现 `is_queue_drained()` 和 `is_idle_event()` 方法
7. 为 `Event` 实现 `is_from_external_source()` 方法（判断 source 是否为外部 EventSource 而非内部连锁任务）

**验收：**
- `QueueDrained` JSON 字段正确序列化（camelCase）
- `IdleEvent.into()` 产生的 `Event` 具有 `priority = Low`
- `Event::is_from_external_source()` 对外部 Source 返回 true，对内部连锁任务返回 false

---

### [x] T1.4 — 实现 IdlePersonality + ChatMode + 辅助类型

| 属性 | 内容 |
|------|------|
| 估时 | 1.5 天 |
| 涉及 | `crates/idle/src/types.rs`、`crates/idle/src/personality.rs` |
| 架构 | §3.4 IdlePersonality、§3.5 IdleCoordination |

**子任务：**
1. 实现 `IdlePersonality` 结构体：enabled_kinds, depth_schedule, poll_interval, poll_relaxation, chat_mode, reflection_breaker, context_isolation
2. 实现 `ChatMode` 结构体：allowed_kinds, grace_period_secs, poll_interval
3. 实现 `ChatMode::as_personality()` 方法：
   - depth_schedule 固定为 [(0, Daze), (1, Boredom)]
   - enabled_kinds 取 allowed_kinds
   - context_isolation.pollute_chat_history 强制为 false
   - resolve() fallback = Daze
4. 实现 `ReflectionBreaker` + `PollRelaxation` + `ContextIsolation` + `PollInterval` 辅助类型
5. 实现 `PollInterval` 的 `next_delay()` 方法（Linear: base + multiplier * depth）
6. 实现 `IdlePersonality::resolve()` 方法：给定 depth 返回 IdleKind（无匹配→Daze）

**验收：**
- `ChatMode.as_personality()` 产生的 personality 深度≥1 时始终返回 Boredom
- `resolve()` 对 depth_schedule 中未定义的 depth 返回 Daze
- `Linear(2.0, 0.5)` 在 depth=5 时产生 4.5s 延迟

---

### [x] T1.5 — 扩展 core crate（SourceType + Event）

| 属性 | 内容 |
|------|------|
| 估时 | 1 天 |
| 涉及 | `crates/core/src/event.rs`、`crates/core/src/source.rs` |
| 架构 | §3.5 IdleCoordination（依赖 SourceType 扩展）、§7.2 core 变更 |

**子任务：**
1. 为 `SourceType` 实现 `to_u8()` 和 `from_u8()` 方法（用于 AtomicU8 存储）
2. 为 `SourceType` 实现 `is_chat()` 方法——Chat Source 返回 true
3. 在 `Event` 上新增 `is_from_external_source()` 方法（由 T1.3 的 source 判断逻辑定义）
4. 确保 `SourceType` 的序号稳定（新增变体追加到末尾，不重排）
5. 添加 `SourceType::Unknown` 变体作为默认值（= 0）

**验收：**
- `SourceType::Chat.to_u8()` 返回固定值
- `SourceType::from_u8(n)` 对未知值返回 Unknown
- `SourceType::Unknown.is_chat()` → false
- `cargo check -p core` 无警告

---

## M2：协调状态与配置系统（3 天）

> 目标：IdleCoordination 跨组件共享状态 + IdleConfig 配置段完整，验证规则就绪。
> 验收：YAML 配置加载并验证通过；IdleCoordination 所有原子操作正确。

### [ ] T2.1 — 实现 IdleCoordination

| 属性 | 内容 |
|------|------|
| 估时 | 1 天 |
| 涉及 | `crates/idle/src/coordination.rs` |
| 架构 | §3.5 IdleCoordination |

**子任务：**
1. 实现 `IdleCoordination` 结构体：
   - `busy_reflecting: Arc<AtomicBool>`
   - `arousal: Arc<ArousalTracker>`
   - `last_source_type: Arc<AtomicU8>`（初始=Unknown）
   - `idle_cancel_token: Arc<RwLock<CancellationToken>>`
   - `real_event_seen: Arc<AtomicBool>`
2. 实现 `IdleCoordination::new()` 构造函数
3. 实现 `reset_idle_signal()` 方法（§3.5 末尾伪代码）：
   - ① `real_event_seen.store(true, SeqCst)`
   - ② 获取写锁 → `cancel()` 旧 token → 替换新 `CancellationToken`
4. 单元测试：`reset_idle_signal()` 后 real_event_seen=true，旧 token 已取消

**验收：**
- `IdleCoordination::new()` 返回的各 Arc 可安全 clone 到多个组件
- `reset_idle_signal()` 调用后 idle_cancel_token 被替换为新 token
- Clippy 无警告

---

### [ ] T2.2 — 实现 ArousalTracker

| 属性 | 内容 |
|------|------|
| 估时 | 1 天 |
| 涉及 | `crates/idle/src/arousal.rs` |
| 架构 | §3.1 IdleKind（ArousalBehavior）、§8 配置（arousal section） |

**子任务：**
1. 实现 `ArousalTracker` 结构体：current_value, half_life_secs, last_update
2. 实现 `current()` 方法：基于时间衰减计算当前 arousal 值（指数衰减模型）
3. 实现 `apply_behavior(ArousalBehavior)` 方法：
   - Passive → 标准衰减（decay_multiplier = 1.0）
   - Engaged(d) → 衰减乘以 d（0.0=不衰减，0.5=半速衰减）
4. 实现 `reset()` 方法：恢复到初始值
5. 单元测试：验证 Engaged(0.0) 不衰减、Engaged(0.5) 半速衰减

**验收：**
- 半衰期 900s → 900s 后剩余约 0.5
- Engaged(0.0) 持续 1000s 后值不变
- 多次 Passive 调用累计衰减正确

---

### [ ] T2.3 — 实现 IdleConfig + 配置验证

| 属性 | 内容 |
|------|------|
| 估时 | 1.5 天 |
| 涉及 | `crates/idle/src/config.rs`、`crates/config/src/` |
| 架构 | §8 Configuration Surface、§4.3 默认配置 |

**子任务：**
1. 在 `crates/config/` 中新增 `IdleConfig` section：
   - `enabled: bool`
   - `reflection: ReflectionConfig`（enabled, timeout_secs, check_items）
   - `personality: IdlePersonality`（完整配置结构）
   - `arousal: ArousalConfig`（initial_value, half_life_secs）
   - `sleep/exploration/meditation/incubation` 子配置段
   - `context: IdleContextConfig`（max_output_buffer）
2. 实现配置验证规则（在 `AgentConfig::validate()` 中）：
   - `allowed_kinds ⊆ enabled_kinds`（不在 enabled 中的 kind 拒绝加载）
   - `reflection_breaker.max_consecutive >= 1`
   - `poll_interval` 配置合法
3. 实现默认配置（与 §4.3 默认值一致）
4. 添加配置文档化注释

**验收：**
- YAML 配置中 `allowed_kinds: [exploration]` 但 `enabled_kinds: [daze]` → 验证失败
- 缺少 `idle` section → 使用默认配置
- `cargo test -p config` 包含 idle 配置测试

---

## M3：Event Bus 空闲增强（2 天）

> 目标：Event Bus 支持 `wait_for_event()` 边沿触发 + IdleEvent 溢出不持久化。
> 验收：wait_for_event 无假唤醒；IdleEvent 在背压溢出时被丢弃而非持久化。

### [x] T3.1 — 实现 wait_for_event() + 假唤醒防护

| 属性 | 内容 |
|------|------|
| 估时 | 1 天 |
| 涉及 | `crates/event-bus/src/lib.rs`（或 `bus.rs`） |
| 架构 | §5.2 Dispatcher 主循环（依赖 wait_for_event）、§9.3 层级6 |

**子任务：**
1. 实现 `EventBus::wait_for_event()` 方法：
   - 返回一个 Future，在队列从空→非空时 resolve
   - 边沿触发（edge-triggered）：只在状态变化时通知，非电平触发
   - 使用 `tokio::sync::Notify` 或类似机制
2. 实现 `EventBus::pending_count()` 方法：返回当前队列中事件数量
3. 在调用侧实现二次确认（§5.2 Dispatcher 伪代码 L549-551）：
   - wait_for_event() resolve 后检查 `pending_count() > 0`
   - 若为 0 → 假唤醒，continue 重新 select!
4. 单元测试：发布事件后 wait_for_event() resolve；空队列时 wait_for_event() 不 resolve

**验收：**
- `wait_for_event()` 在有事件到达时 resolve
- `pending_count()` 返回精确值
- 假唤醒场景不导致错误的事件处理

---

### [x] T3.2 — IdleEvent 背压溢出规则

| 属性 | 内容 |
|------|------|
| 估时 | 0.5 天 |
| 涉及 | `crates/event-bus/src/` |
| 架构 | §3.2 IdleEvent（R4-2 序列化约束） |

**子任务：**
1. 在 Event Bus 的 overflow_to_disk 逻辑中新增优先级检查
2. LOW priority 事件在注入溢出缓冲区前丢弃
3. 丢弃时记录结构化日志（event.id, source, type, reason="low_priority_overflow", timestamp）
4. 确保此规则仅影响 IdleEvent（当前唯一 LOW priority 事件）

**验收：**
- 背压 Level 4A → IdleEvent 被丢弃（日志记录）
- AT_LEAST_ONCE 事件正常溢出到磁盘
- 重启后溢出的 IdleEvent 不会被恢复

---

### [x] T3.3 — Event Bus metrics 扩展

| 属性 | 内容 |
|------|------|
| 估时 | 0.5 天 |
| 涉及 | `crates/event-bus/src/metrics.rs` |
| 架构 | §12 Metrics |

**子任务：**
1. 新增指标：`idle_events_discarded`（溢出不持久化的 IdleEvent 计数）
2. 新增指标：`wait_for_event_wakeups` / `wait_for_event_false_wakeups`
3. 将指标暴露到 Event Bus metrics 端点

**验收：**
- `cargo check -p event-bus` 通过
- 新指标可通过 metrics API 查询

---

## M4：Dispatcher 改造 — Reflection 与空闲集成（6 天）

> 目标：Dispatcher 主循环集成 Reflection、QueueDrained 生产、last_source_type 传播、中断信号、熔断机制。这是整个空闲系统的核心集成点。
> 验收：事件处理完成后触发 Reflection；真实事件到达中断 idle Workflow；连续 Reflection 正确熔断。
>
> ⚠ 本里程碑同时覆盖 `agent-design.md` §3.4 中因 idle 设计新增的 Reflection 相关 Dispatcher 功能。

### [x] T4.1 — 重构 Dispatcher 主循环框架

| 属性 | 内容 |
|------|------|
| 估时 | 1.5 天 |
| 涉及 | `crates/dispatcher/src/lib.rs` |
| 架构 | §5.2 Dispatcher 主循环（完整伪代码）、agent-design.md §3.4（Reflection 触发逻辑） |

**子任务：**
1. 将现有 Dispatcher 主循环改造为新的 `run_loop(&mut self, coord: IdleCoordination)` 签名
2. 实现 `recently_processed_real_event: bool` 追踪标志
3. 实现 `reflection_consecutive_count: u32` 熔断计数器
4. 实现事件分类分支（is_real / is_queue_drained / is_idle_event）
5. 队列空分支（None → 检查 recently_processed_real_event → 判断是否产生 QueueDrained）
6. 确保新循环保持现有路由/过滤/转换逻辑不变

**验收：**
- 现有 Dispatcher 测试仍通过
- `cargo check -p dispatcher` 无错误
- 三个事件分类分支代码路径清晰

---

### [x] T4.2 — 实现 QueueDrained 生产

| 属性 | 内容 |
|------|------|
| 估时 | 1 天 |
| 涉及 | `crates/dispatcher/src/lib.rs` |
| 架构 | §5.2（QueueDrained 构造 L577-583）、§3.3 QueueDrained 定义 |

**子任务：**
1. 在真实事件处理完成后（`recently_processed_real_event == true` + 队列空）构造 `QueueDrained` 事件
2. 设置字段：last_event_type, last_trace_id, last_result_summary, arousal_level, reflection_consecutive_count
3. 将 QueueDrained 发布到 Event Bus
4. `reflection_consecutive_count += 1`
5. 确保 `QueueDrained` 不触发 `recently_processed_real_event = true`（否则死循环）

**验收：**
- 真实事件处理完成 + 队列空 → QueueDrained 发布
- QueueDrained 自身处理后不再产生新的 QueueDrained
- 日志中可追踪 QueueDrained 事件

---

### [x] T4.3 — 实现 select! Reflection 执行模式

| 属性 | 内容 |
|------|------|
| 估时 | 2 天 |
| 涉及 | `crates/dispatcher/src/lib.rs` |
| 架构 | §5.2（select! 分支 L536-558）、agent-design.md §3.4（"Reflection 可被新事件抢先"） |

**子任务：**
1. 在处理 QueueDrained 事件时使用 `tokio::select!`：
   - 分支 1：`reflection_pipeline.execute(&event)` — 执行 Reflection
   - 分支 2：`event_bus.wait_for_event()` — 等待新事件到达
2. 实现 Reflection 完成分支：
   - `busy_reflecting = false`
   - 有产出 → 注入新事件（reflection_consecutive_count 不清零）
   - 无产出 → `reflection_consecutive_count = 0`
3. 实现抢先分支（R2-9 + R2-8）：
   - 二次确认 `pending_count() > 0`（防假唤醒）
   - 先 abort Reflection → 再清 `busy_reflecting` 标志
   - `reflection_consecutive_count = 0`（被抢先→无产出不算连续）
4. 在 Reflection 执行前设置 `busy_reflecting = true`
5. Reflection timeout 配置（从 IdleConfig 读取 `timeout_secs`）

**验收：**
- Reflection 执行中，新事件到达 → Reflection 取消，新事件立即处理
- Reflection 完成后队列仍空 → 进入空闲状态（IdleDetector 接管）
- 被抢先的 Reflection 不增加熔断计数

---

### [x] T4.4 — 实现 last_source_type 传播（R5-1 guard）

| 属性 | 内容 |
|------|------|
| 估时 | 0.5 天 |
| 涉及 | `crates/dispatcher/src/lib.rs` |
| 架构 | §5.2（R2-1 + R5-1 注释 L519-526）、§10 已知风险（R5-1） |

**子任务：**
1. 在真实事件分支中，仅当 `event.is_from_external_source() == true` 时更新 `coord.last_source_type`
2. 使用 `coord.last_source_type.store(source_type.to_u8(), Ordering::Relaxed)`
3. 内部连锁任务（如 Reflection 产出的 lessons_learned）不覆盖 last_source_type
4. 添加注释说明 R5-1 修复逻辑

**验收：**
- Reflection 产出连锁任务 → last_source_type 不变
- 外部 Chat 事件 → last_source_type 正确更新为 Chat
- 聊天对话期间 ChatMode 不被静默停用

---

### [x] T4.5 — 实现 Reflection 熔断机制

| 属性 | 内容 |
|------|------|
| 估时 | 1 天 |
| 涉及 | `crates/dispatcher/src/lib.rs`、`crates/idle/src/types.rs`（ReflectionBreaker） |
| 架构 | §4.5 Reflection 熔断、§9.3 层级2-3 |

**子任务：**
1. 在 QueueDrained 生产前检查熔断条件：
   - `reflection_consecutive_count >= max_consecutive * 2` → 完全跳过 + cooldown_secs 休眠
   - `reflection_consecutive_count >= max_consecutive` → 队列仍空时跳过 lessons_learned（仅执行其他 check_items）
2. 实现 escalate_on_double 逻辑（10 次连续→cooldown）
3. 在 QueueDrained 中携带 `reflection_consecutive_count` 字段
4. 被抢先时计数重置为 0

**验收：**
- 连续 5 次 Reflection 无产出 → 第 6 次跳过 lessons_learned
- 连续 10 次 → cooldown_secs 休眠，期间不产生 QueueDrained
- 新事件到达 → 计数重置

---

### [x] T4.6 — 集成 reset_idle_signal 调用

| 属性 | 内容 |
|------|------|
| 估时 | 0.5 天 |
| 涉及 | `crates/dispatcher/src/lib.rs` |
| 架构 | §5.2（L529）、§5.4 Idle Workflow 取消机制 |

**子任务：**
1. 在真实事件处理前调用 `coord.reset_idle_signal()`
2. 确保调用顺序：last_source_type 写入 → reset_idle_signal → dispatch(event)
3. 验证：Dispatcher 集成测试确认 reset_idle_signal 被调用

**验收：**
- 真实事件到达 → idle Workflow 收到取消信号
- `real_event_seen` 被设置为 true
- 旧的 idle_cancel_token 被取消

---

## M5：IdleDetector 实现（4 天）

> 目标：IdleDetector 作为空闲状态机正确感知队列空闲、产生对应 IdleEvent、处理聊天/完整模式切换。
> R8 后 IdleDetector 不再直接实现 EventSource trait，而是由 AgentIdleManager 的后台 tokio task 驱动。
> 验收：队列持续为空时按深度产生 Daze→Boredom→Sleep→... 事件序列。

### [x] T5.1 — 实现 IdleDetector 空闲状态机骨架

| 属性 | 内容 |
|------|------|
| 估时 | 1 天 |
| 涉及 | `crates/idle/src/detector.rs` |
| 架构 | §5.3 IdleDetector 伪代码（R8：状态机逻辑由 AgentIdleManager 后台 task 驱动） |

**子任务：**
1. 实现 `IdleDetector` 结构体：coord, personality, idle_depth, last_non_idle, was_in_chat_mode, last_event_type, last_idle_outputs, agent_state
2. 字段使用 `pub(crate)` 可见性，供 `AgentIdleManager` 后台 task 读写
3. 实现 `poll()` 方法基本骨架：
   - busy_reflecting 检查 → 直接返回空
   - pending_depth_reset 检查 → 重置 depth → 返回空
   - Local EventBus queue_depth > 0 → 重置 depth → 返回空
   - 队列空 → 确定空闲类型 → 产生 IdleEvent
4. IdleEvent 的 priority = Low，发布到 Agent 的 Local EventBus

**验收：**
- `cargo check -p idle` 通过
- IdleDetector 状态机逻辑正确
- 产出的 IdleEvent 内容正确

---

### [x] T5.2 — 实现 effective_personality（聊天/完整模式切换）

| 属性 | 内容 |
|------|------|
| 估时 | 1.5 天 |
| 涉及 | `crates/idle/src/detector.rs` |
| 架构 | §5.3 effective_personality 伪代码、§6.3 聊天场景适配策略 |

**子任务：**
1. 实现 `effective_personality(&mut self) -> &IdlePersonality` 方法
2. 通过 `coord.last_source_type` 读取当前源类型（而非本地字段）
3. 是否在聊天模式保护期：`is_chat && elapsed < grace_period_secs`
4. 聊天模式激活 → 调用 `chat.as_personality(&self.personality)` 返回聊天子人格
5. 离开聊天模式（源类型变了 OR 超时）：
   - `was_chat` 为 true → `idle_depth = 0`（R3-1 修正）
   - `was_in_chat_mode = false`
6. 单元测试覆盖：纯聊天→超时→完整、聊天→非聊天事件→完整、连续聊天多次 poll

**验收：**
- Chat 事件后 30s 内 poll → ChatMode 激活（仅 Daze+Boredom）
- grace_period 60s 过后退出 ChatMode → depth 重置为 0
- 非 Chat 事件到达 → 直接使用完整 personality

---

### [x] T5.3 — 实现深度驱动空闲类型选择

| 属性 | 内容 |
|------|------|
| 估时 | 0.5 天 |
| 涉及 | `crates/idle/src/detector.rs`、`crates/idle/src/personality.rs` |
| 架构 | §4.1 状态转移规则、§4.3 默认 depth_schedule |

**子任务：**
1. depth=0 → IdleKind::Daze（§4.1 规则5）
2. depth≥1 → `personality.resolve(depth, &agent_state)` → fallback = Daze
3. 每次 poll 产出 IdleEvent 后 `idle_depth += 1`
4. 真实事件到达时 `idle_depth = 0`
5. 调用 `coord.arousal.apply_behavior(kind.arousal_behavior())`

**验收：**
- depth=0 → Daze, depth=1 → Boredom (默认配置), depth=3 → Sleep, depth=5 → Exploration
- depth=0 时跳过 resolve()，直接返回 Daze
- resolve fallback 正确（如 depth=2 在默认 schedule 中回退到 Daze）

---

### [x] T5.4 — 实现 real_event_seen 强制 reset + from_chat_mode 标记

| 属性 | 内容 |
|------|------|
| 估时 | 1 天 |
| 涉及 | `crates/idle/src/detector.rs` |
| 架构 | §5.3（R4-3 注释 L616-620）、§3.2（from_chat_mode 字段 R3-2） |

**子任务：**
1. 在 poll() 开始时读取 `coord.real_event_seen.swap(false, SeqCst)`
2. 若 `real_event_seen == true` → 强制 `idle_depth = 0`（不依赖 pending_count timing window）
3. 在产出 IdleEvent 时设置 `from_chat_mode = self.was_in_chat_mode`
4. 确保从聊天模式退出时 depth 重置优先于本次 poll 的空闲类型确定
5. 集成测试：Dispatcher 处理事件后 IdleDetector poll → depth 被重置

**验收：**
- 真实事件刚处理完（队列可能已空）→ IdleDetector depth 重置为 0
- 聊天模式下产出的 IdleEvent 携带 `from_chat_mode: true`
- 完整模式下产出的 IdleEvent 携带 `from_chat_mode: false`

---

## M6：空闲 Workflow 与 Pipeline（5 天）

> 目标：7 种空闲状态的处理逻辑全部实现，含取消机制、聊天模式 no-op、断点保存。
> 验收：每种空闲状态的事件被正确路由和处理，取消 token 能中断运行中的 Workflow。

### [x] T6.1 — 实现 IdleWorkflowRunner::run_with_cancel

| 属性 | 内容 |
|------|------|
| 估时 | 1 天 |
| 涉及 | `crates/idle/src/workflow.rs` |
| 架构 | §5.4 Idle Workflow 取消机制 |

**子任务：**
1. 实现 `IdleWorkflowRunner::run_with_cancel(workflow, cancel_token)` 方法
2. 每个步骤执行前检查 `cancel_token.is_cancelled()`
3. 被取消时：保存 checkpoint → 返回 `WorkflowResult::Cancelled { saved_checkpoint }`
4. 实现 `WorkflowResult<T>` 枚举：Completed, Cancelled, Error
5. 单元测试：取消 token → 下一步骤前退出；正常 token → 完整执行

**验收：**
- cancel_token 已取消时步骤不执行
- 取消后 checkpoint 被保存
- 正常完成返回 Completed

---

### [x] T6.2 — 实现 Daze + Boredom Pipeline

| 属性 | 内容 |
|------|------|
| 估时 | 1 天 |
| 涉及 | `crates/idle/src/`（新建 pipeline 定义）、路由配置文件 |
| 架构 | §6.2 表格（Daze/Boredom）、§6.3（Boredom 聊天 no-op） |

**子任务：**
1. Daze Pipeline：空 Pipeline，仅记录 metrics（idle_depth, duration）
2. Boredom Pipeline（完整模式）：无状态随机浏览（如随机阅读 skill 文档或近期会话摘要）
3. Boredom Pipeline（聊天模式）：读取 `IdleEvent.from_chat_mode` → 纯 no-op（立即返回）
4. 在路由中注册：`idle.daze → pipeline:idle-daze`、`idle.boredom → pipeline:idle-boredom`
5. Pipeline 为同步执行（通过 `dispatch(event).await`），Dispatcher 阻塞等待完成

**验收：**
- Daze 事件处理 < 1ms
- 聊天模式 Boredom 事件处理 < 1ms（no-op）
- 完整模式 Boredom 执行随机浏览任务

---

### [x] T6.3 — 实现 Sleep Workflow

| 属性 | 内容 |
|------|------|
| 估时 | 1 天 |
| 涉及 | `crates/idle/src/`（新建 Sleep Workflow）、`crates/idle/src/workflow.rs` |
| 架构 | §6.2（Sleep 行）、§5.4 execute_sleep_workflow 示例 |

**子任务：**
1. 实现 `SleepWorkflow`：短期记忆整理（7 天短期记忆 → 长期存储）
2. 使用 `run_with_cancel` 包裹执行
3. 被取消时：WAL checkpoint → 保存进度 → 退出
4. 实现打断策略：可被真实事件打断，损失中等
5. 配置项：short_term_retention_days, cache_expiry_days, max_cpu_seconds
6. 路由：`idle.sleep → workflow:idle-sleep`

**验收：**
- Sleep 运行中真实事件到达 → cancel 触发 checkpoint 保存
- 下次 Sleep 从 checkpoint 恢复
- CPU 使用不超过 max_cpu_seconds

---

### [x] T6.4 — 实现 Exploration Workflow

| 属性 | 内容 |
|------|------|
| 估时 | 1 天 |
| 涉及 | `crates/idle/src/`（新建 Exploration Workflow） |
| 架构 | §6.2（Exploration 行）、§8 exploration 配置 |

**子任务：**
1. 实现 `ExplorationWorkflow`：探索 memory_gaps、skill_audit、recent_failures
2. 使用 `run_with_cancel` 包裹执行
3. 被取消时：断点保存 → 退出（损失低）
4. 实现配额机制：api_rate_per_minute 限流 + on_quota_exhausted = fallback
5. 路由：`idle.exploration → workflow:idle-exploration`

**验收：**
- Exploration 每分钟 API 调用 ≤ 10
- 配额耗尽后进入 fallback 模式
- 取消时保存断点

---

### [x] T6.5 — 实现 Meditation Workflow

| 属性 | 内容 |
|------|------|
| 估时 | 0.5 天 |
| 涉及 | `crates/idle/src/`（新建 Meditation Workflow） |
| 架构 | §6.2（Meditation 行）、§4.4 打断策略矩阵（打断损失：高） |

**子任务：**
1. 实现 `MeditationWorkflow`：生成叙事报告（narrative report）
2. 实现 temp+rename 文件安全写入（原子写入）
3. 使用 `run_with_cancel` 包裹执行
4. 被取消时：丢弃当前草稿（temp 文件删除），不损坏已完成的报告
5. 路由：`idle.meditation → workflow:idle-meditation`

**验收：**
- 报告写入原子化（不出现半截文件）
- 取消时 temp 文件被清理
- 已完成的报告不受影响

---

### [x] T6.6 — 实现 Waiting + Incubation

| 属性 | 内容 |
|------|------|
| 估时 | 0.5 天 |
| 涉及 | `crates/idle/src/incubation.rs` |
| 架构 | §6.2（Waiting/Incubation）、§5.5 Incubation 后台线程 |

**子任务：**
1. Waiting Pipeline：条件检查（极短同步执行），条件满足→Active
2. Incubation Pipeline：启动独立后台线程（CancellationToken + 独立 CT）
3. 实现 `IncubationManager`：max_concurrent=1, active_handles
4. Incubation 不因真实事件中断（纯后台），仅 Phase 4.5 关闭时取消
5. 路由：`idle.waiting → pipeline:idle-waiting`、`idle.incubation → pipeline:idle-incubation`

**验收：**
- Incubation 线程不因外部事件中断
- shutdown 时 IncubationManager.shutdown_all() 正常退出

---

## M7：生命周期集成（3 天）

> 目标：Per-agent idle 系统正确集成到 Agent 生命周期、关闭序列正确、路由配置生效。
> R8：IdleDetector 不再注册为全局 EventSource。AgentRegistry 在 Phase 2 为每个 Agent 创建 AgentIdleManager，
> Phase 4 通过 `start_all_idle_loops()` 启动所有后台 task。
> 验收：Agent 启动后 per-agent idle 系统就绪；关闭时空闲 Workflow 正确终止。

### [x] T7.1 — 注册 idle 路由 + Per-Agent 启动集成 (gateway)

| 属性 | 内容 |
|------|------|
| 估时 | 1 天 |
| 涉及 | `crates/gateway/src/runtime/agent_registry.rs`、`crates/gateway/src/runtime/agent_runtime.rs` |
| 架构 | §9.1 启动序列（R8：Per-Agent） |

**子任务：**
1. Phase 2：AgentRegistry::load_from_config() 为每个 Agent 创建：
   - Local EventBus（InMemoryBus）
   - IdleCoordination（共享协调状态）
   - AgentIdleManager（含 IdleDetector + IncubationManager）
   - 存入 registry 的 idle_managers map
2. Phase 4：agent_registry.start_all_idle_loops().await → 所有 AgentIdleManager 启动后台 tokio task
3. AgentHarness 通过 `get_idle_coordination()` 获取 per-agent 协调状态
4. idle 路由注册（与旧版一致）

**验收：**
- Agent 启动日志中可见 per-agent idle 系统初始化
- `GET /health/ready` 返回 200 后 idle 系统可正常工作
- 每个 Agent 的 idle 独立运行，互不干扰

---

### [x] T7.2 — 关闭时清理 Per-Agent 空闲系统

| 属性 | 内容 |
|------|------|
| 估时 | 1.5 天 |
| 涉及 | `crates/gateway/src/runtime/agent_registry.rs` |
| 架构 | §9.2 关闭序列（R8：Per-Agent） |

**子任务：**
1. `agent_registry.clear()` 遍历所有 AgentIdleManager：
   - 调用 `manager.shutdown()` → IncubationManager.shutdown_all() + reset_idle_signal() + stop_token.cancel()
   - 后台 task 在 5s 内终止
2. 确保 idle Workflow 被取消后 checkpoint 保存到 State Store
3. 清空 agents、local_buses、idle_managers 三个 map
4. 关闭日志中记录被取消的 idle Workflow 数量

**验收：**
- 关闭信号到达 → 所有 per-agent idle task 在 5s 内终止
- Incubation 线程正确退出
- 重启后 idle 系统正常恢复

---

### [x] T7.3 — 路由配置 + 默认 Pipeline/Workflow 定义

| 属性 | 内容 |
|------|------|
| 估时 | 0.5 天 |
| 涉及 | 配置文件 + `crates/gateway/src/runtime/` |
| 架构 | §6.1 路由配置 |

**子任务：**
1. 实现路由规则注册：
   - `system.queue_drained → pipeline:reflection`
   - `idle.daze → pipeline:idle-daze`
   - `idle.boredom → pipeline:idle-boredom`
   - `idle.sleep → workflow:idle-sleep`
   - `idle.exploration → workflow:idle-exploration`
   - `idle.meditation → workflow:idle-meditation`
   - `idle.waiting → pipeline:idle-waiting`
   - `idle.incubation → pipeline:idle-incubation`
2. 注册默认 Pipeline 定义（Daze/Boredom/Waiting/Incubation）和 Workflow 定义（Sleep/Exploration/Meditation）
3. Reflection Pipeline 定义（check_items: chain_tasks, immediate_errors, lessons_learned）

**验收：**
- `system.queue_drained` 事件正确路由到 Reflection Pipeline
- 所有 `idle.*` 事件正确路由到对应处理器

---

## M8：上下文隔离与指标体系（3 天）

> 目标：空闲操作不污染对话历史 + 完整的空闲指标收集与暴露。
> 验收：闲聊场景下空闲操作隔离；metrics 端点暴露所有 IdleMetrics 字段。

### [x] T8.1 — 实现 ContextIsolation

| 属性 | 内容 |
|------|------|
| 估时 | 1.5 天 |
| 涉及 | `crates/idle/src/types.rs`、`crates/idle/src/` |
| 架构 | §3.4 ContextIsolation、§6.3 上下文隔离策略 |

**子任务：**
1. 在 Pipeline 层的 ContextBuilder 中实现 `ContextIsolation` 逻辑
2. `pollute_chat_history = false`：IdleEvent（source 标记）不进入对话 context builder
3. `suspend_on_user_input = true`：用户消息到达时，ContextBuilder 丢弃当前 idle context，仅使用对话上下文组装 LLM prompt
4. 通过 IdleEvent 的 source 字段判断是否需要隔离
5. 集成测试：发送用户消息 → 验证 prompt 中无 idle 上下文

**验收：**
- IdleEvent 处理期间的上下文不出现在下一次对话的 prompt 中
- 用户消息到达 → 立即丢弃当前 idle 上下文
- 非聊天场景下 idle 上下文正常保留

---

### [x] T8.2 — 实现 IdleMetrics + 暴露端点

| 属性 | 内容 |
|------|------|
| 估时 | 1 天 |
| 涉及 | `crates/idle/src/metrics.rs` |
| 架构 | §12 Metrics |

**子任务：**
1. 实现 `IdleMetrics` 结构体（全部 16 个字段，§12）
2. 在 IdleDetector、Dispatcher、各 Workflow/Pipeline 中埋点更新 metrics
3. 通过 runtime HTTP API 暴露 idle metrics（如 `GET /metrics/idle`）
4. 指标包括：idle_depth, idle_kind, total_idle_seconds, reflections_completed/preempted/timeout/breaker, chat_mode_active_seconds, chat_to_full_switches, idle_workflows_cancelled, explorations_completed/quota_exhausted, meditations_completed, incubation_threads_spawned/cancelled, reflections_false_wakeup

**验收：**
- `GET /metrics/idle` 返回所有 16 个字段
- 空闲状态切换时指标正确更新
- metrics 值在 Agent 重启后重置

---

### [x] T8.3 — 端到端集成测试

| 属性 | 内容 |
|------|------|
| 估时 | 1 天 |
| 涉及 | `crates/idle/src/integration_test.rs` |
| 架构 | §4.2 完整事件处理→空闲→再唤醒流程 |

**子任务：**
1. 测试：事件处理 → 队列空 → QueueDrained → Reflection（select!）
2. 测试：Reflection 期间新事件到达 → 抢先取消 → 新事件立即处理
3. 测试：连续空闲 → Daze → Boredom → Sleep → Exploration（验证深度递增）
4. 测试：真实事件到达 → reset_idle_signal → idle Workflow 被中断
5. 测试：聊天场景 → ChatMode 激活 → grace_period 过期 → 退出 ChatMode + depth 重置
6. 测试：Reflection 连续 5/10 次熔断
7. 测试：Incubation 后台运行 + 关闭时清理

**验收：**
- 所有单元测试通过（109 测试覆盖全部 8 个里程碑）
- `cargo check --workspace` 通过，无新增警告

---

## 里程碑汇总

| 里程碑 | 任务数 | 估时 | 可并行性 | 关键依赖 |
|--------|--------|------|---------|---------|
| M1 核心类型系统 | 5 | 5 天 | 无前置，可立即开工 | — |
| M2 协调与配置 | 3 | 3 天 | 需 M1 类型稳定后开工（部分并行） | M1 |
| M3 Event Bus 增强 | 3 | 2 天 | 需 M1 完成 | M1 |
| M4 Dispatcher 改造 | 6 | 6 天 | 需 M1+M2+M3 | M1, M2, M3 |
| M5 IdleDetector | 4 | 4 天 | M4 开始后可并行 | M1, M2, M3 |
| M6 空闲 Workflow | 6 | 5 天 | M4 开始后可并行 | M1, M2, M3 |
| M7 生命周期集成 | 3 | 3 天 | 需 M4+M5+M6 | M4, M5, M6 |
| M8 上下文隔离与指标 | 3 | 3 天 | 需 M7 集成点 | M7 |
| **合计** | **33** | **31 天** | | |

---

## 随时开工（无前置依赖）

以下任务可在项目启动时立即开始，无任何前置依赖：

- **T1.1** — 创建 idle crate 骨架（纯新建文件）
- **T1.2** — IdleKind + ArousalBehavior（纯类型定义）
- **T1.3** — IdleEvent + QueueDrained（纯类型定义）
- **T1.4** — IdlePersonality + ChatMode（纯类型定义）
- **T1.5** — SourceType 扩展（修改 core crate，已知接口）

---

## 架构→模块映射速查

| 架构 § | 模块 | 关键文件 |
|---------|------|---------|
| §3.1 IdleKind | idle crate | `crates/idle/src/types.rs` |
| §3.2 IdleEvent | idle crate | `crates/idle/src/types.rs` |
| §3.3 QueueDrained | idle crate | `crates/idle/src/types.rs` |
| §3.4 IdlePersonality | idle crate | `crates/idle/src/types.rs`、`personality.rs` |
| §3.5 IdleCoordination | idle crate | `crates/idle/src/coordination.rs` |
| §4 状态机 | 跨模块 | dispatcher, idle, runtime |
| §5.2 Dispatcher 主循环 | dispatcher crate | `crates/dispatcher/src/lib.rs` |
| §5.3 IdleDetector / AgentIdleManager | idle crate | `crates/idle/src/detector.rs`、`manager.rs` |
| §5.4 Workflow 取消 | idle crate | `crates/idle/src/workflow.rs` |
| §5.5 Incubation | idle crate | `crates/idle/src/incubation.rs` |
| §6 路由配置 | gateway crate | 配置文件 + 路由注册 |
| §8 配置 | config crate | `crates/config/src/` |
| §9 生命周期 | gateway crate | `crates/gateway/src/runtime/agent_runtime.rs`、`agent_registry.rs` |
| §12 Metrics | idle crate | `crates/idle/src/metrics.rs` |

---

## agent-design.md §3.4 对应关系

本里程碑覆盖 `agent-design.md` §3.4 Event Dispatcher 中因 idle 系统新增的所有功能：

| §3.4 功能 | 里程碑任务 | 说明 |
|-----------|-----------|------|
| QueueDrained 触发 Reflection | T4.2, T4.3 | Dispatcher 在队列清空时产生 QueueDrained 事件 |
| select! 并发模式 | T4.3 | Reflection 与新事件到达并发竞争 |
| busy_reflecting 协调标志 | T4.3, T5.1 | 防止 IdleDetector 在 Reflection 期间产 IdleEvent |
| cancel_idle_workflows() 中断 | T4.6, T6.1 | 真实事件到达时取消运行中的空闲 Workflow |
| last_source_type 传播 | T4.4 | 供 IdleDetector 判断聊天/完整模式 |
| wait_for_event() 异步通知 | T3.1 | Event Bus 提供非忙等的异步新事件通知 |
