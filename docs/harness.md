# Agent Harness — 架构设计

> Agent Harness 是连接 Aman 事件响应式基础设施与 LLM Agent 行为的桥梁层。
> 它将"万物皆事件"的设计公理延伸到 Agent 的思考-行动-观察循环中。

---

## 1. 核心总览

### 1.1 什么是 Agent Harness

Agent Harness 是 LLM Agent 的运行时执行引擎，负责将**一个用户消息**转化为一个完整的 **Think-Act-Observe 循环**（ReAct Loop）。它协调 SOUL（身份）、Tool（能力）、Session（会话）、Memory（记忆）四个子系统，在 Event Bus 之上构建 Agent 的执行语义。

### 1.2 分层定位

```
┌──────────────────────────────────────────────────────────┐
│               Agent Harness（本设计）                        │
│                                                           │
│  Agent Identity    │  ReAct Loop Engine  │  Tool Access   │
│  Session Manager   │  Context Assembler  │  Token Budget  │
│  Multi-Agent Bus   │  Memory Integrator  │  Interrupt Ctrl│
├──────────────────────────────────────────────────────────┤
│               Aman 事件响应式基础设施                          │
│                                                           │
│  Event Bus  │  Dispatcher  │  Pipeline  │  Workflow      │
│  ToolRunner │  Plugin Sys  │  SOUL      │  State Store   │
│  Idle Sys   │  Secret Mgr  │  Backpressure               │
└──────────────────────────────────────────────────────────┘
```

### 1.3 Harness 在事件流中的位置

```
ChatPlatformSource → Event Bus → Dispatcher
                                      │
                                      ▼
                              ┌───────────────┐
                              │  AgentHarness  │  ← 新增层
                              │  (ReAct Loop)  │
                              └───────┬───────┘
                                      │
                          ┌───────────┼───────────┐
                          ▼           ▼           ▼
                    LLM Provider   ToolRunner   State Store
                    (API Call)     (Exec Tools)  (Memory)
```

Harness 本身不是一个独立的进程或组件——它是一套协调逻辑，运行在 Dispatcher 路由到的处理器中。但它定义了新的 Event 类型和状态机来管理 Agent 的执行。

---

## 2. Harness 能力与 Aman 能力映射

| # | Harness 能力 | 定义 | Aman 已有组件 | 实现状态 | 差距 |
|---|-------------|------|-------------|---------|------|
| 1 | **Agent 身份与生命周期** | Agent 的注册、创建、销毁、配置管理 | `config.yaml` agents 段 + SOUL 系统 + `~/.aman/agents/` 目录 + `AgentRegistry` | ✅ 已实现 | 无运行时动态创建/销毁 API（仅通过 config 加载） |
| 2 | **ReAct 循环引擎** | Think-Act-Observe 迭代：LLM 响应 → 解析 Tool Calls → 执行 → 反馈 → 继续 | `AgentHarness` + `LlmReActEngine` + `ToolExecutor` | ✅ 已实现 | — |
| 3 | **Context 组装** | System Prompt + 历史会话 + Tools Schema + 用户消息组合与 Token 预算管理 | `ContextAssembler` + SOUL + `TokenBudget` | ✅ 已实现 | — |
| 4 | **Tool Calling 调度** | 多 Tool Call 的调度策略（串行/并行/部分并行）、错误处理、结果聚合 | Pipeline 的 `concurrency` 模式（serial/parallel/limited） | ⚠️ 部分实现 | Pipeline 不是为 ReAct 循环设计的——缺少"执行 → 反馈 → 再次调用 LLM"的迭代语义 |
| 5 | **会话管理** | 会话创建/激活/处理/空闲/超时/关闭的状态管理 | Chat Session 状态机（ACTIVE/PROCESSING/IDLE/ERROR/TIMEOUT/CLOSED）+ SQLite sessions.db | ✅ 已实现 | 会话级 Tool 访问控制缺失；会话元数据缺少 Agent 绑定 |
| 6 | **Agent 级 Tool 访问控制** | 不同 Agent 可访问不同 Tool 集合；Tool 调用受 Agent 身份约束 | `AgentRegistry::tool_allowed()` + `ToolExecutor::execute_for_agent()` | ✅ 已实现 | — |
| 7 | **Memory 集成** | 长期记忆的存储、检索与注入到 Context | `MemoryStore` +  keyword-based 检索 + `[remember:]` 自动写入 | ✅ 已集成到 ReAct 循环 | — |
| 8 | **流式输出** | LLM 回复的分块发布，页面逐步渲染 | `AgentHarness` ReAct 循环中 `agent:reply_stream_start/chunk/done` 事件发布 | ✅ 已实现 | — |
| 9 | **中断与恢复** | 用户 /stop 终止当前处理，恢复前一个 IDLE 状态 | `InterruptFlag` + `active_interrupts` 注册表 + `STOP_GENERATION` 事件订阅 | ✅ 已集成到 AgentHarness | — |
| 10 | **多 Agent 协调** | Agent 之间的事件传递、任务委托、结果共享 | config.yaml agents 列表 + `~/.aman/agents/*/` 数据隔离 | ⚠️ 设计与目录结构完成 | 无运行时 Agent 间事件路由；无 Agent 间消息传递协议 |
| 11 | **Token 预算与 Context Window 管理** | 跟踪每次 LLM 调用的 Token 消耗，在超限前做历史裁剪/摘要 | `TokenBudget` + `HistoryCompressor`（truncate/summarize） | ✅ 已实现 | — |
| 12 | **Agent 可观测性** | Agent 级别的指标：LLM 延迟、Token 消耗、Tool 调用频率、错误率 | Event 层面 TraceID + `llm:call_started/ended` + `tool:invoke/completed/failed` | ⚠️ 部分实现 | 缺少 Agent 级聚合指标（一个会话内的所有 LLM 调用和 Tool 调用需关联到同一个 Agent） |

