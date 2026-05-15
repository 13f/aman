# LLM 对话能力设计

> 基于 Aman 事件响应式架构的 LLM 对话需求说明。不涉及具体语言或框架，聚焦业务语义与集成方式。

---

## 1. 设计原则

Aman 的核心公理是 **"万物皆事件，响应即行为"**。LLM 对话遵循同样的原则：

1. **用户的聊天消息是一个事件**——由 ChatPlatform 事件源产生 `MESSAGE_RECEIVED` 事件，进入统一事件总线。没有"主循环等待用户输入"，只有"事件到达后响应"。
2. **LLM 的回复是事件处理的结果**——LLM 对话不是一个持续的会话循环，而是一次事件的触发-处理-输出链路。
3. **对话历史是持久化状态**——由 Workflow 或 State Store 管理，不是进程内的内存变量。
4. **聊天页面是一个"事件终端"**——它不是一个传统聊天 UI，而是一个**双向事件查看器**：向上展示事件流（消息、工具调用、系统事件），向下发送事件（用户输入、文件、配置变更）。

与传统 Chatbot 框架的根本区别：传统框架以 Chat 循环为中心，Aman 以统一事件循环为中心，LLM 对话只是众多事件源中的一种。

---

## 2. 架构定位

```
                    ┌──────────────┐
                    │  Chat 用户    │
                    └──────┬───────┘
                           │ 输入消息
                    ┌──────▼───────┐
                    │ ChatPlatform │  ← 事件源: 产生 MESSAGE_RECEIVED
                    │   Source     │     信任等级: untrusted
                    └──────┬───────┘
                           │ MESSAGE_RECEIVED 事件
                    ┌──────▼───────┐
                    │   Event Bus  │  ← 统一事件通道
                    └──────┬───────┘
                           │
                    ┌──────▼───────┐
                    │  Dispatcher  │  ← 路由到匹配的 LLM Skill
                    └──────┬───────┘
                           │
                    ┌──────▼───────┐
                    │  LLM Skill   │  ← Skill: 组装 SOUL + 历史 + 用户消息
                    │              │     调用 LLM Provider Tool
                    └──────┬───────┘
                           │
                    ┌──────▼───────┐
                    │ LLM Provider │  ← Tool: 对接外部 LLM API
                    │    Tool      │     (OpenAI / Anthropic / 本地模型)
                    └──────┬───────┘
                           │ LLM 回复文本
                    ┌──────▼───────┐
                    │  回复输出     │  → 通过事件源返回给用户
                    └──────────────┘
```

关键语义：

| 组件 | 在 LLM 对话中的角色 |
|------|-------------------|
| ChatPlatform Source | 事件源，监听用户输入并产生 `MESSAGE_RECEIVED` 事件 |
| Event Bus | 承载用户消息事件的通道，受背压/去重/优先级控制 |
| LLM Skill | 消费 `MESSAGE_RECEIVED` 事件，组装请求并触发 LLM 调用 |
| LLM Provider Tool | 执行 LLM API 调用的工具，一个 Provider 对应一个 Tool 实例 |
| SOUL | 提供 LLM system prompt（身份/边界/偏好），由 LLM Skill 在请求中注入 |
| Workflow | 管理多轮对话状态（会话保持、角色切换、话题分支） |
| InputSanitizer | 用户消息进入 LLM 前的注入检测与消毒（trust_level: untrusted） |
| OutputValidator | LLM 回复返回给用户前的泄漏检查 |

### 2.1 生命周期 Phase 映射

LLM 对话组件在 Aman Runtime 生命周期（Phase 0→5 启动，Phase 5→0 关闭）中的映射如下：

**启动时序（Phase 0→5）：**

| Phase | 组件操作 | 说明 |
|-------|---------|------|
| 0.5 | SecretResolver 解析 LLM Provider API Key | API Key 必须在所有 Provider 调用前就绪 |
| 2 | WAL 恢复：重放未消费 MESSAGE_RECEIVED 到 Event Bus | Event Bus 必须已初始化（Phase 2+），但 ChatPlatformSource 未启动时事件只入 Bus 不分发 |
| 3 | LLM Skill 注册到 Skill 系统 | Dispatcher 开始路由 MESSAGE_RECEIVED 到 LLM Skill |
| 4 | ChatPlatformSource 启动并监听端口 | WebSocket 可接受连接，CLI 开始读取 stdin |
| 5 | 健康检查 Ready | `/health` 端点返回 200，可接受用户流量 |

**关闭时序（Phase 5→0，逆序）：**

| Phase | 组件操作 | 说明 |
|-------|---------|------|
| 5→4 | ChatPlatformSource 停止监听；拒绝新连接 | 已有连接的 WebSocket 等待当前消息处理完成（500ms 缓存窗口，同 /stop 逻辑） |
| 4→3 | LLM Skill 取消注册 | 不再接收新的 MESSAGE_RECEIVED 路由 |
| 3→2 | WAL Flush：确保所有已处理事件被确认 | 未 ACK 的事件在下次启动时通过 WAL 重放 |
| 2→1 | 关闭 Event Bus | 停止事件分发 |

**时序约束：**

- WAL 恢复（Phase 2）必须在 LLM Skill 注册（Phase 3）**之前**完成，防止重放事件和实时事件交错
- ChatPlatformSource（Phase 4）必须在 WAL 恢复之后启动，防止 Source 产生实时事件时 WAL 尚未就绪
- 关闭时，LLM Skill 取消注册后，ChatPlatformSource 拒绝的新连接应返回"服务正在关闭"提示

---

## 3. 事件源: ChatPlatformSource

ChatPlatform Source 是一个 Push 模式的事件源，负责将用户输入转换为框架内部事件。

**产生的唯一事件类型：** `MESSAGE_RECEIVED`

**事件 Payload 结构：**

```json
{
  "event_id":    string      // 全局唯一事件 ID (UUID v7)，用于幂等去重
  "channel":     string      // 对话渠道标识 (终端/WebSocket/桌面端/Slack/Discord)
  "user":        string      // 用户标识
  "message":     string      // 用户输入的文本
  "session":     string      // 会话标识 (多轮对话关联)
  "server_ts":   timestamp   // 服务端消息到达时间 (Event Bus 入站时间)
  "client_ts":   timestamp   // 客户端用户操作时间 (用于跨渠道顺序仲裁)
}
```

**信任等级：** `untrusted`。所有来自 ChatPlatformSource 的事件 Payload 在传递给 LLM 前必须经过输入消毒（InputSanitizer）。

**事件源配置项：**

| 配置 | 说明 | 默认 |
|------|------|------|
| channel_type | 渠道类型: terminal / websocket / tauri_desktop | terminal |
| listen_addr | 监听地址（WebSocket 模式） | 127.0.0.1:0 |
| session_timeout | 会话空闲超时 | 300s |
| max_message_length | 单条消息最大长度 | 4096 字符 |
| rate_limit | 用户消息频率限制 | 10 条/分钟 |

**支持多渠道：** 同一个 ChatPlatformSource 可以同时监听多个渠道（终端、桌面端、WebSocket），所有渠道产生的 `MESSAGE_RECEIVED` 事件进入同一事件总线，由 Dispatcher 统一路由。

---

## 4. LLM Skill

LLM Skill 是消费 `MESSAGE_RECEIVED` 事件的核心处理器。它负责三点：

1. **组装上下文**：读取 SOUL 的 system prompt + 历史会话 + 当前用户消息
2. **调用 LLM**：调用 LLM Provider Tool 获取回复
3. **输出回复**：将 LLM 回复发布为回复事件或直接输出

**触发器条件：**

```
trigger:
  event_type: MESSAGE_RECEIVED
  match_all: false
```

**LI skill 的职责：**

```
LLM Skill 处理一个 MESSAGE_RECEIVED 事件的流程:

1. 接收 MESSAGE_RECEIVED 事件
2. 从事件 payload 提取 session 标识
3. 从 State Store 加载该会话的历史（如果有）
4. 从 SOUL 读取 to_system_prompt() 作为 system message
5. 组合: [system_prompt] + [历史消息] + [用户消息]
6. 调用 LLM Provider Tool
7. 将 LLM 回复写入 State Store（更新会话历史）
8. 发布 LLM 回复（通过回复事件或直接输出渠道）
```

注意：LLM Skill **不直接实现 LLM API 调用逻辑**，它通过 Tool Runner 调用 LLM Provider Tool。Tool 才是实际执行 API 请求的单元，Skill 负责业务编排。

**并发策略：会话级串行，跨会话并行**

多条 `MESSAGE_RECEIVED` 事件可能连续到达同一会话。为防止状态竞态，LLM Skill 的并发模型如下：

