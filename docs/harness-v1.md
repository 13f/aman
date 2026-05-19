# Agent Harness — 架构设计 (v1 — 已归档)

> ⚠️ 此为第一版设计，已被 `harness.md` 取代。仅保留用于设计演进追踪。
> 请参考 `/Users/jerin/projects/aman/docs/harness.md` 获取当前设计。

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
| 1 | **Agent 身份与生命周期** | Agent 的注册、创建、销毁、配置管理 | `config.yaml` agents 段 + SOUL 系统 + `~/.aman/agents/` 目录 | ✅ 设计与部分实现 | 无运行时 `Agent` 类型；Agent 配置仅在启动时加载，无运行时动态创建/销毁 API |
| 2 | **ReAct 循环引擎** | Think-Act-Observe 迭代：LLM 响应 → 解析 Tool Calls → 执行 → 反馈 → 继续 | LLM Skill 的 §7 Tool Calling 流程描述 | ⚠️ 仅设计描述 | 无正式的循环管理器；Tool Call 自动解析-执行-反馈链路未抽象为可复用的循环引擎 |
| 3 | **Context 组装** | System Prompt + 历史会话 + Tools Schema + 用户消息组合与 Token 预算管理 | SOUL `to_system_prompt()` + LLM Skill 上下文组装 + Tool schema 注入 | ⚠️ 部分实现 | 无 Token 预算追踪与裁剪；Tools Schema 与 Agent 身份不绑定 |
| 4 | **Tool Calling 调度** | 多 Tool Call 的调度策略（串行/并行/部分并行）、错误处理、结果聚合 | Pipeline 的 `concurrency` 模式（serial/parallel/limited） | ✅ 已有 Pipeline | 但 Pipeline 不是为 ReAct 循环设计的——缺少"执行 → 反馈 → 再次调用 LLM"的迭代语义 |
| 5 | **会话管理** | 会话创建/激活/处理/空闲/超时/关闭的状态管理 | Chat Session 状态机（ACTIVE/PROCESSING/IDLE/ERROR/TIMEOUT/CLOSED）+ SQLite sessions.db | ✅ 已实现 | 会话级 Tool 访问控制缺失；会话元数据缺少 Agent 绑定 |
| 6 | **Agent 级 Tool 访问控制** | 不同 Agent 可访问不同 Tool 集合；Tool 调用受 Agent 身份约束 | ToolRegistry 全局注册，无 Agent 级隔离 | ❌ 未实现 | 需要 `Agent → [Tool]` 映射表，Dispatcher 或 Harness 层做权限校验 |
| 7 | **Memory 集成** | 长期记忆的存储、检索与注入到 Context | `~/.aman/agents/*/memory/` 目录 + Memory trait | ✅ 已实现 | 需接入 ReAct 循环：每次 LLM 调用前自动检索相关记忆 |
| 8 | **流式输出** | LLM 回复的分块发布，页面逐步渲染 | `LLM_STREAM_START/CHUNK/TOOL_CALL/TOOL_RESULT/DONE` 事件系列 | ✅ 已实现 | 需 Harness 在 ReAct 循环中统一管理流式事件的发布 |
| 9 | **中断与恢复** | 用户 /stop 终止当前处理，恢复前一个 IDLE 状态 | Session 状态机 TIMEOUT→IDLE 转移 + CancellationToken | ✅ 已实现 | 需 Agent 级 /stop（不仅仅是会话级）：中断当前 ReAct 循环，保留已处理的部分结果 |
| 10 | **多 Agent 协调** | Agent 之间的事件传递、任务委托、结果共享 | config.yaml agents 列表 + `~/.aman/agents/*/` 数据隔离 | ⚠️ 设计与目录结构完成 | 无运行时 Agent 间事件路由；无 Agent 间消息传递协议 |
| 11 | **Token 预算与 Context Window 管理** | 跟踪每次 LLM 调用的 Token 消耗，在超限前做历史裁剪/摘要 | 无对应设计 | ❌ 未设计 | Agent Harness 层需管理累计 Token 消耗，在接近 Context Window 上限时自动压缩历史 |
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

```
        MESSAGE_RECEIVED
               │
               ▼
        ┌──────────────┐
        │  LLM Skill   │
        │              │
        │ 1. 加载 SOUL │
        │ 2. 加载历史   │
        │ 3. 组装上下文 │
        │ 4. 调用 LLM   │──── LLM 返回 text/tool_calls ────┐
        │ 5. 如果是     │                                   │
        │    tool_call  │── → 执行 Tool → 结果附加到消息列表 │
        │ 6. 再次调用    │──────────────────────────────────┘
        │    LLM        │
        │ 7. 输出回复   │
        └──────────────┘
               │
               ▼
         LLM_REPLY_READY
```

