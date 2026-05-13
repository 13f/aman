# LLM 对话能力设计

> 基于 Aman 事件响应式架构的 LLM 对话需求说明。不涉及具体语言或框架，聚焦业务语义与集成方式。

---

## 1. 设计原则

Aman 的核心公理是 **"万物皆事件，响应即行为"**。LLM 对话遵循同样的原则：

1. **用户的聊天消息是一个事件**——由 ChatPlatform 事件源产生 `MESSAGE_RECEIVED` 事件，进入统一事件总线。没有"主循环等待用户输入"，只有"事件到达后响应"。
2. **LLM 的回复是事件处理的结果**——LLM 对话不是一个持续的会话循环，而是一次事件的触发-处理-输出链路。
3. **对话历史是持久化状态**——由 Workflow 或 State Store 管理，不是进程内的内存变量。

这与传统 Chatbot 框架的根本区别：传统框架以 Chat 循环为中心，Aman 以统一事件循环为中心，LLM 对话只是众多事件源中的一种。

---

## 2. 架构定位

```
                    ┌──────────────┐
                    │  Chat 用户    │
                    └──────┬───────┘
                           │ 输入消息
                    ┌──────▼───────┐
                    │ ChatPlatform  │  ← 事件源: 产生 MESSAGE_RECEIVED
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

---

## 3. 事件源: ChatPlatformSource

ChatPlatform Source 是一个 Push 模式的事件源，负责将用户输入转换为框架内部事件。

**产生的唯一事件类型：** `MESSAGE_RECEIVED`

**事件 Payload 结构：**

```
{
  "channel":   string      // 对话渠道标识 (终端/WebSocket/桌面端/Slack/Discord)
  "user":      string      // 用户标识
  "message":   string      // 用户输入的文本
  "session":   string      // 会话标识 (多轮对话关联)
  "timestamp": timestamp   // 消息到达时间
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

一个 Agent 实例可以配置多个 LLM Provider。路由策略：

| 策略 | 行为 |
|------|------|
| primary_fallback | 先调用主 Provider，失败时切换到备用 |
| cost_aware | 根据请求复杂度选择: 简单请求用低成本模型，复杂请求用高成本模型 |
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
  - ACTIVE       (初始态)   ← 用户来消息时处于此状态
  - WAITING_INPUT           ← 等待用户的下一条消息 (非必须，取决于是否保持连接)
  - PROCESSING              ← LLM 正在生成回复
  - IDLE                    ← 等待用户新输入
  - TIMEOUT                 ← 会话超时
  - CLOSED       (终态)    ← 会话结束

转移:
  ACTIVE → PROCESSING   (事件: MESSAGE_RECEIVED)
  PROCESSING → IDLE     (事件: LLM_REPLY_READY)
  IDLE → PROCESSING     (事件: MESSAGE_RECEIVED)
  IDLE → TIMEOUT        (事件: SESSION_TIMEOUT)
  TIMEOUT → CLOSED      (事件: SESSION_END)
  IDLE → CLOSED         (事件: SESSION_END)
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

---

## 9. 安全

### 9.1 输入安全

用户消息（`MESSAGE_RECEIVED`）来自 `untrusted` 事件源，在传递给 LLM 前必须经过 InputSanitizer：

1. 注入模式检测（忽略历史指令、系统提示提取、shell 注入等）
2. 匹配的模式替换为 `[redacted]`
3. 触发审计日志记录

### 9.2 输出安全

LLM 回复在返回给用户前必须经过 OutputValidator：

1. Secret 泄漏检测（私钥、Token 等）
2. 系统提示泄漏检测
3. Tool 注入检测
4. 违规时拦截回复并触发审计

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