### 2.1 能力实现优先级

| 优先级 | 能力 | 理由 |
|--------|------|------|
| **P0** | ① Agent 身份与生命周期、② ReAct 循环引擎 | 定义 Agent 的"存在"和"行为"，是其他能力的基础 |
| **P1** | ④ Tool Calling 调度、③ Context 组装（含 Token 预算）、⑥ Agent 级 Tool 访问控制 | ReAct 循环的必需品 |
| **P2** | ⑦ Memory 集成、⑧ 流式输出、⑨ 中断与恢复 | 体验打磨 |
| **P3** | ⑩ 多 Agent 协调、⑫ Agent 可观测性 | 进阶能力 |

---

## 3. 当前 Chat 链路的 Harness 化迭代

### 3.1 当前链路（无 Harness）

当前 `MESSAGE_RECEIVED` 事件由 **LLM Plugin**（`crates/plugins/llm-plugin/src/lib.rs`）
处理。这是一个单体处理器，内部实现了完整的 chat 处理逻辑：

```
        MESSAGE_RECEIVED
               │
               ▼
        ┌───────────────┐
        │  LLM Plugin    │
        │               │
        │ 1. 加载 SOUL  │
        │ 2. 加载历史    │
        │ 3. 组装上下文  │
        │ 4. 调用 LLM    │──── LLM 返回 text/tool_calls ────┐
        │ 5. 如果是      │                                   │
        │    tool_call   │── → 执行 Tool → 结果附加到消息列表 │
        │ 6. 再次调用     │──────────────────────────────────┘
        │    LLM         │
        │ 7. 输出回复    │
        └───────────────┘
               │
               ▼
         llm_reply_ready
```

**问题：**
- LLM Plugin 兼任了"上下文组装 + 循环控制 + 工具调度 + 输出发布"多重职责
- Tool Calling 循环的迭代控制逻辑硬编码在 Plugin 内部，不可复用
- 没有 Token 预算追踪（仅有基于字符估算的简单历史裁剪，无 context window 感知）
- 没有 Agent 级 Tool 访问控制（Plugin 能调用任何注册的 Tool）
- 中断只能终止整个 Plugin 处理（通过 `STOP_GENERATION` → CancellationToken），不能终止当前 ReAct 循环后保留会话
- Per-session 消息队列（`mpsc queue`）和 history trimming 在 Plugin 内部实现，无法被其他 Agent 行为复用

### 3.2 Harness 化后的链路

```
        MESSAGE_RECEIVED
               │
               ▼
        ┌──────────────────────────────────────────────┐
        │              AgentHarness                     │
        │                                              │
        │  Phase 1: 初始化                              │
        │    ├── 解析 Agent 身份 → SOUL + Tool 权限     │
        │    ├── 创建/恢复 Session                      │
        │    ├── 发布 agent:processing_started 事件     │
        │    └── 初始化 TokenBudget 追踪器              │
        │                                              │
        │  Phase 2: ReAct 循环（可迭代多次）             │
        │    ┌────────────────────────────────────┐     │
        │    │  Step 1: Context Assembly          │     │
        │    │    ├── SoulSnapshot (当前版本固定)  │     │
        │    │    ├── SessionHistory               │     │
        │    │    ├── AgentTools（该 Agent 可用 Tool）│   │
        │    │    ├── MemoryContext（相关记忆）     │     │
        │    │    └── TokenBudget.trim()          │     │
        │    │         → 如果超限，压缩最旧历史     │     │
        │    │                                    │     │
        │    │  Step 2: LLM Invocation            │     │
        │    │    ├── 发布 llm:call_started       │     │
        │    │    ├── 调用 LLM Provider Tool      │     │
        │    │    └── 发布 llm:call_ended         │     │
        │    │                                    │     │
        │    │  Step 3: Response Classification   │     │
        │    │    ┌────────────────────────┐      │     │
        │    │    │ response 类型           │      │     │
        │    │    ├── text_only → Phase 3  │      │     │
        │    │    ├── has_tool_calls →     │      │     │
        │    │    │   ┌────────────────┐   │      │     │
        │    │    │   │ ToolScheduler  │   │      │     │
        │    │    │   │ 解析 tool_calls│   │      │     │
        │    │    │   │ 权限校验       │   │      │     │
        │    │    │   │ 调度执行       │   │      │     │
        │    │    │   │ 发布 tool:*    │   │      │     │
        │    │    │   │ 结果附加到消息 │   │      │     │
        │    │    │   └──────┬─────────┘   │      │     │
        │    │    │         → 回到 Step 1  │      │     │
        │    │    ├── error → ERROR 处理   │      │     │
        │    │    └── stop  → 中断循环     │      │     │
        │    │                              │      │     │
        │    └──────────────────────────────┘      │     │
        │                                          │     │
        │  Phase 3: 输出                           │     │
        │    ├── 将最终回复写入 SessionHistory     │     │
        │    ├── 写入长期记忆（如果需要）          │     │
        │    ├── 发布 llm_reply_ready / stream_*  │     │
        │    ├── 发布 agent:processing_finished    │     │
        │    └── 更新 TokenBudget 记录             │     │
        │                                          │     │
        └──────────────────────────────────────────────┘
```

**循环安全限制：** `max_react_turns`（默认 10）防止 LLM 陷入无限 Tool Calling 循环。
当 ReAct 迭代次数达到上限后：
1. AgentHarness 不再调用 LLM，发布 `agent:react_turns_exhausted` 事件
2. 将当前的工具调用结果和一条强制性截止提示（"已达到最大循环轮次，请基于现有信息回复"）作为最后一次 LLM 输入
3. 如果 LLM 在此次调用后仍返回 tool_calls → 强制终止，将已收集的结果作为最终回复
4. Workflow 转移到 IDLE，保留已处理的部分结果

### 3.3 ReAct 循环的事件流

