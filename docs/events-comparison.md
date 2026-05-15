# Agent Framework 事件系统比较报告

> 调研日期：2026-05-15  
> 覆盖框架：OpenPeon (CESP v1.0)、Hermes Agent、OpenClaw、Claude Code、CrewAI、LangGraph

---

## 1. OpenPeon — CESP v1.0（9 个事件）

OpenPeon 不是完整的 agent 框架，而是一个**编码事件音效标准**（Coding Event Sound Pack Specification）。它的事件刻意保持最小化，聚焦于「用户需要音频反馈」的时刻。

### 核心事件（6 个，播放器必须支持）

| 事件 | 语义 | 触发时机 |
|---|---|---|
| `session.start` | 会话/工作区打开 | IDE 启动、终端会话开始、agent 连接 |
| `task.acknowledge` | 工具已接受任务，正在处理 | 命令已接受、构建开始、agent 工作中 |
| `task.complete` | 工作单元成功完成 | 构建完成、测试通过、agent 任务完成 |
| `task.error` | 某件事失败了 | 构建失败、测试失败、运行时错误、agent 崩溃 |
| `input.required` | 工具阻塞等待用户输入或批准 | 权限提示、确认对话框、审查请求 |
| `resource.limit` | 命中速率/令牌/配额限制 | API 速率限制、上下文窗口已满、额度耗尽 |

### 扩展事件（3 个，可选）

| 事件 | 语义 | 触发时机 |
|---|---|---|
| `user.spam` | 用户发送命令过快 | 快速连续提示、按钮狂按 |
| `session.end` | 会话优雅关闭 | IDE 关闭、终端退出、agent 断开连接 |
| `task.progress` | 长时间任务仍在运行 | 构建进行中、长时间 agent 任务、部署运行中 |

### 设计特点

- 命名规范：`{domain}.{event}` 点号分隔，domain 包括 `session`、`task`、`input`、`resource`、`user`
- 纯标准规范，不绑定任何特定实现
- 已建立 100+ 音效包的注册表生态
- 已集成到 Claude Code、Cursor、VS Code + Copilot 等工具

---

## 2. Hermes Agent（6 个插件钩子）

Hermes 的钩子系统极其精简。通过插件中的 `ctx.register_hook()` 注册。没有消息级钩子，没有压缩钩子，没有子 agent 钩子。

| 钩子 | 时机 | 可决策？ |
|---|---|---|
| `pre_tool_call` | 任何工具执行前 | ✅ 可拦截/修改 |
| `post_tool_call` | 工具返回后 | ❌ 仅观察 |
| `pre_llm_call` | LLM 调用循环前 | ✅ 可注入上下文 |
| `post_llm_call` | 成功回合后 | ❌ 仅观察 |
| `on_session_start` | 新会话首轮 | ❌ 仅观察 |
| `on_session_end` | 对话结束/退出 | ❌ 仅观察 |

### 额外能力

- `ctx.inject_message()` — 向对话中注入消息
- `ctx.register_tool()` — 注册自定义工具
- `ctx.register_cli_command()` — 注册 CLI 子命令
- 自动错误捕获，无需 try/except
- 支持守护线程 fire-and-forget（如音效播放）

---

## 3. OpenClaw（~32 个插件钩子）

最全面的钩子系统。插件钩子（`api.on()`）与内部钩子（`HOOK.md` 脚本）分离。按功能面分组。

### Agent 回合（9 个）

| 钩子 | 时机 | 可决策？ |
|---|---|---|
| `before_model_resolve` | 模型解析前 | 可覆盖 provider/model |
| `agent_turn_prepare` | 提示构建前的回合准备 | 消费队列注入 |
| `before_prompt_build` | 提示构建前 | 注入动态上下文 |
| `before_agent_start` | 兼容性组合阶段 | ⚠️ 已弃用 |
| `before_agent_run` | 最终提示检查 | ✅ 可阻塞 |
| `before_agent_reply` | 短路模型回复 | ✅ 合成回复 |
| `before_agent_finalize` | 检查最终回复 | ✅ 请求追加回合 |
| `agent_end` | 回合结束 | ❌ 观察（消息/成功状态/耗时） |
| `heartbeat_prompt_contribution` | 心跳上下文 | ❌ 注入心跳专用上下文 |

