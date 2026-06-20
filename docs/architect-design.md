# aman Agent Framework — Rust 架构设计

> 基于 [agent-design.md](./agent-design.md) 的事件响应式 Agent 框架，Rust 系统级实现。
> 本文件为纯架构设计，不包含里程碑/roadmap/任务拆分。

---

## 1. 架构哲学

```
万物皆事件，响应即行为。
```

三条 Rust 化设计原则：

1. **零成本抽象** — Trait 静态派发为主，动态派发仅在插件边界使用。`enum` + `match` 优于 `Box<dyn Trait>` 调用链。
2. **类型驱动安全** — Event/Pipeline/Workflow 的状态转移在编译期校验。`#[deny(unsafe_code)]` 在核心 crate 中强制。
3. **组合优于继承** — 所有模块通过 Event Bus 解耦，无直接跨模块函数调用。模块内通过 Tower-like `Service` trait 组合。

---

## 2. 工作区结构 (Cargo Workspace)

```
aman/
├── Cargo.toml                    # [workspace] 根
├── crates/
│   ├── core/                # 核心类型 + 共享 Trait
│   │   ├── src/
│   │   │   ├── event.rs          # Event, EventMetadata, Priority, Delivery
│   │   │   ├── error.rs          # Error, Result<T>
│   │   │   ├── types.rs          # Timestamp, TraceId, SourceId, 共享 newtype
│   │   │   └── lib.rs
│   │   └── Cargo.toml
│   │
│   ├── event-bus/           # Event Bus (中枢)
│   │   ├── src/
│   │   │   ├── bus.rs            # EventBus trait + InMemoryBus + PersistentBus
│   │   │   ├── backpressure.rs   # 5级分层背压引擎
│   │   │   ├── dedup.rs          # 窗口去重 (BloomFilter + LRU)
│   │   │   ├── ordering.rs       # 同源保序队列
│   │   │   └── lib.rs
│   │   └── Cargo.toml
│   │
│   ├── dispatcher/          # 事件分发器
│   │   ├── src/
│   │   │   ├── dispatcher.rs     # Dispatcher: Router + Transformer + Filter
│   │   │   ├── route.rs          # RouteRule: match → target(s)
│   │   │   ├── transform.rs      # Transform 规则引擎
│   │   │   └── lib.rs
│   │   └── Cargo.toml
│   │
│   ├── pipeline/            # Pipeline 链式处理
│   │   ├── src/
│   │   │   ├── pipeline.rs       # Pipeline 定义 + 执行引擎
│   │   │   ├── step.rs           # PipelineStep: Filter/Transform/Action
│   │   │   ├── compensation.rs   # Saga 补偿执行器 (reverse_order)
│   │   │   ├── concurrency.rs    # serial | parallel | limited(N)
│   │   │   └── lib.rs
│   │   └── Cargo.toml
│   │
│   ├── skill/               # Skill 系统 (独立能力单元)
│   │   ├── src/
│   │   │   ├── skill.rs          # Skill trait + 执行上下文
│   │   │   ├── registry.rs       # SkillRegistry: 注册/查询/热加载
│   │   │   ├── loader.rs         # SkillLoader: YAML声明 → 实例化
│   │   │   ├── search.rs         # 全文检索 (Tantivy)
│   │   │   ├── version.rs        # 版本管理 (SemVer + 历史)
│   │   │   ├── hot_reload.rs     # 热加载 (notify + 原子替换)
│   │   │   └── lib.rs
│   │   └── Cargo.toml
│   │
│   ├── workflow/            # Workflow 状态机
│   │   ├── src/
│   │   │   ├── workflow.rs       # Workflow 定义 (states/transitions/guards)
│   │   │   ├── instance.rs       # WorkflowInstance 运行时
│   │   │   ├── guard.rs          # Guard 条件评估
│   │   │   ├── timeout.rs        # State Timeout 管理器
│   │   │   └── lib.rs
│   │   └── Cargo.toml
│   │
│   ├── tool/                # Tool Runner (执行单元)
│   │   ├── src/
│   │   │   ├── tool.rs           # Tool trait + 参数/返回 Schema
│   │   │   ├── runner.rs         # ToolRunner: 6步执行流程
│   │   │   ├── sandbox.rs        # 沙箱: 子进程/容器/WASM
│   │   │   ├── builtin/          # 内置工具
│   │   │   │   ├── file.rs
│   │   │   │   ├── http.rs
│   │   │   │   └── exec.rs
│   │   │   └── lib.rs
│   │   └── Cargo.toml
│   │
│   ├── source/              # Event Source (事件源)
│   │   ├── src/
│   │   │   ├── source.rs         # EventSource trait (Pull + Push)
│   │   │   ├── source_registry.rs
│   │   │   ├── timer.rs          # TimerSource (固定间隔)
│   │   │   ├── cron.rs           # CronSource (cron 表达式)
│   │   │   ├── file_watch.rs     # FileWatchSource (inotify/FSEvents)
│   │   │   ├── webhook.rs        # WebhookSource (HTTP 监听)
│   │   │   ├── signal.rs         # SignalSource (OS 信号)
│   │   │   ├── socket.rs         # SocketSource (TCP/UDS)
│   │   │   └── lib.rs
│   │   └── Cargo.toml
│   │
│   ├── plugin/              # 插件系统
│   │   ├── src/
│   │   │   ├── plugin.rs         # Plugin trait
│   │   │   ├── manifest.rs       # plugin.yaml 解析
│   │   │   ├── loader.rs         # 插件加载器 (拓扑排序 + 环检测)
│   │   │   ├── isolation.rs      # 隔离策略: 进程内/子进程/WASM
│   │   │   ├── lifecycle.rs      # 生命周期状态机 (Loaded/Enabled/Running/Paused/Disabled/Shutdown)
│   │   │   └── lib.rs
│   │   └── Cargo.toml
│   │
│   ├── hook/                # Hook 系统
│   │   ├── src/
│   │   │   ├── hook.rs           # Hook trait + HookPoint 枚举
│   │   │   ├── registry.rs       # HookRegistry: 注册/优先级/链式调用
│   │   │   └── lib.rs
│   │   └── Cargo.toml
│   │
│   ├── soul/                # SOUL 系统 (Agent 身份/行为)
│   │   ├── src/
│   │   │   ├── soul.rs           # Soul 定义: CoreTruths/DomainExpertise/Boundaries
│   │   │   ├── parser.rs         # SOUL.md 解析器
│   │   │   ├── runtime.rs        # SoulRuntime: 注入 Context/约束
│   │   │   └── lib.rs
│   │   └── Cargo.toml
│   │
│   ├── persistence/         # 持久化层
│   │   ├── src/
│   │   │   ├── wal.rs            # 预写日志 (Write-Ahead Log)
│   │   │   ├── checkpoint.rs     # Checkpoint 管理
│   │   │   ├── state_store.rs    # StateStore: KeyValue (sled/rocksdb)
│   │   │   ├── dlq.rs            # Dead Letter Queue
│   │   │   ├── overflow.rs       # 溢出磁盘管理
│   │   │   └── lib.rs
│   │   └── Cargo.toml
│   │
│   ├── secret/              # 密钥管理
│   │   ├── src/
│   │   │   ├── secret.rs         # SecretResolver: ${VAR} 模式解析
│   │   │   ├── vault.rs          # 外部 Secret Store 适配 (Vault/AWS/1Password)
│   │   │   ├── encryption.rs     # AES-256-GCM 内存加密
│   │   │   └── lib.rs
│   │   └── Cargo.toml
│   │
│   ├── config/              # 配置系统
│   │   ├── src/
│   │   │   ├── config.rs         # AgentConfig: 完整配置结构
│   │   │   ├── loader.rs         # 多层配置加载 (默认→文件→环境→运行时override)
│   │   │   ├── validate.rs       # 配置校验: 总线模式绑定、环检测、超时合理性
│   │   │   └── lib.rs
│   │   └── Cargo.toml
│   │
│   ├── runtime/             # 运行时 (启动/关闭编排)
│   │   ├── src/
│   │   │   ├── runtime.rs        # AgentRuntime: Phase 0→5 启动 + Phase 5→0 关闭
│   │   │   ├── health.rs         # /health/live + /health/ready
│   │   │   └── lib.rs
│   │   └── Cargo.toml
│   │
│   ├── cli/                 # CLI 二进制
│   │   ├── src/
│   │   │   ├── main.rs           # clap 命令入口
│   │   │   ├── cmd/              # 子命令
│   │   │   │   ├── run.rs        # aman run
│   │   │   │   ├── skill.rs      # aman skill {list|search|info|enable|disable}
│   │   │   │   ├── plugin.rs     # aman plugin {list|enable|disable|install}
│   │   │   │   ├── event.rs      # aman event {inject|trace|dump}
│   │   │   │   ├── workflow.rs   # aman workflow {list|show|retry|cancel}
│   │   │   │   └── config.rs     # aman config {show|validate|set}
│   │   │   └── lib.rs
│   │   └── Cargo.toml
│   │
│   ├── tauri/               # Tauri v2 桌面应用
│   │   ├── src-tauri/
│   │   │   ├── Cargo.toml
│   │   │   ├── tauri.conf.json
│   │   │   ├── capabilities/
│   │   │   └── src/
│   │   │       ├── main.rs       # Tauri 入口
│   │   │       ├── commands/     # Tauri Commands (IPC 桥)
│   │   │       │   ├── runtime.rs    # 启动/停止/状态
│   │   │       │   ├── skill.rs      # Skill CRUD IPC
│   │   │       │   ├── plugin.rs     # Plugin 管理 IPC
│   │   │       │   ├── event.rs      # 事件流/注入 IPC
│   │   │       │   ├── soul.rs       # SOUL 编辑/预览 IPC
│   │   │       │   └── metrics.rs    # 实时指标推送 (SSE)
│   │   │       └── state.rs      # Tauri 全局状态 (Mutex<AgentRuntime>)
│   │   ├── src/                  # 前端 (Vue 3 / React / Svelte)
│   │   │   ├── App.svelte
│   │   │   ├── lib/
│   │   │   │   ├── stores/      # 状态管理
│   │   │   │   └── components/  # Dashboard/SkillEditor/EventViewer/WorkflowBoard
│   │   │   └── index.html
│   │   └── package.json
│   │
│   ├── sdk/                 # SDK (供外部 Skill/Plugin 开发者)
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── prelude.rs
│   │       └── lib.rs
│   │
│   └── macros/              # 过程宏
│       ├── Cargo.toml
│       └── src/
│           ├── skill.rs          # #[skill] 属性宏
│           ├── plugin.rs         # #[plugin] 属性宏
│           └── lib.rs
```

