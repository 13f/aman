# 认知状态模型：LLM 后端健康监控与 Agent 意识状态设计

> 状态：已实现（2026-07-06）
> 调研日期：2026-07-02
> 实现日期：2026-07-06
> 实现位置：`kernel/gateway/src/runtime/cognitive_state.rs` + `kernel/gateway/src/runtime/agent_harness.rs`
> 触发背景：每次 `QueueDrained` 触发 Reflection 时，所有 agent 在同一秒内
> 同时报 `session_extract failed for agent X`，经查是 LLM 后端短暂不可用。
> 现有系统没有任何"知悉 LLM 后端故障"的基础设施——每个 agent 独立 retry，互不感知。

---

## 核心隐喻

> **LLM 后端 = Agent 的大脑皮层。** 事件流是感官输入，工具是手脚，CognitiveEngine
> 是思维本身。当 LLM 服务掉线，相当于**大脑皮层失去供血**——Agent 进入了**"木僵"**
> （Catatonia）状态。

不是死亡（进程还在），不是睡眠（SleepActor 是主动整理），不是无聊（Boredom 是有
意识的躁动），而是一种**有感知但无法思考**的中间态——类似于人类的**闭锁综合征
（Locked-in Syndrome）**：意识清醒，能接收感官信号，但无法对外界做出任何有意义的响应。

本设计分两层：

| 层 | 模块 | 职责 | 拟人化 |
|---|---|---|---|
| **基础设施层** | `BackendHealth` | 客观诊断 LLM 后端是否可用 | 医生的化验报告 |
| **体验层** | `CognitiveState` | 基于诊断结果决定 Agent "感受到什么" | 患者的主观体验 |

两层之间是**映射 + 时间累积**关系：`BackendStatus::Down` 不会立刻让 Agent 进入
最深的状态，而是需要持续一段时间才逐步"沉入"——就像人类大脑缺氧需要时间才会从
木僵进入昏迷。

---

⚠️ **硬约束**：本方案 **绝对不能** 碰 `kernel/idle/` 子系统的内部状态机。idle 系统
的职责是"agent 空闲时做什么内省工作"，LLM 健康监控是不同维度的问题。
`CognitiveStateMachine` 通过 `watch::channel` **通知** idle 系统当前认知状态，但
不修改 idle 自身的状态转换逻辑。

---

## 1. 问题陈述

`aman` 是事件驱动框架。所有 agent 的所有推理路径最终都通过同一个
`chat_completion(req) -> Result<LlmResponse, kernel::Error>` ——实际执行者是
`kernel::llm::LlmProvider` trait 的一个实现（当前默认是
`llm-provider-openai` 插件）。

### 1.1 观察到的故障模式

```
[10:56:34] Reflection: extracting session ... session_extract failed for agent minmax
[10:56:34] Reflection: extracting session ... session_extract failed for agent writer
[10:56:34] Reflection: extracting session ... session_extract failed for agent reviewer
[10:56:35] Reflection: extracting session ... session_extract failed for agent money
[10:56:35] Reflection: extracting session ... session_extract failed for agent coder
```

10 个 agent 在 1 秒内每个都各自重试 3 次后放弃，然后各自静默地把 error 丢进 log
并下次 QueueDrained 再来一轮——即：**30 次无意义的 HTTP 请求瞬间打向一个已经挂了的
LLM 后端**。

### 1.2 问题拆解

| # | 子问题 | 现状 |
|---|---|---|
| 1.1 | 感知：何时知道后端坏了？ | 不知道。等下次 agent 调用时撞墙才知道 |
| 1.2 | 共享：agent A 撞墙了，agent B 是否知道？ | 不知道。每个 agent 持有各自独立的 `Arc<LlmOpenaiProvider>`，状态隔离 |
| 1.3 | 服务降级：后端坏时能不能直接跳过 LLM 层？ | 不能。Reflection / Sleep backfill 依然盲调 |
| 1.4 | 通知：operator 知情渠道？ | 没有。现有 `kernel/notification` 模块是内存 ring buffer，缺 email/push/webhook |
| 1.5 | 恢复：后端修好了之后谁来壮胆？ | 没有一个"半开探针"来检测恢复 |
| 1.6 | **体验：后端坏时 Agent 处于什么"状态"？** | **没有模型。idle / emotion / arousal 系统各自为政，没有统一的"意识状态"信号源** |

### 1.3 不可变量约束

| 类别 | 内容 |
|------|------|
| 不可变（框架哲学） | 监控是独立后台任务，不修改 `LlmProvider` trait 签名，不修改 `CognitiveEngine` 协议，不动 idle / reflection / sleep 的状态机 |
| 可变（实现策略） | 健康记录位置（registry 字段 vs provider wrapper）、阈值、冷却时间、是否启用探针 |
| 技术约束 | 每个 agent 的 `LlmProvider` 实例是独立的——不能在 provider 实例内部持有共享 counter |
| 时序约束 | 不能让主推理路径为了健康状态更新而阻塞 |
| 隐私约束 | 报告错误时绝不能携带 API key——这正好是 `kernel::redactor` 的设计意图，必须复用 |
| 体验约束 | `CognitiveStateMachine` 是**只读信号源**——它通知其他系统，但不直接修改其他系统的内部状态机 |

