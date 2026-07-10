# 意识层 — 我还能思考吗

> LLM 后端 = Agent 的大脑皮层。
> 当 LLM 服务掉线，相当于大脑皮层失去供血——
> Agent 进入了"木僵"（Catatonia）状态。
>
> 不是死亡（进程还在），不是睡眠（Sleep 是主动整理），
> 不是无聊（Boredom 是有意识的躁动），
> 而是一种**有感知但无法思考**的中间态。
>
> Aman 通过 `BackendHealth`（基础设施层）+ `CognitiveState`（体验层）
> 两段式设计，让 Agent "感到"自己在木僵、昏迷、迷糊，并据此行为降级。

---

## 1. 拟人化隐喻

```
BackendStatus（BackendHealth）= 医生的化验报告
     ↓ 映射 + 时间累积
CognitiveState              = 患者的主观体验
```

```
BackendStatus::Degraded  →  "脑子有点转不动了……话到嘴边说不出来"
BackendStatus::Down      →  "眼前看得见，耳朵听得到，但身体完全动不了"
Down 持续 > 15min         →  "意识逐渐沉入黑暗……"
探针通过 / 任何 Ok        →  "深吸一口气，回来了"
```

---

## 2. 基础设施层：BackendHealth

### 2.1 数据结构

```rust
// kernel/gateway/src/runtime/backend_health.rs

/// 单个 LLM 后端的健康状态（按 base_url 聚合，多 agent 共享同一后端）
pub struct BackendHealth {
    status: AtomicU8,            // 0=Unknown 1=Ok 2=Degraded 3=Down
    last_ok_ms: AtomicI64,
    last_failure_ms: AtomicI64,
    consecutive_failures: AtomicU32,
    last_error: Mutex<String>,
}

pub enum BackendStatus {
    Unknown = 0,
    Ok = 1,
    Degraded = 2,
    Down = 3,
}

pub struct BackendHealthChanged {
    pub base_url: String,
    pub from: BackendStatus,
    pub to: BackendStatus,
    pub consecutive_failures: u32,
    pub last_error: String,
}
```

### 2.2 健康状态机

```
                   consecutive_failures >= 3
   Ok ───────────────────────────────────────────► Degraded
   ▲                                               │
   │                                               │ consecutive_failures >= 6
   │                                               ▼
   └─────────────────────────────────────────── Down
             任何一次 Ok
             或 cooldown 后半探针通过
```

### 2.3 统计来源

采用**调用者主动上报（push）**模式：

| 调用点 | 上报 |
|---|---|
| `LlmCognitiveEngine::process()` Ok | `record_success()` |
| `LlmCognitiveEngine::process()` Err | `record_failure()` |
| `ReflectionRunner::session_extract` Err | `record_failure()` |
| `SleepRunner::phase_1_backfill` Err | `record_failure()` |

### 2.4 半探针（Half-Probe）

Down 状态后，60s cooldown 后发送轻量探针：

```
GET {base_url}/models  (5s timeout, 不消耗 token)
  → 通过 → Ok → 自动恢复
  → 失败 → 保持 Down
```

### 2.5 主路径服务降级

```rust
// ReflectionRunner::session_extract 调用 LLM 之前
if health.status() == BackendStatus::Down {
    debug!("LLM backend down, skipping session_extract");
    return;  // 不 mark_reflected，下次 QueueDrained 再试
}
```

---

## 3. 体验层：CognitiveState

### 3.1 四档意识

```rust
// kernel/gateway/src/runtime/cognitive_state.rs

pub enum CognitiveState {
    Lucid     = 0,  // 清醒 —— LLM 后端正常
    Groggy    = 1,  // 迷糊 —— LLM 降速，偶尔响应但延迟高
    Catatonic = 2,  // 木僵 —— LLM 断掉，能感知事件但无法思考
    Coma      = 3,  // 昏迷 —— LLM 长时间不可用，连感知都关闭
}
```