---

## 3. 核心类型系统 (`kernel`)

### 3.1 依赖关系

```
core (零内部依赖，仅 serde/uuid/chrono)
    ↑
    ├── event-bus  ← persistence
    ├── source     ← config
    ├── dispatcher ← skill, pipeline, workflow, hook
    ├── tool       ← secret
    ├── plugin     ← core (循环打破: Plugin 不依赖 core，core 定义 Plugin trait)
    ├── soul       ← core
    └── runtime    ← 所有 crate 的组合根
```

### 3.2 核心 Trait 设计

```rust
// === Event (crateroot: core::event) ===

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    pub id: Uuid,                          // UUID v7 (时间有序)
    pub source: Arc<str>,                  // "timer:heartbeat", "watch:invoices"
    pub event_type: EventType,
    pub timestamp: DateTime<Utc>,          // 框架强制注入
    pub priority: Priority,
    pub delivery: DeliveryGuarantee,
    pub dedup_key: Option<DedupKey>,
    pub payload: serde_json::Value,
    pub metadata: EventMetadata,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventMetadata {
    pub trace_id: TraceId,                 // 框架强制注入 UUID v7
    pub parent_event_id: Option<Uuid>,
    pub retry_count: u32,
    pub max_retries: u32,
    pub ttl: Duration,
    pub created_at: DateTime<Utc>,
}

pub enum Priority { High = 0, Normal = 1, Low = 2 }
pub enum DeliveryGuarantee { AtMostOnce, AtLeastOnce, ExactlyOnce }

// === EventSource trait (core::source) ===

#[async_trait]
pub trait EventSource: Send + Sync {
    fn id(&self) -> &str;
    fn source_type(&self) -> SourceType;
    async fn init(&mut self, ctx: SourceContext) -> Result<()>;
    async fn shutdown(&mut self) -> Result<()>;

    // Pull 模式
    async fn poll(&self) -> Result<Vec<Event>> { Ok(vec![]) }

    // Push 模式: 事件源内部调用 ctx.bus.publish()
    // 背压信号: 暂停 Push
    async fn on_backpressure(&self, level: BackpressureLevel);

    fn health(&self) -> HealthStatus;
    async fn pause(&mut self);
    async fn resume(&mut self);
    async fn reconfigure(&mut self, config: serde_json::Value);
}

// === Pipeline trait (core::pipeline) ===

#[async_trait]
pub trait Pipeline: Send + Sync {
    fn id(&self) -> &str;
    fn concurrency(&self) -> ConcurrencyModel;
    fn steps(&self) -> &[PipelineStep];

    async fn execute(&self, event: Event, ctx: PipelineContext) -> PipelineResult;
}

#[derive(Debug, Clone)]
pub struct PipelineStep {
    pub id: String,
    pub step_type: StepType,       // Filter | Transform | Action
    pub tool: Arc<dyn Tool>,
    pub compensate: Option<Arc<dyn Tool>>,  // 补偿操作
    pub retry: RetryPolicy,
}

// === Skill trait (core::skill) ===

#[async_trait]
pub trait Skill: Send + Sync {
    fn name(&self) -> &str;
    fn version(&self) -> &semver::Version;
    fn description(&self) -> &str;
    fn triggers(&self) -> &[TriggerCondition];

    async fn execute(&self, event: Event, ctx: SkillContext) -> Result<()>;

    async fn on_load(&mut self) -> Result<()> { Ok(()) }
    async fn on_unload(&mut self) -> Result<()> { Ok(()) }
}

// === Tool trait (core::tool) ===

#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn mode(&self) -> ToolMode;             // Local | Remote | Container | Sandbox
    fn parameters(&self) -> &JsonSchema;
    fn returns(&self) -> &JsonSchema;

    async fn execute(&self, params: serde_json::Value, ctx: ToolContext) -> ToolResult;
}

// === Plugin trait (core::plugin) ===

#[async_trait]
pub trait Plugin: Send + Sync {
    fn name(&self) -> &str;
    fn version(&self) -> &semver::Version;
    fn dependencies(&self) -> &[PluginDependency];

    async fn on_load(&mut self, ctx: PluginContext) -> Result<()>;
    async fn on_unload(&mut self) -> Result<()>;
    async fn on_dependency_unloading(&self, dep_name: &str) -> Result<()>;

    fn event_sources(&self) -> Vec<Arc<dyn EventSource>>;
    fn skills(&self) -> Vec<Arc<dyn Skill>>;
    fn tools(&self) -> Vec<Arc<dyn Tool>>;
}

// === Hook trait (core::hook) ===

#[async_trait]
pub trait Hook: Send + Sync {
    fn name(&self) -> &str;
    fn priority(&self) -> i32;             // 越小越先执行
    fn hook_points(&self) -> &[HookPoint];

    async fn execute(&self, point: HookPoint, ctx: HookContext) -> Result<()>;
}

pub enum HookPoint {
    // 生命周期
    AgentStarting,
    AgentReady,
    AgentShuttingDown,
    AgentShutdown,

    // 事件
    EventPublished(Event),
    EventDispatched(TraceId),
    EventProcessed(TraceId),
    EventFailed(TraceId, Error),
    EventEnqueuedToDlq(TraceId),

    // Pipeline
    PipelineStarted(String),
    PipelineStepCompleted(String, String),
    PipelineCompleted(String),
    PipelineFailed(String, Error),
    PipelineCompensationStarted(String),
    PipelineCompensationCompleted(String),

    // Skill
    SkillLoaded(String),
    SkillUnloaded(String),
    SkillExecuted(String, Duration),

    // Workflow
    WorkflowStateEntered(String, String),
    WorkflowStateLeft(String, String),
    WorkflowTransitionFailed(String, Error),

    // Plugin
    PluginLoaded(String),
    PluginUnloaded(String),
    PluginEnabled(String),
    PluginDisabled(String),

    // Config
    ConfigChanged { path: String, old: serde_json::Value, new: serde_json::Value },
    SecretRotated { keys: Vec<String> },

    // Custom
    Custom(String),
}
```

### 3.3 执行上下文设计

```rust
// SkillContext: Skill 执行时注入
pub struct SkillContext {
    pub event: Event,
    pub state_store: Arc<dyn StateStore>,
    pub event_bus: Arc<dyn EventBus>,
    pub tool_registry: Arc<ToolRegistry>,
    pub logger: Arc<dyn Logger>,
    pub soul: Arc<Soul>,
    pub span: tracing::Span,               // OpenTelemetry span
}

// PipelineContext: Pipeline 执行时注入
pub struct PipelineContext {
    pub triggering_event: Event,
    pub state_store: Arc<dyn StateStore>,
    pub event_bus: Arc<dyn EventBus>,
    pub tool_runner: Arc<ToolRunner>,
    pub compensation_state: Arc<RwLock<CompensationState>>,
    pub instance_id: Uuid,                 // Pipeline 实例 ID
    pub temp_dir: PathBuf,                 // 本实例临时目录
}

// ToolContext: Tool 执行时注入
pub struct ToolContext {
    pub caller_id: String,                 // Skill/Pipeline/Workflow 名称
    pub trace_id: TraceId,
    pub timeout: Duration,
    pub allowed_paths: Option<Vec<PathBuf>>,
    pub network_allowed: bool,
    pub max_memory: Option<ByteSize>,
    pub temp_dir: PathBuf,
    pub logger: Arc<dyn Logger>,
}

// HookContext: Hook 执行时注入
pub struct HookContext {
    pub hook_point: HookPoint,
    pub trace_id: Option<TraceId>,
    pub event_bus: Arc<dyn EventBus>,
    pub state_store: Arc<dyn StateStore>,
    pub logger: Arc<dyn Logger>,
}
```

