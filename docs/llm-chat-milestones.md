# LLM Chat — 开发里程碑与任务拆分

> 基于 `/Users/jerin/projects/aman/docs/llm-chat-architect.md` v2.1
> 每个里程碑有明确的可交付物和验收标准，开发者可直接领取任务。
> 架构引用格式：`§章节` 指向架构文档对应章节。

---

## 依赖关系总览

```
M1 能力框架 ✅ ──┬── M2 聊天骨架 ✅
                  │
                  └── M3 测试基础设施 ✅ ── M4 聊天核心 ⏳ ──┬── M5 聊天前端
                                                           │
                                                           └── M6 集成与加固 ── M7 增强打磨
```

- M1/M2 可并行（后端/前端独立）
- M3 必须先于 M4（测试先行）
- M4 完成后 M5/M6 可部分并行

> **当前进度：M1 ✅、M2 ✅、M3 ✅ 已完成。下一个里程碑：M4（聊天核心）。**

---

## M1：能力框架（5.5 天）✅ 已完成

> 目标：插件可声明 `chat` 能力，运行时识别并广播，前端可查询。
> 验收：启动 Aman → `invoke("get_capabilities")` 返回 `["chat"]`。

### T1.1 — plugin.yaml 解析器扩展 capabilities 字段 ✅

| 属性 | 内容 |
|------|------|
| 估时 | 1 天 |
| 涉及 | `crates/plugin/src/lib.rs` |
| 架构 | §2 决策 2 |
| 状态 | ✅ 已完成 |

**子任务：**
1. ✅ 在 `PluginManifest` 结构体中新增 `capabilities: Vec<String>` 字段（可选，默认空）
2. ✅ 在 `PluginManifest` 结构体中新增 `ui: Option<UiDeclaration>` 字段（可选）
3. ✅ `UiDeclaration` 包含 `pages: Vec<String>` 和 `events: Vec<String>`
4. ✅ 解析器测试：合法 yaml → 正确反序列化；无 capabilities 字段 → 默认空数组

**验收：**
- ✅ 已有插件的 `plugin.yaml` 不加新字段 → 解析不报错
- ✅ 新插件的 `capabilities: [chat]` → 正确解析
- ✅ `cargo test -p plugin` 全部通过（22 tests）

---

### T1.2 — AgentRuntime 能力注册表 + 事件发布 ✅

| 属性 | 内容 |
|------|------|
| 估时 | 2 天 |
| 涉及 | `crates/runtime/src/agent_runtime.rs` |
| 架构 | §2 决策 3 |
| 状态 | ✅ 已完成 |

**子任务：**
1. ✅ 在 `AgentRuntime` 中新增 `capability_registry: RwLock<HashMap<String, Vec<CapabilityEntry>>>`
2. ✅ `CapabilityEntry` 结构体：`{ capability, plugin, version, status: Healthy|Degraded|Error }`
3. ✅ Phase 2 完成后收集所有插件的 capabilities，聚合为全局列表
4. ✅ 实现 `get_capabilities()` 方法：返回当前可用能力名数组
5. ✅ 发布 `CAPABILITY_REGISTRY_UPDATED` 事件（首次聚合完成后）
6. ✅ 发布 `CAPABILITY_AVAILABLE` 事件（插件热加载时）
7. ✅ 发布 `CAPABILITY_REMOVED` 事件（插件卸载时）
8. ✅ 发布 `CAPABILITY_DEGRADED` 事件（插件崩溃、或部分插件失联）

**验收：**
- ✅ `cargo check -p runtime` 通过
- ✅ `refresh_capabilities()` 在 Phase 2 自动调用

---

### T1.3 — Session Workflow 状态机定义 ✅

| 属性 | 内容 |
|------|------|
| 估时 | 1 天 |
| 涉及 | `crates/runtime/src/agent_runtime.rs`（WorkflowDef 注册） |
| 架构 | §3 会话状态机 |
| 状态 | ✅ 已完成 |

**子任务：**
1. ✅ 在 `AgentRuntime::build()` 中注册 `chat-session` WorkflowDef
2. ✅ 状态：`ACTIVE / PROCESSING / IDLE / ERROR / RETRYING / TIMEOUT / CLOSED`
3. ✅ 实现 §3.2 全部状态转移（16 条转移规则）
4. ✅ 实现 §3.3 约束：
   - PROCESSING 态拒绝新的 MESSAGE_RECEIVED → 入队（等待队列待 M4 实现）
   - /retry 最多连续 5 次 → `max_retry_count: 5`
   - CLOSED 终态的补偿路径（TIMEOUT→CLOSED, ABANDON_TIMEOUT→CLOSED）
