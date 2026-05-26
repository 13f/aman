# Work System — Architecture Design

> **核心能力（Core Capability）**：与 Idle System 平级，由 AgentRuntime 在 Phase 4 初始化，
> Phase 0 关闭时销毁。不可禁用，每个 Agent 实例自动获得独立的 WorkSystem。
>
> 将任务认领与执行建模为 Agent 的核心行为。工作不是外部驱动的被动响应，
> 而是 Agent 基于能力与优先级自主选择、分解、执行、反思的完整闭环。
>
> **五种工作状态**：IDLE → CHECKING → CLAIMING → EXECUTING → REVIEWING。
> **IDLE** 是统一的空闲状态——表明 Work System 当前无任何活动，也是系统中断入口：
> 任何时刻收到 `Interrupt` 事件，无论当前处于何种状态，都直接回到 IDLE，
> 将执行权交还给 Agent 调度器，由调度器决定激活哪个子系统。
> 所有状态转换通过 Agent 自身的 Event Bus 事件驱动，与 Idle System 通过 Bus 空/非空自然协作。
>
> **Per-Agent 架构**：每个 Agent 拥有独立的 WorkSystem 实例——自己的 Event Bus、
> 自己的 work_state、自己的 task 队列编排。跨 Agent 协作通过 Global Event Bus 的 kanban/team 完成。

---

## 1. Problem Statement

### 要解决的问题

aman Agent 在 Idle System 的驱动下有了「无事可做时做什么」的答案。但「有事可做时如何做」——即
任务的发现、认领、执行、复核——需要一个与 Idle 对称的核心能力。

核心挑战：
- Agent 需要**自主发现**可做的任务，而不是等待外部指令
- 任务执行不能阻塞 Event Bus（否则 Idle 误触发）
- 多个 Agent 可能在 kanban/team 中竞争同一任务，需要安全的认领机制
- 执行结果需要被复核、记录、反馈给 Idle System（影响 arousal 和后续行为）

### 核心约束

| 约束类型 | 内容 |
|---------|------|
| **不可变（框架哲学）** | 一切行为事件驱动。Work System 不引入新的执行模型，只产生和消费新类型的事件 |
| **可变（业务策略）** | 任务选择策略、冷却时间、步骤分解粒度、复核严格度 |
| **架构约束** | Work System 通过 Agent 自身的 Event Bus 驱动；跨 Agent 通信走 Global Event Bus |
| **协作约束** | Idle System 仅在 Bus 为空时触发 → Work 必须在执行期间持续投递事件 |
| **安全约束** | 认领使用乐观锁；危险操作（rm -rf、git push --force）需人类确认 |

---

## 2. Design Philosophy

```
工作不是「等待任务到达」的被动状态。
而是 Agent 周期性巡视任务板、评估自身能力、
主动认领、分解执行、复核交付的自主行为。
```

五条设计原则：

1. **Event Bus 是唯一的状态推进器** — 所有 Work System 状态转换都由事件触发，无外部 tick。
2. **Bus 非空即活跃** — 任务执行期间通过链式投递事件（ExecuteStep → next ExecuteStep）保持 Bus 非空，Idle 自然不触发。
3. **冷却而非空转** — 无任务时通过 `DelayedWorkTick` 延迟事件安排下一次巡检，冷却期内 Bus 为空，Idle 正常运作。
4. **Per-Agent 隔离** — 每个 Agent 的 Work System 只读自己的 Event Bus，只写自己的 Trace Store。Global Event Bus 仅用于任务板交互。
5. **反馈闭环** — 任务成功/失败信号注入 Idle System（satisfaction/frustration），影响 arousal 和后续工作意愿。

---

## 3. Type System

### 3.1 WorkState — 五种工作状态

```rust
/// Work System 的状态枚举。
///
/// **IDLE** 是中断入口——也是唯一不占用 Event Bus 的状态（Bus 为空 → 全局 Idle 可运行）。
/// 收到 `Interrupt` 事件时，无论当前处于什么状态，都无条件切回 IDLE。
/// 其他四种状态通过链式投递事件保持 Bus 非空。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkState {
    /// 工作闲置。不占用 Event Bus，允许全局 Idle 运行。
    /// 也是中断入口——任何活跃状态收到 Interrupt 后回到此处。
    Idle,
    /// 正在检查任务板（同步操作，极短）。
    Checking,
    /// 正在认领任务（等待 Global Bus 的 ClaimResponse）。
    Claiming,
    /// 正在执行任务的某个子步骤。
    Executing,
    /// 正在复核执行结果。
    Reviewing,
}
```

