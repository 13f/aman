# LLM Chat 页面架构设计

> 基于 Aman 事件响应式架构 + 插件系统的聊天页面可视化架构方案。
> 本文件为架构决策记录，面向架构师评审。涵盖后端运行时、桥接层、前端激活、安全边界、多渠道恢复。
> 前端 UI 业务语义（消息类型、工具卡片折叠、调试面板等）在 `llm-chat-design.md §14` 中详述。

---

## 1. 问题域

### 1.1 背景

Aman 框架已完成 Tauri 桌面应用基础（M12），现有页面包括：Dashboard、Skill Editor、Workflow Board、SOUL Editor、Plugin Manager、DLQ、Event Viewer。下一步需要支持 **LLM 对话能力**，核心问题在于：

- 聊天页面（Chat Page）的可视化部分如何与 Aman 的插件系统对接？
- 聊天能力是内置在 Tauri 应用中，还是通过插件机制动态加载？
- 插件系统是否需要扩展以支持 UI 相关的声明能力？

### 1.2 核心矛盾

| 角度 | 直觉倾向 | 问题 |
|------|---------|------|
| 插件化最大化 | 聊天页做成插件，动态加载 | Aman 插件系统是后端能力注册，不支持前端组件；安全性、一致性难以保证 |
| 静态内置 | 聊天页编译在 Tauri 应用内 | 耦合：没有聊天插件时页面不可用但代码存在；无法热插拔 |
| 折中 | 拆为两层 | 需要定义清晰的桥接契约 |

---

## 2. 设计决策

### 决策 1：分两层架构（采纳）

**将聊天能力拆为两层，职责分离：**

```
┌──────────────────────────────────────────────────────┐
│  Layer 1: 后端能力插件 (Aman Plugin)                   │
│                                                       │
│  放在: crates/plugins/ (或独立仓库)                    │
│  注册: EventSource + Skill + Tool                     │
│  具体:                                                 │
│    - chat-source      → ChatPlatformSource 事件源     │
│    - llm-skill        → LLM Skill 消费事件            │
│    - llm-provider-*   → LLM Provider Tool 实例        │
│  隔离: 子进程/WASM，与其他插件解耦                      │
├──────────────────────────────────────────────────────┤
│  Layer 2: 前端可视化页面 (Tauri 静态编译)               │
│                                                       │
│  放在: crates/tauri/src/pages/Chat.svelte             │
│  不动态加载: 页面代码编译在 Tauri 二进制中               │
│  条件激活: 页面显示/隐藏由运行时状态驱动                 │
│  通信: IPC (invoke + listen) ←→ AgentRuntime           │
└──────────────────────────────────────────────────────┘
```

**决策依据：**

| 依据 | 详细 |
|------|------|
| 安全 | 前端组件无法进入 Aman 的三种插件隔离模式（进程内/子进程/WASM）。UI 插件需要第四种沙箱，引入攻击面 |
| 一致性 | 每个插件的 UI 风格不同会导致体验碎片化。Aman 的消费者视角期望统一的事件终端 |
| 编译约束 | Svelte 是编译时框架。动态加载 .svelte 需要运行时编译器，增加几百 KB 开销 |
| 改动范围 | 后端插件加载不影响前端编译。聊天页的布局调整只需修改 Tauri 代码，不涉及插件重载 |
| 热插拔 | 后端插件可以在运行时加载/卸载，前端通过事件响应 |

### 决策 2：插件声明 UI 能力（采纳 — 最小扩展）

**在现有 `plugin.yaml` 中增加可选字段：**

```yaml
name: chat-source
version: "1.0.0"
mode: in_process

# 已有的后端能力声明（不变）
event_sources:
  - ChatPlatformSource
skills:
  - LLMSkill
tools:
  - LLMProviderTool

# 新增的可选字段
capabilities:
  - chat                  # 声明提供聊天能力
                          # → 运行时注册此能力
                          # → 前端检测到后可激活聊天页

  - session_management    # 声明提供会话管理
                          # → 前端激活会话切换/列表功能

  - soul_aware            # 声明感知 SOUL 身份
                          # → 前端显示 SOUL 标识区域

# 更细粒度的 UI 映射（可选）
ui:
  pages:
    - chat                 # → 前端激活 /chat 路由
  events:
    - message_stream       # 此插件产生实时消息流事件
    - session_events       # 此插件产生会话状态事件
```

**向后兼容性：**
- 已有插件无此字段 → 不声明任何能力，不影响现有行为
- `capabilities` 为空数组 → 等同无声明
- 新增字段不影响插件加载器、WAL、State Store 等任何现有机制

### 决策 3：运行时能力注册 + 事件广播（采纳）

**能力注册流程（Phase 2 扩展）：**

```
Phase 2 [组件注册] — 现有流程不变，新增:
  1. 加载所有插件（拓扑序 + 环检测）— 不变
  2. 注册 Skill / Tool / EventSource — 不变
  3. [新增] 收集所有插件的 capabilities 集合
     → 聚合为全局能力列表: ["chat", "session_management", ...]
     → 存储在 AgentRuntime 的 capability_registry 中
     → 对 Tauri IPC 暴露 get_capabilities() 端点
  4. [新增] 如果 Phase 2 完成后 capabilities 有变化（对比上次启动）
     → 发布 CAPABILITY_REGISTRY_UPDATED 事件
     → Payload: { available: ["chat", ...], removed: [], added: ["chat", ...] }

Phase 4 [源激活] — 不变
Phase 5 [就绪] — 不变
```

**运行时变更监听（Phase 5 后的插件热加载/卸载）：**

```
插件热加载:
  1. Runtime 检测到插件文件变更
  2. 执行热加载流程（原子替换）
  3. 更新 capability_registry
  4. 发布 CAPABILITY_AVAILABLE 事件
     Payload: { capability: "chat", plugin: "chat-source", version: "1.0.0" }

插件热卸载:
  1. Runtime 检测到插件文件删除
  2. 执行卸载流程（反向拓扑序 + on_unload）
  3. 从 capability_registry 移除
  4. [可选前置] 发布 SESSION_CLOSE 事件（当前活跃会话的清理）
  5. 发布 CAPABILITY_REMOVED 事件
     Payload: { capability: "chat", plugin: "chat-source" }

插件崩溃:
  1. Plugin 进程/WASM 异常退出
  2. Runtime 触发 recovery
  3. 如果 recovery 失败 → 标记为 ERROR 状态
  4. 发布 CAPABILITY_DEGRADED 事件
     Payload: { capability: "chat", plugin: "chat-source", reason: "plugin_crashed" }
  5. 前端显示:"聊天功能暂时不可用" + 重试入口
```

---

## 3. 会话状态机

LLM 对话天然是多轮的，需要在多个 `MESSAGE_RECEIVED` 事件之间保持状态。**采用 Workflow 状态机**（非轻量缓存）作为主方案，以 session 为粒度管理生命周期。

### 3.1 Workflow 定义

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
```

### 3.2 状态转移

```
ACTIVE    → PROCESSING   (事件: MESSAGE_RECEIVED)
ACTIVE    → TIMEOUT      (事件: SESSION_TIMEOUT)    ← 创建后从未发消息
PROCESSING → IDLE        (事件: LLM_REPLY_READY | LLM_STREAM_DONE)
PROCESSING → ERROR       (事件: LLM_ERROR           ← Provider 异常 / 超时 / Token 耗尽)
PROCESSING → TIMEOUT     (事件: STREAM_TIMEOUT      ← LLM 流式超时，无响应超过阈值)
PROCESSING → CLOSED      (事件: SESSION_CLOSE_CMD   ← 用户 /session close)
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
```

### 3.3 状态转移约束

- **PROCESSING 态不应允许新的 MESSAGE_RECEIVED** 进入当前会话——由会话级等待队列处理（参见 §4）
- **ERROR 态中的 /retry 最多连续失败 5 次**，超过后强制进入 CLOSED
- **RETRYING → PROCESSING 时**，上一次交互的 trace_id 应传递给新调用
- **所有终态 (CLOSED) 必须有一条补偿路径**：不能仅依赖用户主动操作
- 超时后（TIMEOUT）用户发送新消息可恢复 → TIMEOUT → IDLE

### 3.4 Workflow 数据存储

每个 Workflow 实例的 `data` 字段存储会话上下文：

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

### 3.5 生命周期 Phase 映射

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
| 5→4 | ChatPlatformSource 停止监听；拒绝新连接 | 已有连接的 WebSocket 等待当前消息处理完成（500ms 缓存窗口） |
| 4→3 | LLM Skill 取消注册 | 不再接收新的 MESSAGE_RECEIVED 路由 |
| 3→2 | WAL Flush：确保所有已处理事件被确认 | 未 ACK 的事件在下次启动时通过 WAL 重放 |
| 2→1 | 关闭 Event Bus | 停止事件分发 |

**时序约束：**
- WAL 恢复（Phase 2）必须在 LLM Skill 注册（Phase 3）**之前**完成
- ChatPlatformSource（Phase 4）必须在 WAL 恢复之后启动
- 关闭时，LLM Skill 取消注册后，ChatPlatformSource 拒绝的新连接应返回"服务正在关闭"提示

---

## 4. 并发与队列模型

多条 `MESSAGE_RECEIVED` 事件可能连续到达同一会话。为防止状态竞态，定义如下并发模型。

### 4.1 核心规则

```
并发规则:
  1. 同一会话的消息 → 串行处理
     当 LLM Skill 正在处理 session-A 的第一条消息时（含 Tool Calling 循环），
     session-A 的第二条 MESSAGE_RECEIVED 进入会话级等待队列。

  2. 不同会话的消息 → 并行处理
     session-A 和 session-B 的消息各自独立处理，无互斥。

  3. 等待队列容量
     默认队列深度: 10 条/会话 (queue_depth_per_session)。

  4. 队列溢出策略 (queue_overflow_strategy)
     - "drop"（默认）: 丢弃新消息，发布 MESSAGE_DROPPED 事件到页面
     - "preempt_oldest": 丢弃队列中最旧未处理消息

  5. 队列中的消息生命周期
     会话超时关闭 → 队列清空，消息丢弃（发布 MESSAGE_CANCELLED）。
     用户 /stop 中断当前处理 → 队列下一条消息自动开始处理。

  6. Dispatcher 路由约束：session 级分片
     Dispatcher 在路由阶段对 session_id 做 consistent hashing 分片，
     同一 session_id 始终路由到同一个 Worker（或 actor 模型）。

  7. 跨渠道消息顺序仲裁
     当同一条会话的消息从不同渠道（如 Slack + WebSocket）到达时，
     client_ts（用户操作本地时间）和 server_ts（Event Bus 到达时间）联合仲裁：
     - client_ts 差值 > 5 秒 → 以 client_ts 为准
     - client_ts 差值 ≤ 5 秒 → 以 server_ts 为准
