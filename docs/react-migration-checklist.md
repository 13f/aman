# ReAct 迁移清单：LlmReActEngine → LlmCognitiveEngine::process()

> 审计日期: 2026-06-27
> 源文件: `kernel/gateway/src/runtime/agent_harness.rs`

## 架构说明

当前 ReAct 逻辑分布在三个层：

```
AgentHarness::react_loop()        ← 循环编排器 (line 2912)
  ├── spawn_stream_forwarder()    ← 流式管道 (line 3234)
  ├── process_react_turn()        ← 单轮处理 (line 2734)
  │     ├── engine.execute_turn() ← LLM 调用 (LlmReActEngine line 747)
  │     └── engine.execute_tools()← 工具执行 (LlmReActEngine line 965)
  └── pre-turn checks             ← 压缩/中断/预算/自动续跑
```

迁移目标：把以上所有逻辑内化到 `LlmCognitiveEngine::process()` 中。

---

## 第一层：LlmReActEngine::execute_turn() (line 747-963)

### 1. ✅ 系统提示组装 (line 769-778)
- 从 ctx.soul_snapshot.system_prompt 取基础提示
- 如果有 ctx.memory_context，追加 `## Retrieved Memories`

### 2. ❌ LLM 调用事件 (line 752-767, 826-841)
- `llm:call_started` — 调用前发布到 agent local bus
- `llm:call_ended` — 调用后发布，含 `success: bool`
- 迁移后需保留

### 3. ✅ Provider 解析 (line 785-789)
- `agent_registry.get_llm_provider(&ctx.agent_id)` 
- 迁移后通过 DelegatingLlmProvider 或直接传入

### 4. ❌ LLM 重试机制 (line 791-823)
- 最多 5 次重试
- 指数退避: 1s, 3s, 9s, 27s, 81s (上限 120s)
- `is_retryable_llm_error()` — 区分永久错误和临时错误
  - 永久: 400, 401, 402, 403, billing, insufficient_quota
  - 临时: timeout, connection, 429, 500, 502, 503, 504, stream closed, unexpected eof
- 迁移后需保留

### 5. ✅ OutputValidator (line 845-893)
- 已迁移到 `LlmCognitiveEngine::process()` ✅

### 6. ✅ ContentFilter (line 895-927)
- 已迁移到 `LlmCognitiveEngine::process()` ✅ (2026-06-27)

### 7. ❌ Token 使用事件 (line 929-948)
- `agent:token_used` — 估算 tokens = content.len() / 4
- 迁移后需保留

### 8. ✅ 返回类型映射 (line 949-962)
- 无 tool_calls → `ReActTurn::Finished { content, finish_reason }`
- 有 tool_calls → `ReActTurn::ToolCalls { content, calls, reasoning_content }`
- 错误 → `ReActTurn::Error`

---

## 第二层：LlmReActEngine::execute_tools() (line 965-1210)

### 1. ❌ ToolExecutor 构造 (line 971-990)
- 注入 interrupt_flag, security_config, anon_tool_policy
- 迁移后需保留 — ToolExecutor 是独立的

### 2. ❌ 执行模型分类 (line 996-1008)
- Independent — 并行执行 (如 read, search)
- Stateful/SideEffect — 顺序执行 (如 write, exec, db)
- 通过 `tool.execution_model()` 判断

### 3. ❌ 并行 + 顺序混合执行 (line 1010-1106)
- Phase 1: 所有 Independent 调用并发启动 (tokio::spawn)
- Phase 2: Stateful/SideEffect 调用顺序执行
- 结果按原始顺序合并 (`sort_by_key(|(i, _)| *i)`)

### 4. ❌ 工具重试机制 (line 992-1043)
- 最多 3 次重试，每次间隔 1s
- `is_retryable_error()` — 区分永久和临时
  - 永久: unrecoverable, no such file, not found, permission denied
  - 临时: timeout, connection, refused, reset, temporary, rate limit

### 5. ❌ Detach 处理 (line 1108-1210)
- **Non-blocking (direct_act)**: 记录 pending_detach, 发布 awaiting_detach 事件, 跳过等待
- **Blocking (react_loop)**: 订阅 `tool:completed` 事件, 阻塞等待进程退出
  - 超时处理: 检测超时，kill 进程
  - 中断处理: 检查 interrupt_flag，SIGTERM → SIGKILL
- 迁移后需保留

---

## 第三层：process_react_turn() (line 2734-2850)

### 1. ❌ 流式管道 (line 2741)
- `spawn_stream_forwarder()` — 3 层管道 (sync_channel → bridge thread → tokio mpsc → event bus)
- 发布的事件: `agent:reply_stream_start`, `agent:reply_chunk`, `agent:reply_stream_done`, `agent:reply_stream_error`
- 迁移后需保留

