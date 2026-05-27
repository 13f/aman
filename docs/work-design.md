# Work System — Architecture Design (v2: Passive Push Queue)

> **核心变更**：从「主动巡检 + 认领竞争」退化为「被动队列消费者」。
> 外部系统 (CLI/API/kanban/todo) 直接将 WorkItem 推送到 Agent，
> Work System 只负责顺序消费，不做发现、不做认领、不做竞争。
>
> 为什么叫 **WorkItem** 而非 Task？Task 暗示"任务"，但推送到 Work 队列的可以是
> 任何工作单元——用户指派的任务、看板拖动的卡片、API 触发的请求、Idle Boredom
> 找回来的活、定时提醒等等。WorkItem 是更通用的抽象。

---

## 1. Why This Simplification

旧设计（v1）的问题：

| 问题 | v1 做法 | 实际需求 |
|------|--------|---------|
| 主动巡检 | WorkTick / DelayedWorkTick 周期性检查任务板 | 工作由外部驱动，外部知道何时有新任务，不需要 Agent 轮询 |
| 认领竞争 | CHECKING → CLAIMING，乐观锁 | 谁分配任务给哪个 Agent 是调度器/看板的职责，Agent 不参与竞争 |
| 冷却退避 | claim 失败后指数退避 | 没有认领就没有失败，冷却只在一轮工作完成后才需要 |
| 状态机膨胀 | 5 状态 + 10+ 事件类型 | 只需要 2 状态：IDLE / BUSY |

核心理念转变：

```
旧：Agent 主动巡视任务板、评估能力、认领、执行
    → Work System 承担了「调度器」的职责

新：外部系统决定谁做什么，Agent 只负责执行
    → Work System 就是一个带 Hook 的 FIFO 工作队列消费者
```

谁来决定「哪个工作给哪个 Agent」？**外部调度器**（看板、CLI、API、全局调度器）。
调度策略可以独立演化（轮询、负载均衡、优先级、亲和性），Agent 不需要关心。

---

## 2. Simplified State Machine

### 2.1 Two States

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkState {
    /// 队列为空，无工作执行。Event Bus 空闲时 Idle System 自然运行。
    Idle,
    /// 正在执行当前 WorkItem 的某个步骤。Bus 保持非空，Idle 不触发。
    Busy,
}
```

### 2.2 State Transitions

```
                  WorkItemAssigned
     ┌───────┐  ──────────────────  ┌───────┐
     │ IDLE  │                       │ BUSY  │
     └───┬───┘                       └───┬───┘
         │                                │
         │  Interrupt                     │  当前 Item 完成 + 队列有下一个
         │  (any state → IDLE)            │  → 继续 BUSY
         │                                │
         │                                │  当前 Item 完成 + 队列为空
         │  ◄─────────────────────────────┘  → IDLE
         │
         │  WorkItemAssigned (while IDLE)
         │  → IDLE → BUSY
         │
         └───────────────────────────────

    Interrupt: 任何状态收到 → 保存 checkpoint → 无条件切回 IDLE。
