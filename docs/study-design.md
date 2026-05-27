# Study System — Architecture Design (v2: Passive Push Queue)

> **核心变更**：与 Work System v2 对齐——从「主动巡检 + 自主发现」退化为「被动队列消费者」。
> 外部系统 (CLI/API, Idle Exploration, RSS/arXiv, Review Scheduler) 直接将 StudyItem
> 推送到 Agent 的 Study 队列，Study System 只负责按深度执行学习流程，不做发现、不做巡检。
>
> 为什么叫 **StudyItem**？与 WorkItem 对应——推送到 Study 队列的可以是用户指派的学习任务、
> Idle Exploration 升级的好奇心主题、RSS 新论文、间隔复习提醒等。是通用的"学习工作单元"。

---

## 1. Why This Simplification

旧设计（v1）的问题：

| 问题 | v1 做法 | 实际需求 |
|------|--------|---------|
| 主动发现 | DelayedStudyTick 周期性巡检新材料 | 新材料由 RSS/arXiv 插件推送，用户任务由 CLI/API 直接指派 |
| 状态爆炸 | 6 状态 + 15+ 事件类型 | 学习流程是内部步骤序列，不需要每个阶段都暴露为状态 |
| 巡检回退 | 发现为空时指数退避 | 不需要发现环节，外部有材料时才推送 |
| 事件复杂性 | StartDiscover, DiscoverComplete, StartPlan, PlanComplete, LearnModule, ModuleComplete, StartPractice, PracticeComplete, StartConsolidate, ConsolidateComplete... | 这些都是内部 StepEvent，不需要暴露为 StudyEvent |
| Idle 耦合 | Exploration→Study 升级走单独事件类型 | 统一为 StudyItemAssigned(IdleExploration)，与其他来源无区别 |

核心理念转变：

```
旧：Agent 主动发现材料、规划路径、深度学习、间隔复习
    → Study System 承担了「学习调度器」的职责

新：外部系统决定学什么、什么时候学，Agent 只负责按流程执行
    → Study System 就是一个带 Hook 的 FIFO 学习队列消费者
```

谁来决定「学什么、什么时候学」？
- 用户直接指派 → CLI/API
- RSS/arXiv 新论文 → Material 插件发现后推送
- Idle Exploration 发现有趣主题 → Idle 系统推送
- 间隔复习到期 → Review Scheduler（Study 内部定时器）推送

**关键区别**：Review Scheduler 仍然在 Study 内部（因为间隔重复算法是 Study 的核心能力），
但它不触发"巡检"——它只在 Consolidation 完成后注册定时回调，到期时推送
`StudyItemAssigned(source=ScheduledReview)`，与任何其他来源完全一样。

---

## 2. Simplified State Machine

### 2.1 Two States

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StudyState {
    /// 队列为空，无学习活动。Event Bus 空闲时 Idle System 自然运行。
    Idle,
    /// 正在执行当前 StudyItem 的某个阶段。Bus 保持非空，Idle 不触发。
    Busy,
}
```

### 2.2 State Transitions

```
                  StudyItemAssigned
     ┌───────┐  ────────────────────  ┌───────┐
     │ IDLE  │                         │ BUSY  │
     └───┬───┘                         └───┬───┘
         │                                  │
         │  Interrupt                       │  当前 Item 完成 + 队列有下一个
         │  (any state → IDLE)              │  → 继续 BUSY
         │                                  │
         │                                  │  当前 Item 完成 + 队列为空
         │  ◄───────────────────────────────┘  → IDLE
         │
         │  StudyItemAssigned (while IDLE)
         │  → IDLE → BUSY
         │
         └─────────────────────────────────

    Interrupt: 任何状态收到 → 保存 checkpoint → 无条件切回 IDLE。
