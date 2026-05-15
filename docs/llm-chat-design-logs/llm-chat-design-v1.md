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
| 共享会话 (shared) | 多渠道共享同一个 session_id | 消息来源显示渠道标签 |
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
     - delta: "...生成的文本片段..."
     - accumulated: "到目前为止的全部文本"
     - finish_reason: null (仍在生成中)

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
   ├─ 是否包含疑似注入内容 → 提示用户"此消息可能包含敏感内容"
   ├─ 是否在速率限制内 → 否，显示"请稍后再发送"
   └─ 正常 → 构建 MESSAGE_RECEIVED 事件 → 发布到 Event Bus
```

业务规则：
- 输入框不应该"等待上一条回复完成"——在 Aman 的事件模型下，用户可以发送新消息（即使上一条正在处理）
- 但应清晰指示当前 LLM 是否正在处理，让用户有意识的选择（而非强制等待）
- 如果要保证严格的对话顺序，是 LLM Skill 或 Workflow 的策略选择，不是页面的约束

### 14.9 消息历史的业务存储

消息历史在页面层面需要支持以下业务能力：

| 能力 | 业务说明 | 边界条件 |
|------|---------|---------|
| 无限滚动 | 历史消息按需加载 | 初始加载最近 50 条，滚动触发加载更多 |
| 上下文裁剪 | 当历史超出 LLM context window 时 | 页面显示裁剪提示 + 最早的 N 条已归档 |
| 历史搜索 | 在消息历史中搜索关键字 | 支持用户/日期/消息类型过滤 |
| 导出 | 将会话导出为 Markdown/JSON | 包含工具调用细节（敏感信息脱敏） |
| 书签 | 标记会话中的重要回复 | 书签名 + timestamp |
| 置顶消息 | 将某条消息固定在会话顶部（如当前任务描述） | 会持续出现在系统提示上下文中 |

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

业务规则：
- 命令以 `/` 开头，产生 `SESSION_CMD` 事件（不经过 LLM）
- 命令不占用 LLM context window
- 命令响应在页面内显示为系统消息（浅色居中）
- 未知命令返回 "未知命令，输入 /help 查看可用命令列表"

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