```

### 2.3 Comparison with v1

| 维度 | v1 (主动拉取) | v2 (被动推送) |
|------|-------------|-------------|
| 状态数 | 5 (IDLE/CHECKING/CLAIMING/EXECUTING/REVIEWING) | 2 (IDLE/BUSY) |
| 事件类型 | 10+ | 3 + Interrupt |
| 巡检 | DelayedWorkTick 定时器 | 无 |
| 认领 | Agent 间乐观锁竞争 | 外部调度器直接指派 |
| 冷却 | 认领失败退避 + 巡检冷却 | 仅 Item 间可选冷却 |
| Idle 协作 | DelayedWorkTick 控制 Bus 空/非空节奏 | 队列空时 Bus 自然空 |
| 竞争 | Agent 之间抢任务 | 调度器侧解决 |

---

## 3. Type System

### 3.1 WorkEvent

```rust
/// Work System 的领域事件——只有 3 个业务事件 + 1 个系统事件。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum WorkEvent {
    /// 外部系统推送工作项到 Agent。
    /// 来源：CLI、API、看板调度器、todo、Boredom→SeekTask 响应等。
    WorkItemAssigned {
        item: WorkItem,
        /// 来源标识（用于日志、Hook 决策、trace 分析）。
        source: WorkItemSource,
    },

    /// 当前工作项执行完成。
    WorkItemCompleted {
        item_id: WorkItemId,
        result: WorkItemResult,
        duration: Duration,
    },

    /// 当前工作项执行失败。
    WorkItemFailed {
        item_id: WorkItemId,
        error: WorkError,
        /// 是否可重试（如果 true，WorkItem 重新入队）。
        retryable: bool,
    },

    /// 中断当前执行，强制切回 IDLE。
    /// 任何状态收到此事件 → 保存 checkpoint → 无条件 IDLE。
    Interrupt {
        reason: String,
        by_system: String,
    },
}
```

### 3.2 WorkItemSource

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum WorkItemSource {
    /// 通过 aman CLI 直接指派。
    Cli { operator: String },
    /// 通过 HTTP API 指派。
    Api { endpoint: String, operator: String },
    /// 看板插件调度器分配。
    Kanban { board_id: String, scheduler: String },
    /// Todo 列表插件分配。
    Todo { list_id: String },
    /// Idle Boredom 下 Agent 主动 SeekTask 后，调度器响应。
    SeekResponse { request_id: String },
    /// 其他自定义来源。
    Custom { name: String, metadata: HashMap<String, Value> },
}
```

### 3.3 WorkItem

```rust
/// 推送到 Work 队列的工作单元。
///
/// 比 "Task" 更通用：可以是用户指派的任务、看板卡片、API 触发、
/// 定时提醒、Idle Boredom 找回的活——任何需要 Agent 执行的工作。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkItem {
    pub id: WorkItemId,
    pub title: String,
    pub description: String,

    /// 预设的执行步骤（可选）。
    /// 如果为空，Work System 调用 LLM 自行分解。
    pub steps: Option<Vec<Step>>,

    /// 优先级（队列内排序用）。
    pub priority: Priority,

    /// 执行超时。
    pub timeout: Option<Duration>,

    /// 附带的上下文。
    pub context: HashMap<String, Value>,

    /// 完成后是否通知调用方（通过 Global Bus）。
    pub notify_on_complete: bool,

    /// 创建时间。
    pub created_at: Timestamp,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Step {
    pub index: usize,
    pub description: String,
    pub tool: Option<String>,       // 指定工具（可选）
    pub expect_llm: bool,           // 是否需要 LLM 推理
    pub max_retries: u32,
}
```

### 3.4 WorkContext

```rust
#[derive(Debug, Clone)]
pub struct WorkContext {
    pub state: WorkState,
    /// FIFO 工作队列。
    pub queue: VecDeque<WorkItem>,
    /// 当前正在执行的工作项。
    pub current: Option<WorkItem>,
    /// 当前工作项的步骤列表。
    pub steps: Vec<Step>,
    /// 当前步骤索引。
    pub step_index: usize,
}

impl WorkContext {
    pub fn new() -> Self {
        Self {
            state: WorkState::Idle,
            queue: VecDeque::new(),
            current: None,
            steps: Vec::new(),
            step_index: 0,
        }
    }

    pub fn enqueue(&mut self, item: WorkItem) {
        self.queue.push_back(item);
    }

    pub fn dequeue(&mut self) -> Option<WorkItem> {
        self.queue.pop_front()
    }

    pub fn reset_to_idle(&mut self) {
        self.state = WorkState::Idle;
        self.current = None;
        self.steps.clear();
        self.step_index = 0;
    }
}
```

---

## 4. Core Execution Logic

