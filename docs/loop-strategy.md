# Loop Strategy — LlmCognitiveEngine 三层渐进式迭代

> 状态：✅ 三层全部实现 (2026-06-28)
> 文件：`cognitive/llm/src/lib.rs`
> 起点：`refactor-audit-20260628.md` P0/P1/P2 修复后

---

## 架构定位

```
Gateway (AgentHarness)
  │
  └── CognitiveEngine::process(observations) → Decision
        │
        └── LlmCognitiveEngine  ← 本设计
              │
              ├── ReAct loop (64 turns × 5 continuations)
              ├── Layer 1: Continuation Context 描述→引导
              ├── Layer 2: Progress 二值→五级梯度
              └── Layer 3: Approach Tracking 跨轮记忆
```

LlmCognitiveEngine 是 ReAct 循环的**内化实现**。Gateway 只调用 `CognitiveEngine::process()`，不感知循环内部结构。

---

## 第一层：Continuation Context — 从描述到引导

### 问题

auto-continue 时旧代码只输出一份纯描述性摘要：

```
[Previous Session Summary]
Goal: find all TODO items
Progress: 12 messages exchanged
Tool usage: 8 calls across 3 tools
Status: incomplete
```

LLM 拿到这份摘要后被要求继续，但它不知道**什么方法失败了、什么有效、该避免什么**。

### 方案

新增 `ContinuationContext` 结构体（8 个子类型），将 auto-continue 上下文从"发生了什么"升级为"该怎么做"：

```
[Continuation Context — Round 1/5]
Goal: find all TODO items
Approach so far: I'll use recursive grep filtered by *.rs

✅ EFFECTIVE (continue using):
  grep (3/3 calls) — matched 47 files across src/

❌ INEFFECTIVE (DO NOT retry):
  find (0/2 calls) — all calls failed

⚠️ UNRESOLVED (needs attention this round):
  .tsx files not yet searched

💡 LESSON: Focus on effective patterns, avoid find, address unresolved items
```

### 新增组件

| 组件 | 作用 |
|------|------|
| `ContinuationContext` | 结构化上下文容器 |
| `EffectivePattern` / `IneffectivePattern` | 工具分类 |
| `ToolStat` | 工具调用统计（calls, successes, key_output） |
| `ContinuationStatus` | 四态进度（CollisionFound/Advancing/Incomplete/Stuck） |
| `infer_tool_success()` | 从 output 文本推断工具成败 |
| `extract_tool_output_signal()` | 提取关键指标行 |
| `extract_approach_description()` | 从 assistant 回复提取方法描述 |
| `detect_unresolved_items()` | 检测未完成项 |
| `generate_lesson()` | 生成 prescriptive 指导 |

---

## 第二层：Progress 梯度 — 从二值到五级

### 问题

旧 `SessionProgress` 只有两个 bool：`collision_found` 和 `looks_stuck`。auto-continue 决策是二值的：要么停止，要么继续。所有继续行为消耗相同 budget。

### 方案

五级 `ProgressLevel` 枚举，每级驱动不同的 auto-continue 行为：

| Level | 检测条件 | auto-continue 行为 |
|-------|---------|-------------------|
| **Achieved** | collision found | 停止，正常返回 |
| **Advancing** | tool 成功率 ≥70%，有新发现 | **继续但不消耗 budget**（免费轮次） |
| **Creeping** | tool 成功率 30-50%，或 10+ 调用无发现 | 继续，消耗 budget |
| **Circling** | Jaccard > 0.8 ×3，或 tool 成功率 <30% | 再给 1 次 pivot 机会；≥2 次 → 停止 |
| **Stuck** | 显式声明 stuck，或 100+ 消息无进展 | 停止 |

### 关键设计

- **Advancing 免费**：有价值的进展值得更多时间。agent 可以超出 `max_continuations × max_turns` 的硬上限。
- **Circling 限制**：`consecutive_circling` 独立于 `continuation`。Advancing/Creeping 都重置它。
- **6 级信号检测**：碰撞检测 → 显式声明 → 工具成功率 → Jaccard 重叠 → 长会话检查 → 默认 Advancing