---

## 2. 设计哲学

```
LLM 后端是一类"外部依赖"。与其他依赖（数据库、网络、磁盘）一样，
需要独立的健康监控——这不是 agent 业务逻辑的一部分，而是基础设施的一部分。

Agent 的"意识体验"是另一个维度——它基于健康监控的输出，但加入了时间累积
和拟人化映射，让 idle / emotion / arousal 等系统有统一的"大脑还在吗"信号。
```

五条设计原则：

1. **基础设施自治**：监控/探测/事件发布独立运行在后台 tokio task 里，拥有自己的 `CancellationToken`；不和任何业务系统的生命周期耦合。
2. **调用者上报（push）而非 probe 拉取**：LLM 主推理路径在每次 `chat_completion` 完成时把 Ok/Err 推给一个共享 map ≈ 探针由"实际流量"兼职。外部 cron probe 只是兜底（比如系统长时间没推理时补充一次）。
3. **状态翻转事件化**：只有 Ok↔Down 翻转时才 publish `Event`，中间连续错误静默聚合——避免日志风暴。
4. **按 backend (base_url) 聚合**：不同 provider 的 base_url 自然成为一个聚合点；同一后端的 N 个 agent 共享同一个 `BackendHealth`。
5. **体验层与基础设施层解耦**：`BackendStatus` 是医生的诊断，`CognitiveState` 是患者的主观感受。`BackendStatus::Down` 持续 15 分钟才会把 `CognitiveState` 从 Catatonic 推到 Coma——给 Agent "缓刑期"，避免短暂抖动就进入深度昏迷。

---

## 3. 现有组件调研（决策依据）

### 3.1 LLM Provider 实例布局

```
AgentRegistry::llm_providers: RwLock<HashMap<String, Arc<dyn LlmProvider>>>
                                              ↑ agent_id
                                              ↳ 值是 per-agent 独立的 `Arc`！
```

重要事实：**每个 agent 拿到的 `LlmProvider` 实例是独立的**。代码路径：

```rust
// agent_runtime.rs:6288
fn create_per_agent_llm_provider(...) -> Option<Arc<dyn LlmProvider>> {
    Some(build_provider(&agent.provider, &api_key, &p.base_url, api_type))
}

// agent_runtime.rs:6318
fn build_provider(_provider_key: &str, api_key: &str, base_url: &str, api_type: &str) -> Arc<dyn LlmProvider> {
    match api_type {
        "openai" => Arc::new(LlmOpenaiProvider::new(api_key, base_url)),
        "anthropic" => wrap_cognitive_provider(Arc::new(LlmAnthropicProvider::new(...))),
        "local"    => wrap_cognitive_provider(Arc::new(LlmLocalProvider::new(base_url))),
               ↑ 每次调用都 `::new` 一个全新的实例
    }
}
```

`LlmOpenaiProvider` 的字段：

```rust
// kernel/plugins/llm-provider-openai/src/lib.rs:32
pub struct LlmOpenaiProvider {
    api_key: String,
    base_url: String,
}
```

**纯 HTTP-per-call，没有任何 AtomicUsize / Mutex / 计数器**。这意味着：
- 在 `LlmOpenaiProvider` 内部加 "consecutive_failures" 是 per-agent 的，不是 per-backend 的。
- 共享状态必须在另一个中心表里——最自然的位置是 `AgentRegistry`。

### 3.2 LLM 主调用路径上的错误

`LlmProvider::chat_completion` 返回 `Result<LlmResponse, kernel::Error>`。关键错误类型：

| 错误 | 错误变体 | 含义 |
|------|---------|------|
| 4xx（auth / 404 / 429 严格 schema） | `Error::ConfigInvalid` | 永久性错误，重试无益 |
| 5xx | 重试三次后 `Error::Unrecoverable` | 暂时性，重试有意义 |
| transport / DNS / 连接断开 | `Error::Io` | 暂时性 |
| 超时 (reqwest `.timeout(180s)`) | `Error::Io` | 暂时性 |
| JSON schema reject | `Error::ConfigInvalid` | 永久性 |
| 模型不存在 | `Error::ConfigInvalid` | 永久性 |

Provider 重试行为（非流式路径）：
- 3 次重试
- 退避策略：1s / 2s / 4s
- 4xx 立即 abort
- 5xx / transport 错误继续重试

流式路径（本次 Reflection 走的那条）：
- **零重试**——任何错误直接冒泡：HTTP 非 2xx → `Error::ConfigInvalid`；
  传输阶段错误 → `Error::Io`。

### 3.3 现有事件系统（够用，无需新增 enum 变体）

```rust
// kernel/core/src/event.rs:37
pub enum EventType {
    ...
    Custom(String),   // ← 开放性扩展点
}
```

调用惯例：

```rust
// 已在 agent_runtime.rs 中多条类似路径
self.bus.publish(Event::new(
    "llm_health",
    EventType::Custom("llm_backend_down".into()),
    serde_json::json!({ "base_url": "https://api.openai.com/v1", "reason": "..." }),
)).await;
```