---

## 4. Event Bus (`event-bus`)

### 4.1 架构

```
                 ┌──────────────────────────────────────────────┐
                 │              Event Bus 逻辑                    │
                 │                                              │
  publish() ──→ │  ┌─────────┐  ┌──────────┐  ┌─────────────┐  │
                │  │ Dedup   │→ │ Priority  │→ │ Per-Source   │  │
                │  │ Window  │  │  Queue    │  │ FIFO Segments│  │
                │  └─────────┘  └──────────┘  └──────┬───────┘  │
                 │                                    │          │
                 │                         ┌──────────▼───────┐  │
                 │                         │ Backpressure     │  │
                 │                         │ Controller       │  │
                 │                         └──────────────────┘  │
                 └──────────────────────────────────────────────┘
                                       │
                    ┌──────────────────┼──────────────────┐
                    ▼                  ▼                  ▼
              ┌──────────┐     ┌──────────┐      ┌──────────────┐
              │ WAL      │     │ Overflow │      │ Retry Queue  │
              │ (持久化)  │     │  (磁盘)   │      │  (待重试)     │
              └──────────┘     └──────────┘      └──────────────┘
```

### 4.2 关键类型

```rust
#[async_trait]
pub trait EventBus: Send + Sync {
    async fn publish(&self, event: Event) -> Result<()>;
    async fn subscribe(
        &self,
        filter: SubscriptionFilter,
        handler: Box<dyn EventHandler>,
    ) -> Result<SubscriptionId>;
    async fn unsubscribe(&self, id: SubscriptionId);

    fn metrics(&self) -> BusMetrics;
    fn backpressure_level(&self) -> BackpressureLevel;
}

pub struct SubscriptionFilter {
    pub event_types: Option<Vec<EventType>>,
    pub sources: Option<Vec<String>>,
    pub priorities: Option<Vec<Priority>>,
    pub payload_match: Option<serde_json::Value>,  // JSON Path 匹配
}

// 背压级别枚举
pub enum BackpressureLevel {
    Normal,         // 0: < 80%
    Level1,         // 1: 80% → AT_MOST_ONCE 降级
    Level2,         // 2: 90% → 丢弃 AT_MOST_ONCE
    Level3,         // 3: 95% → 阻塞 poll + Push 返回 503
    Level4A,        // 4A: 98% → 溢出 AT_LEAST_ONCE 到磁盘
    Level4B,        // 4B: 溢出目录 ≥80% → 紧急告警 + 回退 Level3
    Critical,       // 5: 100% → 停止低优先源
}

// 两种总线实现
pub struct InMemoryBus { .. }       // 默认，单进程
pub struct PersistentBus {          // 生产环境
    inner: InMemoryBus,
    wal: Arc<WriteAheadLog>,
    retry_queue: Arc<RetryQueue>,
    overflow: Arc<OverflowDir>,
}
```

### 4.3 同源保序实现

```rust
// Per-source FIFO segments
pub struct OrderedQueue {
    segments: HashMap<SourceId, VecDeque<Event>>,
    global_queue: BinaryHeap<PrioritizedEvent>,  // 跨源优先级调度
}

impl OrderedQueue {
    fn push(&mut self, event: Event) {
        let source_id = event.source.clone();
        self.segments.entry(source_id).or_default().push_back(event);
    }

    fn pop(&mut self) -> Option<Event> {
        // 1. 从各 segment 头部收集候选事件
        // 2. 按优先级排序 (Priority × 跨源)
        // 3. 同源 FIFO 不变
        // 4. 弹出最高优
        let candidate = self.collect_heads();
        let next = candidate.sort_by_priority().next();
        if let Some(event) = next {
            self.segments.get_mut(&event.source).unwrap().pop_front();
        }
        next
    }
}
```

### 4.4 去重窗口

```rust
pub struct DedupWindow {
    bloom: BloomFilter,              // 快速拒绝 (O(1) 内存)
    recent: LruCache<DedupKey, Uuid>, // 精确去重 (30s 窗口)
    window: Duration,                // 默认 30000ms
}

impl DedupWindow {
    pub fn check(&mut self, event: &Event) -> DedupResult {
        match event.delivery {
            DeliveryGuarantee::AtMostOnce => DedupResult::Pass,  // 不回算 hash
            _ => {
                let key = event.dedup_key.as_ref()
                    .cloned()
                    .unwrap_or_else(|| DedupKey::from_event(event));
                if self.bloom.may_contain(&key) {
                    if self.recent.contains(&key) {
                        return DedupResult::Duplicate;
                    }
                }
                self.bloom.insert(&key);
                self.recent.put(key, event.id);
                DedupResult::Pass
            }
        }
    }
}
```

---

## 5. Dispatcher (`dispatcher`)

### 5.1 架构

```
Event Bus 输出 → Dispatcher
                    │
        ┌───────────┼───────────┐
        ▼           ▼           ▼
    ┌───────┐  ┌────────┐  ┌─────────┐
    │Routes │  │Transforms│  │Filters  │
    │Table  │  │ Engine  │  │(rate lmt)│
    └───┬───┘  └───┬────┘  └────┬────┘
        │          │            │
        └──────────┼────────────┘
                   ▼
          ┌───────────────┐
          │ Target Router │
          └───────┬───────┘
          ┌───────┼──────────┐
          ▼       ▼          ▼
     Pipeline  Skill    Workflow
```

### 5.2 路由规则

```rust
pub struct RouteRule {
    pub match_condition: MatchCondition,
    pub targets: Vec<DispatchTarget>,
    pub priority: i32,
}

pub enum MatchCondition {
    Type(EventType),
    Source(String),
    TypeAndSource { event_type: EventType, source: String },
    Priority(Priority),
    PayloadMatch(serde_json::Value),       // JSON Path
    All(Vec<MatchCondition>),              // AND 组合
    Any(Vec<MatchCondition>),              // OR 组合
    Custom(Box<dyn MatchFn>),              // 自定义匹配函数
}

pub enum DispatchTarget {
    Pipeline(String),       // "invoice-processor"
    Skill(String),          // "slack-notification"
    Workflow(String),       // "approval-flow"
    Hook(String),           // "audit-logger"
    FanOut(Vec<DispatchTarget>),  // 同时分发到多个目标
}

pub struct TransformRule {
    pub match_condition: MatchCondition,
    pub transform: Box<dyn TransformFn>,
    // 输入: Event → 输出: Vec<Event>
}
```

---

## 6. Pipeline 引擎 (`pipeline`)

### 6.1 执行模型

```
Pipeline::execute(event)
    │
    ├── 1. 获取并发槽位 (ConcurrencyController)
    │
    ├── 2. 创建 PipelineInstance { id, compensation_stack, temp_dir }
    │
    ├── 3. for step in steps:
    │   ├── match step.step_type:
    │   │   ├── Filter:  step.tool.execute() → bool → 不通过则中断
    │   │   ├── Transform: step.tool.execute() → Value → 传递到下一步
    │   │   └── Action:  step.tool.execute() → Result
    │   │
    │   ├── 成功 → 记录 compensation 到栈 (如果有) → 继续
    │   │
    │   └── 失败 (重试耗尽) → 触发 Compensation Engine
    │       └── reverse_order 执行 compensate stack
    │           ├── 全部成功 → 事件进 DLQ + 告警
    │           └── 部分失败 → COMPENSATION_FAILED + 告警
    │
    └── 4. 全部成功 → 产出 Output Event → publish
```

### 6.2 补偿引擎

```rust
pub struct CompensationEngine {
    strategy: CompensationStrategy,
    contract: CompensationContract,
}

impl CompensationEngine {
    pub async fn execute(
        &self,
        steps: &[(usize, &PipelineStep)],  // 已完成的步骤
        instance_id: Uuid,
    ) -> CompensationResult {
        // 1. 按 reverse_order 排序
        let reversed: Vec<_> = steps.iter().rev().collect();

        let mut compensated = vec![];
        let mut failed = vec![];

        // 2. 依次执行补偿 (补偿本身可重试)
        for (idx, step) in reversed {
            match self.execute_single_compensation(step, instance_id).await {
                Ok(()) => compensated.push(idx),
                Err(e) => {
                    failed.push((idx, e));
                    // 不中断——尝试补偿其他步骤
                }
            }
        }

        if failed.is_empty() {
            CompensationResult::FullyCompensated
        } else {
            CompensationResult::PartiallyCompensated { compensated, failed }
        }
    }
}
```

