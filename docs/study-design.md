# Study System — Architecture Design (v3: Lifecycle Engine)

> **核心变更**：与 Work System 对齐——从「独立 2 状态机」重构为「LifecycleEngine 的领域适配层」。
> 共用逻辑（状态机、FIFO 队列、步骤链式执行、中断/重试、IdleSignal 反馈、全局总线通知）
> 全部由 `kernel/lifecycle::LifecycleEngine<S>` 提供。
> Study System 只需实现 `SystemSpec` trait，提供 Study 领域特有的类型和逻辑。
>
> 架构层次：
> ```
> LifecycleEngine<StudySpec>  ← 通用引擎（lifecycle crate）
>   └─ StudySpec             ← 领域适配（study crate，实现 SystemSpec trait）
>        ├─ Item  = StudyItem
>        ├─ Step  = StudyPhase
>        ├─ decompose()       → 根据 StudyDepth 生成学习阶段序列
>        ├─ execute_step_impl() → 执行单个学习阶段（LLM / 材料检索）
>        └─ collect_result()  → 收集学习成果 + 理解度评估
> ```

---

## 1. Why This Refactoring

v2 中 Work、Study、Daily-Life 各自独立实现了相同的队列/状态/步骤逻辑。v3 将通用部分提取到 `lifecycle` crate：

| 重复逻辑 | v2 做法 | v3 做法 |
|---------|--------|--------|
| 2 状态机 | 手写 `StudyState::Idle/Busy` | `LifecycleState` (lifecycle crate) |
| FIFO 队列 + 上下文 | 手写 `StudyContext` | `LifecycleContext<StudyItem, StudyPhase>` |
| 步骤链式执行 | 手写 `advance_pipeline` (~100行) | 引擎内部自动推进 |
| Interrupt → checkpoint | 手写 | `engine.handle_interrupt()` |
| IdleSignal | 手写 mpsc 发送 | 引擎自动发送 |
| 全局总线通知 | 手写 | 引擎自动发布 |

Study System 现在只需关注**领域特有逻辑**：
- 学习深度（Skim/Read/Deep）→ 阶段序列
- 材料获取策略（GatherMaterials）
- 知识图谱连接 + 间隔复习调度（Consolidate）
- SM-2 算法参数更新

---

## 2. Lifecycle Engine Architecture

### 2.1 StudySystem — Thin Wrapper

```rust
pub struct StudySystem {
    engine: LifecycleEngine<StudySpec>,
    config: StudyConfig,
    local_bus: Arc<dyn EventBus>,
    global_bus: Arc<dyn EventBus>,
    knowledge_graph: KnowledgeGraphStore,   // Study 特有
    memory: MemoryStore,                    // Study 特有
    idle_signal_tx: Mutex<Option<mpsc::UnboundedSender<IdleSignal>>>,
}

impl StudySystem {
    pub fn new(agent_id, config, local_bus, global_bus, memory, kg, system_state) -> Self {
        let spec = StudySpec::new(config.materials.auto_gather);
        let engine = LifecycleEngine::new(
            agent_id, spec,
            config.queue.max_size,
            0,  // step retries handled by StudyPhase internally
            local_bus, global_bus,
            system_state,
            AgentSystemState::Studying,  // BUSY 时设置的系统状态
        );
        // ...
    }

    pub async fn handle(&self, event: StudyEvent) -> StudyResult<()> {
        match event {
            StudyEvent::Interrupt { reason, by_system } => {
                self.engine.handle_interrupt(&reason, &by_system).await?;
            }
            StudyEvent::StudyItemAssigned { item, source } => {
                self.engine.handle_assigned(item, source_json).await?;
            }
            StudyEvent::StudyItemCompleted { item_id, outcome, duration } => {
                // 更新 SM-2 参数（如是 ScheduledReview）
                // 委托引擎
                self.engine.handle_completed(&item_id, result_json, duration).await?;
            }
            StudyEvent::StudyItemFailed { item_id, error, retryable } => {
                self.engine.handle_failed(&item_id, lc_error, retryable).await?;
            }
        }
    }
}
```

---

## 3. State Machine (provided by LifecycleEngine)

与 Work System 完全相同的 2 状态机（参见 work-design.md §3）。

引擎在状态切换时自动更新 `AgentSystemState`：
- `Idle` → `AgentSystemState::Idle`
- `Busy` → `AgentSystemState::Studying`

---

## 4. Domain Types (study-specific)

### 4.1 StudyEvent

