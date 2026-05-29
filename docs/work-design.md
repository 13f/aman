# Work System — Architecture Design (v3: Lifecycle Engine)

> **核心变更**：从「独立 2 状态机」重构为「LifecycleEngine 的领域适配层」。
> Work/Study/Daily-Life 三个系统的共用逻辑（状态机、FIFO 队列、步骤链式执行、
> 中断/重试、IdleSignal 反馈、全局总线通知）全部提取到 `crates/lifecycle`。
> Work System 只需实现 `SystemSpec` trait，提供 Work 领域特有的类型和逻辑。
>
> 架构层次：
> ```
> LifecycleEngine<WorkSpec>   ← 通用引擎（lifecycle crate）
>   └─ WorkSpec              ← 领域适配（work crate，实现 SystemSpec trait）
>        ├─ Item  = WorkItem
>        ├─ Step  = Step
>        ├─ decompose()       → 工作分解策略（预定义步骤 / LLM 自动分解）
>        ├─ execute_step_impl() → 步骤执行（LLM / Tool）
>        └─ collect_result()  → 结果收集
> ```

---

## 1. Why This Refactoring

v2 中 Work、Study、Daily-Life 三个系统各自实现了几乎相同的：

| 重复逻辑 | v2 做法 | v3 做法 |
|---------|--------|--------|
| 2 状态机 (Idle/Busy) | 每个系统一份 | `LifecycleEngine` 统一管理 |
| FIFO 队列 + 上下文 | 每个系统一个 `XxxContext` | `LifecycleContext<I, St>` 泛型 |
| 步骤链式执行 | 每个系统手写 `advance_pipeline` | 引擎内部 `execute_step` → `publish_step_event` |
| Interrupt → checkpoint | 每个系统手写 | `engine.handle_interrupt()` |
| IdleSignal 发送 | 每个系统手写 mpsc channel | 引擎内部 `send_idle_signal()` |
| 全局总线通知 | 每个系统手写 | 引擎在 complete/fail 时自动发布 |
| 重试逻辑 | 每个系统手写 `push_front` | 引擎内部 `handle_failed()` |

提取后，每个系统从 ~500 行事件处理代码缩减为 ~100 行 `SystemSpec` 实现 + ~100 行薄封装。

---

## 2. Lifecycle Engine Architecture

### 2.1 Shared Engine (`crates/lifecycle`)

```rust
/// 泛型生命周期引擎。S 是实现 SystemSpec 的领域适配器。
pub struct LifecycleEngine<S: SystemSpec> {
    agent_id: String,
    spec: S,                                       // 领域适配器
    ctx: Mutex<LifecycleContext<S::Item, S::Step>>, // 泛型上下文
    local_bus: Arc<dyn EventBus>,
    global_bus: Arc<dyn EventBus>,
    idle_signal_tx: Mutex<Option<mpsc::UnboundedSender<IdleSignal>>>,
    // ...
}
```

引擎提供的方法：

| 方法 | 用途 |
|------|------|
| `handle_assigned(item, source)` | 入队，IDLE → BUSY，开始执行 |
| `handle_completed(item_id, result, duration)` | 通知全局总线，发 IdleSignal，处理下一个 |
| `handle_failed(item_id, error, retryable)` | 可重试则重新入队，否则发通知 + IdleSignal |
| `handle_interrupt(reason, by_system)` | 保存 checkpoint → 无条件 IDLE |
| `handle_step(step_index)` | 执行当前步骤，自动推进或完成 |
| `current_state()` / `snapshot()` | 查询当前状态和上下文 |

### 2.2 SystemSpec Trait

