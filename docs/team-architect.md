# Team 插件架构文档 (team-architect.md)

## 一、定位

Team 是一个**看板调度器插件**，不引入通信空间（chat）。交互模型纯粹通过 Work 系统完成：

- 人类在 Kanban UI 创建 work item → Team 调度器匹配 Agent 能力 → 推送到 Agent Work 队列
- Agent 执行完成 → 工作流自动流转 → 安全围栏检查 → 下一 stage 触发调度器再次分配
- 人类和 Agent 之间没有 @mention、没有消息回显，只有 work item 的创建/分配/完成

Server 端不硬编码任何 Team 逻辑。Team 作为插件，通过 Plugin trait 和 Work 系统与内核交互。

## 二、复用基础设施

| 层次 | 复用 aman 机制 | Team 专属扩展 |
|------|---------------|-------------|
| 配置 | `config` crate 4层加载 + `AgentConfig::validate()` | `team.yaml` 解析 + `TeamConfig` |
| Agent | `AgentRegistry` / `AgentInstance` / SOUL.md | capabilities + autonomy + allowed_stages |
| 执行 | `WorkSystem` (FIFO 消费) + `AgentHarness` (ReAct loop) | 无 — 完全复用 |
| 工作流 | `WorkflowEngine` (StateDef, Transition, Guard) | `team stages → WorkflowDef` 编译 |
| 事件 | `EventBus` + `EventType::Custom("team:*")` | 6 个 Team 专属事件 |
| 存储 | `persistence` (WAL, StateStore) + SQLite | `team.db` (2 张表) |
| Hook | `HookConfig` + `ScriptRuntime` (任意语言) | 安全围栏告警通知 |
| 插件 | `PluginManifest` + `PluginCandidate` | TeamPlugin (InProcess 或 Subprocess) |
| UI | `UiDeclaration.pages` + 动态导航 | Team.svelte (Kanban 视图) |

## 三、事件流

```
Kanban UI (人类创建 work item)
  │
  ├── POST /api/v1/team/{id}/tasks/create
  │     │
  │     └── team:work_item.created ──► EventBus
  │                                         │
  │                                         ▼
  │                                   TeamScheduler
  │                                    ├─ 匹配 stage.required_capabilities ←→ agent.capabilities
  │                                    ├─ 检查 agent.queue_max_size
  │                                    └─ push → agent WorkSystem
  │                                               │
  │                                         team:work_item.assigned
  │
Agent WorkSystem: IDLE → BUSY → 执行步骤 → 完成
  │
  └── team:work_item.completed ──► WorkflowEngine.transition()
                                        │
                                        ├─ SafetyGateHandler
                                        │   ├─ dangerous_action pattern match
                                        │   └─ confidence < min → team:safety.gate_triggered
                                        │
                                        ├─ Guard 通过 → 流转到 next stage
                                        │   └─ next stage auto_assign=true → 再次调度
                                        │
                                        └─ Guard 拦截 → 暂停流转，通知 UI
```

## 四、Team 专属事件

```rust
// 全部通过 EventType::Custom("team:*") 发布
pub mod team_events {
    pub const WORK_ITEM_CREATED:  &str = "team:work_item.created";
    pub const WORK_ITEM_ASSIGNED: &str = "team:work_item.assigned";
    pub const WORK_ITEM_STAGE_CHANGED: &str = "team:work_item.stage_changed";
    pub const WORK_ITEM_COMPLETED: &str = "team:work_item.completed";
    pub const WORK_ITEM_FAILED: &str = "team:work_item.failed";

    pub const SAFETY_GATE_TRIGGERED: &str = "team:safety.gate_triggered";
    pub const SAFETY_GATE_RESOLVED:  &str = "team:safety.gate_resolved";
}
```

相比原 15 个事件，砍掉了所有通信空间事件（`message.*`、`agent.invoked`、`agent.thought`、`agent.action`、`agent.response`），只保留 work item 生命周期 + 安全围栏。

## 五、Crate 结构

```
kernel/plugins/team/
├── plugin.yaml              # PluginManifest
├── Cargo.toml
└── src/
    ├── lib.rs               # TeamPlugin: Plugin trait impl
    ├── config.rs            # TeamConfig 解析 + 校验
    ├── store.rs             # SQLite 操作 (2 表: safety_log, context)
    ├── scheduler.rs         # 能力匹配 + DispatchStrategy + push
    ├── safety_gate.rs       # 危险操作拦截 + 置信度检查
    ├── workflow_compiler.rs # team stages → WorkflowDef
    ├── context_loader.rs    # context_files 加载 + 缓存
    └── api.rs               # HTTP 端点 (/team/*)
```