### 3.2 WorkEvent — 工作事件类型

```rust
/// Work System 的领域事件。
///
/// 分为三类：
/// - 外部来源：由 Global Event Bus 注入（TaskBoardUpdated）
/// - 定时触发：由 DelayedWorkTick 延迟事件触发
/// - 内部流转：Work System 自身产生，用于状态机流转
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum WorkEvent {
    // ── 外部来源（通过 Global Bus → Agent Local Bus）──
    /// kanban/team 插件通知任务板有变动。
    TaskBoardUpdated {
        board_id: String,
        change_type: TaskBoardChangeType,
    },
    /// 外部系统（如 cron、webhook）触发的主动巡检。
    WorkTick {
        triggered_by: String,
    },

    // ── 延迟定时事件 ──
    /// 一段时间后触发 WorkTick，用于冷却期巡检。
    DelayedWorkTick {
        fire_at: Timestamp,
        reason: String,
    },

    // ── 内部状态机流转 ──
    /// 开始检查任务板。
    StartCheck,
    /// 认领指定任务。
    ClaimTask(TaskBrief),
    /// 认领响应。
    ClaimResponse {
        task: TaskBrief,
        success: bool,
        reason: Option<String>,  // "claimed" | "task_taken_by_other" | "permission_denied"
    },
    /// 执行下一个子步骤。
    ExecuteStep {
        task_id: TaskId,
        step_index: usize,
    },
    /// 子步骤完成。
    StepComplete {
        task_id: TaskId,
        step_index: usize,
        output: StepOutput,
    },
    /// 子步骤失败。
    StepFailed {
        task_id: TaskId,
        step_index: usize,
        error: WorkError,
    },
    /// 开始复核。
    ReviewTask(TaskBrief),
    /// 复核完成。
    ReviewComplete {
        task_id: TaskId,
        passed: bool,
        feedback: Option<String>,
    },
    /// 提交结果到 kanban/team。
    SubmitResult {
        task_id: TaskId,
        result: TaskResult,
    },
    /// 工作周期完成（日志/指标用）。
    WorkCycleDone {
        task_id: TaskId,
        outcome: WorkOutcome,
        duration: Duration,
    },

    // ── 系统中断 ──
    /// 中断当前 Work System，强制切回 IDLE。
    /// 由 Agent 调度器在需要激活其他子系统时发出。
    /// 任何状态收到此事件 → 保存 checkpoint → 直接进入 IDLE。
    Interrupt {
        reason: String,       // "user_query" | "study_activated" | "daily_activated" | "shutdown"
        by_system: String,    // 哪个系统发出的中断（如 "study", "daily_life", "core"）
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum TaskBoardChangeType {
    TaskAdded,
    TaskRemoved,
    TaskUpdated,
    StageBulkMove,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum WorkOutcome {
    Completed,
    Failed { retryable: bool },
    Abandoned,
}
```

### 3.3 WorkContext — 工作上下文

```rust
/// Work System 的共享状态。
///
/// 全部字段为 Agent 内部私有，不跨 Agent 共享。
#[derive(Debug, Clone)]
pub struct WorkContext {
    /// 当前工作状态。
    pub state: WorkState,
    /// 当前正在处理的任务。
    pub current_task: Option<TaskBrief>,
    /// 任务的子步骤列表（由 decompose_task 产生）。
    pub task_steps: Vec<Step>,
    /// 当前执行到的步骤索引。
    pub step_index: usize,
    /// 上一次检查任务板的时间。
    pub last_check_time: Timestamp,
    /// 连续认领失败的次数（用于退避策略）。
    pub consecutive_claim_failures: u32,
}

impl WorkContext {
    pub fn new() -> Self {
        Self {
            state: WorkState::Idle,
            current_task: None,
            task_steps: Vec::new(),
            step_index: 0,
            last_check_time: Timestamp::now(),
            consecutive_claim_failures: 0,
        }
    }

    /// 重置为闲置状态，清空当前任务上下文。
    pub fn reset_to_idle(&mut self) {
        self.state = WorkState::Idle;
        self.current_task = None;
        self.task_steps.clear();
        self.step_index = 0;
    }

    /// 中断当前任务，保存 checkpoint 后回到 IDLE。
    /// 与 reset_to_idle 不同：会先保存当前进度到 Trace Store，
    /// 以便下次从断点恢复（如果任务支持续传）。
    pub fn interrupt(&mut self, reason: &str) -> WorkCheckpoint {
        let checkpoint = WorkCheckpoint {
            state: self.state,
            task_id: self.current_task.as_ref().map(|t| t.id),
            step_index: self.step_index,
            timestamp: Timestamp::now(),
            reason: reason.to_string(),
        };
        self.reset_to_idle();
        checkpoint
    }
}
```