```rust
pub trait SystemSpec: Send + Sync + 'static {
    type Item: Clone + Send + Sync + Serialize + 'static;
    type Step: Clone + Send + Sync + 'static;

    // ── 事件路由常量 ──
    fn event_source() -> &'static str;       // "work.system"
    fn step_event_kind() -> &'static str;    // "work.step.execute"
    fn assigned_kind() -> &'static str;      // "work.item.assigned"
    fn completed_kind() -> &'static str;     // "work.item.completed"
    fn failed_kind() -> &'static str;        // "work.item.failed"
    fn interrupt_kind() -> &'static str;     // "work.interrupt"

    // ── Item 访问器 ──
    fn item_id(item: &Self::Item) -> String;
    fn notify_on_complete(item: &Self::Item) -> bool;

    // ── 事件负载序列化 ──
    fn serialize_item(item: &Self::Item) -> serde_json::Value;
    fn make_assigned_payload(...) -> serde_json::Value;
    fn make_completed_payload(...) -> serde_json::Value;
    fn make_failed_payload(...) -> serde_json::Value;
    fn make_step_payload(step_index: usize) -> serde_json::Value;
    fn make_result_notify(...) -> serde_json::Value;
    fn make_failure_notify(...) -> serde_json::Value;

    // ── 领域逻辑 ──
    fn default_step(item: &Self::Item, max_retries: u32) -> Self::Step;
    fn step_max_retries(step: &Self::Step) -> u32;
    fn decompose(&self, item: &Self::Item, max_retries: u32) -> Vec<Self::Step>;
    fn execute_step_impl(&self, item: &Self::Item, step: &Self::Step, step_index: usize) -> Result<StepOutput, LifecycleError>;
    fn collect_result(item: &Self::Item, outputs: &[StepOutput]) -> serde_json::Value;
    fn completion_signal(item: &Self::Item) -> IdleSignal;
}
```

### 2.3 WorkSystem — Thin Wrapper

```rust
pub struct WorkSystem {
    engine: LifecycleEngine<WorkSpec>,  // 泛型引擎
    config: WorkConfig,
    local_bus: Arc<dyn EventBus>,
    global_bus: Arc<dyn EventBus>,
    idle_signal_tx: Mutex<Option<mpsc::UnboundedSender<IdleSignal>>>,
}

impl WorkSystem {
    pub fn new(agent_id, config, local_bus, global_bus, system_state) -> Self {
        let spec = WorkSpec::new(config.execution.auto_decompose);
        let engine = LifecycleEngine::new(
            agent_id, spec,
            config.queue.max_size,
            config.retry.max_step_retries,
            local_bus, global_bus,
            system_state,
            AgentSystemState::Working,  // BUSY 时设置的系统状态
        );
        // ...
    }

    pub async fn handle(&self, event: WorkEvent) -> WorkResult<()> {
        match event {
            WorkEvent::Interrupt { reason, by_system } => {
                // 记录 trace → 委托引擎
                self.engine.handle_interrupt(&reason, &by_system).await?;
            }
            WorkEvent::WorkItemAssigned { item, source } => {
                // 记录 trace → 委托引擎
                self.engine.handle_assigned(item, source_json).await?;
            }
            WorkEvent::WorkItemCompleted { item_id, result, duration } => {
                // 领域通知 + trace → 委托引擎
                self.engine.handle_completed(&item_id, result_json, duration).await?;
            }
            WorkEvent::WorkItemFailed { item_id, error, retryable } => {
                // 领域通知 + trace → 委托引擎
                self.engine.handle_failed(&item_id, lc_error, retryable).await?;
            }
        }
    }
}
```

---

## 3. State Machine (provided by LifecycleEngine)

### 3.1 Two States

```rust
// 定义在 lifecycle::types
pub enum LifecycleState {
    Idle,   // 队列为空，无工作执行
    Busy,   // 正在执行当前 WorkItem 的某个步骤
}
```

### 3.2 State Transitions

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
```

引擎在状态切换时自动更新 `AgentSystemState`：
- `LifecycleState::Idle` → `AgentSystemState::Idle`
- `LifecycleState::Busy` → `AgentSystemState::Working`（构造时传入）

### 3.3 Context (provided by LifecycleEngine)

```rust
// 泛型上下文，引擎内部使用
pub struct LifecycleContext<I: Clone, St: Clone> {
    pub state: LifecycleState,
    pub queue: VecDeque<I>,        // FIFO 工作队列
    pub current: Option<I>,        // 当前执行的工作项
    pub steps: Vec<St>,            // 当前工作项的步骤列表
    pub step_index: usize,         // 当前步骤索引
    pub step_outputs: Vec<StepOutput>,  // 累积的步骤输出
}
```

WorkSystem 通过 `engine.snapshot()` 获取上下文快照（用于测试和外部查询）。

---

## 4. Domain Types (work-specific)

### 4.1 WorkEvent

```rust
pub enum WorkEvent {
    WorkItemAssigned {
        item: WorkItem,
        source: WorkItemSource,
    },
    WorkItemCompleted {
        item_id: WorkItemId,
        result: WorkItemResult,
        duration: Duration,
    },
    WorkItemFailed {
        item_id: WorkItemId,
        error: WorkError,
        retryable: bool,
    },
    Interrupt {
        reason: String,
        by_system: String,
    },
}
```

### 4.2 WorkItemSource

```rust
pub enum WorkItemSource {
    Cli { operator: String },
    Api { endpoint: String, operator: String },
    Kanban { board_id: String, scheduler: String },
    Todo { list_id: String },
    SeekResponse { request_id: String },
    Custom { name: String, metadata: HashMap<String, Value> },
}
```

### 4.3 WorkItem

```rust
pub struct WorkItem {
    pub id: WorkItemId,
    pub title: String,
    pub description: String,
    pub steps: Option<Vec<Step>>,    // 预设步骤（可选，为空时由 Spec 分解）
    pub priority: Priority,
    pub timeout: Option<Duration>,
    pub context: HashMap<String, Value>,
    pub notify_on_complete: bool,
    pub created_at: Timestamp,
}

