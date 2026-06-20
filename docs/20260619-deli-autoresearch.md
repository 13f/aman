# Deli AutoResearch SKILL → aman 差距分析

> 2026-06-19 | 基于 https://victorchen96.github.io/auto_research/framework.html
> 2026-06-19 修订 | 复查 aman 实现后更正 #2, #4, #5

## 背景

Deli Chen（DeepSeek）发布的协议框架，一份 SKILL.md，定义了让 LLM agent 在天到周级别自主运行的工程规范。不包含可执行代码，全部是行为约束和状态管理约定。

核心解决三个长跑 agent 的系统性故障模式：
1. **认知循环** — 反复尝试相似方向，卡在局部最优
2. **停滞** — agent 做完工作后等待反馈，表面活跃实则停工
3. **运行时脆弱性** — 上下文压缩打断循环，会话关闭杀掉寄生定时器

---

## aman 现有相关架构

| 模块 | 路径 | 关键能力 |
|------|------|---------|
| Agent Harness | `kernel/gateway/src/runtime/agent_harness.rs` | ReAct loop 编排、max_turns 强制、auto-continue（最多 5 次）、progress 评估驱动续跑决策、continuation context 构建 |
| Workflow Engine | `kernel/workflow` | 状态机、StateTimeout、ERROR recovery、retry（`TOKEN_RETRY`） |
| Eval Engine | `kernel/eval` | session_progress 评估（collision_found、best_partial_match、looks_stuck、unique_tools） |
| Idle System | `kernel/idle` | 空闲检测（Daze→Boredom→Sleep→Exploration）、BoredomActor（加权随机 skill 选择） |
| Context Manager | `kernel/context-manager` | token 预算、压缩、RotDetector（重复工具调用/离题/幻觉/错误循环检测） |
| Persistence | `kernel/persistence` | WAL、StateStore、DLQ |
| Lifecycle | `kernel/lifecycle` | agent 阶段机（0→5 启动，5→0 关闭） |
| Cognitive Engine | `cognitive/llm` | `CognitiveEngine::process()` — 单轮 observations→decisions，ReActContext 携带 max_turns |

---

## 复查结论：3 个确认差距 + 2 个已部分覆盖

### Gap #1: 方向多样性强制 ✅ 确认缺失

**Deli 做法**：`directions_tried.json` 追踪所有已尝试方向，每次新迭代必须与历史不同。stale_count ≥ 2 → 强制改变结构性约束。

**aman 实际**：全代码库搜索 `direction.*tried`、`DirectionTracker`、`direction_diversity` — **零命中**。auto-continue 机制重置 turn 计数器后继续同一方向，不检查「是否已经试过类似做法」。

**差距**：Deli 框架最独特的贡献——防止认知循环的核心机制。aman 完全缺失。

**可能位置**：新模块 `kernel/direction`，或集成到 `kernel/eval`。

```
核心概念：
- Direction 表示（prompt 哈希 / 参数组合 / 策略标签）
- 差异判定（Jaccard / embedding 距离 / 结构性约束变化）
- 与 auto-continue 联动的 forced_pivot 逻辑
```

**优先级：P0** — 核心防御机制，无等价物

---

### Gap #2: 跨 session 累积进展追踪 ⚠️ 已有 per-session 评估，缺跨 session 累积

**Deli 做法**：`progress.json` 有 `total_findings` 和 `stale_count`。每次迭代零新发现 → stale_count+1。≥2 → 强制转向，≥4 → 标记人类。

**aman 实际**（复查修正）：

- ✅ `eval::session_progress` **并非只有 `looks_stuck`**。它实际输出：
  - `collision_found: bool` — 检测 "COLLISION FOUND" 等成功标志
  - `best_partial_match: u32` — 提取 "best_match=3/4" 等部分匹配计数
  - `looks_stuck: bool` — Jaccard 词重叠 > 0.8 或 100+ 条消息无进展
  - `unique_tools: Vec<String>` — 已使用的工具多样性
- ✅ **被 agent harness 实际使用** — `react_loop()` 在 `max_turns` 触发时调用 `session_progress::evaluate()`，根据 `collision_found` 和 `looks_stuck` 决定是否 auto-continue
- ❌ **只在 per-session 内运作** — 每次 `react_loop()` 调用是独立的，没有跨 session 累积计数器
- ❌ **没有 `stale_count`** — 不知道「这是第几次连续零进展」
- ❌ **没有 `total_findings`** — 不知道总产出量

**差距**：aman 的 per-session 评估比 Deli 更丰富（5 种信号 vs 1 个计数器），但缺少**跨 session 累积**能力。Deli 的 orchestrator 可以跨多次 work session 追踪 `stale_count`，aman 的 auto-continue 只在单次 `react_loop` 内判断（最多 5 次续跑），结束后状态不跨 session 持久化。