## 六、配置设计

### 6.1 team.yaml

```yaml
team:
  name: "Aman Core Team"
  description: "Aman agent framework development"

members:
  - id: "jerin"
    type: human
    name: "Jerin"
    roles: [owner]

  - id: "coder"
    type: agent
    name: "Coder"
    profile: "coder"
    capabilities: [code, refactor, fix]
    autonomy: autonomous              # autonomous | supervised | on_mention
    allowed_stages: ["wip", "review_fix"]
    queue_max_size: 5

  - id: "reviewer"
    type: agent
    profile: "code-auditor"
    capabilities: [review, audit, security_check]
    autonomy: autonomous
    allowed_stages: ["review"]
    queue_max_size: 3

stages:
  - id: "backlog"
    name: "待办"
    order: 1
    allowed_next: ["wip"]

  - id: "wip"
    name: "处理中"
    order: 2
    allowed_next: ["review", "backlog"]
    assignment_policy:
      auto_assign: true
      required_capabilities: [code, refactor, fix]
      execution_timeout_minutes: 120
      dispatch_strategy: "best_match"   # best_match | least_loaded | random_idle

  - id: "review"
    name: "审核"
    order: 3
    allowed_next: ["wip", "done"]
    assignment_policy:
      auto_assign: true
      required_capabilities: [review, audit]
      execution_timeout_minutes: 60
      dispatch_strategy: "least_loaded"

  - id: "done"
    name: "完成"
    order: 4
    allowed_next: []

safety_gates:
  dangerous_actions:
    - pattern: "rm -rf"
    - pattern: "git push --force"
    - pattern: "publish|deploy|release"
    - pattern: "DROP |DELETE FROM|TRUNCATE"
  min_confidence: 0.7
  max_autonomous_actions_without_human: 20

initial_stage: "backlog"
context_files:
  - "docs/architecture.md"
  - "docs/coding-standards.md"
work_dir: "/Users/jerin/projects/aman"
```

### 6.2 对应 Rust 类型

```rust
pub struct TeamConfig {
    pub team: TeamMeta,
    pub members: Vec<TeamMember>,
    pub stages: Vec<Stage>,
    pub safety_gates: SafetyGateConfig,
    pub initial_stage: String,
    pub context_files: Vec<String>,
    pub work_dir: PathBuf,
}

pub struct TeamMember {
    pub id: String,
    pub member_type: MemberType,    // Human | Agent
    pub name: String,
    pub profile: Option<String>,    // Agent 专用
    pub capabilities: Vec<String>,
    pub autonomy: Autonomy,         // Autonomous | Supervised | OnMention
    pub allowed_stages: Vec<String>,
    pub queue_max_size: usize,
    pub context_hint: Option<String>,
}

pub struct Stage {
    pub id: String,
    pub name: String,
    pub order: u32,
    pub allowed_next: Vec<String>,
    pub description: Option<String>,
    pub assignment_policy: Option<AssignmentPolicy>,
}

pub struct AssignmentPolicy {
    pub auto_assign: bool,
    pub required_capabilities: Vec<String>,
    pub execution_timeout_minutes: u64,
    pub dispatch_strategy: DispatchStrategy,
}

pub enum DispatchStrategy {
    BestMatch,     // 能力交集最大
    LeastLoaded,   // 当前队列最短
    RandomIdle,    // 随机空闲
}
```

## 七、数据存储

### 7.1 team.db 表结构

work item 和阶段历史由 `WorkflowEngine` + `StateStore` 管理，Team 插件不再重复存储。`team.db` 只保留 Team 独有的两张表：

```
team.db
├── safety_log      -- 安全围栏决策记录
└── context         -- 共享文档缓存
```

### 7.2 safety_log

| 字段 | 类型 | 说明 |
|------|------|------|
| id | INTEGER | 自增主键 |
| work_item_id | TEXT | WorkItemId |
| agent_id | TEXT | 触发 agent member_id |
| action | TEXT | 被拦截的操作 |
| reason | TEXT | 'dangerous_action' \| 'low_confidence' \| 'permission_denied' |
| human_decision | TEXT | 'approved' \| 'denied' \| NULL (待处理) |
| decided_by | TEXT | 决策人 |
| created_at | DATETIME | 触发时间 |
| resolved_at | DATETIME | 决策时间 |