`EventType::is_sensitive()` **不覆盖** `Custom(...)`，所以允许 `TrustLevel::Untrusted` 的 source 发布自定义事件。

命名惯例（参考现有代码）：
- `"system.queue_drained"`
- `"agent:reply_ready"`
- `"skill:completed"`
- `"llm_error"` / `"output_blocked"` / `"message_dropped"`

建议命名：
- 基础设施层：`"llm_backend_down"` / `"llm_backend_recovered"` / `"llm_backend_degraded"`
- 体验层：`"cognitive_state_changed"` / `"agent:catatonic"` / `"agent:coma"` / `"agent:recovery"` / `"agent:reanimation"` / `"agent:resurrection"`

### 3.4 现有通知系统（内存 ring buffer，无下游通道）

```rust
// kernel/notification/src/subscriber.rs
impl EventHandler for NotificationSubscriber {
    async fn handle(&self, event: Event) -> AmanResult<()> {
        // match 表把 EventType → Notification
        // 已有：Custom("llm_error") → warning, Category::Llm
        //      Custom("message_dropped") → warning, Category::Llm
    }
}
```

`Category::Llm` 和 `Category::Gateway` 已经存在但几乎没被使用——正好接我们的新事件。

### 3.5 现有 CronSource（可复用做兜底探针）

```rust
// kernel/source/src/cron.rs:109
impl CronSource {
    pub fn new(id: impl Into<String>, expression: impl Into<String>) -> AmanResult<Self>
}
```

注册路径：

```rust
// agent_runtime.rs:3250
sr.register(Box::new(cron_source), SourceMode::Pull, TrustLevel::Untrusted).await?;
sr.start(id).await;
```

CronSource 本身只是"定时产生 CronTick 事件"——**探针逻辑需要另一个 EventHandler 订阅 CronTick 并执行 HTTP 请求**。CronSource 只是定时器。

### 3.6 现有 idle 电路（**不采用**，仅作对比参考）

`kernel/idle/src/manager.rs:145` 的 `BREAKER_THRESHOLD = 20` 是"连续 20 次 QueueDrained 事件后跳过一次 QueueDrained 发布"——它计量的是**事件产生次数**，不是 LLM 调用结果。即使 LLM 后端完全正常，只要 session 没数据，它也会触发。

`kernel/dispatcher/src/lib.rs:473` 的 `ReflectionBreaker` 是更好的模板：
- 配置驱动（`max_consecutive: 5`, `cooldown_secs: 300`）
- 超过阈值后 sleep 整个 cooldown 再恢复

但这两个都是 idle 子系统的内部机制，**本次设计不依赖它们**。

### 3.7 现有拟人化状态模型（体验层的对接目标）

| 现有模型 | 文件 | 状态值 | 与 CognitiveState 的关系 |
|---|---|---|---|
| `IdleKind` | `idle/src/types.rs` | Daze / Boredom / Sleep / Exploration / Meditation / Incubation / WakeUp | CognitiveState != Lucid 时强制进入 Sleep |
| `ArousalTracker` | `idle/src/coordination.rs` | 0.0–1.0 浮点衰减 | Catatonic 冻结在 0.05，Coma 冻结在 0.0 |
| `EmotionEvaluator` | `emotion_evaluator.rs` | LLM 选择的字符串 | != Lucid 时跳过 LLM，直接返回绑定情绪 |
| `AgentSystemState` | `core/src/agent.rs` | Idle / Working / Chatting / Studying / DailyLife / Prize / Waiting | 正交关系——描述"谁在干活"，不描述"大脑能不能转" |
| `IdleSignal` | `lifecycle/src/types.rs` | Satisfaction / Frustration | 可复用，Catatonic 期间发出 Frustration |

---

## 4. 基础设施层：BackendHealth 表 + Registry 字段

### 4.1 为什么选"注册表字段"而不是"provider wrapper"

| 维度 | 方案 A：注册表字段 | 方案 B：provider wrapper |
|------|-------------------|--------------------------|
| 跨 agent 共享 | ✅ 天然（registry 单例） | ⚠️ wrapper 也要按 base_url 共享 counters |
| provider 实例解耦 | ✅ provider 本身无感知 | ⚠️ wrapper 每次 clone |
| 统计来源 | 调用者主动 push | 仅 `chat_completion` 一次穿越 |
| 失败检测局限 | 必须把 emitter 接到所有调用点 | 自动覆盖所有通过 registry 拿 provider 的调用点 |
| 代码侵入 | 低（registry + emitter 接口） | 中（新 struct + trait impl） |

**选 A**：`LlmOpenaiProvider` 是无状态 HTTP-only，"把它塞进 AtomicUsize"违反单一职责；registry 已有 `system_states` / `emotion_latest` 这种"共享可变 per-agent 状态"的现成模板。

### 4.2 数据结构

