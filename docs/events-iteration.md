# Event Bus 双层架构设计审查

> 审查日期：2026-05-21
> 审查范围：`multi-agents-refactor.md`、`agent-design.md`、`architect-design.md`、`events.md`、代码实现 (`agent_runtime.rs`, `agent_registry.rs`, `agent_harness.rs`)

---

## 审查结论

**Agent 应该拥有独立的 Event Bus，采用双层 Bus 架构：Global Bus + Per-Agent Local Bus。**

---

## 1. 现状分析

当前 Aman 架构中存在**全局唯一**的 EventBus（`agent_runtime.rs:190-197`）：

```
AgentRuntimeBuilder::build()
  → build_runtime_bus() → 唯一的 bus: Arc<dyn EventBus>
  → AgentRegistry::new(bus) — 共享同一 bus
  → AgentHarness::new(bus) — 共享同一 bus
  → ToolExecutor::new(bus) — 共享同一 bus
  → LlmReActEngine::new(bus) — 共享同一 bus
  → SourceRegistry::new(bus) — 共享同一 bus
```

所有 agent 的生命周期事件（`agent:registered`、`agent:removed`、`agent:status_changed`）、LLM 调用事件（`llm:call_started`、`llm:call_ended`）、Tool 执行事件（`tool:dispatched`、`tool:completed`）全部泵入同一条总线。

---

## 2. 为什么当前设计对单 Agent 是合理的

多 Agent 共享一条 Bus 在以下条件下工作正常：

- 订阅者通过 `SubscriptionFilter` 按 `event_type` + `source` + `payload_match` 过滤
- Agent 间通过 `agent:message` 事件（带 `to_agent` payload）做路由
- 所有 agent 的 ReAct 循环是独立的 spawn task，不互相阻塞

但这建立在**所有 agent 共享同一个进程、同一个运行时**的假设上。

---

## 3. 为什么需要 Per-Agent Event Bus

从 `multi-agents-refactor.md` 的设计来看，Agent 将是独立实体，各自有不同的 SOUL、不同的 provider/model、不同的 tool/skill 权限。Agent 应该拥有独立 Event Bus 的四个理由：

### 3.1 隔离性 (Isolation)

当前设计中，Agent A 的 `llm:call_started` 事件会经过全局 Bus。任何订阅了该事件类型的 handler 都会收到所有 agent 的 LLM 调用。这意味着：
- 一个 agent 的背压会影响其他 agent（共享队列）
- 一个 agent 产生的事件洪峰（如 tool 循环）会挤占其他 agent 的事件通道
- 无法按 agent 维度做独立的背压策略

### 3.2 安全性 (Security)

`agent-design.md` 和 `multi-agents-refactor.md` 都强调 agent 间需要隔离。SOUL 身份、tool 白名单、skill 过滤都已经是 per-agent 的。但 EventBus 不隔离意味着：
- Agent A 的 tool 调用事件可能被 Agent B 的订阅者观察到
- 敏感操作（如 `tool:invoke` 的参数）暴露给所有订阅者

### 3.3 可伸缩性 (Scalability)

未来如果 agent 跑在不同进程/容器中（`architect-design.md` 提到 WASM 沙箱、子进程隔离），共享一个 in-process EventBus 根本不可能。Per-agent bus 是进程边界的前提。

### 3.4 背压粒度 (Backpressure Granularity)

当前 5 级背压（L1 80% → L2 90% → L3 95% → L4 98%）是全局的。Agent A 的消息洪峰触发的背压会影响 Agent B 的正常工作。Per-agent bus 可以做到：
- Agent A 的 queue 满了 → 只 block Agent A 的 publisher
- Agent B 继续正常运行

---

## 4. 目标架构：双层 Bus 设计

