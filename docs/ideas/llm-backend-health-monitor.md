# LLM 后端健康监控

> 状态：设计方案（等待实现）
> 调研日期：2026-07-02
> 触发背景：每次 `QueueDrained` 触发 Reflection 时，所有 agent 在同一秒内
> 同时报 `session_extract failed for agent X`，经查是 LLM 后端短暂不可用。
> 现有系统没有任何"知悉 LLM 后端故障"的基础设施——每个 agent 独立 retry，互不感知。

⚠️ **硬约束**：本方案 **绝对不能** 碰 `kernel/idle/` 子系统。idle 系统的职责是
"agent 空闲时做什么内省工作"，LLM 健康监控是不同维度的问题。下面是独立工作流、
独立 CancellationToken、独立后台任务——与 `AgentIdleManager` 唯一的关系是"共存在同一个进程里"。

---

## 1. 问题陈述

`aman` 是事件驱动框架。所有 agent 的所有推理路径最终都通过同一个
`chat_completion(req) -> Result<LlmResponse, kernel::Error>` ——实际执行者是
`kernel::llm::LlmProvider` trait 的一个实现（当前默认是
`llm-provider-openai` 插件）。

### 观察到的故障模式

```
[10:56:34] Reflection: extracting session ... session_extract failed for agent minmax
[10:56:34] Reflection: extracting session ... session_extract failed for agent writer
[10:56:34] Reflection: extracting session ... session_extract failed for agent reviewer
[10:56:35] Reflection: extracting session ... session_extract failed for agent money
[10:56:35] Reflection: extracting session ... session_extract failed for agent coder
```

10 个 agent 在 1 秒内每个都各自重试 3 次后放弃，然后各自静默地把 error 丢进 log
并下次 QueueDrained 再来一轮——即：**30 次无意义的 HTTP 请求瞬间打向一个已经挂了的 LLM 后端**。

### 问题拆解

| # | 子问题 | 现状 |
|---|---|---|
| 1.1 | 感知：何时知道后端坏了？ | 不知道。等下次 agent 调用时撞墙才知道 |
| 1.2 | 共享：agent A 撞墙了，agent B 是否知道？ | 不知道。每个 agent 持有各自独立的 `Arc<LlmOpenaiProvider>`，状态隔离 |
| 1.3 | 服务降级：后端坏时能不能直接跳过 LLM 层？ | 不能。Reflection / Sleep backfill 依然盲调 |
| 1.4 | 通知：operator 知情渠道？ | 没有。现有 `kernel/notification` 模块是内存 ring buffer，缺 email/push/webhook |
| 1.5 | 恢复：后端修好了之后谁来壮胆？ | 没有一个"半开探针"来检测恢复 |

### 不可变量约束

| 类别 | 内容 |
|------|------|
| 不可变（框架哲学） | 监控是独立后台任务，不修改 `LlmProvider` trait 签名，不修改 `CognitiveEngine` 协议，不动 idle / reflection / sleep 的状态机 |
| 可变（实现策略） | 健康记录位置（registry 字段 vs provider wrapper）、阈值、冷却时间、是否启用探针 |
| 技术约束 | 每个 agent 的 `LlmProvider` 实例是独立的——不能在 provider 实例内部持有共享 counter |
| 时序约束 | 不能让主推理路径为了健康状态更新而阻塞 |
| 隐私约束 | 报告错误时绝不能携带 API key——这正好是 `kernel::redactor` 的设计意图，必须复用 |

---

## 2. 设计哲学

```
LLM 后端是一类"外部依赖"。与其他依赖（数据库、网络、磁盘）一样，
需要独立的健康监控——这不是 agent 业务逻辑的一部分，而是基础设施的一部分。
```

四条设计原则：

1. **基础设施自治**：监控/探测/事件发布独立运行在后台 tokio task 里，拥有自己的 `CancellationToken`；不和任何业务系统的生命周期耦合。
2. **调用者上报（push）而非 probe 拉取**：LLM 主推理路径在每次 `chat_completion` 完成时把 Ok/Err 推给一个共享 map ≈ 探针由"实际流量"兼职。外部 cron probe 只是兜底（比如系统长时间没推理时补充一次）。
3. **状态翻转事件化**：只有 Ok↔Down 翻转时才 publish `Event`，中间连续错误静默聚合——避免日志风暴。
4. **按 backend (base_url) 聚合**：不同 provider 的 base_url 自然成为一个聚合点；同一后端的 N 个 agent 共享同一个 `BackendHealth`。

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

建议命名：`"llm_backend_down"` / `"llm_backend_recovered"` / `"llm_backend_degraded"`。

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

---

## 4. 推荐设计：BackendHealth 表 + Registry 字段

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

## 5. 实现计划（MVP 范围）

### Phase 1：核心数据结构与 Registry 接入

- [ ] `kernel/gateway/src/runtime/backend_health.rs` 新文件
  - `BackendHealth` 结构体 + `BackendStatus` 枚举
  - `record_success` / `record_failure` / `transition` 方法
  - `BackendHealthRegistry` 结构体 + `get_or_insert` / `normalized_url` 方法