### 3.4 WorkPersonality — 每个 Agent 的工作人格

```rust
/// 定义 Agent 如何发现、选择、执行任务的行为参数。
///
/// 与 IdlePersonality 对称——前者定义「如何工作」，后者定义「如何空闲」。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkPersonality {
    /// 是否启用自主认领。
    pub auto_claim: bool,

    /// 能力标签（用于匹配任务板的 skill_match）。
    pub capabilities: Vec<String>,

    /// 最大并发任务数。
    pub max_concurrent: usize,

    /// 工作冷却时间（两次巡检之间的最小间隔）。
    pub work_cooldown: Duration,

    /// 认领失败后的退避策略。
    pub claim_retry: RetryStrategy,

    /// 任务选择策略。
    pub selection: TaskSelectionStrategy,

    /// 步骤分解策略。
    pub decomposition: DecompositionStrategy,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetryStrategy {
    /// 基础重试延迟。
    pub base_delay: Duration,
    /// 退避倍数。
    pub backoff_multiplier: f64,
    /// 最大重试延迟（上限）。
    pub max_delay: Duration,
    /// 最大连续失败次数后放弃本次工作周期。
    pub max_consecutive_failures: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum TaskSelectionStrategy {
    /// 优先选择能力匹配度最高的任务。
    BestMatch,
    /// 优先选择最早创建的任务（FIFO）。
    EarliestFirst,
    /// 优先选择高优先级任务。
    HighPriorityFirst,
    /// 加权综合评分：priority * 0.4 + match_score * 0.4 + age * 0.2
    Weighted {
        priority_weight: f64,
        match_weight: f64,
        age_weight: f64,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DecompositionStrategy {
    /// 每个子步骤的最大估算耗时。
    pub max_step_duration: Duration,
    /// 是否将 LLM 调用放在独立步骤中。
    pub isolate_llm_calls: bool,
    /// 是否将工具调用（I/O）放在独立步骤中。
    pub isolate_tool_calls: bool,
}
```

---

## 4. State Machine

### 4.1 完整状态转移图

```
                         ┌─────────────────────────────────────────────┐
                         │    Interrupt 事件（来自 Agent 调度器）        │
                         │    任何活跃状态 → 保存 checkpoint → IDLE     │
                         │    CHECKING ──Interrupt──▶ IDLE             │
                         │    CLAIMING ──Interrupt──▶ IDLE             │
                         │    EXECUTING──Interrupt──▶ IDLE             │
                         │    REVIEWING──Interrupt──▶ IDLE             │
                         │                                              │
                         ▼                                              │
 ┌───────┐  TaskBoardUpdated  ┌───────────┐                            │
 │ IDLE  │───────────────────▶│ CHECKING  │                            │
 └───┬───┘   or WorkTick      └─────┬─────┘                            │
     │         (cooldown 已过)       │                                  │
     │                               │ 无可用任务                       │
     │                               ├──────────────▶ IDLE             │
     │                               │   + post DelayedWorkTick        │
     │                               │                                  │
     │                               │ 有可用任务                       │
     │                               ▼                                  │
     │                         ┌───────────┐                           │
     │                         │ CLAIMING  │                           │
     │                         └─────┬─────┘                           │
     │                               │                                  │
     │                   ClaimResponse(success=true)                   │
     │                               │                                  │
     │                               ▼                                  │
     │                         ┌───────────┐  链式：step→next step      │
     │                         │ EXECUTING │◄────────────────────┐     │
     │                         └─────┬─────┘                     │     │
     │                               │                            │     │
     │                  所有步骤完成  │   有下一步骤               │     │
     │                               │                            │     │
     │                               ▼                            │     │
     │                         ┌───────────┐                     │     │
     │                         │ REVIEWING │                     │     │
     │                         └─────┬─────┘                     │     │
     │                               │                            │     │
     │                   ReviewComplete(passed=true)              │     │
     │                               │                            │     │
     │                               ├──▶ SubmitResult            │     │
     │                               ├──▶ inject_satisfaction()   │     │
     │                               └──▶ IDLE                    │     │
     │                                     + post DelayedWorkTick │     │
     │                                                            │     │
     │                   ReviewComplete(passed=false)             │     │
     │                               │                            │     │
     │                               ├──▶ SubmitResult(failed)    │     │
     │                               └──▶ IDLE                    │     │
     │                                     + post DelayedWorkTick │     │
     │                                                            │     │
     │                   StepFailed (重试可能)                     │     │
     │                               └──▶ EXECUTING (retry) ─────┘     │
     │                                                            │     │
     │                   ClaimResponse(success=false)             │     │
     │                               │                            │     │
     │                               ├──▶ inject_frustration()    │     │
     │                               └──▶ IDLE                    │     │
     │                                     + post DelayedWorkTick │     │
     └────────────────────────────────────────────────────────────┘
```