### 新增组件

| 组件 | 作用 |
|------|------|
| `ProgressLevel` | 五级梯度枚举 |
| `evaluate_progress_level()` | 多级信号检测函数 |
| `count_tool_results()` | 工具成功率统计 |

---

## 第三层：Approach Tracking — 跨轮方法记忆

### 问题

每次 auto-continue 后 `messages = [system(summary)]`，轮次之间的上下文是孤立的。Round 2 的 LLM 不知道 Round 1 的 continuation context 说了什么。可能出现：

- Round 1: "avoid find, use ripgrep"
- Round 2: 又用 find → 重复失败

### 方案

维护 `continuation_history: Vec<ContinuationRecord>`（保留最近 3 轮），每次构建新 context 时注入历史：

```
[Prior Rounds]
  Round 1: I'll use recursive grep filtered by *.rs
    ✅ effective: grep (3/3 calls), cat (1/1 call)
    ❌ ineffective: find (0/2 calls)
    💡 lesson: Progress is good, but avoid: find
  Round 2: I'll switch to ripgrep and narrow scope
    ✅ effective: rg (4/4 calls)
    🔍 [rg] matched 12 files with TODO markers

[Continuation Context — Round 3/5]
...
💡 LESSON:
  You've tried 2 different approaches: grep, ripgrep.
  Across all rounds, consistently avoid: find.
```

### 跨轮信号

`generate_lesson()` 利用历史记录生成：

- **全局无效工具累积**：跨轮汇总所有失败工具 → "across all rounds, consistently avoid: find"
- **已尝试方法统计**：`distinct_approaches` → "You've tried 3 different approaches: grep, ripgrep, fd"
- **轮次感知建议**：Round 3+ 且多种方法都试过 → "Running out of rounds. You've tried N approaches..."

### 新增组件

| 组件 | 作用 |
|------|------|
| `ContinuationRecord` | 单轮次记录（approach, effective_tools, ineffective_tools, key_findings, lesson） |
| `continuation_history: Vec<ContinuationRecord>` | 引擎内保留最近 3 轮 |
| `prior_rounds` 字段 | ContinuationContext 携带历史 |
| `render()` Prior Rounds 渲染 | 可视化历史 |
| `generate_lesson()` 跨轮逻辑 | 全局无效工具 + 方法统计 |

---

## 完整数据流

```
User message
  ↓
LlmCognitiveEngine::process()
  ├─ max_turns 到达
  │   ↓
  │   evaluate_progress_level(messages)
  │   ├── Achieved/Stuck → 停止
  │   ├── Circling → consecutive_circling++
  │   │   ≥2 → 停止
  │   │   =1 → 继续, consume budget
  │   ├── Creeping → 继续, consume budget
  │   └── Advancing → 继续, FREE
  │       ↓
  │   build_continuation_context(messages, round, max_rounds, prior_rounds)
  │   ├── 分析工具成败 → EffectivePattern / IneffectivePattern
  │   ├── 提取方法描述 → approach_description
  │   ├── 检测未完成项 → unresolved_items
  │   ├── 生成引导 → generate_lesson(prior_rounds)
  │   │   ├── 全局无效工具累积
  │   │   ├── 已尝试方法统计
  │   │   └── 轮次感知建议
  │   └── 渲染 + 注入 prior_rounds
  │       ↓
  │   continuation_history.push(ContinuationRecord)
  │   messages = [system(summary)]
  │   continue
  │
  ├─ LLM call → response
  ├─ tool_calls → execute → feed back → turn++
  └─ text → break → final reply
```

---

## 实现验证

```
cargo check -p cognitive-llm     ✅ 零 warning
cargo test -p cognitive-llm      ✅ 40/40 通过
cargo check -p gateway           ✅ 通过
```

文件改动：`cognitive/llm/src/lib.rs`，约 +350 行净增。