```

### 2.3 Comparison with v1

| 维度 | v1 (主动巡检) | v2 (被动推送) |
|------|-------------|-------------|
| 状态数 | 6 (IDLE/DISCOVERING/PLANNING/LEARNING/PRACTICING/CONSOLIDATING) | 2 (IDLE/BUSY) |
| 事件类型 | 15+ | 3 + Interrupt |
| 巡检 | DelayedStudyTick 定时器 | 无 |
| 学习流程 | 6 个状态，每个有独立事件 | 内部 Step Pipeline，不暴露为事件 |
| 材料发现 | Study 内部 DISCOVERING 阶段 | 外部（RSS/arXiv/Idle Exploration）推送 |
| Idle 协作 | ExplorationUpgraded 单独事件 | 统一为 StudyItemAssigned(IdleExploration) |
| 间隔复习 | DelayedReviewTick | 定时回调 → StudyItemAssigned(ScheduledReview) |
| 中断 | 5 个活跃状态均可 Interrupt | 1 个活跃状态 (BUSY) |

---

## 3. Type System

### 3.1 StudyEvent

```rust
/// Study System 的领域事件——只有 3 个业务事件 + 1 个系统事件。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum StudyEvent {
    /// 外部系统推送学习项到 Agent。
    /// 来源：用户指派、Idle Exploration、RSS/arXiv、间隔复习调度等。
    StudyItemAssigned {
        item: StudyItem,
        source: StudyItemSource,
    },

    /// 当前学习项完成。
    StudyItemCompleted {
        item_id: StudyItemId,
        outcome: StudyOutcome,
        duration: Duration,
    },

    /// 当前学习项失败。
    StudyItemFailed {
        item_id: StudyItemId,
        error: StudyError,
        retryable: bool,
    },

    /// 中断当前学习，强制切回 IDLE。
    Interrupt {
        reason: String,
        by_system: String,
    },
}
```

### 3.2 StudyItemSource

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum StudyItemSource {
    /// 用户通过 CLI/API 显式指派。
    UserAssigned { operator: String },
    /// Idle Exploration 发现有趣主题，升级为学习任务。
    IdleExploration { curiosity_topic: String },
    /// RSS/arXiv 等材料订阅源推送的新内容。
    MaterialSubscription { feed_url: String },
    /// 间隔重复复习调度到期。
    ScheduledReview { node_id: KnowledgeNodeId, review_round: u32 },
    /// Idle Boredom 下 Agent 主动 SeekStudy 后，调度器响应。
    SeekResponse { request_id: String },
    /// 其他自定义来源。
    Custom { name: String, metadata: HashMap<String, Value> },
}
```

### 3.3 StudyItem

```rust
/// 推送到 Study 队列的学习工作单元。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StudyItem {
    pub id: StudyItemId,
    pub topic: String,

    /// 学习材料（可选）。为空时由 Study System 自行搜索（调用 LLM/Tool）。
    pub materials: Option<Vec<MaterialRef>>,

    /// 学习深度——决定内部执行流程的复杂度。
    pub depth: StudyDepth,

    /// 优先级。
    pub priority: Priority,

    /// 执行超时。
    pub timeout: Option<Duration>,

    /// 附带的上下文。
    pub context: HashMap<String, Value>,

    /// 创建时间。
    pub created_at: Timestamp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum StudyDepth {
    /// 略读：标题+摘要+结论，不记笔记，不连接知识图谱。
    Skim,
    /// 通读：完整阅读，记要点笔记，连接知识图谱。
    Read,
    /// 深度学习：完整阅读 + 笔记 + 练习 + 知识图谱连接 + 间隔重复调度。
    Deep,
}
```

### 3.4 StudyContext

```rust
#[derive(Debug, Clone)]
pub struct StudyContext {
    pub state: StudyState,
    /// FIFO 学习队列。
    pub queue: VecDeque<StudyItem>,
    /// 当前正在执行的学习项。
    pub current: Option<StudyItem>,
    /// 当前学习路径（Plan 阶段生成）。
    pub learning_path: Option<LearningPath>,
    /// 当前模块索引。
    pub module_index: usize,
    /// 学习过程中积累的笔记。
    pub accumulated_notes: StudyNotes,
}

impl StudyContext {
    pub fn new() -> Self {
        Self {
            state: StudyState::Idle,
            queue: VecDeque::new(),
            current: None,
            learning_path: None,
            module_index: 0,
            accumulated_notes: StudyNotes::default(),
        }
    }

    pub fn enqueue(&mut self, item: StudyItem) {
        self.queue.push_back(item);
    }

    pub fn dequeue(&mut self) -> Option<StudyItem> {
        self.queue.pop_front()
    }

    pub fn reset_to_idle(&mut self) {
        self.state = StudyState::Idle;
        self.current = None;
        self.learning_path = None;
        self.module_index = 0;
        self.accumulated_notes = StudyNotes::default();
    }
}
```

### 3.5 Learning Depth → Internal Pipeline

学习深度决定内部执行步骤，而不是暴露为独立状态：

