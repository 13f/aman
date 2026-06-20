# Deli AutoResearch SKILL → aman 差距分析与整合建议

> 2026-06-19 | 基于 https://victorchen96.github.io/auto_research/framework.html
> 2026-06-20 修订 | 合并差距分析与整合路径，修正 aman 能力评估
> 2026-06-20 三修 | CronManager 已移除，CronSource 简化为 EventSource 走 SourceRegistry 管理；同步更新整合路径

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

### Gap #1: 方向多样性强制 ✅ **P0 — 完全缺失，核心防御机制**

**Deli 做法**：`directions_tried.json` 追踪所有已尝试方向。stale_count ≥ 2 → 强制改变结构性约束（不仅是参数微调）。新方向必须与历史不同。

**aman 实际**：全代码库搜索 `direction.*tried`、`DirectionTracker`、`direction_diversity` — **零命中**。auto-continue 重置 turn 计数器后在同一方向上继续，不检查"是否已经试过类似做法"。

**差距**：Deli 框架最独特的贡献——防止认知循环的核心机制。aman 的 `RotDetector`（`kernel/context-manager/src/rot.rs`）能捕捉部分循环模式：
- `RepeatedToolCall` — 同工具+同参数重复调用（≥3 次 warning，≥5 次 critical）
- `ToolErrorLoop` — 连续工具调用失败
- `OffTopicDrift` — 回复偏离任务主题
- `Contradiction` — 输出与之前矛盾

但这些是**单 session 内的信号检测**，不等同于方向多样性——`RotDetector` 告诉你"出问题了"，但不提供"换个方向试试"的机制。Deli 的方向多样性是在跨迭代层面强制改变探索策略，aman 完全缺失这一层。这是唯一一个 aman 没有任何等价物的 P0 项。

**可能位置**：新 crate `kernel/direction`，或集成到 `kernel/eval`。

```
核心概念：
- Direction 表示（prompt 策略标签 + 参数组合哈希）
- 差异判定（Jaccard 快判 + embedding 精判）
- 与 auto-continue + TaskProgressTracker 联动的 forced_pivot 逻辑
- 与 RotDetector 互补：RotDetector 发现循环 → DirectionTracker 提供替代方向
```

---

### Gap #2: 跨 session 累积进展追踪 ⚠️ **P1 — per-session 评估已有且丰富，缺跨 session 累积**

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

**差距**：aman 的 per-session 评估比 Deli 更丰富（5 种信号 vs 1 个计数器），但缺少**跨 session 累积**能力。Deli 的 orchestrator 可以跨多次 work session 追踪 `stale_count`，aman 的 auto-continue 只在单次 `react_loop` 内判断（最多 5 次续跑），结束后状态不跨 session 持久化。

**可能位置**：扩展 `kernel/eval/src/session_progress.rs`，或在 `agent_harness` 层引入跨 session 的 `TaskProgressTracker`。

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

### 第一阶段：P0 + P1 基础能力（~4–5 天）

#### 1. DirectionTracker — 方向多样性（P0）

**新增** `kernel/direction/` crate，或集成到 `kernel/eval`。

```
DirectionTracker {
    tried: HashMap<DirectionHash, DirectionRecord>,  // 已尝试方向
    similarity_threshold: f32,                        // Jaccard / embedding 阈值
}

方法：
- try_new_direction(hint: &str) -> Result<Direction, ConflictReason>
- record_direction(dir: &Direction)                  // 标记已尝试
- suggest_pivot(stale_count: u32) -> Direction        // 停滞时生成新方向
```

- 与 `auto-continue` 联动：`stale_count >= 2` → 在 continuation context 中注入 pivot 方向
- `Direction` 表示：prompt 策略标签 + 参数组合哈希
- 差异判定：先用 Jaccard（快），必要时用 embedding（准）

**优先级理由**：这是 Deli 对 aman 最有价值的贡献——防止认知循环。aman 目前完全缺失。

#### 2. TaskProgressTracker — 跨 session 累积进展（P1）

**新增** `kernel/persistence/src/task_progress.rs`，基于 `StateStore`。

```rust
struct TaskProgress {
    task_id: String,
    stale_count: u32,          // 连续零进展 session 数
    total_findings: u32,       // 累计有效产出
    sessions_completed: u32,   // 已完成的 session 数
    last_evaluated_session: String,
    last_progress_at: Timestamp,
}
```

- 每次 `react_loop` 结束时写入（复用现有 `session_progress::evaluate()` 的输出）
- `stale_count` 更新规则：`collision_found || best_partial_match 有增长` → 归零；否则 +1
- 在 continuation context 构建时读取，注入跨 session 状态
- `stale_count >= 2` → 触发 DirectionTracker 的 pivot 建议
- `stale_count >= 4` → 发布 `ESCALATE` 事件（通知用户）

**优先级理由**：aman 的 per-session 评估已经完善，补跨 session 累积是增量改进，且是 DirectionTracker 生效的前提。

#### 3. Cron 配置持久化（L1 心跳基础）—— 已完成 CronSource 简化