```
┌─────────────────────────────────────────────────────────┐
│                    Global Event Bus                       │
│  (基础设施事件: gateway:*, source:*, agent:lifecycle)      │
│  低吞吐、高可靠性、全局可见                                  │
└────────────┬──────────────────────────────┬──────────────┘
             │                              │
    ┌────────▼────────┐            ┌───────▼────────┐
    │ Agent "cortana" │            │ Agent "coder"   │
    │   Local Bus     │            │   Local Bus     │
    │                  │            │                 │
    │ llm:call_started │            │ llm:call_started│
    │ llm:call_ended   │            │ llm:call_ended  │
    │ tool:dispatched  │            │ tool:dispatched │
    │ tool:completed   │            │ tool:completed  │
    │ agent:reply_*    │            │ agent:reply_*   │
    │ (高吞吐、隔离)    │            │ (高吞吐、隔离)   │
    └──────────────────┘            └─────────────────┘
```

### 4.1 Global Bus（保留）

负责低吞吐、全局可见的基础设施事件：

| 事件 | 类别 |
|------|------|
| `gateway:starting` / `gateway:ready` / `gateway:stopping` | 网关生命周期 |
| `FileCreated` / `FileChanged` / `FileDeleted` | 文件系统事件 |
| `CronTick` / `TimerTick` / `Heartbeat` | 定时器事件 |
| `WebhookReceived` / `SystemSignal` | 外部输入事件 |
| `agent:registered` / `agent:removed` / `agent:status_changed` | Agent 生命周期 |
| `agent:message`（带 `to_agent` 路由） | 跨 Agent 通信 |
| `ConfigChanged` / `SkillReloaded` / `soul_changed` | 系统配置变更 |

### 4.2 Local Bus（新增，每个 Agent 一个实例）

负责高吞吐、仅该 Agent 可见的内部事件：

| 事件 | 类别 |
|------|------|
| `llm:call_started` / `llm:call_ended` / `llm_error` | LLM 调用 |
| `agent:reply_stream_start` / `agent:reply_chunk` / `agent:reply_stream_done` | 流式响应 |
| `tool:dispatched` / `tool:completed` / `tool:failed` | Tool 执行 |
| `agent:token_used` | Token 使用统计 |
| `session:started` / `session:closed` | Session 生命周期 |
| `message:dispatch` / `message:completed` | Skill 分发 |

---

## 5. 对 multi-agents-refactor.md 的修改建议

当前 `multi-agents-refactor.md` 第 7 节（事件系统）没有提及 per-agent bus。建议补充：

### 在第 7 节后新增 7.4

```markdown
### 7.4 Per-Agent Event Bus 隔离

每个 Agent 实例拥有独立的 Local EventBus。事件按以下规则路由：

| 事件类别 | 目标 Bus | 路由规则 |
|---------|----------|---------|
| Agent 生命周期 (agent:created/deleted/selected) | Global Bus | 全局可见 |
| LLM 调用 (llm:call_started/ended/error) | Local Bus | 仅该 Agent 可见 |
| Tool 执行 (tool:dispatched/completed/failed) | Local Bus | 仅该 Agent 可见 |
| Streaming (agent:reply_stream_*) | Local Bus | 仅该 Agent 可见 |
| 跨 Agent 消息 (agent:message) | Global Bus | Dispatcher 按 to_agent 路由到目标 Agent 的 Local Bus |
| Session (session:started/closed) | Local Bus | 仅该 Agent 可见 |
| 系统源事件 (FileCreated, CronTick 等) | Global Bus | 全局可见，Dispatcher 按 agent 的 source subscription 分发 |

Local Bus 继承 Global Bus 的背压机制（5 级），但队列独立：
- 每个 Agent 可配置独立的 max_queue_size（默认 1000，低于 Global 的 10000）
- Agent A 的背压不影响 Agent B
```

### 对 config crate 的改动（第 4 节修改）

`AgentEntryConfig` 需要新增 `event_bus` 配置段：

```rust
pub struct AgentEntryConfig {
    pub display_name: String,
    pub provider: String,
    pub model: String,
    pub system_prompt_override: Option<String>,
    // 新增：per-agent event bus 配置
    #[serde(default)]
    pub event_bus: Option<PartialEventBusConfig>,
}
```