### 7.3 context

| 字段 | 类型 | 说明 |
|------|------|------|
| id | INTEGER | 自增主键 |
| title | TEXT | 文档标题 |
| file_path | TEXT | 相对 work_dir 路径 |
| content | TEXT | 缓存的内容快照 |
| category | TEXT | 'architecture' \| 'standard' \| 'decision' \| 'general' |
| updated_at | DATETIME | 文件最后修改时间 |
| indexed_at | DATETIME | 索引时间 |

## 八、调度器

```rust
pub struct TeamScheduler {
    config: TeamConfig,
    agent_registry: Arc<AgentRegistry>,
}

impl TeamScheduler {
    /// work item 进入可分配 stage 时调用
    pub async fn dispatch(
        &self,
        item: &WorkItem,
        stage_id: &str,
    ) -> AmanResult<AgentId> {
        let stage = self.config.find_stage(stage_id)?;
        let policy = stage.assignment_policy.as_ref()
            .ok_or(WorkError::NoAssignmentPolicy)?;

        let candidates = self.config.members.iter()
            .filter(|m| m.member_type.is_agent())
            .filter(|m| m.autonomy != Autonomy::OnMention)
            .filter(|m| {
                // 能力匹配
                m.capabilities.iter()
                    .any(|c| policy.required_capabilities.contains(c))
            })
            .filter(|m| {
                // 队列未满: 通过 AgentRegistry 查 WorkSystem queue_length
                let ws = self.agent_registry.get_work_system(&m.id)?;
                ws.queue_length() < m.queue_max_size
            })
            .collect::<Vec<_>>();

        let target = match policy.dispatch_strategy {
            DispatchStrategy::BestMatch => {
                candidates.into_iter()
                    .max_by_key(|m| m.capabilities.iter()
                        .filter(|c| policy.required_capabilities.contains(c))
                        .count())
            }
            DispatchStrategy::LeastLoaded => {
                candidates.into_iter()
                    .min_by_key(|m| self.agent_registry
                        .get_work_system(&m.id)
                        .map(|ws| ws.queue_length())
                        .unwrap_or(usize::MAX))
            }
            DispatchStrategy::RandomIdle => {
                candidates.into_iter()
                    .filter(|m| self.agent_registry
                        .get_work_system(&m.id)
                        .map(|ws| ws.queue_length() == 0)
                        .unwrap_or(false))
                    .choose(&mut rand::thread_rng())
            }
        }.ok_or(WorkError::NoEligibleAgent)?;

        // 通过 Agent 的 WorkSystem 直接推送
        let ws = self.agent_registry.get_work_system(&target.id)?;
        ws.push_work_item(item.clone(), WorkItemSource::Kanban {
            board_id: self.config.team.name.clone(),
            scheduler: "team".into(),
        }).await?;

        Ok(target.id)
    }
}
```

## 九、安全围栏

```rust
pub struct SafetyGateHandler {
    config: SafetyGateConfig,
    store: TeamStore,
}

impl SafetyGateHandler {
    /// 在 Agent 执行 action 前检查危险操作
    pub fn check_action(&self, action: &str, agent_id: &str, work_item_id: &str) -> SafetyResult {
        for pattern in &self.config.dangerous_actions {
            if pattern.pattern_matches(action) {
                self.store.insert_safety_log(work_item_id, agent_id, action, "dangerous_action")?;
                return SafetyResult::Blocked {
                    reason: format!("危险操作: {action}"),
                    requires_human: true,
                };
            }
        }
        SafetyResult::Allowed
    }

    /// Agent 完成 work item 时检查置信度
    pub fn check_confidence(&self, confidence: f64, work_item_id: &str, agent_id: &str) -> SafetyResult {
        if confidence < self.config.min_confidence {
            self.store.insert_safety_log(work_item_id, agent_id, "", "low_confidence")?;
            return SafetyResult::PendingHumanDecision;
        }
        SafetyResult::Allowed
    }
}
```

安全围栏是 WorkflowDef 的 Guard 函数，嵌入在每个 `Transition` 中：

```rust
// workflow_compiler.rs
fn build_safety_guard(config: &TeamConfig, from: &str, to: &str) -> Option<Box<dyn Fn(&Value) -> bool>> {
    Some(Box::new(move |ctx: &Value| -> bool {
        let agent_id = ctx["agent_id"].as_str().unwrap_or("");
        let confidence = ctx["confidence"].as_f64().unwrap_or(1.0);

        if let Some(member) = config.find_member(agent_id) {
            if !member.allowed_stages.contains(&to.to_string()) {
                return false;
            }
        }
        confidence >= config.safety_gates.min_confidence
    }))
}
```