> **2026-06-20 已实施**：`CronManager` 已移除，`CronSource` 精简为 ~210 行纯 `EventSource`，直接通过 `SourceRegistry` 管理生命周期。调度由 `SourceRegistry` 的后台 `poll_loop` 统一驱动。

**待完成**：`SourceRegistry` 当前是纯内存的——重启后 cron 配置丢失。需要：
- `SourceRegistry` 增加配置导出/导入钩子，或
- `AgentRuntime` 在 Phase 4 启动时从 config 文件恢复 cron source 并注册到 `SourceRegistry`

### 第二阶段：编排与调度（~5–7 天）

#### 4. SubTaskScheduler — 子任务调度模式

**新增** `kernel/workflow/src/patterns.rs`。

四种调度模式定义为 `WorkflowDef`，复用现有 `AgentMessage::TaskDelegation` + `FanOut`：

| 模式 | 实现 |
|------|------|
| GoalDriven | 单 agent → 目标 prompt → 收集 `findings` → 返回 |
| ParallelExploration | `FanOut` N 个 agent（不同方向）→ 收集结果 → 去重 |
| PollingExperiment | 提交 → `TimerSource` 轮询 → 检测完成/失败 → 重试或返回 |
| PostIterationVerify | 独立 agent 读取 `findings` → 审计证据链 → 标记 verified/rejected |

#### 5. Orchestrator — 跨迭代编排

**新增** `kernel/workflow/src/orchestrator.rs`。

基于现有 `WorkflowDef` 的状态机：

```
ACTIVE → ITERATING ⇄ STALLED → PIVOTING → ITERATING
                  ↘ ESCALATED (需人工介入)
                  → COMPLETE
```

`OrchestratorAgent`：内置 agent（类似 idle 的 BoredomActor），负责：
- 创建/恢复 workflow 实例
- 每轮迭代通过 `AgentMessage::TaskDelegation` 启动 subagent
- 监听 subagent 完成，调用 `session_progress::evaluate()` + `TaskProgressTracker`
- 检测停滞 → 调用 `DirectionTracker::suggest_pivot()` → 发布 `PIVOT` 事件
- 更新 `WorkflowInstance.data` 中的迭代指标

### 第三阶段：Guardian 协议（P1，按需）

#### 6. Guardian 协议约束

**修改** `kernel/idle/src/types.rs` 或新增 `kernel/idle/src/guardian.rs`。

- 定义 `GuardianAction` enum（LivenessCheck / Restart / Nudge），作为 idle 触发的动作边界
- `BoredomActor` 在触发非自身任务时，限制为仅 GuardianAction
- 与 `AgentMessage` 联动，nudge 通过 `AgentMessage` 发送而非直接操作目标 agent 状态

---

## 七、依赖关系

```
DirectionTracker (P0) ──────┐
                            ├──→ Orchestrator (阶段二)
TaskProgressTracker (P1) ───┤        ↑
                            │        │
Cron 配置持久化 ────────────┤   SubTaskScheduler (阶段二)
                            │        ↑
                            └────────┘
                               (复用 AgentMessage)
```

- DirectionTracker、TaskProgressTracker、Cron 配置持久化三者**互相独立**，可并行开发
- SubTaskScheduler 依赖 TaskProgressTracker + 现有 AgentMessage
- Orchestrator 依赖 DirectionTracker + TaskProgressTracker + Cron 配置持久化 + SubTaskScheduler
- Guardian 协议与其他组件独立，可随时进行

---

## 八、涉及的关键文件

| 文件 | 变更 | 说明 |
|------|------|------|
| `kernel/direction/src/lib.rs` | **新增** | DirectionTracker（P0：方向多样性） |
| `kernel/persistence/src/task_progress.rs` | **新增** | TaskProgressTracker（P1：跨 session 进展） |
| `kernel/persistence/src/lib.rs` | 修改 | 加 `pub mod task_progress;` |
| `kernel/source/src/cron.rs` | **已完成** | CronSource 精简为 ~210 行纯 EventSource；CronManager 已移除 |
| `kernel/workflow/src/patterns.rs` | **新增** | SubTaskScheduler 四种调度模式 |
| `kernel/workflow/src/orchestrator.rs` | **新增** | OrchestratorWorkflow + OrchestratorAgent |
| `kernel/workflow/src/lib.rs` | 修改 | 加 `pub mod patterns;` `pub mod orchestrator;` |
| `kernel/eval/src/session_progress.rs` | 修改 | 与 TaskProgressTracker 联动：评估结果写入跨 session 状态 |
| `kernel/gateway/src/runtime/agent_harness.rs` | 修改 | auto-continue 注入 DirectionTracker pivot；react_loop 结束时写入 TaskProgressTracker |
| `kernel/idle/src/guardian.rs` | **新增** | GuardianAction 枚举及约束逻辑（第三阶段） |
| `kernel/config/src/lib.rs` | 修改 | 扩展 `WorkflowConfig`（stale 阈值），新增 `OrchestratorConfig` |

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