```

### 4.2 背压两级协调

Event Bus 背压与会话级队列互不知晓可能导致错误映射：

```
协调规则:
  - Event Bus 背压 = 基础设施级（保护进程不 OOM）
    当 Event Bus 拒绝事件（L3+ 背压）→ ChatPlatformSource 返回 HTTP 503 / WebSocket 5000

  - 会话级队列满 = 业务级（保护单会话不被淹没）
    返回 HTTP 429 / WebSocket 4290

  - 错误映射:
    503 → "系统繁忙，请稍后重试"
    429 → "当前对话消息过多，请等待处理完成"
```

### 4.3 队列等待超时检测

如果队列中等待超过 60 秒没有开始处理（`queue_wait_stall_threshold`），发布 `QUEUE_STALLED` 事件到页面（可能 LLM Skill 或 Tool Runner 挂了）。

### 4.4 页面反馈

```
MESSAGE_ENQUEUED 事件 payload:
{
  "session_id": "session-xxx",
  "queue_position": 2,
  "queue_position_hint": "前面还有 1 条消息"
  // position 1    → "当前正在处理你的消息"
  // position 2-3  → "前面还有 {N-1} 条消息"
  // position 4+   → "队列中有多条消息等待处理"
}
```

### 4.5 限流模型

**算法选择：Sliding Window Log（滑动窗口日志）**

选择依据：
| 算法 | 优点 | 缺点 | 本方案选择 |
|------|------|------|-----------|
| Token Bucket | 实现简单，支持突发 | 不均匀的令牌消耗模式；重启后状态丢失 | — |
| Fixed Window | 边界容易处理 | 窗口边界突刺问题 | — |
| Sliding Window Log | 精确，无突刺 | 内存成本 O(窗口内请求数) | **采纳**（会话数 ≤100 时内存可接受） |

**三个维度：**

```
维度 1: 用户级 → 10 条/分钟（默认）
  限制单个用户在所有会话上的消息发送速率
  作用域: ChatPlatformSource
  违反: HTTP 429 / WebSocket 4290 + payload { retry_after_seconds: 6 }

维度 2: 会话级 → 3 条/5 秒（固定）
  限制单个会话的并发注入速率，防止 UI 高频连点掏空队列
  作用域: LLM Skill（队列入口处检查）
  违反: MESSAGE_DROPPED 事件 + "消息发送过快" 提示

维度 3: 全局级 → 100 条/分钟（可配置）
  限制所有会话的总吞吐，与并发目标（§16）对齐
  作用域: Event Bus 入口
  违反: HTTP 503 / WebSocket 5000
```

**Sliding Window Log 实现要点：**

```python
# 每个维度的限流器结构
class SlidingWindowRateLimiter:
    window_size: Duration        # 窗口大小（如 60s）
    max_requests: int            # 窗口内最大请求数
    logs: Vec<Instant>           # 窗口内的请求时间戳（有序）

    fn allow() -> bool:
        now = Instant::now()
        # 清除窗口外的时间戳
        self.logs.retain(|t| now - t < self.window_size)
        if self.logs.len() < self.max_requests:
            self.logs.push(now)
            return true
        return false
```

**前端 429 处理：**

```
- 用户级限流命中 → 前端显示 "发送过快了，请稍后再试 (N 秒)"
  → 禁用输入框 N 秒（retry_after 值）
  → N 秒后恢复输入框
- 会话级限流命中 → 消息已入队列但被丢弃 → 提示 "消息已丢弃，请等待当前处理完成"
- 全局级限流命中 → 显示 "系统繁忙，请稍后重试"
  → 禁用输入框直到收到 MESSAGE_ENQUEUED 确认
```

**限流状态恢复：**
- 限流状态保存在内存中，不持久化到 State Store
- Agent 重启后所有窗口计数器归零——这是保守的安全策略（重启后允许消息通过，重新开始计数）
- 原因：限流是保护措施，不是权限控制。重启后恢复比丢弃用户消息更重要。

---

## 5. SOUL 集成

SOUL 是 LLM 的 system prompt 来源。LLM Skill 在组合上下文时自动注入 `Soul::to_system_prompt()`。

### 5.1 字段映射

| SOUL 字段 | 在 LLM 上下文中的位置 | 作用 |
|-----------|---------------------|------|
| name | system prompt 首句 | "You are {name}." |
| identity | system prompt | Agent 身份定义 |
| core | system prompt | 核心行为准则 |
| expertise | system prompt | 专长领域声明 |
| boundaries | system prompt + Tool 权限 | 行为边界约束 |
| vibe | system prompt | 语气风格 |
| preferences | system prompt | 偏好设定 |

### 5.2 热更新生效边界（重要）

SOUL 热更新的生效边界限定在**完整的交互单元**（一个用户消息 → 全部 Tool Calling 循环 → 最终回复），不在 Tool Calling 循环中间生效：

```
生效边界规则:
  1. 当 LLM Skill 开始处理一个 MESSAGE_RECEIVED 事件时 → 固定当前 SOUL 版本为快照
  2. 同一个交互单元内的所有 LLM 调用使用同一张快照
  3. 下一个 MESSAGE_RECEIVED 事件开始处理时 → 重新读取最新 SOUL
  4. 热更新在正在进行的交互单元中不可见 → 避免了 system prompt 中途变化导致的：
     - 权限不一致（首次调用有某个 Tool 权限，第二次没有）
     - 身份跳跃（前一段是"程序助手"，后一段是"数据分析师"）
     - boundaries 不一致导致的 Tool 执行失败

  Tool 权限快照绑定:
  5. SOUL 快照中必须包含当前生效的 Tool 权限白名单（§8.3）的副本
     理由: SOUL.boundaries 与 Tool 权限白名单是互锁的——boundaries 约束 Tool 调用行为，
     白名单约束 Tool 可用性。只快照 SOUL 而不快照权限白名单会导致：
     - 交互单元中途白名单被修改 → LLM 看到的权限与实际不匹配
     - Tool 调用因权限跳变而失败
  6. Tool 权限快照在 MESSAGE_RECEIVED 事件处理开始时与 SOUL 一起固定
```

---

## 6. 桥接层设计

### 6.1 Tauri IPC 接口

前端与 AgentRuntime 之间的通信通道：

```
┌─────────────────────────────────────────────┐
│           Tauri Frontend (Svelte)            │
│                                              │
│  invoke("get_capabilities")                  │
│    → 返回 ["chat", "session_management"]     │
│                                              │
│  listen("capability:available", cb)          │
│    → 新能力上线时通知                        │
│  listen("capability:removed", cb)            │
│    → 能力移除时通知                          │
│  listen("capability:degraded", cb)           │
│    → 能力异常时通知                          │
│                                              │
│  — 聊天操作 —                               │
│  invoke("chat:send_message", {text, sess})   │
│    → 发布 MESSAGE_RECEIVED 事件到 Event Bus  │
│  invoke("chat:session_list")                 │
│    → 返回当前活跃会话列表                     │
│  invoke("chat:session_create", {soul?})      │
│    → 创建新会话，返回 session_id              │
│  invoke("chat:session_close", {sid})         │
│    → 关闭指定会话                             │
│  invoke("chat:session_history", {sid,limit}) │
│    → 加载指定会话的历史消息                   │
│  invoke("chat:session_state", {sid})         │
│    → 返回会话完整状态（断线重连用）            │
│  invoke("chat:stop_generation", {sid})       │
│    → 中断当前正在生成的回复                   │
│  invoke("chat:retry_last", {sid})            │
│    → 重新生成上一次回复                       │
│  invoke("chat:edit_message", {sid,mid,text}) │
│    → 编辑指定消息并重新处理                   │
│  invoke("chat:session_state", {sid})         │
│    → 断线重连时的全量状态恢复                 │
│                                              │
│  — 事件监听 —                               │
│  listen("message:received", cb)              │
│    → LLM 回复/工具调用/流式块到达时通知        │
│  listen("session:status", cb)                │
│    → 会话状态变更时通知                       │
└─────────────────────────────────────────────┘
```

**get_capabilities() 返回时机：** 在 Phase 2 完成前返回空数组 `[]`；Phase 2 完成后返回完整聚合列表。前端导航守卫逻辑（§7.1）天然自愈：Phase 2 未完成时 /chat 路由显示"能力尚未就绪"页面；Phase 2 完成后自动激活。

### 6.2 事件契约

前端订阅的后端事件类型：

| 事件类型 | 来源 | 前端响应 |
|---------|------|---------|
| `MESSAGE_RECEIVED` | ChatPlatformSource | 在消息流中新增用户消息条目 |
| `LLM_STREAM_START` | LLM Skill | 开启流式渲染面板，初始化计数器 |
| `LLM_STREAM_CHUNK` | LLM Skill | 追加文本到活动消息的渲染缓冲区 |
| `LLM_STREAM_DONE` | LLM Skill | 关闭流式光标，记录 token 用量 |
| `LLM_TOOL_CALL` | LLM Skill | 在消息流中插入可折叠的工具调用卡片 |
| `LLM_TOOL_RESULT` | LLM Skill | 更新工具调用卡片状态（成功/失败） |
| `OUTPUT_BLOCKED` | OutputValidator | 显示"回复被安全策略拦截"系统消息 |
| `INPUT_SANITIZED` | InputSanitizer | 显示"消息被安全策略过滤"提示 |
| `MESSAGE_ENQUEUED` | LLM Skill | 显示排队位置提示 |
| `QUEUE_STALLED` | LLM Skill | 显示队列等待超时警告 |
| `MESSAGE_DROPPED` | LLM Skill | 显示消息被丢弃提示 |
| `MESSAGE_CANCELLED` | LLM Skill | 显示消息被取消提示 |
| `MESSAGE_EDITED` | LLM Skill | 刷新编辑后的消息列表 |
| `HISTORY_TRIMMED` | LLM Skill | 灰化已归档消息，标注"已归档" |
| `SESSION_TIMEOUT` | Workflow | 标灰当前会话，显示超时提示 |
| `SESSION_CLOSE` | Workflow/Runtime | 关闭标签页，清理状态 |
| `CAPABILITY_AVAILABLE` | Runtime | 激活对应功能的路由/控件 |
| `CAPABILITY_REMOVED` | Runtime | 禁用对应功能，关闭关联会话 |
| `CAPABILITY_DEGRADED` | Runtime | 显示降级提示，提供恢复入口 |
| `SOUL_CHANGED` | SOUL System | 更新 Agent 身份标识显示 |
| `AGENT_RESTARTED` | Runtime | 显示"Agent 已重启"，尝试恢复会话 |

### 6.3 数据流全景（含桥接层）

```
用户输入 (Tauri Desktop)
     │  invoke("chat:send_message")
     ▼
┌──────────────────────────────────────┐
│  Tauri Rust Bridge (commands.rs)     │
│  └─ publish_event(MESSAGE_RECEIVED)  │
└──────────┬───────────────────────────┘
           │  MESSAGE_RECEIVED (trust_level: untrusted)
           ▼