### 4.1 Event Handler

```rust
impl WorkSystem {
    pub async fn handle(
        &mut self,
        event: WorkEvent,
        ctx: &mut WorkContext,
        local_bus: &dyn EventBus,
        global_bus: &dyn GlobalEventBus,
        idle_coord: &IdleCoordination,
        trace: &mut TraceStore,
    ) -> WorkResult<()> {
        match event {
            // ── Interrupt（最高优先级，任何状态）────────────────
            WorkEvent::Interrupt { reason, by_system } => {
                if ctx.state == WorkState::Busy {
                    let checkpoint = self.save_checkpoint(ctx);
                    trace.record(WorkTraceEvent::Interrupted { checkpoint, by_system });
                }
                ctx.reset_to_idle();
                return Ok(());
            }

            // ── 收到新工作项 ──────────────────────────────────
            WorkEvent::WorkItemAssigned { item, source } => {
                trace.record(WorkTraceEvent::ItemReceived {
                    item_id: item.id,
                    source: source.clone(),
                });
                ctx.enqueue(item);

                if ctx.state == WorkState::Idle {
                    ctx.state = WorkState::Busy;
                    let next = ctx.dequeue().unwrap();
                    self.start_item(next, ctx, local_bus).await?;
                }
                // else: 正在 BUSY，Item 已在队列中，当前完成后自动出队
            }

            // ── 工作项完成 ────────────────────────────────────
            WorkEvent::WorkItemCompleted { item_id, result, duration } => {
                trace.record(WorkTraceEvent::ItemCompleted {
                    item_id, duration, outcome: "completed".into(),
                });

                if let Some(ref item) = ctx.current {
                    if item.notify_on_complete {
                        global_bus.post(WorkItemResultEvent {
                            item_id,
                            result: result.clone(),
                            agent_id: self.agent_id.clone(),
                        }).await?;
                    }
                }

                idle_coord.inject(IdleSignal::Satisfaction { work_item_id: item_id });
                self.process_next(ctx, local_bus).await?;
            }

            // ── 工作项失败 ────────────────────────────────────
            WorkEvent::WorkItemFailed { item_id, error, retryable } => {
                trace.record(WorkTraceEvent::ItemFailed {
                    item_id,
                    error: error.to_string(),
                    retryable,
                });

                if retryable && self.should_retry(&error) {
                    if let Some(item) = ctx.current.take() {
                        ctx.queue.push_front(item); // 重新入队到头部
                    }
                } else {
                    global_bus.post(WorkItemFailedEvent {
                        item_id,
                        error: error.to_string(),
                        agent_id: self.agent_id.clone(),
                    }).await?;

                    idle_coord.inject(IdleSignal::Frustration {
                        reason: Some(error.to_string()),
                    });
                }

                self.process_next(ctx, local_bus).await?;
            }
        }
        Ok(())
    }

    /// 处理队列中下一个工作项；队列为空则切回 IDLE。
    async fn process_next(
        &mut self,
        ctx: &mut WorkContext,
        local_bus: &dyn EventBus,
    ) -> WorkResult<()> {
        match ctx.dequeue() {
            Some(next) => {
                self.start_item(next, ctx, local_bus).await?;
            }
            None => {
                // 队列空 → IDLE，Bus 变空，Idle System 自然接管
                ctx.reset_to_idle();
            }
        }
        Ok(())
    }

    /// 开始执行一个工作项：运行前置 Hook，分解步骤，投递首个 ExecuteStep。
    async fn start_item(
        &mut self,
        item: WorkItem,
        ctx: &mut WorkContext,
        local_bus: &dyn EventBus,
    ) -> WorkResult<()> {
        self.run_hooks(HookPoint::BeforeExecution, &item).await?;

        ctx.steps = match item.steps {
            Some(predefined) => predefined,
            None => self.decompose_with_llm(&item).await?,
        };
        ctx.step_index = 0;
        ctx.current = Some(item);

        // 投递首个执行步骤 → Bus 保持非空
        local_bus.post(StepEvent::Execute { step_index: 0 }).await?;
        Ok(())
    }
}
```