```
StudyDepth::Skim:
  StudyItemAssigned → BUSY
    → internal: scan materials (single pass, no notes)
    → StudyItemCompleted

StudyDepth::Read:
  StudyItemAssigned → BUSY
    → internal: plan modules
    → internal: learn each module (chain, with notes)
    → internal: consolidate (write notes + connect knowledge graph)
    → StudyItemCompleted

StudyDepth::Deep:
  StudyItemAssigned → BUSY
    → internal: plan modules
    → internal: learn each module (chain, with notes)
    → internal: practice (generates exercises, self-test)
    → internal: consolidate (notes → memory, knowledge graph connections,
                             schedule spaced repetition reviews)
    → StudyItemCompleted
```

---

## 4. Core Execution Logic

### 4.1 Event Handler

```rust
impl StudySystem {
    pub async fn handle(
        &mut self,
        event: StudyEvent,
        ctx: &mut StudyContext,
        local_bus: &dyn EventBus,
        memory: &mut MemoryStore,
        knowledge_graph: &mut KnowledgeGraphStore,
        trace: &mut TraceStore,
    ) -> StudyResult<()> {
        match event {
            // ── Interrupt（最高优先级，任何状态）────────────────
            StudyEvent::Interrupt { reason, by_system } => {
                if ctx.state == StudyState::Busy {
                    let checkpoint = self.save_checkpoint(ctx);
                    trace.record(StudyTraceEvent::Interrupted { checkpoint, by_system });
                }
                ctx.reset_to_idle();
                return Ok(());
            }

            // ── 收到新学习项 ──────────────────────────────────
            StudyEvent::StudyItemAssigned { item, source } => {
                trace.record(StudyTraceEvent::ItemReceived {
                    item_id: item.id,
                    topic: item.topic.clone(),
                    source: source.clone(),
                });
                ctx.enqueue(item);

                if ctx.state == StudyState::Idle {
                    ctx.state = StudyState::Busy;
                    let next = ctx.dequeue().unwrap();
                    self.start_item(next, ctx, local_bus, memory, knowledge_graph).await?;
                }
            }

            // ── 学习项完成 ────────────────────────────────────
            StudyEvent::StudyItemCompleted { item_id, outcome, duration } => {
                trace.record(StudyTraceEvent::ItemCompleted {
                    item_id, outcome: outcome.clone(), duration,
                });

                // 如果是 ScheduledReview，更新 SM-2 参数
                if let Some(ref item) = ctx.current {
                    if let StudyItemSource::ScheduledReview { node_id, .. } = item.context.get("source") {
                        // 更新知识节点的复习记录
                    }
                }

                self.process_next(ctx, local_bus, memory, knowledge_graph).await?;
            }

            // ── 学习项失败 ────────────────────────────────────
            StudyEvent::StudyItemFailed { item_id, error, retryable } => {
                trace.record(StudyTraceEvent::ItemFailed {
                    item_id, error: error.to_string(), retryable,
                });

                if retryable && self.should_retry(&error) {
                    if let Some(item) = ctx.current.take() {
                        ctx.queue.push_front(item);
                    }
                }

                self.process_next(ctx, local_bus, memory, knowledge_graph).await?;
            }
        }
        Ok(())
    }

    async fn process_next(
        &mut self,
        ctx: &mut StudyContext,
        local_bus: &dyn EventBus,
        memory: &mut MemoryStore,
        knowledge_graph: &mut KnowledgeGraphStore,
    ) -> StudyResult<()> {
        match ctx.dequeue() {
            Some(next) => {
                self.start_item(next, ctx, local_bus, memory, knowledge_graph).await?;
            }
            None => {
                ctx.reset_to_idle();
            }
        }
        Ok(())
    }

    async fn start_item(
        &mut self,
        item: StudyItem,
        ctx: &mut StudyContext,
        local_bus: &dyn EventBus,
        memory: &mut MemoryStore,
        knowledge_graph: &mut KnowledgeGraphStore,
    ) -> StudyResult<()> {
        self.run_hooks(HookPoint::BeforeExecution, &item).await?;

        ctx.current = Some(item);

        // 根据 StudyDepth 选择不同的内部 Pipeline
        let pipeline = match ctx.current.as_ref().unwrap().depth {
            StudyDepth::Skim => StudyPipeline::skim(),
            StudyDepth::Read => StudyPipeline::read(),
            StudyDepth::Deep => StudyPipeline::deep(),
        };

        // 投递首个内部步骤
        local_bus.post(StudyStepEvent::Execute { phase: pipeline.first_phase() }).await?;
        Ok(())
    }
}
```

### 4.2 Internal Step Execution (Pipeline)

学习流程的各个阶段（Discover Materials / Plan / Learn / Practice / Consolidate）
是 Study System 的**内部步骤**，通过 `StudyStepEvent` 链式执行，不暴露为 `StudyEvent`。