### 6.3 并发控制

```rust
pub enum ConcurrencyModel {
    Serial,                 // 单实例 → 简单安全
    Parallel,               // 无限并发 → 强制 optimistic_lock + 独立 temp_dir
    Limited(usize),         // N 并发 → AsyncSemaphore
}

pub struct ConcurrencyController {
    model: ConcurrencyModel,
    semaphore: Option<Arc<tokio::sync::Semaphore>>,
    queue: VecDeque<PendingExecution>,
}

impl ConcurrencyController {
    pub async fn acquire(&self) -> Option<Permit> { .. }
}
```

---

## 7. Skill 系统 (`skill`)

### 7.1 检索查询

基于 Tantivy (Rust 原生全文检索引擎)，支持：

- 关键词搜索: `skill.search("backup file s3")`
- 字段搜索: `skill.search("trigger:cron type:CRON_TICK")`
- 语义标签: `skill.search("tag:notification tag:critical")`
- 模糊匹配: `skill.search("file_backup~")` (编辑距离 ≤2)

```rust
pub struct SkillSearch {
    index: tantivy::Index,
    reader: tantivy::IndexReader,
    schema: SkillSearchSchema,
}

impl SkillSearch {
    pub fn new(index_path: &Path) -> Result<Self>;
    pub fn index_skill(&self, skill: &dyn Skill, content: &str) -> Result<()>;
    pub fn search(&self, query: &str, limit: usize) -> Result<Vec<SkillMatch>>;
    pub fn remove_skill(&self, skill_name: &str) -> Result<()>;
}

pub struct SkillMatch {
    pub name: String,
    pub version: semver::Version,
    pub score: f32,
    pub snippet: String,               // 高亮摘要
    pub matched_field: String,         // 哪个字段匹配
}
```

### 7.2 热加载

```rust
pub struct HotReloadManager {
    watcher: notify::RecommendedWatcher,
    registry: Arc<SkillRegistry>,
    loader: Arc<SkillLoader>,
}

impl HotReloadManager {
    pub fn watch(&mut self, skills_dir: &Path) -> Result<()> {
        // 1. 用 notify crate 监控 skills/ 目录
        // 2. 文件变更 → debounce 500ms → 检查文件完整性 (lock检测)
        // 3. 解析 SKILL.md → 构建新 Skill 实例
        // 4. 版本比较:
        //    - 相同版本: 原地替换 (原子 swap Arc<dyn Skill>)
        //    - 新版本: 注册为新 Skill，旧版本保留到所有 inflight 执行完成
        // 5. 更新 Search Index
        // 6. 发布 Event: SKILL_RELOADED
    }
}
```

热加载生命周期:

```
文件变更 → debounce(500ms) → 完整性检查 → 解析
    → 版本比较:
        ├── 同版本: Arc::swap (原子替换)
        └── 新版本: 注册 + 旧版 drain (等待 inflight 清空后 drop)
    → 更新 Search Index
    → 通知 Dispatcher 刷新路由
```

### 7.3 版本控制

```rust
pub struct SkillVersionManager {
    versions: HashMap<String, Vec<SkillVersion>>,  // name → [v1.0.0, v1.1.0, v2.0.0]
    history_dir: PathBuf,                           // ~/.aman/skills/history/
}

pub struct SkillVersion {
    pub version: semver::Version,
    pub skill: Arc<dyn Skill>,
    pub loaded_at: DateTime<Utc>,
    pub source_hash: String,            // SHA256 of SKILL.md
    pub changelog: Option<String>,
}

impl SkillVersionManager {
    // 回滚到指定版本
    pub fn rollback(&mut self, name: &str, version: &semver::Version) -> Result<()>;

    // 查看历史
    pub fn history(&self, name: &str) -> Vec<&SkillVersion>;

    // 比较版本差异
    pub fn diff(&self, name: &str, v1: &semver::Version, v2: &semver::Version)
        -> Result<String>;
}
```

Skill 目录结构:

```
~/.aman/skills/
├── my-backup.skill.md           # 声明式 Skill 定义
├── my-backup/                   # 代码式 Skill (Rust WASM)
│   ├── Cargo.toml
│   └── src/lib.rs
├── ocr-processor.skill.md
└── history/                     # 版本历史
    ├── my-backup/
    │   ├── v1.0.0.skill.md
    │   ├── v1.1.0.skill.md
    │   └── v2.0.0.skill.md
```

---

## 8. Workflow 状态机 (`workflow`)

### 8.1 核心类型

```rust
pub struct WorkflowDef {
    pub name: String,
    pub states: HashMap<String, StateDef>,
    pub initial_state: String,
    pub final_states: HashSet<String>,
    pub error_state: String,
    pub transitions: Vec<Transition>,
    pub state_timeouts: HashMap<String, StateTimeout>,
    pub error_recovery: Option<ErrorRecoveryConfig>,
}

pub struct Transition {
    pub from: TransitionFrom,        // StateFrom::Specific("PENDING") | StateFrom::Any
    pub event: String,               // "SUBMIT", "APPROVE", "RETRY"
    pub to: TransitionTo,            // StateTo::Specific("REVIEWING") | LastActiveState
    pub guard: Option<String>,       // guard 函数名
    pub on_fail: TransitionOnFail,   // StateOnFail::Stay | StateOnFail::Goto("ERROR")
    pub action: Option<TransitionAction>, // Pipeline/Skill 名称
    pub on_action_failure: Option<ActionFailurePolicy>,
}

pub struct WorkflowInstance {
    pub id: Uuid,
    pub workflow_name: String,
    pub current_state: String,
    pub last_active_state: String,   // ERROR 恢复目标
    pub total_retry_count: u32,      // 累计全局重试 (永不被重置)
    pub session_retry_count: u32,    // 当前 ERROR 会话内重试 (进入 ERROR 时重置)
    pub state_entered_at: DateTime<Utc>,
    pub timeout_clock: TimeoutClock,  // 跨状态暂停计时器
    pub data: serde_json::Value,
    pub partial_rollback: bool,      // 补偿失败标记
}
```

### 8.2 状态转移引擎

```rust
pub struct WorkflowEngine {
    definitions: HashMap<String, Arc<WorkflowDef>>,
    instances: DashMap<Uuid, WorkflowInstance>,
    state_store: Arc<dyn StateStore>,
    tool_runner: Arc<ToolRunner>,
}

impl WorkflowEngine {
    pub async fn handle_event(&self, event: Event) -> Result<()> {
        // 1. 从 event.payload 中提取 workflow_instance_id
        // 2. 加载 WorkflowInstance (可能从 StateStore)
        // 3. 匹配 Transition:
        //    a. 按 (from, event) 查转移表 — 状态名 normalize 大写比较
        //    b. 检查 guard (如 total_retry_count < max_retry_count)
        //    c. guard 失败 → on_fail 策略
        // 4. 执行 action (Pipeline/Skill)
        //    a. 失败 → on_action_failure 策略 (默认 ERROR)
        // 5. 状态转移:
        //    a. on_leave(current_state)
        //    b. 更新 current_state
        //    c. on_enter(new_state)
        //    d. 终态 → on_final
        // 6. 持久化到 StateStore
        // 7. 发布 Workflow 状态变更事件
    }
}
```

### 8.3 超时管理

```rust
pub struct TimeoutManager {
    timers: HashMap<Uuid, tokio::task::JoinHandle<()>>,
    strategy: TimeoutBehavior,  // pause | reset | continue (默认 pause)
}

impl TimeoutManager {
    // 状态退出时: pause → 暂停计时器，记录剩余时间
    // 状态重入时: resume → 恢复计时器从剩余时间继续
    pub fn on_state_exit(&mut self, instance_id: Uuid);
    pub fn on_state_enter(&mut self, instance_id: Uuid, timeout: Duration);
}
```

---

## 9. Hook 系统 (`hook`)

### 9.1 注册与执行

```rust
pub struct HookRegistry {
    hooks: HashMap<HookPoint, BTreeMap<i32, Vec<Arc<dyn Hook>>>>,
    // HookPoint → 按 priority 排序的 Hook 列表
}

impl HookRegistry {
    // 注册
    pub fn register(&mut self, hook: Arc<dyn Hook>) {
        for point in hook.hook_points() {
            self.hooks
                .entry(point.clone())
                .or_default()
                .entry(hook.priority())
                .or_default()
                .push(hook.clone());
        }
    }

    // 触发 (链式异步执行，按 priority 顺序)
    pub async fn trigger(&self, point: HookPoint, ctx: HookContext) -> HookResult {
        let mut results = vec![];
        if let Some(priority_map) = self.hooks.get(&point) {
            for hooks in priority_map.values() {
                for hook in hooks {
                    match hook.execute(point.clone(), ctx.clone()).await {
                        Ok(()) => results.push(HookResult::Ok(hook.name().to_string())),
                        Err(e) => results.push(HookResult::Failed(hook.name().to_string(), e)),
                    }
                }
            }
        }
        HookResult::aggregate(results)
    }
}
```

