# Rig vs Aman CognitiveEngine — 架构设计深度对比

> 2026-06-06

两者的根本差异可以归结为一句话：**rig 是一个 LLM 库，aman 是一个认知运行时**。这不是好坏之分，而是完全不同的抽象层级。

---

## 一、核心抽象的定位

### Rig：Concrete Agent（具体智能体）

```rust
// rig 的设计 —— Agent 是一个具体 struct，不是 trait
pub struct Agent<M: CompletionModel> {
    model: Arc<M>,                    // 模型实例
    preamble: Option<String>,         // system prompt
    tools: ToolServerHandle,          // 工具集
    memory: Option<Arc<dyn ConversationMemory>>,  // 对话记忆
    static_context: Vec<Document>,    // 静态上下文文档
    dynamic_context: Arc<Vec<...>>,   // 向量存储索引
    temperature: Option<f64>,
    max_tokens: Option<u64>,
    default_max_turns: Option<usize>, // 最大循环轮次
    output_schema: Option<Schema>,    // 结构化输出 schema
    // ...
}
```

rig 的 `Agent` 是 **"所有东西都在一起的盒子"** — 模型、工具、记忆、上下文全部封装在一个 struct 里。调用者只需要：

```rust
let response = agent.prompt("写一个排序函数").await?;
```

Agent 内部自己管理 ReAct 循环、工具调用、记忆更新。**调用者看不到中间过程。**

### Aman：Pure Function Trait（纯函数抽象）

```rust
// aman 的设计 — CognitiveEngine 是一个 trait，定义的是契约
#[async_trait]
pub trait CognitiveEngine: Send + Sync {
    fn name(&self) -> &str;

    async fn process(
        &self,
        ctx: &CognitiveContext,           // 引擎无关的上下文
        observations: Vec<Observation>,   // 输入：事件流
    ) -> Result<Vec<Decision>, CognitiveError>;  // 输出：决策流

    fn subscribe(&self, listener: Arc<dyn CognitiveListener>);
    fn unsubscribe(&self, listener: &Arc<dyn CognitiveListener>);
    async fn reset_session(&self, session_id: &str) -> Result<(), CognitiveError>;
}
```

Aman 的引擎是一个 **纯函数**：`(Context, Observations) → Decisions`。引擎**不拥有**工具、不管理记忆、不执行 ReAct 循环 — 它只是把观察转化为决策，其他都是外层（Gateway）的职责。

---

## 二、工具系统的设计对比

### Rig：Tool 是 Agent 的内部组件

```
Agent 内部 → ToolServerHandle → Tool::call() → 结果直接返回给 Agent
                                      ↑
                              Agent 自己也是 Tool
                              （子智能体递归调用）
```

- `Tool` trait 有 `call()` 方法，**Agent 自己调用工具并获取结果**
- Agent 实现了 `Tool` trait，所以一个 Agent 可以作为另一个 Agent 的工具（子智能体）
- 工具执行是 **同步阻塞在 Agent 内部** 的
- `ToolEmbedding` 让工具可以被 RAG 检索

### Aman：Tool 是外部事件循环的一部分

```
CognitiveEngine::process() → Decision::CallTools → Gateway 执行工具
                                                         ↓
                                              Observation::ToolCompleted
                                                         ↓
CognitiveEngine::process() ← (新的观察)
```

- 引擎**不执行工具** — 它只输出 `Decision::CallTools`
- Gateway 拿到 Decision，在沙箱中执行工具，结果以 `Observation::ToolCompleted` 的形式**再次进入引擎**
- 这是一个**外部的、可观测的循环**，不是引擎内部的隐藏步骤

**这是最关键的架构差异**：rig 的工具执行是引擎内部的实现细节，aman 的工具执行是平台层的事件循环。

---

## 三、Decision 的语义丰富度

### Rig：输出就是文本（或结构化 JSON）

```rust
// rig 的 prompt() 返回
Result<String, PromptError>       // 文本
Result<T, PromptError>            // TypedPrompt → 结构化提取
```

rig 的输出本质上是 **LLM 的 completion 结果**。工具调用虽然存在，但被封装在 Agent 内部消化了。

### Aman：输出是丰富的"决策"语义

```rust
pub enum DecisionKind {
    Reply { text: String, is_final: bool },       // 回复文本
    CallTools { calls: Vec<ToolCallRequest>, ... }, // 调用工具
    Delegate { target_agent_id: String, task: String }, // 委托给其他 Agent
    WaitFor { event_types: Vec<String>, timeout_ms: Option<u64> }, // 等待事件
    Remember { key: String, content: String, importance: f64 },    // 存入记忆
    NoOp,                                         // 什么都不做
}
```

aman 的 Decision 不只是 "LLM 说了什么"，而是**完整的智能体行为语义**：