```rust
/// 内部步骤事件——只在 Study System 内部流转。
#[derive(Debug, Clone)]
enum StudyStepEvent {
    Execute { phase: StudyPhase },
    PhaseComplete { phase: StudyPhase, output: PhaseOutput },
    PhaseFailed { phase: StudyPhase, error: StudyError },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StudyPhase {
    /// 获取/搜索材料（当 StudyItem.materials 为空时）。
    GatherMaterials,
    /// 制定学习路径。
    Plan,
    /// 学习单个模块。
    LearnModule { index: usize },
    /// 练习。
    Practice,
    /// 巩固（笔记写入、知识图谱连接、间隔复习调度）。
    Consolidate,
}

impl StudySystem {
    pub async fn execute_phase(
        &mut self,
        phase: StudyPhase,
        ctx: &mut StudyContext,
        local_bus: &dyn EventBus,
        memory: &mut MemoryStore,
        knowledge_graph: &mut KnowledgeGraphStore,
    ) -> StudyResult<()> {
        let item = ctx.current.as_ref().unwrap();

        self.run_hooks(HookPoint::BeforePhase, item).await?;

        let result = match phase {
            StudyPhase::GatherMaterials => {
                if item.materials.is_some() {
                    // 已有材料，跳过
                    Ok(PhaseOutput::Skipped)
                } else {
                    self.gather_materials(&item.topic).await
                }
            }
            StudyPhase::Plan => {
                let materials = item.materials.as_ref().unwrap_or(&vec![]);
                let path = self.create_learning_path(&item.topic, materials).await?;
                ctx.learning_path = Some(path.clone());
                Ok(PhaseOutput::PlanComplete { path })
            }
            StudyPhase::LearnModule { index } => {
                let path = ctx.learning_path.as_ref().unwrap();
                let module = &path.modules[index];
                let (notes, comprehension) = self.study_module(module).await?;
                ctx.accumulated_notes.merge(notes);
                ctx.module_index = index;
                Ok(PhaseOutput::ModuleComplete { index, comprehension })
            }
            StudyPhase::Practice => {
                let exercises = self.generate_exercises(&ctx.accumulated_notes).await?;
                let result = self.run_exercises(exercises).await?;
                for m in &result.mistakes {
                    ctx.accumulated_notes.open_questions.push(m.clone());
                }
                Ok(PhaseOutput::PracticeComplete { score: result.score })
            }
            StudyPhase::Consolidate => {
                // 1. 笔记写入 Memory
                let nodes = memory.write_study_notes(&ctx.accumulated_notes).await?;

                // 2. 连接知识图谱
                let connections = knowledge_graph.connect_nodes(
                    &nodes, &self.config.knowledge_graph,
                ).await?;

                // 3. 间隔复习调度（仅 Deep 模式）
                if item.depth == StudyDepth::Deep {
                    self.schedule_reviews(&nodes, &ctx.accumulated_notes, local_bus).await?;
                }

                Ok(PhaseOutput::ConsolidateComplete { nodes, connections })
            }
        };

        self.run_hooks(HookPoint::AfterPhase, item).await?;

        match result {
            Ok(output) => {
                self.advance_pipeline(phase, output, ctx, local_bus, memory, knowledge_graph).await
            }
            Err(error) => {
                local_bus.post(StudyEvent::StudyItemFailed {
                    item_id: item.id.clone(),
                    error,
                    retryable: true,
                }).await?;
                Ok(())
            }
        }
    }

    /// 根据当前深度和阶段决定下一个步骤。
    async fn advance_pipeline(
        &mut self,
        current: StudyPhase,
        _output: PhaseOutput,
        ctx: &mut StudyContext,
        local_bus: &dyn EventBus,
        memory: &mut MemoryStore,
        knowledge_graph: &mut KnowledgeGraphStore,
    ) -> StudyResult<()> {
        let item = ctx.current.as_ref().unwrap();
        let next = match (item.depth, current) {
            // Skim: GatherMaterials → done
            (StudyDepth::Skim, StudyPhase::GatherMaterials) => None,

            // Read: (GatherMaterials) → Plan → LearnModule(0..N) → Consolidate → done
            (StudyDepth::Read, StudyPhase::GatherMaterials) => Some(StudyPhase::Plan),
            (StudyDepth::Read, StudyPhase::Plan) => {
                let path = ctx.learning_path.as_ref().unwrap();
                if path.modules.is_empty() { None }
                else { Some(StudyPhase::LearnModule { index: 0 }) }
            }
            (StudyDepth::Read, StudyPhase::LearnModule { index }) => {
                let path = ctx.learning_path.as_ref().unwrap();
                if index + 1 < path.modules.len() {
                    Some(StudyPhase::LearnModule { index: index + 1 })
                } else {
                    Some(StudyPhase::Consolidate)
                }
            }
            (StudyDepth::Read, StudyPhase::Consolidate) => None,

            // Deep: same as Read but with Practice before Consolidate
            (StudyDepth::Deep, StudyPhase::GatherMaterials) => Some(StudyPhase::Plan),
            (StudyDepth::Deep, StudyPhase::Plan) => {
                let path = ctx.learning_path.as_ref().unwrap();
                if path.modules.is_empty() { None }
                else { Some(StudyPhase::LearnModule { index: 0 }) }
            }
            (StudyDepth::Deep, StudyPhase::LearnModule { index }) => {
                let path = ctx.learning_path.as_ref().unwrap();
                if index + 1 < path.modules.len() {
                    Some(StudyPhase::LearnModule { index: index + 1 })
                } else {
                    Some(StudyPhase::Practice)
                }
            }
            (StudyDepth::Deep, StudyPhase::Practice) => Some(StudyPhase::Consolidate),
            (StudyDepth::Deep, StudyPhase::Consolidate) => None,

            _ => None,
        };

        match next {
            Some(phase) => {
                local_bus.post(StudyStepEvent::Execute { phase }).await?;
            }
            None => {
                // 所有阶段完成
                let item = ctx.current.as_ref().unwrap();
                let duration = item.created_at.elapsed();
                self.run_hooks(HookPoint::AfterExecution, item).await?;
                self.run_hooks(HookPoint::OnSuccess, item).await?;

                local_bus.post(StudyEvent::StudyItemCompleted {
                    item_id: item.id.clone(),
                    outcome: StudyOutcome::Completed {
                        comprehension: self.estimate_comprehension(ctx),
                    },
                    duration,
                }).await?;
            }
        }
        Ok(())
    }
}
```