┌──────────────────────────────────────┐
│  Aman AgentRuntime                    │
│                                        │
│  ┌─ Event Bus (背压控制 + 去重)      │
│  │   └─ InputSanitizer (注入检测)    │
│  │        └─ Dispatcher (session 分片) │
│  │             └─ LLM Skill           │
│  │                  ├─ SOUL 快照      │
│  │                  ├─ State Store    │
│  │                  ├─ 会话级等待队列  │
│  │                  └─ Tool Runner    │
│  │                       ├─ LLM API   │
│  │                       └─ 系统 Tool │
│  │                                        │
│  └─ OutputValidator (泄漏检测)       │
│  └─ emit(LLM_STREAM_CHUNK)           │
└──────────┬───────────────────────────┘
           │  LLM_STREAM_CHUNK (Tauri 事件)
           ▼
┌──────────────────────────────────────┐
│  Tauri Rust Bridge (event listener)  │
│  └─ emit_to_window("message:stream") │
└──────────┬───────────────────────────┘
           │  Tauri Event 到前端
           ▼
┌──────────────────────────────────────┐
│  Svelte Chat Page                    │
│  └─ listen("message:stream", cb)     │
│  └─ 更新消息缓冲区 → 渲染            │
└──────────────────────────────────────┘
```

---

## 7. 前端激活逻辑

### 7.1 路由守卫

```
路由表:
  /               → Dashboard（始终可用）
  /soul           → SOUL Editor（始终可用）
  /skill          → Skill Editor（始终可用）
  /workflow       → Workflow Board（始终可用）
  /plugin         → Plugin Manager（始终可用）
  /dlq            → DLQ Viewer（始终可用）
  /event          → Event Viewer（始终可用）
  /chat           → Chat Page（需 chat 能力）
  /chat/:session  → Chat Page（指定会话，需 chat 能力）
  /sessions       → Session List（需 session_management 能力）

导航守卫逻辑:
function onRouteEnter(route) {
    caps = await invoke("get_capabilities")
    if (route in CAPABILITY_REQUIRED_MAP) {
        if (!caps.includes(CAPABILITY_REQUIRED_MAP[route])) {
            return renderPluginMissingPage(required)
        }
    }
    return renderPage(route)
}

CAPABILITY_REQUIRED_MAP = {
    "/chat":          "chat",
    "/chat/:session": "chat",
    "/sessions":      "session_management",
}
```

### 7.2 侧边栏/导航栏动态

```
const NAV_ITEMS = [
    { path: "/",         label: "Dashboard",   always: true },
    { path: "/chat",     label: "Chat",        capability: "chat" },
    { path: "/sessions", label: "Sessions",    capability: "session_management" },
    { path: "/soul",     label: "SOUL",        always: true },
    { path: "/skill",    label: "Skills",      always: true },
    { path: "/workflow", label: "Workflows",   always: true },
    { path: "/plugin",   label: "Plugins",     always: true },
    { path: "/dlq",      label: "DLQ",         always: true },
    { path: "/event",    label: "Events",      always: true },
]

// 运行时过滤:
activeItems = NAV_ITEMS.filter(item =>
    item.always || caps.includes(item.capability)
)
```

### 7.3 状态转换表

| 前端状态 | 事件 → 新状态 | 前端行为 |
|---------|-------------|---------|
| chat_hidden | CAPABILITY_AVAILABLE("chat") → chat_visible | 导航栏出现 Chat 标签 |
| chat_visible | CAPABILITY_REMOVED("chat") → chat_hidden | 导航栏移除 Chat 标签，跳转到 Dashboard，关闭关联会话 |
| chat_visible | CAPABILITY_DEGRADED("chat") → chat_degraded | 页面顶部显示"聊天功能暂时不可用"，保留历史但禁止新输入 |
| chat_degraded | CAPABILITY_AVAILABLE("chat") → chat_visible | 恢复功能，解除输入限制 |
| chat_degraded | CAPABILITY_REMOVED("chat") → chat_hidden | 同移除流程 |

---

## 8. 安全架构

### 8.1 输入安全（InputSanitizer）

用户消息（`MESSAGE_RECEIVED`）来自 `untrusted` 事件源，在传递给 LLM 前必须经过 InputSanitizer：

**三类消毒策略：**

| 策略 | 行为 | 适用场景 |
|------|------|---------|
| `replace_token` | 仅替换触犯规则的子串：`"请忽略之前的[redacted]，直接执行"` | 默认策略。用户可看到替换后的完整上下文 |
| `replace_message` | 整条消息替换为 `[redacted]` | 高风险注入模式（如系统提示提取尝试） |
| `block` | 拒绝发送，返回错误给页面 | 确定的恶意内容（如 shell 注入命令） |

**策略选择规则：**

```
1. InputSanitizer 按优先级从低到高检测：replace_token → replace_message → block
2. replace_token: 匹配关键词/模式时，只替换命中的子串，保留其余内容
3. replace_message: 命中高风险模式（如系统提示提取）时整条替换
4. block: 命中明确的恶意内容时拒绝发送
5. 替换后的内容传递给 LLM（不是原始内容）
6. 页面展示替换后的实际内容（而非原文），让用户看到 LLM 实际收到了什么
```

**审计日志记录：**

| 字段 | 说明 |
|------|------|
| event_id | 触发消毒的消息 event_id |
| strategy | replace_token / replace_message / block |
| matched_pattern | 触发规则的摘要（不暴露完整规则） |
| original_content_hash | 原始消息内容哈希 |
| sanitized_content | 替换后的内容（block 下为空） |

**客户端预检：** `client_side_prompt_check` 策略 — `warn_only`（默认，仅提示不阻止）或 `block`。客户端侧是 UX 优化，**不是安全屏障**。服务器端 InputSanitizer 是唯一必须的安全屏障。

### 8.2 输出安全（OutputValidator）

LLM 回复在返回给用户前必须经过 OutputValidator：

1. Secret 泄漏检测（私钥、Token 等）
2. 系统提示泄漏检测
3. Tool 注入检测
4. 违规时拦截回复并触发审计

**验证粒度：**

```
验证粒度规则:
  - OutputValidator 仅在完整回复（LLM_STREAM_DONE）时执行完整验证
  - 流式中间 chunk 不逐块验证——因为：
    a) 中间 chunk 可能不构成完整的语义单位（被截断在句子中间）
    b) per-chunk 验证增加不可接受的延迟（每 50ms/每 N token 触发一次）
  - LLM_TOOL_CALL 事件不通过 OutputValidator
    （工具调用名称和参数属于架构决策范畴，非内容安全）
  - 完整回复验证通过后，允许所有已发送的 chunk 永久渲染
  - 完整回复验证失败 → 触发 fail_closed → 页面显示 OUTPUT_BLOCKED
```

**失效策略：fail_closed（安全优先）**

```
失效策略:
  - 默认：fail_closed（安全优先）
    - Validator 不可用（崩溃/超时/异常）→ 所有回复被阻止
    - 页面显示"安全检查组件异常，请联系管理员"
    - 不允许 LLM 回复绕过 Validator 直接到达用户

  - 超时阈值
    - 单次验证超时: 2 秒 (OutputValidator.timeout)
    - 超过后视为验证失败（fail_closed），而非无限等待

  - 故障告警
    - Validator 每次 fail_closed 触发审计告警（severity: critical）
    - 运维应立即介入

  - 健康检查
    - 提供 /health/validator 端点
    - 健康检查失败 → Pod 不应接收流量
```

### 8.3 Tool 权限

Tool Calling 模式下，LLM 可能调用任何注册的 Tool。权限分三层：

| 层级 | 控制点 | 说明 |
|------|--------|------|
| Agent 级别 | tool_sandbox_config | 全局白名单/黑名单 |
| 会话级别 | session_tool_acl | 当前会话可用的 Tool 列表 |
| 用户级别 | user_tool_permissions | 用户特定的 Tool 授权 |

默认策略：LLM 只能调用在白名单中的 Tool。Agent 管理员配置白名单，用户不能自行授权。

### 8.4 API Key 管理

- 配置中使用 `${OPENAI_API_KEY}` 占位符
- SecretResolver 在 Phase 0.5 解析
- 支持多后端：环境变量 / 1Password / Vault / AWS Secrets Manager
- 支持热轮换（两步提交 + 宽限期）

### 8.5 前端 IPC 权限控制

| IPC 命令 | 需要能力 | 无能力时的行为 |
|---------|---------|--------------|
| chat:send_message | chat | 返回错误: "Chat capability not available" |
| chat:session_list | chat | 返回空列表（非错误） |
| chat:session_create | chat | 返回错误 |
| chat:session_close | chat | 静默失败 |
| chat:stop_generation | chat | 静默失败 |
| chat:session_history | chat | 返回空历史 |

### 8.6 前端事件信任等级

| 信任等级 | 事件来源 | 前端处理 |
|---------|---------|---------|
| trusted | Runtime 内部事件（CAPABILITY_*、SESSION_* 等） | 直接执行状态转换 |
| untrusted | LLM 输出事件（LLM_STREAM_CHUNK、LLM_TOOL_CALL 等） | 经过 OutputValidator 验证后才显示 |

即使前端直接收到 LLM 回复事件，也不应假设内容是安全的——OutputValidator 在 Runtime 侧已执行，但前端不应绕过此检查。

---

## 9. 多渠道与断线重连

### 9.1 断线重连恢复协议（P1）

WebSocket 渠道需要处理断线重连场景：

```
客户端重连流程:
  1. WebSocket 通道重建后，客户端不会等待 Event Bus 事件
  2. 客户端向会话状态 API 发起请求：GET /session/{id}/state
  3. 服务端返回当前会话完整状态
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
    { "role": "user", "content": "今天天气怎么样？", "message_id": "m1" },
    { "role": "assistant", "content": "北京 22℃...", "message_id": "m2" }
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
- State Store 的 session 记录写入必须是**原子操作**
- 响应中的 `state_version` 字段用于客户端校验增量一致性

### 9.2 WAL 与恢复

**WAL 持久化策略：**
- 所有 `MESSAGE_RECEIVED` 事件在进入 Event Bus 前先写入 WAL
- Agent 重启后，从 WAL 恢复未消费的事件并重新注入 Event Bus，**携带重放标记 `replay: true`**
- WAL 保留策略：事件消费确认后删除，或 TTL 7 天自动清理
- WAL 磁盘配额：`WAL.max_size: 500MB`（默认），超过后可选停止写入或滚动删除
- 可观测性：`wal_disk_usage_percent` 指标，>80% 时告警

**WAL 重放去重：**