### 对话观察（4 个）

| 钩子 | 时机 |
|---|---|
| `model_call_started` | 模型调用开始 |
| `model_call_ended` | 模型调用结束 |
| `llm_input` | 观察 provider 输入（系统提示、历史等） |
| `llm_output` | 观察 provider 输出、用量、token 预算 |

### 工具（4 个）

| 钩子 | 时机 | 可决策？ |
|---|---|---|
| `before_tool_call` | 工具执行前 | ✅ 重写参数/阻塞/要求批准 |
| `after_tool_call` | 工具执行后 | ❌ 观察结果/错误/耗时 |
| `tool_result_persist` | 工具结果持久化前 | ✅ 重写 assistant 消息 |
| `before_message_write` | 消息写入前 | ✅ 阻塞（罕见） |

### 消息与投递（6 个）

| 钩子 | 时机 | 可决策？ |
|---|---|---|
| `inbound_claim` | 入站消息路由前 | ✅ 声明处理权 |
| `message_received` | 收到入站消息 | ❌ 观察 |
| `message_sending` | 出站消息发送前 | ✅ 重写内容或取消投递 |
| `message_sent` | 出站消息发送后 | ❌ 观察成功/失败 |
| `before_dispatch` | 分发前 | ✅ 重写 |
| `reply_dispatch` | 回复分发管道 | ❌ 参与最终分发 |

### 会话与压缩（5 个）

| 钩子 | 时机 |
|---|---|
| `session_start` | 会话开始（含 reason: new/reset/idle/daily/compaction/deleted/shutdown/restart） |
| `session_end` | 会话结束 |
| `before_compaction` | 压缩前 |
| `after_compaction` | 压缩后 |
| `before_reset` | 会话重置前 |

### 子 Agent（4 个）

| 钩子 | 时机 |
|---|---|
| `subagent_spawning` | 子 agent 生成中 |
| `subagent_delivery_target` | 子 agent 投递目标确定 |
| `subagent_spawned` | 子 agent 已生成 |
| `subagent_ended` | 子 agent 结束 |

### 生命周期（4 个）

| 钩子 | 时机 | 可决策？ |
|---|---|---|
| `gateway_start` | Gateway 启动 | ❌ 启动插件服务 |
| `gateway_stop` | Gateway 停止 | ❌ 停止插件服务 |
| `cron_changed` | 定时任务变更 | ❌ 观察 |
| `before_install` | 技能/插件安装前 | ✅ 可阻塞 |

### 关键差异化特性

- **优先级系统** — 数字越大越先执行，同优先级按注册顺序
- **每钩子超时预算** — 可配置 per-hook timeout
- **类型化 SDK** — `definePluginEntry()` + TypeScript
- **双层架构** — 插件钩子（代码级）vs 内部钩子（HOOK.md 脚本，操作员级）

---

## 4. Claude Code（27 个钩子事件）

Shell 命令式钩子，配置在 `.claude/settings.json` 中。三种节奏：per-session、per-turn、per-tool-call。通过 `matcher` 和 `if` 条件过滤。

| 事件 | 节奏 | 决策能力 |
|---|---|---|
| `SessionStart` | Per session | 注入上下文、环境变量 |
| `Setup` | --init 模式 | 一次性准备工作 |
| `InstructionsLoaded` | 加载 CLAUDE.md 时 | 观察 |
| `UserPromptSubmit` | Per turn | 可阻塞展开 |
| `UserPromptExpansion` | 命令展开时 | 可阻塞 |
| `PreToolUse` | Per tool call | 阻塞、修改参数 |
| `PermissionRequest` | 权限弹窗时 | 自动批准/拒绝 |
| `PermissionDenied` | 自动模式拒绝时 | 信号重试 |
| `PostToolUse` | 工具成功后 | 修改输出 |
| `PostToolUseFailure` | 工具失败后 | 观察 |
| `PostToolBatch` | 并行工具批次完成后 | 观察 |
| `Notification` | 通知时 | 桌面通知 |
| `SubagentStart` | 子 agent 生成 | 观察 |
| `SubagentStop` | 子 agent 完成 | 观察 |
| `TaskCreated` | 任务创建 | 观察 |
| `TaskCompleted` | 任务标记完成 | 观察 |
| `Stop` | Claude 完成响应 | 观察 |
| `StopFailure` | API 错误导致回合结束 | 观察（输出和退出码忽略） |
| `TeammateIdle` | Agent 团队成员即将空闲 | 观察 |
| `ConfigChange` | 配置文件变更 | 观察 |
| `CwdChanged` | 工作目录变更（cd 命令） | 观察 |
| `FileChanged` | 被监视的文件变更 | 观察 |
| `WorktreeCreate` | 工作树创建 | 覆盖默认行为 |
| `WorktreeRemove` | 工作树移除 | 观察 |
| `PreCompact` | 上下文压缩前 | 注入上下文 |
| `PostCompact` | 压缩完成后 | 观察 |
| `Elicitation` | MCP 服务器请求用户输入 | 观察 |
| `ElicitationResult` | 用户响应 MCP 请求后 | 观察 |
| `SessionEnd` | 会话终止 | 观察 |