5. ✅ 状态超时配置：ACTIVE(5min) / PROCESSING(2min) / IDLE(10min) / ERROR(2min) / TIMEOUT(2min)

**验收：**
- ✅ `cargo check -p runtime` 通过
- ✅ WorkflowDef.validate() 全部通过

---

### T1.4 — Tauri IPC 添加 get_capabilities 端点 ✅

| 属性 | 内容 |
|------|------|
| 估时 | 0.5 天 |
| 涉及 | `crates/tauri/src/commands.rs`、`crates/tauri/src/models.rs`、`crates/tauri/src/lib.rs` |
| 架构 | §6.1（get_capabilities 返回时机） |
| 状态 | ✅ 已完成 |

**子任务：**
1. ✅ 实现 `#[tauri::command] fn get_capabilities()` → 返回 `Vec<CapabilityEntry>`
2. ✅ Phase 2 完成前返回空数组 `[]`；Phase 2 完成后返回完整列表
3. ✅ 在 `lib.rs` 的 `invoke_handler` 中注册此命令
4. ✅ 前端调用 `invoke("get_capabilities")` 可正常获取

**验收：**
- ✅ `cargo check --workspace` 通过（所有 20 crates）
- ✅ 前端 `App.svelte` 已集成 `get_capabilities` 调用进行动态导航

---

## M2：聊天页面骨架（5 天）✅ 已完成

> 目标：前端出现 Chat 导航项，点击进入聊天页骨架（消息列表+输入框，无后端）。
> 验收：导航到 /chat 显示空白聊天界面，发消息无响应（后端未接）。

### T2.1 — 前端导航栏动态过滤 ✅

| 属性 | 内容 |
|------|------|
| 估时 | 1 天 |
| 涉及 | `crates/tauri/src/App.svelte` |
| 架构 | §7.2 |
| 状态 | ✅ 已完成 |

**子任务：**
1. ✅ 导航栏拆分为 `staticPages` + `chatPage`，条件化合并
2. ✅ 实现 `checkCapabilities()` 调用 `invoke("get_capabilities")` 过滤 Chat 项
3. ✅ 运行时状态变更时自动重新检查能力
4. ✅ 能力缺失时 Chat 导航项不显示（直接隐藏）

**验收：**
- ✅ 无聊天插件时导航栏无 Chat 项
- ✅ 运行时启动后自动检查并更新导航

---

### T2.2 — 前端 Chat.svelte 骨架 ✅

| 属性 | 内容 |
|------|------|
| 估时 | 3 天 |
| 涉及 | `crates/tauri/src/pages/Chat.svelte`（新建） |
| 架构 | §11 页面业务架构 |
| 状态 | ✅ 已完成 |

**子任务：**
1. ✅ 创建 `Chat.svelte` 组件，包含：
   - 会话列表面板（左侧 240px）
   - 消息列表区域（可滚动）
   - 输入框 + 发送按钮
   - 空状态提示
2. ✅ 消息列表的基本渲染：
   - 用户消息右对齐 / Agent 消息左对齐
   - 支持 §11.2 消息类型的基础样式
3. ✅ 输入框功能：
   - Enter 发送，Shift+Enter 换行
   - 发送后清空输入框
   - 空消息不允许发送
4. ✅ 预留 IPC 调用桩（TODO 注释）
5. ✅ 事件订阅桩（onMount 预留）

**验收：**
- ✅ `npm run build` 通过（vite build success）
- ✅ 页面渲染正常，消息列表可滚动
- ✅ 导航到 Chat 显示空白聊天界面

---

### T2.3 — 前端路由守卫 ✅

| 属性 | 内容 |
|------|------|
| 估时 | 0.5 天 |
| 涉及 | `crates/tauri/src/App.svelte`、`crates/tauri/src/pages/Chat.svelte` |
| 架构 | §7.1 |
| 状态 | ✅ 已完成 |

**子任务：**
1. ✅ Chat 导航项仅当 `get_capabilities()` 返回包含 `chat` 时显示
2. ✅ 运行时未启动 / 无能力时 Chat 页面显示空状态 + 提示文案
3. ✅ Chat.svelte 输入框在无能力时自动 disabled
4. ✅ 空状态提示"Tip: Chat capability is not active until the runtime detects a chat plugin"