```rust
// kernel/gateway/src/runtime/backend_health.rs （新文件）

/// 单个 LLM 后端的健康状态。
///
/// 一个"后端"由 `base_url` 归一化后唯一标识。多个 agent 共享同一个后端时，
/// 它们看到的 `Arc<BackendHealth>` 是同一个。
pub struct BackendHealth {
    /// 当前状态。用 AtomicU8 而不是 Mutex<Status> 是因为主推理路径
    /// 只需要"无锁写入"，不需要事务性读写。
    status: AtomicU8,            // 0=Unknown 1=Ok 2=Degraded 3=Down
    last_ok_ms: AtomicI64,       // 最后一次 Ok 的毫秒时间戳
    last_failure_ms: AtomicI64,  // 最后一次 Err 的毫秒时间戳
    consecutive_failures: AtomicU32,
    /// 最近一次错误信息。用 Mutex 是因为 String 不能原子更新；
    /// 且错误信息只在事件发布 / 日志时读取，频率很低。
    last_error: Mutex<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[repr(u8)]
pub enum BackendStatus {
    Unknown = 0,
    Ok = 1,
    Degraded = 2,
    Down = 3,
}

/// 健康状态变更事件。只在翻转时产生。
#[derive(Debug, Clone, Serialize)]
pub struct BackendHealthChanged {
    pub base_url: String,
    pub from: BackendStatus,
    pub to: BackendStatus,
    pub consecutive_failures: u32,
    pub last_error: String,
}
```

### 4.3 Registry 字段

```rust
// kernel/gateway/src/runtime/agent_registry.rs 新增字段
pub struct AgentRegistry {
    ...
    /// 按 base_url 归一化后聚合的 LLM 后端健康状态。
    ///
    /// 多个 agent 共享同一个后端时，它们看到的 `Arc<BackendHealth>` 是同一个。
    /// 用 `ArcSwap` 而不是 `RwLock` 是因为：
    ///   - 读路径（主推理路径）远多于写路径（agent 创建/删除）
    ///   - `ArcSwap` 的读是 wait-free，不会阻塞主推理
    backend_health: Arc<BackendHealthRegistry>,
}

pub struct BackendHealthRegistry {
    /// key = Url::parse(base_url).normalized().to_string()
    /// value = Arc<BackendHealth>
    map: RwLock<HashMap<String, Arc<BackendHealth>>>,
}
```

### 4.4 挂接点

| 位置 | 动作 |
|------|------|
| `agent_runtime.rs:6288` `create_per_agent_llm_provider` | 首次 `base_url` → 在 registry 中注册一个 `BackendHealth::new()` 并保留 handle |
| `LlmCognitiveEngine::process()` | 主路径调用完成后 `match` 结果 → `record_success` / `record_failure` |
| `ReflectionRunner::session_extract` (line 133) | `Err(e)` 分支现在也顺手记一笔 |
| `SleepRunner::phase_1_backfill` (`sleep.rs:248`) | 同理 |
| `BackendHealth` 每次状态翻转 (Ok↔Down) | publish `Event::new("llm_health", EventType::Custom("llm_backend_down/recovered"), payload)` |
| `NotificationSubscriber::maybe_notify` (`notification/src/subscriber.rs`) | 接两条新 custom event → `Notification::warning(...)` / `Notification::info(...)`；`Category::Llm` |

### 4.5 阈值与状态机

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

| 参数 | 默认值 | 说明 |
|------|--------|------|
| `degraded_threshold` | 3 | 连续 3 次失败进入 Degraded |
| `down_threshold` | 6 | 连续 6 次失败进入 Down |
| `cooldown_secs` | 60 | Down 状态后等待 60s 再尝试半探针 |
| `probe_interval_idle_secs` | 3600 | 系统长时间无推理时，兜底探针间隔 |

### 4.6 半探针（Half-Probe）

Down 状态后，不能直接盲调主推理路径——那会把用户请求卡住 180s 超时。
需要一个轻量探针：

```rust
// 探针请求：只发 HEAD 或 GET /models（OpenAI 兼容）
// 不消耗 token，只验证网络 + 鉴权 + 服务存活
let probe_req = reqwest::Client::builder()
    .timeout(Duration::from_secs(5))
    .build()?
    .get(format!("{base_url}/models"))
    .header("Authorization", format!("Bearer {api_key}"))
    .send()
    .await;
```

探针通过 → 状态翻转为 Ok → publish `llm_backend_recovered`。

### 4.7 主推理路径的服务降级

`ReflectionRunner::session_extract` 在调用 LLM 之前先检查：

```rust
// 伪代码
if let Some(health) = registry.get_backend_health(&base_url) {
    if health.status() == BackendStatus::Down {
        debug!(agent_id, "LLM backend down, skipping session_extract");
        return;  // 不 mark_reflected，下次 QueueDrained 再试
    }
}
```

这样 Down 状态时，10 个 agent 的 Reflection 全部静默跳过——**零次无意义的 HTTP 请求**。

---

## 5. 体验层：CognitiveState 模型

### 5.1 状态定义