### 2. ❌ Activity 状态更新 (line 2743, 2764)
- `set_activity("Thinking...")` — LLM 调用时
- `set_activity("Using tools: ...")` — 工具执行时
- 迁移后需保留

### 3. ❌ Finished 分支 (line 2746-2756)
- 清空 stream_cb, 等待 stream_handle
- 追加 assistant 消息到 ctx.history
- 记录 token 使用
- 返回 `Ok(false)` — 循环结束

### 4. ❌ ToolCalls 分支 (line 2757-2827)
- 清空 stream_cb, 等待 stream_handle (保证 reply_stream_done 先于工具执行)
- 发布 `agent:got_tool_calls` 事件
- 历史记录: assistant 消息 + 格式化的 tool_calls
- 调用 `engine.execute_tools(ctx, &calls, true)` — block_on_detach=true
- 发布 `agent:tool_results_fed_back` 事件
- **skill_view 特殊处理**: 检测 skill_view 调用 → 加载 skill body → 强化提示
- **format_reminder**: 2 轮后且加载过 skill 时，追加格式提醒
- ctx.turn += 1
- 返回 `Ok(true)` — 继续循环

### 5. ❌ Error/Err 分支 (line 2829-2848)
- 清空 stream_cb, 等待 stream_handle
- 发布 `llm_error` 事件
- 返回 Error

---

## 第四层：react_loop() (line 2912-3129)

### 1. ❌ Pre-turn 检查 (line 2929-3055)
- **Max turns 到达**: 
  - 压缩历史 → 持久化到 session store
  - `session_progress::evaluate()` — 检测 collision_found, looks_stuck
  - **Background 自动续跑**: 
    - 条件: background && continuation_count < 5 && !collision_found && !looks_stuck
    - 发布 `agent:auto_continue` 事件
    - ctx.turn = 0, continue 循环
    - 不满足条件: 发布 `agent:auto_continue_stopped`, 返回 MaxTurnsReached
  - **非 Background**: 直接返回 MaxTurnsReached
- **中断检查**: 检查 interrupt_flag, 发布 `agent:reply_interrupted`, 返回 Interrupted

### 2. ❌ Token 预算 + 压缩 (line 3077-3116)
- 每轮估算 history tokens
- `token_budget.needs_trim()` → `compressor.compress_with_boundaries()`
- 压缩后发布 `agent:history_compressed` 事件（含 messages_removed, tokens_saved, strategy 等）

### 3. ❌ 单轮处理 (line 3119)
- 调用 `process_react_turn()`
- 返回 false → 循环结束，返回 final reply
- 返回 true → 继续循环

---

## 第五层：AgentHarness 辅助方法

### 1. ❌ spawn_stream_forwarder (line 3234-3330)
- 3 层背压管道
- 同时发布到 global bus 和 agent local bus
- 需要迁移到 cognitive engine 内部或保留在 gateway 作为通用组件

### 2. ❌ build_full_system_prompt (line 3166-3211)
- Python-first (SelfBridge), Rust fallback (build_system_prompt_fallback)
- 需要继续由 gateway 提供，传入 CognitiveEngine

### 3. ❌ build_tool_descriptors (line 3132-3157)
- 过滤 LLM provider 工具
- 检查 agent tool policy (tool_allowed)
- 需要继续由 gateway 提供

---

## 迁移优先级

### 先迁移（核心逻辑）
1. LLM 重试 + 指数退避 (execute_turn)
2. Token 使用事件发布 (execute_turn)
3. LLM 调用事件发布 (execute_turn)  
4. 工具执行 + 重试 (execute_tools)
5. Detach 阻塞/非阻塞处理 (execute_tools)

### 再迁移（编排逻辑）
6. process_react_turn 的 Finished/ToolCalls/Error 分支
7. react_loop 的 pre-turn 检查 (max turns, 中断, 压缩, 自动续跑)
8. 流式管道 (spawn_stream_forwarder)

### 保留在 gateway（依赖注入）
9. build_full_system_prompt — 依赖 SelfBridge (Python)
10. build_tool_descriptors — 依赖 AgentRegistry
11. ToolExecutor — 独立组件，作为依赖注入 CognitiveEngine

---

## 事件清单（8 个需保留）

| 事件 | 触发时机 |
|---|---|
| `llm:call_started` | LLM 调用前 |
| `llm:call_ended` | LLM 调用后 |
| `agent:token_used` | 收到 LLM 回复后 |
| `agent:got_tool_calls` | 检测到工具调用 |
| `agent:tool_results_fed_back` | 工具结果注入历史后 |
| `agent:reply_stream_start/chunk/done/error` | 流式输出 |
| `agent:reply_interrupted` | 用户中断 |
| `agent:history_compressed` | 上下文压缩 |
| `agent:auto_continue` | 后台自动续跑 |
| `agent:auto_continue_stopped` | 后台续跑终止 |
| `llm_error` | LLM 调用失败 |