### 4.2 状态转移规则

1. **IDLE → CHECKING**：收到 `TaskBoardUpdated` 或 `WorkTick`/`DelayedWorkTick` 且冷却时间已过。
2. **CHECKING → IDLE**：任务板无可认领任务。投递 `DelayedWorkTick` 安排下次巡检。
3. **CHECKING → CLAIMING**：选出最佳匹配任务，投递 `ClaimTask`。
4. **CLAIMING → EXECUTING**：收到 `ClaimResponse(success=true)`，分解任务为子步骤，投递首个 `ExecuteStep`。
5. **CLAIMING → IDLE**：收到 `ClaimResponse(success=false)`，注入挫败感，退避重试。
6. **EXECUTING → EXECUTING**：`StepComplete` 且还有下一步，投递下一个 `ExecuteStep`。保持 Bus 非空。
7. **EXECUTING → REVIEWING**：所有步骤完成，投递 `ReviewTask`。
8. **EXECUTING → IDLE**：`StepFailed` 且不可重试（超过最大重试或错误不可恢复）。
9. **REVIEWING → IDLE**：复核完成，提交结果，注入满足/挫败感，投递 `DelayedWorkTick`。

**⭐ 中断规则（最高优先）**：
10. **{CHECKING, CLAIMING, EXECUTING, REVIEWING} → IDLE**：收到 `Interrupt` 事件，保存 checkpoint，无条件切回 IDLE。之后由 Agent 调度器决定激活哪个子系统。

### 4.3 链式事件投递：Bus 非空保证

```
ExecuteStep(step=0)
  → 执行 → StepComplete(step=0)
  → post_event(ExecuteStep(step=1))
  → 执行 → StepComplete(step=1)
  → post_event(ExecuteStep(step=2))
  ...
  → 执行 → StepComplete(step=N-1)
  → N 步完成 → post_event(ReviewTask)
  → 复核 → post_event(ReviewComplete) + SubmitResult
  → post_event(WorkCycleDone)
  → post_delayed_event(DelayedWorkTick, delay=work_cooldown)
```

整个执行期间 Bus 始终有事件（每个 step 执行期间有当前事件，完成后立刻投递下一个），Idle 系统不会误触发。

### 4.4 冷却与退避策略

```
无任务时的冷却：
  IDLE + TaskBoardUpdated/WorkTick + now - last_check >= work_cooldown
    → CHECKING
  IDLE + TaskBoardUpdated/WorkTick + now - last_check < work_cooldown
    → 忽略（不产生新事件 → Bus 继续空 → 全局 Idle 继续运行）

认领失败的退避：
  consecutive_claim_failures = 0 → 立即 post DelayedWorkTick(work_cooldown)
  consecutive_claim_failures = 1 → post DelayedWorkTick(work_cooldown * 2)
  consecutive_claim_failures = 2 → post DelayedWorkTick(work_cooldown * 4)
  ...
  consecutive_claim_failures >= max → 放弃本次工作周期
```

---

## 5. Event Dispatch Logic

### 5.1 事件路由

Work System 在 Agent 的 `handle_event` 中注册以下事件：

```rust
impl Agent {
    fn route_event(&self, event: &Event) -> Option<Handler> {
        match event.kind() {
            // 外部来源 → 总是进入 Work System
            "kanban.task_board_updated"
            | "team.work_tick"
            | "work.work_tick"
            | "work.delayed_work_tick" => Some(Handler::WorkSystem),

            // 内部流转事件 → Work System
            "work.start_check"
            | "work.claim_task"
            | "work.claim_response"
            | "work.execute_step"
            | "work.step_complete"
            | "work.step_failed"
            | "work.review_task"
            | "work.review_complete"
            | "work.submit_result"
            | "work.cycle_done" => Some(Handler::WorkSystem),

            // 中断事件 → Work System（无论在做什么，强制切 IDLE）
            "work.interrupt" => Some(Handler::WorkSystem),

            // 其余事件 → 其他处理器
            _ => None,
        }
    }
}
```

### 5.2 WorkSystem::handle 伪代码