**验收：**
- ✅ 无聊天插件时导航栏无 Chat 项（完全隐藏）
- ✅ 能力就绪后 Chat 页面正常显示
- ✅ 运行时停止后 Chat 导航项自动消失

---

## M3：测试基础设施（3 天）✅ 已完成

> 目标：搭建测试夹具，M4 的所有核心组件可以 TDD 开发。
> 验收：FakeEventBus + MockLLMProvider + DeterministicClock 可独立运行。
> 实际成果：13 个测试通过（MockLLMProvider 6、FakeEventBus+Clock 7）+ 6 个 proptest 通过

### T3.1 — MockLLMProvider ✅

| 属性 | 内容 |
|------|------|
| 估时 | 0.5 天 |
| 涉及 | `crates/test-utils/src/mock_llm.rs`（新建） |
| 架构 | §13.6 |
| 状态 | ✅ 已完成 |

**子任务：**
1. ✅ 实现 `MockLLMProvider` 结构体，实现 LLM Provider trait（`complete`/`chat`）
2. ✅ 支持配置：固定 token 序列、延迟模拟、错误模式（第 N 次调用失败）、per-call config
3. ✅ 记录调用历史（次数、prompt 内容、参数）
4. ✅ 支持 Tool Calling 模拟

**验收：**
- ✅ 调用 `mock.complete("hello")` → 返回预定义文本
- ✅ 设置 error_on_call(3) → 前 2 次成功，第 3 次返回错误
- ✅ 调用历史可读取

---

### T3.2 — FakeEventBus + DeterministicClock ✅

| 属性 | 内容 |
|------|------|
| 估时 | 1.5 天 |
| 涉及 | `crates/test-utils/src/fake_event_bus.rs`、`crates/test-utils/src/clock.rs`（新建） |
| 架构 | §13.6 |
| 状态 | ✅ 已完成 |

**子任务：**
1. ✅ 实现 `FakeEventBus`：内存事件总线，支持 publish/subscribe/unsubscribe
2. ✅ 背压模拟（配置 L1/L2/L3 背压阈值，达到后返回错误）
3. ✅ 事件检索（published_events / events_matching）
4. ✅ 实现 `DeterministicClock`：始于 UNIX_EPOCH，通过 tick(Duration) 手动推进

**验收：**
- ✅ FakeEventBus 发布/订阅正常
- ✅ 背压模拟：设置 L1=2 → 第 3 个事件返回背压拒绝
- ✅ DeterministicClock：tick(60s) 后 now() 增加 60s

---

### T3.3 — 状态机 proptest 框架 ✅

| 属性 | 内容 |
|------|------|
| 估时 | 1 天 |
| 涉及 | `crates/workflow/tests/proptest_chat_session.rs` |
| 架构 | §13.2 |
| 状态 | ✅ 已完成 |

**子任务：**
1. ✅ 实现 ChatEvent 枚举（12 个变体）+ legal_for/illegal_for 静态分析
2. ✅ 实现 proptest 策略：合法/非法转移验证
3. ✅ 不变性断言：CLOSED 态不接受任何事件
4. ✅ 集成测试：ACTIVE→CLOSED 路径、非法事件拒绝、5 次重试后强制 CLOSED

**验收：**
- ✅ `cargo test -p workflow --test proptest_chat_session` 通过（6 tests）
- ✅ 非法转移被正确拒绝（不 panic，返回错误）
- ✅ 边界序列 5 次重试后确实进入 CLOSED

---

## M4：聊天核心（8 天）

> 目标：消息从输入框到 LLM 回复的完整链路跑通。
> 验收：在 Chat 页面输入"你好"，收到 LLM 回复"你好！有什么可以帮助你的？"

### T4.1 — ChatPlatformSource 插件实现

| 属性 | 内容 |
|------|------|
| 估时 | 3 天 |
| 涉及 | 新建 `crates/plugins/chat-source/` |
| 架构 | §6 桥接层设计 |

**子任务：**
1. 创建 `chat-source` 插件 crate，注册为 EventSource
2. 实现 `ChatPlatformSource` 结构体：
   - 渠道类型：`tauri_desktop`（通过 Tauri IPC 接收用户消息）
   - 接收到的消息封装为 `MESSAGE_RECEIVED` 事件，`trust_level: untrusted`
   - 单条消息限制 `max_message_length_chars: 4096`
