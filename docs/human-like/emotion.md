# 情绪层 — 我感受如何

> Agent 不应该假装"微笑服务"。真正的情绪感知是：
> Agent 能根据当前处境（刚完成任务、被用户纠正、后端故障）选择最合适的"情绪表达"，
> 并通过事件机制让整个系统感知到这一状态。
>
> Aman 通过 `EmotionEvaluator`（LLM 驱动）+ 状态→emoji 映射 + 认知状态绑定，
> 实现**可观测、可下游消费**的 Agent 情绪系统。

---

## 1. 设计哲学

```
情绪不是装饰，是信号。

Agent 的"挫败"不是假笑——它是后端连不上时的 Catatonic 状态，
是连续失败后的 EmotionEvaluator 选了"😩"，
是 idle 太久后 arousal 降到 0.05 时的"😐"。

这些情绪通过事件发布，可被 Notification、UI、LifecycleEngine 消费。
```

**两个层次：**

| 层 | 机制 | 触发 |
|---|---|---|
| **主动情绪** | `EmotionEvaluator` 后台任务，周期性调 LLM 选 emotion | 时间间隔 + 上下文变化 |
| **被动情绪** | 状态→emoji 映射表 | 系统状态变更（idle → 😴, working → 💻） |

---

## 2. 主动情绪：EmotionEvaluator

### 2.1 架构

```
Per-Agent 后台 tokio task
     │
     ├─ sleep (interval_secs ± 15% jitter)
     │
     ├─ 收集上下文：
     │   ├─ 最近 N 条 session 消息
     │   ├─ 最近 5 条 trace 记录
     │   └─ 当前 arousal level
     │
     ├─ LLM 调用 (15s timeout, retry 2x)
     │
     └─ 发布 `emotion:evaluated` 事件
         └─ payload: { emotion_id, reasoning }
```

### 2.2 代码位置

`kernel/gateway/src/runtime/emotion_evaluator.rs`

### 2.3 数据结构

```rust
/// 从 LLM 解析的情绪响应
struct EmotionResponse {
    emotion_id: String,   // 如 "frustrated", "curious", "satisfied"
    reasoning: String,    // LLM 的选择理由（用于可观测性）
}

/// 单个可选情绪
struct EmotionCandidate {
    id: String,
    description: String,
    tags: Vec<String>,    // 中英文标签（LLM 可能返回 tag 而非 id）
}
```

### 2.4 门控（Gating）

只有 Agent 的 `emotions/` 目录存在且包含有效的 `data.json` + 所有引用图片时，
`EmotionEvaluator` 才会启动。否则回退到状态→emoji 映射。

### 2.5 配置

```yaml
# config.yaml
agents:
  coder:
    emotion:
      enabled: true
      interval_secs: 300      # 每 5 分钟评估一次
      temperature: 0.7
      max_context_messages: 10
```

---

## 3. 被动情绪：状态→emoji 映射

当 LLM 不可用（CognitiveState ≠ Lucid）时，情绪由系统状态**硬编码绑定**：

```rust
// 伪代码 — kernel/gateway/src/runtime/emotion_evaluator.rs
if cognitive_state != CognitiveState::Lucid {
    return match cognitive_state {
        CognitiveState::Groggy    => "groggy",     // 😵‍💫 "脑子有点糊"
        CognitiveState::Catatonic => "catatonic",  // 😶 "动不了"
        CognitiveState::Coma      => "coma",       // 💤 "没知觉了"
        _ => unreachable!(),
    };
}
```

### 3.1 系统状态 → 情绪映射

| AgentSystemState | Emoji | 含义 |
|---|---|---|
| `Idle` | 🪹 / 😐 | 空闲 / 平静 |
| `Working` | 💻 / 🔧 | 专注工作 |
| `Chatting` | 💬 | 对话中 |
| `Studying` | 📚 | 学习中 |
| `DailyLife` | 🌿 | 日常维护 |
| `Prize` | 🎁 | 游戏/奖励 |

### 3.2 IdleKind → 情绪映射

