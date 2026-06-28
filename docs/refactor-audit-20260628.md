# Aman 重构审计报告：LlmCognitiveEngine + process_message_v2

审计日期：2026-06-28
范围：agent_harness.rs (1403行) + cognitive/llm/src/lib.rs (791行)
状态：构建通过，LLM 可运行，但结果不满意 — 根源确认为功能退化

---

## 总览

| # | 问题 | 严重度 | 影响 |
|---|------|--------|------|
| 1 | session_progress::evaluate() 被砍 | 🔴 P0 | Agent 无法判断是否有进展，盲跑 5 轮 |
| 2 | build_continuation_context() 已死 | 🔴 P0 | 「继续」功能退化，不再压缩结构化摘要 |
| 3 | ContinuationMode 被丢弃 | 🔴 P0 | Fresh/Continue/Replay 三路径合并为同一路径 |
| 4 | InterruptFlag 未传入引擎 | 🔴 P0 | /stop 命令在 ReAct 循环中完全无效 |
| 5 | 6/11 关键事件缺失 | 🟠 P1 | 前端监控盲区，仪表盘看不到工具执行状态 |
| 6 | Token 预算被丢弃 | 🟠 P1 | 无 token 超限预警，无 config_warning 事件 |
| 7 | skill_view/format_reminder 缺失 | 🟡 P2 | 复杂 skill 执行质量下降 |
| 8 | 无多轮 ReAct 循环测试 | 🟡 P2 | 回归风险高 |

---

## 🔴 P0-1: session_progress::evaluate() — 进度评估被砍

**旧代码 (react_loop)**：max_turns 到达时调用 `session_progress::evaluate()`，检测 5 种信号：
- `collision_found`：工具输出中出现 COLLISION FOUND
- `looks_stuck`：Jaccard 词重叠 > 0.8 或 100+ 条消息无进展
- `best_partial_match`：部分匹配计数
- `unique_tools`：使用的工具多样性
- 然后根据这些信号决定是否 auto-continue

**新代码 (LlmCognitiveEngine::process)** 第 585-605 行：
```rust
if turn >= max_turns {
    if self.config.background && continuation < self.config.max_continuations {
        continuation += 1;
        turn = 0;
        if estimated_total_tokens > MAX_HISTORY_TOKENS {
            let keep = messages.len() / 2;
            messages = messages[messages.len() - keep..].to_vec();
        }
        continue;  // 无条件续跑
    }
    return Err(CognitiveError::MaxDepthReached { depth: turn });
}
```

问题：**完全无条件续跑**。无论 agent 是否 stuck、是否产出碰撞、是否有进展，都盲跑 5 轮。

修复路径：
1. 将 `session_progress::evaluate()` 的输入改为 `Vec<ChatMessage>`（而非依赖 harness 的全局状态）
2. 在 `process()` 的 max_turns 分支中调用它
3. 仅当 `!collision_found && !looks_stuck` 时才继续

---

## 🔴 P0-2: build_continuation_context() — 已死代码

文件：`agent_harness.rs:1039`，标记 `#[allow(dead_code)]`

旧逻辑：将完整 session history 压缩为结构化摘要：
```
[Previous Session Summary]
Goal: <用户第一轮消息>
Progress: <完成/未完成，碰撞发现>
4 messages exchanged
Key findings: ...
Tool usage: 3 calls across 2 unique tools
  grep: 2 calls
  cat: 1 calls
Last action: ...
```

新逻辑：auto-continue 时只做 `messages = messages[messages.len()/2..]`（粗暴截断后半段），丢失所有结构化信息。

修复路径：
1. 去掉 `#[allow(dead_code)]`
2. 在 `process()` 的 auto-continue 分支中，将 history 压缩后替换为 `build_continuation_context()` 的输出
3. 将 `build_continuation_context` 移入 `cognitive-llm`（或暴露为公共函数供 engine 调用）

---

## 🔴 P0-3: ContinuationMode — 三种路径合并为一种

文件：`agent_harness.rs:679`
```rust
let _ = (&react_mode, &continuation_mode); // consumed by process_message_v2
```

旧逻辑 `process_message()` 区分三条路径：
- `ContinuationMode::Fresh` — 普通消息，追加到 history
- `ContinuationMode::Continue` — 用户点「继续」，调用 `build_continuation_context()` 压缩
- `ContinuationMode::Replay` — gateway 重启恢复，`restore_session_history()` 完整重建

现在：三条路径全部调用相同的 `process_message_v2()`，它不知道 continuation_mode。用户点「继续」的效果和发新消息完全一样。

修复路径：
1. `process_message_v2` 增加 `continuation_mode: ContinuationMode` 参数
2. `ContinuationMode::Continue` 时调用 `build_continuation_context()` 替代原始 history
3. `ContinuationMode::Replay` 时保持已有此前的 replay 后恢复的 history

---

## 🔴 P0-4: InterruptFlag — 注册了但引擎不检查

文件：`agent_harness.rs:246-247`
```rust
let flag = Arc::new(InterruptFlag::new());
self.register_interrupt(session_id, Arc::clone(&flag));
let engine = self.build_cognitive_engine(agent_id, model, session_id, background).await?;
// flag 从未传入 engine!
```

`build_cognitive_engine` 不接收 `InterruptFlag`，`LlmCognitiveEngine` 的 `process()` 循环中没有任何中断检查。用户发 `/stop` 只会设置 flag，但正在运行的 LLM 调用和工具执行完全不受影响。

修复路径：
1. `build_cognitive_engine()` 增加 `interrupt: Arc<InterruptFlag>` 参数
2. 传入 `LlmCognitiveEngine`（新增字段）
3. 在 `process()` 循环的每次迭代开始和 LLM 调用前检查 `flag.is_interrupted()`
4. 中断时返回 `CognitiveError` 或返回部分内容