3. 发布到 Event Bus
4. 监听 Tauri IPC `chat:send_message` → 生成 event_id（UUID v7）→ publish
5. 启动/停止生命周期：
   - Phase 4 启动监听
   - Phase 5→4 关闭时拒绝新连接，等待 500ms 缓存窗口
6. 集成测试：通过 Tauri IPC 发送消息 → Event Bus 收到 MESSAGE_RECEIVED

**验收：**
- `invoke("chat:send_message", {text: "你好", session: "s1"})` → Event Bus 出现 MESSAGE_RECEIVED 事件
- 消息超长（>4096 chars）→ 返回错误
- 插件未加载时调用 IPC → 返回 "Chat capability not available"

---

### T4.2 — LLM Skill 插件实现（含会话级等待队列）✅

| 属性 | 内容 |
|------|------|
| 估时 | 3 天 |
| 涉及 | 新建 `crates/plugins/llm-skill/` |
| 架构 | §3 会话状态机、§4 并发与队列模型 |
| 状态 | ✅ 已完成 |

**子任务：**
1. ✅ 创建 `llm-skill` 插件 crate，注册为 Skill（`LlmSkill` 结构体实现 `Skill` trait）
2. ✅ 订阅 `MESSAGE_RECEIVED` 事件（`TriggerCondition { event_types: [MessageReceived] }`）
3. ✅ 实现会话级等待队列（§4.1）：
   - `queue_depth_per_session: 10`（可配置）
   - 同一会话消息串行处理（`mpsc::channel` + 后台 task），不同会话并行
   - 队列溢出策略：drop（`try_send` 返回 `Full` 时丢弃）
   - 队列满时发布 `message_dropped`
4. 路由约束（待 M4 final 阶段完成）
5. ✅ 事件发布：
   - `message_queued`（消息入队成功）
   - `message_dropped`（队列满溢出）
   - MVP 回复事件 `llm_reply_ready`（模拟 100ms 延迟后发布 `Echo: {text}`）
6. ✅ 集成测试（7 tests 全部通过）：
   - `accepts_message_received_event`：入队 → 模拟处理 → LLM 回复
   - `drops_message_when_session_queue_is_full`：队列满 → 丢弃 → message_dropped
   - `processes_messages_sequentially_per_session`：同一会话串行
   - `different_sessions_processed_independently`：不同会话并行

**验收：**
- ✅ 同一会话 2 条消息同时到达 → 串行处理（`processes_messages_sequentially_per_session` 验证顺序）
- ✅ 两个不同会话消息同时到达 → 并行处理（`different_sessions_processed_independently`）
- ✅ 队列满 → 新消息被丢弃，发布 `message_dropped`（`drops_message_when_session_queue_is_full`）
- ⏳ 队列中消息随 session close 被清空（待 T6.5 实现）

---

### T4.3 — LLM Provider Tool（OpenAI）实现 ✅

| 属性 | 内容 |
|------|------|
| 估时 | 2 天 |
| 涉及 | 新建 `crates/plugins/llm-provider-openai/` |
| 架构 | §8.4 API Key 管理 |
| 状态 | ✅ 已完成 |

**子任务：**
1. ✅ 创建 `llm-provider-openai` 插件 crate，注册为 Tool（`LlmOpenaiTool` 结构体实现 `Tool` trait）
2. ✅ 实现 LLM Provider Tool：
   - 调用 OpenAI Chat Completion API（异步 `reqwest::Client` + rustls-tls）
   - 流式响应（SSE）预留 → 后续 SSE 解析层独立追加
   - Tool Calling 支持（`format_tools_for_openai` 转换简化 tool def → OpenAI function calling 格式）
3. ✅ API Key 支持两种方式：参数 `api_key` 字段或 `OPENAI_API_KEY` 环境变量
4. ✅ 错误处理全部覆盖：
   - 超时 → `error_type: "timeout"` + 明确错误信息
   - 连接失败 → `error_type: "connection_error"`
   - 认证失败 → `error_type: "authentication_error"` + HTTP status
   - Rate limit → `error_type: "rate_limit_exceeded"` + `retry_after_seconds`
   - 请求失败 → `error_type: "request_failed"`
   - 响应解析失败 → `error_type: "parse_error"` + status_code
