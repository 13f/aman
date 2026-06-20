# Deli AutoResearch SKILL → aman 差距分析与整合建议

> 2026-06-19 | 基于 https://victorchen96.github.io/auto_research/framework.html
> 2026-06-20 修订 | 合并差距分析与整合路径，修正 aman 能力评估
> 2026-06-20 三修 | CronManager 已移除，CronSource 简化为 EventSource 走 SourceRegistry 管理；同步更新整合路径
> 2026-06-20 四修 | Planner tool 已实现（`kernel/tool/src/planner.rs`），覆盖 Gap #1（方向多样性）和 Gap #2（跨 session 进展追踪）
> 2026-06-20 五修 | Cron 配置持久化已实现（`kernel/source/src/cron_store.rs`），`~/.aman/agents/{agent_key}/cron/jobs.json`，重启后自动恢复
> 2026-06-20 六修 | `spawn_anonymous` + `delegate_task` tool 已实现：匿名 agent 基础设施 + LLM 可调用的子 agent 派生 tool。SubAgentSpawner trait（`cognitive/llm`）解耦 cognitive 层与 gateway 实现。覆盖第二阶段 SubTaskScheduler 的 GoalDriven / ParallelExploration / PostIterationVerify 三种模式。

---

## 一、背景

**Deli AutoResearch**（Deli Chen / DeepSeek）发布的协议框架，一份 SKILL.md，定义了让 LLM agent 在天到周级别自主运行的工程规范。不包含可执行代码，全部是行为约束和状态管理约定。

### 核心解决的三个系统性故障

| 故障模式 | 表现 | 框架对策 |
|---|---|---|
| **认知循环** | Agent 反复尝试相似方向，卡在局部最优 | 方向多样性强制、stale_count 检测后强制转向 |
| **静默停滞** | 工作停止但会话看起来仍在运行 | 三层心跳看门狗（L0 shell → L1 cron → L2 业务循环） |
| **运行时脆弱** | 上下文压缩打断循环，会话关闭杀掉寄生定时器 | 新鲜会话策略：每次迭代启动新 session，状态全部持久化到文件 |

### Deli 组件速览

| # | 组件 | 说明 |
|---|------|------|
| 1 | 零交互策略 | Agent 不得提问、用 Plan Mode、或请求确认 |
| 2 | 状态文件持久化 | `state/` 下 task_spec.md, progress.json, findings.jsonl, directions_tried.json |
| 3 | 停滞检测 | stale_count ≥ 2 强制 pivot，≥ 4 标记需人工介入 |
| 4 | 方向多样性 | 新方向必须与所有已尝试路径有实质差异 |
| 5 | 三层心跳 | L0 shell guard → L1 持久化 cron → L2 业务循环 |
| 6 | Guardian/Worker 分离 | 心跳 agent 只能做 liveness-check、restart、nudge |
| 7 | 子 Agent 调度 | 四种模式：GoalDriven / ParallelExploration / Polling / PostVerify |
| 8 | 工程约束 | 15 轮或 30 分钟上限、300 行/文件、每 20 条引用验证 |
| 9 | 编排器循环 | 监控 state → 检测停滞 → 注入新方向 → 启动 subagent |

### 三层验证机制（Deli 特有）

| 层 | 方式 | 说明 |
|----|------|------|
| **第一层：机械验证** | 编译/测试/检查 | 每次迭代间自动执行，引用每 20 条抽样验证 |
| **第二层：独立 subagent 审计** | Pattern D | 独立 agent 审计 findings 证据链，不和 worker 共享上下文 |
| **第三层：多人格模拟评审** | 论文特化 | 多个 reviewer persona（不同偏好/严格度）模拟同行评审 |

核心原则：**执行和评估严格分离**——做事的 agent 不评估自己的产出。

> **局限（Deli 自认）**：幻觉和数据伪造源于 LLM 本身，框架只让检查流程化，不消除错误源头。

---

## 二、Aman 现有相关架构