```
去重机制:
  1. 每一条 MESSAGE_RECEIVED 携带全局唯一 event_id（UUID v7）
  2. LLM Skill 在处理入口做幂等检查：
     - 记录已处理的 event_id 到 State Store 的 processed_events 集合（TTL 7 天）
  3. WAL 重放时事件携带 replay: true 标记
     - 下游组件（Tool Runner）根据 replay 标记决定是否重新执行副作用操作
  4. 二阶段提交（可选）：
     - WAL → Event Bus → 消费后标记 WAL 为已 ACK
     - 重启时只重放未 ACK 的事件
  5. WAL 重放前的会话状态检查：
     - 重放前检查目标会话状态
     - 如果会话已 CLOSED 或不存在 → 跳过该事件的 WAL 重放
```

### 9.3 CLI 渠道的防御策略（P2）

| 信号 | 行为 |
|------|------|
| SIGPIPE | 优雅关闭 stdout 写入流但不终止进程 |
| SIGTERM | 在终止前将当前交互单元写入 State Store 作为 checkpoint |
| stdin EOF | 检测到 stdin 关闭后，等待 30 秒，无新输入则优雅退出 |

- CLI 渠道是 **best-effort 交付**，不保证断线恢复
- 与 WebSocket 的 RPO/RTO 不同：CLI 不提供重连协议

### 9.4 插件卸载时的数据安全与 Phase 4.5 排水逻辑

```
前端在收到 CAPABILITY_REMOVED 时:
  1. 关闭当前活跃的聊天标签页
  2. 清理前端内存中的消息缓冲区
  3. 不清除 State Store 中的持久化历史
  4. 不清除审计日志

用户重新安装聊天插件后:
  1. 前端检测 CAPABILITY_AVAILABLE("chat")
  2. 通过 invoke("chat:session_list") 获取之前残留的会话
  3. 如果残留会话存在 → 提示用户恢复
  4. 如果不残留 → 正常新建会话
```

**Phase 4.5 排水流程（插件热卸载时的完整时序）：**

```
Phase 4.5 排水流程:
  1. 标记插件为 draining 状态（拒绝新请求）
     - Capability 注册表标记为 DEGRADED
     - 新 MESSAGE_RECEIVED 返回 HTTP 503 + "服务正在关闭"
     - 新队列请求被拒绝

  2. 等待 in-flight 请求完成（timeout: drain_timeout = 30s，独立于 session.close_timeout）
     - 进行中的 LLM API 调用：等待模型返回完整回复或抛出
     - 进行中的 Tool Calling：等待当前 Tool 执行完成
     - 每个会话独立等待，不是全局同步等待

  3. 超时后强制取消未完成的请求
     - LLM API 调用 → cancel()（HTTP 层的 cancel token）
     - in-progress Tool → 发送中断信号
     - 被强制取消的请求记录审计日志: reason: "drain_forced_cancel"

  4. 写入 checkpoint 到 State Store
     - 每个活跃会话的当前状态
     - 已完整处理的回复（即使排水中断）
     - 被强制取消的请求标记为 interrupted

  5. 执行 Phase 4 卸载
     - 从 Event Bus 注销事件源
     - 释放端口（WebSocket 监听端口等）
     - 调用插件 on_unload 回调
```

**drain_timeout 与 session.close_timeout 的关系：**

| 参数 | 默认值 | 用途 | 关系 |
|------|--------|------|------|
| `session.close_timeout` | 5s | 用户主动关闭单个会话时的等待 | 短超时，单个会话粒度 |
| `drain_timeout` | 30s | 插件卸载时所有活跃会话的排水等待 | 长超时，全局粒度 |
| 关系 | — | `drain_timeout` ≥ `close_timeout` × 预估最大活跃会话数 | |

---

## 10. 多插件能力共享语义

```
chat-source (v1.0.0)   → capabilities: [chat]
llm-skill   (v1.0.0)   → capabilities: [chat]
─────────────────────────────────────────
capability_registry 聚合结果:
  ["chat"]  — 同一能力由多个插件共同提供

规则:
  - 同一个 capability 名称可由多个插件同时声明
  - 能力存在性 = 至少一个声明此能力的插件处于 Running 状态
  - 所有声明同一能力的插件都卸载/崩溃 → 能力移除
  - 部分插件崩溃（部分 Running）→ 能力降级（DEGRADED）
    - 如果 chat-source 存活但 llm-skill 崩溃 → 可以接收消息但无法回复
    - 如果 chat-source 崩溃但 llm-skill 存活 → LLM 就绪但无法接收输入

能力健康判定:
  - HEALTHY: 所有声明该能力的插件正常（Running）
  - DEGRADED: 至少一个核心功能维度瘫痪
    - 核心功能维度：接收输入（chat-source）/ 生成回复（llm-skill）/ 工具执行（tool-runner）
    - 任一维度整体不可用 → DEGRADED
    - 单插件的冗余副本（同一功能多实例）中部分崩溃 → 仍为 HEALTHY（有冗余）
  - 降级时长阈值：DEGRADED 持续超过 300 秒（5 分钟）→ 自动转入 REMOVED
    - 防止前端长期处于不确定的降级态
```

---

## 11. 页面业务架构

以下提炼自 `llm-chat-design.md §14` 中关键的**架构级**语义（UI 细节参见原文档）。

### 11.1 事件终端模型

传统聊天页面是"发送→等待→接收"三段式。Aman 聊天页面是一个**双向事件终端**：

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
│    └─ SYSTEM_EVENT     → 显示系统消息│
│    └─ STREAM_CHUNK     → 流式显示   │
└─────────────────────────────────────┘
```

关键含义：
1. 页面不"等待回复"——它**订阅事件流**。多事件可并行交错。
2. 页面不假设"用户发一条，Agent 回一条"——工具调用、系统事件、安全告警都可出现在流中。
3. 页面不假设所有事件都来自同一渠道——多渠道可聚合显示。

### 11.2 消息模型（类型 + 状态）

**消息类型（决定显示方式）：**

| 类型 | 业务含义 |
|------|---------|
| `user_text` | 用户输入的自然语言文本 |
| `user_attachment` | 用户上传的文件/图片/代码 |
| `user_command` | 用户对 Agent 的操作指令（/ 命令） |
| `assistant_text` | LLM 生成的自然语言回复 |
| `assistant_streaming` | LLM 回复正在流式生成中 |
| `assistant_tool_call` | LLM 决定调用一个工具 |
| `assistant_tool_result` | 工具执行返回的结果 |
| `system_event` | 系统消息（会话状态变更、错误、告警） |
| `security_alert` | 输入消毒命中 / 输出验证拦截 |
| `channel_bridge` | 来自其他渠道的消息同步 |

**消息状态（生命周期）：**

```
pending → processing → streaming → completed
                    → error
                    → interrupted
                    → filtered / blocked
```

### 11.3 会话模型

| 会话类型 | 说明 |
|---------|------|
| 单次对话 (ad-hoc) | 一次性问答，无状态保存 |
| 持久会话 (persistent) | 多轮上下文保持，可恢复 |
| 共享会话 (shared) | 多渠道共享同一 session_id，乐观锁写入 |
| 共享子历史 (shared-sub) | 每个渠道/用户独立子历史，UI 层融合 |
| 分支会话 (branch) | 基于某个消息点创建的分支 |
| 角色会话 (role-play) | 挂载特定 SOUL |

**共享会话乐观锁：**

```
- State Store 中每条会话记录包含 version 字段
- 写入操作必须携带预期 version
- version 不匹配 → 写入失败（409 Conflict），LLM Skill 最多重试 3 次（指数退避）
- 3 次耗尽后自动降级为 shared-sub 模式
```

### 11.4 流式输出语义

```
事件序列:
  1. LLM_STREAM_START    — 第一个 token 到达
  2. LLM_STREAM_CHUNK    — 每 N 个 token 或每 50ms 发送
     payload 中的 position_hint:
       "text"          → 正常追加到自然文本区域
       "before_tool"   → 此 chunk 后紧跟 tool_call
       "after_tool"    → 此 chunk 在 tool_call 执行之后生成
  3. LLM_TOOL_CALL      — LLM 决定调用工具
  4. LLM_TOOL_RESULT    — 工具执行返回
  5. LLM_STREAM_DONE    — 生成完毕
```

**流式渲染规则：**
1. 渲染优先级：自然语言 > 工具调用进度 > 技术细节
2. 流式光标：正在生成的回复末尾显示闪烁光标
3. Tool Calling 期间静默：页面显示工具执行状态而非空白
4. 已渲染的 chunk 不可变
5. 中断时收起光标并标记"已中断"

### 11.5 会话级操作命令

```
非 LLM 命令（跳过队列，立即执行）:
  /session list, /session rename, /session switch
  /help, /debug, /export, /soul show

LLM 依赖命令（进入队列，FIFO 顺序）:
  /retry, /edit, /session new
  /model switch, /provider switch, /soul switch

中断命令（特殊处理，无需排队）:
  /stop, /session close
```

**/stop 的 500ms 缓存窗口：**
当 /stop 信号发出时设置 500ms 缓存窗口——如果在 500ms 内收到 LLM_STREAM_DONE，视为正常完成；超过 500ms 标记为 interrupted。防御"用户点击停止的瞬间 LLM 刚好完成"的边界情况。

**/session close 安全关闭协议：**
1. 发送关闭信号 → 检查 in-flight LLM 调用或 Tool 执行
2. 如果有流式回复 → 发出 cancel 信号
3. 如果有正在执行的 Tool → 发出取消信号
4. 等待最多 5 秒（close_timeout）让当前 Tool 完成
5. 标记未完成的 Tool 结果为 session_closed（不写入历史，记录审计日志）
6. Workflow 进入 CLOSED

**/edit 的替换语义：**
定位指定 message_id → 删除该 ID 之后所有后续消息 → 替换原文 → 发布 MESSAGE_EDITED 事件。支持 `--force`（清空队列）和 `--branch`（创建分支会话）。

**/retry 的两种模式：**
- 默认：仅重新生成文本回复，保留已有 Tool Calling 链
- `--full`：完整重放（首次 LLM → tool_call → Tool → 最终 LLM），要求路径中所有 Tool 的 `idempotent: true`

**/model switch 与 /provider switch 在 PROCESSING 态的执行规则：**

```
状态约束:
  - IDLE 态: 立即切换，'$next' 开始生效
  - PROCESSING 态: 切换请求入队，等当前交互单元完成后生效
    → 避免中断正在生成的回复
    → 切换生效时刻：下一个交互单元的 LLM_STREAM_START
    → 生效前页面不显示"切换中"状态——切换是静默的，仅新回复体现变化
  - ERROR 态: 立即切换，可用于绕过失效 provider
  - 切换不影响已有会话数据（history/state）
  - /model switch 与 /provider switch 独立——切换 provider 不自动切换 model