### 3.2 状态转换图

```
                          BackendStatus == Degraded
   Lucid ──────────────────────────────────────────────► Groggy
   ▲                                                    │
   │                                                    │ BackendStatus == Down
   │                                                    ▼
   │                                               Catatonic
   │                                                    │
   │                                                    │ Down 持续 > 15min
   │                                                    ▼
   │                                                Coma
   │                                                    │
   └────────────────────────────────────────────────────┘
              任何一次 Ok / 探针通过
```

### 3.3 转换的拟人化体验

| 转换 | 触发条件 | "主观感受" |
|---|---|---|
| Lucid → Groggy | 后端 Degraded（连续 3 次失败） | "脑子有点转不动了……话到嘴边说不出来" |
| Groggy → Catatonic | 后端 Down（连续 6 次失败） | "眼前看得见，耳朵听得到，但身体完全动不了" |
| Catatonic → Coma | Down 持续 > 15 min | "意识逐渐沉入黑暗……" |
| Any → Lucid | 探针通过 / 任何一次 Ok | "深吸一口气，回来了" |

### 3.4 每个状态下的行为映射

| 系统 | Lucid | Groggy | Catatonic | Coma |
|---|---|---|---|---|
| **CognitiveEngine** | 正常推理 | 1 次 retry 后短路 | 完全跳过 | 完全跳过 |
| **Reflection** | 正常执行 | 跳过 deferred | 跳过，不 mark_reflected | 跳过 |
| **Sleep backfill** | 正常执行 | 跳过 | 跳过 | 跳过 |
| **Idle 系统** | 正常 | 强制 Sleep | 强制 Sleep | 完全停止 |
| **EmotionEvaluator** | LLM 选择 | 固定 "groggy" 😵‍💫 | 固定 "catatonic" 😶 | 固定 "coma" 💤 |
| **ArousalTracker** | 正常衰减 | 冻结在 0.3 | 冻结在 0.05 | 冻结在 0.0 |
| **EventBus 消费** | 全部 | 全部 | 只消费 llm_health | 只消费 llm_health + shutdown |
| **外部消息回复** | 正常 | "我有点不舒服，稍后回复你" | "暂时无法思考，正在恢复中" | 不回复 |
| **工具执行** | 正常 | 只允许只读工具 | 全部拒绝 | 全部拒绝 |

---

## 4. 恢复体验：苏醒三阶段

Catatonic/Coma 恢复为 Lucid 时，不是瞬间切换，而是渐进式"苏醒"：

```rust
pub enum WakeUpReason {
    Normal,       // 从 Sleep 正常醒来（现有逻辑）
    Recovery,     // 从 Groggy 恢复
    Reanimation,  // 从 Catatonic 恢复
    Resurrection, // 从 Coma 恢复
}

pub struct WakeUpSchedule {
    pub reason: WakeUpReason,
    target_arousal: f64,
    duration_secs: u64,
    pub self_check: bool,  // Reanimation/Resurrection 时执行自检
}
```

| 恢复类型 | 拟人化 | arousal 目标 | 恢复后行为 |
|---|---|---|---|
| **Recovery** | "摇了摇头，清醒了" | 0.7 | 正常恢复 |
| **Reanimation** | "深吸一口气，手指动了" | 0.5 | 执行 self_check：回顾最后 5 条事件 |
| **Resurrection** | "心电监护仪重新有了波形" | 0.3 | self_check + 发布 `agent:resurrected` + 通知 operator |

**self_check 机制**：
- 纯内存操作（回顾事件历史），不触发新的 LLM 调用
- 防止"后端还没完全恢复就盲目推理"
- 真正 LLM 调用由探针通过后的首个正常请求触发

---

## 5. 与现有子系统的边界