```
并发规则:
  1. 同一会话的消息 → 串行处理
     当 LLM Skill 正在处理 session-A 的第一条消息时（含 Tool Calling 循环），
     session-A 的第二条 MESSAGE_RECEIVED 进入会话级等待队列，不进入全局 Event Bus。
     队列位置信息通过事件回复给页面显示。

  2. 不同会话的消息 → 并行处理
     session-A 和 session-B 的消息各自独立处理，无互斥。

  3. 等待队列容量
     默认队列深度: 10 条/会话。
     队列深度可配置 (queue_depth_per_session)。

  4. 队列溢出策略
     检测层: ChatPlatformSource 在构造 MESSAGE_RECEIVED 事件前检查队列深度。
       - 如果已满 → 同步返回"发送失败：当前会话队列已满"到页面（WebSocket error 或 HTTP 429）
       - 如果在 Event Bus 中但 Dispatcher 发现溢出 → 丢弃并发布 MESSAGE_DROPPED 事件到页面
     配置项 queue_overflow_strategy: "drop"（默认，丢弃新消息）| "preempt_oldest"（丢弃队列中最旧未处理消息）
     preempt_oldest 模式下，被丢弃的消息通过 MESSAGE_DROPPED 事件通知页面。

  5. 队列中的消息生命周期
     会话超时关闭 → 队列清空，消息丢弃（发布 MESSAGE_CANCELLED 通知页面）。
     用户 /stop 中断当前处理 → 队列下一条消息自动开始处理。

  6. Dispatcher 路由约束：session 级分片
     即使来自不同 WebSocket 连接的同一条会话消息，也必须保证全局串行。
     Dispatcher 应在路由阶段对 session_id 做 consistent hashing 分片，
     同一 session_id 始终路由到同一个 Worker（或 actor 模型：每个 session 映射到一个 actor）。

  7. 跨渠道消息顺序仲裁
     当同一条会话的消息从不同渠道（如 Slack + WebSocket）到达时，
     MESSAGE_RECEIVED payload 中的 client_ts（用户操作本地时间）和 server_ts（Event Bus 到达时间）
     用于仲裁处理顺序：
     - 如果两条消息的 client_ts 差值 > 5 秒 → 以 client_ts 为准（用户操作顺序）
     - 如果 client_ts 差值 ≤ 5 秒 → 以 server_ts 为准（网络波动导致的乱序）
     - 仲裁在 Dispatcher 路由阶段执行，确保 LLM Skill 收到的顺序已稳定
```

**会话级等待队列的页面反馈：**

```json
{
  "event": "MESSAGE_ENQUEUED",
  "payload": {
    "session_id": "session-xxx",
    "queue_position": 2,
    "queue_position_hint": "前面还有 1 条消息"  // 阶跃式指示:
       // position 1    → "当前正在处理你的消息"
       // position 2-3  → "前面还有 {N-1} 条消息"
       // position 4+   → "队列中有多条消息等待处理"
  }
}
```

页面收到 `MESSAGE_ENQUEUED` 事件后，在用户消息气泡上显示队列位置提示。不显示预计等待时间（Tool Calling 循环长度不可预测，估算值会误导用户）。

**Event Bus 背压与会话级队列的两级协调：**

MESSAGE_RECEIVED 事件在到达会话级队列前必须先通过 Event Bus。两级容量控制互不知晓可能导致错误映射。

```
协调规则:
  - Event Bus 背压 = 基础设施级（保护进程不 OOM）
    当 Event Bus 拒绝事件（L3+ 背压）→ ChatPlatformSource 返回 HTTP 503 / WebSocket 5000
    → 页面错误提示: "系统繁忙，请稍后重试"

  - 会话级队列满 = 业务级（保护单会话不被淹没）
    事件已通过 Event Bus 但被会话队列拒绝 → 返回 HTTP 429 / WebSocket 4290
    → 页面错误提示: "当前对话消息过多，请等待处理完成"

  - 错误映射规则:
    503 → "系统繁忙，请稍后重试"
    429 → "当前对话消息过多，请等待处理完成"

  - 简化设计（可选）：将会话级队列前置到 ChatPlatformSource 层
    在 MESSAGE_RECEIVED 进入 Event Bus 前检查队列深度 → 两类容量控制不会互相干扰
```

**队列等待超时检测：**

```json
{
  "event": "QUEUE_STALLED",
  "payload": {
    "session_id": "session-xxx",
    "queue_position": 2,
    "wait_seconds": 65,
    "threshold_seconds": 60
  }
}
```

如果队列中等待超过 60 秒没有开始处理，发布 `QUEUE_STALLED` 事件到页面（可能 LLM Skill 或 Tool Runner 挂了）。

---

## 5. LLM Provider Tool

LLM Provider Tool 是实际执行 LLM API 调用的执行单元。一个 Provider 是一个 Tool 实例。

**Tool 定义：**

```
Tool {
  name: "llm-provider"
  description: "调用 LLM API 生成回复"
  parameters:
    - model:      string   // 模型名称 (gpt-4 / claude-3 / 本地模型)
    - messages:   array    // 消息列表 [{role, content}]
    - temperature: float   // 温度参数 (默认 0.7)
    - max_tokens:  integer // 最大生成 token 数
    - stop:       array    // 停止序列 (可选)
  returns:
    - content:    string   // 生成的回复文本
    - model:      string   // 实际使用的模型
    - usage:      object   // token 用量 {prompt, completion, total}
    - finish_reason: string // 结束原因 (stop / length / tool_calls)
}
```

**支持的 Provider 类型：**

| Provider | 鉴权方式 | 备注 |
|----------|---------|------|
| OpenAI | API Key (`${OPENAI_API_KEY}`) | 兼容第三方 OpenAI 代理 |
| Anthropic | API Key (`${ANTHROPIC_API_KEY}`) | |
| 本地 LLM | 无需鉴权 | 通过 HTTP 访问本地推理服务 |

API Key 通过 SecretResolver 注入，不在配置文件中明文存储。

**多 Provider 路由策略：**

| 策略 | 行为 |
|------|------|
| primary_fallback | 先调用主 Provider，失败时切换到备用 |
| cost_aware | 根据请求复杂度选择: 简单请求用低成本模型，复杂请求用高成本模型。预算计数器使用原子操作（读取→决策→扣减三步为原子事务），防止并发会话的预算超支。路由决策提前至上下文加载之后、裁剪之前（先选模型→以选定模型的 context window 为基准裁剪） |
| round_robin | 依次轮换 |
| user_preference | 由用户的会话配置指定使用的 Provider |

---

## 6. 对话状态管理

LLM 对话天然是多轮的，需要在多个 `MESSAGE_RECEIVED` 事件之间保持状态。

**方式一：Workflow 状态机（推荐）**

将一次对话定义为 Workflow 实例：

```
Workflow 定义: "chat-session"

状态:
  - ACTIVE       (初始态)   ← 会话已创建，等待用户第一条消息
  - PROCESSING              ← LLM 正在生成回复或执行 Tool Calling 循环
  - IDLE                    ← 等待用户新输入（上一条回复已完成）
  - ERROR                   ← LLM Provider 异常 / Token 耗尽 / 超时 / Tool 执行失败
  - RETRYING                ← 用户触发 /retry，正在重新尝试
  - TIMEOUT                 ← 会话空闲超时
  - CLOSED       (终态)    ← 会话结束

转移:
  ACTIVE    → PROCESSING   (事件: MESSAGE_RECEIVED)
  ACTIVE    → TIMEOUT      (事件: SESSION_TIMEOUT)    ← 创建后从未发消息
  PROCESSING → IDLE        (事件: LLM_REPLY_READY | LLM_STREAM_DONE)
  PROCESSING → ERROR       (事件: LLM_ERROR           ← Provider 异常 / 超时 / Token 耗尽)
  PROCESSING → TIMEOUT     (事件: STREAM_TIMEOUT      ← LLM 流式超时，无响应超过阈值)
  PROCESSING → CLOSED      (事件: SESSION_CLOSE_CMD   ← 用户 /session close, 关闭前执行关闭协议见 §14.11)
  IDLE      → PROCESSING   (事件: MESSAGE_RECEIVED)
  IDLE      → TIMEOUT      (事件: SESSION_TIMEOUT)
  IDLE      → CLOSED       (事件: SESSION_END)
  ERROR     → RETRYING     (事件: RETRY_CMD           ← 用户 /retry)
  ERROR     → IDLE         (事件: SESSION_END)        ← 用户放弃，关闭出错会话
  ERROR     → CLOSED       (事件: ABANDON_TIMEOUT)    ← ERROR 态过期自动归入 CLOSED
  RETRYING  → PROCESSING   (事件: RETRY_STARTED       ← 重试启动)
  RETRYING  → ERROR        (事件: RETRY_FAILED        ← 重试失败，可多次)
  TIMEOUT   → CLOSED       (事件: SESSION_END)
  TIMEOUT   → IDLE         (事件: MESSAGE_RECEIVED)   ← 超时后用户发送新消息恢复

状态转移约束:
  - PROCESSING 态不应允许新的 MESSAGE_RECEIVED 进入当前会话（参见 §4 并发策略）
  - ERROR 态中的 /retry 最多连续失败 5 次，超过后强制进入 CLOSED
  - RETRYING → PROCESSING 时，上一次交互的 trace_id 应传递给新调用
  - 所有终态 (CLOSED) 必须有一条补偿路径：不能仅依赖用户主动操作
```