| 模块 | 路径 | 关键能力 |
|------|------|---------|
| Agent Harness | `kernel/gateway/src/runtime/agent_harness.rs` | ReAct loop、max_turns=64、**auto-continue**（最多 5 次）、**progress 评估驱动续跑决策**、continuation context 构建 |
| Eval Engine | `kernel/eval` | `session_progress` 评估（collision_found、best_partial_match、**looks_stuck**、unique_tools） |
| Workflow Engine | `kernel/workflow` | FSM、StateTimeout、ERROR recovery、retry（TOKEN_RETRY）、StateStore trait |
| Idle System | `kernel/idle` | 7 级深度空闲检测（Daze→Boredom→Sleep→Exploration…）、BoredomActor 主动建议 |
| Context Manager | `kernel/context-manager` | token 预算、压缩、**RotDetector**（重复工具调用/离题/幻觉/错误循环检测） |
| Persistence | `kernel/persistence` | WAL、StateStore、DLQ、PersistentBus |
| Session | `kernel/gateway/src/runtime/session/` | SessionStore（SQLite + JSONL）、确定性 session ID、跨重启"断点续传" |
| Cognitive Engine | `cognitive/llm` | `CognitiveEngine::process()`、ReActContext（max_turns + token budget + interrupt） |
| Dispatcher | `kernel/dispatcher` | 事件路由：Type/Source/Priority → Pipeline/Skill/Workflow/Hook/FanOut |
| Event Bus | `kernel/event-bus` | 5 级背压、Dedup（Bloom + LRU）、Persistent 模式 |
| Agent Message | `kernel/core/src/agent.rs` | TaskDelegation、ResultSharing、StatusQuery（agent 间通信） |

---

## 三、差距分析（复查后修正）

> **关键修正**：复查 `agent_harness.rs` 和 `kernel/eval` 后，发现 aman 的 auto-continue 机制远比最初估计完善。#4（时间上限）和 #5（nudge）实际上已被 aman 覆盖，从主要差距降级。

---

### Gap #1: 方向多样性强制 ✅ **P0 — 已由 Planner Tool 覆盖 (2026-06-20)**

**Deli 做法**：`directions_tried.json` 追踪所有已尝试方向。stale_count ≥ 2 → 强制改变结构性约束（不仅是参数微调）。新方向必须与历史不同。

**aman 实际**：全代码库搜索 `direction.*tried`、`DirectionTracker`、`direction_diversity` — **零命中**。auto-continue 重置 turn 计数器后在同一方向上继续，不检查"是否已经试过类似做法"。

**差距**：Deli 框架最独特的贡献——防止认知循环的核心机制。aman 的 `RotDetector`（`kernel/context-manager/src/rot.rs`）能捕捉部分循环模式：
- `RepeatedToolCall` — 同工具+同参数重复调用（≥3 次 warning，≥5 次 critical）
- `ToolErrorLoop` — 连续工具调用失败
- `OffTopicDrift` — 回复偏离任务主题
- `Contradiction` — 输出与之前矛盾

但这些是**单 session 内的信号检测**，不等同于方向多样性——`RotDetector` 告诉你"出问题了"，但不提供"换个方向试试"的机制。

> **2026-06-20 已解决**：`PlannerTool`（`kernel/tool/src/planner.rs`）的 `record_direction` 操作在 task 粒度上记录所有已尝试方向（`{id, description, parameters}`），planner status/resume 输出完整的方向历史。stale_count 由 `increment_stale` 累积、`complete` 归零。Direction diversity 的判断逻辑（读取历史→确保新方向有差异）由 agent/orchestrator 调用 planner 完成，不在 planner 内部实现。

```
核心概念：
- Direction 表示（prompt 策略标签 + 参数组合哈希）
- 差异判定（Jaccard 快判 + embedding 精判）
- 与 auto-continue + TaskProgressTracker 联动的 forced_pivot 逻辑
- 与 RotDetector 互补：RotDetector 发现循环 → DirectionTracker 提供替代方向
```

---

### Gap #2: 跨 session 累积进展追踪 ⚠️ **P1 — 已由 Planner Tool 覆盖 (2026-06-20)**

**Deli 做法**：`progress.json` 有 `total_findings` 和 `stale_count`。每次迭代零新发现 → stale_count+1。≥2 → 强制转向，≥4 → 标记人类。

**aman 实际**（复查修正——此前理解不完整）：

- ✅ `eval::session_progress` **并非只有 `looks_stuck`**。实际输出：
  - `collision_found: bool` — 检测 "COLLISION FOUND" 等成功标志
  - `best_partial_match: u32` — 提取 "best_match=3/4" 等部分匹配计数
  - `looks_stuck: bool` — Jaccard 词重叠 > 0.8 或 100+ 条消息无进展
  - `unique_tools: Vec<String>` — 已使用的工具多样性