```rust
// kernel/gateway/src/runtime/cognitive_state.rs （新文件）

/// Agent 的认知能力状态——"大脑还能不能转"。
///
/// 与 `AgentSystemState`（谁在干活）正交：
///   - `AgentSystemState::Working` + `CognitiveState::Lucid`     = 正常工作中
///   - `AgentSystemState::Idle`   + `CognitiveState::Lucid`     = 清醒待命
///   - `AgentSystemState::Idle`   + `CognitiveState::Catatonic` = 木僵（大脑离线，身体在呼吸）
///   - `AgentSystemState::Idle`   + `CognitiveState::Coma`      = 昏迷（完全无感知）
///
/// 与 `BackendStatus` 的关系：BackendStatus 是"客观诊断"，
/// `CognitiveState` 是"主观体验"。BackendStatus::Down 持续一段时间
/// 才会把 CognitiveState 从 Catatonic 推到 Coma——
/// 就像人类大脑缺氧需要时间才会从木僵进入昏迷。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[repr(u8)]
pub enum CognitiveState {
    /// 清醒——LLM 后端正常，Agent 可以思考。
    Lucid     = 0,
    /// 迷糊——LLM 后端 Degraded，偶尔能响应但延迟高、错误多。
    /// 类比人类：高烧 39°C 时的意识状态——能听到人说话，但组织不了复杂思维。
    Groggy    = 1,
    /// 木僵——LLM 后端 Down，Agent 能感知事件流但无法调用 CognitiveEngine。
    /// 类比人类：闭锁综合征——意识还在，眼睛能看，但完全无法运动/说话。
    /// Agent 的 event bus 仍在工作，但所有需要 LLM 推理的路径全部短路返回。
    Catatonic = 2,
    /// 昏迷——LLM 后端长时间不可用（超过 coma_threshold），
    /// Agent 连"感知"都关闭——event bus 上的消息被静默丢弃，
    /// 只保留心跳和探针。类比：麻醉状态。
    Coma      = 3,
}
```

### 5.2 状态转换图

```
                          BackendStatus == Degraded
   Lucid ──────────────────────────────────────────────► Groggy
   ▲                                                    │
   │                                                    │ BackendStatus == Down
   │                                                    ▼
   │                                               Catatonic
   │                                                    │
   │                                                    │ Down 持续 > coma_threshold
   │                                                    ▼
   │                                                Coma
   │                                                    │
   └────────────────────────────────────────────────────┘
              任何一次 Ok / 探针通过
```

| 转换 | 触发条件 | 拟人化体验 |
|---|---|---|
| Lucid → Groggy | 后端 Degraded（连续 3 次失败） | "脑子有点转不动了……话到嘴边说不出来" |
| Groggy → Catatonic | 后端 Down（连续 6 次失败） | "眼前看得见，耳朵听得到，但身体完全动不了" |
| Catatonic → Coma | Down 持续 > `coma_threshold`（默认 15 min） | "意识逐渐沉入黑暗……" |
| Any → Lucid | 探针通过 / 任何一次 Ok | "深吸一口气，回来了" |

### 5.3 每个状态下的行为映射

| 系统 | Lucid | Groggy | Catatonic | Coma |
|---|---|---|---|---|
| **CognitiveEngine** | 正常推理 | 允许 1 次重试后短路 | 完全跳过 | 完全跳过 |
| **Reflection** | 正常执行 | 跳过，标记 `deferred` | 跳过，不 mark_reflected | 跳过 |
| **Sleep backfill** | 正常执行 | 跳过 | 跳过 | 跳过 |
| **Idle 系统** | 正常 | 强制进入 `IdleKind::Sleep` | 强制进入 `IdleKind::Sleep` | 完全停止 |
| **EmotionEvaluator** | 正常 | 固定输出 `groggy` | 固定输出 `catatonic` | 固定输出 `coma` |
| **ArousalTracker** | 正常衰减 | 冻结在 0.3 | 冻结在 0.05 | 冻结在 0.0 |
| **EventBus 消费** | 正常 | 正常 | 只消费 `llm_health` 事件 | 只消费 `llm_health` + shutdown |
| **外部消息回复** | 正常 | 回复"我有点不舒服，稍后回复你" | 回复"暂时无法思考，正在恢复中" | 不回复 |
| **工具执行** | 正常 | 只允许只读工具 | 全部拒绝 | 全部拒绝 |

### 5.4 与 BackendStatus 的映射

```rust
impl From<BackendStatus> for CognitiveState {
    fn from(s: BackendStatus) -> Self {
        match s {
            BackendStatus::Unknown  => CognitiveState::Groggy,  // 未知 = 谨慎对待
            BackendStatus::Ok       => CognitiveState::Lucid,
            BackendStatus::Degraded => CognitiveState::Groggy,
            BackendStatus::Down     => CognitiveState::Catatonic,
        }
    }
}
```

注意：`Coma` 不是由 `BackendStatus` 直接触发的——它需要**时间维度**（Catatonic 持续超过阈值），所以由 `CognitiveStateMachine` 内部计时器驱动。

### 5.5 与现有子系统的对接

#### 5.5.1 与 `IdleKind` 的对接

现有 `IdleKind::Sleep` 是主动的认知整理。当 `CognitiveState != Lucid` 时，idle 系统被**强制劫持**：