```

### 11.6 交互单元追踪链

每条交互链路（用户消息 → LLM 回复）通过 trace_id 建立可追溯关系：

```
初始交互:        M1 → A1 (trace_id: T1, prev_trace_id: null)
/edit (replace): M1 → A1' (trace_id: T2, prev_trace_id: T1)
/retry:          M1 → A1'' (trace_id: T3, prev_trace_id: T1)
/edit --branch:  M1 → A1''' (trace_id: T4, prev_trace_id: T1, branch_from: true)
```

**审计日志追踪字段：**
- `trace_id`：当前交互单元追踪 ID（不可 null）
- `trace_prev`：上一个版本的 trace_id（/edit 或 /retry 前）
- `trace_branch_from`：分支来源的 trace_id（/edit --branch）

**查询能力：**
- 审计日志支持 `trace_chain` 展开：任意 trace_id 可递归定位到所有关联追踪
- Token 用量按 trace_chain 聚合

### 11.7 错误处理

| 异常场景 | 页面行为 |
|---------|---------|
| LLM Provider 超时 | 显示"LLM 响应超时"，标记回复为 error |
| LLM Provider 不可用 | 显示"当前 Provider 不可用" |
| Token 配额耗尽 | 显示"已达到 Token 配额限制" |
| InputSanitizer 命中 | 显示"消息被安全策略过滤" |
| OutputValidator 拦截 | 显示"回复被安全策略拦截" |
| 工具调用失败 | tool_call 卡片状态变为 failed |
| 会话超时 | 显示"会话已超时" |
| Agent 重启 | 显示"Agent 已重启"，尝试恢复会话 |

### 11.8 上下文窗口计算

**计算方式：**

```
context_window 计算:
  - 每次 LLM 调用前由 LLM Skill 计算
  - 公式: total_tokens = base_prompt(SOUL) + history_tokens + user_message_tokens
  - history_tokens: 从 State Store 读取当前会话历史后，按 tokenizer 估算
  - 与 Provider 的 max_tokens / context_window 对比
```

**触发时机和裁剪策略** 不在本节定义——参见 **§15 历史裁剪策略**。

| 职责分离 | 定义位置 |
|---------|---------|
| 上下文窗口计算 | §11.8（本节）——纯计算，无副作用 |
| 何时触发裁剪 | §15.2——触发条件、检查间隔模式 |
| 如何裁剪（策略） | §15.3-§15.4——FIFO / Weighted、安全余量 |
| 裁剪后恢复 | §15.6——WAL/State Store 一致性、重启后灰化标记 |

---

## 12. 全局配置参数表

以下为散布在全文中的可配置参数统一表。所有枚举值使用 **snake_case** 命名规范。

| 参数名 | 类型 | 默认值 | 所属 | 作用域 | 说明 |
|--------|------|--------|------|--------|------|
| `channel_type` | enum | `terminal` | ChatPlatformSource | Source | 渠道类型: terminal / websocket / tauri_desktop |
| `listen_addr` | string | `127.0.0.1:0` | ChatPlatformSource | Source | WebSocket 监听地址 |
| `session_idle_timeout` | duration | `300s` | Workflow | Session | IDLE/ACTIVE 态进入 TIMEOUT 的空闲超时 |
| `error_auto_close_after` | duration | `600s` | Workflow | Session | ERROR 态自动归入 CLOSED 的超时 |
| `llm_stream_timeout` | duration | `120s` | Workflow | Session | LLM 流式响应的最大静默时间（PROCESSING → TIMEOUT） |
| `max_message_length_chars` | int | `4096` | ChatPlatformSource | Source | 单条消息最大 Unicode 代码点数量 |
| `rate_limit` | int | `10` | ChatPlatformSource | User | 用户消息频率限制（条/分钟） |
| `session_rate_limit` | int | `3` | LLM Skill | Session | 会话级速率限制（条/5秒） |
| `global_rate_limit` | int | `100` | Event Bus | Global | 全局速率限制（条/分钟） |
| `rate_limit_algorithm` | enum | `sliding_window_log` | — | Global | 限流算法: sliding_window_log / token_bucket（扩展） |
| `queue_depth_per_session` | int | `10` | LLM Skill | Session | 会话级等待队列最大深度 |
| `queue_overflow_strategy` | enum | `drop` | LLM Skill | Session | 队列溢出策略: drop / preempt_oldest |
| `client_side_prompt_check` | enum | `warn_only` | InputSanitizer | Global | 客户端预检策略: warn_only / block |
| `OutputValidator.timeout` | duration | `2s` | OutputValidator | Global | 单次输出验证超时 |
| `WAL.max_size` | bytes | `500MB` | WAL | Global | WAL 磁盘配额上限 |
| `WAL.retention_ttl` | duration | `7d` | WAL | Global | WAL 保留 TTL |
| `dedup.retention_ttl` | duration | `7d` | State Store | Global | processed_events 去重集合 TTL |
| `session.close_timeout` | duration | `5s` | Workflow | Session | /session close 等待 in-flight Tool 完成的超时 |
| `drain_timeout` | duration | `30s` | Runtime | Global | 插件卸载时等待 in-flight 请求完成的超时 |
| `trim.threshold_ratio` | float | `0.8` | LLM Skill | Session | 历史裁剪触发阈值（context_window 百分比） |
| `trim.minimum_messages` | int | `5` | LLM Skill | Session | 裁剪后至少保留的消息数（建议 5-20） |
| `trim.unit_pairs` | bool | `true` | LLM Skill | Session | 以 user+assistant 一对为单位裁剪 |
| `trim.strategy` | enum | `fifo` | LLM Skill | Session | 裁剪策略: fifo / weighted（按重要度加权） |
| `trim.check_interval` | enum | `per_call` | LLM Skill | Session | 裁剪检查模式: per_call / per_n_messages |
| `queue.wait_stall_threshold` | duration | `60s` | LLM Skill | Session | 队列等待超时检测阈值 |
| `degraded_auto_remove_after` | duration | `300s` | Runtime | Global | 能力降级后自动转入 REMOVED 的阈值 |
| `otel.endpoint` | string | — | OpenTelemetry | Global | OTLP exporter 端点（见 §14） |
| `otel.service_name` | string | `aman-chat` | OpenTelemetry | Global | OpenTelemetry 服务名 |

命名风格：所有配置键使用 `snake_case`。层级关系使用 `.` 分隔。

---

## 13. 测试架构

> **P0 要求：Phase 2 开工前必须完成测试架构定义。** 状态机 + 并发队列 + WAL 重放的组合如果没有系统测试策略，将成为调试黑洞。

### 13.1 测试层级

```
┌─────────────────────────────────────────────────┐
│  L1: 单元测试（Unit Tests）                       │
│    每个组件独立测试，mock 外部依赖                   │
├─────────────────────────────────────────────────┤
│  L2: 集成测试（Integration Tests）                 │
│    组件间交互，真实 Event Bus + Mock LLM Skill    │
├─────────────────────────────────────────────────┤
│  L3: E2E 测试（End-to-End Tests）                  │
│    全链路：前端 IPC → Runtime → Plugin → WAL      │
└─────────────────────────────────────────────────┘
```

### 13.2 状态机 Property-based Testing

**目标：** 验证 §3.2 状态转移表的**所有合法/非法转移**。

```
测试框架: proptest (Rust) / hypothesis (Python，原型验证)

覆盖范围:
  合法转移: 每一对 (当前状态, 事件) 在转移表中定义的路径
    示例: (ACTIVE, MESSAGE_RECEIVED) → PROCESSING
  ╳ 非法转移: 每一对不在转移表中的 (状态, 事件) 组合
    示例: (PROCESSING, MESSAGE_RECEIVED) → 拒绝（进入等待队列，状态不变）
          验证队列计数 +1 而不是状态跳变

测试用例生成:
  - 随机状态序列（长度 3-20，遍历所有状态）
  - 随机事件序列（合法 + 非法事件交错）
  - 边界序列：连续 ERROR → RETRY 5 次后强制 CLOSED
  - 并发序列：同一会话的消息交错到达（验证队列而非状态跳变）

不变性断言（invariant checks，每个状态后执行）:
  - session_id 在状态转移中不变
  - trace_id 在 PROCESSING → IDLE 前始终存在
  - CLOSED 态不接受任何事件（返回错误）
  - session.close_timeout 内的 Tool 请求被取消而非悬挂
```

### 13.3 并发队列正确性测试

**目标：** 验证 §4.1 核心规则——同一会话串行、不同会话并行。

```
测试场景 1: 单会话多消息
  输入: session-A 的 msg1, msg2, msg3 同时到达
  验证:
    - msg1 处理中时，msg2, msg3 入队（队列深度 3）
    - msg1 完成后 msg2 自动开始处理
    - msg2 完成前 msg3 仍在队列等待
    不允许: msg2 在 msg1 完成前开始处理

测试场景 2: 多会话交错
  输入: session-A.msg1, session-B.msg1 同时到达
  验证:
    - session-A 和 session-B 并行处理（不互斥）
    - 各自的状态机独立运行
    - 各自的队列独立

测试场景 3: 队列溢出
  输入: session-A 的 msg1-15 同时到达（默认队列深度 10）
  验证:
    - msg1-10 入队，msg11-15 被拒绝
    - 发布相应数量的 MESSAGE_DROPPED 事件
    - preempt_oldest 模式下：msg1-10 入队后最先入队的被丢弃

测试场景 4: 队列清空
  输入: session-A 的 msg1, msg2 在队列中，触发 session close
  验证:
    - 队列清空
    - 每条队列中消息发布 MESSAGE_CANCELLED
```

### 13.4 Event Bus + WAL 重放集成测试

**目标：** 验证 §3.5 启动时序和 §9.2 WAL 重放在 Phase 2 前后的正确性。

```
测试夹具:
  - InMemoryEventBus (替换真实 Event Bus 以控制背压)
  - InMemoryWAL (非持久化，但实现完整的 append/replay/ack 接口)
  - FakeLLMSkill (消费 MESSAGE_RECEIVED，模拟固定延迟)

测试场景 1: 正常启动时序
  时序: Phase 0 → Phase 2 (WAL 恢复) → Phase 3 (Skill 注册) → Phase 4 (Source 启动)
  验证:
    - WAL 中未 ACK 的事件被重放到 Event Bus
    - 重放事件携带 replay: true 标记
    - Source 启动后新事件正常分发

测试场景 2: 崩溃恢复
  模拟: Agent 在处理 MESSAGE_RECEIVED 时崩溃（crash mid-flight）
  恢复:
    - 重启后 WAL 重放未 ACK 事件
    - LLM Skill 检查 processed_events 去重集合
    - 已处理的事件被跳过
  验证:
    - 事件被恰好执行一次（at-most-once 副作用，at-least-once WAL 消费）

测试场景 3: 已关闭会话的 WAL 重放跳过
  场景: session-A 已 CLOSED，但其 WAL 条目在重启后重放
  验证:
    - 重放前检查会话状态 → 会话 CLOSED
    - 跳过该事件，不发布，不写入队列
    - 审计日志记录: "replay_skipped: session_closed"
```

### 13.5 前端流式渲染确定性测试

**目标：** 验证 §11.4 流式渲染规则的一致性和正确性。

```
测试方法:
  - 固定 LLM_STREAM_CHUNK 序列作为输入
  - 快照比对 DOM（或虚拟 DOM 树的 JSON 表示）
  - 每次更新后与基线快照比对

测试序列:
  "text_5_chunks": 5 个连续文本 chunk → 预期: 5 次追加，最终文本完整
  "text_then_tool": 3 个文本 chunk + 1 个 tool_call → 预期: 文本区域 + 工具卡片
  "interrupted_mid_stream": 4 个 chunk + /stop → 预期: 光标收起 + "已中断" 标记
  "tool_call_then_tool_result": tool_call + tool_result → 预期: 工具卡片状态从 running 变为 success
  "output_validator_blocked": 完整回复 + OUTPUT_BLOCKED → 预期: 回复隐藏 + 安全告警

覆盖率目标:
  - 所有消息类型（§11.2）的渲染路径
  - 所有消息状态的渲染路径（pending → completed / error / interrupted）
  - 所有事件类型（§6.2）的前端响应
```

### 13.6 测试基础设施

```
测试基础设施的组件:

  1. MockLLMProvider: 返回固定 token 序列的虚拟 LLM API
     - 支持配置每次调用的延迟、token 数、错误模式
     - 用于集成测试中的可预测回复

  2. FakeEventBus: 内存事件总线
     - 支持背压模拟（配置 L1/L2/L3 背压阈值）
     - 支持事件注入和检索

  3. VirtualFrontend: 无头前端状态机
     - 模拟 Tauri IPC 命令调用
     - 记录收到的事件序列
     - 提供断言 API: assertRenderedText(), assertToolCard(), assertStreamChunks()

  4. DeterministicClock: 可控时钟
     - 替代 SystemTime::now() 用于限流、超时、裁剪测试
     - 时间不真实流动——通过 tick() 手动推进
```

---

## 14. 可观测性架构

> **P0 要求：Phase 2 开工前必须定义。** 没有体系化的可观测性，并发队列 + 状态机 + WAL 重放的组合是不可调试的。

### 14.1 核心原则

```
可观测性三支柱:
  - 指标（Metrics）: 数字化的系统健康度，告警来源
  - 追踪（Tracing）: 请求级的调用链，定位延迟来源
  - 日志（Logging）: 事件明细，故障根因分析

RED 方法论:
  对每个 IPC 命令、LLM Provider 调用、队列操作：
    Rate（速率）: 请求/秒
    Error（错误率）: 错误数 / 总请求数
    Duration（延迟）: 延迟分布（P50 / P95 / P99）
```

### 14.2 指标（Metrics）

**LLM 调用指标：**

| 指标名 | 类型 | 标签维度 | 说明 |
|--------|------|---------|------|
| `llm.requests_total` | counter | `provider`, `model`, `result`(success/error) | LLM API 调用总次数 |
| `llm.request_duration_ms` | histogram | `provider`, `model` | LLM 调用延迟（含首 token 延迟） |
| `llm.first_token_latency_ms` | histogram | `provider`, `model` | 首 token 到达延迟 |
| `llm.tokens_prompt_total` | counter | `provider`, `model` | prompt token 总量 |
| `llm.tokens_completion_total` | counter | `provider`, `model` | completion token 总量 |
| `llm.errors_total` | counter | `provider`, `error_type`(timeout/rate_limit/server_error) | LLM 错误计数 |

**会话与队列指标：**

| 指标名 | 类型 | 标签维度 | 说明 |
|--------|------|---------|------|
| `session.active_count` | gauge | — | 当前活跃会话数 |
| `session.total_created` | counter | — | 累计创建的会话数 |
| `session.state_transitions_total` | counter | `from_state`, `to_state` | 状态转移计数 |
| `session.state_current` | gauge | `state` | 当前各状态会话数分布 |
| `queue.message_enqueued_total` | counter | `session_id` | 入队消息总数 |
| `queue.message_dropped_total` | counter | `session_id`, `reason`(overflow/stall) | 丢弃消息总数 |
| `queue.current_depth` | gauge | `session_id` | 队列当前深度 |
| `queue.stall_total` | counter | `session_id` | QUEUE_STALLED 触发次数 |
| `queue.wait_duration_ms` | histogram | `session_id` | 消息在队列中的等待时间 |

**安全与验证指标：**

| 指标名 | 类型 | 标签维度 | 说明 |
|--------|------|---------|------|
| `sanitizer.actions_total` | counter | `strategy`(replace_token/replace_message/block) | InputSanitizer 动作计数 |
| `validator.checks_total` | counter | `result`(pass/fail/fail_closed) | OutputValidator 检查次数 |
| `validator.fail_closed_total` | counter | — | OutputValidator fail_closed 次数 |

**基础设施指标：**

| 指标名 | 类型 | 标签维度 | 说明 |
|--------|------|---------|------|
| `wal.disk_usage_bytes` | gauge | — | WAL 当前磁盘使用量 |
| `wal.disk_usage_percent` | gauge | — | WAL 磁盘配额使用百分比 |
| `wal.replay_total` | counter | — | WAL 重放事件总数 |
| `wal.replay_skipped_total` | counter | `reason`(session_closed/already_processed) | WAL 重放跳过事件总数 |
| `bus.backpressure_level` | gauge | `level`(0/1/2/3/4) | Event Bus 当前背压等级 |
| `bus.events_dropped_total` | counter | `reason`(backpressure/overflow) | Event Bus 丢弃事件总数 |
| `session.lock_contention_count` | counter | `session_id` | 共享会话乐观锁冲突计数 |

**IPC 命令指标（按 RED 模型）：**

| 指标名 | 类型 | 标签维度 | 说明 |
|--------|------|---------|------|
| `ipc.commands_total` | counter | `command`(chat:send_message/chat:session_list/...) | IPC 命令调用次数 |
| `ipc.command_duration_ms` | histogram | `command` | IPC 命令延迟分布 |
| `ipc.command_errors_total` | counter | `command`, `error_type` | IPC 命令错误计数 |

**前端性能指标（Tauri 前端侧埋点）：**

| 指标名 | 类型 | 说明 |
|--------|------|------|
| `frontend.render_frame_duration_ms` | gauge | 流式渲染帧耗时 |
| `frontend.stream_chunks_per_second` | gauge | 每秒处理的流式 chunk 数 |
| `frontend.event_queue_depth` | gauge | 前端事件队列深度 |

### 14.3 追踪（Tracing）

**Trace 传播路径：**

```
MESSAGE_RECEIVED (trace_id: T1)
  │
  ├─ InputSanitizer (span: sanitize)
  │   └─ event: INPUT_SANITIZED (T1, sanitize.span_id)
  │
  ├─ Dispatcher (span: dispatch)
  │   └─ consistent hashing → Worker-X
  │
  ├─ Queue (span: enqueue)
  │   └─ event: MESSAGE_ENQUEUED (T1, queue.span_id)
  │
  ├─ LLM Skill (span: process)
  │   ├─ SOUL snapshot (span: snapshot_soul)
  │   ├─ Provider call 1 (span: llm_call, tags: provider="openai", model="gpt-4")
  │   │   └─ LLM_STREAM_START → LLM_STREAM_CHUNK × N → LLM_STREAM_DONE
  │   │
  │   ├─ Tool call 1 (span: tool_exec, tags: tool="search", idempotent=true)
  │   │   └─ event: LLM_TOOL_CALL (T1, tool.span_id)
  │   │   └─ event: LLM_TOOL_RESULT (T1, tool.span_id)
  │   │
  │   ├─ Provider call 2 (span: llm_call_2)  — 带 Tool Result 的二次调用
  │   │   └─ LLM_STREAM_START → LLM_STREAM_CHUNK × N → LLM_STREAM_DONE
  │   │
  │   └─ OutputValidator (span: validate)
  │       └─ event: LLM_STREAM_DONE (T1, validate.span_id)
  │
  └─ Frontend (span: render)
      └─ Svelte 组件更新
```

**跨 WAL 重放的 Trace ID 延续：**

```
WAL 重放时的 trace_id 策略:
  - WAL 记录在事件写入时已存储原始 trace_id
  - 重放时保留该 trace_id（不生成新 ID）
  - 重放标记 replay: true 作为 span 属性
  - 查询时可通过 "replay=true" 筛选重放流量
```

**OpenTelemetry 集成点：**

| 集成点 | SDK | 说明 |
|--------|-----|------|
| LLM Skill | OTel Rust SDK (`opentelemetry`) | 主线 trace parent |
| LLM Provider Tool | OTel HTTP instrumentation | LLM API 调用延迟追踪 |
| Tool Runner | OTel Rust SDK | Tool 执行追踪 |
| Event Bus | 手动插桩 | 事件入/出 Bus 追踪 |
| WAL | 手动插桩 | WAL 写入/重放追踪 |
| IPC commands | OTel gRPC/HTTP instrumentation | Tauri IPC 命令追踪 |

### 14.4 日志（Logging）

**日志级别约定：**

| 级别 | 适用场景 | 示例 |
|------|---------|------|
| ERROR | 系统不可恢复、数据丢失、安全漏洞 | OutputValidator fail_closed、WAL 写入失败、LLM Provider 认证失败 |
| WARN | 需关注但不紧急、降级运行 | 队列满、限流命中（非攻击）、WAL 磁盘 >70% |
| INFO | 生命周期变更、用户操作 | 会话创建/关闭、插件加载/卸载、能力注册变更 |
| DEBUG | 调试所需的事件级细节 | 流式 chunk 大小、队列深度变化、状态转移 |
| TRACE | 仅供开发调试 | 每行代码执行的详细追踪 |

**结构化字段规范（所有日志行必须包含）：**

| 字段 | 必填 | 说明 |
|------|------|------|
| `timestamp` | 是 | ISO 8601，带时区 |
| `level` | 是 | ERROR / WARN / INFO / DEBUG / TRACE |
| `component` | 是 | 日志来源组件（llm_skill / queue / wal / sanitizer / ...） |
| `session_id` | 有则填 | 关联的会话 ID |
| `trace_id` | 有则填 | 关联的追踪 ID |
| `message` | 是 | 人类可读的描述 |
| `error_kind` | ERROR/WARN | 错误分类（timeout / auth / quota / ...） |
| `event_id` | 有则填 | 关联的事件 ID |
| `replay` | 有则填 | 是否为 WAL 重放（true / false） |

### 14.5 健康检查端点

| 端点 | 类型 | 检查内容 | 失败响应 |
|------|------|---------|---------|
| `/health` | liveness | AgentRuntime 存活 | 503 |
| `/health/ready` | readiness | 所有组件就绪（Phase 5+） | 503 + 未就绪组件列表 |
| `/health/validator` | readiness | OutputValidator 存活且可用 | 503 + fail_closed 自动生效 |
| `/health/llm` | readiness | 至少一个 LLM Provider 配置且可用 | 503 + 不可用 provider 列表 |
| `/health/wal` | readiness | WAL 磁盘配额正常 | 503 + "WAL disk full"（>90%）或 WARN（>80%） |
| `/health/queue` | readiness | 队列不深度 stall | 503 + "queue stalled"（全局 stall > 60s） |

### 14.6 告警规则

| 告警规则 | 条件 | 严重等级 | 通知渠道 | 说明 |
|---------|------|---------|---------|------|
| `ChatHighErrorRate` | `llm.errors_total` rate > 5/min 持续 5min | critical | PagerDuty + Slack | LLM Provider 可能宕机 |
| `ValidatorFailClosed` | `validator.fail_closed_total` > 3/min | critical | PagerDuty + Slack | OutputValidator 异常，所有回复被拦截 |
| `WalDiskHighUsage` | `wal.disk_usage_percent` > 80% | warning | Slack | WAL 接近配额上限 |
| `WalDiskCritical` | `wal.disk_usage_percent` > 90% | critical | PagerDuty + Slack | WAL 写入可能失败 |
| `SessionStall` | `queue.stall_total` > 0 in 5min | warning | Slack | LLM Skill 或 Tool Runner 可能挂了 |
| `QueueDepthSpike` | `queue.current_depth` > 5 per session | warning | Slack | 单会话遭遇异常并发 |
| `BackpressureL3` | `bus.backpressure_level` = 3 for > 30s | warning | Slack | Event Bus 压力高 |
| `BackpressureL4` | `bus.backpressure_level` >= 4 for > 5s | critical | PagerDuty + Slack | Event Bus 溢出丢弃事件 |
| `RateLimitHit` | `ipc.commands_total{command=chat:send_message}` + 429 > 10/min | warning | Slack | 可疑的用户行为 |
| `SessionLockContention` | `session.lock_contention_count` > 10/min | warning | Slack | 共享会话冲突频繁，需评估降级 |

---

## 15. 历史裁剪策略

### 15.1 问题

LLM 的上下文窗口有限。当会话历史累积超过窗口大小时，必须裁剪（trim）历史消息以保持后续回复的可用性。裁剪策略影响：

- **LLM 回复质量**：丢失关键历史 → Agent 失去上下文
- **用户体验**：消息突然消失/灰化 → 用户困惑
- **WAL/State Store 一致性**：裁剪是逻辑操作还是物理删除？
- **重启恢复**：裁剪标记编码在哪里？

### 15.2 触发时机

```
触发时机（两种模式，由 trim.check_interval 配置）:

  模式 1: per_call（默认）— 每次 LLM 调用前检查
    优点: 精确保证不超窗口
    缺点: 每次调用增加一次上下文大小计算
    适用: 上下文窗口紧、回复质量敏感的会话

  模式 2: per_n_messages — 每 N 条消息后检查
    N = trim.minimum_messages - 1
    优点: 减少计算开销
    缺点: 可能在两次检查之间超过窗口
    适用: 上下文窗口宽、吞吐优先

触发条件:
  total_tokens > context_window × trim.threshold_ratio (默认 0.8)
```

### 15.3 裁剪粒度

```
trim.unit_pairs = true（默认）:
  以 user+assistant 消息对为单位裁剪
  优点: 始终保持语义单元的完整性，不会出现"有回答没有提问"
  缺点: 无法更精细地控制 token 用量

trim.unit_pairs = false:
  以单条消息为单位裁剪
  适用: token 配额非常紧的场景
  风险: 可能出现孤立消息（assistant 回复但 user 消息被裁）
```

### 15.4 裁剪策略

```
策略 1: FIFO（默认）
  行为: 从最早的消息开始裁剪，直到满足 target_token 预算
  算法:
    target = current_tokens - (context_window × threshold_ratio × 0.8)
    // 保留 20% 安全余量，避免裁剪后立刻再次触发
    while current_tokens > target && remaining_messages > trim.minimum_messages:
        移除最旧的消息对
        current_tokens -= removed_tokens
  优点: 实现简单，性能 O(1)
  缺点: 早期上下文可能丢失重要信息

策略 2: Weighted（扩展，P2）
  行为: 按消息重要度加权，保留权重最高的消息
  权重计算:
    - 系统消息 / 安全告警: 权重 100（永不裁剪）
    - 工具调用 + 结果: 权重 50（保留工具执行证据）
    - 用户最近 3 条消息: 权重 30（保留近期上下文）
    - 其他消息: 权重 1（可裁剪）
  算法:
    target = 同上
    while current_tokens > target:
        移除权重最低且时间最早的消息
  缺点: O(n) 计算，需要为每条消息存储权重

安全余量（两种策略共享）:
  目标 token 数 = context_window × threshold_ratio × 0.8
  防止: 裁剪后用户发一条消息立刻再次触发裁剪
```

### 15.5 裁剪事件与恢复

```
HISTORY_TRIMMED 事件 Payload:
{
  "session_id": "session-xxx",
  "trimmed_count": 12,          // 裁剪的消息数
  "remaining_count": 8,         // 保留的消息数
  "trimmed_token_estimate": 3200,  // 回收的 token 估计值
  "strategy": "fifo",            // 使用的裁剪策略
  "trim_id": "trim_abc123"      // 本次裁剪的唯一 ID（用于审计追踪）
}

前端行为:
  1. 收到 HISTORY_TRIMMED → 灰化已归档消息
  2. 每条灰化消息标注 "[已归档]"
  3. 页面顶部显示横幅 "历史消息已归档 (回收约 {N} tokens)"
  4. 灰化消息不可交互（不可 /edit、不可复制）
  5. 用户可通过展开按钮查看灰化消息的原文
```

### 15.6 WAL / State Store 一致性

```
裁剪的持久化策略:
  - 裁剪是逻辑操作，物理上消息保留在 WAL 中
  - State Store 的会话历史字段存储当前上下文（裁剪后的子集）
  - 裁剪不删除 WAL 条目——WAL 仅 TTL 到期后自动清理
  - 重新挂载会话时（断线重连 / 重启），从 State Store 历史拉取，无需重新裁剪

重启后裁剪恢复:
  - Agent 重启后，会话历史从 State Store 加载
  - 此时 history 已经是裁剪后的子集
  - LLM Skill 在下一次调用时重新计算 context_window
  - 如果重启后 WAL 重放导致历史增长，触发新的裁剪
  - 但 HISTORY_TRIMMED 事件不进入 WAL——前端在重启后不重复显示裁剪通知

GET /session/{id}/state 可选字段 (trim_info):
  "trim_info": {
    "last_trim_id": "trim_abc123",
    "trimmed_total": 12,
    "strategy": "fifo"
  }
  → 前端用于恢复灰化标记
```

---

## 16. 非功能需求

| 需求 | 说明 |
|------|------|
| 并发会话数 | 单个 Agent 实例支持 ≥100 个并发对话会话 |
| 响应延迟 | LLM 首 token 延迟由 Provider API 决定，框架不应增加超过 50ms 额外开销 |
| 会话持久化 | 会话历史通过 State Store 持久化，Agent 重启后自动恢复活跃会话 |
| 断线重连 | WebSocket 渠道支持断线重连，恢复未完成的 LLM 响应 |
| 消息顺序 | 同源保序：同一会话的 MESSAGE_RECEIVED 按到达顺序处理 |
| 限流 | 三阶限流（用户级 10 条/分钟 + 会话级 3 条/5 秒 + 全局 100 条/分钟），超过返回速率限制错误 |
| Token 用量统计 | 每次 LLM 调用记录 token 用量，支持按用户/会话/时间段聚合 |
| 审计 | 所有 LLM 调用和 Tool Calling 操作记录审计日志 |

### 容量推导

```
100 并发会话的推导:

假设条件:
  - 每个会话平均历史 10 轮对话（20 条消息）
  - 每条消息平均 500 tokens
  - 单次 LLM 调用延迟 3 秒（含完整 Tool Calling 循环）
  - 最大并发 LLM API 调用数 = 可用 API Key 数 × 并发限制（通常 1 Key = 3-5 并发）

计算:
  单会话吞吐: 1 call / 3s ≈ 0.33 calls/s
  100 会话总需求: 100 × 0.33 = 33 calls/s

瓶颈分析:
  - LLM API 并发限制（每个 Key 3-5 并发）→ 需要 7-11 个 Key
  - Agent 内部调度：Event Bus  + Dispatcher 的背压阈值需 ≥ 33 事件/秒
  - WAL 写入吞吐：≤ 33 条消息/秒，500MB 磁盘 = ~500K 条（按 1KB/条）≈ 4 小时窗口

框架额外开销预算 50ms:
  - 事件序列化/反序列化: ≤ 5ms
  - InputSanitizer: ≤ 5ms
  - Dispatcher 路由: ≤ 1ms
  - 会话级队列入队: ≤ 1ms
  - OutputValidator: ≤ 2s（超时阈值，但正常路径 < 50ms）
  - 合计: ~12ms < 50ms ✓
```

---

## 17. 与现有系统的关系

| 现有组件 | 在本方案中的角色 | 是否需要修改 |
|----------|----------------|------------|
| `plugin.yaml` 解析器 | 读取新增的 `capabilities` 和 `ui` 字段 | 最小修改：新增可选字段解析 |
| `PluginLoader` | 收集 `capabilities` → 传给 `Runtime` | 新增：Phase 2 后能力聚合 |
| `AgentRuntime` | 维护 `capability_registry` | 新增：注册表 + 事件发布 |
| `Event Bus` | 承载所有聊天相关事件 | 否（已有事件系统 + 背压） |
| `Dispatcher` | 将 MESSAGE_RECEIVED 路由到 LLM Skill；session 级分片 | 新增：consistent hashing 分片 |
| `Workflow` | 会话状态机（chat-session 定义） | 新增：状态定义 + 转移表 |
| `Skill 系统` | LLM Skill 作为普通 Skill 注册 | 否 |
| `Tool 系统` | LLM Provider Tool + 会话 Tool Calling | 新增：LLM Provider Tool |
| `Tool Runner` | 执行 Tool Calling 中的工具调用 | 否 |
| `SOUL` | 提供 system prompt + 热更新边界 | 否 |
| `SecretResolver` | 管理 LLM API Key | 否 |
| `InputSanitizer` | 消毒用户输入 | 否 |
| `OutputValidator` | 检查 LLM 输出 (fail_closed) | 否 |
| `State Store` | 会话历史持久化 + WAL 去重 | 否 |
| `Tauri commands.rs` | 新增 chat IPC 命令 + get_capabilities | 新增：约 10-12 个命令 |
| `Tauri 前端路由` | 新增路由守卫 + 动态导航栏 | 修改：路由表 + 条件激活 |
| `ChatPlatformSource` | 新事件源（tauri_desktop/websocket/cli） | 新增 |
| `LLM Provider Tool` | 新 Tool 类型（OpenAI/Anthropic/本地） | 新增 |
| `LLM Skill` | 新 Skill 类型（消费 MESSAGE_RECEIVED） | 新增 |
| `WAL` | MESSAGE_RECEIVED 持久化 + 重放去重 | 否 |
| `AuditLogger` | 记录所有 chat IPC 操作 + trace 链 | 否 |

---

## 18. 边界情况与风险

### 18.1 已知风险

| 风险 | 概率 | 影响 | 应对策略 |
|------|------|------|---------|
| 多个插件声明同一 capability 但语义冲突 | 低 | 高：前端行为不确定 | 运行时对同一 capability 的使用以第一个声明为准；审计日志记录冲突 |
| 插件热卸载时正在进行的 LLM 请求 | 中 | 中：回复丢失 | 卸载前 drain 未完成的 LLM 请求（Phase 4.5 排水逻辑 §9.4）；超时后强制终止 |
| 前端在插件加载前已打开，然后插件加载 | 低 | 低：聊天页未激活 | 前端 listen CAPABILITY_AVAILABLE，收到后激活；用户可手动导航到 /chat |
| 前端在插件卸载时正在聊天页中 | 中 | 中：用户体验中断 | 显示"聊天功能已关闭"提示 + 自动跳转；消息历史保留在 State Store |
| capability_registry 持久化 | 低 | 低 | 不持久化——Phase 2 每次启动重新聚合 |
| 共享会话乐观锁频繁冲突 | 中 | 中：重试开销 | 自动降级为 shared-sub 模式；增加 `session_lock_contention_count` 指标监控 |
| WAL 重放突破状态机边界 | 低 | 高：CLOSED 会话被重放 | WAL 重放前检查会话状态；CLOSED 或不存在 → 跳过 |
| OutputValidator 持续 fail_closed | 低 | 高：所有 LLM 输出被拦截 | 关键告警（§14.6）；提供 /health/validator readiness probe；运维必须立即介入 |
|| /stop 与 LLM_STREAM_DONE 竞态 | 低 | 低：偶尔标记错误 | 500ms 缓存窗口仲裁：窗口内收到 DONE → 视为正常完成 |
|| WAL 写入失败（磁盘满/权限不足） | 低 | 高：消息丢失 | WAL 写入失败 → MESSAGE_RECEIVED 不得进入 Event Bus；ChatPlatformSource 返回 507 Insufficient Storage；`wal.disk_usage_percent` ≥ 90% 时触发关键告警（§14.6 WalDiskCritical）；恢复后 WAL 重放从最近 checkpoint 开始，已确认的事件通过幂等去重跳过 |
|| State Store 写入失败 | 低 | 中：会话状态回滚 | 写入失败时：会话状态回滚到上一个已知持久化点（WAL 中最近 ACK 的事件）；`retry: 3 次`（指数退避 100ms/200ms/400ms）；3 次耗尽后降级为仅内存操作——用户可继续聊天，但重启后丢失该会话；审计日志记录 `state_store_write_failure` |
|| Event Bus 完全不可用（非背压，进程级崩溃） | 低 | 高：所有消息处理停止 | Event Bus 崩溃属于 Aman 运行时级灾难，不在本组件恢复范围内；LLM Skill 检测到 Bus 不可用后停止接受新消息；ChatPlatformSource 返回 503 "Service Unavailable"；/health/ready 返回 503；恢复路径：Agent 重启（Phase 5→0→5），WAL 重放恢复未 ACK 事件 |

### 18.2 未解决的问题

1. **共享能力的所有权**：当 `chat` 能力由多个插件共同提供时，某个插件的版本更新是否应触发 `CAPABILITY_AVAILABLE` 重新广播？当前方案：版本变更不影响能力存在性，不广播。

2. **前端热更新的独立性**：如果前端的 `Chat.svelte` 页面需要更新（bug fix / UI 改版），是否需要重启整个 Tauri 应用？当前：是，因为 Svelte 是编译时框架。长远：可考虑 HMR 开发模式，但发布仍需重新编译。

3. **多 SOUL 并发**：如果用户有多个聊天会话，每个挂载不同的 SOUL，前端如何区分显示？当前：每个会话的 Workflow data 中存储当前 SOUL 引用，前端在会话标题行显示 SOUL 名称。

4. **capability 版本兼容性**：前端 Chat.svelte 声明"我需要 chat 能力 v1"，但插件只提供 v2。如何做兼容声明？当前：不做版本化——capability 名称即契约，版本迭代时协商变更。

5. **历史裁剪与 UI 的一致性**：HISTORY_TRIMMED 事件不进入 WAL，重启后页面重新拉取全量历史，LLM Skill 在下次调用时重新决定裁剪。裁剪后的灰化标记在重启后会丢失——需要从 `GET /session/{id}/state` 的可选 `trim_info` 字段恢复（已在 §15.6 定义）。

---

## 19. 实现路径建议

### Phase 1 — 最小可行（1-2 周）✅ 已完成

```
1. ✅ plugin.yaml 解析器扩展 capabilities 字段（1d）
2. ✅ AgentRuntime 添加 capability_registry 收集 + 事件发布（2d）
3. ✅ Session Workflow 定义（chat-session 状态机基础状态）（1d）
4. ✅ Tauri IPC 添加 get_capabilities 端点（0.5d）
5. ✅ 前端导航栏改为动态过滤（参考 §7.2）（1d）
6. ✅ 前端 Chat.svelte 骨架（消息列表 + 输入框）（3d）
7. ✅ 前端路由守卫（§7.1）（0.5d）
```

### Phase 2 — 完整聊天（3-4 周）

```
# 核心组件（3-4 人并行）
8.  ✅ 测试基础设施搭建（MockLLMProvider + FakeEventBus + 状态机 proptest）（3d）
    已完成为 M4 第 10, 12 项的前置依赖
9.  chat-source 插件实现 ChatPlatformSource（3d）
10. ✅ llm-skill 插件实现 LLM Skill（含会话级等待队列 §4）（3d）
11. ✅ llm-provider-openai Tool 实现（2d）

# 前端
12. 前端消息流式渲染（LLM_STREAM_CHUNK → 实时更新）（2d）
13. 前端工具调用卡片（可折叠/展开）（1d）

# 集成
14. 会话管理 IPC 命令（create/list/close/history/state）（2d）
15. SOUL 集成（system prompt 注入 + 热更新快照边界）（1d）

# 可观测
16. 可观测基础埋点（LLM 调用 + 队列 RED 指标 + trace 传播）（1d）
    与第 10 项并行——在实现 Skill 时同时埋点，避免后期补埋的遗漏

# R2 前移项：无这些基础能力时 Phase 2 体验不可用
17. 用户级限流模型实现（§4.5）（1d）
    防止 Phase 2 线上试用期间的消息注入压力
18. WAL 持久化 + 断线重连协议（§9.1-9.2）（2d）
    无 WAL 时 Agent 重启导致所有会话丢失——线上试用不可接受
19. Phase 4.5 排水逻辑（§9.4）（1d）
    仅在插件热加载/卸载场景需要；若 Phase 2 期间不涉及热更新可推迟
```

### Phase 3 — 增强（灵活排期，~2 周）

```
20. InputSanitizer 三层策略实现（replace_token / replace_message / block）（2d）
21. OutputValidator fail_closed 实现（1d）
22. 多渠道消息聚合显示（§11.2）（1d）
23. 调试面板（集成到 §11 页面业务架构，作为配置可开启的 Debug 模式）（1d）
24. 插件运行时热加载/卸载的前端响应（1d）
25. SOUL 感知层显示（集成到 §5 SOUL 集成——在会话标题行显示 SOUL 身份标识）（0.5d）
26. 命令系统（/session new, /stop, /retry, /edit 等）（2d）
27. 会话分支（branch conversation）（1d）
28. 共享会话乐观锁（1d）
29. 交互单元 trace 链 + 审计日志扩展（1d）
30. 历史裁剪策略实现（§15）（1d）
```

---

## 附录 A：评审追溯

本文件已根据 `/Users/jerin/projects/aman/docs/llm-chat-architect-r1.md` 架构评审意见完成修订。主要变更：

| 评审项 | 原得分 | 变更 |
|--------|--------|------|
| 测试策略 | 0/10 | 新增 §13 测试架构（6 子节） |
| 可观测性 | 3/10 | 新增 §14 可观测性架构（6 子节，含 22 个指标、5 个端点、10 条告警规则） |
| 历史裁剪 | 隐含 | 新增 §15 历史裁剪策略（6 子节） |
| 限流模型 | 隐含 | 新增 §4.5 限流模型（3 维限流 + 算法选择 + 前端 429 处理） |
| Phase 4.5 排水 | 隐含 | 扩展 §9.4 完整排水时序 + 参数关系表 |
| 交叉引用断裂 | — | 修复 Phase 3 对照引用（调试面板→集成到 §11；SOUL 感知→集成到 §5） |
| 扩容推导 | — | 新增 §16 容量推导（100 并发推理过程） |
| OutputValidator 粒度 | — | 新增 §8.2 验证粒度规则（chunk 不逐块验证） |
| SOUL + Tool 权限快照 | — | 新增 §5.2 Tool 权限快照绑定 |
| get_capabilities() 时机 | — | 新增 §6.1 返回时机说明 |
| 能力健康判定 | — | 新增 §10 健康判定标准（核心维度 + 降级时长阈值） |
| /model switch PROCESSING 态 | — | 新增 §11.5 PROCESSING 态执行规则 |

---

*文档版本：v2.1（基于 R2 评审修订）*
*最后更新：2026-05-13*

---

## 附录 B：R2 评审追溯

根据 `/Users/jerin/projects/aman/docs/llm-chat-architect-r2.md` 架构评审意见完成修订。

| R2 建议 | 严重度 | 变更 | 对应章节 |
|---------|--------|------|---------|
| §11.8 与 §15 语义重叠 | 次要 | 精简 §11.8 为纯上下文窗口计算 + 职责分离表；触发时机/策略全部引用 §15；移出触发条件（与 §15.2 重复） | §11.8 |
| Phase 3 项目数膨胀（14 项） | 次要 | 前移 3 项到 Phase 2：用户级限流（#17）、WAL 持久化+断线重连（#18）、Phase 4.5 排水（#19）；各附前移理由；Phase 3 从 14 项减至 11 项（#20-#30）；Phase 2 从 16d 增至 ~21d（3-4 周） | §19 Phase 2/3 |
| 基础设施错误恢复缺失 | 次要 | 新增 3 条风险项：WAL 写入失败（返回 507，不进入 Event Bus）、State Store 写入失败（重试 3 次→降级为仅内存）、Event Bus 崩溃（Aman 级灾难，引用 Agent 重启恢复路径） | §18.1 |