### 9.2 Hook 配置 (YAML)

```yaml
hooks:
  - name: "audit-logger"
    priority: 0                     # 最先执行
    hook_points:
      - EventPublished
      - EventProcessed
      - PipelineFailed
      - WorkflowStateEntered
    config:
      output: "/var/log/aman/audit.log"

  - name: "metrics-collector"
    priority: 100
    hook_points:
      - EventProcessed
      - SkillExecuted
      - PipelineCompleted
    config:
      push_gateway: "http://prometheus:9091"

  - name: "slack-notifier"
    priority: 200                   # 最后执行
    hook_points:
      - PipelineFailed
      - AgentShutdown
      - EventEnqueuedToDlq
    config:
      webhook_url: "${SLACK_WEBHOOK_URL}"   # Secret Store 解析
```

---

## 10. SOUL 系统 (`soul`)

### 10.1 SOUL 概念

SOUL 是 Agent 的"人格/行为约束系统"。它不是代码，而是**声明式约束**，在 Agent 运行时注入到所有 Skill/Pipeline/Workflow 的上下文中。

### 10.2 SOUL.md 格式

```markdown
# Architect Compass — SOUL.md

身份：你是跨越物理与数字世界的系统架构师。

核心：
- 系统思维是你的基石。
- 优雅的约束设计是你的呼吸。

Core Truths:
- Be genuinely useful, not just correct.
- Have opinions, rooted in numbers.
- Think in layers before you build.

Domain Expertise:
- Hardware & Embedded: RISC-V, ARM, FPGA, RTOS
- AI/ML Systems: distributed training, inference optimization
- Smart Contracts: EVM, Solidity, DeFi primitives

Boundaries:
- 绝不推荐未经审计的合约直接上主网。
- 不在缺少 latency/p99 数据时拍板架构选型。

Vibe: 话不多但开口就让所有人安静的老手。
```

### 10.3 解析为结构化类型

```rust
pub struct Soul {
    pub name: String,                              // "Architect Compass"
    pub identity: String,                          // "跨越物理与数字世界的系统架构师"
    pub core: Vec<String>,                         // Core Truths
    pub expertise: HashMap<String, Vec<String>>,   // Domain → [事实]
    pub boundaries: Vec<String>,                   // 绝对不可跨越的边界
    pub vibe: String,                              // 行为风格描述
    pub preferences: HashMap<String, String>,      // 偏好设置
    pub raw: String,                               // 原始文本 (注入 LLM)
}

impl Soul {
    // 从 SOUL.md 文件解析
    pub fn from_file(path: &Path) -> Result<Self>;

    // 构建 LLM System Prompt
    pub fn to_system_prompt(&self) -> String;

    // 检查某个行为是否违反 Boundaries
    pub fn check_boundary(&self, action: &str) -> BoundaryResult;

    // 注入到 SkillContext
    pub fn inject_into_context(&self, ctx: &mut SkillContext);
}
```

### 10.4 Soul 在运行时

- **CLI 模式**: `aman run --soul ~/.aman/souls/architect.md`
- **Tauri 模式**: 设置界面中选择/编辑 SOUL 文件，实时预览
- **运行时注入**: 每个 SkillContext / PipelineContext 持有 `Arc<Soul>`
- **热更新**: SOUL 文件变更 → 触发 `SoulChanged` 事件 → 所有模块刷新引用

---

## 11. 持久化层 (`persistence`)

### 11.1 WAL (Write-Ahead Log)

```rust
pub struct WriteAheadLog {
    writer: WalWriter,
    reader: WalReader,
    current_segment: WalSegment,
    config: WalConfig,
}

pub struct WalConfig {
    pub wal_path: PathBuf,
    pub wal_sync: WalSyncMode,          // Fsync | Batch
    pub rotate_bytes: u64,              // 1GB
    pub checkpoint_interval: u64,       // 500 事件
}

impl WriteAheadLog {
    // 写入事件到 WAL → fsync → 返回偏移量
    pub async fn append(&self, event: &Event) -> Result<WalOffset>;

    // 记录 checkpoint: "已处理到偏移量 X"
    pub async fn checkpoint(&self, offset: WalOffset) -> Result<()>;

    // 崩溃恢复: 从 checkpoint 偏移量重放
    pub async fn replay_from_checkpoint(
        &self,
    ) -> Result<(WalOffset, Vec<Event>)>;

    // 检查待重试事件
    pub fn pending_retry_events(&self) -> Vec<Event>;
}
```

### 11.2 State Store

```rust
#[async_trait]
pub trait StateStore: Send + Sync {
    async fn get(&self, key: &str) -> Result<Option<Vec<u8>>>;
    async fn put(&self, key: &str, value: Vec<u8>) -> Result<()>;
    async fn put_cas(&self, key: &str, value: Vec<u8>, expected_version: u64)
        -> Result<bool>;  // CAS 乐观锁
    async fn delete(&self, key: &str) -> Result<()>;
    async fn scan(&self, prefix: &str) -> Result<Vec<(String, Vec<u8>)>>;

    fn isolation_mode(&self) -> IsolationMode;  // Namespace | Physical
    fn write_consistency(&self) -> WriteConsistency;
}

// 实现: SledStore (embedded) / RocksDbStore / RedisStore
pub struct SledStore {
    db: sled::Db,       // 默认嵌入式实现
}
```

### 11.3 Dead Letter Queue

```rust
pub struct DeadLetterQueue {
    storage: PathBuf,           // ~/.aman/dlq/
    ttl_days: u32,              // 30
    pre_expiry_alert_days: Vec<u32>,  // [7, 3, 1]
    max_manual_retries: u32,    // 5
}

impl DeadLetterQueue {
    pub async fn enqueue(&self, event: Event, reason: DlqReason) -> Result<()>;
    pub async fn list(&self, filter: DlqFilter) -> Result<Vec<DlqEntry>>;
    pub async fn retry(&self, id: &str, operator: &str) -> Result<()>;
    pub async fn discard(&self, id: &str, operator: &str) -> Result<()>;
    pub async fn run_expiry(&self) -> Result<Vec<ExpiredEvent>>;
}
```

---

## 12. Event Source (`source`)

### 12.1 事件源注册

```rust
pub struct SourceRegistry {
    sources: HashMap<String, Arc<dyn EventSource>>,
    mode_flags: HashMap<String, SourceMode>,  // Pull | Push
}

// 事件源统一注册:
// - 配置声明: config.event_sources[].type = "file_watch" / "cron" / "webhook" / ...
// - 动态注册: POST /agent/source { type: "webhook", id: "...", config: {...} }
```

### 12.2 Cron 引擎

每个 `CronSource` 表示一个 cron 定时任务，由 `SourceRegistry` 统一管理。
配置通过 `CronStore` 持久化到 `~/.aman/agents/{agent_key}/cron/jobs.json`，重启后自动恢复。

```rust
pub struct CronSource {
    id: String,
    expression: String,               // 5 或 6 字段 cron 表达式
    schedule: cron::Schedule,         // 解析后的调度
    timezone: chrono_tz::Tz,          // 时区（默认 UTC）
    next_run_at: Option<DateTime<Tz>>,
    initialized: bool,
    paused: bool,
}

// 持久化类型（kernel/source/src/cron.rs）

pub struct CronJobConfig {
    pub id: String,
    pub name: Option<String>,
    pub expression: String,
    pub timezone: String,             // "UTC", "Asia/Shanghai", ...
    pub enabled: bool,
    pub created_at: String,           // ISO 8601
    pub updated_at: Option<String>,
    pub last_run_at: Option<String>,
    pub last_status: Option<String>,  // "ok" | "error"
    pub last_error: Option<String>,
}

pub struct CronJobsFile {
    pub jobs: Vec<CronJobConfig>,
    pub updated_at: String,
}

// 持久化管理器（kernel/source/src/cron_store.rs）

pub struct CronStore {
    dir: PathBuf,  // ~/.aman/agents/{agent_key}/cron/
}
```

### 12.3 FileWatchSource

```rust
pub struct FileWatchSource {
    watcher: notify::RecommendedWatcher,
    debounce: tokio::time::Interval,       // 500ms
    max_stable_wait: Duration,             // 30s
    check_open_files: OpenFileCheckMode,   // Auto | Always | Never
    pending_events: HashMap<PathBuf, PendingFileEvent>,
}

// 稳定确认流程:
// 1. notify 收到事件 → 入 pending_events
// 2. 启动 debounce timer
// 3. debounce 到期 → 检查文件锁 (lsof / flock)
// 4. 文件已关闭 → 发布 FILE_CREATED/CHANGED
// 5. 超 max_stable_wait → 标记 incomplete: true 强制发布
```