每个 Workflow 实例的 `data` 字段存储会话历史：

```
data:
  session_id: "session-xxx"
  history:
    - { role: "user", content: "你好" }
    - { role: "assistant", content: "你好！有什么可以帮助你的？" }
    - { role: "user", content: "今天天气怎么样？" }
  current_provider: "openai"
  preference:
    temperature: 0.7
    model: "gpt-4"
```

Workflow 引擎负责实例的持久化和超时管理。即使 Agent 重启，对话会话可通过 State Store 恢复。

**方式二：轻量状态缓存**

对于不需要持久化的简单对话（如一次性问答），可以使用 State Store 的 TTL 缓存，以 session_id 为 key 存储最近的 N 条消息。

适用场景：终端单次问答、无状态 Webhook。

---

## 7. 多轮协作与 Tool Calling

LLM 在生成回复时可能需要调用外部工具（查询数据库、调用 API、执行计算）。这是 Agent 区别于纯 Chatbot 的核心能力。

**Tool Calling 流程：**

```
用户: "帮我查一下上周的销售额"
               │
               ▼
LLM Skill 组装上下文 → 调用 LLM Provider Tool
               │
               ▼
LLM 返回 tool_call: get_sales_data(date_range: "last_week")
               │
               ▼
LLM Skill 识别 tool_call → 调用对应 Tool (get_sales_data)
               │
               ▼
Tool 返回: { total: 125000, orders: 342 }
               │
               ▼
LLM Skill 将 Tool 结果附加到消息列表 → 再次调用 LLM Provider Tool
               │
               ▼
LLM 返回自然语言回复: "上周总销售额为 125,000 美元，共 342 笔订单。"
               │
               ▼
回复输出给用户
```

这里的"工具"就是 Aman 已有的 Tool 系统——LLM Skill 通过 Tool Runner 执行实际操作。Tool Calling 不是新功能，而是 LLM Skill 对已有 Tool 系统的编排：

1. 首次调用 LLM：system prompt 中包含可用 Tool 列表（function calling schema）
2. LLM 选择调用哪个 Tool → LLM Skill 拦截 `tool_call` 响应
3. LLM Skill 通过 Tool Runner 执行该 Tool
4. 将 Tool 返回附加到上下文 → 再次调用 LLM 获取最终回复

**可用 Tool 列表的来源：**

LLM Skill 可以访问的 Tool 集合由两部分组成：

| 来源 | 说明 |
|------|------|
| Agent 注册的全局 Tool | 框架内置工具（文件/HTTP/DB/exec）+ 插件注册的工具 |
| 会话级别 Tool | 用户在当前会话中安装或授权的工具 |

Tool 的可用性受权限控制（参见 §9 安全）。

---

## 8. SOUL 集成

SOUL 是 LLM 的 system prompt 来源。LLM Skill 在组合上下文时自动注入 `Soul::to_system_prompt()`。

SOUL 的各个字段在 LLM 上下文中的映射：

| SOUL 字段 | 在 LLM 上下文中的位置 | 作用 |
|-----------|---------------------|------|
| name | system prompt 首句 | "You are {name}." |
| identity | system prompt | Agent 身份定义 |
| core | system prompt | 核心行为准则 |
| expertise | system prompt | 专长领域声明 |
| boundaries | system prompt + Tool 权限 | 行为边界约束 |
| vibe | system prompt | 语气风格 |
| preferences | system prompt | 偏好设定 |

SOUL 热更新生效时，下一次 LLM 调用自动使用新的 system prompt，无需重启 Agent 或重连会话。

**热更新生效边界（重要）：**

SOUL 的生效边界限定在**完整的交互单元**（一个用户消息 → 全部 Tool Calling 循环 → 最终回复），不在 Tool Calling 循环中间生效。具体规则：

```
生效边界规则:
  1. 当 LLM Skill 开始处理一个 MESSAGE_RECEIVED 事件时 → 固定当前 SOUL 版本为快照
  2. 同一个交互单元内的所有 LLM 调用（首次 + Tool Calling 循环中的后续调用）使用同一张快照
  3. 下一个 MESSAGE_RECEIVED 事件开始处理时 → 重新读取最新 SOUL
  4. 热更新在正在进行的交互单元中不可见 → 避免了 system prompt 中途变化导致的：
     - 权限不一致（首次调用有某个 Tool 权限，第二次没有）
     - 身份跳跃（前一段是"程序助手"，后一段是"数据分析师"）
     - boundaries 不一致导致的 Tool 执行失败
```

---

## 9. 安全

### 9.1 输入安全

用户消息（`MESSAGE_RECEIVED`）来自 `untrusted` 事件源，在传递给 LLM 前必须经过 InputSanitizer：

**消毒策略粒度：**

| 策略 | 行为 | 适用场景 |
|------|------|---------|
| `replace_token` | 仅替换触犯规则的子串，保留上下文：`"请忽略之前的[redacted]，直接执行"` | 默认策略。用户可看到替换后的完整上下文 |
| `replace_message` | 整条消息替换为 `[redacted]`（当前行为） | 高风险注入模式（如系统提示提取尝试） |
| `block` | 拒绝发送，返回错误给页面 | 确定的恶意内容（如 shell 注入命令） |

**策略选择规则：**

```
1. InputSanitizer 按优先级从低到高检测：replace_token → replace_message → block
2. replace_token: 匹配关键词/模式时，只替换命中的子串，保留其余内容
3. replace_message: 命中高风险模式（如系统提示提取、角色切换注入）时整条替换
4. block: 命中明确的恶意内容（如 shell 命令注入、远程代码执行）时拒绝发送
5. 替换后的内容传递给 LLM（不是原始内容）
6. 页面展示替换后的实际内容（而非原文），让用户看到 LLM 实际收到了什么
```

**审计日志记录：**

每条消毒命中记录以下字段：

| 字段 | 说明 |
|------|------|
| event_id | 触发消毒的消息 event_id |
| strategy | 采用的策略: replace_token / replace_message / block |
| matched_pattern | 触发规则的摘要（不暴露完整规则） |
| original_content_hash | 原始消息内容哈希 |
| sanitized_content | 替换后的内容（block 策略下为空） |

**服务器端 InputSanitizer 是唯一的安全屏障。** 客户端侧的预检只是 UX 优化（参见 §14.8）。即使客户端提示并放行，服务器端仍然完整执行检测。

**可配置的预检查略 `client_side_prompt_check`：**

| 策略 | 说明 | 安全含义 |
|------|------|---------|
| `warn_only`（默认） | 客户端仅提示，不阻止发送 | 安全完全依赖服务端，推荐 |
| `block` | 客户端尝试阻止发送 | 不可靠（客户端可绕过），不推荐作为安全策略 |

### 9.2 输出安全

LLM 回复在返回给用户前必须经过 OutputValidator：

1. Secret 泄漏检测（私钥、Token 等）
2. 系统提示泄漏检测
3. Tool 注入检测
4. 违规时拦截回复并触发审计

**失效策略：fail-closed**

OutputValidator 自身可能因崩溃或超时而失效。文档定义其为 **fail-closed** 组件：

```
失效策略:
  - 默认：fail-closed（安全优先）
    - Validator 不可用（崩溃/超时/异常）→ 所有回复被阻止
    - 页面显示"安全检查组件异常，请联系管理员"
    - 不允许 LLM 回复绕过 Validator 直接到达用户

  - 超时阈值
    - 单次验证超时: 2 秒
    - 超过后视为验证失败（fail-closed），而非无限等待
    - 超时事件记录到审计日志，severity: critical

  - 故障告警
    - Validator 每次 fail-closed 触发审计告警（Alert severity: critical）
    - 运维应立即介入（Validator 不可用意味着输出安全零防护）

  - 健康检查
    - 提供 /health/validator 端点
    - 用于 Deployment 的 readiness probe
    - 健康检查失败 → Pod 不应接收流量
```

### 9.3 Tool 权限

Tool Calling 模式下，LLM 可能调用任何注册的 Tool。权限控制：

| 层级 | 控制点 | 说明 |
|------|--------|------|
| Agent 级别 | tool_sandbox_config | 全局白名单/黑名单 |
| 会话级别 | session_tool_acl | 当前会话可用的 Tool 列表 |
| 用户级别 | user_tool_permissions | 用户特定的 Tool 授权 |

默认策略：LLM 只能调用在白名单中的 Tool。Agent 管理员配置白名单，用户不能自行授权。

### 9.4 API Key 管理

LLM Provider 的 API Key 通过 SecretResolver 管理：

- 配置中使用 `${OPENAI_API_KEY}` 占位符
- SecretResolver 在 Phase 0.5 解析
- 支持多后端：环境变量 / 1Password / Vault / AWS Secrets Manager
- 支持热轮换（两步提交 + 宽限期）

---

## 10. 对话渠道

LLM 对话可以在以下渠道上运行：