每次 LLM 调用和 Tool 执行都通过 Event Bus 发布事件，保持 Aman 的"万物皆事件"原则：

```
Agent ReAct 循环事件序列（一次用户消息 → 最终回复）：

Phase 1:
  agent:processing_started → { agent_id, session_id, message_id }

Phase 2 (迭代 1..N 次):
  ┌─ llm:call_started     → { agent_id, session_id, turn, model, input_tokens }
  │  llm:call_ended       → { agent_id, session_id, turn, output_tokens, has_tool_calls }
  │
  │  [如果有 Tool Calls]:
  │    tool:dispatched  → { agent_id, session_id, turn, tool_name, tool_args }
  │    tool:completed   → { agent_id, session_id, turn, tool_name, result_summary }
  │  [或 Tool 失败]:
  │    tool:failed      → { agent_id, session_id, turn, tool_name, error }
  │
  │  agent:tool_results_fed_back  → { agent_id, session_id, turn, n_results }
  └─ [循环]

Phase 3:
  agent:reply_stream_start → { agent_id, session_id }
  agent:reply_chunk       → { agent_id, session_id, content }
  agent:reply_stream_done → { agent_id, session_id, finish_reason }
  agent:processing_finished → { agent_id, session_id, total_llm_calls, total_tool_calls, total_tokens, latency_ms }
```

> **事件复用说明**：`llm:call_started` 和 `llm:call_ended` 复用了现有 LLM Plugin 已定义的事件类型（`EventType::Custom("llm:call_started")`）。AgentHarness 统一管理这些事件的发布，不再由 LLM Plugin 单独发布。同样，`tool:invoke`/`tool:completed`/`tool:failed` 复用现有 PipelineEngine 的 `ToolEventSink` 通道，Harness 通过同一 EventBus 接口发布，确保 TraceID 链路完整。

### 3.4 中断（Interrupt）的事件流

```
用户发送 /stop → MESSAGE_RECEIVED { content: "/stop" }

AgentHarness:
  1. 检测到 session_id 匹配当前正在处理的会话
  2. 设置 InterruptFlag（共享原子变量 / CancellationToken）
  3. ReAct 循环在下一次迭代开始前检查 Flag
  4. 循环终止，进入 Phase 3 输出:
     └─ 发布 agent:reply_interrupted → { agent_id, session_id, processed_turns }
  5. Session 状态回到 IDLE，等待下一条用户消息
```

对比当前链路：当前 /stop 是在 Skill 级别通过 CancellationToken 中断整个处理，
Harness 版本在 Agent 级别管理中断，可以输出"已处理了 N 轮"的中间结果，
而非整个丢弃。

### 3.5 ReAct 循环与 chat-session Workflow 状态机集成

AgentHarness 的 ReAct 循环不引入新的状态机——它运行在现有 chat-session Workflow
（`crates/gateway/src/runtime/agent_runtime.rs:202-352`）的 `PROCESSING` 状态内部。
两者通过事件驱动协作：

```
Workflow 状态机                      AgentHarness (在 PROCESSING 内)
────────────                        ──────────────────────────────
ACTIVE
  │  MESSAGE_RECEIVED
  ▼
PROCESSING ─────────────────────→  process_message() 开始
                                       ├── Phase 1: 初始化
                                       │       └── agent:processing_started
                                       ├── Phase 2: ReAct 循环 (迭代 1..N)
                                       │       ├── llm:call_started
                                       │       ├── llm:call_ended
                                       │       ├── tool:dispatched/completed/failed (可选)
                                       │       └── agent:tool_results_fed_back (可选)
                                       │
                                       └── Phase 3: 输出
                                               ├── llm_reply_ready  ← 关键：触发状态转移
                                               └── agent:processing_finished

  │  llm_reply_ready
  ▼
IDLE
```

**关键集成点：**

| 集成点 | 说明 |
|--------|------|
| **Workflow 守卫** | 现有状态转移 `PROCESSING + llm_reply_ready → IDLE`。AgentHarness 在 Phase 3 发布 `llm_reply_ready` 事件，触发 Workflow 自动转移到 IDLE。**无需修改 Workflow 转移表**。 |
| **错误路径** | `PROCESSING + llm_error → ERROR`。AgentHarness 在 LLM 调用失败时发布 `llm_error`，复用现有错误恢复路径。 |
| **流式路径** | `PROCESSING + agent:reply_stream_done → IDLE`。**新增转移规则**：流式输出场景下，`llm_reply_ready` 不发（因为流式响应不产生完整的单次文本），由 `agent:reply_stream_done` 触发状态转移。 |
| **中断路径** | `PROCESSING + agent:reply_interrupted → IDLE`。**新增转移规则**：用户 /stop 中断 ReAct 循环后，AgentHarness 发布 `agent:reply_interrupted`，Workflow 回到 IDLE 等待下一条消息。 |
| **超时路径** | `STREAM_TIMEOUT (120s)` 和 `SESSION_TIMEOUT (300s)` 由 Workflow 超时管理器独立触发，不受 AgentHarness 管理——AgentHarness 的 ReAct 循环被 CancellationToken 中断时优雅退出。 |

> **会话隔离**：同一 session_id 的消息在 Harness 内部串行处理（通过 per-session 互斥锁），跨 session 并发。这与现有 `LlmPlugin` 的 `per-session mpsc queue` 策略一致，Harness 在内部实现等价机制。

---

## 4. 里程碑与任务拆分

### ✅ M1：Agent 运行时类型 ⭐ P0 — 已完成

> 目标：定义 Agent 的运行时类型系统，使 Agent 成为框架的一等公民。
> 验收：AgentRuntime 可以注册/查询/创建 Agent 实例。
>
> **实现**: `kernel::agent` 类型系统 + `AgentRegistry` + Phase 2 config 加载 + HTTP/Tauri 端点