### 4.2 Step Execution (Internal)

步骤执行是 Work System 的内部循环，不暴露为 WorkEvent（只通过 `StepEvent` 内部流转）。
保持"一步完成即投递下一步"的链式模式，确保 Bus 持续非空。

```rust
impl WorkSystem {
    pub async fn execute_step(
        &mut self,
        step_index: usize,
        ctx: &mut WorkContext,
        local_bus: &dyn EventBus,
    ) -> WorkResult<()> {
        let step = &ctx.steps[step_index];
        let item = ctx.current.as_ref().unwrap();
        let start = Instant::now();

        // 步骤前置 Hook
        self.run_hooks(HookPoint::BeforeStep, item).await?;

        let result = if step.expect_llm {
            self.execute_llm_step(step, item).await
        } else if let Some(ref tool_name) = step.tool {
            self.execute_tool_step(tool_name, step, item).await
        } else {
            self.execute_simple_step(step, item).await
        };

        // 步骤后置 Hook
        self.run_hooks(HookPoint::AfterStep, item).await?;

        match result {
            Ok(_output) => {
                if step_index + 1 < ctx.steps.len() {
                    ctx.step_index = step_index + 1;
                    local_bus.post(StepEvent::Execute { step_index: step_index + 1 }).await?;
                } else {
                    let duration = start.elapsed();
                    let result = self.collect_result(ctx);
                    local_bus.post(WorkEvent::WorkItemCompleted {
                        item_id: item.id.clone(),
                        result,
                        duration,
                    }).await?;
                }
            }
            Err(error) => {
                if step_index < step.max_retries as usize {
                    local_bus.post(StepEvent::Execute { step_index }).await?;
                } else {
                    local_bus.post(WorkEvent::WorkItemFailed {
                        item_id: item.id.clone(),
                        error,
                        retryable: false,
                    }).await?;
                }
            }
        }
        Ok(())
    }
}
```

### 4.3 Bus Non-Empty Guarantee

```
WorkItemAssigned → IDLE→BUSY → ExecuteStep(0)
  → 执行 → ExecuteStep(1)
  → 执行 → ExecuteStep(2)
  → 执行 → WorkItemCompleted
  → dequeue → 有下一个? ExecuteStep(0) for next item
            → 无下一个? IDLE

执行期间 Bus 始终非空 → Idle System 不触发。
队列空 → Bus 空 → Idle System 自然接管。
```

无需任何 DelayedWorkTick、冷却计时器。

---

## 5. How External Systems Push Work

### 5.1 Unified Push Interface

```rust
/// 外部系统向 Agent 推送工作项的统一接口。
#[async_trait]
pub trait WorkItemPushChannel {
    /// 向指定 Agent 推送工作项。
    async fn push(
        &self,
        agent_id: &AgentId,
        item: WorkItem,
        source: WorkItemSource,
    ) -> Result<()>;

    /// 推送工作项，由全局调度器决定目标 Agent。
    async fn push_any(
        &self,
        item: WorkItem,
        source: WorkItemSource,
        strategy: DispatchStrategy,
    ) -> Result<AgentId>;
}

#[derive(Debug, Clone)]
pub enum DispatchStrategy {
    /// 指定目标。
    Direct(AgentId),
    /// 随机空闲 Agent。
    RandomIdle,
    /// 队列最短的 Agent。
    LeastLoaded,
    /// 根据能力标签匹配。
    BestMatch { capabilities: Vec<String> },
    /// 自定义（看板/全局调度器实现）。
    Custom(Box<dyn Fn(&[AgentStatus]) -> AgentId>),
}
```

### 5.2 CLI / API