5. ✅ 参数 schema 声明：messages（必填）、model、temperature、max_tokens、api_key、api_base、tools
6. ✅ 返回值 schema：content、finish_reason、tool_calls、usage、error、error_type、status_code

**验收：**
- ✅ 单条消息 → 返回 response（content + finish_reason + usage + status_code）
- ✅ Tool Calling：LLM 返回 function_call → 解析并返回 `{ name, arguments }` 数组
- ✅ API Key 错误 → 返回明确错误信息（Incorrect API key provided）
- ✅ 8 tests 全部通过（`cargo test -p llm-provider-openai`）

---

## M5：聊天前端（3 天）

> 目标：前端接收流式事件，实时渲染消息。
> 验收：发消息后看到逐字出现的 LLM 回复，Tool Calling 显示工具卡片。

### T5.1 — 前端消息流式渲染

| 属性 | 内容 |
|------|------|
| 估时 | 2 天 |
| 涉及 | `crates/tauri/src/pages/Chat.svelte` |
| 架构 | §11.4 流式输出语义 |

**子任务：**
1. 监听事件（§6.2 事件契约）：
   - `LLM_STREAM_START` → 新增 assistant_streaming 消息，显示闪烁光标
   - `LLM_STREAM_CHUNK` → 追加文本到渲染缓冲区，光标保持在末尾
   - `LLM_STREAM_DONE` → 关闭光标，消息状态变为 completed
2. 处理 position_hint：
   - `"text"` → 正常追加
   - `"before_tool"` → 暂停追加，准备插入工具卡片
   - `"after_tool"` → 在工具卡片后继续追加
3. 流式渲染规则（§11.4）：
   - 已渲染 chunk 不可变
   - 中断时收起光标并标记"已中断"
   - Tool Calling 期间显示工具执行状态而非空白
4. 性能：每秒处理 ≥60 个 chunk 不掉帧

**验收：**
- 输入"你好" → 看到 LLM 回复逐字出现
- /stop 点击后光标消失，标记"已中断"
- Tool Calling 流程中工具卡片出现后文本继续追加

---

### T5.2 — 前端工具调用卡片

| 属性 | 内容 |
|------|------|
| 估时 | 1 天 |
| 涉及 | `crates/tauri/src/pages/Chat.svelte` 或独立组件 |
| 架构 | §11.2 消息类型 |

**子任务：**
1. 实现 `ToolCallCard.svelte` 组件：
   - 折叠/展开（默认折叠）
   - 显示工具名称 + 参数摘要
   - 执行状态：running（旋转图标）/ success（绿色勾）/ failed（红色叉）
2. 监听事件：
   - `LLM_TOOL_CALL` → 插入 assistant_tool_call 消息（状态 running）
   - `LLM_TOOL_RESULT` → 更新卡片状态为 success/failed，显示结果摘要
3. 样式：区别于普通文本消息（缩进 + 边框 + 图标）

**验收：**
- Tool Calling 出现时卡片正确渲染
- 卡片可点击展开查看参数和结果
- running → success 状态转换流畅

---

## M6：集成与加固（9 天）

> 目标：会话持久化、断线重连、限流、SOUL、可观测就绪。用户可正常使用聊天功能而不丢数据。
> 验收：重启 Aman → 之前的会话历史可恢复。

### T6.1 — 会话管理 IPC 命令

| 属性 | 内容 |
|------|------|
| 估时 | 2 天 |
| 涉及 | `crates/tauri/src/commands.rs` |
| 架构 | §6.1（聊天操作 IPC） |

**子任务：**
1. 实现 IPC 命令：
   - `chat:session_list` → 返回活跃会话列表
   - `chat:session_create` → 创建 Workflow 实例（chat-session），返回 session_id
   - `chat:session_close` → §11.5 安全关闭协议（排水→cancel→5s 等待→CLOSED）
   - `chat:session_history` → 加载指定会话历史消息
   - `chat:session_state` → 返回完整会话状态（断线重连用，§9.1）
   - `chat:stop_generation` → 中断当前回复（500ms 缓存窗口）
   - `chat:retry_last` → 重新生成上次回复
   - `chat:edit_message` → 编辑指定消息并重新处理
2. 前端 IPC 权限控制（§8.5）：能力缺失时返回错误/空结果
3. 集成测试：create → send → history → state → close 完整生命周期

**验收：**
- 创建会话 → session_list 可见 → 发送消息 → history 可查 → close 后不再出现
- 断线重连：重连后 chat:session_state 返回完整状态
- stop_generation 有效中断正在生成的回复