#### T1.1 — 定义 Agent 核心类型（core crate）

| 属性 | 内容 |
|------|------|
| 涉及 | `crates/core/src/`（新建 `agent.rs`） |
| 描述 | 定义 Agent 核心数据结构，作为框架内 Agent 的运行时表示 |

```rust
/// Agent 运行时标识与配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentDescriptor {
    pub agent_id: String,          // config.yaml 中的 agent key
    pub display_name: String,
    pub provider: String,          // provider key
    pub model: String,
    pub soul_path: Option<PathBuf>,
    pub enabled: bool,
}

/// Agent 运行时状态
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AgentStatus {
    Disabled,    // 配置中禁用
    Idle,        // 已加载，无活跃会话
    Busy,        // 有活跃会话正在处理
    Error,       // 初始化失败或运行时异常
}

/// Agent 运行时实例（由 AgentRegistry 管理）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentInstance {
    pub descriptor: AgentDescriptor,
    pub status: AgentStatus,
    pub active_session_id: Option<String>,
    pub registered_at: Timestamp,
}
```

**子任务：**
1. 新增 `AgentDescriptor`、`AgentStatus`、`AgentInstance` 结构体
2. 新增 `AgentEvent` 枚举（后续事件类型的容器）
3. `cargo test -p core` 通过

#### T1.2 — 实现 AgentRegistry（runtime crate）

| 属性 | 内容 |
|------|------|
| 涉及 | `crates/runtime/src/`（新建 `agent_registry.rs`） |
| 描述 | Agent 实例的运行时注册表，管理 Agent 的 CRUD 和状态 |

```rust
pub struct AgentRegistry {
    agents: RwLock<HashMap<String, AgentInstance>>,
}

impl AgentRegistry {
    pub fn register(descriptor: AgentDescriptor) -> Result;
    pub fn unregister(agent_id: &str) -> Result;
    pub fn get(agent_id: &str) -> Option<AgentInstance>;
    pub fn list() -> Vec<AgentInstance>;
    pub fn set_status(agent_id: &str, status: AgentStatus) -> Result;
    pub fn get_available_tools(agent_id: &str) -> Vec<ToolDescriptor>;
}
```

**子任务：**
1. 实现 `AgentRegistry` 结构体与同步原语
2. 实现 CRUD 方法
3. AgentRegistry 集成到 AgentRuntime 的 build() 中
4. Phase 2（组件注册）完成后自动从 config.yaml 加载所有 Agent
5. Agent 注册/状态变更时发布 `agent:registered` / `agent:status_changed` 事件

#### T1.3 — 为 AgentRegistry 添加事件发布

| 属性 | 内容 |
|------|------|
| 涉及 | `crates/runtime/src/agent_registry.rs`、`crates/core/src/event.rs` |
| 描述 | Agent 生命周期事件：注册、状态变更、卸载 |

| 新增事件 | 发布时机 |
|---------|---------|
| `agent:registered` | Agent 注册到 Registry 时 |
| `agent:status_changed` | Agent 状态变化时（Idle↔Busy↔Error） |
| `agent:removed` | Agent 从 Registry 移除时 |

> **命名约定**：遵循 Aman 现有事件系统的 `namespace:event` 命名风格（参考 `llm:call_started`、`tool:invoke`、`session:started`）。Harness 引入的所有新事件均使用 `agent:` 前缀。

#### T1.4 — Tauri IPC 添加 Agent 管理端点

| 属性 | 内容 |
|------|------|
| 涉及 | `crates/tauri/src/commands.rs` |
| 描述 | 新增 `list_agents`、`get_agent`、`set_agent_status` IPC 命令 |

---

### ✅ M2：ReAct 循环引擎 ⭐ P0 — 已完成

> 目标：实现可复用的 Think-Act-Observe 循环引擎，统一管理 LLM 调用、Tool 执行、结果反馈的迭代过程。
> 验收：AgentHarness 可以接收 MESSAGE_RECEIVED 事件，完整执行 ReAct 循环并输出最终回复。
>
> **已实现**: `ReActEngine` trait + `AgentHarness::process_message()` + `LlmReActEngine` + `ToolExecutor` + 完整事件发布 + `MessageReceivedHandler`（MESSAGE_RECEIVED 订阅）+ 流式输出（SSE `agent:reply_chunk` 事件）

#### T2.1 — 定义 ReAct 循环的核心 trait（core crate）

| 属性 | 内容 |
|------|------|
| 涉及 | `crates/core/src/`（新建 `react.rs`） |
| 描述 | 定义 ReAct 循环引擎的核心 trait 和类型 |

```rust
/// ReAct 循环的一次迭代结果
#[derive(Debug, Clone)]
pub enum ReActTurn {
    /// LLM 返回了纯文本回复 → 循环结束
    Finished { content: String, finish_reason: String },
    /// LLM 返回了 Tool Calls → 需要继续循环
    ToolCalls(Vec<ParsedToolCall>),
    /// LLM 调用失败 → 循环异常终止
    Error(ReActError),
}

/// 解析后的 Tool Call
#[derive(Debug, Clone)]
pub struct ParsedToolCall {
    pub id: String,
    pub tool_name: String,
    pub args: serde_json::Value,
}

/// ReAct 循环的上下文
#[derive(Debug, Clone)]
pub struct ReActContext {
    pub agent_id: String,
    pub session_id: String,
    pub turn: u32,                          // 当前迭代轮次（0 起始，用于 max_react_turns 检查）
    pub max_turns: u32,                     // 配置的 max_react_turns 上限
    pub soul_snapshot: SoulSnapshot,
    pub history: Vec<ChatMessage>,
    pub agent_tools: Vec<ToolDescriptor>,
    pub memory_context: Option<String>,
    pub token_budget: TokenBudget,
}

/// ReAct 循环引擎 trait
#[async_trait]
pub trait ReActEngine: Send + Sync {
    /// 执行一次 ReAct 迭代
    async fn execute_turn(&self, ctx: &ReActContext, messages: Vec<ChatMessage>)
        -> Result<ReActTurn>;

    /// 处理 Tool Calls 并返回结果消息
    async fn execute_tools(&self, ctx: &ReActContext, calls: &[ParsedToolCall])
        -> Result<Vec<ChatMessage>>;
}
```