```
用户: aman work assign --agent alice "fix bug #1234"

CLI:
  1. 构造 WorkItem { title: "fix bug #1234", ... }
  2. POST /api/v1/agents/alice/work/push
     Body: { item: {...}, source: "cli" }
  3. AgentRuntime → 构造 WorkItemAssigned 事件
  4. 投递到 alice 的 Local Event Bus
  5. WorkSystem.handle() 消费
```

### 5.3 Kanban / Team Board

```
┌──────────────────────────────────────────┐
│              Kanban Plugin               │
│                                          │
│  ┌──────────┐   ┌────────────────────┐  │
│  │ 看板 UI  │   │  Item Scheduler    │  │
│  │ (列/卡片)│   │                    │  │
│  └────┬─────┘   │  1. 监控 Backlog   │  │
│       │         │  2. 根据策略选择    │  │
│       │ 拖动卡片 │     目标 Agent     │  │
│       │ 到 "开发中"│  3. push(item)    │  │
│       │         │                    │  │
│       └─────────┤  策略:             │  │
│                 │  - 手动指派         │  │
│                 │  - 自动分配（空闲） │  │
│                 │  - 能力匹配         │  │
│                 │  - 负载均衡         │  │
│                 └────────────────────┘  │
└──────────────────────────────────────────┘

Agent 不看任务板，看板决定任务给谁。
```

### 5.4 Todo List

```
Todo Plugin:
  - 用户设定每日任务清单
  - 到达执行时间 → Todo Scheduler.push(item) 到配置的 Agent
  - Agent 执行 → 完成后通过 Global Bus 通知 Todo 更新状态
```

---

## 6. Hook Mechanism

### 6.1 Hook Points

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HookPoint {
    /// 工作项开始执行前。
    BeforeExecution,
    /// 每个步骤执行前。
    BeforeStep,
    /// 每个步骤执行后。
    AfterStep,
    /// 工作项执行完成后（无论成败）。
    AfterExecution,
    /// 工作项成功时。
    OnSuccess,
    /// 工作项失败时。
    OnFailure,
}
```

Hook 调用次序：

```
BeforeExecution
  ├─ BeforeStep → (execute) → AfterStep
  ├─ BeforeStep → (execute) → AfterStep
  ├─ ...
  └─ AfterExecution
       ├─ OnSuccess (if completed)
       └─ OnFailure (if failed)
```

### 6.2 Hook Registration

```rust
#[derive(Debug, Clone)]
pub struct Hook {
    pub name: String,
    pub point: HookPoint,
    pub action: HookAction,
    /// Hook 失败时是否中止整个 WorkItem。
    pub abort_on_failure: bool,
}

#[derive(Debug, Clone)]
pub enum HookAction {
    /// 调用内置工具。
    Tool { tool_name: String, params: HashMap<String, Value> },
    /// 调用 LLM（传入 WorkItem 上下文）。
    Llm { system_prompt: String, max_tokens: u32 },
    /// 发送事件到 Global Bus。
    EmitEvent { event_type: String, payload_template: String },
}
```

### 6.3 Configuration

```yaml
work:
  hooks:
    before_execution:
      - name: log_start
        action:
          type: tool
          tool_name: trace.record
          params:
            event: "work.item.started"

      - name: check_permissions
        action:
          type: llm
          system_prompt: "检查此工作项是否需要额外权限。如需确认，返回 'block'。"
        abort_on_failure: false

    before_step: []

    after_step:
      - name: update_progress
        action:
          type: emit_event
          event_type: "work.progress.updated"
          payload_template: |
            { "item_id": "{{item.id}}", "step": "{{step_index}}/{{total_steps}}" }

    after_execution: []

    on_success:
      - name: notify_completion
        action:
          type: emit_event
          event_type: "kanban.item.completed"

    on_failure:
      - name: log_failure
        action:
          type: tool
          tool_name: trace.record
          params:
            event: "work.item.failed"