- [ ] `AgentRegistry` 新增 `backend_health: Arc<BackendHealthRegistry>` 字段
- [ ] `AgentRegistry::new` / `clear` / `remove_agent` 同步更新
- [ ] `create_per_agent_llm_provider` 中首次注册 `BackendHealth`

### Phase 2：主推理路径上报

- [ ] `LlmCognitiveEngine::process()` 的 Ok/Err 分支调用 `record_success` / `record_failure`
- [ ] `ReflectionRunner::session_extract` 的 Err 分支调用 `record_failure`
- [ ] `SleepRunner::phase_1_backfill` 的 Err 分支调用 `record_failure`

### Phase 3：事件发布与通知

- [ ] `BackendHealth::transition` 在翻转时返回 `Option<BackendHealthChanged>`
- [ ] 调用方 publish `Event::new("llm_health", EventType::Custom(...), ...)`
- [ ] `NotificationSubscriber::maybe_notify` 接两条新 custom event
- [ ] `Category::Llm` 用于 Down / Degraded；`Category::Gateway` 用于 Recovered

### Phase 4：服务降级

- [ ] `ReflectionRunner::session_extract` 在调用 LLM 之前检查 `BackendStatus`
- [ ] Down 状态时跳过 + debug 日志 + 不 mark_reflected

### Phase 5：半探针（可选，Phase 1-4 已能解决 80% 的问题）

- [ ] 新增 `LlmHealthProbe` EventHandler 订阅 `CronTick`
- [ ] 在 `CronStore` 中注册一个 `*/1 * * * *` 的 cron job
- [ ] 探针逻辑：`GET {base_url}/models`，5s 超时
- [ ] 探针通过 → `record_success` → 状态翻转 → 自动恢复

### Phase 6：测试

- [ ] `kernel/gateway/src/runtime/backend_health.rs` 单元测试
  - `test_record_success_initial_state`
  - `test_consecutive_failures_increment`
  - `test_transition_ok_to_degraded_at_threshold`
  - `test_transition_degraded_to_down_at_threshold`
  - `test_transition_down_to_ok_on_success`
  - `test_normalized_url_strips_trailing_slash`
- [ ] 集成测试：用 `mockall` 模拟一次 OK → 三次 Err → 状态翻转到 Down → event 发布

---

## 6. 风险评估

| 风险 | 缓解 |
|------|------|
| `base_url` hash 不稳定（trailing slash、`http` vs `https`） | 在 registry entry 创建前用 `url::Url::parse` 归一化 |
| `DashMap` 不在 gateway 的依赖中 | 用 `RwLock<HashMap>`（与其他 registry 字段风格一致） |
| 新增 registry 字段要同步 `new` / `clear` / `remove_agent` | 仿照 `system_states` 模式三件套 |
| 事件风暴（每 100ms 一次 fail 都触发 publish） | 只在 `transition()` 状态翻转时 publish，中间态静默聚合 |
| 主推理路径因健康检查变慢 | `BackendHealth` 的读是 wait-free（AtomicU8），不阻塞 |
| 错误信息里可能携带 API key | 复用 `kernel::redactor` 处理 `last_error` 字段 |

---

## 7. 与现有子系统的边界

| 子系统 | 本次改动 | 不改动 |
|--------|---------|--------|
| `kernel/idle/` | 无 | 全部 |
| `kernel/notification/` | `subscriber.rs` 加 2 条 match 分支 | 其他 |
| `kernel/source/` | Phase 5 可选注册一个 cron job | CronSource 本身不动 |
| `kernel/plugins/llm-provider-openai/` | 无 | 全部 |
| `kernel/core/src/llm.rs` | 无 | 全部 |
| `kernel/gateway/src/runtime/agent_registry.rs` | 加 1 个字段 + 3 个方法 | 其他 |
| `kernel/gateway/src/runtime/agent_runtime.rs` | `create_per_agent_llm_provider` 加 1 行注册 | 其他 |
| `kernel/gateway/src/runtime/reflection.rs` | `Err` 分支加 1 行 `record_failure` + 调用前加 1 行状态检查 | 其他 |
| `kernel/gateway/src/runtime/sleep.rs` | `Err` 分支加 1 行 `record_failure` | 其他 |
| `cognitive/llm/` | `LlmCognitiveEngine::process` 的 Ok/Err 分支加 1 行上报 | 其他 |

---

## 8. 参考资料

- 触发问题的日志：`session_extract failed for agent X` 同时出现在 10:56:34-35
- 现有 idle 电路参考：`kernel/idle/src/manager.rs:145` `BREAKER_THRESHOLD = 20`
- 现有 dispatcher 电路参考：`kernel/dispatcher/src/lib.rs:473` `ReflectionBreaker`
- 现有通知事件参考：`kernel/notification/src/subscriber.rs` `Custom("llm_error")`
- 现有 cron 参考：`kernel/source/src/cron.rs` `CronSource::new`