**子任务：**
1. 定义 `ReActTurn`、`ParsedToolCall`、`ReActContext`、`ReActEngine` trait
2. 定义 `ReActError` 枚举（LLMError / ToolError / BudgetExceeded / Interrupted）
3. 定义 `ToolPermission` 结构体（`{tool_name, allowed_agents: Vec<String>}`）
4. `cargo test -p core` 通过

#### T2.2 — 实现 AgentHarness（runtime crate）

| 属性 | 内容 |
|------|------|
| 涉及 | `crates/runtime/src/`（新建 `agent_harness.rs`） |
| 描述 | AgentHarness 是 ReAct 循环的编排器，协调 Context 组装、LLM 调用、Tool 执行、事件发布 |

```rust
pub struct AgentHarness {
    registry: Arc<AgentRegistry>,
    tool_executor: Arc<ToolExecutor>,     // 封装 ToolRunner + 权限校验
    event_publisher: Arc<dyn EventPublisher>,
    session_store: Arc<dyn SessionStore>,
    memory_store: Arc<dyn MemoryStore>,
    soul_loader: Arc<dyn SoulLoader>,
}

impl AgentHarness {
    /// 处理一个 MESSAGE_RECEIVED 事件
    /// 执行完整的 ReAct 循环，直到 LLM 返回最终回复或循环终止
    pub async fn process_message(&self, event: MESSAGE_RECEIVED) -> Result<AgentOutput> {
        // -> 详见下文链路
    }
}
```

**AgentHarness::process_message 完整实现：**

```
1. 从 AgentRegistry 获取 AgentInstance（事件可附带 agent_id，或从 session_id 反查）
2. 更新 Agent 状态为 Busy
3. 加载 SOUL → SoulSnapshot（固定此版本）
4. 从 SessionStore 加载历史
5. 从 MemoryStore 检索相关记忆
6. 从 AgentRegistry 获取该 Agent 可用的 Tool 列表
7. 初始化 TokenBudget
8. 进入 ReAct 循环:
   a. TokenBudget.trim() → 压缩历史（如需要）
   b. Context Assembler 组装最终消息
   c. 发布 llm:call_started 事件
   d. 调用 LLM Provider Tool → 获取响应
   e. 发布 llm:call_ended 事件
   f. 分类响应:
      - text_only → 结束循环
      - has_tool_calls → g
      - error → 走错误处理
      - interrupted → 终止循环
   g. 遍历 Tool Calls:
      - 权限校验（该 Agent 是否可以使用该 Tool）
      - 串行执行（或 parallel，取决于配置）
      - 发布 tool:dispatched / tool:completed / tool:failed
      - 结果格式化为 ChatMessage
   h. 发布 agent:tool_results_fed_back
   i. 回到 a
9. 将最终回复写入 SessionHistory
10. 写入长期记忆（如需要）
11. 发布 agent:reply_* 系列事件
12. 更新 Agent 状态为 Idle
13. 更新 TokenBudget 记录
```

**子任务：**
1. 实现 `AgentHarness` 结构体
2. 实现 `process_message()` 方法（核心 ReAct 循环）
3. 实现 `ContextAssembler`：组合 SOUL + History + Memory + Tool Schema
4. 实现 `ToolExecutor`：封装 ToolRunner + 权限校验
5. 实现 `TokenBudget` 追踪器（初始版：记录累计 token 数 + 配置上限，裁剪最旧历史）
6. 实现循环中断检测（InterruptFlag）
7. 在 ReAct 循环各节点发布事件（llm:call_started/llm:call_ended、tool:dispatched/tool:completed/tool:failed 等）

#### T2.3 — 注册 AgentHarness 到 Dispatcher

| 属性 | 内容 |
|------|------|
| 涉及 | `crates/runtime/src/agent_runtime.rs`、`crates/dispatcher/src/` |
| 描述 | 使 AgentHarness 成为 MESSAGE_RECEIVED 事件的处理器 |

**子任务：**
1. 在 AgentRuntime::build() 中创建 AgentHarness 实例
2. 在 Dispatcher 中注册路由：`MESSAGE_RECEIVED` → `AgentHarness.process_message()`
3. 保留现有的 Agent 级会话等待队列（同一会话串行，跨会话并行）
4. 确保 AgentHarness 与现有会话生命周期事件正确协作：
   - `session:started`/`session:closed` 仍由 HTTP handlers（`http.rs`）发布，AgentHarness 不需要接管
   - AgentHarness 在 Phase 1 中通过 session_id 恢复或创建会话上下文
   - 现有的 `session:timeout` 事件（由 Workflow 超时管理器触发）正确中断 Harness 的 ReAct 循环

#### ✅ T2.4 — 集成流式输出

| 属性 | 内容 |
|------|------|
| 涉及 | `crates/core/src/react.rs` + `crates/gateway/src/runtime/agent_harness.rs` + `crates/gateway/src/runtime/agent_runtime.rs` |
| 描述 | LLM Provider 返回流式响应时，Harness 实时发布 Stream 事件 |

**实现：**
- `StreamEvent` 枚举（Start / Chunk / Done / Error）+ `stream_cb` 回调字段添加到 `ReActContext`
- `model` 字段添加到 `ReActContext`
- `LlmReActEngine::streaming_llm_call()` — 直接对 LLM API 发起 streaming SSE 请求，逐 delta 通过回调发送
- `react_loop()` — 创建 tokio mpsc channel，spawn consumer 任务将 `StreamEvent` 发布为 `agent:reply_stream_start/chunk/done` 事件
- `LlmReActEngine` 持有 `api_key` / `base_url`，从 `build_llm_config()` 透传
- 无 stream 回退：`stream_cb` 为 None 时走原有工具调用路径