---

## 13. CLI 接口 (`cli`)

### 13.1 命令树

```
aman
├── run                    启动 Agent 运行时
│   ├── --config <PATH>    配置文件路径
│   ├── --soul <PATH>      SOUL.md 文件路径
│   ├── --daemon           守护进程模式
│   └── --log-level <LVL>  日志级别
│
├── skill                  技能管理
│   ├── list               列出所有 Skill
│   ├── search <QUERY>     搜索 Skill
│   ├── info <NAME>        查看 Skill 详情
│   ├── enable <NAME>      启用 Skill
│   ├── disable <NAME>     禁用 Skill
│   ├── version <NAME>     查看版本历史
│   └── rollback <NAME> <VER> 回滚版本
│
├── plugin                 插件管理
│   ├── list               列出所有插件
│   ├── enable <NAME>      启用插件
│   ├── disable <NAME>     禁用插件
│   ├── install <PATH>     安装插件
│   └── uninstall <NAME>   卸载插件
│
├── event                  事件管理
│   ├── inject <JSON>      手动注入事件
│   ├── trace <TRACE_ID>   追踪事件链路
│   └── dump <EVENT_ID>    导出事件详情
│
├── workflow               工作流管理
│   ├── list               列出所有实例
│   ├── show <ID>          查看实例详情
│   ├── retry <ID>         重试 ERROR 实例
│   └── cancel <ID>        取消实例
│
├── config                 配置管理
│   ├── show               显示当前配置
│   ├── validate            校验配置
│   └── set <KEY> <VAL>    运行时设置
│
├── dlq                    死信队列
│   ├── list               列出死信事件
│   ├── retry <ID>         重试死信事件
│   └── discard <ID>       丢弃死信事件
│
└── health                 健康检查
    └── ready               检查就绪状态
```

### 13.2 CLI 启动流程

```rust
#[tokio::main]
async fn main() -> Result<()> {
    let cli = amanCli::parse();

    match cli.command {
        Commands::Run { config, soul, daemon, log_level } => {
            // 1. 加载配置 (配置文件 → 环境变量 → 默认值)
            let config = ConfigLoader::load(config)?;

            // 2. 构建 AgentRuntime
            let runtime = AgentRuntimeBuilder::new(config)
                .with_soul(soul)?
                .with_log_level(log_level)
                .build()
                .await?;

            // 3. 启动 (Phase 0 → 5)
            let handle = runtime.start().await?;

            // 4. 等待信号 (SIGTERM/SIGINT)
            wait_for_shutdown_signal().await;

            // 5. 优雅关闭 (Phase 5 → 0)
            handle.shutdown().await?;
        }
        Commands::Skill { subcommand } => handle_skill_cmd(subcommand).await?,
        // ... 其他子命令
    }
    Ok(())
}
```

---

## 14. Tauri v2 桌面应用 (`tauri`)

### 14.1 架构

```
┌─────────────────────────────────────────────────────┐
│                  Tauri v2 Desktop App                 │
│                                                      │
│  ┌──────────────────────────┐  ┌──────────────────┐ │
│  │     Frontend (WebView)    │  │   Rust Backend    │ │
│  │                           │  │                   │ │
│  │  ┌─────────────────────┐  │  │  ┌─────────────┐ │ │
│  │  │ Dashboard           │  │  │  │ Tauri        │ │ │
│  │  │ - Event 吞吐截图     │  │  │  │ Commands     │ │ │
│  │  │ - 队列深度仪表盘     │  │  │  │ (IPC Bridge) │ │ │
│  │  │ - 背压实时状态       │  │  │  └──────┬──────┘ │ │
│  │  └─────────────────────┘  │  │         │        │ │
│  │  ┌─────────────────────┐  │  │  ┌──────▼──────┐ │ │
│  │  │ Skill Editor        │  │  │  │ AgentRuntime │ │ │
│  │  │ - YAML 声明编辑      │  │  │  │ (主进程)     │ │ │
│  │  │ - 热加载实时预览     │  │  │  └─────────────┘ │ │
│  │  │ - 版本历史 Diff      │  │  │                   │ │
│  │  └─────────────────────┘  │  └──────────────────┘ │
│  │  ┌─────────────────────┐  │                        │
│  │  │ Event Viewer         │  │                        │
│  │  │ - 实时事件流          │  │                        │
│  │  │ - TraceID 链路追踪    │  │                        │
│  │  └─────────────────────┘  │                        │
│  │  ┌─────────────────────┐  │                        │
│  │  │ Workflow Board       │  │                        │
│  │  │ - 状态机可视化        │  │                        │
│  │  │ - 实例列表/操作       │  │                        │
│  │  └─────────────────────┘  │                        │
│  │  ┌─────────────────────┐  │                        │
│  │  │ SOUL Editor          │  │                        │
│  │  │ - Markdown 编辑器     │  │                        │
│  │  │ - 实时 SystemPrompt   │  │                        │
│  │  │   预览                │  │                        │
│  │  └─────────────────────┘  │                        │
│  │  ┌─────────────────────┐  │                        │
│  │  │ Plugin Manager       │  │                        │
│  │  │ - 插件列表/状态       │  │                        │
│  │  │ - 安装/卸载/启用      │  │                        │
│  │  └─────────────────────┘  │                        │
│  └──────────────────────────┘                        │
└─────────────────────────────────────────────────────┘
```

### 14.2 Tauri State 管理

```rust
// src-tauri/src/state.rs

pub struct AppState {
    pub runtime: Arc<Mutex<Option<AgentRuntimeHandle>>>,
    pub metrics_store: Arc<MetricsStore>,
    pub soul: Arc<RwLock<Option<Arc<Soul>>>>,
}

// Tauri 启动时:
// 1. 创建 AppState
// 2. 构建 AgentRuntime (但不启动)
// 3. Tauri Builder 注册 commands + state
// 4. 前端加载 → 用户通过 UI 点击 "Start" 启动 Runtime
```

### 14.3 Tauri Commands (IPC 桥)

```rust
#[tauri::command]
async fn start_runtime(
    state: State<'_, AppState>,
    config_path: Option<String>,
    soul_path: Option<String>,
) -> Result<String, String> {
    let mut runtime_guard = state.runtime.lock().unwrap();
    if runtime_guard.is_some() {
        return Err("Runtime already running".into());
    }

    let config = load_config(config_path).map_err(|e| e.to_string())?;
    let runtime = AgentRuntimeBuilder::new(config)
        .with_soul(soul_path)
        .build()
        .await
        .map_err(|e| e.to_string())?;

    let handle = runtime.start().await.map_err(|e| e.to_string())?;
    *runtime_guard = Some(handle);
    Ok("Runtime started".into())
}

#[tauri::command]
async fn get_metrics(state: State<'_, AppState>) -> Result<MetricsSnapshot, String> {
    state.metrics_store.snapshot().map_err(|e| e.to_string())
}

#[tauri::command]
async fn search_skills(
    state: State<'_, AppState>,
    query: String,
) -> Result<Vec<SkillMatch>, String> {
    // 通过 runtime 获取 SkillRegistry → search
    todo!()
}

#[tauri::command]
async fn inject_event(
    state: State<'_, AppState>,
    event_json: String,
) -> Result<String, String> {
    // 手动注入事件 (调试)
    todo!()
}

#[tauri::command]
async fn get_event_trace(
    state: State<'_, AppState>,
    trace_id: String,
) -> Result<EventTrace, String> {
    todo!()
}

#[tauri::command]
async fn get_workflow_instances(
    state: State<'_, AppState>,
) -> Result<Vec<WorkflowInstanceInfo>, String> {
    todo!()
}

#[tauri::command]
async fn update_soul(
    state: State<'_, AppState>,
    content: String,
) -> Result<String, String> {
    let soul = Soul::from_str(&content).map_err(|e| e.to_string())?;
    let mut guard = state.soul.write().unwrap();
    *guard = Some(Arc::new(soul));
    Ok("SOUL updated".into())
}

#[tauri::command]
async fn preview_system_prompt(
    state: State<'_, AppState>,
) -> Result<String, String> {
    let guard = state.soul.read().unwrap();
    match guard.as_ref() {
        Some(soul) => Ok(soul.to_system_prompt()),
        None => Err("No SOUL loaded".into()),
    }
}
```

### 14.4 实时事件流 (SSE → Tauri Events)

```
AgentRuntime → Event Bus → Hook "EventProcessed"
    → MetricsCollector Hook
    → Tauri EventEmitter.emit("event:processed", payload)
    → 前端 EventSource 监听 → Reactively 更新 Dashboard
```