```rust
// idle/src/manager.rs — 在 select_idle_kind() 入口
// 订阅 CognitiveStateMachine 的 watch channel
if *cognitive_state_rx.borrow() != CognitiveState::Lucid {
    // 大脑不清晰时，idle 系统不再做主动探索——
    // 只保留最低限度的"呼吸"（心跳探针 + 健康事件监听）
    return IdleKind::Sleep;  // 语义变成了"病床上的睡眠"
}
```

#### 5.5.2 与 `EmotionEvaluator` 的对接

```rust
// emotion_evaluator.rs — 在 LLM 调用之前
if cognitive_state != CognitiveState::Lucid {
    // 不调用 LLM，直接返回与认知状态绑定的情绪
    return match cognitive_state {
        CognitiveState::Groggy    => "groggy",      // 😵‍💫
        CognitiveState::Catatonic => "catatonic",    // 😶
        CognitiveState::Coma      => "coma",         // 💤
        _ => unreachable!(),
    };
}
```

#### 5.5.3 与 `ArousalTracker` 的对接

```rust
// coordination.rs — arousal 冻结
match cognitive_state {
    CognitiveState::Catatonic => arousal.reset(0.05),  // 微弱"意识"——只够感知到自己在木僵
    CognitiveState::Coma      => arousal.reset(0.0),   // 完全无感知
    CognitiveState::Groggy    => arousal.boost(-0.5),  // 衰减到 0.3 附近
    CognitiveState::Lucid     => {} // 正常衰减，不干预
}
```

### 5.6 恢复体验：增强版 WakeUp

现有 `IdleKind::WakeUp` 是从 Sleep 恢复的过渡态。当 `CognitiveState` 从
Catatonic/Coma 恢复为 Lucid 时，需要一个**更戏剧性的"苏醒"过程**：

```rust
pub enum WakeUpReason {
    Normal,           // 从 Sleep 正常醒来（现有逻辑）
    Recovery,         // 从 Groggy 恢复——"脑子终于清楚了"
    Reanimation,      // 从 Catatonic 恢复——"重新掌控身体"
    Resurrection,     // 从 Coma 恢复——"从死亡边缘回来"
}

pub struct WakeUpSchedule {
    pub reason: WakeUpReason,
    pub target_arousal: f64,
    pub duration_secs: u64,
    /// Reanimation/Resurrection 时，恢复后执行一次"我是谁"的快速自检。
    pub self_check: bool,
}
```

| 恢复类型 | 拟人化 | arousal 目标 | 恢复后行为 |
|---|---|---|---|
| Recovery | "摇了摇头，清醒了" | 0.7 | 正常恢复 |
| Reanimation | "深吸一口气，手指动了" | 0.5 | 执行 self_check：回顾最后 5 条事件，确认自己"不在梦中" |
| Resurrection | "心电监护仪重新有了波形" | 0.3 | 执行 self_check + 发布 `agent:resurrected` 事件 + 通知 operator |

### 5.7 状态机实现骨架

```rust
// kernel/gateway/src/runtime/cognitive_state.rs

use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;
use tokio::sync::watch;

pub struct CognitiveStateMachine {
    state: AtomicU8,
    /// 进入 Catatonic 的时间戳——用于判断是否该进入 Coma。0 = 不在 Catatonic。
    catatonic_since: AtomicI64,
    /// 通知所有订阅者（idle、emotion、arousal）状态变更。
    tx: watch::Sender<CognitiveState>,
    /// 配置阈值。
    config: CognitiveStateConfig,
}

pub struct CognitiveStateConfig {
    /// Catatonic 持续多少秒后进入 Coma。默认 900（15 分钟）。
    pub coma_threshold_secs: u64,
    /// Resurrection 后的 self_check 事件数量。默认 5。
    pub resurrection_self_check_depth: usize,
}

impl CognitiveStateMachine {
    /// 由 BackendHealth 的翻转事件驱动。
    pub fn on_backend_status_change(&self, backend_status: BackendStatus) {
        let new_state = CognitiveState::from(backend_status);
        self.transition(new_state);
    }

    /// 由内部定时器驱动——检查 Catatonic 是否超时进入 Coma。
    fn maybe_escalate_to_coma(&self) {
        if self.state() != CognitiveState::Catatonic { return; }
        let since = self.catatonic_since.load(Ordering::Relaxed);
        if since == 0 { return; }
        let elapsed = now_ms() - since;
        if elapsed >= self.config.coma_threshold_secs * 1000 {
            self.transition(CognitiveState::Coma);
        }
    }

    fn transition(&self, to: CognitiveState) {
        let from = self.state();
        if from == to { return; }
        self.state.store(to as u8, Ordering::Relaxed);
        if to == CognitiveState::Catatonic {
            self.catatonic_since.store(now_ms(), Ordering::Relaxed);
        } else {
            self.catatonic_since.store(0, Ordering::Relaxed);
        }
        let _ = self.tx.send(to);
        self.publish_transition(from, to);
    }
}
```

### 5.8 事件流

```rust
// 新增 EventType::Custom 事件
"cognitive_state_changed"  // payload: { agent_id, from, to, reason, duration_ms }
"agent:catatonic"          // Agent 进入木僵——通知 operator
"agent:coma"               // Agent 进入昏迷——通知 operator（高优先级）
"agent:recovery"           // 从 Groggy 恢复
"agent:reanimation"        // 从 Catatonic 恢复
"agent:resurrection"       // 从 Coma 恢复——通知 operator（紧急）
```