---

### ✅ M3：Agent 级 Tool 访问控制 ⭐ P1 — 已完成

> 目标：Tool 的可用性绑定到 Agent 身份，不同 Agent 可以使用不同的 Tool 集合。
> 验收：Agent A 可以调用 tool-X，Agent B 调用 tool-X 时被拒绝。
>
> **实现**: `config::ToolsConfig`（allow/deny）+ `AgentRegistry::tool_allowed()` + `ToolExecutor::execute_for_agent()`

#### T3.1 — 定义 Tool 权限模型

| 属性 | 内容 |
|------|------|
| 涉及 | `crates/core/src/`（扩展 `tool.rs`）+ `config.yaml` 格式扩展 |

**config.yaml 扩展：**
```yaml
agents:
  coder:
    display_name: Coder
    provider: openai
    model: gpt-5.4-flash
    tools:                          # 新增：该 Agent 可用的 Tool 列表
      allow: ["*"]                  # 通配符 = 全部可用
  analyst:
    display_name: Data Analyst
    provider: deepseek
    model: deepseek-v4-pro
    tools:
      allow: ["db-query", "chart-gen", "file-read"]
      deny: ["exec", "network"]     # 显式拒绝
    tool_timeout: 60s               # 该 Agent 下工具的默认超时
```

**子任务：**
1. 扩展 `AgentDescriptor`，增加 `allowed_tools: Option<Vec<String>>` 和 `denied_tools: Option<Vec<String>>`
2. 扩展 `config.yaml` 解析，支持 per-agent tools 配置
3. 权限模型支持：`allow: ["*"]`（全部可用）、`allow: [...]`（白名单）、`allow+deny`（白名单+黑名单）

#### T3.2 — 在 ToolExecutor 中实现权限校验

| 属性 | 内容 |
|------|------|
| 涉及 | `crates/runtime/src/agent_harness.rs`（ToolExecutor） |
| 描述 | AgentHarness 在执行 Tool 前检查该 Agent 是否有权限 |

```rust
impl ToolExecutor {
    pub async fn execute_for_agent(
        &self,
        agent_id: &str,
        tool_name: &str,
        args: Value,
    ) -> Result<ToolResult> {
        // 1. 查询该 Agent 的 allowed_tools
        // 2. 如果 tool_name 不在 allowlist 中 → 返回 ToolPermissionDenied
        // 3. 如果 tool_name 在 denylist 中 → 返回 ToolPermissionDenied
        // 4. 执行 Tool
        // 5. 记录审计日志
    }
}
```

**子任务：**
1. 实现 `execute_for_agent()` 方法
2. 权限拒绝时返回结构化错误（`tool:failed { reason: "permission_denied" }`）
3. ReAct 循环中 Tool 权限错误 → 将错误消息作为 LLM 的下一次输入
   （让 LLM 知道该 Tool 不可用，可以尝试其他方法）
4. 添加审计日志记录

---

### ✅ M4：Token 预算与 Context Window 管理 ⭐ P1 — 已完成

> 目标：追踪 Token 消耗，在超限前自动压缩历史，防止 Context Window 溢出。
> 验收：长时间对话中，当累计 Token 接近上限时，最旧的历史被自动摘要/裁剪。
>
> **实现**: `TokenBudget`（model-aware context window）+ `HistoryCompressor`（truncate/summarize）+ 集成到 ReAct 循环

#### T4.1 — 实现 TokenBudget 追踪器

| 属性 | 内容 |
|------|------|
| 涉及 | `crates/runtime/src/`（新建 `token_budget.rs`） |
| 描述 | Token 用量追踪与预算管理 |

```rust
pub struct TokenBudget {
    model: String,                    // 模型名（用于获取 context_window 大小）
    context_window: usize,            // 模型最大 context window
    max_output_tokens: usize,         // 保留给输出的 token 数
    max_prompt_tokens: usize,         // max(context_window - max_output_tokens, 0)
    
    current_history_tokens: usize,    // 当前历史累计 token
    current_tool_schema_tokens: usize,// Tool Schema 的 token 数
    current_system_tokens: usize,     // System Prompt 的 token 数
}

impl TokenBudget {
    pub fn new(model: &str, context_window: usize) -> Self;
    
    /// 估算一组消息的 token 数（简化：text.len() / 4 + 1）
    pub fn estimate_tokens(text: &str) -> usize;
    
    /// 检查是否需要裁剪
    pub fn needs_trim(&self) -> bool;
    
    /// 返回需要裁剪的 token 数
    pub fn trim_amount(&self) -> usize;
    
    /// 记录新增 token
    pub fn record_usage(&mut self, prompt_tokens: usize, completion_tokens: usize);
}
```

**子任务：**
1. 实现 `TokenBudget` 结构体与核心方法
2. 提供 `estimate_tokens()` 估算函数（tokenizer-free 版本）
3. 集成到 AgentHarness 的 ReAct 循环中：
   - 每次 LLM 调用前检查 `needs_trim()`，如果超限则触发历史压缩
   - 记录每次 LLM 调用的 token 消耗

#### T4.2 — 实现历史压缩策略

| 属性 | 内容 |
|------|------|
| 涉及 | `crates/runtime/src/`（新建 `history_compressor.rs`） |
| 描述 | 当 Token 预算超限时，自动压缩/摘要最旧的对话历史 |

**子任务：**
1. 实现 `HistoryCompressor` 结构体
2. 支持两种压缩策略：
   - `truncate`：裁剪最旧的消息，直到 token 数低于阈值
   - `summarize`：调用 LLM 摘要最旧的消息块，用摘要替换原文