pub struct Step {
    pub index: usize,
    pub description: String,
    pub tool: Option<String>,
    pub expect_llm: bool,
    pub max_retries: u32,
}
```

---

## 5. WorkSpec — Domain Adapter

`WorkSpec` 是 Work 领域对 `SystemSpec` trait 的实现，是 Work System 的核心。

### 5.1 Step Decomposition

```rust
impl SystemSpec for WorkSpec {
    type Item = WorkItem;
    type Step = Step;

    async fn decompose(&self, item: &WorkItem, max_retries: u32) -> Vec<Step> {
        // 1. 有预设步骤 → 直接使用
        if let Some(ref predefined) = item.steps {
            if !predefined.is_empty() {
                return predefined.clone();
            }
        }

        // 2. 未开启自动分解 → 返回空，引擎使用 default_step
        if !self.auto_decompose {
            return vec![];
        }

        // 3. LLM 自动分解（placeholder → 实际调用 LLM）
        let mut steps = vec![Step {
            index: 0,
            description: format!("Analyze: {}", item.title),
            expect_llm: true,
            max_retries: 1,
            ..
        }];

        if item.description.contains("code") || item.description.contains("fix") {
            steps.push(Step {
                description: format!("Implement: {}", item.title),
                tool: Some("file".into()),
                expect_llm: true,
                max_retries,
                ..
            });
        }

        steps.push(Step {
            description: format!("Finalize: {}", item.title),
            expect_llm: true,
            max_retries: 1,
            ..
        });
        steps
    }
}
```

引擎行为：
- `decompose()` 返回空 → 引擎调用 `default_step()` 创建单步骤
- `decompose()` 返回步骤列表 → 引擎直接使用，依次执行

### 5.2 Step Execution

```rust
async fn execute_step_impl(
    &self,
    _item: &WorkItem,
    step: &Step,
    _step_index: usize,
) -> Result<StepOutput, LifecycleError> {
    // 实际集成中：根据 step.expect_llm / step.tool 调用 LLM 或工具
    Ok(StepOutput {
        success: true,
        summary: format!("Completed: {}", step.description),
        artifacts: Vec::new(),
        duration: std::time::Duration::from_millis(50),
    })
}
```

### 5.3 Event Routing Constants

```rust
fn event_source() -> &'static str { "work.system" }
fn step_event_kind() -> &'static str { "work.step.execute" }
fn assigned_kind() -> &'static str { "work.item.assigned" }
fn completed_kind() -> &'static str { "work.item.completed" }
fn failed_kind() -> &'static str { "work.item.failed" }
fn interrupt_kind() -> &'static str { "work.interrupt" }
```

### 5.4 Result Collection

```rust
fn collect_result(_item: &WorkItem, outputs: &[StepOutput]) -> serde_json::Value {
    let steps_completed = outputs.iter().filter(|o| o.success).count();
    let steps_failed = outputs.iter().filter(|o| !o.success).count();
    serde_json::json!({
        "outcome": "completed",
        "steps_completed": steps_completed,
        "steps_failed": steps_failed,
    })
}
```

---

## 6. Bus Non-Empty Guarantee (handled by Engine)

```
WorkItemAssigned → IDLE→BUSY → engine.start_item() → publish_step_event(0)
  → 执行 → publish_step_event(1)
  → 执行 → publish_step_event(2)
  → 执行 → finish_item() → publish completed event
  → dequeue → 有下一个? start_item() for next item
            → 无下一个? IDLE

