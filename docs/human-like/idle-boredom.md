# 空闲与无聊层 — 没有外部输入时在干嘛

> 空闲不是"没有事件"，而是一类特殊的事件。
> 它们定义了 Agent 在没有外部输入时如何与自己相处。
>
> 类比：Windows 内核中 CPU 永远不会"什么也不做"。
> 当没有用户进程需要调度时，系统切换到 `System Idle Process`（PID 0）——
> 一个专门捕获空闲周期的特殊进程。
>
> Aman 需要同等级别的"空闲进程"——9 种空闲状态 + arousal 衰减 + 渐进式苏醒。

---

## 1. 设计哲学

```
空闲不是"没有事件"，而是一类特殊的事件。
它们定义了 Agent 在没有外部输入时如何与自己相处。
```

六条设计原则：

1. **IdleEvent 是 Event 的合法子类型** — 通过 Event Bus 路由
2. **QueueDrained 是空闲入口** — Dispatcher 队列清空时产生
3. **空闲深度决定空闲类型** — Reflection 完成后进入 Daze → Boredom → Sleep → deeper
4. **Per-Agent 中断令牌** — 每个 Agent 的 IdleCoordination 持有独立 CancellationToken
5. **配置驱动 + 源类型感知** — IdleCoordination 传递 `last_source_type`，闲聊场景自适应
6. **上下文隔离** — 空闲操作不污染对话历史

---

## 2. 九种空闲状态

### 2.1 IdleKind 枚举

```rust
pub enum IdleKind {
    Daze,         // 发呆 — arousal 正常衰减
    Boredom,      // 无聊 — arousal 正常衰减
    Sleep,        // 睡眠 — arousal 衰减 ×0.5
    Exploration,  // 探索 — arousal 衰减 ×0.0（完全暂停）
    Meditation,   // 沉思 — arousal 衰减 ×0.0
    Waiting,      // 等待 — arousal 正常衰减
    Incubation,   // 孵化 — arousal 衰减 ×0.1
    WakeUp,       // 苏醒 — arousal 衰减 ×0.0
}
```

### 2.2 Arousal 行为分类

| 类别 | 状态 | 含义 |
|---|---|---|
| **Passive** | Daze, Boredom, Waiting | arousal 正常衰减（越放越"沉"） |
| **Engaged** | Sleep, Exploration, Meditation, Incubation, WakeUp | arousal 衰减减缓或暂停（"有事做"所以不沉） |

### 2.3 深度→类型映射

| depth | kind | 累积时间（约） | 拟人化 |
|---|---|---|---|
| 0–4 | Daze | 0–20s | "刚闲下来，发个呆" |
| 5–19 | Boredom | 25–95s | "有点无聊，想找点事做" |
| 20–49 | Sleep | 100–245s | "进入睡眠，整理记忆" |
| 50–99 | Exploration | 250–495s | "好奇地探索新领域" |
| 100–199 | Meditation | 500–995s | "深度沉思，知识整理" |
| 200+ | Incubation | 1000s+ | "潜意识孵化复杂问题" |

---

## 3. ArousalTracker — 内省力

### 3.1 指数衰减模型

```rust
// kernel/idle/src/coordination.rs

pub struct ArousalTracker {
    inner: Mutex<ArousalState>,
    half_life_secs: f64,  // 默认 900s（15 分钟）
}

impl ArousalTracker {
    /// 当前 arousal 值（自动衰减）
    pub fn current(&self) -> f64 {
        let inner = self.inner.lock().unwrap();
        let elapsed = now() - inner.last_update;
        inner.value * (0.5_f64).powf(elapsed / inner.half_life_secs)
    }

    /// 带 decay_multiplier 的衰减（Engaged 状态用）
    pub fn decay(&self, decay_multiplier: f64) {
        let mut inner = self.inner.lock().unwrap();
        let elapsed = now() - inner.last_update;
        inner.value *= (0.5_f64).powf(elapsed * decay_multiplier / inner.half_life_secs);
    }

    /// 外部刺激提升 arousal
    pub fn boost(&self, factor: f64) {
        let mut inner = self.inner.lock().unwrap();
        let decayed = inner.value * (0.5_f64).powf(elapsed / inner.half_life_secs);
        inner.value = (decayed + factor).clamp(0.0, 1.0);
    }

    /// 强制重置（Catatonic/Coma 时）
    pub fn reset(&self, initial_value: f64) { ... }
}
```