3. 集成到 `TokenBudget.trim()` 中
4. 压缩发生时发布 `agent:history_compressed` 事件（通知前端）

---

### ✅ M5：Memory 集成到 ReAct 循环 ⭐ P2 — 已完成

> 目标：在每次 LLM 调用前自动检索相关记忆，注入 Context。
> 验收：Agent 可以在对话中回忆之前会话中存储的信息。
>
> **实现**: `MemoryStore`（keyword-based 检索）+ `process_message()` 中检索并注入 `ctx.memory_context` + `[remember:]` 自动写入

#### T5.1 — MemoryStore 集成到 ContextAssembly

| 属性 | 内容 |
|------|------|
| 涉及 | `crates/runtime/src/agent_harness.rs` + `crates/runtime/src/memory_store.rs` |
| 描述 | 在 Context Assembly 阶段从记忆库检索相关片段 |

**子任务：**
1. 在 AgentHarness 的 Context Assembly 阶段添加记忆检索步骤
2. 检索策略：使用当前用户消息的文本相似度匹配记忆条目
3. 检索到的记忆作为 system prompt 的附加段注入
4. 可配置 `memory_max_results: 5`（最多返回多少条记忆）

#### T5.2 — 自动记忆写入

| 属性 | 内容 |
|------|------|
| 涉及 | `crates/runtime/src/agent_harness.rs` |
| 描述 | ReAct 循环结束后，自动将关键信息写入长期记忆 |

**子任务：**
1. 定义触发记忆写入的条件（如：用户明确说"记住…"、会话中有新事实出现）
2. 简单实现：Agent 回复中标记 `[remember: ...]` 格式的话提取为记忆

---

### ✅ M6：中断与恢复增强 ⭐ P2 — 已完成

> 目标：用户 /stop 可以中断当前 ReAct 循环并保留中间结果。
> 验收：用户在 Tool Calling 循环中发送 /stop，Agent 输出已完成的部分。
>
> **实现**: `ReactOutcome` 枚举 + `active_interrupts` 注册表 + `STOP_GENERATION` 事件订阅 → `interrupt_session()` → `agent:reply_interrupted`

#### T6.1 — 注册 InterruptFlag 到 AgentHarness

| 属性 | 内容 |
|------|------|
| 涉及 | `crates/runtime/src/agent_harness.rs` |
| 描述 | ReAct 循环检查全局中断标志 |

**子任务：**
1. AgentHarness 在 ReAct 循环中（每次迭代开始前）检查 `InterruptFlag`
2. `InterruptFlag` 由 Agent 级别的 `CancellationToken` 实现
3. 中断时发布 `agent:reply_interrupted` 事件
4. Session 状态回到 IDLE，等待下一条消息

#### T6.2 — 中断恢复

| 属性 | 内容 |
|------|------|
| 涉及 | `crates/runtime/src/agent_harness.rs` |
| 描述 | 中断后用户新消息可以继续同一会话，历史保持不变 |

---

### ✅ M7：多 Agent 运行时协调 ⭐ P3 — 已完成

> 目标：Agent 之间可以互相传递事件和任务。
> 验收：Agent A 可以发布事件触发 Agent B 的处理。

#### ✅ T7.1 — Agent 间事件路由

| 属性 | 内容 |
|------|------|
| 涉及 | `crates/core/src/event.rs`、`crates/gateway/src/runtime/agent_runtime.rs`、`crates/gateway/src/runtime/agent_harness.rs` |
| 描述 | 通过 EventBus subscription 实现 Agent 间事件路由 |

**子任务：**
1. ✅ 新增 `EventType::AgentMessage` variant（→ `"agent:message"`）
2. ✅ `AgentMessageHandler` 订阅 `agent:message` 事件，按 `to_agent` 路由到目标 Agent
3. ✅ `AgentHarness::publish_agent_message()` 方法发布事件给其他 Agent

#### ✅ T7.2 — Agent 间消息协议

| 属性 | 内容 |
|------|------|
| 涉及 | `crates/core/src/agent.rs` |
| 描述 | 定义 Agent 间消息的标准格式 |

```
agent:message {
    message_id: UUID v7,
    from_agent: String,
    to_agent: String,
    content_type: "task_delegation" | "result_sharing" | "status_query",
    payload: Value,
    reply_to: Option<UUID>,     // 回复链追踪
}
```

---

## 5. 实现要求

### 5.0 LLM Plugin → AgentHarness 迁移策略

当前的 `LlmPlugin`（`crates/plugins/llm-plugin/`）在 AgentHarness 完成后将被逐步替换。
迁移分为三个阶段：

**阶段 A（共存期，M2 完成后）：**
- AgentHarness 和 LLM Plugin 同时加载
- Dispatcher 通过 `agent_id` 路由决定谁处理 `MESSAGE_RECEIVED`：
  - 配置了 `harness` 段的 Agent → AgentHarness 处理
  - 未配置 `harness` 段的 Agent → 回退到 LLM Plugin（向后兼容）
- 两者共享事件总线：`llm:call_started`/`llm:call_ended` 完全兼容

**阶段 B（前端迁移完成，Plugin 保留）：**
- 前端 Chat.svelte 新增 AgentHarness 事件监听（`agent:reply_stream_start/chunk/done`、`tool:dispatched/completed/failed`、`agent:reply_ready`）
- 流式输出支持：`assistant_streaming` 消息类型现已可用
- 去重机制：AgentHarness 处理的 session 自动跳过 LLM Plugin 的 `llm_reply_ready`
- LLM Plugin 后端仍在运行，Phase C 待测试后执行

**阶段 C（移除 Plugin，M6 完成后）：**
- LLM Plugin 从 workspace 中移除
- `LlmConfig.sessions_dir` / `skills_dir` / `tool_registry` 逻辑迁移到 AgentHarness + config.yaml
- 事件名完全统一：不再有 `llm_reply_ready` 以外的 LLM Plugin 特有事件