```rust
pub enum StudyEvent {
    StudyItemAssigned { item: StudyItem, source: StudyItemSource },
    StudyItemCompleted { item_id: StudyItemId, outcome: StudyOutcome, duration: Duration },
    StudyItemFailed { item_id: StudyItemId, error: StudyError, retryable: bool },
    Interrupt { reason: String, by_system: String },
}
```

### 4.2 StudyItemSource

```rust
pub enum StudyItemSource {
    UserAssigned { operator: String },
    IdleExploration { curiosity_topic: String },
    MaterialSubscription { feed_url: String },
    ScheduledReview { node_id: KnowledgeNodeId, review_round: u32 },
    SeekResponse { request_id: String },
    Custom { name: String, metadata: HashMap<String, Value> },
}
```

### 4.3 StudyItem & StudyDepth

```rust
pub struct StudyItem {
    pub id: StudyItemId,
    pub topic: String,
    pub materials: Option<Vec<MaterialRef>>,
    pub depth: StudyDepth,
    pub priority: Priority,
    pub timeout: Option<Duration>,
    pub context: HashMap<String, Value>,
    pub created_at: Timestamp,
}

pub enum StudyDepth {
    Skim,   // 略读
    Read,   // 通读 + 笔记 + 知识图谱
    Deep,   // 深度学习 + 练习 + 间隔复习
}
```

---

## 5. StudySpec — Domain Adapter

`StudySpec` 是 Study 领域对 `SystemSpec` trait 的实现。

### 5.1 Step Type: StudyPhase

```rust
// StudySpec::Step = StudyPhase
pub enum StudyPhase {
    GatherMaterials,                          // 获取/搜索材料
    Plan,                                     // 制定学习路径
    LearnModule { index: usize },             // 学习单个模块
    Practice,                                 // 练习（仅 Deep）
    Consolidate,                              // 巩固（笔记→记忆、知识图谱、间隔复习）
}
```

### 5.2 Decomposition by StudyDepth

```rust
impl SystemSpec for StudySpec {
    type Item = StudyItem;
    type Step = StudyPhase;

    async fn decompose(&self, item: &StudyItem, _max_retries: u32) -> Vec<StudyPhase> {
        let mut phases = Vec::new();

        // 无预设材料 → 先收集
        if item.materials.is_none() {
            phases.push(StudyPhase::GatherMaterials);
        }

        phases.push(StudyPhase::Plan);

        // 根据深度决定阶段序列
        match item.depth {
            StudyDepth::Skim => {
                // Skim: GatherMaterials(如有) → Plan → 完成
            }
            StudyDepth::Read => {
                // Read: GatherMaterials(如有) → Plan → LearnModule(0..N) → Consolidate
                phases.push(StudyPhase::LearnModule { index: 0 });
                phases.push(StudyPhase::Consolidate);
            }
            StudyDepth::Deep => {
                // Deep: Read + Practice + Spaced Repetition
                phases.push(StudyPhase::LearnModule { index: 0 });
                phases.push(StudyPhase::Practice);
                phases.push(StudyPhase::Consolidate);
            }
        }

        phases
    }
}
```

> **注意**：多模块学习（LearnModule 0→1→2）的推进由引擎的步骤链处理。
> `decompose()` 只返回第一个 LearnModule。当 LearnModule{0} 完成时，
> `execute_step_impl` 检查是否还有模块需要学习，通过修改步骤列表或发布事件来推进。

### 5.3 Step Execution

```rust
async fn execute_step_impl(
    &self,
    item: &StudyItem,
    phase: &StudyPhase,
    _step_index: usize,
) -> Result<StepOutput, LifecycleError> {
    match phase {
        StudyPhase::GatherMaterials => {
            let materials = self.search_materials(&item.topic).await?;
            Ok(StepOutput {
                success: true,
                summary: format!("Found {} materials", materials.len()),
                artifacts: materials.into_iter().map(|m| m.url).collect(),
                duration: elapsed,
            })
        }
        StudyPhase::Plan => {
            let path = self.create_learning_path(&item.topic, materials).await?;
            Ok(StepOutput {
                success: true,
                summary: format!("Planned {} modules", path.modules.len()),
                artifacts: vec![serde_json::to_string(&path).unwrap()],
                duration: elapsed,
            })
        }
        StudyPhase::LearnModule { index } => {
            let module = &learning_path.modules[*index];
            let (notes, comprehension) = self.study_module(module).await?;
            Ok(StepOutput {
                success: comprehension >= self.config.min_comprehension,
                summary: format!("Module {}: comprehension {:.0}%", index, comprehension * 100.0),
                artifacts: vec![serde_json::to_string(&notes).unwrap()],
                duration: elapsed,
            })
        }
        StudyPhase::Practice => {
            let result = self.run_exercises(&notes).await?;
            Ok(StepOutput { .. })
        }
        StudyPhase::Consolidate => {
            // 1. 笔记写入 Memory
            let nodes = self.memory.write_study_notes(&notes).await?;
            // 2. 连接知识图谱
            let connections = self.knowledge_graph.connect_nodes(&nodes).await?;
            // 3. 间隔复习调度（仅 Deep 模式）
            //    通过 post_delayed(StudyItemAssigned(ScheduledReview)) 实现
            Ok(StepOutput { .. })
        }
    }
}
```