```

### 6.4 Hook Execution

```rust
impl WorkSystem {
    async fn run_hooks(&self, point: HookPoint, item: &WorkItem) -> WorkResult<()> {
        for hook in &self.config.hooks.for_point(point) {
            let result = match &hook.action {
                HookAction::Tool { tool_name, params } => {
                    self.call_tool(tool_name, params, item).await
                }
                HookAction::Llm { system_prompt, max_tokens } => {
                    self.call_llm(system_prompt, *max_tokens, item).await
                }
                HookAction::EmitEvent { event_type, payload_template } => {
                    let payload = self.render_template(payload_template, item);
                    self.global_bus.emit(event_type, payload).await
                }
            };

            if result.is_err() && hook.abort_on_failure {
                return Err(WorkError::HookFailed {
                    hook: hook.name.clone(),
                    error: result.unwrap_err().to_string(),
                });
            }
        }
        Ok(())
    }
}
```

---

## 7. Integration with Idle System

### 7.1 Natural Collaboration

```
Work 队列空 + IDLE
  → Event Bus 为空
  → Idle System 运行（Daze → Boredom → ...）

外部推送 WorkItemAssigned
  → 事件进入 Event Bus → Bus 非空，Idle 停止
  → Work System 切到 BUSY，开始执行
  → 执行期间链式投递 StepEvent → Bus 持续非空

队列空 + 当前 Item 完成
  → Work 切回 IDLE
  → Bus 为空
  → Idle System 自然恢复
```

### 7.2 Boredom → SeekTask (Active Exploration)

Agent 在无聊时主动找活的需求完全归入 Idle System：

```rust
// IdleSystem 的 Boredom 处理中：
if self.boredom_level >= self.config.seek_task_threshold {
    global_bus.post(SeekTaskRequest {
        agent_id: self.agent_id,
        capabilities: self.capabilities.clone(),
    }).await?;
}

// 看板/全局调度器收到 SeekTaskRequest：
//   1. 查找适合该 Agent 的工作
//   2. 如果有 → push(agent_id, item, WorkItemSource::SeekResponse { ... })
//   3. 如果无 → 忽略
//
// Agent 收到 WorkItemAssigned(SeekResponse) → 退出 Boredom → 开始工作
```

Work System 不感知 SeekTask 协议，只接收结果。主动探索的拟人行为在 Idle 侧闭环。

### 7.3 Feedback Loop

```
WorkItemCompleted → IdleSignal::Satisfaction   → arousal ↑
WorkItemFailed    → IdleSignal::Frustration    → arousal ↓
```

直接在事件处理中调用 `idle_coord.inject()`，无需额外的注入路径。

---

## 8. Interrupt Protocol

与 v1 一致，但因只有 2 个状态而更简单：

```rust
impl AgentScheduler {
    pub async fn activate_system(&mut self, target: SystemKind, activation_event: Event) {
        if let Some(active) = self.active_system {
            if active != target {
                self.local_bus.post(WorkEvent::Interrupt {
                    reason: format!("{:?}_activated", target),
                    by_system: target.to_string(),
                }).await?;
            }
        }
        self.local_bus.post(activation_event).await?;
    }
}
```

| 触发条件 | 行为 |
|---------|------|
| 用户切换（"别工作了，学习吧"） | Interrupt → Work save checkpoint → IDLE → 激活 Study |
| 高优事件到达 | Interrupt → IDLE → 路由事件到其他系统 |
| Work 自然完成（队列空） | 自己回到 IDLE，无需 Interrupt |

---

## 9. Configuration

```yaml
work:
  execution:
    # 是否启用 LLM 自动分解步骤（当 WorkItem.steps 为空时）
    auto_decompose: true
    # 单步最大执行时间
    step_timeout: 120s
    # 工作项之间的可选冷却（0 = 无冷却）
    inter_item_cooldown: 0s

  hooks:
    before_execution: []
    before_step: []
    after_step: []
    after_execution: []
    on_success: []
    on_failure: []

  queue:
    # 最大队列长度（超过后拒绝新 WorkItem）
    max_size: 100
    # 是否启用优先级队列（false = 纯 FIFO）
    priority_queue: false

  retry:
    max_step_retries: 3
    retry_delay: 5s