| IdleKind | Emoji | 拟人化 |
|---|---|---|
| `Daze` | 😐 | "发呆中" |
| `Boredom` | 🪹 / 😑 | "有点无聊，想找点事做" |
| `Sleep` | 😴 | "在整理记忆" |
| `Exploration` | 🔭 | "好奇地探索" |
| `Meditation` | 🧘 | "沉思中" |
| `Waiting` | ⏳ | "等待条件满足" |
| `Incubation` | 💡 | "潜意识里在处理问题" |
| `WakeUp` | 🌅 | "刚刚苏醒" |

---

## 4. 情绪的事件消费

情绪事件发布后，可被多个下游系统消费：

```
emotion:evaluated 事件
    │
    ├─▶ NotificationSubscriber → 推送到桌面/移动端
    │
    ├─▶ SSE agent_states:updated → UI 展示 emoji
    │
    ├─▶ LifecycleEngine → 影响 IdleSignal (Satisfaction / Frustration)
    │
    └─▶ ArousalTracker → 情绪唤醒度反馈
```

### 4.1 与 Arousal 的联动

```rust
// 情绪 → arousal 的反馈
match emotion_id {
    "excited" | "curious"   => arousal.boost(+0.2),  // 兴奋/好奇 → 唤醒
    "frustrated" | "anxious"=> arousal.boost(-0.1),  // 沮丧 → 轻微抑制
    "satisfied" | "calm"    => arousal.boost(-0.05), // 满足 → 平静
    "bored"                 => arousal.boost(-0.3),  // 无聊 → 大幅衰减
    _ => {}
}
```

---

## 5. 认知状态对情绪的覆盖

当 Agent 的"大脑"（LLM 后端）不工作时，情绪不再由 LLM 选择，
而是**直接绑定到认知状态**：

| CognitiveState | 拟人化 | 情绪行为 |
|---|---|---|
| **Lucid** | "清醒中" | 正常 LLM 评估 |
| **Groggy** | "脑子有点转不动" | 😵‍💫 固定输出，不调用 LLM |
| **Catatonic** | "闭锁综合征" | 😶 固定输出，不调用 LLM |
| **Coma** | "麻醉状态" | 💤 完全无感知 |

**关键**：这不是 prompt 注入（"你现在感到 catatonic，请表现出相应的行为"），
而是**直接返回预设值**，因为 LLM 调用本身就应在 Catatonic/Coma 时被跳过。

---

## 6. 目录结构

```
~/.aman/agents/{agent_id}/
├── SOUL.md              # 身份（见 identity.md）
├── emotions/
│   ├── data.json        # 可选情绪列表 + 图片引用
│   ├── happy.png
│   ├── frustrated.png
│   ├── curious.png
│   ├── satisfied.png
│   ├── bored.png
│   └── ...
└── ...
```

`data.json` 示例：

```json
[
  {
    "id": "satisfied",
    "description": "任务顺利完成，感到满足",
    "image": "satisfied.png",
    "tags": ["满足", "满意", "satisfied"]
  },
  {
    "id": "frustrated",
    "description": "连续失败或遇到障碍",
    "image": "frustrated.png",
    "tags": ["沮丧", "挫败", "frustrated"]
  }
]
```

---

## 7. 实现位置

| 组件 | 文件 | 职责 |
|---|---|---|
| EmotionEvaluator | `kernel/gateway/src/runtime/emotion_evaluator.rs` | Per-agent 后台情绪评估 |
| 状态→emoji 映射 | `kernel/gateway/src/runtime/agent_harness.rs` | CognitiveState ≠ Lucid 时的情绪绑定 |
| 情绪事件 | `EventBus` | `emotion:evaluated` / `emotion:changed` |
| UI 消费 | 桌面 app | SSE `agent_states:updated` → emoji 渲染 |
| 门控检查 | `SoulRuntime` + 启动验证 | emotions 目录有效性检查 |

---

> **参考：**
> - [意识状态](./consciousness.md) — 认知状态如何覆盖 LLM 情绪选择
> - [空闲系统](./idle-boredom.md) — IdleKind 到情绪的映射
> - [EmotionEvaluator 源代码](../../kernel/gateway/src/runtime/emotion_evaluator.rs)