---

## 6. Spaced Repetition (Study-specific Logic)

间隔复习是 Study 领域特有的能力，不经过 lifecycle 引擎：

```rust
impl StudySystem {
    async fn schedule_reviews(&self, nodes: &[KnowledgeNodeId]) {
        for node_id in nodes {
            let review_item = StudyItem {
                topic: format!("Review: {}", node_title),
                depth: StudyDepth::Read,
                source: StudyItemSource::ScheduledReview { node_id, review_round: 1 },
                ..
            };
            // 投递延迟事件：到期后推送 StudyItemAssigned
            self.local_bus.post_delayed(
                StudyEvent::StudyItemAssigned { item: review_item, .. },
                next_review - now,
            ).await;
        }
    }
}
```

到期后的 `StudyItemAssigned` 通过路由表回到 `StudySystem.handle()` → `engine.handle_assigned()`，
与其他来源完全一样处理。SM-2 参数更新在 `StudyItemCompleted` 处理中进行。

---

## 7. Knowledge Graph (Study-specific)

```rust
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
}

pub struct Edge {
    pub target: KnowledgeNodeId,
    pub relation: RelationType,  // Prerequisite, Extends, Contradicts, etc.
    pub strength: f64,
}
```

知识图谱的读写完全在 `StudySpec::execute_step_impl` (Consolidate 阶段) 中进行，
引擎不感知。

---

## 8. Integration with Idle System

与 Work System 完全相同（参见 work-design.md §7）：
- 队列空 → Bus 空 → Idle 接管
- 新 StudyItem 入队 → Bus 非空 → Idle 停止
- Idle Exploration 发现主题 → `StudyItemAssigned(IdleExploration)` → 入队

---

## 9. Configuration

```yaml
study:
  execution:
    default_depth: deep
    phase_timeout: 600s
    inter_item_cooldown: 0s

  queue:
    max_size: 50
    priority_queue: false

  materials:
    auto_gather: true
    search_sources: [arxiv, web_search, local_knowledge_graph]

  learning:
    max_module_duration: 600s
    min_comprehension: 0.7

  spaced_repetition:
    intervals_days: [1, 3, 7, 14, 30, 60, 120]
    ease_factor: 2.5

  knowledge_graph:
    min_connections: 2
    auto_connect: true
```

---

## 10. Event Routing

```yaml
routes:
  - match: { event_type: "study.item.assigned" }   → handler:study
  - match: { event_type: "study.item.completed" }  → handler:study
  - match: { event_type: "study.item.failed" }     → handler:study
  - match: { event_type: "study.interrupt" }       → handler:study
```

内部 `study.step.execute` 事件由引擎发布和消费，不走路由表。

---

## 11. Summary

| 维度 | v2 (独立实现) | v3 (Lifecycle Engine) |
|------|-------------|----------------------|
| **状态机** | 手写 `StudyState` | `LifecycleState` (引擎提供) |
| **队列/上下文** | 手写 `StudyContext` | `LifecycleContext<StudyItem, StudyPhase>` |
| **步骤链** | 手写 `advance_pipeline` | 引擎自动推进 |
| **领域代码量** | ~600 行 | ~120 行 (spec + wrapper) |
| **知识图谱** | 嵌入事件处理中 | `execute_step_impl` 内集中处理 |
| **间隔复习** | 回调投递 StudyItem | 不变（Study 特有，不经过引擎） |

**核心原则**：
1. Study System 是 `LifecycleEngine<StudySpec>` 的薄封装。
2. 学习深度（Skim/Read/Deep）决定阶段序列 → `decompose()` 实现。
3. 每个阶段的执行细节（材料搜索、模块学习、练习、巩固）→ `execute_step_impl()` 实现。
4. 知识图谱和间隔复习是 Study 特有，不经过引擎，在 `execute_step_impl` 中处理。