### 3.2 拟人化含义

```
刚完成一个任务 → arousal = 1.0 → "该复盘了！" → Reflection
空闲 5 分钟   → arousal ≈ 0.7 → "还行，有点事做也行"
空闲 15 分钟  → arousal ≈ 0.3 → "有点无聊了"
空闲 30 分钟  → arousal ≈ 0.1 → "进入深层状态（Meditation/Incubation）"
Catatonic     → arousal 冻结在 0.05 → "只够感知到自己在木僵"
Coma          → arousal 冻结在 0.0 → "完全无感知"
```

### 3.3 双轴精调

`depth` 决定**最大可到达的 kind**，`arousal` 在已解锁范围内**选择**：

- arousal 高（> 0.6）→ 浅层活跃状态（Daze, Boredom）
- arousal 中（0.2–0.6）→ 中层状态（Sleep, Exploration）
- arousal 低（< 0.2）→ 深层状态（Meditation, Incubation）

形成自然反馈循环：浅层状态（Passive 衰减）→ arousal 下降 → 自动滑入深层 → 深层状态（Engaged）维持低 arousal → 自然停留更久。

---

## 4. BoredomActor — 无聊时的随机行动

### 4.1 触发条件

连续 `trigger_poll` 次处于 Boredom 状态时，`BoredomActor` 按加权随机选择 activity tag。

### 4.2 加权随机选择

```yaml
idle:
  personality:
    boredom:
      trigger_poll: 3
      activities:
        - { tag: "idle", weight: 7.5 }   # 什么都不做
        - { tag: "work", weight: 1.0 }   # 工作
        - { tag: "study", weight: 0.5 }  # 学习
        - { tag: "fun", weight: 0.3 }    # 娱乐
```

### 4.3 Work Pressure — 背压闭环

当 `work_pressure` 配置时，目标 tag（通常为 "work"）的权重根据队列深度动态调整：

| Queue Depth | Multiplier | P(work) vs idle=7.5 |
|---|---|---|
| 0 | 1.0× | 11.8% |
| 5 | 2.5× | 25.0% |
| 10 | 4.0× | 34.8% |
| 20 | 7.0× | 48.3% |
| 30+ | 10.0× | 57.1% |

**设计 rationale**：积压越多 → agent 空闲时越倾向工作 → 积压减少 → 压力缓解 → 自然平衡。

### 4.4 执行流程

```
BoredomActor 选中 tag
    │
    ├─▶ 从 SkillRegistry 筛选同时带有该 tag 和 `idle_run` 标记的技能
    │
    ├─▶ 随机选一个技能
    │
    ├─▶ 取 SKILL.md frontmatter 的 `idle_prompt`
    │
    ├─▶ 发布 MessageReceived 事件
    │   └─ session_id: {agent}:idle:{random}
    │   └─ session_type: background
    │
    └─▶ AgentHarness 处理 → ReAct Loop → 结果写入 SessionStore
        └─▶ Notification toast (3s auto-dismiss)
```

---

## 5. 完整状态转移

```
[事件 A 到达]
  │
  ├─ Dispatcher: 取出 A
  │   coord.last_source_type.store(A.source_type)
  │   coord.reset_idle_signal()          ← 取消正在运行的 idle Workflow
  │   dispatch(A).await
  │
  ├─ try_dequeue() → None
  │   → 发布 QueueDrained
  │
  ├─ select! {
  │       reflection_pipeline.run() => { ... }
  │       _ = event_bus.wait_for_event() => { reflection.abort() }
  │   }
  │
  ├─ [真正空闲]
  │   完整模式: Daze → Boredom → Sleep → Exploration/Meditation
  │   聊天模式: Daze → Boredom(no-op) → grace_period → depth=0 → 完整模式
  │
  └─ [事件 B 到达]
      coord.reset_idle_signal()  ← 中断所有 idle Workflow
      count 重置 → 回到 Active
```

