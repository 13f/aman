# Events 里程碑

将 events-comparison.md 分析的高价值补充拆分为三个独立里程碑，按实施顺序排列。

---

## M1：Session & Gateway 生命周期事件

**范围**：会话创建/关闭/超时 + Gateway 进程启动/就绪/停止

### 事件清单

| 事件 | 类型 | 发布时机 | 参考来源 |
|------|------|----------|----------|
| `session:started` | `Custom("session:started")` | `chat_session_create` HTTP handler 完成后 | OpenPeon `session.start`、Hermes `on_session_start` |
| `session:closed` | `Custom("session:closed")` | `chat_session_close` 工作流转移完成时 | OpenPeon `session.end`、Claude Code `SessionEnd` |
| `session:timeout` | `Custom("session:timeout")` | 工作流状态机因超时进入 TIMEOUT/CLOSED 时 | OpenPeon `resource.limit`（接近） |
| `gateway:starting` | `Custom("gateway:starting")` | Gateway main.rs `run()` 入口，runtime start 前 | OpenClaw `gateway_start` |
| `gateway:ready` | `Custom("gateway:ready")` | Gateway `runtime.start()` 成功，HTTP server 开始服务 | OpenClaw `gateway_start` |
| `gateway:stopping` | `Custom("gateway:stopping")` | Gateway 收到 SIGTERM/SIGINT，`runtime.shutdown()` 前 | OpenClaw `gateway_stop` |

### 涉及文件

| 文件 | 改动 |
|------|------|
| `crates/gateway/src/runtime/http.rs` | `chat_session_create`→发布 `session:started`；`chat_session_close`→发布 `session:closed` |
| `crates/gateway/src/main.rs` | 三处生命周期点发布 gateway 事件 |

> **`session:timeout` 暂未实现** — 当前工作流 engine 的 `handle_timeouts()` 仅在测试模块中被调用，
> 生产环境没有 timeout 轮询。需要一个后台 task 定期调用 `workflow_engine.handle_timeouts(now)`
> 才能触发此事件。该事件保留在计划中，待引入正式 timeout 轮询时一起完成。

### 验证

- [x] 创建聊天会话 → EventStore 中出现 `session:started` 事件
- [x] 关闭会话 → EventStore 中出现 `session:closed` 事件
- [ ] 会话超时（等待 120s）→ EventStore 中出现 `session:timeout` 事件（待 timeout 轮询实现）
- [x] `pkill -f gateway` → EventStore 中出现 `gateway:stopping` 事件（需 EventStore 已订阅）
- [x] 重启 gateway → EventStore 中出现 `gateway:starting`、`gateway:ready` 事件

---

## M2：Tool & Message Dispatch 事件

**范围**：工具调用前后 + 消息分发给 skill 的前后

### 事件清单

| 事件 | 类型 | 发布时机 | 参考来源 |
|------|------|----------|----------|
| `tool:invoke` | `Custom("tool:invoke")` | `ToolRegistry` 执行工具前 | Hermes `pre_tool_call`、CrewAI `ToolUsageStarted` |
| `tool:completed` | `Custom("tool:completed")` | 工具成功执行后 | Hermes `post_tool_call`、CrewAI `ToolUsageFinished` |
| `tool:failed` | `Custom("tool:failed")` | 工具执行出错时 | CrewAI `ToolUsageError`、Claude Code `PostToolUseFailure` |
| `message:dispatch` | `Custom("message:dispatch")` | Skill dispatcher 将事件路由给 skill 时 | OpenClaw `before_dispatch` |
| `message:completed` | `Custom("message:completed")` | Skill 处理完成后 | OpenClaw `reply_dispatch` |

### 涉及文件

| 文件 | 改动 |
|------|------|
| `crates/pipeline/src/lib.rs` | 新增 `ToolEventSink` trait；`PipelineEngine` 增加 `tool_sink` 字段 + `with_tool_sink()`；`execute_tool_with_retry()` 中 invoke/completed/failed 三点发出事件 |
| `crates/gateway/src/runtime/agent_runtime.rs` | 新增 `BusToolEventSink` 实现（将 `ToolEventSink` 事件发布到 EventBus）；`SkillEventDispatcher` 中 dispatch 前/后发布 `message:dispatch` / `message:completed` |