### 四种处理器类型

- **command** — shell 命令，JSON 通过 stdin 传入
- **HTTP** — POST 请求，JSON body
- **prompt-based** — 使用 Claude 模型评估条件
- **agent-based** — 使用 Claude agent 评估条件

---

## 5. CrewAI（事件总线 + 装饰器钩子）

类型化事件总线（`crewai_event_bus.on(EventClass)`），不是钩子系统。事件是 Python 数据类。粒度较粗——在 crew/agent/task 级别，不在 tool-loop 级别。

### Crew 事件（7 个）

`CrewKickoffStarted`, `CrewKickoffCompleted`, `CrewKickoffFailed`, `CrewTestStarted`, `CrewTestCompleted`, `CrewTestFailed`, `CrewTrainStarted`, `CrewTrainCompleted`, `CrewTrainFailed`

### Agent 事件（6 个）

`AgentExecutionStarted`, `AgentExecutionCompleted`, `AgentExecutionError`, `LiteAgentExecutionStarted`, `LiteAgentExecutionCompleted`, `LiteAgentExecutionError`, `AgentEvaluationStarted`, `AgentEvaluationCompleted`, `AgentEvaluationFailed`

### Task 事件（4 个）

`TaskStarted`, `TaskCompleted`, `TaskFailed`, `TaskEvaluation`

### Tool 事件（6 个）

`ToolUsageStarted`, `ToolUsageFinished`, `ToolUsageError`, `ToolValidateInputError`, `ToolExecutionError`, `ToolSelectionError`

### MCP 事件（5 个）

`MCPConnectionStarted`, `MCPConnectionFailed`, `MCPToolExecutionStarted`, `MCPToolExecutionCompleted`, `MCPToolExecutionFailed`

### LLM 事件

通过独立的 `@llm_callback` 装饰器处理，不在事件总线中。

### 装饰器钩子（代码级）

`@before_kickoff`, `@after_kickoff`, `@agent_execution_callback`, `@task_callback`, `@llm_callback`, `@tool_callback`

---

## 6. LangGraph（流式模式）

不是事件钩子——是一个**流式系统**。通过 `stream_mode` 订阅：

| 模式 | 返回内容 |
|---|---|
| `values` | 每步后的完整状态 |
| `updates` | 每节点的状态增量 |
| `messages` | LLM 消息 token/chunk |
| `debug` | 完整跟踪，包含所有内部事件 |
| `custom` | 用户定义的 `StreamWriter` 数据 |
| `events` | 所有内部事件（节点开始/结束、通道更新等） |

### 其他相关机制

- **checkpointer** — 持久化/断点续传
- **interrupt** — 人机协作断点
- **节点级** — `on_chain_start`、`on_chain_end` 等 LangChain 回调

LangGraph 是根本不同的范式——图执行而非 agent 生命周期。

---

## 综合对比矩阵