**可能位置**：扩展 `kernel/eval/src/session_progress.rs`，或在 `agent_harness` 层引入跨 session 的 `TaskProgressTracker`。

```
// 跨 session 持久化的进展状态（缺失）
struct TaskProgress {
    task_id: String,
    stale_count: u32,          // 连续零进展 session 数
    total_findings: u32,       // 累计有效产出
    last_evaluated_session: String,
}
```

**优先级：P1**（从 P0 下调）— per-session 评估已有且被使用，补跨 session 累积是增量改进

---

### Gap #3: Guardian/Worker 分离协议 ✅ 确认缺失

**Deli 做法**：心跳 patrol 对非自己的任务只能做三件事：liveness-check、restart、nudge。不能读数据、改状态、代为汇报。

**aman 实际**：`IdleDetector` + `BoredomActor` 可以触发任何带 `idle_run` 标签的 skill。全代码库搜索 `guardian`、`GuardianProtocol`、`TOKEN_NUDGE` — **零命中**。idle 系统没有「这是别人的任务，我不能碰」的边界概念。

**差距**：没有 protocol 层约束 guardian 进程的权限范围。

**可能位置**：`kernel/idle/src/types.rs` 或 `coordination.rs`。

```
enum GuardianAction {
    LivenessCheck,
    Restart,
    Nudge,
    // 明确禁止：ReadWorkerData, ModifyWorkerState, ReportForWorker
}
```

**优先级：P1** — 有现有模块，但需要改变 idle 行为语义

---

### Gap #4: 时间上限 ❌ 大幅修正——aman 的 auto-continue 比 Deli 更完善

**Deli 做法**：单次 work session 最多 15 轮或 30 分钟。

**aman 实际**（复查修正——之前理解严重不完整）：

`agent_harness.rs::react_loop()`（第 2437 行起）实现了一套远比 Deli 复杂的机制：

- ✅ **`max_turns` 强制** — `ctx.turn >= ctx.max_turns` 时触发（line 2454）
- ✅ **进度评估驱动决策** — 触发时调用 `session_progress::evaluate()`，检查 `collision_found`、`best_partial_match`、`looks_stuck`、`unique_tools`（line 2486-2507）
- ✅ **Auto-continue** — background 模式下，如果 `!collision_found && !looks_stuck`，自动续跑最多 5 次（`MAX_CONTINUATIONS = 5`），每次续跑重置 `ctx.turn = 0`（line 2511-2575）
- ✅ **Continuation context** — `build_continuation_context()` 将完整 history 压缩为摘要（用户消息、assistant 回复、工具调用统计、关键发现），在新轮次中注入（line 2844）
- ✅ **事件发布** — auto-continue 和 auto-continue-stopped 都有对应事件，可供监控（line 2530-2573）
- ✅ **用户续跑** — `ContinuationMode::Continue` 支持用户在 max_turns 后发送 `/continue`（line 1332-1343）

**aman 缺失的**：
- ❌ **Wall-clock 时间上限** — 全代码库搜索 `max_duration`、`session_duration`、`max_runtime`、`time_budget` — 零命中（仅 cron 有 `WallClock` 作为触发模式，不是 session cap）
- 但这是否真的需要？auto-continue 已有 5 次上限 × max_turns 轮次上限，间接限制了运行时长

**结论**：这个差距基本不成立。aman 的 auto-continue + progress evaluation + continuation context 组合已经实现了 Deli「短 session + 评估 + 决定是否继续」的意图，且实现更完善。唯一可能补充的是显式的 wall-clock 上限。

**优先级：P3（降级）** — aman 实现比 Deli 更完善；wall-clock cap 是锦上添花

---

### Gap #5: Nudge vs Retry ⚠️ aman 的 auto-continue 本身就是 nudge 机制

**Deli 做法**：「nudge」是向停滞 agent 注入 task_spec + progress，在新 session 中继续。「retry」是从失败状态重试。

**aman 实际**（复查修正）：

- ✅ **Auto-continue = nudge** — `react_loop` 的 auto-continue 分支：压缩 history → 重置 turn 计数器 → 注入 continuation context → 继续 loop。这不是重试，而是「整理上下文后继续」
- ✅ **ContinuationContext** — `build_continuation_context()` 提取用户消息、assistant 回复摘要、工具调用统计、关键发现（"COLLISION FOUND" / error markers / exit codes），这正是 Deli 说的「注入 task_spec + progress」
- ✅ **User-driven continue** — `ContinuationMode::Continue` 是显式的用户侧 nudge 路径
- ❌ **In-session nudge，不是 fresh-session nudge** — Deli 的 nudge 是**新 session**（新进程/新上下文），aman 的 auto-continue 是**同一 session 内**的上下文整理后继续。Deli 的 fresh-session 方式避免了上下文累积导致的认知循环，aman 的 continuation context 压缩可以缓解但不能完全消除
- ✅ **Retry 已有** — `kernel/workflow` 的 `TOKEN_RETRY` → `LastActiveState` + `ErrorRecovery`