---

### T6.2 — SOUL 集成

| 属性 | 内容 |
|------|------|
| 估时 | 1 天 |
| 涉及 | `crates/plugins/llm-skill/` |
| 架构 | §5 SOUL 集成 |

**子任务：**
1. LLM Skill 在处理 MESSAGE_RECEIVED 时读取当前 SOUL → `Soul::to_system_prompt()`
2. 实现 SOUL 热更新快照边界（§5.2）：
   - 交互单元开始时固定 SOUL 快照
   - 同一交互单元内所有 LLM 调用使用同一快照
   - Tool 权限白名单与 SOUL 快照绑定
3. 前端显示当前会话的 SOUL 名称（会话标题行）

**验收：**
- 不同 SOUL 的会话回复风格不同
- 对话进行中修改 SOUL → 当前回复不受影响，下一条消息才生效
- Tool 权限随 SOUL 快照绑定，热更新不会导致权限跳变

---

### T6.3 — 可观测基础埋点

| 属性 | 内容 |
|------|------|
| 估时 | 1 天 |
| 涉及 | `crates/plugins/llm-skill/`、`crates/runtime/` |
| 架构 | §14 可观测性架构 |

**子任务：**
1. 集成 OpenTelemetry Rust SDK（`opentelemetry` crate）
2. 关键指标埋点（§14.2）：
   - `llm.requests_total`、`llm.request_duration_ms`、`llm.first_token_latency_ms`
   - `session.active_count`、`session.state_transitions_total`
   - `queue.message_enqueued_total`、`queue.message_dropped_total`、`queue.current_depth`
   - `ipc.commands_total`、`ipc.command_duration_ms`
3. Trace 传播（§14.3）：
   - MESSAGE_RECEIVED 携带 trace_id
   - LLM Skill 创建 root span，Provider 调用/Tool 执行为子 span
   - WAL 重放保留原始 trace_id
4. 健康检查端点（§14.5）：
   - `/health`（liveness）
   - `/health/ready`（readiness）
   - `/health/llm`（LLM Provider 可用性）

**验收：**
- Jaeger/Grafana 可看到 trace 链路
- Prometheus 可抓取指标
- `/health/ready` 在 Phase 5 前返回 503，Phase 5 后返回 200

---

### T6.4 — 用户级限流实现

| 属性 | 内容 |
|------|------|
| 估时 | 1 天 |
| 涉及 | `crates/plugins/chat-source/` |
| 架构 | §4.5 限流模型 |

**子任务：**
1. 实现 Sliding Window Log 限流器（§4.5 Python 伪代码 → Rust）
2. 用户级限流：10 条/分钟
3. 限流命中 → HTTP 429 / WebSocket 4290 + `retry_after_seconds`
4. 前端 429 处理：禁用输入框 N 秒 + 倒计时提示
5. 限流状态不持久化（重启后清零——§4.5 安全策略）

**验收：**
- 1 分钟内发送 11 条消息 → 第 11 条返回 429
- 前端输入框被禁用，显示倒计时
- 倒计时结束后可继续发送

---

### T6.5 — WAL 持久化 + 断线重连

| 属性 | 内容 |
|------|------|
| 估时 | 2 天 |
| 涉及 | `crates/plugins/chat-source/`、`crates/plugins/llm-skill/` |
| 架构 | §9.1-9.2 |

**子任务：**
1. MESSAGE_RECEIVED 在进入 Event Bus 前写入 WAL
2. WAL 重放去重（§9.2）：
   - 每个事件携带 UUID v7
   - LLM Skill 入口幂等检查（processed_events 集合）
   - 重放事件标记 `replay: true`
3. 重放前会话状态检查：CLOSED → 跳过
4. 断线重连协议（§9.1）：
   - `GET /session/{id}/state` 返回完整状态
   - `state_version` 用于增量一致性校验
   - 客户端用服务端状态覆盖本地缓存
5. WAL 配置：
   - 磁盘配额：500MB，>80% 告警
   - 保留 TTL：7 天
   - 二阶段提交：WAL→Event Bus→消费后 ACK

**验收：**
- 发送消息 → 杀进程 → 重启 → 会话历史完整恢复
- WAL 重放：已处理事件跳过（幂等），未处理事件正确消费
- CLOSED 会话的 WAL 事件被跳过
- 断线重连后前端状态与服务端一致

---