**问题：**
- LLM Skill 兼任了"上下文组装 + 循环控制 + 工具调度 + 输出发布"多重职责
- Tool Calling 循环的迭代控制逻辑与 Skill 业务逻辑耦合
- 没有 Token 预算管理（Context Window 满了怎么办？）
- 没有 Agent 级 Tool 访问控制（Skill 能调用任何注册的 Tool）
- 中断只能终止整个 Skill 处理，不能终止当前 ReAct 循环后保留会话

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
        │    ├── 发布 AGENT_PROCESSING_STARTED 事件     │
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
        │    │    ├── 发布 LLM_CALL_STARTED       │     │
        │    │    ├── 调用 LLM Provider Tool      │     │
        │    │    └── 发布 LLM_CALL_ENDED         │     │
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
        │    │    │   │ 发布 TOOL_*   │   │      │     │
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
        │    ├── 发布 LLM_REPLY_READY / stream_*  │     │
        │    ├── 发布 AGENT_PROCESSING_FINISHED    │     │
        │    └── 更新 TokenBudget 记录             │     │
        │                                          │     │
        └──────────────────────────────────────────────┘
```

### 3.3 ReAct 循环的事件流

每次 LLM 调用和 Tool 执行都通过 Event Bus 发布事件，保持 Aman 的"万物皆事件"原则：

```
Agent ReAct 循环事件序列（一次用户消息 → 最终回复）：

Phase 1:
  AGENT_PROCESSING_STARTED → { agent_id, session_id, message_id }

Phase 2 (迭代 1..N 次):
  ┌─ LLM_CALL_STARTED     → { agent_id, session_id, turn, model, input_tokens }
  │  LLM_CALL_COMPLETED   → { agent_id, session_id, turn, output_tokens, has_tool_calls }
  │
  │  [如果有 Tool Calls]:
  │    TOOL_CALL_DISPATCHED  → { agent_id, session_id, turn, tool_name, tool_args }
  │    TOOL_CALL_COMPLETED   → { agent_id, session_id, turn, tool_name, result_summary }
  │  [或 Tool 失败]:
  │    TOOL_CALL_FAILED      → { agent_id, session_id, turn, tool_name, error }
  │
  │  TOOL_RESULTS_FED_BACK  → { agent_id, session_id, turn, n_results }
  └─ [循环]

Phase 3:
  AGENT_REPLY_STREAM_START → { agent_id, session_id }
  AGENT_REPLY_CHUNK       → { agent_id, session_id, content }
  AGENT_REPLY_STREAM_DONE → { agent_id, session_id, finish_reason }
  AGENT_PROCESSING_FINISHED → { agent_id, session_id, total_llm_calls, total_tool_calls, total_tokens, latency_ms }
```

### 3.4 中断（Interrupt）的事件流

```
用户发送 /stop → MESSAGE_RECEIVED { content: "/stop" }

AgentHarness:
  1. 检测到 session_id 匹配当前正在处理的会话
  2. 设置 InterruptFlag（共享原子变量 / CancellationToken）
  3. ReAct 循环在下一次迭代开始前检查 Flag
  4. 循环终止，进入 Phase 3 输出:
     └─ 发布 AGENT_REPLY_INTERRUPTED → { agent_id, session_id, processed_turns }
  5. Session 状态回到 IDLE，等待下一条用户消息