| 渠道 | 集成方式 | 用途 |
|------|---------|------|
| 终端 (CLI) | 读取 stdin / 输出 stdout | 开发测试、快速交互 |
| Tauri 桌面端 | `inject_event` IPC + 回复事件监听 | 桌面 UI 聊天面板 |
| WebSocket | ChatPlatformSource 监听 | Web 端、远程连接 |
| Slack/Discord | 插件事件源 | 团队协作场景 |

每个渠道是一个 ChatPlatformSource 实例，配置不同的 `channel_type`。多渠道共享事件总线，LLM Skill 不需要感知消息来自哪个渠道。

### 10.1 断线重连与会话恢复（P1）

WebSocket 渠道需要处理断线重连场景。Agent 重启或网络中断期间的事件和状态恢复策略如下：

**重连恢复协议：**

```
客户端重连流程:
  1. WebSocket 通道重建后，客户端不会等待 Event Bus 事件
  2. 客户端向会话状态 API 发起请求：GET /session/{id}/state
  3. 服务端返回当前会话完整状态（见下方响应格式）
  4. 客户端用服务端返回的最新状态覆盖本地缓存
  5. Event Bus 从客户端重连时刻开始正常推送增量事件
```

**`GET /session/{id}/state` 响应格式：**

```json
{
  "session_id": "session-xxx",
  "status": "IDLE",
  "state_version": 42,
  "workflow_state": "IDLE",
  "history": [
    { "role": "user", "content": "今天天气怎么样？", "message_id": "m1", "timestamp": "..." },
    { "role": "assistant", "content": "北京 22℃...", "message_id": "m2", "timestamp": "..." }
  ],
  "last_llm_response": {
    "trace_id": "trace_abc123",
    "status": "completed",
    "content": "北京 22℃...",
    "model": "gpt-4",
    "usage": { "prompt_tokens": 120, "completion_tokens": 45 }
  },
  "queue_depth": 0,
  "created_at": "...",
  "updated_at": "..."
}
```

**读取一致性保障：**

- State Store 的 session 记录写入必须是**原子操作**（一次性写所有关联字段），确保读不会看到写一半的数据
- 如果 State Store 支持 MVCC，`GET /session/{id}/state` 应读取最新**已提交**版本
- 响应中的 `state_version` 字段用于客户端校验：
  - 客户端缓存的 `state_version` 每次更新时递增
  - 当客户端收到增量事件后，校验 `after_state_version != client_state_version` → 如果不等，说明状态有变化，需重新拉取全量状态

**断线期间的消息处理：**

| 场景 | 恢复策略 |
|------|---------|
| 断线期间 LLM 回复已完成并写入 State Store | 页面通过 GET /session/{id}/state 拉取到历史，无需 Event Bus 事件 |
| 断线期间用户发送了一条消息但未收到回复 | 如果 State Store 中没有该消息的回复记录，页面标记为"可能丢失"，提示用户重发 |
| Event Bus 重启丢失未消费事件 | 所有入站事件应通过 ChatPlatformSource 的 WAL 持久化，重启后从 WAL 重放 |
| Agent 重启过程中 WebSocket 断开 | 恢复后，页面显示 "Agent 已重启" 系统消息（参见 §14.12） |

**一次性 UI 事件的 WAL 策略：**

以下事件是**一次性 UI 提示事件**，**不进入 WAL**（与 HISTORY_TRIMMED 同类型）：`MESSAGE_DROPPED`（队列溢出丢弃）, `MESSAGE_CANCELLED`（/edit --force 清空队列）。重启后页面通过 `GET /session/{id}/state` 获取以下标记：

- `dropped_message_ids`: 被队列溢出丢弃的消息 ID 列表
- `cancelled_message_ids`: 被 /edit --force 清空取消的消息 ID 列表

**WAL 持久化策略：**

- 所有 ChatPlatformSource 产生的 `MESSAGE_RECEIVED` 事件在进入 Event Bus 前先写入 WAL，**包含 `event_id`**
- Agent 重启后，从 WAL 恢复未消费的事件并重新注入 Event Bus，**携带重放标记 `replay: true`**
- WAL 保留策略：事件消费确认后删除，或 TTL 24 小时自动清理

WAL 重放去重（关键）:

WAL 保证"消息不丢"，但可能"重复投递"。去重防护通过以下机制实现：

```
去重机制:
  1. 每一条 MESSAGE_RECEIVED 携带全局唯一 event_id（UUID v7，§3）
  2. LLM Skill 在处理入口做幂等检查：
     - 记录已处理的 event_id 到 State Store 的 processed_events 集合（TTL 7 天）
     - 如果 event_id 已存在 → 跳过处理，不产生回复
  3. WAL 重放时事件携带 replay: true 标记
     - 下游组件（Tool Runner）根据 replay 标记决定是否重新执行副作用操作
     - 幂等 Tool 可正常执行；非幂等 Tool 应检查后跳过
  4. 二阶段提交（可选）：
     - WAL → Event Bus 分发 → 消费后标记 WAL 为已 ACK
     - 重启时只重放未 ACK 的事件，减少重复窗口
  5. WAL 重放前的会话状态检查：
     - 重放前检查目标会话状态（从 State Store 或 Workflow 实例）
     - 如果会话已 CLOSED 或不存在 → 跳过该事件的 WAL 重放（ad-hoc 会话等）
     - 防止 WAL 重放突破状态机边界约束（§6 中 CLOSED 是终态）
```

### 10.2 CLI 渠道的防御策略（P2）

CLI 渠道与 WebSocket 渠道不同，没有断线重连协议。以下策略定义了 CLI 渠道的防御边界：

**信号处理：**

| 信号 | 行为 |
|------|------|
| SIGPIPE | 优雅关闭 stdout 写入流但不终止进程（使用信号掩码或 `write` 替代 `printf`） |
| SIGTERM | 在终止前将当前交互单元写入 State Store 作为 checkpoint |
| stdin EOF | ChatPlatformSource 检测到 stdin 关闭后，弹出提示并等待 30 秒，如果无新 stdin 输入则优雅退出 |

**服务等级声明：**

- CLI 渠道是 **best-effort 交付**（尽力而为），不保证断线恢复和会话持久化
- 与 WebSocket 渠道的 RPO/RTO 不同：
  - WebSocket：支持断线重连、WAL 重放、会话恢复
  - CLI：不提供重连协议，重启后无法恢复之前的 CLI 会话
- 可选增强：CLI 渠道分配持久 session_id（基于终端名称或 PID 哈希），支持 State Store 恢复

---

## 11. 数据流全景

```
用户输入 (任意渠道)
     │
     ▼
ChatPlatformSource (事件源)
  └─ 创建 MESSAGE_RECEIVED 事件
     ├─ trust_level: untrusted
     └─ 发布到 Event Bus
           │
           ▼
     InputSanitizer (可选, 在 Dispatcher 路由阶段)
       └─ 检测注入模式 → 标记或阻断
           │
           ▼
     Dispatcher 路由到 LLM Skill
           │
           ▼
     LLM Skill 处理
       ├─ 从 SOUL 读取 to_system_prompt()
       ├─ 从 State Store 加载会话历史
       ├─ 调用 LLM Provider Tool
       │    └─ SecretResolver 注入 API Key
       │         │
       │         ▼
       │    LLM API (OpenAI / Anthropic / 本地)
       │         │
       │         ▼
       │    LLM 返回 (文本 / tool_call)
       │
       ├─ [如果返回 tool_call]
       │    └─ 调用对应 Tool → 结果追加到上下文 → 重新调用 LLM
       │
       ├─ OutputValidator 检查回复
       ├─ 更新 State Store 中的会话历史
       └─ 发布回复事件 / 直接输出
            │
            ▼
      用户收到回复
```

---

## 12. 与现有系统的关系

| 现有组件 | 在 LLM 对话中的角色 | 是否需要修改 |
|----------|-------------------|------------|
| Event Bus | 承载 `MESSAGE_RECEIVED` / `LLM_REPLY` 事件 | 否 |
| Dispatcher | 将消息事件路由到 LLM Skill | 否 |
| Skill 系统 | LLM Skill 作为普通 Skill 注册 | 否 |
| Tool 系统 | LLM Provider 作为 Tool 存在 | 新增: LLM Provider Tool |
| Tool Runner | 执行 Tool Calling 中的工具调用 | 否 |
| SOUL | 提供 system prompt | 否 |
| SecretResolver | 管理 LLM API Key | 否 |
| InputSanitizer | 消毒用户输入 | 否 |
| OutputValidator | 检查 LLM 输出 | 否 |
| Workflow | 管理会话状态机 | 新增: chat-session 定义 |
| State Store | 持久化会话历史 | 否 |
| Plugin 系统 | ChatPlatformSource 可作为插件提供 | 否 |
| AuditLogger | 记录注入/泄漏事件 | 否 |
| ChatPlatformSource | 新事件源 | 新增 |
| LLM Provider Tool | 新 Tool 类型 | 新增 |
| LLM Skill | 新 Skill 类型 | 新增 |

---

## 13. 非功能需求