---

## 🟠 P1-1: 6/11 关键事件缺失

迁移清单 `docs/react-migration-checklist.md` 记录了 11 个必须保留的事件。现状：

| 事件 | 状态 | 位置 |
|------|------|------|
| `llm:call_started` | ✅ | process() line 616 |
| `llm:call_ended` | ✅ | process() line 671 |
| `agent:token_used` | ✅ | process() line 710 |
| `agent:reply_stream_*` | ✅ | CognitiveListener→SB adapter |
| `agent:auto_continue` | ✅ | process() line 597 |
| `agent:got_tool_calls` | ❌ | 缺失 — 工具执行前不发布 |
| `agent:tool_results_fed_back` | ❌ | 缺失 — 工具结果注入后不发布 |
| `agent:reply_interrupted` | ❌ | 缺失 — 无中断机制 |
| `agent:history_compressed` | ❌ | 缺失 — 无压缩事件 |
| `agent:auto_continue_stopped` | ❌ | 缺失 — 续跑终止时不发布 |
| `llm_error` | ❌ | 缺失 — 错误只包装为 EngineError 返回 |

影响：仪表盘无法显示工具执行状态、无法追踪压缩、无法感知中断。

修复路径：
1. `execute_tool_calls()` 前后发布 `got_tool_calls` + `tool_results_fed_back`
2. `process()` 的 auto-continue 结束分支发布 `auto_continue_stopped`
3. history 压缩时发布 `history_compressed`
4. LLM 重试耗尽后发布 `llm_error` 事件再返回错误

---

## 🟠 P1-2: Token 预算被丢弃

文件：`agent_harness.rs:244`
```rust
let _tb = self.init_token_budget(agent_id, session_id, model, &inst, &soul_snapshot, &past_history, &tools).await;
```

`init_token_budget()` 做了三件事：
1. 根据模型初始化 `TokenBudget`（含 `session_token_limit`）
2. 估算 system/tool-schema/history tokens
3. 配置不正确时发布 `agent:config_warning` 事件

返回值被 `let _tb` 丢弃。引擎内部用的是简单粗暴的 `content.len() / 2` 估算，不关联 model-specific limits。

修复路径：
1. `init_token_budget()` 返回 `TokenBudget` 实例
2. 传入 `LlmEngineConfig`（新增 `token_budget` 字段）
3. 引擎用 `TokenBudget::estimate_tokens()` 替代手动估算

---

## 🟡 P2-1: skill_view 特殊处理 + format_reminder 缺失

旧 `process_react_turn()` 的 ToolCalls 分支包含：
- 检测 `skill_view` 工具调用 → 加载 skill body → 注入强化提示
- 2 轮后且加载过 skill 时，追加 format_reminder：`"Fill ALL sections"`

新引擎的 `process()` 的第 738-764 行只是通用地执行工具+注入结果，没有任何 skill 感知逻辑。

影响：使用复杂 skill（如 ipo-research）时输出质量下降，可能出现截断或不完整。

修复路径：
1. 引擎接收 skill body 信息（通过 `CognitiveContext` 或 engine config）
2. 检测到 skill 被调用后，在后续 prompt 中注入强化指令

---

## 🟡 P2-2: 零多轮 ReAct 循环测试

`cognitive/llm/tests/cognitive_engine_contract.rs` 的 8 个测试覆盖：
- 空观测短路
- provider 错误包装
- 单轮 text reply
- 单轮 tool_call（无 tool_registry 时返回 Decision）
- subscribe/unsubscribe 契约
- reset_session 幂等

零覆盖：
- **多轮 ReAct 循环**（LLM 返回 tool_call → 引擎执行 → 反馈 → LLM 再次响应 → final reply）
- **auto-continue** 触发与终止
- **max_turns** 边界
- **工具执行失败后的重试与回退**
- **history 压缩**

修复路径：新增 `multi_turn_react_with_tool_execution` 测试（需要 mock ToolRegistry）

---

## 🟡 P2-3: auto_continue 事件 source 不一致

引擎发布的 `agent:auto_continue` 事件 source 为 `"cognitive-engine"`，而旧代码用的是 `SOURCE_AGENT_HARNESS`。如果仪表盘/监控按 source 过滤，这些事件将不可见。

---

## 修复优先级建议

### 第一批（阻塞性 — 本周完成）
1. **P0-3** ContinuationMode 恢复 — 最少改动，最高用户感知
2. **P0-4** InterruptFlag 接入 — /stop 功能回归
3. **P0-1** session_progress 接入 — 恢复智能续跑判断

### 第二批（质量 — 下周完成）
4. **P0-2** build_continuation_context 复活
5. **P1-1** 6 个缺失事件补全
6. **P1-2** Token 预算传入引擎

### 第三批（增强 — 后续迭代）
7. **P2-1** skill_view 强化逻辑
8. **P2-2** 多轮循环测试
9. **P2-3** 事件 source 统一

---

## 架构评价

重构方向正确 — `CognitiveEngine` trait 解耦 gateway 和模型实现是正确的长期方向。
但迁移清单 `react-migration-checklist.md` 中标记 ❌ 的项目被标记为「待迁移」后实际被遗忘，
导致关键功能退化。核心问题是：

1. **auto-continue 从「智能评估驱动」退化为「盲跑计数器」**
2. **session 续跑从「结构化压缩」退化为「粗暴截断」**
3. **用户中断从「响应式」退化为「完全无视」**

LLM 能正常运行（provider 适配层正确）但结果不满意的根因就在于这些编排层功能的退化。