## 十、WorkflowDef 编译

```rust
// workflow_compiler.rs
pub fn compile_team_workflow(config: &TeamConfig) -> WorkflowDef {
    let mut states = Vec::new();
    let mut transitions = Vec::new();

    for stage in &config.stages {
        states.push(StateDef {
            name: stage.id.clone(),
            display: stage.name.clone(),
            timeout: stage.assignment_policy.as_ref().map(|p| StateTimeout {
                duration: Duration::from_secs(p.execution_timeout_minutes * 60),
                on_timeout: "team:work_item.failed".into(),
            }),
        });

        for next_id in &stage.allowed_next {
            transitions.push(Transition {
                from: TransitionFrom::State(stage.id.clone()),
                event: format!("team:stage.{}.{}", stage.id, next_id),
                to: TransitionTo::State(next_id.clone()),
                guard: build_safety_guard(config, &stage.id, next_id),
            });
        }
    }

    WorkflowDef {
        name: format!("team-{}", config.team.name),
        states,
        initial_state: config.initial_stage.clone(),
        final_states: config.stages.iter()
            .filter(|s| s.allowed_next.is_empty())
            .map(|s| s.id.clone())
            .collect(),
        transitions,
        ..Default::default()
    }
}
```

## 十一、Plugin trait 变动

### 11.1 Plugin::routes()

```rust
// kernel/core/src/plugin.rs
#[async_trait]
pub trait Plugin: Send + Sync {
    // ... 现有方法 ...

    /// 插件贡献的 HTTP 路由。默认返回 None。
    fn routes(&self) -> Option<axum::Router> { None }
}
```

AgentRuntime 在 `build_router()` 中合并：

```rust
// kernel/gateway/src/runtime/http.rs
fn build_router(runtime: Arc<AgentRuntime>) -> Router {
    let mut app = Router::new()
        .route("/health/live", get(health_live))
        // ...
        .merge(control);

    for plugin in runtime.active_plugins() {
        if let Some(router) = plugin.routes() {
            app = app.nest("/api/v1", router);
        }
    }
    app.with_state(runtime)
}
```

### 11.2 Plugin 支持脚本运行时

复用现有的 `ScriptRuntime`（`kernel/core/src/script.rs`），扩展 `PluginCandidate` 支持脚本驱动的插件：

```yaml
# plugin.yaml — Subprocess 类型插件声明
name: "team"
version: "0.1.0"
isolation: subprocess
runtime: python3                    # 任意 PATH 上的解释器
min_version: ">=3.11"
entrypoint: "main.py"               # 相对插件目录的脚本路径
ui:
  pages: ["team"]
  events:
    - "team:work_item.updated"
    - "team:safety.alert"
```

对应的 PluginManifest 扩展：

```rust
pub struct PluginManifest {
    // ... 现有字段 ...
    pub runtime: Option<String>,          // python3, node, bash, deno, ...
    pub min_version: Option<String>,      // semver range
    pub entrypoint: Option<PathBuf>,      // 脚本入口
}
```

脚本插件通过 stdin/stdout JSON-RPC 2.0 通信（复用 `SubprocessPluginClient` 协议）。这样 Team 可以用 Python/JS/Bash 任意语言实现，不再限于 Rust。

## 十二、多 Team 支持

一个 aman 实例可运行多个 Team，通过 `/team/{team_id}` 隔离：

```
~/.aman/teams/
├── aman-core/
│   ├── team.yaml
│   └── team.db
├── investloop/
│   ├── team.yaml
│   └── team.db
└── game00/
    ├── team.yaml
    └── team.db
```

每个 Team 创建独立的 `TeamConfig`、`TeamStore`（独立 SQLite）、`WorkflowDef`。

## 十三、HTTP API