| 需求 | 说明 |
|------|------|
| 并发会话数 | 单个 Agent 实例支持 ≥100 个并发对话会话 |
| 响应延迟 | LLM 首 token 延迟由 Provider API 决定，框架不应增加超过 50ms 额外开销 |
| 会话持久化 | 会话历史通过 State Store 持久化，Agent 重启后自动恢复活跃会话 |
| 断线重连 | WebSocket 渠道支持断线重连，恢复未完成的 LLM 响应 |
| 消息顺序 | 同源保序：同一用户的 MESSAGE_RECEIVED 按到达顺序处理 |
| 限流 | 每用户 10 条/分钟，超过返回速率限制错误 |
| Token 用量统计 | 每次 LLM 调用记录 token 用量，支持按用户/会话/时间段聚合 |
| 审计 | 所有 LLM 调用和 Tool Calling 操作记录审计日志 |

---

## 14. 聊��页面的业务逻辑设计

> 本章节新增。以下描述的是聊天页面作为"事件终端"的业务语义——不涉及具体 UI 框架或渲染技术。

### 14.1 页面定位：事件终端而非传统聊天框

传统聊天页面是一个"输入框 + 消息列表"的被动显示组件。Aman 中的聊天页面是一个**双向事件终端**：

```
事件终端（Chat Page）的角色:
┌─────────────────────────────────────┐
│  作为 事件源 (Event Source)          │
│    └─ 用户输入 → MESSAGE_RECEIVED   │
│    └─ 文件拖入 → FILE_ATTACHED      │
│    └─ 会话操作 → SESSION_CMD        │
│                                     │
│  作为 事件查看器 (Event Viewer)       │
│    └─ LLM_REPLY_READY  → 显示回复   │
│    └─ LLM_TOOL_CALL    → 显示工具调用│
│    └─ LLM_TOOL_RESULT  → 显示工具结果│
│    └─ SYSTEM_EVENT     → 显示系统消息│
│    └─ STREAM_CHUNK     → 流式显示   │
│    └─ STREAM_DONE      → 流式完成   │
└─────────────────────────────────────┘
```

关键含义：

1. 页面不"等待回复"——它**订阅事件流**。LLM 回复到达之前，其他事件（系统事件、其他渠道的消息）都可以在页面中显示。
2. 页面不假设"用户发一条，Agent 回一条"——多事件可以并行发生（如流式分块、多个工具调用交错）。
3. 页面不假设所有事件都来自 LLM——工具执行事件、系统通知、安全告警都可以出现在事件流中。

### 14.2 消息单元的业务语义

每条消息在页面中不只是一个文本块。业务上，消息应区分以下**类型**和**状态**：

**消息类型（决定显示方式）：**

| 消息类型 | 业务含义 | 显示要求 |
|---------|---------|---------|
| `user_text` | 用户输入的自然语言文本 | 左对齐，渠道标识可见 |
| `user_attachment` | 用户上传的文件/图片/代码 | 内嵌缩略图/文件信息，可预览 |
| `user_command` | 用户对 Agent 的操作指令（非对话） | 特殊标识（如 / 前缀） |
| `assistant_text` | LLM 生成的自然语言回复 | 右对齐，显示使用的模型 |
| `assistant_streaming` | LLM 回复正在流式生成中 | 实时逐字更新，光标闪烁 |
| `assistant_tool_call` | LLM 决定调用一个工具 | 折叠面板或内嵌卡片显示调用详情 |
| `assistant_tool_result` | 工具执行返回的结果 | 关联到对应的 tool_call，可展开 |
| `system_event` | 系统消息（会话状态变更、错误、告警） | 居中浅色，不影响对话流 |
| `security_alert` | 输入消毒命中 / 输出验证拦截 | 显着告警色，可审计追踪 |
| `channel_bridge` | 来自其他渠道的消息（如 Slack→桌面端同步） | 渠道标签标识来源 |

**消息状态（生命周期）：**

```
每条消息在整个过程中经历这些状态：

pending          → 用户已发送，事件已进入 Event Bus
processing       → LLM Skill 正在处理（可显示等待时间）
streaming        → LLM 回复正在流式生成中
completed        → LLM 回复已完成（含最终内容）
tool_calling     → 正在执行工具调用（可显示进度）
error            → 处理过程中发生错误
interrupted      → 被用户中断或超时终止
filtered         → 被 InputSanitizer 阻断/标记
blocked          → 被 OutputValidator 拦截
```

业务含义：页面需要根据消息状态实时更新表现，而不是"发送→等待→收到"三段式。

### 14.3 会话模型

传统聊天页面只有一个"活跃会话"。Aman 中的会话模型更丰富：

| 会话类型 | 说明 | 页面表现 |
|---------|------|---------|
| 单次对话 (ad-hoc) | 一次性问答，无状态保存 | 临时标签，关闭即丢弃 |
| 持久会话 (persistent) | 多轮上下文保持，可恢复 | 持久标签，断线重连自动恢复 |
|| 共享会话 (shared) | 多渠道共享同一个 session_id，多个用户/渠道同时写入 | 消息来源显示渠道标签；写入使用乐观锁 |
|| 共享子历史 (shared-sub) | 每个用户/渠道有自己的子历史，只在 UI 层融合 | 消息来源显示渠道+用户标签，底层存储无竞态 |
| 分支会话 (branch) | 基于某个消息点创建的分支 | 可视化的分支树或标签页分组 |
| 角色会话 (role-play) | 挂载特定 SOUL | 显示当前 SOUL 名称/模式 |

**会话切换的表现：**

```
用户操作:
  /session new               → 创建新会话标签
  /session list              → 列出所有活跃会话
  /session switch <id>       → 切换到指定会话（历史自动加载）
  /session close             → 关闭当前会话
  /session share <channel>   → 将会话共享到其他渠道

Agent 发起:
  会话超时 → TIMEOUT 事件 → 页面显示"会话已超时"
  新消息在其他渠道到达 → 当前会话标签闪烁通知
```

**共享会话的并发写入策略：**

共享会话（shared）中多个用户/渠道同时写入时，使用乐观锁防止竞态：

```
乐观锁机制:
  - State Store 中每条会话记录包含 version 字段（单调递增整数）
  - 写入操作必须携带预期 version
  - 写入时 version 匹配 → 写入成功，version +1
  - 写入时 version 不匹配 → 写入失败（HTTP 409 Conflict），LLM Skill 重试（重新加载最新版本）
  - 重试策略：最多 3 次，指数退避

乐观锁 3 次重试耗尽后的兜底策略:
  - **自动降级**：将当前写入请求降级为 shared-sub 模式
    （以 `session_id:{channel}:{user}` 独立存储，UI 层按时间戳融合）
  - **失败通知**：如果自动降级不可行（shared 模式无法动态切换到 shared-sub），
    向页面返回明确的错误消息"会话写入冲突，请稍后重试"，消息不丢失
  - **可观测性**：增加 `session_lock_contention_count` 指标，
    当频繁冲突时，运维可决策是否将 session 永久切换为 shared-sub 配置

降级方案（shared-sub）:
  - 如果乐观锁冲突频繁，设计为 shared-sub 模式
  - 每个渠道/用户的子历史以 `session_id:{channel}:{user}` 为 key 独立存储
  - 只在 UI 层面按时间戳全局排序融合显示
  - 完全消除写入竞态，代价是 LLM 上下文需要额外的交叉渠道历史合并逻辑
```

这两种策略在会话创建时通过 `session_type: "shared" | "shared-sub"` 配置，可动态切换。

### 14.4 流式输出的业务语义

流式输出不是简单的"逐字打字"。业务上需要分级处理：

**流式事件序列：**

```
LLM Provider Tool 输出的事件流:

1. LLM_STREAM_START     ← 第一个 token 到达
   payload:
     - session_id
     - model: "gpt-4"
     - timestamp

2. LLM_STREAM_CHUNK     ← 每 N 个 token 或每 50ms 发送
   payload:
     - delta: "..."               // 生成的文本片段
     - accumulated: "..."         // 到目前为止的全部文本
     - finish_reason: null        // null (仍在生成中) 或 "tool_calls"
     - position_hint: "text" | "before_tool" | "after_tool"
       // position_hint: 指导 UI 渲染位置
       //   "text" (默认)       → 正常追加到自然文本区域
       //   "before_tool"      → 此 chunk 后紧跟 tool_call，渲染时应在其后保留 tool_call 卡片插入点
       //   "after_tool"       → 此 chunk 在 tool_call 执行之后生成（需等 tool_result 返回后渲染）

3. [可选] LLM_TOOL_CALL  ← LLM 决定调用工具
   payload:
     - tool_name: "get_weather"
     - arguments: { "city": "北京" }
     - tool_call_id: "call_xxx"

4. [可选] LLM_TOOL_RESULT ← 工具执行返回
   payload:
     - tool_call_id: "call_xxx"
     - tool_name: "get_weather"
     - result: { "temperature": 22, "condition": "晴" }
   (LLM 继续生成回复 → 回到步骤 2)

5. LLM_STREAM_DONE      ← 生成完毕
   payload:
     - full_content: "...完整的回复..."
     - finish_reason: "stop" | "length" | "tool_calls"
     - usage: { prompt_tokens, completion_tokens, total_tokens }
```