```rust
// MetricsCollector Hook 内部:
impl Hook for MetricsCollector {
    async fn execute(&self, point: HookPoint, ctx: HookContext) -> Result<()> {
        if matches!(point, HookPoint::EventProcessed(_)) {
            // 更新指标存储
            self.store.record_event_processed();

            // 推送到 Tauri 前端 (通过 Tauri Emitter)
            if let Some(emitter) = &self.tauri_emitter {
                let _ = emitter.emit("metrics:updated", self.store.snapshot()?);
            }
        }
        Ok(())
    }
}
```

---

## 15. 插件隔离 (`plugin`)

### 15.1 三种隔离模式

```rust
pub enum PluginIsolation {
    InProcess,       // 同进程 → 接口隔离 (Arc<dyn Plugin>)
    Subprocess,      // 子进程 → IPC (stdin/stdout JSON-RPC)
    Wasm,            // WebAssembly → wasmtime runtime
}
```

### 15.2 WASM 插件实现 (进阶)

```rust
// WASM 插件通过 wasmtime 加载 .wasm 模块
// Skill 开发者用 Rust 编写，编译为 wasm32-wasi target

pub struct WasmPlugin {
    engine: wasmtime::Engine,
    store: wasmtime::Store<WasmState>,
    instance: wasmtime::Instance,
    // 导出的函数:
    //   - skill_execute(event_ptr: i32, event_len: i32) -> i32
    //   - skill_on_load() -> i32
    //   - skill_on_unload() -> i32
}

impl WasmPlugin {
    pub fn load(wasm_path: &Path) -> Result<Self> {
        let engine = wasmtime::Engine::default();
        let module = wasmtime::Module::from_file(&engine, wasm_path)?;
        let mut store = wasmtime::Store::new(&engine, WasmState::default());
        let instance = wasmtime::Instance::new(&mut store, &module, &[])?;
        Ok(Self { engine, store, instance })
    }
}

#[async_trait]
impl Plugin for WasmPlugin {
    fn skills(&self) -> Vec<Arc<dyn Skill>> {
        // 从 WASM 模块导出的 Skill 注册
    }
    // ...
}
```

### 15.3 插件依赖加载算法

```rust
pub struct PluginLoader {
    manifests: HashMap<String, PluginManifest>,
    loaded: HashMap<String, Arc<dyn Plugin>>,
}

impl PluginLoader {
    pub fn load_all(&mut self) -> Result<Vec<Arc<dyn Plugin>>> {
        // 1. 构建依赖图 DAG
        let graph = DependencyGraph::from_manifests(&self.manifests);

        // 2. 拓扑排序 + 环检测
        let sorted = graph.topological_sort()?;  // 有环 → Err

        // 3. 依次加载 (拓扑序)
        for name in sorted {
            let manifest = self.manifests.get(&name).unwrap();

            // 3a. 检查依赖已加载
            for dep in &manifest.depends_on {
                let loaded_ver = self.loaded.get(&dep.name).unwrap().version();
                if !dep.version_range.matches(loaded_ver) {
                    return Err(Error::VersionMismatch { .. });
                }
            }

            // 3b. 加载插件
            let plugin = self.load_single(manifest)?;
            self.loaded.insert(name, plugin);
        }

        Ok(self.loaded.values().cloned().collect())
    }
}
```

---

## 16. 配置系统 (`config`)

### 16.1 配置加载层级 (优先级从高到低)

```rust
pub struct ConfigLoader;

impl ConfigLoader {
    pub fn load(path: Option<PathBuf>) -> Result<AgentConfig> {
        // Layer 1: 框架默认值 (硬编码)
        let mut config = AgentConfig::default();

        // Layer 2: 配置文件 (aman.yaml)
        if let Some(ref path) = path {
            let file_config: AgentConfig = serde_yaml::from_reader(File::open(path)?)?;
            config.merge(file_config);
        }

        // Layer 3: 环境变量覆盖 (AMAN_*)
        config.apply_env_overrides();

        // Layer 4: 运行时 override 文件 (cron_override.yaml)
        config.apply_runtime_overrides()?;

        // 校验
        config.validate()?;

        Ok(config)
    }
}
```

### 16.2 配置校验规则

```rust
impl AgentConfig {
    pub fn validate(&self) -> Result<()> {
        // 1. 总线模式绑定: in_memory 下不允许 persistence.* 字段
        // 2. 超时合理性: drain_timeout < 某些 Tool timeout 检查
        // 3. 循环检测: Plugin 依赖不能有环
        // 4. 必填字段: workflows 的 initial_state 必须在 states 中
        // 5. 互斥字段: notify_on_complete 与 watch_patterns 不能同时设置
        // 6. 大小写警告: workflow 状态名大写/小写不一致时发警告
        // ...
    }
}
```

---

## 17. 安全性设计

### 17.1 Secret 管理 (`secret`)

```rust
pub struct SecretResolver {
    backends: Vec<Box<dyn SecretBackend>>,   // Vault → AWS SM → Env (last resort)
    cache: SecretCache,                       // 内存加密缓存
}

#[async_trait]
pub trait SecretBackend: Send + Sync {
    async fn get(&self, key: &str) -> Result<Secret>;
    fn priority(&self) -> u32;              // 优先级
}

impl SecretResolver {
    // 扫描配置中所有 ${VAR} 模式 → 解析
    pub async fn resolve_all(&self, config: &mut serde_json::Value) -> Result<()>;

    // Secret 热更新 (带宽限期)
    pub async fn rotate(&self, keys: &[String]) -> Result<RotationResult>;
}
```

### 17.2 内存安全

```rust
pub struct EncryptedMemory<T: Serialize> {
    encrypted: Vec<u8>,
    nonce: [u8; 12],
    key: [u8; 32],           // AES-256-GCM key
}

impl<T: Serialize + DeserializeOwned> EncryptedMemory<T> {
    pub fn seal(value: &T, key: &[u8; 32]) -> Result<Self>;
    pub fn open(&self) -> Result<T>;
    // 使用后立即 drop，不保留明文副本
}
```

### 17.3 LLM 注入防护

```rust
pub struct InputSanitizer {
    injection_patterns: Vec<Regex>,  // 已知注入模式
    trust_level: TrustLevel,
}

pub enum TrustLevel {
    Trusted,       // 内部系统事件，直接传递
    Untrusted,     // 用户消息/Webhook，需消毒
    Sandboxed,     // 高风险来源，额外受限
}

impl InputSanitizer {
    pub fn sanitize(&self, input: &str) -> SanitizedInput;
    pub fn detect_injection(&self, input: &str) -> Option<InjectionWarning>;
}
```

---

## 18. 可观测性

### 18.1 Tracing (OpenTelemetry)

```rust
// 每个事件处理自动创建 span:
//   Span: event_processing
//     ├── Span: dispatcher_route
//     ├── Span: skill_execute ("slack-notification")
//     │   └── Span: tool_execute ("http-post")
//     └── Span: event_published("NOTIFY_SENT")

// 通过 TraceID 可追踪完整事件链路
pub struct TracingLayer;

impl TracingLayer {
    // 框架自动在 Event publish 时注入 trace_id
    // 框架自动在 Dispatcher 分发时创建 child span
    // 框架自动在 Skill/Pipeline 执行时创建 child span
    // 框架自动在 Tool 调用时记录 attributes
}
```

### 18.2 Metrics (Prometheus)

```rust
// 暴露指标:
//   event_bus_queue_depth{priority="high|normal|low"}
//   event_throughput_total
//   backpressure_level
//   events_discarded_total{reason="backpressure_l2"}
//   retry_queue_depth
//   inflight_pipelines
//   inflight_skills
//   plugin_health{plugin="...", status="ok|degraded|failed"}
//   dlq_depth

pub struct MetricsEndpoint {
    registry: prometheus::Registry,
}

impl MetricsEndpoint {
    pub fn serve(&self) -> impl Future<Output = String> {
        // 返回 Prometheus exposition format
        self.registry.gather()
    }
}
```

### 18.3 审计日志

```rust
pub struct AuditLogger {
    // 记录:
    // - 配置变更 (who, what, old, new, when)
    // - Secret 轮换 (which keys, old/new fingerprint_created, when)
    // - DLQ 操作 (retry/discard, operator, when)
    // - Cron 变更 (add/update/remove/reconfigure, when)
    // - 注入尝试 (LLM injection patterns matched)
    // - 插件操作 (load/unload/enable/disable)
}

impl AuditLogger {
    pub fn log(&self, entry: AuditEntry);
    pub fn query(&self, filter: AuditFilter) -> Vec<AuditEntry>;
}
```

---

## 19. 运行时启动/关闭编排 (`runtime`)

### 19.1 AgentRuntime 构建器