- ✅ **被 agent harness 实际使用** — `react_loop()` 在 `max_turns` 触发时调用 `session_progress::evaluate()`，根据 `collision_found` 和 `looks_stuck` 决定是否 auto-continue
- ❌ **只在 per-session 内运作** — 每次 `react_loop()` 调用是独立的，没有跨 session 累积计数器
- ❌ **没有 `stale_count`** — 不知道「这是第几次连续零进展」
- ❌ **没有 `total_findings`** — 不知道总产出量

**差距**：aman 的 per-session 评估比 Deli 更丰富（5 种信号 vs 1 个计数器），但缺少**跨 session 累积**能力。

> **2026-06-20 已解决**：`PlannerTool` 的 `.progress` 文件（`~/.aman/plans/{plan_id}.progress`）提供跨 session 持久化的进展状态：`iteration`、`stale_count`、`current_task_id`、`current_milestone_id`、`current_direction_id`、`last_progress_at`、`last_session_id`、`retry_counts`。`planner.resume` 操作在 session 重启时恢复状态并返回 next_task。不再需要单独的 `TaskProgressTracker` crate。

```rust
// 跨 session 持久化的进展状态（缺失）
struct TaskProgress {
    task_id: String,
    stale_count: u32,          // 连续零进展 session 数
    total_findings: u32,       // 累计有效产出
    last_evaluated_session: String,
}
```

---

### Gap #3: Guardian/Worker 分离协议 ⚠️ **P1 — idle 系统无任务边界概念**

**Deli 做法**：心跳 patrol 对非自己的任务只能做三件事：liveness-check、restart、nudge。不能读数据、改状态、代为汇报。

**aman 实际**：`IdleDetector` + `BoredomActor` 可以触发任何带 `idle_run` 标签的 skill。全代码库搜索 `guardian`、`GuardianProtocol`、`TOKEN_NUDGE` — **零命中**。idle 系统没有「这是别人的任务，我不能碰」的边界概念。

**差距**：没有 protocol 层约束 guardian 进程的权限范围。

**可能位置**：`kernel/idle/src/types.rs` 或 `coordination.rs`。

```rust
enum GuardianAction {
    LivenessCheck,
    Restart,
    Nudge,
    // 明确禁止：ReadWorkerData, ModifyWorkerState, ReportForWorker
}
```

---

### Gap #4: 时间上限 ~~差距不成立~~ **P3 — aman auto-continue 比 Deli 更完善**

**Deli 做法**：单次 work session 最多 15 轮或 30 分钟。

**aman 实际**（复查修正——此前理解严重不完整）：

`agent_harness.rs::react_loop()` 实现了一套远比 Deli 复杂的机制：

- ✅ **`max_turns` 强制** — `ctx.turn >= ctx.max_turns` 时触发
- ✅ **进度评估驱动决策** — 触发时调用 `session_progress::evaluate()`，检查 5 种信号
- ✅ **Auto-continue** — background 模式下，如果 `!collision_found && !looks_stuck`，自动续跑最多 5 次（`MAX_CONTINUATIONS = 5`），每次续跑重置 `ctx.turn = 0`
- ✅ **Continuation context** — `build_continuation_context()` 将完整 history 压缩为摘要（用户消息、assistant 回复、工具调用统计、关键发现），在新轮次中注入
- ✅ **事件发布** — auto-continue 和 auto-continue-stopped 都有对应事件，可供监控
- ✅ **用户续跑** — `ContinuationMode::Continue` 支持用户在 max_turns 后发送 `/continue`
- ❌ **Wall-clock 时间上限** — 全代码库搜索 `max_duration`、`session_duration`、`time_budget` — 零命中

**结论**：这个差距基本不成立。aman 的 auto-continue + progress evaluation + continuation context 组合已经实现了 Deli「短 session + 评估 + 决定是否继续」的意图，且实现更完善。唯一可能补充的是显式的 wall-clock 上限，但 auto-continue 已有 5 次上限 × max_turns 轮次上限，间接限制了运行时长。

---

### Gap #5: Nudge vs Retry ~~差距不成立~~ **P3 — aman auto-continue 本身就是 nudge 机制**