### 4.3 Spaced Repetition Scheduling

Review Scheduler 是 Study System 内部组件，负责在 Consolidation 完成后注册定时回调：

```rust
impl StudySystem {
    /// Consolidation 完成后，为 Deep 模式的学习项安排间隔复习。
    async fn schedule_reviews(
        &self,
        nodes: &[KnowledgeNodeId],
        notes: &StudyNotes,
        local_bus: &dyn EventBus,
    ) -> StudyResult<()> {
        for node_id in nodes {
            let review = ScheduledReview {
                node_id: node_id.clone(),
                next_review: Timestamp::now() + Duration::days(1),  // 第 1 次复习 = 1 天后
                round: 1,
                ease_factor: self.config.spaced_repetition.ease_factor,
            };

            // 投递延迟事件：到期后推送 StudyItemAssigned
            local_bus.post_delayed(
                StudyEvent::StudyItemAssigned {
                    item: StudyItem {
                        id: StudyItemId::new(),
                        topic: format!("Review: {}", notes.key_concepts.first().map(|c| &c.0).unwrap_or(&"unknown".into())),
                        materials: Some(self.review_materials_for_node(node_id).await?),
                        depth: StudyDepth::Read,  // 复习用 Read 深度（不需要再练习）
                        priority: Priority::Normal,
                        timeout: Some(Duration::from_secs(600)),
                        context: {
                            let mut ctx = HashMap::new();
                            ctx.insert("source".into(), serde_json::to_value(
                                StudyItemSource::ScheduledReview {
                                    node_id: node_id.clone(),
                                    review_round: review.round,
                                }
                            ).unwrap());
                            ctx
                        },
                        created_at: Timestamp::now(),
                    },
                    source: StudyItemSource::ScheduledReview {
                        node_id: node_id.clone(),
                        review_round: review.round,
                    },
                },
                review.next_review - Timestamp::now(),
            ).await?;
        }
        Ok(())
    }
}
```

SM-2 参数更新在 `StudyItemCompleted` 处理中进行（见 4.1 handler）。

### 4.4 Bus Non-Empty Guarantee

```
StudyItemAssigned → IDLE→BUSY → PhaseExecute(GatherMaterials)
  → PhaseComplete → PhaseExecute(Plan)
  → PhaseComplete → PhaseExecute(LearnModule{0})
  → PhaseComplete → PhaseExecute(LearnModule{1})
  → PhaseComplete → PhaseExecute(Practice)       [Deep only]
  → PhaseComplete → PhaseExecute(Consolidate)
  → PhaseComplete → StudyItemCompleted
  → dequeue → 有下一个? PhaseExecute(...) for next item
            → 无下一个? IDLE

执行期间 Bus 始终非空 → Idle System 不触发。
队列空 → Bus 空 → Idle System 自然接管。
```