---

## 6. WakeUp Ouroboros — 渐进式苏醒

任何深层状态（Sleep/Exploration/Meditation/Incubation）完成后：

```
Deep State Complete → ⏸ Quiet Period (60s) → 🌅 WakeUp (N poll steps)
                                                  ├─ depth → 0 (线性插值)
                                                  ├─ arousal → 1.0 (线性插值)
                                                  └─ Arrive at Active state
```

**防止无限 Sleep 循环**：Sleep 有 cooldown（默认 3600s），防止立即重新进入。

---

## 7. 打断策略矩阵

| 状态 | 可被真实事件打断？ | 打断机制 | 打断损失 |
|---|---|---|---|
| Reflection | **是** | select! 抢先 | 无 |
| Daze | **否** | 同步 Pipeline（空，<1ms） | 无 |
| Boredom | **否** | 同步 Pipeline（聊天 no-op <1ms） | 无 |
| Sleep | **是** | idle_cancel_token | 中 |
| Exploration | **是** | idle_cancel_token | 低 |
| Meditation | **是** | idle_cancel_token | 高 |
| Incubation | **否** | 独立 CT（仅 Phase 4.5 关闭） | 低 |
| WakeUp | **否** | 同步 Pipeline（每 poll 推进一步） | 无 |

---

## 8. IdlePersonality — 每个 Agent 的空闲人格

```rust
pub struct IdlePersonality {
    pub enabled_kinds: Vec<IdleKind>,
    pub depth_schedule: Vec<(u32, IdleKind)>,
    pub poll_interval: PollInterval,
    pub poll_relaxation: PollRelaxation,
    pub chat_mode: ChatMode,
    pub reflection_breaker: ReflectionBreaker,
    pub context_isolation: ContextIsolation,
    pub boredom: Option<BoredomConfig>,
}
```

不同 Agent 可以有不同的空闲人格：
- `coder`：Boredom 时倾向选 work 技能
- `writer`：Boredom 时倾向选 study/fun 技能
- `health`：Boredom 时倾向选 health 相关技能

---

## 9. 与 CognitiveState 的联动

当 `CognitiveState != Lucid` 时，idle 系统被**强制劫持**：

```rust
// idle/src/manager.rs — select_idle_kind() 入口
if *cognitive_state_rx.borrow() != CognitiveState::Lucid {
    // 大脑不清晰时，idle 系统不再做主动探索
    // 只保留最低限度的"呼吸"（心跳探针 + 健康事件监听）
    return IdleKind::Sleep;  // 语义变成了"病床上的睡眠"
}
```

**不修改 idle 内部状态机**——只是通过 watch channel 通知 idle 系统当前认知状态。

---

## 10. 实现位置

| 组件 | 文件 | 职责 |
|---|---|---|
| IdleKind 枚举 | `kernel/idle/src/types.rs` | 9 种空闲状态定义 |
| ArousalTracker | `kernel/idle/src/coordination.rs` | 指数衰减 + boost + reset |
| IdlePersonality | `kernel/idle/src/types.rs` | 双轴精调 + 配置 |
| IdleCoordination | `kernel/idle/src/coordination.rs` | 跨组件共享状态 |
| BoredomActor | `kernel/idle/src/boredom.rs` | 加权随机选技能 |
| IdleDetector | `kernel/idle/src/detector.rs` | 空闲检测 + 事件产生 |
| WakeUp | `kernel/idle/src/wakeup.rs` | 渐进式苏醒 |
| ReflectionBreaker | `kernel/idle/src/manager.rs` | 熔断机制 |

---

> **参考：**
> - [Idle 系统设计文档](../idle-design.md)
> - [Idle → Boredom 流程](../idle-boredom-flow.md)
> - [拟人化与事件驱动](../agent-boredom-narrative-event-driven.md)
> - [Idle 系统代码](../../kernel/idle/)