**Deli 做法**：「nudge」是向停滞 agent 注入 task_spec + progress，在新 session 中继续。「retry」是从失败状态重试。

**aman 实际**（复查修正）：

- ✅ **Auto-continue = nudge** — `react_loop` 的 auto-continue 分支：压缩 history → 重置 turn 计数器 → 注入 continuation context → 继续 loop。这不是重试，而是「整理上下文后继续」
- ✅ **ContinuationContext** — `build_continuation_context()` 提取用户消息、assistant 回复摘要、工具调用统计、关键发现，这正是 Deli 说的「注入 task_spec + progress」
- ✅ **User-driven continue** — `ContinuationMode::Continue` 是显式的用户侧 nudge 路径
- ❌ **In-session nudge，不是 fresh-session nudge** — Deli 的 nudge 是**新 session**（新进程/新上下文），aman 的 auto-continue 是**同一 session 内**的上下文整理。Deli 的方式对防止认知循环更强（彻底清空上下文），但 aman 的 continuation context 压缩可以缓解（不能完全消除）
- ✅ **Retry 已有** — `kernel/workflow` 的 `TOKEN_RETRY` → `LastActiveState` + `ErrorRecovery`

**差距**：不是「缺少 nudge 概念」，而是 nudge 的实现方式不同。aman 在同 session 内通过压缩上下文实现；Deli 通过 fresh session + 文件注入实现。这是架构偏好差异，非功能缺失。

---

### 验证层对比

Deli 的三层验证体系是其与 aman 差异最大的领域之一：

| 验证方式 | Deli | aman | 差距 |
|---------|------|------|------|
| 机械验证（编译/测试/检查） | 迭代间自动执行 | `eval` crate 的规则评估 | aman 偏 LLM 评估，缺少自动化脚本执行 |
| 独立 subagent 审计（Pattern D） | 独立 agent 审计 findings 证据链 | ❌ 缺失 | eval 是 LLM-as-Judge（同进程内评估），不是独立 agent |
| 多人格模拟评审 | 多个 reviewer persona 同行评审 | ❌ 缺失 | Deli 论文场景特有；aman 可泛化为多视角 code review |
| LLM-as-Judge | — | ✅ `eval` 的 `llm_judge` 策略 | — |
| 执行/评估分离 | Worker 不评估自己产出 | ✅ eval crate 架构已分离 | aman 分离在 crate 级别；Deli 分离在进程/session 级别 |

**Pattern D（独立 subagent 审计）深度分析**：

Deli 的 Pattern D 关键设计：
- 审计 agent **不和 worker 共享上下文**——只看 `findings.jsonl` 和原始输出
- 审计标准是**证据链完整性**，不是结果正确性（后者 LLM 无法保证）
- 这是 Deli 三层验证中唯一可直接泛化到非论文场景的模式

在 aman 中实现 Pattern D 的路径：
1. 复用 `AgentMessage::TaskDelegation` 将 findings 发送给独立 agent
2. 该 agent 使用**不同的 session**（新鲜上下文），只接收 findings 摘要
3. 审计结果写回 `TaskProgressTracker`，标记每个 finding 的 verification 状态
4. `Orchestrator` 根据 verification 结果决定是否接受该迭代的产出

这与 aman 现有的 eval 互补：eval 做快速启发式评估（per-session），Pattern D 做深度审计（跨 session）。

---

**优先级总结**：5 个 Gap 中，真正需要关注的就两个——**方向多样性（P0）** 完全缺失，**跨 session 进展追踪（P1）** 是增量改进。其余三个（时间上限、nudge、Guardian 协议）要么 aman 已有更完善的等价实现，要么属于架构偏好差异。

---

## 五、不适合入 Aman 内核层的内容

以下 Deli 概念更适合在 Hermes 技能层或部署层实现，不入 Rust 核心：

| 概念 | 原因 |
|------|------|
| 三层 watchdog 中的 L0（OS 看门狗） | 平台相关（launchd/systemd），属于部署工具链而非框架内核 |
| 状态文件布局约定（`state/` 下各文件） | 文件格式是技能层的 convention，persistence 提供 StateStore 原语即可。aman 用 `TaskProgressTracker`（StateStore 持久化）替代平文件 |
| Zero-interaction 模式 | 行为约束，适合作为 skill flag 或 agent persona rule |
| Ready-means-execute | 同样是行为约定，不是引擎能力 |
| 工程约束（行数/文件数限制） | 太特化于论文写作场景，应放在 skill 层实现 |