| Decision | 含义 |
|---|---|
| `Reply` | 文本回复（支持 streaming chunk） |
| `CallTools` | 调用工具（阻塞或 detach） |
| `Delegate` | 委托给另一个 Agent — 多智能体协作 |
| `WaitFor` | 暂停等待外部事件（timer、webhook、另一个 agent） |
| `Remember` | 主动记忆管理 |
| `NoOp` | 空闲状态，什么都不做 |

这些语义超出了 LLM completion 的范畴，是**认知运行时的原生概念**。

---

## 四、上下文的抽象层级

### Rig：上下文 = ChatHistory + Documents

```rust
// rig 的上下文是 LLM 原生的
agent.chat(prompt, &mut chat_history)  // 对话历史
agent.static_context                    // Vec<Document> 静态文档
agent.dynamic_context                   // VectorStore 检索结果
agent.memory                            // ConversationMemory
```

所有上下文最终都变成 LLM 的 prompt tokens。

### Aman：上下文 = CognitiveContext（引擎无关）

```rust
pub struct CognitiveContext {
    pub agent_id: String,
    pub session_id: String,
    pub identity: CognitiveIdentity,     // 身份（name, boundaries, expertise, vibe, raw）
    pub capabilities: Vec<Capability>,   // 能力列表（Tool | Skill）
    pub memory_context: Vec<MemoryItem>, // 记忆项
    pub engine_config: Value,            // 引擎特定配置（不透明 JSON blob）
}

pub struct CognitiveIdentity {
    pub name: String,                    // 显示名称
    pub identity: String,                // 核心身份声明
    pub boundaries: Vec<String>,         // 行为边界（不能做什么）
    pub expertise: Vec<String>,          // 专长领域
    pub vibe: Option<String>,            // 沟通风格
    pub raw: String,                     // 原始 SOUL.md 内容
}
```

关键设计决策：
- `CognitiveIdentity` 包含 **boundaries（行为边界）** 和 **expertise（专长领域）** — 这不是 prompt engineering，而是运行时约束
- `engine_config` 是一个 **不透明 JSON blob** — LLM 引擎可以放 model/temperature，世界模型引擎可以放 latent dims
- 这个 struct **可以喂给任何类型的引擎**，不假设引擎是 LLM

---

## 五、Provider 抽象的对比

### Rig：CompletionModel trait（一层抽象）

```rust
// 统一 20+ LLM 提供商的 API 差异
pub trait CompletionModel {
    type Response;
    type StreamingResponse;
    // ...
}
```

所有 provider 实现了同一个 trait，可以互换。但**只覆盖 LLM 类模型**。

### Aman：LlmProvider + CognitiveEngine（两层抽象）

```rust
// 第一层：LLM 提供商抽象（provider-agnostic）
pub trait LlmProvider: Send + Sync {
    fn name(&self) -> &str;
    async fn chat_completion(
        &self,
        req: LlmChatRequest,
        cb: Option<Arc<dyn Fn(StreamEvent) + Send + Sync>>,
    ) -> Result<LlmResponse, String>;
}

// 第二层：认知引擎抽象（engine-agnostic）
pub trait CognitiveEngine: Send + Sync { ... }
```

Aman 有两层抽象：
1. **`LlmProvider`** — 统一 OpenAI/Anthropic/Ollama 等 LLM 后端（类似 rig 的 `CompletionModel`）
2. **`CognitiveEngine`** — 统一 LLM / 世界模型 / 混合系统等**不同类型的"大脑"**

rig 没有第二层。它假设所有智能体都是 LLM-based 的。aman 的第二层允许未来接入非 LLM 的认知引擎而不需要改动 Gateway 代码。

---

## 六、流式处理的对比

### Rig：独立的 Streaming trait

```rust
// streaming 作为独立的 API 路径
agent.stream_prompt("...").await  // → StreamingPromptRequest
agent.stream_chat("...", &mut history).await
```

Streaming 和 non-streaming 是不同的 trait 路径。

### Aman：CognitiveListener 观察者模式

```rust
pub trait CognitiveListener: Send + Sync {
    fn on_cognitive_event(&self, event: CognitiveEvent);
}

pub enum CognitiveEvent {
    TextChunk { session_id: String, text: String },
    StreamStart { session_id: String },
    StreamDone { session_id: String, finish_reason: String },
    StreamError { session_id: String, error: String },
    Diagnostic { session_id: String, engine_name: String, data: Value },
}
```

Streaming 通过 **观察者模式** 从引擎"推送"出来，而不是通过返回值：
- 引擎不需要知道谁会消费这些事件
- 同一个引擎可以被多个 listener 监听
- `Diagnostic` 事件允许引擎暴露内部状态（reasoning traces、confidence scores 等）
- `process()` 的返回值（`Vec<Decision>`）是**最终决策**，中间的 streaming chunk 走 listener