### 5.1 符合 Aman 设计理念

- **万物皆事件**：AgentHarness 的所有关键操作（LLM 调用开始/结束、Tool 执行、循环迭代）都通过 Event Bus 发布事件，不引入新的执行模型
- **响应即行为**：AgentHarness 本身是事件处理器——它响应 `MESSAGE_RECEIVED`、`RETRY_CMD`、`STOP_GENERATION` 等事件，不主动轮询
- **通过 Event Bus 通信**：AgentHarness 不直接访问 LLM Provider 或 ToolRunner，而是通过已有的 ToolRunner（由 Pipeline 或直接调用调度）

### 5.2 增量实现

- 每一层都向后兼容：M1 只新增类型，不改现有逻辑
- M2 完成后，现有的 LLM Plugin 逻辑可以逐步迁移到 AgentHarness
- 迁移期间，LLM Plugin 和 AgentHarness 可以并存（通过 Dispatcher 路由配置决定谁处理 MESSAGE_RECEIVED）

### 5.3 已有设施复用

| 已有设施 | 在 Harness 中的用途 |
|---------|-------------------|
| Event Bus | Harness 的全部关键操作通过事件发布（agent:*, llm:*, tool:*） |
| ToolRunner | ReAct 循环中的 Tool 执行 |
| Workflow | Session 状态管理（复用现有 chat-session 状态机） |
| SOUL | Agent 身份与行为边界的来源 |
| Secret Store | LLM Provider API Key 解析 |
| Pipeline | Tool Calling 调度（serial/parallel/limited 复用 Pipeline 并发模型） |
| Dispatcher | MESSAGE_RECEIVED → AgentHarness 路由 |
| Idle System | Agent 空闲时的 Reflection 和深度空闲。Harness 通过 AgentInstance.status（Busy/Idle）通知 IdleDetector：Agent 在处理消息期间为 Busy → IdleDetector 跳过该 Agent 的空闲周期。会话结束时 Agent 恢复 Idle，空闲深度从 Daze 重新开始（Harness 在处理完消息后重置该 Agent 的 idle_depth） |
| State Store | Session 历史和 Memory 的持久化 |
| Plugin System | Agent 的动态能力扩展 |

### 5.4 设计约束

| 约束 | 内容 |
|------|------|
| **无 unsafe** | 遵循 crate 级 `#![forbid(unsafe_code)]` |
| **无新执行模型** | Harness 不引入独立的事件循环或线程，运行在已有的 Runtime tokio 执行器上 |
| **可观测** | 所有 Harness 事件携带 TraceID，通过已有 Trace API 可追踪完整 ReAct 链路 |
| **可中断** | ReAct 循环的每次迭代可被中断，不影响其他 Agent 的会话 |
| **可配置** | Agent 的行为（max_turns、tool_timeout、memory_enabled 等）通过 config.yaml 配置 |

### 5.5 配置示例（扩展后）

```yaml
agents:
  cortana:
    display_name: Cortana
    provider: openai
    model: gpt-5.4-flash
    system_prompt_override: null
    harness:
      max_react_turns: 10            # 单次消息最多 10 轮 ReAct 循环
      tool_timeout: 60s              # 该 Agent 下 Tool 的默认超时
      tool_concurrency: serial       # Tool 执行策略（serial | parallel | limited:N）
    tools:
      allow: ["file-read", "http", "db-query", "exec"]
      deny: ["exec/rm"]
    memory:
      enabled: true
      max_results: 5
    token_budget:
      context_window_ratio: 0.8      # 使用 80% 的 context window（保留 20% 给输出）
      trim_strategy: truncate        # truncate | summarize
      trim_ratio: 0.5               # 超限时裁剪到 50%

  analyst:
    display_name: Data Analyst
    provider: deepseek
    model: deepseek-v4-pro
    harness:
      max_react_turns: 5
      tool_concurrency: parallel
    tools:
      allow: ["db-query", "chart-gen", "file-read"]
    memory:
      enabled: false
```

---

## 6. 依赖关系总览

```
M1 (Agent Runtime 类型) ⭐ P0 ✅
│
├── M2 (ReAct 循环引擎) ⭐ P0 ✅
│   ├── M2.1 (Core trait) ✅
│   ├── M2.2 (AgentHarness 实现) ✅
│   ├── M2.3 (Dispatcher 注册) ✅
│   └── M2.4 (流式输出集成) ✅
│
├── M3 (Tool 访问控制) ⭐ P1 ✅
│   ├── M3.1 (权限模型) ✅
│   └── M3.2 (权限校验) ✅
│
├── M4 (Token 预算) ⭐ P1 ✅
│   ├── M4.1 (TokenBudget) ✅
│   └── M4.2 (历史压缩) ✅
│
├── M5 (Memory 集成) ⭐ P2 ✅
│   ├── M5.1 (检索集成) ✅
│   └── M5.2 (自动写入) ✅
│
├── M6 (中断/恢复) ⭐ P2 ✅
│   ├── M6.1 (InterruptFlag 注册) ✅
│   └── M6.2 (中断恢复) ✅
└── M7 (多 Agent 协调) ⭐ P3 ✅
    ├── M7.1 (事件路由) ✅
    └── M7.2 (消息协议) ✅
```

| 依赖 | 说明 |
|------|------|
| M1 → (无) | 基础，独立实现 |
| M2 → M1 | AgentRegistry 为 Harness 提供 Agent 身份查询 |
| M3 → M2 | ToolExecutor 是 ReAct 循环的组成部分 |
| M4 → M2 | TokenBudget 在 Context Assembly 中使用 |
| M5 → M2 | Memory 在 Context Assembly 阶段注入 |
| M6 → M2 | InterruptFlag 在 ReAct 循环中检查 |
| M7 → M1 + EventBus Subscription | 依赖 Agent 身份和事件路由能力 |