> **关于 watchdog**：L2（业务循环心跳）aman 已有 `TimerSource` → `EventType::Heartbeat`；L1（持久化 cron）`CronSource` 已简化为普通 `EventSource` 走 `SourceRegistry` 管理，跨重启配置持久化见 §六·3。只有 L0 是纯部署层关注点。

---

## 六、推荐整合路径

> **2026-06-20 更新**：第一阶段（P0 + P1）的核心能力已通过 `PlannerTool`（`kernel/tool/src/planner.rs`）实现。DirectionTracker 和 TaskProgressTracker 不再需要作为独立 crate——planner tool 统一管理方向记录、进展追踪和跨 session 状态持久化。以下原始方案保留作为设计参考。

### 第一阶段：P0 + P1 基础能力 ✅ 已通过 Planner Tool 实现

#### 1. DirectionTracker → `planner.record_direction` ✅

原本计划新增 `kernel/direction/` crate。现在由 planner tool 的 `record_direction` 操作替代：
- `directions_tried` 存储在 `.plan` 文件的每个 task 上（`{id, description, parameters}`）
- `stale_count` 通过 `planner.increment_stale` 累积、`planner.complete` 归零
- Direction diversity 判断由 agent/orchestrator 读取 planner 数据后执行

#### 2. TaskProgressTracker → `planner.{status,resume}` ✅

原本计划新增 `kernel/persistence/src/task_progress.rs`。现在由 planner tool 的 `.progress` 文件替代：
- `iteration`、`stale_count`、`current_task_id`、`current_milestone_id`、`current_direction_id`、`last_progress_at`、`last_session_id`、`retry_counts`
- 跨 session 恢复通过 `planner.resume` → 返回 next_task 和完整状态
- 文件位于 `~/.aman/plans/{plan_id}.progress`

#### 原始方案（保留作为设计参考）

<details>
<summary>原 DirectionTracker 设计</summary>

**新增** `kernel/direction/` crate，或集成到 `kernel/eval`。

```
DirectionTracker {
    tried: HashMap<DirectionHash, DirectionRecord>,
    similarity_threshold: f32,
}
```

</details>

<details>
<summary>原 TaskProgressTracker 设计</summary>

**新增** `kernel/persistence/src/task_progress.rs`，基于 `StateStore`。

```rust
struct TaskProgress {
    task_id: String,
    stale_count: u32,
    total_findings: u32,
    ...
}
```

</details>

#### 3. Cron 配置持久化（L1 心跳基础）—— 已完成 ✅

> **2026-06-20 已实施**：`CronManager` 已移除，`CronSource` 精简为 ~210 行纯 `EventSource`，直接通过 `SourceRegistry` 管理生命周期。调度由 `SourceRegistry` 的后台 `poll_loop` 统一驱动。
>
> **2026-06-20 已实施（五修）**：Cron 配置持久化已实现。新增 `CronStore`（`kernel/source/src/cron_store.rs`），以 `~/.aman/agents/{agent_key}/cron/jobs.json` 存储每个 agent 的定时任务配置（参考 Hermes 的 `~/.hermes/cron/jobs.json` 格式）。`add_cron_job`/`update_cron_job`/`remove_cron_job` 自动同步到磁盘；`Phase 4` 启动时从所有 agent 的 `jobs.json` 恢复 cron source 并注册到 `SourceRegistry`。接口层（gRPC/HTTP/stdio/CLI/tool dispatch）均增加 `agent_key` 参数。

### 第二阶段：编排与调度（部分完成）

#### 4. SubTaskScheduler — 子任务调度模式 ✅ 核心原语已实现

> **2026-06-20 六修**：核心理念已通过 `spawn_anonymous` + `delegate_task` tool 实现，
> 但实现方式不同于原始设计。原始设计将模式定义为 `WorkflowDef`，实际实现采用
> 更轻量的匿名 agent + tool 层方案。

**已实现**（`cognitive/llm/src/delegate_task.rs` + `kernel/gateway/src/runtime/subagent_spawner.rs`）：