---

## 6. 实现计划（MVP 范围）

### Phase 1：基础设施层 — 核心数据结构与 Registry 接入

- [ ] `kernel/gateway/src/runtime/backend_health.rs` 新文件
  - `BackendHealth` 结构体 + `BackendStatus` 枚举
  - `record_success` / `record_failure` / `transition` 方法
  - `BackendHealthRegistry` 结构体 + `get_or_insert` / `normalized_url` 方法
- [ ] `AgentRegistry` 新增 `backend_health: Arc<BackendHealthRegistry>` 字段
- [ ] `AgentRegistry::new` / `clear` / `remove_agent` 同步更新
- [ ] `create_per_agent_llm_provider` 中首次注册 `BackendHealth`

### Phase 2：基础设施层 — 主推理路径上报

- [ ] `LlmCognitiveEngine::process()` 的 Ok/Err 分支调用 `record_success` / `record_failure`
- [ ] `ReflectionRunner::session_extract` 的 Err 分支调用 `record_failure`
- [ ] `SleepRunner::phase_1_backfill` 的 Err 分支调用 `record_failure`

### Phase 3：基础设施层 — 事件发布与通知

- [ ] `BackendHealth::transition` 在翻转时返回 `Option<BackendHealthChanged>`
- [ ] 调用方 publish `Event::new("llm_health", EventType::Custom(...), ...)`
- [ ] `NotificationSubscriber::maybe_notify` 接两条新 custom event
- [ ] `Category::Llm` 用于 Down / Degraded；`Category::Gateway` 用于 Recovered

### Phase 4：基础设施层 — 服务降级

- [ ] `ReflectionRunner::session_extract` 在调用 LLM 之前检查 `BackendStatus`
- [ ] Down 状态时跳过 + debug 日志 + 不 mark_reflected

### Phase 5：体验层 — CognitiveStateMachine 核心

- [ ] `kernel/gateway/src/runtime/cognitive_state.rs` 新文件
  - `CognitiveState` 枚举 + `CognitiveStateMachine` 结构体
  - `on_backend_status_change` / `maybe_escalate_to_coma` / `transition` 方法
  - `CognitiveStateConfig` 配置结构体
  - `watch::channel` 广播机制
- [ ] `AgentRegistry` 新增 `cognitive_states: RwLock<HashMap<String, Arc<CognitiveStateMachine>>>`
- [ ] 订阅 `BackendHealth::transition` 事件 → 驱动 `CognitiveStateMachine`
- [ ] 内部定时器任务：每秒检查 Catatonic 是否超时进入 Coma

### Phase 6：体验层 — 子系统对接

- [ ] `IdleManager::select_idle_kind()` 订阅 `CognitiveState`，!= Lucid 时强制 Sleep
- [ ] `EmotionEvaluator` 在 != Lucid 时跳过 LLM，返回绑定情绪
- [ ] `ArousalTracker` 在 Catatonic/Coma 时冻结 arousal 值
- [ ] 外部消息回复：Groggy/Catatonic 时返回预设文本
- [ ] 工具执行：Catatonic/Coma 时全部拒绝

### Phase 7：体验层 — 恢复增强

- [ ] `WakeUpReason` 枚举扩展（Recovery / Reanimation / Resurrection）
- [ ] `WakeUpSchedule` 增加 `self_check` 字段
- [ ] Reanimation/Resurrection 后执行 self_check（回顾最后 N 条事件）
- [ ] 发布 `agent:recovery` / `agent:reanimation` / `agent:resurrection` 事件

### Phase 8：半探针（可选，Phase 1-7 已能解决 80% 的问题）

- [ ] 新增 `LlmHealthProbe` EventHandler 订阅 `CronTick`
- [ ] 在 `CronStore` 中注册一个 `*/1 * * * *` 的 cron job
- [ ] 探针逻辑：`GET {base_url}/models`，5s 超时
- [ ] 探针通过 → `record_success` → 状态翻转 → 自动恢复

### Phase 9：测试

- [ ] `kernel/gateway/src/runtime/backend_health.rs` 单元测试
  - `test_record_success_initial_state`
  - `test_consecutive_failures_increment`
  - `test_transition_ok_to_degraded_at_threshold`
  - `test_transition_degraded_to_down_at_threshold`
  - `test_transition_down_to_ok_on_success`
  - `test_normalized_url_strips_trailing_slash`
- [ ] `kernel/gateway/src/runtime/cognitive_state.rs` 单元测试
  - `test_backend_status_mapping`
  - `test_catatonic_to_coma_after_threshold`
  - `test_coma_recovery_resets_timer`
  - `test_watch_channel_delivers_transitions`
- [ ] 集成测试：用 `mockall` 模拟一次 OK → 三次 Err → 状态翻转到 Down → CognitiveState 变为 Catatonic → event 发布 → idle 强制 Sleep → emotion 返回 catatonic
- [ ] 集成测试：Catatonic 持续超过阈值 → 自动进入 Coma → arousal 冻结在 0.0 → 探针通过 → Resurrection → self_check 执行