---

## 5. How External Systems Push Study Items

### 5.1 Unified Push Interface

```rust
#[async_trait]
pub trait StudyItemPushChannel {
    /// 向指定 Agent 推送学习项。
    async fn push(
        &self,
        agent_id: &AgentId,
        item: StudyItem,
        source: StudyItemSource,
    ) -> Result<()>;

    /// 推送学习项，由全局调度器决定目标 Agent。
    async fn push_any(
        &self,
        item: StudyItem,
        source: StudyItemSource,
        strategy: DispatchStrategy,
    ) -> Result<AgentId>;
}
```

### 5.2 CLI / API

```
用户: aman study assign --agent alice "Learn Rust async runtime internals"

CLI:
  1. 构造 StudyItem { topic: "Rust async runtime internals", depth: Deep, ... }
  2. POST /api/v1/agents/alice/study/push
  3. AgentRuntime → StudyItemAssigned 事件
  4. StudySystem.handle() 消费
```

### 5.3 RSS / arXiv Material Subscription

```
Material Plugin:
  - 监听 RSS/arXiv 更新
  - 新论文到达 → 评估相关性、重要性
  - 通过评分 → push(agent_id, StudyItem { topic, materials, depth }, MaterialSubscription)
  - Agent 队列收到 → 按深度执行学习流程
```

### 5.4 Idle Exploration → Study

```
Idle Exploration 发现有趣主题
  → Idle System post StudyItemAssigned {
      item: StudyItem { topic: curiosity_topic, depth: Read },
      source: StudyItemSource::IdleExploration,
    }
  → Study Queue 收到 → BUSY

不再需要单独的 ExplorationUpgraded 事件类型。
```

---

## 6. Hook Mechanism

### 6.1 Hook Points

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HookPoint {
    /// 学习项开始前。
    BeforeExecution,
    /// 每个学习阶段开始前（GatherMaterials / Plan / Learn / Practice / Consolidate）。
    BeforePhase,
    /// 每个学习阶段完成后。
    AfterPhase,
    /// 学习项完成后（无论成败）。
    AfterExecution,
    /// 学习项成功时。
    OnSuccess,
    /// 学习项失败时。
    OnFailure,
}
```

### 6.2 Configuration

```yaml
study:
  hooks:
    before_execution:
      - name: log_start
        action:
          type: tool
          tool_name: trace.record
          params:
            event: "study.item.started"

    before_phase:
      - name: check_context_switch
        action:
          type: llm
          system_prompt: "检查是否有更高优先级的事情需要打断当前学习。如有，返回 'interrupt'。"
        abort_on_failure: false

    after_phase:
      - name: update_progress
        action:
          type: emit_event
          event_type: "study.progress.updated"

    on_success:
      - name: update_knowledge_graph
        action:
          type: tool
          tool_name: knowledge_graph.commit

    on_failure:
      - name: log_failure_notes
        action:
          type: tool
          tool_name: trace.record
          params:
            event: "study.item.failed"
```

---

## 7. Knowledge Graph & Spaced Repetition

### 7.1 Knowledge Node

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeNode {
    pub id: KnowledgeNodeId,
    pub title: String,
    pub topic: String,
    pub summary: String,
    pub key_concepts: Vec<String>,
    pub connections: Vec<Edge>,
    pub comprehension: f64,
    pub last_reviewed: Timestamp,
    pub review_count: u32,
    pub is_stale: bool,
    pub source: StudyItemSource,
    pub created_at: Timestamp,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Edge {
    pub target: KnowledgeNodeId,
    pub relation: RelationType,
    pub strength: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum RelationType {
    Prerequisite,
    Extends,
    Contradicts,
    ExampleOf,
    Analogous,
    Related,
}
```

### 7.2 SM-2 Spaced Repetition

```rust
impl SpacedRepetitionConfig {
    pub fn next_review(&self, quality: u8, current_round: u32, ease_factor: f64) -> ScheduledReview {
        let mut ef = ease_factor;
        let (interval_days, next_round) = if quality >= 3 {
            ef = ef + (0.1 - (5 - quality) as f64 * (0.08 + (5 - quality) as f64 * 0.02));
            let idx = min(current_round as usize, self.intervals_days.len() - 1);
            let days = (self.intervals_days[idx] as f64 * ef) as u32;
            (days, current_round + 1)
        } else {
            ef = (ef - 0.2).max(1.3);
            (self.min_interval_on_fail, 0)
        };

        ScheduledReview {
            next_review: Timestamp::now() + Duration::days(interval_days as i64),
            round: next_round,
            ease_factor: ef,
        }
    }
}
```