| 模式 | 实现方式 |
|------|---------|
| **GoalDriven** | `delegate_task(prompt="...", background=false)` → 同步等待结果 |
| **ParallelExploration** | 并行调用多个 `delegate_task(prompt="方向A/B/C", background=true)` → 各方向独立运行 |
| **PostIterationVerify** | `delegate_task(prompt="审计这些 findings...", system_prompt="You are a skeptical auditor...")` |

**架构**：
```
cognitive/llm/subagent.rs       SubAgentSpawner trait（认知层抽象）
cognitive/llm/delegate_task.rs  DelegateTaskTool（LLM tool，只依赖 trait）
gateway/runtime/subagent_spawner.rs  GatewaySubAgentSpawner（gateway 实现）
gateway/runtime/agent_harness.rs     AgentHarness::spawn_anonymous（底层原语）
```

> **2026-06-20 七修**：`collect_result` 已通过 `delegate_task(operation="collect")` 实现。
> `GatewaySubAgentSpawner` 内部维护 `pending_handles` HashMap（纯内存），`background=true` 时
> 存入 handle，`operation="collect"` 时取出并等待结果。

**已实现**：

| 模式 | 实现方式 |
|------|---------|
| **GoalDriven** | `delegate_task(prompt="...")` → 同步等待结果 |
| **ParallelExploration** | 并行 `delegate_task(prompt="方向A/B", background=true)` → 各方向独立 → `delegate_task(operation="collect", agent_id="...")` 逐个取回 |
| **PostIterationVerify** | `delegate_task(prompt="审计...", system_prompt="You are a skeptical auditor...")` |

**未实现**：
- **PollingExperiment** — 需要 Timer 轮询 + 编排逻辑（LLM 可通过 spawn→等待→collect 手动实现）

#### 5. Orchestrator — 跨迭代编排 ❌ 未开始

**新增** `kernel/workflow/src/orchestrator.rs`。

基于现有 `WorkflowDef` 的状态机：

```
ACTIVE → ITERATING ⇄ STALLED → PIVOTING → ITERATING
                  ↘ ESCALATED (需人工介入)
                  → COMPLETE
```

`OrchestratorAgent`：内置 agent（类似 idle 的 BoredomActor），负责：
- 创建/恢复 workflow 实例
- 每轮迭代通过 `delegate_task` tool 启动 subagent
- 监听 subagent 完成，调用 `session_progress::evaluate()` + Planner progress
- 检测停滞 → 调用 Planner pivot → 发布 `PIVOT` 事件
- 更新迭代指标

### 第三阶段：Guardian 协议（P1，按需）

#### 6. Guardian 协议约束

**修改** `kernel/idle/src/types.rs` 或新增 `kernel/idle/src/guardian.rs`。

- 定义 `GuardianAction` enum（LivenessCheck / Restart / Nudge），作为 idle 触发的动作边界
- `BoredomActor` 在触发非自身任务时，限制为仅 GuardianAction
- 与 `AgentMessage` 联动，nudge 通过 `AgentMessage` 发送而非直接操作目标 agent 状态

---

## 七、依赖关系（2026-06-20 六修后）

```
Planner tool (P0+P1) ✅  ──────┐
                                ├──→ Orchestrator (阶段二，未开始)
spawn_anonymous ✅  ────────────┤        ↑
delegate_task tool ✅  ─────────┤        │
SubAgentSpawner trait ✅  ──────┘        │
                                ├────────┘
Cron 配置持久化 ✅  ────────────┤
                                │
Guardian 协议 (P1) ─────────────┘ (第三阶段，独立)
```

---

## 八、涉及的关键文件