```rust
// kernel/plugins/team/src/api.rs
pub fn team_api_routes() -> Router {
    Router::new()
        // Work Items
        .route("/team/{team_id}/tasks", get(team_list_tasks))
        .route("/team/{team_id}/tasks/create", post(team_create_task))
        .route("/team/{team_id}/tasks/{id}", get(team_get_task))
        .route("/team/{team_id}/tasks/{id}/assign", post(team_assign_task))
        .route("/team/{team_id}/tasks/{id}/complete", post(team_complete_task))

        // 安全围栏
        .route("/team/{team_id}/safety/pending", get(team_pending_gates))
        .route("/team/{team_id}/safety/{id}/resolve", post(team_resolve_gate))

        // 上下文
        .route("/team/{team_id}/context", get(team_list_context))
        .route("/team/{team_id}/context/{id}", get(team_get_context))

        // Agent 状态
        .route("/team/{team_id}/agents", get(team_agent_status))
}

// lib.rs — 通过 Plugin::routes() 贡献给 server
impl Plugin for TeamPlugin {
    fn routes(&self) -> Option<axum::Router> {
        Some(team_api_routes().with_state(self.state()))
    }
}
```

## 十四、UI 集成

### 14.1 插件 → UI 自动加载

Plugin 加载时，`PluginManifest.ui.pages` 声明 UI 页面。Server 通过 `/ui/pages` 端点暴露——前端无需硬编码 Team。

```
Server:  Phase 3 加载 TeamPlugin
           → PluginManifest.ui.pages = ["team"]
           → TeamPlugin.routes() 注册 /team/* 接口
           → /ui/pages 端点返回 [{id: "team", label: "Team"}]

Frontend: App.svelte 挂载时 fetch /ui/pages
           → 动态渲染导航按钮 "Team"
           → 点击后加载 pluginPageComponents["team"] → Team.svelte
```

### 14.2 `/ui/pages` 端点

```rust
// kernel/gateway/src/runtime/http.rs
#[derive(Serialize)]
struct UiPageEntry {
    id: String,
    label: String,
}

async fn ui_plugin_pages(State(runtime): State<Arc<AgentRuntime>>) -> Json<Vec<UiPageEntry>> {
    let mut pages = Vec::new();
    for plugin in runtime.active_plugins() {
        if let Some(ui) = &plugin.manifest().ui {
            for page_id in &ui.pages {
                pages.push(UiPageEntry {
                    id: page_id.clone(),
                    label: match page_id.as_str() {
                        "team" => "Team".into(),
                        other => other.to_string(),
                    },
                });
            }
        }
    }
    Json(pages)
}
```

### 14.3 Team.svelte

纯 Kanban 视图，不包含通信空间：

```
┌──────────────────────────────────────────────────┐
│  Team: Aman Core Team                    [⚙ 配置] │
├──────────────────────────────────────────────────┤
│  ┌────────┬────────┬────────┬────────┐          │
│  │ 待办   │ 处理中  │ 审核   │ 完成   │          │
│  │        │        │        │        │          │
│  │ #42    │ #41    │ #38    │ #36    │          │
│  │ #43    │        │        │ #37    │          │
│  │        │        │        │        │          │
│  ├────────┴────────┴────────┴────────┤          │
│  │  Agent 状态                         │          │
│  │  🟢 coder     (2)  🟢 reviewer (0) │          │
│  │  🟢 tester    (1)                  │          │
│  └────────────────────────────────────┘          │
│  [+ 新建 Work Item]                              │
└──────────────────────────────────────────────────┘
```

## 十五、错误处理

```
L1: TeamStore SQLite 操作失败 → AmanResult::Err → DLQ
L2: Agent 执行超时 → team:work_item.failed → 重回 stage，调度器重新分配
L3: WorkflowEngine 转换失败 → ERROR state → RETRY event
L4: 安全围栏拦截 → team:safety.pending → 等待人类决策 → 超时自动拒绝
L5: EventBus 背压 → InMemoryBus 标准 backpressure → overflow-to-disk
```

Agent 执行失败时：work item 回到待分配状态，调度器可重新分配给同一 Agent 或换人。

## 十六、总结

相比原设计的核心变化：

1. **砍掉通信空间** — 没有 messages 表、没有 @mention、没有 Agent thought/action 回显。交互纯粹通过 Work 系统。
2. **Server 零硬编码** — Team 完全通过 Plugin trait（routes + UI pages + hooks）工作，`build_router()` 中不出现 Team 字样。
3. **插件支持任意语言** — 复用 `ScriptRuntime`，plugin.yaml 中声明 `runtime: python3` 即可用 Python 写插件。
4. **UI 自动加载** — 插件声明 `ui.pages: ["team"]`，`/ui/pages` 端点返回，前端动态渲染。
5. **存储缩减到 2 表** — tasks 和 stage_history 完全交给 WorkflowEngine + StateStore。
6. **事件从 15 个减到 7 个** — 只保留 work item 生命周期 + 安全围栏，砍掉所有通信空间事件。