```rust
impl WorkSystem {
    pub async fn handle(
        &mut self,
        event: WorkEvent,
        ctx: &WorkContext,
        global_bus: &dyn GlobalEventBus,
        local_bus: &dyn EventBus,
        idle_coord: &IdleCoordination,
        trace: &mut TraceStore,
    ) -> WorkResult<()> {
        match (ctx.state, event) {
            // ── ★ Interrupt（最高优先级，任何状态）────────────
            (_, WorkEvent::Interrupt { reason, by_system }) => {
                // 保存 checkpoint，无条件切到 IDLE
                let checkpoint = ctx.interrupt(&reason);
                trace.record(WorkTraceEvent::Interrupted {
                    checkpoint,
                    by_system,
                });
                // 不投递后续事件——Bus 变空，调度器接管
                return Ok(());
            }

            // ── IDLE ────────────────────────────────────────
            (WorkState::Idle, WorkEvent::TaskBoardUpdated { .. }) => {
                ctx.state = WorkState::Checking;
                local_bus.post(WorkEvent::StartCheck).await?;
            }
            (WorkState::Idle, WorkEvent::WorkTick { .. })
            | (WorkState::Idle, WorkEvent::DelayedWorkTick { .. }) => {
                if now() - ctx.last_check_time >= self.personality.work_cooldown {
                    ctx.state = WorkState::Checking;
                    local_bus.post(WorkEvent::StartCheck).await?;
                }
                // else: 忽略，Bus 继续空 → 全局 Idle 继续
            }
            (WorkState::Idle, _) => { /* 忽略 */ }

            // ── CHECKING ─────────────────────────────────────
            (WorkState::Checking, WorkEvent::StartCheck) => {
                ctx.last_check_time = now();
                match kanban.get_available_tasks(&self.personality.capabilities).await {
                    Ok(tasks) if !tasks.is_empty() => {
                        let best = self.personality.selection.select(tasks);
                        ctx.state = WorkState::Claiming;
                        local_bus.post(WorkEvent::ClaimTask(best)).await?;
                    }
                    _ => {
                        ctx.reset_to_idle();
                        // 投递延迟巡检
                        let delay = self.claim_backoff_delay(ctx.consecutive_claim_failures);
                        local_bus.post_delayed(WorkEvent::DelayedWorkTick { /* ... */ }, delay).await?;
                    }
                }
            }

            // ── CLAIMING ─────────────────────────────────────
            (WorkState::Claiming, WorkEvent::ClaimTask(task)) => {
                global_bus.post(ClaimRequest { task_id: task.id, agent_id: self.agent_id }).await?;
                // 等待 ClaimResponse（通过 Global Bus → Local Bus 回传）
            }
            (WorkState::Claiming, WorkEvent::ClaimResponse { task, success, reason }) => {
                if success {
                    ctx.current_task = Some(task.clone());
                    ctx.task_steps = self.decompose_task(&task);
                    ctx.step_index = 0;
                    ctx.consecutive_claim_failures = 0;
                    ctx.state = WorkState::Executing;
                    local_bus.post(WorkEvent::ExecuteStep {
                        task_id: task.id, step_index: 0,
                    }).await?;
                } else {
                    ctx.consecutive_claim_failures += 1;
                    ctx.reset_to_idle();
                    // 注入挫败感到 Idle System
                    idle_coord.inject_event(IdleSignal::Frustration { reason });
                    let delay = self.claim_backoff_delay(ctx.consecutive_claim_failures);
                    local_bus.post_delayed(WorkEvent::DelayedWorkTick { /* ... */ }, delay).await?;
                }
            }

            // ── EXECUTING ────────────────────────────────────
            (WorkState::Executing, WorkEvent::ExecuteStep { task_id, step_index }) => {
                let step = &ctx.task_steps[step_index];
                let result = self.execute_step(step).await;
                if result.is_ok() {
                    if step_index + 1 < ctx.task_steps.len() {
                        // 链式投递下一步 → Bus 保持非空
                        local_bus.post(WorkEvent::ExecuteStep {
                            task_id, step_index: step_index + 1,
                        }).await?;
                    } else {
                        ctx.state = WorkState::Reviewing;
                        local_bus.post(WorkEvent::ReviewTask(ctx.current_task.clone().unwrap())).await?;
                    }
                } else {
                    // 步骤失败
                    if self.should_retry(step_index, &result) {
                        // 重试同一步骤
                        local_bus.post(WorkEvent::ExecuteStep { task_id, step_index }).await?;
                    } else {
                        ctx.state = WorkState::Idle;
                        local_bus.post(WorkEvent::StepFailed { task_id, step_index, error: result.unwrap_err() }).await?;
                    }
                }
            }

            // ── REVIEWING ────────────────────────────────────
            (WorkState::Reviewing, WorkEvent::ReviewTask(task)) => {
                let passed = self.verify_result(&task).await;
                trace.record(WorkTraceEvent {
                    task_id: task.id,
                    state: if passed { "review_passed" } else { "review_failed" },
                    timestamp: now(),
                });
                if passed {
                    global_bus.post(TaskCompleted { task_id: task.id, result: self.collect_result() }).await?;
                    idle_coord.inject_event(IdleSignal::Satisfaction { task_id: task.id });
                } else {
                    global_bus.post(TaskFailed { task_id: task.id, reason: "review_failed".into() }).await?;
                    idle_coord.inject_event(IdleSignal::Disappointment { task_id: task.id });
                }
                ctx.reset_to_idle();
                local_bus.post_delayed(WorkEvent::DelayedWorkTick { /* ... */ }, self.personality.work_cooldown).await?;
            }

            // ── 无效转换 ─────────────────────────────────────
            _ => {
                log::warn!("WorkSystem: invalid transition {:?} + {:?}", ctx.state, event);
            }
        }
        Ok(())
    }
}
```