### 7.3 Knowledge Graph Storage

```rust
pub struct KnowledgeGraphStore {
    search_index: TantivyIndex,
    relational_db: SqlitePool,
}

impl KnowledgeGraphStore {
    pub async fn search_similar(&self, query: &str, threshold: f64) -> Vec<KnowledgeNode>;
    pub async fn upsert_node(&self, node: KnowledgeNode) -> Result<KnowledgeNodeId>;
    pub async fn create_edges(&self, edges: Vec<Edge>) -> Result<()>;
    pub async fn get_subgraph(&self, root: KnowledgeNodeId, depth: u32) -> Result<Subgraph>;
}
```

---

## 8. Integration Points

### 8.1 Idle System → Study

```
Idle Exploration:
  发现有趣主题 → post StudyItemAssigned(IdleExploration)
  → Study 收到，队列非空 → BUSY → 开始学习

Idle Boredom:
  boredom_level >= seek_study_threshold → post SeekStudyRequest 到 Global Bus
  → 外部调度器（或 Material 插件）响应 → push StudyItem(SeekResponse)
  → Study 收到 → BUSY

Idle Meditation:
  知识节点连接发现 → 直接写入 KnowledgeGraph（不经过 Study System）
  学习产出物扩展 Exploration 的探索范围
```

### 8.2 Work System ↔ Study

互斥通过 AgentScheduler 管理：

```rust
impl AgentScheduler {
    pub async fn activate_system(&mut self, target: SystemKind, activation_event: Event) {
        if let Some(active) = self.active_system {
            if active != target {
                // 向当前活跃系统发送 Interrupt
                self.local_bus.post(match active {
                    SystemKind::Work => WorkEvent::Interrupt { ... },
                    SystemKind::Study => StudyEvent::Interrupt { ... },
                    _ => ...
                }).await?;
            }
        }
        self.local_bus.post(activation_event).await?;
    }
}
```

Work 事件在 Study BUSY 时被延迟，反之亦然。详见 work-design.md §8。

### 8.3 Feedback Loop

```
StudyItemCompleted → IdleSignal::Satisfaction   → arousal ↑, 扩展 Exploration 范围
StudyItemFailed    → IdleSignal::Frustration    → arousal ↓
```

---

## 9. Configuration

```yaml
study:
  execution:
    # 默认学习深度（当 StudyItem 未指定时）
    default_depth: deep
    # 单阶段最大执行时间
    phase_timeout: 600s
    # 学习项之间可选冷却
    inter_item_cooldown: 0s

  hooks:
    before_execution: []
    before_phase: []
    after_phase: []
    after_execution: []
    on_success: []
    on_failure: []

  queue:
    max_size: 50
    priority_queue: false

  # 材料获取
  materials:
    # 当 StudyItem.materials 为空时，是否自动搜索
    auto_gather: true
    # 搜索源优先级
    search_sources: [arxiv, web_search, local_knowledge_graph]
    max_candidates: 10
    min_relevance: 0.6

  # 学习策略
  learning:
    max_module_duration: 600s
    min_comprehension: 0.7      # 低于此值自动重学当前模块
    auto_practice: true          # Deep 模式下是否自动进入练习阶段

  # 间隔重复
  spaced_repetition:
    intervals_days: [1, 3, 7, 14, 30, 60, 120]
    max_review_rounds: 7
    ease_factor: 2.5
    min_interval_on_fail: 1

  # 知识图谱
  knowledge_graph:
    min_connections: 2
    auto_connect: true
    similarity_threshold: 0.6

  # RSS/arXiv 订阅（由 Material 插件管理，非 Study 直接管理）
  feeds:
    - url: "https://blog.rust-lang.org/feed.xml"
      category: "rust"
    - url: "https://arxiv.org/rss/cs.AI"
      category: "machine-learning"

  # Idle Exploration → Study 升级阈值
  idle_upgrade:
    enabled: true
    min_curiosity_score: 0.7
```

对比 v1：不再有 `auto_discover`、`study_cooldown`、`discovery strategy`、`source_priority`（在 `materials.search_sources` 中简化）——发现和巡检逻辑全部移除。

---

## 10. Runtime Integration

与 Work System 相同的生命周期（Phase 4 初始化，Phase 0 销毁）：