| 文件 | 变更 | 说明 |
|------|------|------|
| `kernel/direction/src/lib.rs` | **新增** | DirectionTracker（P0：方向多样性） |
| `kernel/persistence/src/task_progress.rs` | **新增** | TaskProgressTracker（P1：跨 session 进展） |
| `kernel/persistence/src/lib.rs` | 修改 | 加 `pub mod task_progress;` |
| `kernel/source/src/cron.rs` | **已完成** | CronSource 精简为 ~210 行纯 EventSource；CronManager 已移除；新增 `CronJobConfig`、`CronJobsFile` |
| `kernel/source/src/cron_store.rs` | **已完成** | `CronStore` — 读写 `~/.aman/agents/{agent_key}/cron/jobs.json`，atomic write |
| `kernel/source/src/lib.rs` | **已完成** | 导出 `CronStore`, `CronJobConfig`, `CronJobsFile` |
| `kernel/gateway/src/runtime/agent_runtime.rs` | **已完成** | cron 接口增加 `agent_key` 参数 + 持久化；Phase 4 恢复逻辑 |
| `kernel/gateway/src/runtime/grpc.rs` | **已完成** | `AddCronJobRequest` 等增加 `agent_key` 字段 |
| `kernel/gateway/src/runtime/http.rs` | **已完成** | cron handler 提取 `agent_key` |
| `kernel/gateway/src/runtime/stdio.rs` | **已完成** | cron handler 提取 `agent_key` |
| `kernel/cli/src/main.rs` | **已完成** | CLI `cron add/update/remove` 增加 `--agent-key` 选项 |
| `kernel/cli/src/grpc_client.rs` | **已完成** | gRPC client wrapper 增加 `agent_key` 参数 |
| `proto/aman.proto` | **已完成** | Request message 增加 `agent_key` 字段 |
| `cognitive/llm/src/subagent.rs` | **新增** | `SubAgentSpawner` trait + `SubAgentResult`（认知层抽象） |
| `cognitive/llm/src/delegate_task.rs` | **新增** | `DelegateTaskTool` — LLM 可调用的子 agent 派生 tool |
| `cognitive/llm/src/lib.rs` | 修改 | 加 `pub mod subagent;` `pub mod delegate_task;` |
| `kernel/gateway/src/runtime/subagent_spawner.rs` | **新增** | `GatewaySubAgentSpawner` — 实现 `SubAgentSpawner` trait |
| `kernel/gateway/src/runtime/agent_harness.rs` | **修改** | 新增 `spawn_anonymous`、`process_anonymous_message`、`AnonymousAgentHandle`、`build_tool_descriptors_anon`、`with_tool_policy_override` |
| `kernel/gateway/src/runtime/agent_registry.rs` | 修改 | 新增 `remove_llm_provider` 方法 |
| `kernel/core/src/react.rs` | 修改 | `ReActContext` 新增 `anon_tool_policy` 字段 |
| `kernel/gateway/src/runtime/agent_runtime.rs` | 修改 | 注册 `delegate_task` tool + 注入 `GatewaySubAgentSpawner` |
| `kernel/workflow/src/orchestrator.rs` | **待新增** | OrchestratorWorkflow + OrchestratorAgent（未开始） |
| `kernel/eval/src/session_progress.rs` | 待修改 | 与 Planner progress 联动（未开始） |
| `kernel/gateway/src/runtime/agent_harness.rs` | 待修改 | auto-continue 注入 pivot（未开始） |
| `kernel/idle/src/guardian.rs` | **待新增** | GuardianAction 枚举及约束逻辑（第三阶段） |
| `kernel/config/src/lib.rs` | 待修改 | 扩展 `WorkflowConfig`（stale 阈值），新增 `OrchestratorConfig` |

---

## 九、总结

Deli AutoResearch 的核心洞察——**方向多样性、跨迭代停滞检测、执行与评估分离**——是通用的，不限于论文写作场景。

Aman 已经具备实现这些模式的大部分底层能力：

| 已有 | 用途 |
|------|------|
| `session_progress::evaluate()` | 丰富的 per-session 进展评估（5 种信号） |
| `react_loop` auto-continue | 评估驱动的续跑决策 + continuation context |
| `WorkflowEngine` | FSM + timeout + retry + StateStore |
| `AgentMessage` | agent 间 TaskDelegation / ResultSharing |
| `StateStore` | 跨 session 状态持久化原语 |

**唯一完全缺失且无可替代的是方向多样性（P0）**。这是 Deli 对 aman 最有价值的贡献——防止 Agent 在长期运行中陷入认知循环。

整合策略：
1. **不引入新的基础架构** — 复用 WorkflowEngine + StateStore + AgentMessage
2. **不照搬平文件模式** — 用 StateStore 替代文件系统耦合
3. **不违反 aman 架构原则** — 保留 chat-first 交互，编排器作为后台增强
4. **渐进交付** — DirectionTracker + TaskProgressTracker 先行（独立可用且立即生效），编排器随后