**页面业务规则：**

1. **渲染优先级**：自然语言文本 > 工具调用进度 > 技术细节
2. **流式光标**：正在实时生成的回复末尾显示闪烁光标
3. **工具调用期间的静默**：当 LLM 在等待工具返回值时，流式输出暂停，页面应显示工具执行状态而非空白
4. **边生成边补全**：已经渲染过的 chunk 不应该二次变动（不可变渲染区域）
5. **中断处理**：用户发出"停止"事件时，LLM_STREAM_DONE 不会到达，页面应收起光标并标记"已中断"

**Tool Call 插入时机规则：**

根据 `position_hint` 和 `LLM_TOOL_CALL` 在流中的位置，UI 渲染遵循以下规则：

| 场景 | position_hint | 渲染行为 |
|------|---------------|---------|
| 首个 token 就是 tool_call（无前置文本） | — | tool_call 卡片在文本之前显示，然后显示工具执行状态，最后显示结果文本 |
| tool_call 在文本流中间 | `before_tool` | 已渲染的上半段文本 → tool_call 卡片（可折叠） → 等待 tool_result → 下半段文本 |
| tool_call 在文本流结束后（finish_reason: tool_calls） | — | tool_call 卡片在完整的文本块之后显示，不打断已完成的文本块 |
| tool_call 后有额外文本 | `after_tool` | 这些 chunk 缓存在 tool_call 卡片之后，等 tool_result 返回后一并渲染 |

### 14.5 工具调用的可视化语义

工具调用不是"黑盒"——业务上需要向用户展示 Agent 的"思考过程"：

**折叠/展开层次：**

```
助理回复的第一层:
  "我现在来帮你查北京的天气..."

┌── ▼ [已调用: get_weather] ───────────────┐
│  参数: { city: "北京" }                   │
│  状态: [● 执行中...] 或 [✓ 已完成 120ms]  │
│  结果:                                    │
│    { temperature: 22, condition: "晴" }  │
└──────────────────────────────────────────┘

  "北京现在 22℃，晴天，适合出门。"

助理回复的末尾:
  [提示] 本次使用了 2 个工具 · 共消耗 1,234 tokens
```

**工具调用的业务状态机：**

```
pending    → 已被 LLM 请求，等待执行
executing  → Tool Runner 正在执行
succeeded  → 执行成功，结果可用
failed     → 执行失败（可显示错误信息，LLM 可能继续也可能终止）
```

业务提示：
- 工具调用卡片默认折叠（信任用户的 Agent 自动处理），但用户可以展开查看细节
- 工具执行失败时自动展开
- 敏感信息（密码、API Key）不应在工具调用的结果展示中明文出现

### 14.6 多渠道消息聚合

聊天页面可以同时展示来自多个渠道的消息流。这是 Aman 事件驱动架构的独特能力。

**渠道标签语义：**

```
[桌面] 用户: 今天天气怎么样？
[Slack] 用户: 帮我查一下邮件
[桌面] 助理: 北京现在 22℃，晴天。
[Discord] 用户: 设置一个 5 分钟的定时器
```

当多渠道活跃时：

1. **同一用户**不同渠道的消息按时间戳全局排序
2. **不同用户**在共享会话中混合显示时，渠道标签 + 用户标签双重区分
3. **跨渠道回复**：如果 agent 在 Slack 上收到消息，桌面端页面可以同步显示正在进行的回复
4. **渠道过滤**：用户可以选择只看某个渠道的消息（设计应支持该过滤模式但不默认启用）

### 14.7 SOUL 感知层

聊天页面需要让用户感知到当前 Agent 的 SOUL 配置。业务上这是人机交互的"身份认知"基础。

**SOUL 信息的展示时机：**

| 时机 | 展示内容 | 交互 |
|------|---------|------|
| 会话开始时 | "你正在与 {name} 对话" 系统消息 | 可点击查看更多详情 |
| 会话中持续 | SOUL 名称/版本轻量标识（页面角落的小标签） | 悬停显示当前 SOUL 的简要身份描述 |
| SOUL 热更新后 | "Agent 的身份已更新" 系统消息 | 可展开查看变更摘要 |
| 多角色切换 | "当前使用 SOUL: assistant" 等 | 下拉选择可用 SOUL |

**SOUL 边界对用户的可见性：**

```
页面应该在会话启动时提示 Agent 的边界（隐含）：

简化版（默认）:
  "你可以问我关于编程、设计、写作、数据分析的问题。
   我无法执行：修改文件系统、运行未授权的脚本。"

完整版（用户点击展开）:
  完整 SOUL context，包括:
  - 可用工具列表白名单
  - 不可访问的路径/域名
  - 响应风格设置
  - 语言偏好
```

### 14.8 输入区域的业务语义

输入区域不仅是文本框。在 Aman 的事件终端视角下，它应支持：

**输入模式：**

| 模式 | 触发 | 行为 |
|------|------|------|
| 文本消息 | 默认 | 产生 MESSAGE_RECEIVED 事件 |
| 命令 | 以 `/` 开头 | 产生 SESSION_CMD 事件，不经过 LLM |
| 文件附加 | 拖入/粘贴/选择 | 产生 FILE_ATTACHED 事件 + 文件元数据 |
| 代码块 | 输入检测到代码块 | 可选语言标注 |
| 多行模式 | Shift+Enter | 不断行发送，保持输入上下文可见 |

**发送前的业务处理：**

```
用户输入 → 输入区域检测:
   ├─ 是否超过 max_message_length → 给出提示，不发送
   ├─ 是否包含疑似注入内容 → 提示用户"此消息可能包含敏感内容"（仅提示，不阻止发送）
   ├─ 是否在速率限制内 → 否，显示"请稍后再发送"
   └─ 正常 → 构建 MESSAGE_RECEIVED 事件 → 发布到 Event Bus
```

**安全角色声明（关键）：**

客户端侧的注入检测是**UX 优化**（帮助用户减少误触），**不是安全屏障**。原因：

1. 客户端可被绕过（开发者工具、直接 API 调用），不能承担安全责任
2. 服务器端 InputSanitizer（参见 §9.1）是**唯一且必须的**安全屏障
3. 客户端提示不阻止发送 → 消息到达服务器后，InputSanitizer 仍然完整执行

可配置策略 `client_side_prompt_check`：

| 策略 | 行为 | 说明 |
|------|------|------|
| `warn_only`（默认） | 仅提示，不阻止发送 | UX 优化，安全依赖服务端 |
| `block` | 阻止发送，但页面明示这是客户端策略 | 不可靠，不推荐作为安全手段 |

业务规则：
- 输入框不应该"等待上一条回复完成"——在 Aman 的事件模型下，用户可以发送新消息（即使上一条正在处理）
- 但应清晰指示当前 LLM 是否正在处理，让用户有意识的选择（而非强制等待）
- 如果要保证严格的对话顺序，是 LLM Skill 或 Workflow 的策略选择，不是页面的约束

### 14.9 消息历史的业务存储

消息历史在页面层面需要支持以下业务能力：

| 能力 | 业务说明 | 边界条件 |
|------|---------|---------|
| 无限滚动 | 历史消息按需加载 | 初始加载最近 50 条，滚动触发加载更多 |
| 上下文裁剪 | 当历史超出 LLM context window 时（token-based 检测，由 LLM Skill 在调用 Provider 前决定） | 页面通过 `HISTORY_TRIMMED` 事件感知裁剪决定，灰化已归档消息并标注"已归档" |
| 已归档消息（新） | 被裁剪出 LLM 上下文的早期消息 | 仍然保留在 State Store 中（仅影响 LLM 上下文），用户可展开查看完整内容；页面显示灰化 + "已归档"标签 |
| 历史搜索 | 在消息历史中搜索关键字 | 支持用户/日期/消息类型过滤 |
| 导出 | 将会话导出为 Markdown/JSON | 包含工具调用细节（敏感信息脱敏） |
| 书签 | 标记会话中的重要回复 | 书签名 + timestamp |
| 置顶消息 | 将某条消息固定在会话顶部（如当前任务描述） | 会持续出现在系统提示上下文中 |

**HISTORY_TRIMMED 事件定义：**

当 LLM Skill 在调用 Provider 前裁剪会话历史时，发布此事件到 Event Bus，页面监听后更新 UI：

```json
{
  "event": "HISTORY_TRIMMED",
  "payload": {
    "session_id": "session-xxx",
    "trim_strategy": "token_based",
    "trimmed_from": 1,
    "trimmed_to": 25,
    "trimmed_count": 25,
    "remaining_count": 50,
    "trimmed_tokens": 12000,
    "message_ids_archived": ["m1", "m2", ..., "m25"]
  }
}
```

**HISTORY_TRIMMED 不进入 WAL：** 此事件是一次性的 UI 提示事件，不持久化到 WAL（参见 §10.1）。重启后页面通过 `GET /session/{id}/state` 获取完整历史（未裁剪状态），LLM Skill 在下次调用时重新决定裁剪。在 `GET /session/{id}/state` 响应中可包含可选的 `trim_info` 字段，告知页面哪些消息在当前 LLM 上下文中不可见。