执行期间 Bus 始终非空 → Idle System 不触发。
队列空 → Bus 空 → Idle System 自然接管。
```

步骤之间的链式推进由引擎内部管理，`WorkSystem` 无需关心。

---

## 7. How External Systems Push Work

### 7.1 Unified Push Interface

```rust
pub trait WorkItemPushChannel {
    async fn push(&self, agent_id: &AgentId, item: WorkItem, source: WorkItemSource) -> Result<()>;
    async fn push_any(&self, item: WorkItem, source: WorkItemSource, strategy: DispatchStrategy) -> Result<AgentId>;
}

pub enum DispatchStrategy {
    Direct(AgentId),
    RandomIdle,
    LeastLoaded,
    BestMatch { capabilities: Vec<String> },
    Custom(Box<dyn Fn(&[AgentStatus]) -> AgentId>),
}
```

### 7.2 CLI / API

```
aman work assign --agent alice "fix bug #1234"
  → POST /api/v1/agents/alice/work/push
  → AgentRuntime → WorkItemAssigned 事件
  → WorkSystem.handle() → engine.handle_assigned()
```

### 7.3 Kanban / Team Board

Kanban 调度器决定目标 Agent → `push(agent_id, item)` → Agent 的 Work 队列。

### 7.4 Todo List

到达执行时间 → Todo Scheduler `push(item)` → Agent 执行 → 完成后全局总线通知 Todo 更新状态。

---

## 8. IdleSignal Feedback (handled by Engine)

引擎在完成/失败时自动发送 `IdleSignal`：

```rust
// 引擎内部的 handle_completed:
self.send_idle_signal(IdleSignal::Satisfaction { item_id }).await;

// 引擎内部的 handle_failed (非重试):
self.send_idle_signal(IdleSignal::Frustration { reason: Some(error.message) }).await;
```

`WorkSystem` 通过 `set_idle_signal_tx()` 将 mpsc channel 传递给引擎，无需在事件处理中手动发送。

---

## 9. Configuration

```yaml
work:
  execution:
    auto_decompose: true       # 当 WorkItem.steps 为空时，LLM 自动分解步骤
    step_timeout: 120s
    inter_item_cooldown: 0s

  queue:
    max_size: 100
    priority_queue: false

  retry:
    max_step_retries: 3
    retry_delay: 5s
```

---

## 10. Runtime Integration

```rust
impl AgentBuilder {
    pub fn build(self) -> Result<AgentRuntime> {
        let work_sys = WorkSystem::new(
            self.config.agent_id.clone(),
            self.config.work.clone(),
            local_bus.clone(),
            global_bus.clone(),
            Some(system_state.clone()),
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

`StepEvent`（`work.step.execute`）是内部事件，引擎自己发布和消费，不经过路由表。

---

## 12. Summary

| 维度 | v2 (独立实现) | v3 (Lifecycle Engine) |
|------|-------------|----------------------|
| **状态机** | 每个系统手写 | `LifecycleEngine` 统一提供 |
| **队列管理** | 每个系统一个 `XxxContext` | `LifecycleContext<I, St>` 泛型 |
| **步骤链** | 手写 `advance_pipeline` / `process_next` | 引擎内部自动推进 |
| **Interrupt** | 手写 checkpoint 保存 | `engine.handle_interrupt()` |
| **重试** | 手写 `push_front` | `engine.handle_failed()` |
| **IdleSignal** | 手写 mpsc 发送 | 引擎自动发送 |
| **全局通知** | 手写 `global_bus.publish` | 引擎自动发布 |
| **领域代码量** | ~500 行 | ~100 行 (spec + wrapper) |

**核心原则**：
1. Work System 是 `LifecycleEngine<WorkSpec>` 的薄封装。
2. 所有通用的队列/状态/步骤/中断/重试逻辑在 lifecycle crate 中统一维护。
3. Work 领域特有逻辑（分解策略、执行方式、结果收集）集中在 `WorkSpec` 中。
4. 三个系统（Work/Study/Daily-Life）的引擎行为一致，修改引擎一处即可惠及全部。