配置示例：

```yaml
agents:
  cortana:
    display_name: Cortana
    provider: openai
    model: gpt-5.4-flash
    event_bus:
      max_queue_size: 2000   # cortana 高频调用，需要更大队列
  coder:
    display_name: Coder
    provider: deepseek
    model: deepseek-v4-pro
    # 不配置则使用默认值（max_queue_size: 1000）
```

---

## 6. 实施评估

### 6.1 代码改动范围

| 改动范围 | 工作量 | 说明 |
|---------|--------|------|
| `event-bus` crate | 无需改动 | `InMemoryBus` 本身支持多实例 |
| `config` crate | ~30 lines | 给 `AgentEntryConfig` 加 `event_bus` 字段 |
| `agent_runtime.rs` | ~100 lines | `build()` 中为每个 agent 创建独立 Bus；Local/Global 事件分类 |
| `agent_harness.rs` | ~30 lines | `LlmReActEngine` + `ToolExecutor` 注入 local bus 而非 global bus |
| `agent_registry.rs` | ~20 lines | 注册 agent 时创建其 Local Bus 并存储 |
| `multi-agents-refactor.md` | ~20 lines | 补充 7.4 节 |
| 测试 | ~100 lines | Per-agent bus 隔离验证 |

**总改动量小**：核心原因是 `InMemoryBus` 已经设计为独立实例可用。`AgentHarness`、`ToolExecutor`、`LlmReActEngine` 通过 `Arc<dyn EventBus>` trait object 接收 bus，只需要注入不同的实例即可。

### 6.2 关键实现路径

```rust
// agent_runtime.rs build() — 当前：
let bus: Arc<dyn EventBus> = Arc::new(bus);
let agent_registry = Arc::new(AgentRegistry::new(Arc::clone(&bus)));

// agent_runtime.rs build() — 目标：
let global_bus: Arc<dyn EventBus> = Arc::new(bus);
let agent_registry = Arc::new(AgentRegistry::new(
    Arc::clone(&global_bus),  // AgentRegistry 仍用 Global Bus 发生命周期事件
));
// 为每个 agent 创建 Local Bus
for (agent_id, _) in config.agents {
    let local_bus = Arc::new(InMemoryBus::new(local_config));
    agent_registry.set_local_bus(&agent_id, local_bus).await;
}

// agent_harness.rs — AgentHarness::spawn_process_message() 中：
let local_bus = agent_registry.get_local_bus(&agent_id).await;
let engine = LlmReActEngine::new(tools, agent_registry, local_bus, llm, prompt);
```

### 6.3 风险与缓解

| 风险 | 概率 | 缓解 |
|------|------|------|
| Global Bus 与 Local Bus 之间事件丢失 | 低 | Local Bus 不向 Global Bus 转发 — 两类事件泾渭分明，不存在跨 bus 路由 |
| agent:message 路由延迟 | 低 | `agent:message` 仍在 Global Bus 上，Dispatcher 按 `to_agent` 注入目标 Agent 的 Local Bus，无额外跳数 |
| 现有测试依赖 Global Bus | 中 | 单 agent 场景下，Global Bus 就是唯一的 Bus，行为不变；多 agent 测试需新增 Local Bus 注入 |

---

## 7. 总结

当前架构是"一个 Bus 管所有"，对单 agent 场景足够，但对 multi-agent 存在三个硬伤：

1. **无隔离** — Agent A 的内部事件暴露给 Agent B
2. **无独立背压** — 一个 agent 的洪峰拖垮所有 agent
3. **不可跨进程** — 共享 in-process Bus 无法支持未来的进程/容器隔离

建议采用双层 Bus 设计：**Global Bus** 保留给基础设施和跨 agent 通信，每个 Agent 持有独立的 **Local Bus** 承载自身的高吞吐事件。这个改动对现有代码侵入极小（`InMemoryBus` 天然支持多实例），但在架构层面是 agent 真正独立的前提。