---

## 七、记忆系统对比

### Rig：ConversationMemory trait（Agent 拥有记忆）

```rust
// 可选组件，Agent 内部管理
memory: Option<Arc<dyn ConversationMemory>>
```

记忆 = 对话历史的存储和检索。Agent 内部使用，对调用者不透明。

### Aman：外部化的记忆

引擎不管理记忆。它通过两条路径与记忆交互：

1. **输入**：`CognitiveContext::memory_context` — Gateway 在调用 `process()` 之前**主动检索并注入**
2. **输出**：`Decision::Remember` — 引擎**主动要求存储**某些内容

这意味着：
- 记忆检索策略由 Gateway/平台控制（可以 A/B 测试不同策略而不改引擎代码）
- 记忆存储是平台层的能力，多个引擎共享同一套记忆后端
- 引擎对记忆系统**零依赖** — 它只是接收和发出记忆相关的信号
- 引擎不需要知道记忆是存在 SQLite、Redis、还是向量数据库中

---

## 八、架构全景图

```
┌─────────────────────────────────────────────────────────────────┐
│  rig (Library)                                                   │
│                                                                  │
│  User Code                                                       │
│     │                                                            │
│     ▼                                                            │
│  Agent::prompt("...")   ← 一个方法调用，同步等待结果               │
│     │                                                            │
│     ├── CompletionModel::completion()  ← LLM 调用                 │
│     ├── Tool::call()                   ← 内部执行工具              │
│     ├── ConversationMemory             ← 内部管理记忆              │
│     └── 返回 String                    ← 文本输出                 │
│                                                                  │
│  Agent 是一个黑盒 — 你给它输入，它给你输出                         │
└─────────────────────────────────────────────────────────────────┘

┌─────────────────────────────────────────────────────────────────┐
│  aman (Platform / Runtime)                                       │
│                                                                  │
│  EventBus                                                        │
│     │                                                            │
│     ▼                                                            │
│  Gateway                                                         │
│     │                                                            │
│     ├── 检索记忆 ──→ CognitiveContext.memory_context              │
│     ├── 构建上下文 ──→ CognitiveContext                           │
│     │                                                            │
│     ▼                                                            │
│  CognitiveEngine::process(ctx, observations)  ← 纯函数            │
│     │                                                            │
│     ▼                                                            │
│  Vec<Decision>                                                   │
│     │                                                            │
│     ├── Reply ──→ 推送到 EventBus → 用户看到                      │
│     ├── CallTools ──→ Sandbox 执行 → Observation → 回到引擎       │
│     ├── Delegate ──→ 路由到另一个 Agent                           │
│     ├── WaitFor ──→ 暂停，等待外部事件                             │
│     └── Remember ──→ 写入记忆存储                                 │
│                                                                  │
│  引擎只是管道中的一个环节，周围是完整的事件循环                      │
└─────────────────────────────────────────────────────────────────┘
```

---

## 九、各自的取舍和后果

| 维度 | rig | aman |
|---|---|---|
| **抽象层级** | 库级别：封装 LLM + 工具 + 记忆 | 平台级别：定义认知运行时的契约 |
| **扩展方式** | 换 provider → 实现 `CompletionModel` | 换"大脑" → 实现 `CognitiveEngine` trait |
| **工具执行** | Agent 内部同步执行 | 外部沙箱执行，通过事件循环反馈 |
| **多智能体** | Agent 作为 Tool（嵌套调用） | `Decision::Delegate`（事件总线路由） |
| **流式输出** | 独立 API 路径（`StreamingPrompt` trait） | 观察者模式（`CognitiveListener`） |
| **记忆** | Agent 拥有记忆组件 | 引擎无记忆依赖，平台层管理 |
| **状态管理** | Agent 实例持有所有状态 | 引擎内部不透明 + 外部 session 状态 |
| **可观测性** | 取决于具体实现 | `CognitiveListener` + `Diagnostic` 事件原生支持 |
| **假设前提** | 所有智能体都是 LLM-based | 智能体可以是任何东西 |
| **使用复杂度** | 低：一个方法调用 | 高：需要理解事件循环和 Gateway |
| **平台能力** | 无：不提供事件总线、持久化、秘钥管理 | 完整：WAL、DLQ、沙箱、多协议、插件系统 |

---

## 十、总结

rig 是"把复杂性封装在库里面"，aman 是"把复杂性暴露为平台能力"。前者适合做**工具**，后者适合做**操作系统**。两者并不冲突 — 事实上，aman 的 `LlmCognitiveEngine` 内部完全可以集成 rig 作为其 LLM 调用层，享受 rig 的 20+ provider 支持和成熟的工具系统，同时保留 aman 的事件驱动架构、`CognitiveEngine` trait 的引擎无关性、以及平台层的能力（沙箱、多智能体路由、持久化等）。