### 涉及运行时改动

- `ToolEventSink` trait 定义于 `pipeline` crate，三种回调：`on_tool_invoke` / `on_tool_completed` / `on_tool_failed`
- `BusToolEventSink` 实现于 `gateway` crate（依赖 `pipeline` + `event-bus`），将 sink 回调转为 EventBus publish
- `PipelineEngine.execute_tool_with_retry()` 中在每个 tool 调用前后调用 sink（若配置）
- `SkillEventDispatcher.handle()` 中在 `execute_matching()` 前后分别发布 `message:dispatch` 和 `message:completed`

### 架构说明

当前 `PipelineEngine` 不在生产路径中（仅用于 tests 和 dispatcher crate），因此 tool 事件在生产环境的聊天流程中暂时不会触发。
生产路径中 tool 调用由 LLM plugin 的 `rig::agent::prompt()` 直接处理，未经过 `ToolRunner` 或 `PipelineEngine`。
如需生产环境 tool 事件，后续需在 LLM plugin 级别的 tool invocation 处补充，或待 dispatcher/PipelineEngine 接入生产路径。

### 验证

1. `cargo test -p pipeline` 全部通过（12 tests）— `ToolEventSink` 不影响 sink=None 的现有逻辑
2. `message:dispatch` + `message:completed` 在生产路径的 `SkillEventDispatcher` 中触发（events → skills）
3. `tool:invoke` + `tool:completed` + `tool:failed` 在 `PipelineEngine` 路径中触发（通过 `ToolEventSink`）

---

## M3：LLM Call 详细事件

**范围**：LLM provider 调用的进出观察点

### 事件清单

| 事件 | 类型 | 发布时机 | 参考来源 |
|------|------|----------|----------|
| `llm:call_started` | `Custom("llm:call_started")` | LLM provider 调用即将发起，已计算出输入 token 数时 | Hermes `pre_llm_call`、OpenClaw `model_call_started` |
| `llm:call_ended` | `Custom("llm:call_ended")` | LLM provider 返回完整响应，记录 token 用量和耗时后 | Hermes `post_llm_call`、OpenClaw `model_call_ended` |

### 涉及文件

| 文件 | 改动 |
|------|------|
| `agent_harness.rs` | `call_llm()` 前发布 `llm:call_started`，成功后发布 `llm:call_ended`；失败时 publish `llm_error`（不出现 `call_ended`） |

### 不涉及改动

- 不需要改 `crates/core/src/event.rs`，用 `Custom` 即可
- 不需要改 event bus 基础设施
- 不需要改 gateway / Tauri

### 实现细节

- `llm:call_started` payload: `session_id`, `model`, `input_tokens_estimate`, `original_message_id`, `soul_name`
- `llm:call_ended` payload: `session_id`, `model`, `input_tokens_estimate`, `output_tokens_estimate`, `latency_ms`, `original_message_id`, `soul_name`
- token 数为估算值（`text.len() / 4 + 1`），非 provider 精确计数
- 失败路径：`llm:call_started` 已发布但 `call_ended` 不发布 + 原有 `llm_error` 事件

### 验证

- [x] LLM Plugin removed from workspace (moved to AgentHarness)
- [x] `cargo check -p gateway -p pipeline` 全部通过，无新警告
- [ ] 端到端：聊天消息 → LLM 调用后 EventStore 中出现 `llm:call_started` + `llm:call_ended`
- [ ] 模拟 LLM 调用失败 → EventStore 中出现 `llm:call_started` + `llm_error`（无 `call_ended`）

---

## 实施顺序策略

```
M1 ──→ M2 ──→ M3
```

- **M1** 最独立、改动最小（现有代码路径加几行 publish），先完成可以快速验证事件扩展方案正确。
- **M2** 改动涉及 `ToolRegistry` 和 skill dispatcher，影响面稍大，排第二。
- **M3** 完全在 AgentHarness 内部，不涉及外部依赖，最可控，排最后。

每个里程碑完成后都验证：
1. `cargo test --workspace` 全部通过
2. 打开 Debug Panel，确认新增事件出现在 EventStore 列表中
3. 检查新增事件的 trace_id 能否与上下游串联
