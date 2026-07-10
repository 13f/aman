# 自省层 — 元认知与事后复盘

> 自省是将处理能力转向自身的**元认知操作**。
> 不是被动的"日志回放"，而是 Agent 主动审视自身行为、
> 从中提取规律、并将规律写入长期记忆的过程。
>
> Aman 通过 `QueueDrained` 触发 Reflection + `workflow::completed` 触发经验萃取，
> 实现**多层次、事件驱动**的自省系统。

---

## 1. 设计哲学

```
自省不是日志回放，是元认知。

触发时机：
  - 刚完成一个复杂任务 → 事后复盘
  - 连续多次类似失败 → 模式识别
  - 空闲状态          → 维护性自省
  - 每日一次（定时）   → 综合自省
  - 被用户明确纠正     → 即时修正
```

### 为什么 Reflection 是事件驱动而非 cron？

事件处理完、队列刚清空的那一刻，Agent 的**上下文还热**——应该立刻复盘刚完成的任务。
这是 Dispatcher 的责任，不是 IdleDetector 的责任。

```
QueueDrained 事件 = Agent 的"呼出一口气"时刻
  → "刚才做了什么？有没有值得记录的？"
```

---

## 2. Reflection 的触发与执行

### 2.1 QueueDrained — 两个来源

| 来源 | 触发条件 | 拟人化 |
|---|---|---|
| **正常路径** | AgentIdleManager 检测到 busy→empty 转换 | "刚忙完，呼口气" |
| **冷启动路径** | 启动后队列持续为空超过 3-5s → 合成 QueueDrained | "刚出生，看看周围" |

### 2.2 select! 抢先机制

Reflection 在 Dispatcher 中通过 `select!` 执行——**新事件可抢先取消**：

```rust
select! {
    reflection_pipeline.run() => {
        // 完成或超时 (60s)
        if has_output { 注入新事件, count 不清零 }
        else { count = 0 }
    }
    _ = event_bus.wait_for_event() => {
        // 新事件到达 → 立即取消 Reflection
        reflection.abort()
        // 被抢先 → 熔断计数重置（无产出不算连续）
        reflection_consecutive_count = 0
    }
}
```

### 2.3 Reflection 操作序列

```
level 1 - 日志追溯:
  扫描最近 N 次会话，提取:
  - 被用户纠正过的地方 → 写入 memory
  - 重复报错的工具调用 → 分析根因
  - 耗时异常的操作 → 考虑优化方案

level 2 - 知识整理:
  - memory 去重：合并相似的记忆条目
  - skill 审计：标记使用频率、准确度
  - 过时知识标记：根据时间戳衰减

level 3 - 行为模式分析:
  - 用户最常问的领域 → 是否需要预加载知识？
  - 我经常犯错的地方 → 是否需要硬编码规则？
  - 被跳过的功能 → 是否需要废弃或改进？
```

---

## 3. ReflectionBreaker — 熔断机制

连续 Reflection 无产出时，防止无限空转：

```
count < max_consecutive (5)     → 执行全部 check_items
count >= 5, < 10                → 跳过 lessons_learned
count >= 10                     → 跳过所有 check_items + cooldown 禁止
```

| 参数 | 默认值 | 说明 |
|---|---|---|
| `max_consecutive` | 5 | 连续 5 次无产出后降级 |
| `cooldown_secs` | 30 | 完全跳过后的冷却时间 |
| `escalate_on_double` | true | 连续翻倍时加速熔断 |

---

## 4. 经验萃取 — 从事件到 EXP.md

### 4.1 触发时机

```
workflow::completed 事件
  → 经验萃取器订阅
  → 提取：用了什么工具组合、结果如何、是否匹配已有经验
  → 匹配到 → 升级 confidence，追加 evidence
  → 没匹配 → 新条目，confidence = 0.5
  → 发布 experience:extracted 事件
```

### 4.2 不经过 LLM

经验萃取是**纯结构化的统计更新**——不需要 LLM 参与。
只有当经验需要"文字总结"时才调 LLM（低频，可在 idle 时做）。

### 4.3 Experience Think — 与 yantrik think() 平行

拟人隐喻：

> **yantrik think()** = 睡眠时巩固记忆（把短期记忆变长期，遗忘不重要的）
> **experience.consolidate()** = 睡醒后复盘手感（哪些功夫没生疏、哪些招式该练了）

```
现有 think() 调用链：
  idle 触发 → yantrik.think() → 合并/冲突/模式

修改后：
  idle 触发 → yantrik.think() + experience.consolidate()
                            ↓                    ↓
                       memory 整理           经验整理
                                            ├── confidence 重算
                                            ├── 长期没用 → 标 "需验证"
                                            ├── 矛盾经验 → 标记等人工确认
                                            └── pattern_score 衰减
```

两者并行但不混合——memory 管"知道什么"，experience 管"会做什么"。

---

## 5. 自省的偏差问题

Agent 自省天生有偏差——它只能看到它知道它做错的事，看不到不知道做错的事。

**解决方案：引入外部锚点**

| 锚点 | 权重 | 来源 |
|---|---|---|
| 用户的纠正 | 最高 | `correction_received` 事件 |
| 日志中的异常模式 | 中等 | Reflection 日志追溯 |
| 与历史行为的对比 | 低 | Experience 翻译器 pattern_score |

---

## 6. 实现位置

| 组件 | 文件 | 职责 |
|---|---|---|
| Reflection Pipeline | `kernel/dispatcher/` | QueueDrained → Reflection 执行 |
| ReflectionBreaker | `kernel/dispatcher/` | 熔断机制 |
| 经验萃取器 | `kernel/experience/` | workflow::completed → EXP.md 更新 |
| Experience Think | `kernel/experience/` | idle 触发 → 经验整理 |
| 事件订阅 | `kernel/event-bus/` | Custom 事件路由 |

---

> **参考：**
> - [认知翻译层](../cognitive-memory.md) — Experience 翻译器
> - [经验系统](./experience.md) — EXP.md 结构与更新机制
> - [Idle 系统](./idle-boredom.md) — Reflection 在空闲流程中的位置