**裁剪策略语义：**

| 属性 | 值 |
|------|-----|
| 触发条件 | LLM 上下文总 token 数 > context_window × 0.8（80% 阈值，可配置） |
| 裁剪单位 | 从最早的 `{role: user}` 消息开始，每次移除 2 条（user + assistant 为一对） |
| 保留最低数量 | 至少保留最近 5 条消息（防止完全丢失上下文） |
| 架构原则 | 裁剪只在 LLM 上下文层面生效，State Store 中的完整历史不受影响 |
| 用户操作 | 用户可通过展开"已归档"区域查看被裁剪的完整内容 |

**路由交互策略：**

如果使用 cost_aware 路由策略（§5），裁剪与路由的顺序关系为：

```
cost_aware 下的上下文窗口策略:
  - 路由决策在上下文加载之后、裁剪之前执行
  - 处理流程: ①加载历史 → ②预估 token 用量 → ③选择模型 → ④以选定模型 context window 为基准裁剪 → ⑤调用 Provider
  - 或保守策略（推荐）: 以所有候选模型中最小的 context window 为基准裁剪，确保无论路由到哪个模型都不会溢出

路由作用域:
  - per-interaction（推荐）: 整个 Tool Calling 循环使用同一模型
  - per-call（可选）: 每次 LLM 调用独立路由（需要更保守的上下文窗口策略）
```

### 14.10 可观测性面板

聊天页面可以配合一个"调试视图"——这是 Aman 事件终端区别于传统聊天框的关键差异。

**调试视图展示内容（可插拔，默认折叠）：**

```
┌── [调试面板] ───────────────────────────┐
│  Event Bus 状态:                         │
│    当前排队: 23 事件                      │
│    背压等级: L1 (正常)                    │
│  ──────────────────────────────────────   │
│  当前 LLM 请求:                          │
│    模型: gpt-4 · Temperature: 0.7       │
│    已消耗: 1,234 / 4,096 tokens         │
│    耗时: 2.3s                           │
│  ──────────────────────────────────────   │
│  事件日志:                              │
│    12:00:01  MESSAGE_RECEIVED  [desktop] │
│    12:00:02  LLM_STREAM_START  [gpt-4]  │
│    12:00:03  LLM_TOOL_CALL    [get_wthr]│
│    12:00:04  TOOL_EXECUTING   [get_wthr]│
│    12:00:04  TOOL_RESULT      [get_wthr]│
│    12:00:05  LLM_STREAM_DONE           │
└──────────────────────────────────────────┘
```

业务规则：
- 调试视图默认折叠，不对普通用户展示
- 开发者模式下自动展开
- 事件日志可以导出为 JSON（保留完整 trace_id 链）
- 调试面板不刷新页面主内容——它是**元信息**层

### 14.11 会话级操作

用户可以在聊天页面执行以下会话级操作（作为事件发布）：

```
/session new [--soul <name>]      → 创建新会话（可指定 SOUL）
/session close                     → 关闭当前会话
/session switch <id>               → 切换会话
/session rename <name>             → 重命名会话

/model switch <model_name>         → 切换当前会话使用的 LLM 模型
/provider switch <provider_name>   → 切换 Provider（openai/anthropic/local）

/stop                              → 中断当前正在生成的回复
/retry                             → 重新生成上一次回复
/edit <message_id>                 → 编辑上一条用户消息，重新提交

/soul switch <name>                → 切换 SOUL（Agent 身份）
/soul show                         → 显示当前 SOUL 摘要

/debug on|off                      → 切换调试面板
/export [format]                   → 导出会话

/help                              → 显示可用命令列表
```

**业务规则：**

- 命令以 `/` 开头，产生 `SESSION_CMD` 事件（不经过 LLM）
- 命令不占用 LLM context window
- 命令响应在页面内显示为系统消息（浅色居中）
- 未知命令返回 "未知命令，输入 /help 查看可用命令列表"

**SESSION_CMD 的队列行为：**

SESSION_CMD 按以下三类进入会话级等待队列（§4）：

| 分类 | 包含命令 | 队列行为 |
|------|---------|---------|
| **非 LLM 命令** | `/session list`, `/session rename`, `/session switch`, `/help`, `/debug`, `/export`, `/soul show` | **跳过队列**，立即执行 |
| **LLM 依赖命令** | `/retry`, `/edit`, `/session new`, `/model switch`, `/provider switch`, `/soul switch`, `/retry --full` | **进入队列**，与 MESSAGE_RECEIVED 共用 FIFO 顺序 |
| **中断命令** | `/stop`, `/session close` | **特殊处理**：无需排队，直接注入当前状态的对应信号（cancel 或 close 协议） |

队列中 SESSION_CMD 和 MESSAGE_RECEIVED 共用 FIFO，所有操作按到达顺序处理。

**/session close 安全关闭协议：**

`/session close` 在 PROCESSING 态执行时，必须执行优雅关闭协议防止孤儿 Tool 调用：

```
关闭协议步骤:
  1. 发送关闭信号 → 检查当前是否有 in-flight LLM 调用或 Tool 执行
  2. 如果有 in-flight LLM 流式回复 → 发出 cancel 信号（同 /stop 行为，500ms 缓存窗口，参见 §14.11 /stop）
  3. 如果有正在执行的 Tool → 发出取消信号（如果 Tool 支持 cancel）
  4. 等待：若 Tool 不支持取消，最多等待 close_timeout（默认 5 秒，可配置）让当前 Tool 完成
  5. 标记未完成的 Tool 结果为 session_closed（不写入历史，但记录审计日志）
  6. Workflow 进入 CLOSED 终态（`PROCESSING → CLOSED` 事件: SESSION_CLOSE_CMD）
  7. 页面收到确认"会话已关闭"
```

关闭期间到达的消息 → 直接丢弃（因为会话即将销毁）。

**/edit 的精确语义（替换模式）：**

```
触发条件:
  - 当前会话状态为 IDLE（上一条回复已完成）
  - 当前会话级队列为空（没有 pending 的 MESSAGE_RECEIVED 或 LLM 依赖型 SESSION_CMD）
  - message_id 必须属于当前会话的用户消息

行为:
  1. 定位到指定 message_id
  2. 从该 message_id 之后，**删除所有后续消息**（包括用户消息和助理回复）
  3. 将编辑后的消息替换原消息（保留原始 message_id，更新 content 和时间戳）
  4. 发布 MESSAGE_EDITED 事件（payload 包含 original_content、new_content）
  5. LLM Skill 收到 MESSAGE_EDITED 后，基于编辑后的上下文重新处理

如果队列非空:
  - 页面提示"当前会话有未处理的消息，请等待完成后编辑"
  - 或在 /edit 时附加 --force 标志，清空会话级等待队列
  - 清空队列时，对每条被丢弃的消息发布 MESSAGE_CANCELLED 事件通知页面

页面反馈:
  - 显示"编辑将清除后续所有回复"确认对话框
  - 编辑完成后，被删除的历史消息从页面移除
  - 成功消息显示"消息已编辑，正在重新生成回复..."

分支模式 (/edit --branch):
  - 用户可附加 `--branch` 标志，在当前消息点创建一个分支会话
  - 原始会话保持不变，分支会话继承编辑点的上下文
  - 审计日志中记录 edit_mode: "replace" | "branch"
```

**审计日志：**

所有 /edit 操作记录以下字段：

| 字段 | 说明 |
|------|------|
| action | `message_edit` |
| edit_mode | `replace` \| `branch` |
| original_message_id | 被编辑的消息 ID |
| edited_message_id | 编辑后的消息 ID（replace 模式与 original 相同） |
| original_content_hash | 原消息内容哈希 |
| new_content_hash | 编辑后消息内容哈希 |
| removed_messages | replace 模式下被删除的后续消息 ID 列表 |
| timestamp | 编辑时间 |
| user | 操作用户 |

**/retry 的精确语义：**

```
默认行为（方式① — 仅重试文本生成）:
  - 保留完整的 Tool Calling 链（原 tool_call + tool_result 不变）
  - 只重新调用 LLM 生成最终文本回复
  - 新的回复携带 retry_of: <original_trace_id>
  - 最快、副作用最小

可选行为（方式② — 完整重放）:
  - 用户可附加 `--full` 标志：/retry --full
  - 重新执行完整流程：消息→首次LLM调用→tool_call→Tool→最终LLM
  - 需要在相关 Tool 的注册声明中标记 `idempotent: true`（默认为 false）
  - 运行时检查：如果重放路径涉及的任何 Tool 的 `idempotent == false`，
    返回错误"无法执行完整重放：包含非幂等 Tool，建议使用默认重试模式"
  - 审计日志记录 retry_of: <original_trace_id>

Tool 幂等性声明（在 ToolDescriptor 中）:
  ```rust
  struct ToolDescriptor {
      name: String,
      idempotent: bool,           // 默认为 false（非幂等）
      description: String,
      parameters: Vec<Parameter>,
      // ...
  }
  ```
  - 非幂等 Tool（如 send_email、create_order）不可出现在 --full 重放路径中
  - 幂等 Tool（如 get_weather、get_sales_data 只读查询）可安全重放
  - 该声明由 Tool 开发者在注册时提供，系统运行时强制检查

不可用场景:
  - 当前没有可重试的回复（没有历史或历史为空）→ 返回提示
  - 会话处于 ERROR 态 → 自动进入 RETRYING 态
  - 会话已 CLOSED → 返回提示
```