```rust
impl AgentBuilder {
    pub fn build(self) -> Result<AgentRuntime> {
        let study_sys = StudySystem::new(
            self.config.study.clone(),
            local_bus.clone(),
            self.persistence.memory_store(),
            self.persistence.knowledge_graph(),
            self.persistence.trace_store(),
        );
        // ...
    }
}
```

关闭时：
- 取消所有待定的 `post_delayed`（间隔复习定时回调）
- flush knowledge graph（提交未写入的连接）
- persist 当前学习进度（checkpoint）

---

## 11. Event Routing

```yaml
routes:
  - match: { event_type: "study.item.assigned" }   → handler:study
  - match: { event_type: "study.item.completed" }  → handler:study
  - match: { event_type: "study.item.failed" }     → handler:study
  - match: { event_type: "study.interrupt" }       → handler:study
```

说明：
- `StudyStepEvent`（Execute/PhaseComplete/PhaseFailed）是内部事件，不走路由表
- 间隔复习的定时回调直接投递 `study.item.assigned`，路由到 Study System 处理
- `SeekStudyRequest/Response` 走 Global Event Bus，不属于 Study System 路由

---

## 12. Migration Path from v1

| 删除 | 替换为 |
|------|-------|
| `StudyState::Discovering` | 内部 `StudyPhase::GatherMaterials` |
| `StudyState::Planning` | 内部 `StudyPhase::Plan` |
| `StudyState::Learning` | 内部 `StudyPhase::LearnModule` |
| `StudyState::Practicing` | 内部 `StudyPhase::Practice` |
| `StudyState::Consolidating` | 内部 `StudyPhase::Consolidate` |
| `ExplorationUpgraded` | `StudyItemAssigned(IdleExploration)` |
| `NewMaterialAvailable` | `StudyItemAssigned(MaterialSubscription)` |
| `DelayedStudyTick` | 删除 |
| `StartDiscover` / `DiscoverComplete` | 内部 `StudyStepEvent` |
| `StartPlan` / `PlanComplete` | 内部 `StudyStepEvent` |
| `LearnModule` / `ModuleComplete` | 内部 `StudyStepEvent` |
| `StartPractice` / `PracticeComplete` | 内部 `StudyStepEvent` |
| `StartConsolidate` / `ConsolidateComplete` | 内部 `StudyStepEvent` |
| `StudyAssigned` → 多态外部事件 | `StudyItemAssigned` + `StudyItemSource` |
| `DelayedReviewTick` | `post_delayed(StudyItemAssigned(ScheduledReview))` |
| `StudyCycleDone` | `StudyItemCompleted` |
| `StudyPersonality` (interests, auto_discover, discovery, study_cooldown) | 移到外部 Material 插件 / 简化配置 |

保留：
- `Interrupt` + checkpoint 机制
- 间隔重复算法（SM-2）
- Knowledge Graph 模型和持久化
- `StudyDepth`（Skim/Read/Deep）及对应的执行流程差异
- `StudyNotes`、`LearningPath`、`LearningModule`
- Phase 4 初始化 / Phase 0 销毁
- Per-Agent 架构 + Trace Store + KnowledgeGraph 集成

---

## 13. Summary

| 维度 | v1 (主动巡检) | v2 (被动推送) |
|------|-------------|-------------|
| **状态** | 6 | 2 (IDLE/BUSY) |
| **事件类型** | 15+ | 3 + Interrupt |
| **巡检** | DelayedStudyTick 定时器 | 无 |
| **材料发现** | DISCOVERING 阶段 | 外部（RSS/arXiv/Idle Exploration）推送 |
| **学习流程** | 6 个状态，每个有进入/完成事件 | 内部 Phase Pipeline，4 个 StudyStepEvent |
| **Idle → Study** | ExplorationUpgraded 单独事件 | StudyItemAssigned(IdleExploration) |
| **间隔复习** | DelayedReviewTick | post_delayed(StudyItemAssigned(ScheduledReview)) |
| **Hook** | 无 | 6 个 Hook 点 |
| **主动找学习** | Study 自身巡检 | Idle Boredom → SeekStudy |
| **配置项** | 15+ 项 | 精简到 5 组 |

**核心原则**：
1. Study System 就是一个带 Hook 的 FIFO 学习队列消费者。
2. 学什么、什么时候学由外部系统（用户、RSS、Idle Exploration、Review Scheduler）决定。
3. 学习深度（Skim/Read/Deep）决定内部执行流程，不暴露为状态机状态。
4. 间隔复习通过定时回调推送 StudyItem 到队列，与任何其他来源一样处理。
5. 队列空时 Idle System 自然运行，队列有项时 Study 自动接管。
