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
| `crates/gateway/src/runtime/agent_runtime.rs` | 聊天工作流超时逻辑中补充 `session:timeout` 事件 |
| `crates/gateway/src/main.rs` | 三处生命周期点发布 gateway 事件 |
| 可选：`crates/core/src/event.rs` | 如果这些事件使用频率高，可以加枚举变体（非必须，`Custom` 即可） |

### 验证

1. 创建聊天会话 → EventStore 中出现 `session:started` 事件
2. 关闭会话 → EventStore 中出现 `session:closed` 事件
3. 会话超时（等待 120s）→ EventStore 中出现 `session:timeout` 事件
4. `pkill -f gateway` → EventStore 中出现 `gateway:stopping` 事件
5. 重启 gateway → EventStore 中出现 `gateway:starting`、`gateway:ready` 事件

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
| `crates/tool/src/lib.rs` | 在 `ToolRegistry` invoke 逻辑中补充 tool invoke/completed/failed 事件发布到 event bus |
| `crates/gateway/src/runtime/agent_runtime.rs` | Skill event dispatcher（subscribe handler）中，dispatch 前/后发布 message:dispatch / message:completed |
| 可选：`crates/core/src/event.rs` | 同上，非必须 |

### 涉及运行时改动

- `ToolRegistry` 需要持有 `EventBus` 引用。目前它不依赖 event bus。
  - 方案 A：`ToolRegistry` 加 `Option<Arc<dyn EventBus>>` 字段，可选注入
  - 方案 B：在 runtime 层面包装 tool invocation，由 runtime 负责发布事件（推荐——不需要改 tool 签名）
- Skill dispatcher 在 `ensure_observer_subscribed` 的 handler 中已有 `EventStore`；可以复用或追加 publisher。

### 验证

1. 聊天中发送消息引发 tool call → EventStore 中出现 `tool:invoke` + `tool:completed` 事件
2. 模拟工具失败 → EventStore 中出现 `tool:failed` 事件
3. Skill 匹配处理事件 → EventStore 中出现 `message:dispatch` + `message:completed` 事件
4. Trace chain 能通过 parent_event_id 串联 `message:dispatch` → `tool:invoke` → `tool:completed`

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
| `crates/plugins/llm-plugin/src/lib.rs` | LLM provider 调用前后发布事件。具体位置在 `llm_plugin` 调用 `CompletionProvider` 的 `complete()` 前后 |

### 不涉及改动

- 不需要改 `crates/core/src/event.rs`，用 `Custom` 即可
- 不需要改 event bus 基础设施
- 不需要改 gateway / Tauri

### 验证

1. 聊天中发送消息触发 LLM 调用 → EventStore 中出现 `llm:call_started` + `llm:call_ended` 事件
2. 事件 payload 应包含 `model`, `input_tokens`, `output_tokens`, `latency_ms`
3. 如果 LLM 调用失败 → EventStore 中出现 `llm:call_started` + 原有 `llm_error` 事件（不出现 `call_ended`）

---

## 实施顺序策略

```
M1 ──→ M2 ──→ M3
```

- **M1** 最独立、改动最小（现有代码路径加几行 publish），先完成可以快速验证事件扩展方案正确。
- **M2** 改动涉及 `ToolRegistry` 和 skill dispatcher，影响面稍大，排第二。
- **M3** 完全在 llm-plugin 内部，不涉及外部依赖，最可控，排最后。

每个里程碑完成后都验证：
1. `cargo test --workspace` 全部通过
2. 打开 Debug Panel，确认新增事件出现在 EventStore 列表中
3. 检查新增事件的 trace_id 能否与上下游串联