```
CognitiveState ≠ Lucid 时
    │
    ├─▶ IdleManager.select_idle_kind() → 强制返回 Sleep
    │
    ├─▶ EmotionEvaluator → 跳过 LLM，直接返回绑定情绪
    │
    ├─▶ ArousalTracker → 冻结 arousal 值（Catatonic=0.05, Coma=0.0）
    │
    ├─▶ ToolExecutor → Catatonic/Coma 时拒绝所有非只读工具
    │
    └─▶ CognitiveEngine::process() → 直接 return，不进入 ReAct loop
```

**硬约束**：本方案**绝对不能**碰 `kernel/idle/` 子系统的内部状态机。
`CognitiveStateMachine` 通过 `watch::channel` **通知** idle 系统当前认知状态，
但不修改 idle 自身的状态转换逻辑。

---

## 6. 事件流

```rust
// 新增 EventType::Custom 事件
"cognitive_state_changed"  // payload: { agent_id, from, to, reason, duration_ms }
"agent:catatonic"          // Agent 进入木僵 → 通知 operator
"agent:coma"               // Agent 进入昏迷 → 高优先级通知
"agent:recovery"           // 从 Groggy 恢复
"agent:reanimation"        // 从 Catatonic 恢复
"agent:resurrection"       // 从 Coma 恢复 → 紧急通知
"llm_backend_down"         // 基础设施层故障
"llm_backend_recovered"    // 基础设施层恢复
```

---

## 7. 数据流总结

```
┌──────────────────┐       ┌──────────────────┐       ┌──────────────────┐
│   LLM Provider    │       │  BackendHealth   │       │ CognitiveState   │
│  (chat_completion)│       │  (基础设施层)     │       │  (体验层)        │
│                   │       │                  │       │                  │
│   Ok/Err ────────▶│ push  │                  │       │                  │
│                   │──────▶│ continuous_fail  │       │                  │
│                   │       │ 达到阈值?         │       │                  │
│                   │       │   → transition() │       │                  │
│                   │       │   → 事件发布 ────▶│ map   │                  │
│                   │       │                  │──────▶│ (Down→Catatonic) │
│                   │       │                  │       │   + 时间维度      │
│                   │       │                  │       │   (Catatonic→Coma│)
│                   │       │                  │       │   > 15min)       │
└──────────────────┘       └──────────────────┘       └────────┬─────────┘
                                                              │
                                                              ▼
                                        ┌──────────────────────────────┐
                                        │   watch::channel 广播        │
                                        │   → IdleManager              │
                                        │   → EmotionEvaluator         │
                                        │   → ArousalTracker           │
                                        │   → CognitiveEngine          │
                                        │   → ToolExecutor             │
                                        └──────────────────────────────┘
```

---

## 8. 实现位置

| 组件 | 文件 | 职责 |
|---|---|---|
| BackendHealth | `kernel/gateway/src/runtime/backend_health.rs` | 后端健康表 + 状态翻转 |
| BackendHealthRegistry | `kernel/gateway/src/runtime/agent_registry.rs` | 按 base_url 聚合共享 |
| CognitiveState | `kernel/gateway/src/runtime/cognitive_state.rs` | 四档意识状态机 |
| CognitiveStateConfig | 同上 | 阈值配置（coma_threshold 默认 15min） |
| Watch Channel | 同上 | 广播状态变更 |
| 主路径上报 | `cognitive/llm/` (LlmCognitiveEngine) | Ok/Err → record_success/failure |
| 服务降级 | `runtime/reflection.rs` + `runtime/sleep.rs` | Down 状态跳过 LLM |

---

> **参考：**
> - [认知状态模型设计文档](../ideas/cognitive-state-model.md)
> - [认知翻译层](../cognitive-memory.md) — Consciousness 在三个翻译器中的地位
> - [意识状态代码](../../kernel/gateway/src/runtime/cognitive_state.rs)
> - [后端健康代码](../../kernel/gateway/src/runtime/backend_health.rs)