```

对比 v1：不再有 `capabilities`、`auto_claim`、`selection strategy`、`claim_retry`、`board`、`review`——这些要么属于外部调度器，要么通过 Hook 实现。

---

## 10. Runtime Integration

与 v1 相同：Phase 4 初始化，Phase 0 销毁。构造更简单：

```rust
impl AgentBuilder {
    pub fn build(self) -> Result<AgentRuntime> {
        let work_sys = WorkSystem::new(
            self.config.work.clone(),
            local_bus.clone(),
            global_bus.clone(),
            self.persistence.trace_store(),
        );
        // ...
    }
}
```

---

## 11. Event Routing

```yaml
routes:
  - match: { event_type: "work.item.assigned" }   → handler:work
  - match: { event_type: "work.item.completed" }  → handler:work
  - match: { event_type: "work.item.failed" }     → handler:work
  - match: { event_type: "work.interrupt" }       → handler:work
```

说明：
- 事件类型的 wire name 使用 `work.item.*`（snake_case 的 `WorkItemAssigned` → `work.item.assigned`）
- `StepEvent` 是内部流转事件，不走路由表，直接在 WorkSystem 内部消费
- `SeekTaskRequest` / `SeekTaskResponse` 是 Idle System 的事件，不经过 Work System

---

## 12. Migration Path from v1

| 删除 | 替换为 |
|------|-------|
| `WorkState::Checking` | 删除 |
| `WorkState::Claiming` | 删除 |
| `WorkState::Reviewing` | `on_success` / `on_failure` Hook |
| `WorkState::Executing` | `WorkState::Busy` |
| `Task` 类型 | `WorkItem` |
| `WorkTick` / `DelayedWorkTick` | 外部推送 |
| `StartCheck` / `ClaimTask` / `ClaimResponse` | 删除 |
| `ReviewTask` / `ReviewComplete` | Hook |
| `TaskBoardUpdated` | 移到看板插件内部 |
| `WorkPersonality` (capabilities, selection, claim_retry) | 外部调度器配置 |
| `WorkBoardClient` trait | `WorkItemPushChannel` trait |

保留：
- `Interrupt` 事件 + checkpoint 机制
- `IdleSignal` 注入（简化路径）
- Phase 4 初始化 / Phase 0 销毁
- Per-Agent 架构 + Trace Store 集成

---

## 13. Summary

| 维度 | v1 (主动拉取) | v2 (被动推送) |
|------|-------------|-------------|
| **状态** | 5 | 2 (IDLE/BUSY) |
| **事件类型** | 10+ | 3 + Interrupt |
| **巡检** | DelayedWorkTick 定时器 | 无 |
| **认领** | Agent 间乐观锁竞争 | 外部调度器决定 |
| **Bus 非空保证** | ExecuteStep + DelayedWorkTick | 链式 ExecuteStep |
| **Idle 协作** | DelayedWorkTick 控制节奏 | 队列空时自然空 |
| **Hook** | 无 | 6 个 Hook 点 |
| **主动找活** | Work System 自身 | Idle Boredom → SeekTask |
| **配置项** | 10+ 项（含 board、review、claim_retry 等） | 4 组（execution、hooks、queue、retry） |

**核心原则**：
1. Work System 就是一个带 Hook 的 FIFO 工作队列消费者。
2. 谁做什么由外部调度器决定，Agent 只负责执行。
3. 队列空时 Idle System 自然运行，无需任何协调代码。
4. 「主动找活」保留在 Idle Boredom 中，不污染 Work 的简洁性。