### 5.3 System Interruption Protocol（系统中断协议）

三个子系统（Work / Study / DailyLife）共享同一个 IDLE 语义。Agent 调度器通过
`Interrupt` 事件实现子系统切换：

```
          ┌─────────────────────────────────────┐
          │         Agent 调度器                 │
          │  ┌─────────────────────────────┐    │
          │  │ 当前活跃系统: Work           │    │
          │  │ ctx.state = Executing       │    │
          │  └──────────────┬──────────────┘    │
          │                 │                    │
          │  用户: "别工作了，帮我读这篇论文"   │
          │                 │                    │
          │                 ▼                    │
          │  ┌─────────────────────────────┐    │
          │  │ 1. post Interrupt 到 Work   │    │
          │  │    → Work: IDLE (checkpoint) │    │
          │  │ 2. post StudyAssigned       │    │
          │  │    → Study: DISCOVERING     │    │
          │  └─────────────────────────────┘    │
          └─────────────────────────────────────┘
```

中断规则的三个层次：

| 层次 | 触发条件 | 行为 |
|------|---------|------|
| **系统间切换** | 用户显式切换（「别工作了，学习吧」） | 调度器发 `Interrupt` 到当前系统 → IDLE → 发激活事件到目标系统 |
| **高优抢占** | 外部事件优先级高于当前系统 | 调度器发 `Interrupt` → 当前系统 IDLE → 路由新事件到对应系统 |
| **自然完成** | 子系统自身状态机走到终点 | 直接回到 IDLE，调度器不做任何切换 |

关键不变量：
- **只有 IDLE 中的系统可以被激活** — 调度器在投递 StudyAssigned/WorkTick 等激活事件前，确保目标系统处于 IDLE
- **Interrupt 总是成功的** — 任何活跃状态收到 Interrupt 后无条件切到 IDLE，不依赖锁或等待
- **checkpoint 保底** — Interrupt 前保存当前进度到 Trace Store，恢复时从断点继续（如果任务支持续传）
- **IDLE 时 Bus 为空** — 系统进入 IDLE 后不持有任何待处理事件，全局 Idle System 可以正常运作

```rust
/// Agent 调度器的子系统切换逻辑。
impl AgentScheduler {
    pub async fn activate_system(
        &mut self,
        target: SystemKind,  // Work | Study | DailyLife
        activation_event: Event,
    ) -> Result<()> {
        // 1. 如果另一个系统正在活跃 → 发送 Interrupt
        if let Some(active) = self.active_system {
            if active != target {
                self.local_bus.post(Event::interrupt(
                    active,
                    format!("{:?}_activated", target),
                )).await?;
                // 等待 Interrupt 被处理（当前 tick 内同步完成）
            }
        }

        // 2. 确认目标系统处于 IDLE
        debug_assert!(self.get_system_state(target) == SystemState::Idle);

        // 3. 投递激活事件
        self.local_bus.post(activation_event).await?;
        self.active_system = Some(target);

        Ok(())
    }
}
```

---

## 6. Configuration

### 6.1 YAML 配置