### T6.6 — Phase 4.5 排水逻辑

| 属性 | 内容 |
|------|------|
| 估时 | 1 天 |
| 涉及 | `crates/runtime/`、`crates/plugins/llm-skill/` |
| 架构 | §9.4 Phase 4.5 |

**子任务：**
1. 实现排水流程：
   - 标记插件 draining → 拒绝新请求
   - 等待 in-flight 请求完成（drain_timeout: 30s）
   - 超时后强制 cancel（审计日志 reason: "drain_forced_cancel"）
   - 写入 checkpoint 到 State Store
2. drain_timeout vs session.close_timeout 区分（§9.4 参数关系表）
3. 前端收到 CAPABILITY_REMOVED 时：
   - 关闭活跃聊天标签页
   - 清理前端消息缓冲区
   - 不删除 State Store 持久化历史

**验收：**
- 插件卸载时 in-flight LLM 请求在 30s 内完成或被 cancel
- 被 cancel 的请求记录审计日志
- 重新安装插件后可恢复之前会话

---

## M7：增强打磨（约 2 周）

> 目标：安全过滤、命令系统、多会话分支、裁剪等完善。
> 验收：完整功能上线，所有边缘情况有处理。

### T7.1 — InputSanitizer 三层策略

| 属性 | 内容 |
|------|------|
| 估时 | 2 天 |
| 涉及 | `crates/core/src/sanitizer.rs`（或等价） |
| 架构 | §8.1 |

**子任务：**
1. 实现 replace_token 策略：正则/关键词匹配 → 替换命中子串
2. 实现 replace_message 策略：高风险模式 → 整条替换为 `[redacted]`
3. 实现 block 策略：确定恶意内容 → 拒绝发送
4. 审计日志：event_id + strategy + matched_pattern + original_content_hash + sanitized_content
5. 前端显示替换后的实际内容

**验收：**
- 包含 prompt injection 模式的消息被消毒
- 恶意 shell 注入被 block
- 审计日志记录完整

---

### T7.2 — OutputValidator fail_closed

| 属性 | 内容 |
|------|------|
| 估时 | 1 天 |
| 涉及 | `crates/core/src/validator.rs` |
| 架构 | §8.2 |

**子任务：**
1. 完整回复（LLM_STREAM_DONE 后）验证：
   - Secret 泄漏检测（私钥/Token 正则）
   - 系统提示泄漏检测
   - Tool 注入检测
2. fail_closed：Validator 不可用 → 所有回复被阻止
3. 超时阈值：2s
4. 前端显示 OUTPUT_BLOCKED 系统消息
5. `/health/validator` 端点

**验收：**
- LLM 回复含 API Key → 被拦截
- Validator 进程崩溃 → 所有回复被阻止（不绕过）
- `/health/validator` 失败时 Pod 不接收流量

---

### T7.3 — 命令系统实现

| 属性 | 内容 |
|------|------|
| 估时 | 2 天 |
| 涉及 | `crates/tauri/src/pages/Chat.svelte`、`crates/plugins/chat-source/` |
| 架构 | §11.5 |

**子任务：**
1. 命令分类实现：
   - 非 LLM 命令（跳过队列）：`/session list|rename|switch`、`/help`、`/debug`、`/export`
   - LLM 依赖命令（入队 FIFO）：`/retry`、`/edit`、`/session new`、`/model switch`、`/provider switch`、`/soul switch`
   - 中断命令：`/stop`、`/session close`
2. `/stop` 500ms 缓存窗口仲裁
3. `/session close` 安全关闭协议（§11.5）
4. `/edit` 替换语义：定位 message_id → 删除后续 → 替换 → MESSAGE_EDITED
5. `/retry` 两种模式：默认（仅文本）/ `--full`（完整重放，要求 idempotent）
6. `/model switch` PROCESSING 态规则（§11.5）

**验收：**
- `/retry` 重新生成回复
- `/stop` 中断当前生成（500ms 内收到 DONE→正常完成）
- `/edit` 修改历史消息后后续消息被移除 → 重新生成
- `/model switch` 在 IDLE 态立即生效，PROCESSING 态入队等待

---

### T7.4 — 历史裁剪策略实现

| 属性 | 内容 |
|------|------|
| 估时 | 1 天 |
| 涉及 | `crates/plugins/llm-skill/` |
| 架构 | §15 |