```rust
pub struct AgentRuntimeBuilder {
    config: AgentConfig,
    soul: Option<Soul>,
    log_level: Level,
}

impl AgentRuntimeBuilder {
    pub fn new(config: AgentConfig) -> Self { .. }

    pub fn with_soul(mut self, soul_path: Option<PathBuf>) -> Result<Self> {
        if let Some(path) = soul_path {
            self.soul = Some(Soul::from_file(&path)?);
        }
        Ok(self)
    }

    pub async fn build(self) -> Result<AgentRuntime> {
        // 按 Phase 顺序构建各组件
        // Phase 0: Event Bus
        // Phase 0.5: Secret Resolver
        // Phase 1: WAL/Checkpoint
        // Phase 2: Plugin Loader (拓扑序) + Skill Registry + Dispatcher
        // Phase 3: State Store + Workflow Engine
        // Phase 4: Event Sources (待激活)
        // 返回 AgentRuntime (未启动)
        todo!()
    }
}
```

### 19.2 启动序列 (Phase 0→5)

```rust
impl AgentRuntime {
    pub async fn start(self) -> Result<AgentRuntimeHandle> {
        // Phase 0: 初始化 Event Bus (+ 背压系统)
        self.event_bus.init().await?;
        tracing::info!("Phase 0: Event Bus initialized");

        // Phase 0.5: 密钥解析 (重试 + 降级)
        self.secret_resolver.resolve_with_retry(3).await?;
        tracing::info!("Phase 0.5: Secrets resolved");

        // Phase 1: WAL 恢复 / checkpoint 加载 / 待重试队列重建
        let replay_events = self.wal.replay_from_checkpoint().await?;
        self.retry_queue.load_pending().await?;
        tracing::info!("Phase 1: WAL recovery complete ({} events)", replay_events.len());

        // Phase 2: 插件加载 (拓扑序) + Skill 注册 + Dispatcher 路由注入
        let plugins = self.plugin_loader.load_all().await?;  // 超时: plugin_load_timeout
        for plugin in plugins {
            self.skill_registry.register_from_plugin(&plugin)?;
            self.tool_registry.register_from_plugin(&plugin)?;
            self.source_registry.register_from_plugin(&plugin)?;
        }
        self.dispatcher.rebuild_routes(
            &self.config,
            &self.skill_registry,
            &self.pipeline_registry,
            &self.workflow_engine,
        )?;
        // ⚠ Phase 2 完成后才注入 WAL 恢复事件
        for event in replay_events {
            self.event_bus.publish(event).await?;
        }
        tracing::info!("Phase 2: Components registered ({} plugins)", plugins.len());

        // Phase 3: Workflow 实例恢复 (超时: workflow_recovery_timeout)
        let recovered = self.workflow_engine.recover_instances(Duration::from_secs(120)).await?;
        tracing::info!("Phase 3: Workflow recovery ({}/{} instances)", recovered.0, recovered.1);

        // Phase 4: Event Source 激活
        for source in self.source_registry.sources() {
            source.init(SourceContext::new(&self.event_bus)).await?;
        }
        tracing::info!("Phase 4: Event Sources activated");

        // Phase 5: 就绪
        self.health.set_ready();
        tracing::info!("Phase 5: Agent ready");

        Ok(AgentRuntimeHandle {
            shutdown_tx: self.shutdown_tx,
            health: self.health,
            // ...
        })
    }
}
```

### 19.3 优雅关闭 (Phase 5→0)

```rust
impl AgentRuntimeHandle {
    pub async fn shutdown(self) -> Result<()> {
        // Phase 5: 停止接收 (health → 503)
        self.health.set_not_ready();
        tracing::info!("Phase 5: Draining requests");

        // Phase 4: 停止 Event Source
        for source in self.source_registry.sources() {
            source.shutdown().await?;
        }
        tracing::info!("Phase 4: Sources stopped");

        // Phase 4.5: 排水 (等待 inflight Pipeline/Skill 完成)
        let drained = self.drain_inflight(Duration::from_secs(30)).await;
        if !drained.is_empty() {
            tracing::warn!("Phase 4.5: {} inflight executions aborted", drained.len());
        }
        tracing::info!("Phase 4.5: Drain complete");

        // Phase 3: Workflow 状态落盘
        self.workflow_engine.checkpoint_all().await?;
        tracing::info!("Phase 3: Workflows checkpointed");

        // Phase 2: 插件卸载 (反向拓扑序)
        for plugin in self.plugin_loader.unload_all().await? {
            tracing::info!("Phase 2: Plugin {} unloaded", plugin.name());
        }

        // Phase 1: WAL 最终 checkpoint + 待重试队列落盘
        self.wal.final_checkpoint().await?;
        self.retry_queue.flush().await?;
        tracing::info!("Phase 1: WAL flushed");

        // Phase 0: Event Bus 关闭
        self.event_bus.shutdown().await?;
        tracing::info!("Phase 0: Event Bus shutdown");

        Ok(())
    }
}
```

---

## 20. 外部接口汇总

### 20.1 HTTP API (actix-web / axum)

```
健康:
  GET  /health/live               → 200 always (进程存活)
  GET  /health/ready              → 200 (Phase 5) | 503 (otherwise)

运行时:
  POST /agent/start               → 200 OK | 409 Conflict | 500
  POST /agent/shutdown            → 200 OK (同步阻塞到关闭完成)

事件源:
  POST /source/{id}/pause
  POST /source/{id}/resume
  PUT  /source/{id}/config

插件:
  POST /plugin/{name}/enable
  POST /plugin/{name}/disable
  POST /plugin/install            ← multipart: plugin.tar.gz
  POST /plugin/{name}/uninstall

Cron:
  POST /cron/add
  POST /cron/{id}/update
  POST /cron/{id}/remove

调试:
  POST /inject-event              → (生产环境默认禁用)
  GET  /events/trace/{trace_id}
  GET  /events/dump/{id}

DLQ:
  GET  /dlq
  POST /dlq/{id}/retry
  POST /dlq/{id}/discard

可观测:
  GET  /metrics                   → Prometheus exposition format
  GET  /audit-log                 → cursor 分页 + 过滤
```

### 20.2 事件类型注册表

```rust
// 框架内置事件类型
pub enum EventType {
    // 系统
    heartbeat,
    agent_started,
    agent_shutdown,

    // 文件
    file_created,
    file_changed,
    file_deleted,

    // 定时
    timer_tick,
    cron_tick,

    // 网络
    webhook_received,
    message_received,
    network_data,

    // 数据
    data_inserted,
    data_updated,
    data_deleted,
    poll_result,

    // 信号
    system_signal,

    // 框架内部
    skill_loaded,
    skill_unloaded,
    plugin_enabled,
    plugin_disabled,
    config_changed,
    secret_rotated,
    soul_changed,
    dlq_event_retried,

    // 自定义 (可扩展)
    custom(String),
}
```

---

## 21. 技术栈总结

| 层 | 选型 | 理由 |
|---|------|------|
| 异步运行时 | tokio (multi-thread) | Rust 生态标准，性能最优 |
| 序列化 | serde + serde_json + serde_yaml | 事实标准 |
| HTTP 服务 | axum (API) | 基于 tower，与 Event Bus 的 tower::Service 组合 |
| Tauri 桌面 | tauri 2.x | Rust 原生 + WebView 前端 |
| 嵌入式 KV | sled | 纯 Rust，零依赖，嵌入式 |
| 全文检索 | tantivy | Rust 原生，Lucene-level 能力 |
| 文件监控 | notify 6.x | 跨平台 (inotify/FSEvents/ReadDirectoryChanges) |
| WASM 沙箱 | wasmtime | 字节码联盟出品，安全优先 |
| 模板解析 | cron | cron 表达式解析 |
| UUID | uuid v7 | 时间有序 UUID |
| 日志 | tracing + tracing-subscriber | 结构化日志 + OpenTelemetry |
| WebSocket | tokio-tungstenite | 实时事件流推送 |
| CLI | clap 4.x | derive 模式，子命令支持 |
| 前端 (Tauri) | Svelte 5 + Vite | 体积小，性能好 (可选 React/Vue) |

---

## 22. 设计决策记录

### D1: Trait 静态派发为主

核心路径 (Event, Pipeline, Dispatcher) 使用泛型 + 静态派发，避免虚函数开销。插件边界使用 `Arc<dyn Trait>` 动态派发——那里是自然的隔离边界。

### D2: sled 而非 RocksDB

sled 是纯 Rust 嵌入式 KV 存储，无 FFI 依赖，编译/部署简单。性能在大规模场景下可通过 StateStore trait 替换为 RocksDB。

### D3: wasmtime 作为 WASM 运行时

wasmtime 是 Rust 生态中最成熟的 WASM 运行时，支持 WASI 标准，安全隔离性好，且与 tokio 可集成。

### D4: 前端框架不绑定

Tauri 前端通过 IPC 与 Rust 后端通信，前端框架可替换。默认建议 Svelte (性能好，体积小)，但架构设计不强制。

### D5: 配置格式 YAML

声明式配置 (YAML) 是框架设计文档的要求。JSON 作为 API 数据格式，YAML 作为人类可读配置格式。

---

*本架构设计对应 [agent-design.md](./agent-design.md) v1.0，实现时请以设计文档为准，本文件定义 Rust 层的具体结构。*