```yaml
work:
  personality:
    auto_claim: true
    capabilities: [code, refactor, fix, review]
    max_concurrent: 2
    work_cooldown: 60s                 # 两次巡检最小间隔

    claim_retry:
      base_delay: 30s
      backoff_multiplier: 2.0
      max_delay: 300s
      max_consecutive_failures: 5

    selection:
      type: weighted
      priority_weight: 0.4
      match_weight: 0.4
      age_weight: 0.2

    decomposition:
      max_step_duration: 120s
      isolate_llm_calls: true
      isolate_tool_calls: true

  # 任务板连接
  board:
    type: kanban                  # kanban | team | custom
    poll_interval: 30s            # TaskBoardUpdated 的最大间隔
    query:
      stages: [backlog, wip]
      limit: 20

  # 复核配置
  review:
    auto_verify: true
    require_human_approval_for:
      - "git push --force"
      - "rm -rf"
      - "DROP TABLE"
    timeout: 120s
```

### 6.2 配置验证规则

```rust
impl ConfigValidator for WorkConfig {
    fn validate(&self) -> Result<(), ConfigError> {
        if self.personality.max_concurrent == 0 {
            return Err(ConfigError::invalid("work.personality.max_concurrent", "must be >= 1"));
        }
        if self.personality.work_cooldown < Duration::from_secs(5) {
            return Err(ConfigError::invalid("work.personality.work_cooldown", "must be >= 5s to avoid busy-loop"));
        }
        // capabilities 不能为空（除非 auto_claim = false）
        if self.personality.auto_claim && self.personality.capabilities.is_empty() {
            return Err(ConfigError::invalid("work.personality.capabilities", "must not be empty when auto_claim=true"));
        }
        Ok(())
    }
}
```

---

## 7. Runtime Integration

Work System 是 AgentRuntime 的核心组件，不在 plugin 层加载，而是在 Phase 4
（Agent 实例创建阶段）由 `AgentBuilder` 直接构造：

```
AgentRuntime 启动流程 (Phase 0→5):
  Phase 0: Config 加载 → 解析 work.personality
  Phase 1: EventBus 初始化 (Global + Per-Agent Local)
  Phase 2: Secret / Persistence 初始化
  Phase 3: Pipeline / Skill / Tool 加载
  Phase 4: Agent 实例创建 ★
    ├─ IdleSystem (AgentIdleManager)  ← 核心能力
    ├─ WorkSystem                    ← 核心能力
    ├─ StudySystem                   ← 核心能力
    ├─ DailyLifeSystem               ← 核心能力
    └─ AgentScheduler (子系统调度器)  ← 核心能力
  Phase 5: Source 启动 → Agent 进入事件循环

AgentRuntime 关闭流程 (Phase 5→0):
  Phase 5: Source 停止
  Phase 4: AgentScheduler.shutdown() → Interrupt 所有活跃系统 → IDLE
           WorkSystem.shutdown()  → flush trace, cancel delayed ticks
           StudySystem.shutdown() → flush knowledge graph
           DailyLifeSystem.shutdown() → persist today's snapshot
  Phase 3→0: ...
```

```rust
/// AgentBuilder 在 Phase 4 构造所有核心系统。
impl AgentBuilder {
    pub fn build(self) -> Result<AgentRuntime> {
        let local_bus = self.create_local_event_bus()?;
        let global_bus = self.global_bus.clone();
        let persistence = self.persistence.clone();

        // 核心能力：每个 Agent 实例自动获得
        let idle_sys = AgentIdleManager::new(
            self.config.personality.idle.clone(),
            local_bus.clone(),
        );

        let work_sys = WorkSystem::new(
            self.config.personality.work.clone(),
            local_bus.clone(),
            global_bus.clone(),
            persistence.trace_store(),
        );

        let study_sys = StudySystem::new(
            self.config.personality.study.clone(),
            local_bus.clone(),
            persistence.knowledge_graph(),
            persistence.memory_store(),
        );

        let daily_sys = DailyLifeSystem::new(
            self.config.personality.daily_life.clone(),
            local_bus.clone(),
            persistence.daily_life_store(),
        );

        let scheduler = AgentScheduler::new(
            vec![
                SystemKind::Work(work_sys),
                SystemKind::Study(study_sys),
                SystemKind::DailyLife(daily_sys),
            ],
            local_bus.clone(),
        );

        Ok(AgentRuntime {
            idle: idle_sys,
            work: work_sys,
            study: study_sys,
            daily_life: daily_sys,
            scheduler,
            local_bus,
            // ...
        })
    }
}
```

---

## 8. Integration Points

### 8.1 与 Idle System 的协作