**子任务：**
1. 实现 context_window 计算（§11.8）
2. 实现 FIFO 裁剪策略（默认）：
   - 以 user+assistant 消息对为单位
   - 保留 20% 安全余量
   - 最少保留 `trim.minimum_messages` 条
3. 发布 HISTORY_TRIMMED 事件（§15.5 payload）
4. 前端处理：灰化已归档消息 + 横幅提示
5. WAL/State Store 一致性（§15.6）：裁剪是逻辑操作，物理保留在 WAL
6. 重启恢复：trim_info 字段恢复灰化标记

**验收：**
- 会话历史超过窗口 80% → 自动裁剪
- 裁剪后前端显示灰化消息和横幅
- 重启后灰化标记通过 trim_info 恢复

---

### T7.5 — 会话分支与共享会话

| 属性 | 内容 |
|------|------|
| 估时 | 1 天 |
| 涉及 | `crates/plugins/llm-skill/` |
| 架构 | §11.3 会话模型 |

**子任务：**
1. 实现分支会话（branch conversation）：基于某个 message_id 创建分支
2. 实现共享会话乐观锁：
   - version 字段校验
   - 冲突时重试 3 次（指数退避）
   - 耗尽后降级为 shared-sub 模式
3. 会话类型扩展：ad-hoc / persistent / shared / shared-sub / branch / role-play

**验收：**
- 基于消息创建分支 → 独立会话，不影响原会话
- 多渠道同时写入 → 乐观锁冲突检测正常
- 3 次重试后自动降级

---

### T7.6 — 多渠道消息聚合 + 调试面板 + SOUL 感知 + 热加载响应

| 属性 | 内容 |
|------|------|
| 估时 | 2 天 |
| 涉及 | 前端、运行时 |
| 架构 | §11.1、§11.2、§9.4、§10 |

**子任务：**
1. 多渠道消息聚合显示：同一会话来自不同渠道的消息在 UI 层融合
2. 调试面板：配置可开启的 Debug 模式，显示事件流、trace_id、状态转移日志
3. SOUL 感知层显示：会话标题行显示 SOUL 名称和身份标识
4. 插件热加载/卸载前端响应：CAPABILITY_AVAILABLE/REMOVED/DEGRADED 事件完整处理

**验收：**
- WebSocket + Tauri 双渠道消息在同一会话中聚合
- Debug 面板显示实时事件流
- 会话标题显示 SOUL 名称

---

### T7.7 — 交互单元 trace 链 + 审计日志扩展

| 属性 | 内容 |
|------|------|
| 估时 | 1 天 |
| 涉及 | `crates/plugins/llm-skill/`、审计日志系统 |
| 架构 | §11.6 |

**子任务：**
1. 实现 trace_chain 追踪：trace_id → trace_prev → trace_branch_from 递归展开
2. 审计日志支持 trace_chain 查询
3. Token 用量按 trace_chain 聚合
4. 每条 trace 记录：trace_id、trace_prev、trace_branch_from（不可 null 的 trace_id）

**验收：**
- /edit 后新 trace 的 trace_prev 指向被替换的 trace
- 审计日志可按 trace_chain 展开完整编辑/重试历史
- Token 用量报表按 trace_chain 聚合正确

---

## 里程碑汇总

| 里程碑 | 任务数 | 估时 | 进度 | 可并行性 |
|--------|--------|------|------|---------|
| M1 能力框架 | 4 | 5.5d | ✅ 已完成 | 与 M2 并行 |
| M2 聊天骨架 | 3 | 5d | ✅ 已完成 | 与 M1 并行 |
| M3 测试基础设施 | 3 | 3d | ✅ 已完成 | 需 M1 完成 |
| M4 聊天核心 | 3 | 8d | ✅ 已完成 | 需 M3 完成 |
| M5 聊天前端 | 2 | 3d | ⏳ 待开始 | 需 M4 完成 |
| M6 集成与加固 | 6 | 9d | ⏳ 待开始 | 需 M4 完成 |
| M7 增强打磨 | 7 | 10d | ⏳ 待开始 | 需 M6 完成 |
| **总计** | **28** | **~43d** | **完成 12/28 任务** | M1∥M2 → M3 → M4 → M5∥M6 → M7 |

---

## 当前焦点

M1+M2+M3 已完成。M4 全部完成（T4.1 ChatPlatformSource ✅、T4.2 LLM Skill ✅、T4.3 LLM Provider OpenAI ✅）。下一个里程碑：**M5（聊天前端）**。