---

## 7. 风险评估

| 风险 | 缓解 |
|------|------|
| `base_url` hash 不稳定（trailing slash、`http` vs `https`） | 在 registry entry 创建前用 `url::Url::parse` 归一化 |
| `DashMap` 不在 gateway 的依赖中 | 用 `RwLock<HashMap>`（与其他 registry 字段风格一致） |
| 新增 registry 字段要同步 `new` / `clear` / `remove_agent` | 仿照 `system_states` 模式三件套 |
| 事件风暴（每 100ms 一次 fail 都触发 publish） | 只在 `transition()` 状态翻转时 publish，中间态静默聚合 |
| 主推理路径因健康检查变慢 | `BackendHealth` 的读是 wait-free（AtomicU8），不阻塞 |
| 错误信息里可能携带 API key | 复用 `kernel::redactor` 处理 `last_error` 字段 |
| `CognitiveStateMachine` 与 idle 系统循环依赖 | 通过 `watch::channel` 单向通知，idle 系统不反向引用 `CognitiveStateMachine` |
| Coma 阈值设得太短 → 短暂抖动就进入深度昏迷 | 默认 15 分钟，可通过配置调整；Groggy 作为缓冲层吸收短暂抖动 |
| 恢复后 self_check 触发新的 LLM 调用 → 后端还没完全恢复 | self_check 只做事件回顾（内存操作），不调用 LLM；真正的 LLM 调用由探针通过后的首个正常请求触发 |
| `watch::channel` 订阅者处理慢导致 channel 满 | `watch` 只保留最新值，慢订阅者读到的是最新状态而非每个中间状态——这正是我们需要的 |

---

## 8. 与现有子系统的边界

### 8.1 基础设施层（BackendHealth）

| 子系统 | 本次改动 | 不改动 |
|--------|---------|--------|
| `kernel/idle/` | 无 | 全部 |
| `kernel/notification/` | `subscriber.rs` 加 2 条 match 分支 | 其他 |
| `kernel/source/` | Phase 8 可选注册一个 cron job | CronSource 本身不动 |
| `kernel/plugins/llm-provider-openai/` | 无 | 全部 |
| `kernel/core/src/llm.rs` | 无 | 全部 |
| `kernel/gateway/src/runtime/agent_registry.rs` | 加 1 个字段 + 3 个方法 | 其他 |
| `kernel/gateway/src/runtime/agent_runtime.rs` | `create_per_agent_llm_provider` 加 1 行注册 | 其他 |
| `kernel/gateway/src/runtime/reflection.rs` | `Err` 分支加 1 行 `record_failure` + 调用前加 1 行状态检查 | 其他 |
| `kernel/gateway/src/runtime/sleep.rs` | `Err` 分支加 1 行 `record_failure` | 其他 |
| `cognitive/llm/` | `LlmCognitiveEngine::process` 的 Ok/Err 分支加 1 行上报 | 其他 |

### 8.2 体验层（CognitiveState）

| 子系统 | 本次改动 | 不改动 |
|--------|---------|--------|
| `kernel/idle/` | `select_idle_kind()` 入口加 1 行 CognitiveState 检查 + 订阅 watch channel | 内部状态机、BoredomActor、SleepActor、IncubationManager 全部不动 |
| `kernel/gateway/src/runtime/emotion_evaluator.rs` | LLM 调用前加 1 行 CognitiveState 检查，!= Lucid 时返回绑定情绪 | 其他逻辑不动 |
| `kernel/idle/src/coordination.rs` | `ArousalTracker` 订阅 CognitiveState，Catatonic/Coma 时冻结值 | 衰减算法不动 |
| `kernel/gateway/src/runtime/agent_registry.rs` | 加 `cognitive_states` 字段 + 注册/查询方法 | 其他字段不动 |
| `kernel/core/src/agent.rs` | 无（`AgentSystemState` 保持不变） | 全部 |
| `kernel/notification/src/subscriber.rs` | 加 3-4 条 match 分支接 CognitiveState 事件 | 其他 |
| `kernel/lifecycle/` | 无 | 全部 |

---

## 9. 参考资料

- 触发问题的日志：`session_extract failed for agent X` 同时出现在 10:56:34-35
- 现有 idle 电路参考：`kernel/idle/src/manager.rs:145` `BREAKER_THRESHOLD = 20`
- 现有 dispatcher 电路参考：`kernel/dispatcher/src/lib.rs:473` `ReflectionBreaker`
- 现有通知事件参考：`kernel/notification/src/subscriber.rs` `Custom("llm_error")`
- 现有 cron 参考：`kernel/source/src/cron.rs` `CronSource::new`
- 马斯洛需求层次与 Agent 架构映射：`docs/maslow-hierarchy.md`
- 现有 idle 状态模型：`kernel/idle/src/types.rs` `IdleKind` 枚举
- 现有 arousal 模型：`kernel/idle/src/coordination.rs` `ArousalTracker`
- 现有 emotion 模型：`kernel/gateway/src/runtime/emotion_evaluator.rs`