```
┌─────────────────────────────────────────────────────────────────┐
│                     Agent Event Bus                              │
│                                                                  │
│  ┌──────────┐   ┌──────────┐   ┌──────────┐   ┌──────────────┐ │
│  │  Work    │   │  Work    │   │  Work    │   │ DelayedWork  │ │
│  │  Start   │──▶│ Execute  │──▶│ Execute  │──▶│ Tick         │ │
│  │  Check   │   │ Step(0)  │   │ Step(1)  │   │ (fire later) │ │
│  └──────────┘   └──────────┘   └──────────┘   └──────┬───────┘ │
│                                                       │         │
│  Bus 非空 ────────────────────────────────────────────┘         │
│  → Idle System 不触发                                            │
│                                                                  │
│  ═══════════════════ 任务完成 ═══════════════════════            │
│                                                                  │
│  Bus 变为空 (DelayedWorkTick 尚未到期)                            │
│  → Idle System 触发: Daze → Boredom → ...                       │
│                                                                  │
│  ═══════════════ DelayedWorkTick 到期 ═══════════════            │
│                                                                  │
│  Bus 非空 (DelayedWorkTick 事件)                                  │
│  → Work System 处理 → CHECKING → ...                             │
└─────────────────────────────────────────────────────────────────┘
```

### 8.2 与 Trace System 的整合

Work System 将以下事件写入 Agent 私有的 Trace Store：

```rust
#[derive(Debug, Clone, Serialize)]
pub enum WorkTraceEvent {
    /// 巡检开始。
    CheckStarted { candidates_count: usize },
    /// 认领尝试。
    ClaimAttempted { task_id: TaskId, outcome: ClaimOutcome },
    /// 步骤执行。
    StepExecuted {
        task_id: TaskId,
        step_index: usize,
        duration: Duration,
        success: bool,
        error: Option<String>,
    },
    /// 复核结果。
    ReviewCompleted {
        task_id: TaskId,
        passed: bool,
        confidence: f64,
    },
    /// 工作周期汇总。
    CycleCompleted {
        task_id: TaskId,
        outcome: WorkOutcome,
        total_duration: Duration,
        steps_completed: usize,
        steps_failed: usize,
    },
}
```

Trace 系统分析后，可动态调整的参数：
- `work_cooldown` — 高频任务环境缩短冷却，低频环境延长
- `selection strategy` — 根据历史成功率调整权重
- `max_concurrent` — 根据步骤平均耗时调整并发数
- `decomposition.max_step_duration` — 根据步骤成功率调整粒度

### 8.3 与 Global Event Bus 的交互

```rust
/// Work System 与外部系统的接口定义。
#[async_trait]
pub trait WorkBoardClient {
    /// 获取可认领的任务列表（按 Agent 能力过滤）。
    async fn get_available_tasks(&self, capabilities: &[String]) -> Result<Vec<TaskBrief>>;

    /// 发送认领请求（乐观锁）。
    async fn claim_task(&self, task_id: TaskId, agent_id: &str) -> Result<ClaimResponse>;

    /// 提交任务结果。
    async fn submit_result(&self, task_id: TaskId, result: TaskResult) -> Result<()>;
}
```

kanban 和 team 两个插件各自实现此 trait，Work System 不关心后端是哪个。

---

## 9. Event Routing Configuration

```yaml
routes:
  # 外部事件 → Work System
  - match: { event_type: "kanban.task_board_updated" }  → handler:work
  - match: { event_type: "team.work_tick" }             → handler:work
  - match: { event_type: "work.work_tick" }             → handler:work

  # 内部流转事件 → Work System
  - match: { event_type: "work.*" }                     → handler:work
```

---

## 10. Summary

| 维度 | 设计决策 |
|------|---------|
| **驱动方式** | 完全事件驱动，不引入独立线程/tick |
| **状态管理** | 五状态状态机，转换由事件触发 |
| **Idle 协作** | Bus 非空时 Idle 不触发；冷却期通过 DelayedWorkTick 保持 Bus 空 |
| **跨 Agent 协作** | 通过 Global Event Bus + kanban/team 插件的乐观锁认领 |
| **反馈闭环** | 成功/失败注入 Idle System 的 satisfaction/frustration，影响 arousal |
| **可观测性** | 通过 Trace Store 记录完整的执行历史，支持动态参数调整 |
| **配置灵活性** | 任务选择策略、退避策略、分解策略均可按 Agent 独立配置 |

**最终效果**：
- Agent 的事件循环保持简单统一（poll event → 处理 → 无事件则 idle）
- Work 和 Idle 系统通过 Event Bus 自然协同，无额外标志位
- 完全兼容 per-Agent Event Bus、Memory、LLM、Trace 的架构约束
- 保持了拟人性的同时引入了清晰的任务协作机制