| 维度 | OpenPeon | Hermes | OpenClaw | Claude Code | CrewAI | LangGraph |
|---|---|---|---|---|---|---|
| 事件数量 | 9 | 6 | ~32 | 27 | ~30+ 类 | 6 模式 |
| 类型 | 规范标准 | 插件代码钩子 | 插件代码钩子 | Shell 配置钩子 | 事件总线 (Python) | 流式 |
| 可决策钩子 | ❌ | ✅ (3/6) | ✅ (~12/32) | ✅ (~10/27) | ❌ (观察) | ❌ |
| 工具 pre/post | ❌ | ✅ | ✅ | ✅ | ✅ (总线) | ❌ |
| LLM 调用钩子 | ❌ | ✅ | ✅ | ❌ | ✅ (装饰器) | ❌ |
| 会话生命周期 | ✅ | ✅ | ✅ | ✅ | ❌ | ❌ |
| 上下文压缩 | ❌ | ❌ | ✅ | ✅ | ❌ | ❌ |
| 子 agent 钩子 | ❌ | ❌ | ✅ | ✅ | ❌ | ✅ (嵌套图) |
| 消息投递管道 | ❌ | ❌ | ✅ | ❌ | ❌ | ❌ |
| Gateway 生命周期 | ❌ | ❌ | ✅ | ❌ | ❌ | ❌ |
| 优先级系统 | ❌ | ❌ | ✅ | ❌ | ❌ | ❌ |
| 超时预算 | ❌ | ❌ | ✅ | ❌ | ❌ | ❌ |
| 用户输入钩子 | ✅ | ❌ | ✅ | ✅ | ❌ | ✅ (interrupt) |
| 权限钩子 | ❌ | ❌ | ✅ | ✅ | ❌ | ❌ |
| 文件变更钩子 | ❌ | ❌ | ❌ | ✅ | ❌ | ❌ |
| 工作树钩子 | ❌ | ❌ | ❌ | ✅ | ❌ | ❌ |

---

## 架构洞察

四大设计哲学：

1. **音效通知**（OpenPeon）— 单一目的，面向用户的音频反馈。不是框架事件系统，但在其领域内定义清晰，已被广泛采用。

2. **生命周期钩子**（Hermes、OpenClaw、Claude Code）— agent 执行循环中的拦截点。
   - **OpenClaw** 最细粒度（32 个钩子，分 6 个面），有优先级、超时预算、typed SDK
   - **Claude Code** 最操作员友好（shell 命令，零代码），有文件监视、工作树钩子
   - **Hermes** 最精简（6 个钩子），适合快速集成

3. **事件总线**（CrewAI）— crew/agent/task 边界间的粗粒度组织事件。配合装饰器回调处理 LLM/工具调用。适合监控和可观测性。

4. **图流式**（LangGraph）— 根本不同：事件是图执行跟踪（节点开始/结束、状态转换），而非 agent 生命周期钩子。适合构建有状态工作流。

---

## OpenPeon 9 事件在所有框架中的映射

OpenPeon 的 9 个事件映射到各框架中的对应概念：

| OpenPeon | Hermes | OpenClaw | Claude Code | CrewAI |
|---|---|---|---|---|
| `session.start` | `on_session_start` | `session_start` | `SessionStart` | `CrewKickoffStarted` |
| `task.acknowledge` | `pre_llm_call` | `agent_turn_prepare` / `before_prompt_build` | `PreToolUse` | `ToolUsageStarted` |
| `task.complete` | `post_llm_call` | `agent_end` | `Stop` | `CrewKickoffCompleted` |
| `task.error` | `post_tool_call` (检查错误) | `after_tool_call` (检查错误) | `PostToolUseFailure` / `StopFailure` | `CrewKickoffFailed` / `AgentExecutionError` |
| `input.required` | ❌ 无直接映射 | `message_received` (入站时) | `PermissionRequest` / `Notification` | ❌ 无直接映射 |
| `resource.limit` | ❌ 无直接映射 | ❌ 无直接映射 | `PreCompact` (接近) | ❌ 无直接映射 |
| `user.spam` | ❌ 无直接映射 | ❌ 无直接映射 | `UserPromptSubmit` (rapid 检测) | ❌ 无直接映射 |
| `session.end` | `on_session_end` | `session_end` | `SessionEnd` | `CrewKickoffCompleted` |
| `task.progress` | ❌ 无直接映射 | `heartbeat_prompt_contribution` | `TeammateIdle` | ❌ 无直接映射 |

任何框架的事件都超出了 OpenPeon 的覆盖范围，但在「何时播放音效」这个特定场景下，OpenPeon 的 9 个核心领域覆盖在所有框架中都是充分的。