**差距**：不是「缺少 nudge 概念」，而是 nudge 的实现方式不同。aman 在同 session 内通过压缩上下文实现；Deli 通过 fresh session + 文件注入实现。Deli 的方式对防止认知循环更强（彻底清空上下文），但 aman 的 continuation context 压缩也是合理折中。

**优先级：P3（降级）** — aman 已有 nudge 等价物；fresh-session 变体是架构偏好差异，非功能缺失

---

## 不适合入 aman 基础设施层的内容

以下 Deli 概念更适合在 Hermes 技能层实现，不入 Rust 核心：

| 概念 | 原因 |
|------|------|
| 三层 watchdog（L0/L1/L2） | 进程监控是部署层关注点，不是 workflow engine 的职责 |
| 状态文件布局约定 | 文件格式是技能层的 convention，persistence 提供原语即可 |
| Zero-interaction 模式 | 行为约束，适合作为 skill flag 或 agent persona rule |
| Ready-means-execute | 同样是行为约定，不是引擎能力 |
| 分离执行与评估 | aman 的 `eval` crate 已架构分离；agent harness 已经在实践中分离（evaluate 在 max_turns 边界，不干扰执行循环） |

---

## 修正后优先级总结

| # | 差距 | 优先级 | 原优先级 | 变化 | 备注 |
|---|------|--------|---------|------|------|
| 1 | 方向多样性 | **P0** | P0 | — | 核心防御机制，完全缺失 |
| 2 | 跨 session 累积进展 | **P1** | P0 | ↓ | per-session 评估已有且丰富；缺跨 session stalen_count |
| 3 | Guardian 协议 | P1 | P1 | — | idle 安全边界 |
| 4 | 时间上限 | **P3** | P1 | ↓↓ | aman auto-continue 比 Deli 更完善；wall-clock cap 锦上添花 |
| 5 | Fresh-session nudge | **P3** | P2 | ↓ | aman auto-continue + continuation context 已是 nudge 等价物 |

**现在真正需要关注的就两个**：方向多样性（P0）和跨 session 进展追踪（P1）。另外三个要么 aman 已有等价实现，要么是架构偏好差异。

---

autoresearch技能有提到怎么对结果进行评估验证/evaluate/validate吗？

有的，分两层：
    
    
    
    第一层：机械验证（迭代间自动执行）
    
    SKILL.md §9 Engineering Constraints：
    
    > "Validation (test / compile / check) must run between iterations."
    > "Citation-like content is verified every 20 entries, never batched up."
    
    这不是 LLM 评估——就是跑测试/编译/检查。引用类内容每 20 条抽样验证一次，不攒到最后。
    
    
    
    第二层：独立 subagent 审计（Pattern D）
    
    §8 Subagent Scheduling Patterns：
    
    > Pattern D – Verification: "Post-iteration QA. An independent subagent audits the evidence chain of findings."
    
    每次迭代结束后，一个独立的 verification subagent 审计 findings 的证据链。它不和 work agent 共享上下文，只看 findings.jsonl 和原始输出。
    
    
    
    第三层（论文场景特有）：多人格模拟评审
    
    §10 Validation & Limits：
    
    > "Scores come from in-framework multi-persona simulated review; comparable only longitudinally within the same protocol, not an external quality claim."
    
    论文质量评分（8.0-8.6/10）是用多个 reviewer persona（不同偏好、不同严格度）模拟同行评审。Deli 明确说分数只能在同协议内纵向比较，不是外部质量声明。
    
    
    
    关键约束：执行和评估严格分离
    
    三层验证的共同原则：做事的 agent 不评估自己的产出。机械验证是自动化脚本，subagent 审计是独立进程，模拟评审是不同的 persona——work agent 永远不是自己的裁判。
    
    
    
    局限（Deli 自己承认的）
    
    > "Fabricated citations and data artifacts originate from the LLM itself; the framework makes external checking a mechanical step in the process, it does not remove the error source."
    
    评估不消除幻觉源头，只让检查流程化。
    
    
    对比 aman：aman 的 eval crate 有 llm_judge 策略（LLM-as-Judge）和规则评估，但没有「独立 subagent 审计 findings 证据链」的概念，也没有「多人格模拟评审」。这两个是 Deli 验证层 aman 直接可参考的模式。