```

对比当前链路：当前 /stop 是在 Skill 级别通过 CancellationToken 中断整个处理，
Harness 版本在 Agent 级别管理中断，可以输出"已处理了 N 轮"的中间结果，
而非整个丢弃。

---

## 4. 里程碑与任务拆分

### M1：Agent 运行时类型 ⭐ P0

> 目标：定义 Agent 的运行时类型系统，使 Agent 成为框架的一等公民。
> 验收：AgentRuntime 可以注册/查询/创建 Agent 实例。

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
5. Agent 注册/状态变更时发布 `AGENT_REGISTERED` / `AGENT_STATUS_CHANGED` 事件

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

#### T1.4 — Tauri IPC 添加 Agent 管理端点

| 属性 | 内容 |
|------|------|
| 涉及 | `crates/tauri/src/commands.rs` |
| 描述 | 新增 `list_agents`、`get_agent`、`set_agent_status` IPC 命令 |

---

### M2：ReAct 循环引擎 ⭐ P0

> 目标：实现可复用的 Think-Act-Observe 循环引擎，统一管理 LLM 调用、Tool 执行、结果反馈的迭代过程。
> 验收：AgentHarness 可以接收 MESSAGE_RECEIVED 事件，完整执行 ReAct 循环并输出最终回复。

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
   c. 发布 LLM_CALL_STARTED 事件
   d. 调用 LLM Provider Tool → 获取响应
   e. 发布 LLM_CALL_COMPLETED 事件
   f. 分类响应:
      - text_only → 结束循环
      - has_tool_calls → g
      - error → 走错误处理
      - interrupted → 终止循环
   g. 遍历 Tool Calls:
      - 权限校验（该 Agent 是否可以使用该 Tool）
      - 串行执行（或 parallel，取决于配置）
      - 发布 TOOL_CALL_DISPATCHED / TOOL_CALL_COMPLETED / TOOL_CALL_FAILED
      - 结果格式化为 ChatMessage
   h. 发布 TOOL_RESULTS_FED_BACK
   i. 回到 a
9. 将最终回复写入 SessionHistory
10. 写入长期记忆（如需要）
11. 发布 AGENT_REPLY_* 系列事件
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
7. 在 ReAct 循环各节点发布事件（LLM_CALL_STARTED/COMPLETED、TOOL_CALL_DISPATCHED/COMPLETED/FAILED 等）

#### T2.3 — 注册 AgentHarness 到 Dispatcher

| 属性 | 内容 |
|------|------|
| 涉及 | `crates/runtime/src/agent_runtime.rs`、`crates/dispatcher/src/` |
| 描述 | 使 AgentHarness 成为 MESSAGE_RECEIVED 事件的处理器 |

**子任务：**
1. 在 AgentRuntime::build() 中创建 AgentHarness 实例
2. 在 Dispatcher 中注册路由：`MESSAGE_RECEIVED` → `AgentHarness.process_message()`
3. 保留现有的 Agent 级会话等待队列（同一会话串行，跨会话并行）
4. 迁移 `session:started/closed/timeout` 事件从当前 Chat 处理器到 AgentHarness

#### T2.4 — 集成流式输出

| 属性 | 内容 |
|------|------|
| 涉及 | `crates/runtime/src/agent_harness.rs` |
| 描述 | LLM Provider 返回流式响应时，Harness 实时发布 Stream 事件 |

**子任务：**
1. 支持流式 LLM Provider Tool 调用（`StreamingLLMProvider` trait）
2. ReAct 循环的 Phase 2 Step 2 使用流式调用
3. Agent 最终回复时按 chunk 发布 `AGENT_REPLY_CHUNK` 事件
4. Tool Call 结果也在流中发布（`AGENT_TOOL_CALL_NOTIFICATION`）

---

### M3：Agent 级 Tool 访问控制 ⭐ P1

> 目标：Tool 的可用性绑定到 Agent 身份，不同 Agent 可以使用不同的 Tool 集合。
> 验收：Agent A 可以调用 tool-X，Agent B 调用 tool-X 时被拒绝。

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
2. 权限拒绝时返回结构化错误（`TOOL_CALL_FAILED { reason: "permission_denied" }`）
3. ReAct 循环中 Tool 权限错误 → 将错误消息作为 LLM 的下一次输入
   （让 LLM 知道该 Tool 不可用，可以尝试其他方法）
4. 添加审计日志记录

---

### M4：Token 预算与 Context Window 管理 ⭐ P1

> 目标：追踪 Token 消耗，在超限前自动压缩历史，防止 Context Window 溢出。
> 验收：长时间对话中，当累计 Token 接近上限时，最旧的历史被自动摘要/裁剪。

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
4. 压缩发生时发布 `HISTORY_COMPRESSED` 事件（通知前端）

---

### M5：Memory 集成到 ReAct 循环 ⭐ P2

> 目标：在每次 LLM 调用前自动检索相关记忆，注入 Context。
> 验收：Agent 可以在对话中回忆之前会话中存储的信息。

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

### M6：中断与恢复增强 ⭐ P2

> 目标：用户 /stop 可以中断当前 ReAct 循环并保留中间结果。
> 验收：用户在 Tool Calling 循环中发送 /stop，Agent 输出已完成的部分。

#### T6.1 — 注册 InterruptFlag 到 AgentHarness

| 属性 | 内容 |
|------|------|
| 涉及 | `crates/runtime/src/agent_harness.rs` |
| 描述 | ReAct 循环检查全局中断标志 |

**子任务：**
1. AgentHarness 在 ReAct 循环中（每次迭代开始前）检查 `InterruptFlag`
2. `InterruptFlag` 由 Agent 级别的 `CancellationToken` 实现
3. 中断时发布 `AGENT_REPLY_INTERRUPTED` 事件
4. Session 状态回到 IDLE，等待下一条消息

#### T6.2 — 中断恢复

| 属性 | 内容 |
|------|------|
| 涉及 | `crates/runtime/src/agent_harness.rs` |
| 描述 | 中断后用户新消息可以继续同一会话，历史保持不变 |

---

### M7：多 Agent 运行时协调 ⭐ P3

> 目标：Agent 之间可以互相传递事件和任务。
> 验收：Agent A 可以发布事件触发 Agent B 的处理。

#### T7.1 — Agent 间事件路由

| 属性 | 内容 |
|------|------|
| 涉及 | `crates/dispatcher/src/` |
| 描述 | Dispatcher 支持按目标 Agent 路由事件 |

**子任务：**
1. 新增事件类型 `AGENT_MESSAGE`（`{ from_agent, to_agent, content }`）
2. Dispatcher 增加按 `to_agent` 字段路由的规则
3. AgentHarness 可以发布事件给其他 Agent

#### T7.2 — Agent 间消息协议

| 属性 | 内容 |
|------|------|
| 涉及 | `crates/core/src/` |
| 描述 | 定义 Agent 间消息的标准格式 |

```
AGENT_MESSAGE {
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

### 5.1 符合 Aman 设计理念

- **万物皆事件**：AgentHarness 的所有关键操作（LLM 调用开始/结束、Tool 执行、循环迭代）都通过 Event Bus 发布事件，不引入新的执行模型
- **响应即行为**：AgentHarness 本身是事件处理器——它响应 `MESSAGE_RECEIVED`、`RETRY_CMD`、`STOP_CMD` 等事件，不主动轮询
- **通过 Event Bus 通信**：AgentHarness 不直接访问 LLM Provider 或 ToolRunner，而是通过已有的 ToolRunner（由 Pipeline 或直接调用调度）

### 5.2 增量实现

- 每一层都向后兼容：M1 只新增类型，不改现有逻辑
- M2 完成后，现有的 LLM Skill 逻辑可以逐步迁移到 AgentHarness
- 迁移期间，LLM Skill 和 AgentHarness 可以并存（通过 Dispatcher 路由配置决定谁处理 MESSAGE_RECEIVED）

### 5.3 已有设施复用

| 已有设施 | 在 Harness 中的用途 |
|---------|-------------------|
| Event Bus | Harness 的全部关键操作通过事件发布（AGENT_*, LLM_*, TOOL_*） |
| ToolRunner | ReAct 循环中的 Tool 执行 |
| Workflow | Session 状态管理（复用现有 chat-session 状态机） |
| SOUL | Agent 身份与行为边界的来源 |
| Secret Store | LLM Provider API Key 解析 |
| Pipeline | Tool Calling 调度（serial/parallel/limited 复用 Pipeline 并发模型） |
| Dispatcher | MESSAGE_RECEIVED → AgentHarness 路由 |
| Idle System | Agent 空闲时的 Reflection 和深度空闲 |
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
M1 (Agent Runtime 类型) ⭐ P0
│
├── M2 (ReAct 循环引擎) ⭐ P0
│   ├── M2.1 (Core trait)
│   ├── M2.2 (AgentHarness 实现)
│   ├── M2.3 (Dispatcher 注册)
│   └── M2.4 (流式输出集成)
│
├── M3 (Tool 访问控制) ⭐ P1
│   ├── M3.1 (权限模型)
│   └── M3.2 (权限校验)
│
├── M4 (Token 预算) ⭐ P1
│   ├── M4.1 (TokenBudget)
│   └── M4.2 (历史压缩)
│
├── M5 (Memory 集成) ⭐ P2
│   ├── M5.1 (检索集成)
│   └── M5.2 (自动写入)
│
├── M6 (中断/恢复) ⭐ P2
│
└── M7 (多 Agent 协调) ⭐ P3
    ├── M7.1 (事件路由)
    └── M7.2 (消息协议)
```

| 依赖 | 说明 |
|------|------|
| M1 → (无) | 基础，独立实现 |
| M2 → M1 | AgentRegistry 为 Harness 提供 Agent 身份查询 |
| M3 → M2 | ToolExecutor 是 ReAct 循环的组成部分 |
| M4 → M2 | TokenBudget 在 Context Assembly 中使用 |
| M5 → M2 | Memory 在 Context Assembly 阶段注入 |
| M6 → M2 | InterruptFlag 在 ReAct 循环中检查 |
| M7 → M1 + 现有 Dispatcher | 依赖 Agent 身份和事件路由能力 |