**/stop 的精确语义：**

```
触发条件:
  - 会话处于 PROCESSING 态（LLM 正在流式或 Tool 正在执行）

流式输出中的 /stop:
  - 立即终止 LLM 调用（发送 cancel 信号到 Provider API）
  - 标记当前 LLM 回复为 interrupted
  - 已经渲染的文本 chunk 保留在页面上
  - 已经写入历史的 tool_call 和 tool_result 保留

Tool 执行中的 /stop:
  - 向 Tool Runner 发出取消信号（如果 Tool 支持 cancel）
  - 标记 tool_call 为 cancelled
  - 如果 Tool 不支持取消：等待执行完成，但将结果标记为 discarded 且不交付给 LLM
  - 不记录 Tool 结果为历史（避免副作用使历史不一致）

/stop 与 LLM_STREAM_DONE 的竞态仲裁：
  1. 当 /stop 信号发出时，为当前流式请求设置 500ms 的"缓存窗口"
  2. 如果在 500ms 内收到 LLM_STREAM_DONE → 视为正常完成，标记为 completed
  3. 如果超过 500ms 未收到 LLM_STREAM_DONE → 视为已中断，标记为 interrupted
  4. 仲裁窗口防御了"用户点击停止的瞬间 LLM 刚好完成"的边界情况

/stop 后的会话状态:
  - Workflow 从 PROCESSING → IDLE
  - 中断的消息保留在历史中，内容标记为 interrupted
  - 用户可继续发送新消息
  - 如果存在等待队列中的消息，下一条自动开始处理（参见 §4 并发策略）
```

**页面行为约束：**

- /stop 按钮在 PROCESSING 态显示，IDLE/ERROR 态隐藏
- /retry 按钮仅在 IDLE 或 ERROR 态显示（有完整的上一次回复可重试时）
- /edit 按钮仅在用户消息上可见，且在 IDLE 态可用

### 14.12 错误处理与异常状态

聊天页面需要处理以下异常状态的业务逻辑：

| 异常场景 | 页面行为 | 用户可执行的操作 |
|---------|---------|----------------|
| LLM Provider 超时 | 显示 "LLM 响应超时" 系统消息，标记当前回复为 error | /retry 或 /stop |
| LLM Provider 不可用 | 显示 "当前 Provider 不可用，请检查 API Key 或网络连接" | 切换 Provider 或重试 |
| Token 配额耗尽 | 显示 "已达到 Token 配额限制" | 等待配额恢复或切换模型 |
| InputSanitizer 命中 | 显示 "消息被安全策略过滤"（不展示具体过滤规则） | 修改消息后重新发送 |
| OutputValidator 拦截 | 显示 "回复被安全策略拦截"（不展示具体内容） | 联系管理员 |
| 工具调用失败 | tool_call 卡片状态变为 failed，显示错误信息 | 手动重试或跳过 |
| 会话超时 | 显示 "会话已超时" 系统消息，建议新建会话 | /session new |
| 并发权限冲突 | 如果另一个操作正在影响同一会话 | 提示冲突来源，等待 |
| Agent 重启 | 显示 "Agent 已重启" 系统消息，尝试恢复会话 | 等待恢复完成 |

### 14.13 消息关联与锚定

在消息密集的对话中，需要建立消息之间的关联关系。这对于 Aman 的事件溯源模型特别重要：

**关联类型：**

```
用户消息 "查北京天气"
    │ 关联: 触发
    ▼
助理回复 "北京 22℃"
    │ 关联: 使用了工具
    ├── get_weather(city="北京")
    │   └─ 关联: 返回结果
    │       └─ { temperature: 22, condition: "晴" }
    │
用户回复 "那上海呢？"
    │ 关联: 引用上下文（隐含")
    ▼
助理回复 "上海 18℃，多云"
```

**页面中的表现：**

1. **引用线**：用户点击某条消息时，系统可高亮显示其"触发链"上的关联消息
2. **父子关系**：工具调用的结果自动关联到对应的 tool_call，视觉上缩进或连线
3. **分支锚点**：用户可以在某条消息上右键新建分支会话（创建一个新 session，但该消息作为上下文种子）
4. **消息锚点链接**：`/session switch <id>?anchor=<msg_id>` 切换到指定会话并定位到具体消息

### 14.14 多页面/多标签的业务语义

桌面端的聊天页面可能同时打开多个会话标签。业务上：

| 标签状态 | 含义 | 页面表现 |
|---------|------|---------|
| 活跃 (active) | 用户当前正在交互 | 高亮显示 |
| 等待回复 (waiting) | 用户已发送消息，Agent 正在处理 | 标签上显示旋转指示器 |
| 空闲 (idle) | 会话打开但无活动 | 正常显示 |
| 收到新消息 (notified) | 后台会话收到新回复（用户在其他标签） | 标签上显示小红点/计数器 |
| 已超时 (timeout) | 会话空闲超时 | 标签灰显，提示用户恢复或关闭 |
| 错误 (error) | 会话处理过程中出错 | 标签红色标记 |

**标签数量约束：**

```
正常使用: 1-5 个标签
大量使用: 5-15 个标签（建议显示滚动标签栏）
过度使用: 15+ 标签（建议标签分组或关闭提示）
```

### 14.15 输入与输出的语义完整性

每一条从用户到 LLM 再到用户的完整链路，都应可作为**可引用单元**：

```
一个完整交互单元 (Interaction Unit):

┌─ 输入栏 ─────────────────────────────┐
│  message_id: "msg_u_001"              │
│  用户: "查北京天气"                    │
│  时间: 12:00:01                       │
│  渠道: desktop                        │
├───────────────────────────────────────┤
│  trace_id: "trace_abc123"            │
├───────────────────────────────────────┤
│  输出链:                               │
│  ├─ tool_call: get_weather(city:北)   │
│  ├─ tool_result: {temp:22}            │
│  └─ final: "北京现在22℃"              │
│  model: gpt-4 · tokens: 234          │
│  耗时: 2.3s                           │
└───────────────────────────────────────┘
```

业务含义：
- 用户点击"复制回复"时可以选择"仅最终文本"或"完整交互（含工具调用）"
- 导出 JSON 格式时保留完整的 trace_id 链
- 审计日志中的每条记录都应可以关联到具体的交互单元

**Trace Chain（追踪链）：**

/retry、/edit 等操作会替换交互单元的回复内容，产生新的 trace_id。追踪链建立了新旧 trace_id 之间的可追溯关系：

```
创建规则:
  - 初始交互:        M1 → A1 (trace_id: T1, prev_trace_id: null)
  - /edit (replace): M1 → A1' (trace_id: T2, prev_trace_id: T1)
  - /retry:          M1 → A1'' (trace_id: T3, prev_trace_id: T1)
  - /edit --branch:  M1 → A1''' (trace_id: T4, prev_trace_id: T1, branch_from: true)
```

**审计日志扩展字段：**

| 字段 | 说明 | 可为 null |
|------|------|-----------|
| trace_id | 当前交互单元的追踪 ID | 否 |
| trace_prev | 上一个版本的 trace_id（/edit 或 /retry 前） | 是 |
| trace_branch_from | 分支来源的 trace_id（/edit --branch 创建的分支） | 是 |

**查询能力：**

- 审计日志支持 `trace_chain` 展开：给定任意 trace_id，能递归定位到所有关联的追踪（prev 和 branch）
- token 用量按 trace_chain 聚合：同一交互单元的所有版本（T1+T2+T3）统一计入该交互单元

---

## 15. 总结

LLM 聊天页面在 Aman 框架中的角色定位与传统聊天 UI 有本质区别：

| 维度 | 传统 Chat UI | Aman 事件终端 |
|------|-------------|--------------|
| 输入模型 | 发送-等待-接收 三段式 | 持续事件流，可异步交错 |
| 消息模型 | user ↔ assistant 一对一 | 多类型事件：文本、工具、系统、安全 |
| 回复方式 | 完整回复后显示 | 流式逐步显示，工具调用可视化 |
| 状态管理 | 本地内存 + 服务器轮询 | Event Bus 自然管理，Workflow 持久化 |
| 多渠道 | 通常单渠道 | 多渠道聚合，同源显示 |
| 可观测性 | 日志在后端 | 调试面板在页面侧边 |
| SOUL 感知 | 用户不知道 system prompt | 用户可感知 Agent 身份/边界 |
| 断线恢复 | 通常丢失上下文 | State Store 恢复 + Workflow 重建 |
