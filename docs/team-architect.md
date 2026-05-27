# Team 插件架构文档 (team-architect.md)

## 一、定位与复用

Team 作为 aman 插件实现，复用现有基础设施，不引入新范式：

| 层次 | 复用 aman 机制 | Team 专属扩展 |
|------|---------------|-------------|
| 配置 | `config` crate 4层加载 + `AgentConfig::validate()` | `team.yaml` 解析 + TeamConfig schema |
| 身份 | `AgentRegistry` / `AgentInstance` / SOUL.md | member type (human/agent) + capabilities + autonomy |
| 运行时 | `AgentHarness` (ReAct loop + InterruptFlag) + `WorkSystem` (FIFO 队列消费) | TeamHarness (team 上下文注入) |
| 事件 | `EventBus` + `Custom("team:*")` 事件类型 | 8个 Team 专属事件 |
| 工作流 | `WorkflowEngine` (StateDef, Transition, Guard) | team workflow → WorkflowDef 编译 |
| 存储 | `persistence` (WAL, StateStore) + SQLite | `team.db` (6张表) |
| 通知 | `notification::NotificationStore` | 安全围栏告警 |
| 插件 | `PluginManifest` + `PluginCandidate` | TeamPlugin (InProcess) |
| UI | `App.svelte` menuGroups + pages | Team.svelte + 双视图 (Chat + Kanban) |

---

## 二、事件响应机制（核心）

### 2.1 事件流全景

```
通信空间                    EventBus                         Agent 执行
  │                           │                                │
  │  人类发送 "@coder 修bug"   │                                │
  ├──► team:message.sent ────►│                                │
  │                           ├──► TeamMessageHandler          │
  │                           │    ├─ 解析 @mention            │
  │                           │    ├─ 创建 work item (如需要)  │
  │                           │    └─ 发射 team:agent.invoked  │
  │                           │         │                      │
  │                           │         └──► AgentHarness      │
  │                           │              ├─ 加载 context   │
  │                           │              ├─ ReAct loop     │
  │                           │              │   ├─ thought    │
  │                           │              │   └─ action     │
  │                           │              └─ 发射 team:agent.response
  │                           │                     │          │
  │  收到 agent 回复 ◄────────┤◄────────────────────┘          │
  │                           │                                │
  │  人类: "开始修"            │                                │
  ├──► team:message.sent ────►│                                │
  │                           ├──► 追加约束到 work item        │
  │                           ├──► 看板调度器决定分配           │
  │                           │    ├─ 匹配 Agent 能力          │
  │                           │    └─ WorkItemPushChannel      │
  │                           │         │                      │
  │                           │         └──► Agent WorkSystem  │
  │                           │              ├─ IDLE → BUSY    │
  │                           │              └─ 开始执行步骤   │
  │                           │                                │
  │              ...Agent 执行...                               │
  │                           │                                │
  │                           │◄── Agent 完成, 发射             │
  │                           │    team:work_item.completed    │
  │                           │         │                      │
  │                           │         ├──► WorkflowEngine    │
  │                           │         │    └─ 检查 gate      │
  │                           │         │       ├─ 通过 → 流转 │
  │                           │         │       └─ 拦截 →      │
  │                           │         │          team:safety.gate_triggered
  │                           │         │              │        │
  │  收到安全告警 ◄───────────┤◄────────┴──────────────┘        │
```

### 2.2 Team 专属事件类型

所有 Team 事件使用 `EventType::Custom("team:*")` 命名空间：

```rust
// 注册到 EventType::Custom 的事件
pub mod team_events {
    // ── 通信空间 ──
    pub const MESSAGE_SENT:      &str = "team:message.sent";
    pub const MESSAGE_MENTIONED: &str = "team:message.mentioned";   // 被 @mention 触发

    // ── Agent 交互 ──
    pub const AGENT_INVOKED:     &str = "team:agent.invoked";       // Agent 被触发
    pub const AGENT_THOUGHT:     &str = "team:agent.thought";       // Agent 推理步骤
    pub const AGENT_ACTION:      &str = "team:agent.action";        // Agent 工具调用
    pub const AGENT_RESPONSE:    &str = "team:agent.response";      // Agent 最终回复

    // ── Work Item 状态 ──
    pub const WORK_ITEM_CREATED:   &str = "team:work_item.created";
    pub const WORK_ITEM_ASSIGNED:  &str = "team:work_item.assigned";    // 调度器推送后触发
    pub const WORK_ITEM_STAGE_CHANGED: &str = "team:work_item.stage_changed";
    pub const WORK_ITEM_COMPLETED: &str = "team:work_item.completed";
    pub const WORK_ITEM_FAILED:    &str = "team:work_item.failed";

    // ── 安全围栏 ──
    pub const SAFETY_GATE_TRIGGERED: &str = "team:safety.gate_triggered";
    pub const SAFETY_GATE_RESOLVED:  &str = "team:safety.gate_resolved";
}
```

### 2.3 EventBus 订阅架构

```
                        InMemoryBus
                             │
          ┌──────────────────┼──────────────────┐
          │                  │                  │
   SubscriptionFilter  SubscriptionFilter  SubscriptionFilter
   event_types=[          event_types=[       event_types=[
     "team:message.*"      "team:work_item.*"  "team:safety.*"
   ]                      ]                   ]
          │                  │                  │
   TeamMessageHandler   WorkItemBridge      SafetyGateHandler
   (通信空间处理)         (work item→工作流)    (安全围栏)
          │                  │                  │
          ▼                  ▼                  ▼
   ┌──────────────┐   ┌──────────────┐   ┌──────────────┐
   │ 解析 @mention │   │ 调度器分配   │   │ 检查 pattern │
   │ 创建 work item│   │ WorkflowEngine│  │ 写入安全日志 │
   │ 路由到 Agent  │   │ .transition()│   │ 通知人类     │
   │ 回显消息     │   │ 更新stage_his│   │ 等待决策输入 │
   └──────────────┘   └──────────────┘   └──────────────┘
```

### 2.4 关键事件链路

**链路 A：@mention → Agent 执行 → 回显**

```
人类消息 ──► EventBus.publish(team:message.sent)
                │
                ▼
         TeamMessageHandler.handle()
           ├─ 解析 mentions (正则: /@(\w+)/)
           ├─ 查找 member_id → AgentInstance
           ├─ 若消息包含可执行意图 → 创建 work item (team:work_item.created)
           │
           ├─ EventBus.publish(team:agent.invoked, payload={
           │      agent_id, message, task_id?, thread_context
           │  })
           │      │
           │      ▼
           │  AgentHarness.run_react_loop(agent, context)
           │    ├─ 每步 thought → EventBus.publish(team:agent.thought)
           │    └─ 每步 action  → EventBus.publish(team:agent.action)
           │      │
           │      ▼
           │  EventBus.publish(team:agent.response, payload={
           │      agent_id, content, task_id, confidence
           │  })
           │
           └─ TeamMessageHandler 收到 response →
              写入 messages 表 → UI 更新（thought/action 折叠）
```

**链路 B：调度器推送 → 工作流流转 → 安全围栏**

```
team:work_item.created ──► WorkItemBridge
                             ├─ work item 进入 initial_stage
                             ├─ assignment_policy.auto_assign == true?
                             │   YES → 调度器执行 DispatchStrategy
                             │     ├─ best_match: 匹配 required_capabilities
                             │     ├─ least_loaded: 选队列最短 Agent
                             │     └─ random_idle: 随机空闲 Agent
                             │           │
                             │           ▼
                             │   WorkItemPushChannel::push(agent_id, item)
                             │   → team:work_item.assigned
                             │   → Agent WorkSystem: IDLE → BUSY
                             │
                             ▼
                       Agent 执行完成 ──► team:work_item.completed
                                             │
                                             ▼
                                        WorkflowEngine.transition()
                                          ├─ 检查 allowed_next
                                          ├─ 检查 safety_gates
                                          │   ├─ dangerous_action? → team:safety.gate_triggered
                                          │   └─ confidence < min?  → team:safety.gate_triggered
                                          └─ 通过 → 流转到新 stage
                                          └─ 新 stage 有 auto_assign → 触发下一次调度器推送
```

**链路 C：人类追加约束 → 更新执行上下文**

```
人类: "不要改 disk overflow"
  │
  ▼
team:message.sent ──► TeamMessageHandler
                        ├─ 检测到活跃 work item (当前有 agent 执行)
                        ├─ 追加到 work item.description
                        ├─ EventBus.publish(team:agent.invoked, payload={
                        │      is_constraint_update: true,  ← 标记
                        │      constraint_text: "不要改 disk overflow"
                        │  })
                        │      │
                        │      ▼
                        │  AgentHarness: 中断并重入 ReAct loop
                        │    └─ 新约束注入 system prompt
```

---

## 三、插件架构

### 3.1 插件声明

```yaml
# crates/plugins/team/plugin.yaml (PluginManifest)
name: "team"
version: "0.1.0"
capabilities: ["team", "multi_agent_collaboration"]
isolation: InProcess
exports:
  tools:
    - "team.send_message"
    - "team.assign_task"              # 调度器：分配 work item 给 Agent
    - "team.complete_task"
    - "team.create_task"
    - "team.list_tasks"
  hooks:
    - "team.on_message"
    - "team.on_work_item_assigned"    # 调度器推送 work item 后触发
    - "team.on_safety_gate"
  event_sources:
    - "team_source"             # team.yaml 的 FileWatch
ui:
  pages: ["team"]               # 注册到左侧导航
  events:                       # 前端事件
    - "team:message.new"
    - "team:work_item.updated"
    - "team:safety.alert"
```

### 3.2 Crate 结构

```
crates/plugins/team/
├── plugin.yaml
├── Cargo.toml
└── src/
    ├── lib.rs                  # TeamPlugin: Plugin trait impl
    ├── config.rs               # TeamConfig 解析 + 校验
    ├── team_store.rs           # SQLite 操作 (6 表)
    ├── message_handler.rs      # TeamMessageHandler (EventBus subscriber)
    ├── work_item_bridge.rs     # WorkItemBridge (work item→调度器→WorkflowEngine)
    ├── safety_gate.rs          # SafetyGateHandler (安全围栏)
    ├── scheduler.rs            # TeamScheduler: 技能匹配 + DispatchStrategy + push
    ├── context_loader.rs       # context_files 加载 + 缓存
    ├── workflow_compiler.rs    # team stages → WorkflowDef
    └── api.rs                  # HTTP API 端点 (/team/*)
```

### 3.3 TeamPlugin 实现

```rust
// lib.rs
use kernel::plugin::Plugin;
use kernel::hook::Hook;

pub struct TeamPlugin {
    config: TeamConfig,
    store: TeamStore,
    event_bus: Arc<dyn EventBus>,
    agent_registry: Arc<AgentRegistry>,
    workflow_engine: Arc<WorkflowEngine>,
}

impl Plugin for TeamPlugin {
    fn name(&self) -> &str { "team" }
    fn version(&self) -> Version { Version::new(0, 1, 0) }

    fn hooks(&self) -> Vec<Box<dyn Hook>> {
        vec![
            Box::new(TeamMessageHook::new(self.store.clone(), self.event_bus.clone())),
            Box::new(TeamWorkItemHook::new(self.workflow_engine.clone())),
            Box::new(TeamSafetyHook::new(self.store.clone(), self.event_bus.clone())),
        ]
    }

    fn tools(&self) -> Vec<Box<dyn Tool>> {
        vec![
            Box::new(TeamSendMessageTool),
            Box::new(TeamAssignTaskTool),
            Box::new(TeamCompleteTaskTool),
            Box::new(TeamCreateTaskTool),
        ]
    }
}
```

### 3.4 运行时初始化

AgentRuntime 在 Phase 3（插件加载）阶段加载 TeamPlugin：

```
Phase 0: 配置加载
Phase 1: EventBus 创建
Phase 2: AgentRegistry 填充 (从 config.yaml agents 段)
Phase 3: 插件加载
    │
    ├── InfoHubPlugin::init()
    ├── TeamPlugin::init()
    │     ├─ 解析 team.yaml → TeamConfig
    │     ├─ 打开/创建 team.db → TeamStore
    │     ├─ 编译 stages → WorkflowDef → 注册到 WorkflowEngine
    │     ├─ 初始化 TeamScheduler + WorkItemPushChannel 集成
    │     ├─ 注册 EventBus 订阅 (3 个 SubscriptionFilter)
    │     └─ 注册 HTTP 路由 (/team/*) — 通过 Plugin::routes()
    │
Phase 4: Source 启动 (FileWatch 监听 team.yaml 热更新)
Phase 5: Runtime Ready
```

> **前置依赖**：Team 以 Plugin 形式实现需先完成 Plugin 系统的两项升级（`Plugin::routes()` 方法 + UI 动态导航）。详见 [chat-refactor.md §二](./chat-refactor.md#二plugin-系统升级两个缺口)。

---

## 四、数据存储

### 4.1 team.db 表结构

沿用 persistence crate 的 SQLite 模式，通过 `TeamStore` 封装：

```
team.db
├── tasks           -- work item 主表
├── stage_history   -- 阶段流转历史
├── messages        -- 通信空间消息 (含线程)
├── chat_to_work    -- 对话 → work item 映射
├── safety_log      -- 安全围栏决策记录
└── context         -- 共享文档缓存
```

### 4.2 与 persistence crate 的集成

- **WAL + 事务**：所有写操作通过 `WriteAheadLog` 保证持久性
- **StateStore**：tasks 和 stage_history 关联到 `StateStore`，供 WorkflowEngine 查询当前状态
- **DLQ**：事件处理失败的消息进入 DeadLetterQueue

---

## 五、与 WorkflowEngine 的集成

### 5.1 team stages → WorkflowDef 编译

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
                event: format!("team:stage.{}→{}", stage.id, next_id),
                to: TransitionTo::State(next_id.clone()),
                guard: Some(build_safety_guard(config, &stage.id, next_id)),
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
        ..
    }
}
```

### 5.2 Guard 函数：安全围栏检查

```rust
fn build_safety_guard(config: &TeamConfig, from: &str, to: &str) -> Option<Box<dyn Fn(&Value) -> bool>> {
    // 检查目标 stage 是否在 agent 的 allowed_stages 内
    // 检查 confidence 是否 >= min_confidence
    // 检查是否有待处理的安全围栏决策
    Some(Box::new(move |ctx: &Value| -> bool {
        let agent_id = ctx["agent_id"].as_str().unwrap_or("");
        let confidence = ctx["confidence"].as_f64().unwrap_or(1.0);

        if let Some(member) = config.find_member(agent_id) {
            if member.member_type == "agent" {
                if !member.allowed_stages.contains(&to.to_string()) {
                    return false;  // Agent 无权进入此 stage
                }
            }
        }

        confidence >= config.safety_gates.min_confidence
    }))
}
```

---

## 六、调度器推送与分配策略

### 6.1 TeamScheduler

调度器是 Team 插件内的组件，负责在 work item 进入可分配 stage 时，根据策略选择目标 Agent 并通过 `WorkItemPushChannel` 推送。

```rust
// scheduler.rs
pub struct TeamScheduler {
    config: TeamConfig,
    store: TeamStore,
    push_channel: Arc<dyn WorkItemPushChannel>,
}

impl TeamScheduler {
    /// 为进入某个 stage 的 work item 分配 Agent 并推送
    pub async fn dispatch(
        &self,
        item: &WorkItem,
        stage_id: &str,
    ) -> AmanResult<AgentId> {
        let stage = self.config.find_stage(stage_id)?;
        let policy = stage.assignment_policy.as_ref()
            .ok_or(WorkError::NoAssignmentPolicy)?;

        // 按 dispatch_strategy 选择目标 Agent
        let candidates = self.config.members.iter()
            .filter(|m| m.member_type == "agent")
            .filter(|m| m.autonomy != "on_mention")  // on_mention 不参与自动分配
            .filter(|m| {
                // 能力匹配：Agent 的 capabilities 与 required_capabilities 有交集
                m.capabilities.iter().any(|cap| policy.required_capabilities.contains(cap))
            })
            .filter(|m| {
                // 队列未满
                let queue_len = self.push_channel.queue_length(&m.id);
                queue_len < m.queue_max_size
            })
            .collect::<Vec<_>>();

        let target = match policy.dispatch_strategy {
            DispatchStrategy::BestMatch => {
                // 能力交集最大的 Agent
                candidates.into_iter()
                    .max_by_key(|m| {
                        m.capabilities.iter()
                            .filter(|c| policy.required_capabilities.contains(c))
                            .count()
                    })
            }
            DispatchStrategy::LeastLoaded => {
                // 当前队列最短的 Agent
                candidates.into_iter()
                    .min_by_key(|m| self.push_channel.queue_length(&m.id))
            }
            DispatchStrategy::RandomIdle => {
                // 随机空闲 Agent
                candidates.into_iter()
                    .filter(|m| self.push_channel.queue_length(&m.id) == 0)
                    .choose(&mut rand::thread_rng())
            }
        }.ok_or(WorkError::NoEligibleAgent)?;

        // 构造来源信息
        let source = WorkItemSource::Kanban {
            board_id: self.config.team.name.clone(),
            scheduler: "team".into(),
        };

        // 推送 WorkItem 到目标 Agent
        let item = WorkItem {
            id: item.id.clone(),
            title: item.title.clone(),
            description: item.description.clone(),
            steps: None,           // 由 Agent 的 Work System 自行分解
            priority: item.priority,
            timeout: Some(Duration::from_secs(policy.execution_timeout_minutes * 60)),
            context: self.build_context(item),
            notify_on_complete: true,
            created_at: now(),
        };

        self.push_channel.push(&target.id, item, source).await?;

        // 写入 stage_history
        self.store.assign_item(&item.id, stage_id, &target.id)?;

        Ok(target.id)
    }
}
```

### 6.2 WorkItemPushChannel 集成

Team 调度器通过 aman 的 `WorkItemPushChannel` trait 推送工作项到 Agent 的 Work System：

```rust
/// 向 Agent 推送 work item（aman 内核接口，非 Team 专属）
#[async_trait]
pub trait WorkItemPushChannel {
    async fn push(
        &self,
        agent_id: &AgentId,
        item: WorkItem,
        source: WorkItemSource,
    ) -> Result<()>;

    fn queue_length(&self, agent_id: &AgentId) -> usize;
}
```

推送后 Agent 侧流程：

```
WorkItemPushChannel::push(agent_id, item)
  → AgentRuntime 路由到目标 Agent 的 Local EventBus
  → EventBus.publish(WorkItemAssigned { item, source })
  → WorkSystem.handle() → IDLE → BUSY
  → 分解步骤 → 链式执行
  → WorkItemCompleted → Global Bus 通知 Team 插件
```

### 6.3 分配触发时机

| 触发条件 | 说明 |
|---------|------|
| Work item 进入 `auto_assign = true` 的 stage | 调度器自动执行 dispatch |
| 人类手动指派 | 通过 `/assign @agent` 命令或 API |
| Agent Idle Boredom → SeekTask | Idle System 发射 SeekTaskRequest，调度器响应 |
| 上一个 Agent 执行超时/失败 | work item 回到 stage，调度器重新分配 |

---

## 七、HTTP API 端点

Team 的路由通过 `Plugin::routes()` 方法贡献，AgentRuntime 在插件初始化后自动将其 merge 进 `/api/v1` 前缀下：

```rust
// crates/plugins/team/src/api.rs
pub fn team_api_routes() -> Router {
    Router::new()
        // 通信空间
        .route("/team/{team_id}/messages", get(team_messages))
        .route("/team/{team_id}/messages/send", post(team_send_message))
        .route("/team/{team_id}/messages/{id}/promote", post(team_promote_message))

        // Work Item
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
```

// crates/plugins/team/src/lib.rs
impl Plugin for TeamPlugin {
    fn routes(&self) -> Option<axum::Router> {
        Some(team_api_routes().with_state(self.state()))
    }
}
```

AgentRuntime 在 Phase 3 插件加载后自动注册：

```rust
// crates/gateway/src/runtime/agent_runtime.rs
for plugin in &self.active_plugins {
    if let Some(plugin_router) = plugin.routes() {
        app_router = app_router.nest("/api/v1", plugin_router);
    }
}
```

最终暴露的端点：

```
GET    /api/v1/team/{team_id}/messages
POST   /api/v1/team/{team_id}/messages/send
POST   /api/v1/team/{team_id}/messages/{id}/promote
GET    /api/v1/team/{team_id}/tasks
POST   /api/v1/team/{team_id}/tasks/create
GET    /api/v1/team/{team_id}/tasks/{id}
POST   /api/v1/team/{team_id}/tasks/{id}/assign
POST   /api/v1/team/{team_id}/tasks/{id}/complete
GET    /api/v1/team/{team_id}/safety/pending
POST   /api/v1/team/{team_id}/safety/{id}/resolve
GET    /api/v1/team/{team_id}/context
GET    /api/v1/team/{team_id}/context/{id}
GET    /api/v1/team/{team_id}/agents
```

这些端点不需要在 `build_router()` 中手动添加——插件被加载后自动出现，插件被禁用后自动消失。

---

## 八、UI 集成

### 8.1 导航注册（插件驱动）

Team 不硬编码到 App.svelte。通过 `UiDeclaration.pages` 字段声明 `["team"]`，后端通过 `/ui/pages` 端点暴露，App.svelte 动态消费：

**后端：UI 页面查询端点**

```rust
// crates/gateway/src/runtime/http.rs
.route("/ui/pages", get(ui_plugin_pages))

// handler
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

**前端：App.svelte 动态导航**

```svelte
<script lang="ts">
  let pluginPages = $state<{id: string, label: string}[]>([]);

  onMount(async () => {
    try {
      pluginPages = await invoke<{id: string, label: string}[]>("get_ui_plugin_pages");
    } catch { /* gateway 未运行 */ }
  });
</script>

<!-- 导航栏中，Workspace 组下方动态追加插件页面 -->
{#each pluginPages as pg}
  <button class="nav-btn" class:active={currentPage === pg.id}
    onclick={() => navigateTo(pg.id)}>
    <span class="status-dot running"></span>
    {pg.label}
  </button>
{/each}
```

**前端：页面组件分发**

插件页面组件通过预注册映射表加载（后续可升级为动态 import）：

```svelte
<!-- App.svelte <main> 区域 -->
{#if currentPage === "home"}
  <Home ... />
{:else if currentPage === "chat"}
  <Chat ... />
<!-- 内置页面如上，插件页面通过映射表分发 -->
{:else if pluginPageComponents[currentPage]}
  <svelte:component this={pluginPageComponents[currentPage]} />
{/if}
```

```typescript
// src/pages/plugin-pages.ts — 映射表
import Team from "./plugins/Team.svelte";
// 未来: import Chat from "./plugins/Chat.svelte";

export const pluginPageComponents: Record<string, any> = {
  "team": Team,
  // 插件安装时在此注册，或通过构建时代码生成
};
```

**过渡方案**：在升级 B（UI 动态导航）完成前，可以暂时在 App.svelte 硬编码一行 `{ id: "team", label: "Team" }`。这不是最终形态，但确保 Team 功能可以先上线。

### 8.2 Team.svelte 页面结构

Team 页面采用**左右双栏**布局：

```
┌──────────────────────────────────────────────────┐
│  Team: Aman Core Team                    [⚙ 配置] │
├────────────────────┬─────────────────────────────┤
│   通信空间 (60%)    │     看板 (40%)               │
│                    │                             │
│  ┌──────────────┐  │  ┌──────┬──────┬──────┐    │
│  │ Jerin:       │  │  │待办  │处理中│ 审核  │    │
│  │ @coder 修bug │  │  │      │      │      │    │
│  │              │  │  │ #42  │ #41  │ #38  │    │
│  │ ┌ Coder ───┐ │  │  │ #43  │      │      │    │
│  │ │ [分析中]  │ │  │  │      │      │      │    │
│  │ │ 发现3处.. │ │  │  ├──────┼──────┼──────┤    │
│  │ └──────────┘ │  │  │ 测试 │ 完成  │      │    │
│  │              │  │  │      │      │      │    │
│  │ ┌ Coder ───┐ │  │  │ #39  │ #36  │      │    │
│  │ │ 建议方案: │ │  │  │      │ #37  │      │    │
│  │ │ 1. 降低.. │ │  │  │      │      │      │    │
│  │ │ 要修吗?   │ │  │  └──────┴──────┴──────┘    │
│  │ └──────────┘ │  │                             │
│  │              │  │  Agent 状态                  │
│  │ Jerin:       │  │  🟢 coder     (2 work items)     │
│  │ 开始修       │  │  🟢 reviewer  (idle)        │
│  │              │  │  🟢 tester    (1 work item)      │
│  └──────────────┘  │                             │
│  ┌──────────────┐  │                             │
│  │ 输入消息...   │  │                             │
│  └──────────────┘  │                             │
└────────────────────┴─────────────────────────────┘
```

### 8.3 前端事件流

```
Team.svelte
  │
  ├── onMount → 订阅 team:message.new (SSE/WebSocket)
  │              订阅 team:work_item.updated
  │              订阅 team:safety.alert
  │
  ├── 人类输入 → invoke("team_send_message", {team_id, content})
  │               │
  │               ▼
  │          后端: EventBus.publish("team:message.sent")
  │               → TeamMessageHandler 处理
  │               → 消息落库 messages 表
  │               → 如含 @mention → team:agent.invoked
  │
  ├── Agent thought → 前端收到 event {type: "thought", collapsed: true}
  │   Agent action  → 前端收到 event {type: "action", collapsed: true}
  │   Agent final   → 前端收到 event {type: "response", content: "..."}
  │
  ├── 看板视图 ← invoke("team_list_tasks") → 轮询或事件更新
  │
  └── 安全告警 → 弹出确认对话框 → invoke("team_resolve_gate", {id, decision})
```

---

## 九、安全围栏实现

### 9.1 拦截点

```rust
// safety_gate.rs
pub struct SafetyGateHandler {
    config: TeamConfig,
    store: TeamStore,
    bus: Arc<dyn EventBus>,
}

impl SafetyGateHandler {
    /// 在 Agent 执行 action 之前检查
    pub fn check_action(&self, action: &str, agent_id: &str, task_id: i64) -> SafetyResult {
        // 1. 危险操作 pattern 匹配
        for pattern in &self.config.safety_gates.dangerous_actions {
            if pattern.matches(action) {
                return SafetyResult::Blocked {
                    reason: format!("危险操作: {action}"),
                    requires_human: true,
                };
            }
        }

        // 2. 权限检查（Agent 是否在 allowed_stages 内操作）
        let task = self.store.get_task(task_id)?;
        let member = self.config.find_member(agent_id)?;
        if !member.allowed_stages.contains(&task.current_stage) {
            return SafetyResult::Blocked {
                reason: "无权操作此 stage".into(),
                requires_human: true,
            };
        }

        SafetyResult::Allowed
    }

    /// Agent 完成 stage 时检查置信度
    pub fn check_confidence(&self, confidence: f64, task_id: i64) -> SafetyResult {
        if confidence < self.config.safety_gates.min_confidence {
            // 写入 safety_log，发射事件，等待人类决策
            let log_id = self.store.insert_safety_log(task_id, "low_confidence", confidence);
            let _ = self.bus.publish(Event::new(
                EventType::Custom("team:safety.gate_triggered".into()),
                json!({"task_id": task_id, "log_id": log_id, "reason": "low_confidence"}),
            ));
            return SafetyResult::PendingHumanDecision(log_id);
        }
        SafetyResult::Allowed
    }
}
```

---

## 十、多 Team 支持

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

每个 Team 在 Phase 3 加载时创建独立的：
- `TeamConfig` 实例
- `TeamStore` (独立 SQLite 连接)
- `WorkflowDef` (独立工作流定义)
- EventBus 订阅（按 team_id 过滤）

---

## 十一、错误处理与恢复

```
错误层级:
  L1: SQLite 操作失败 → AmanResult::Err → DLQ
  L2: Agent 执行超时 → team:work_item.failed → 重新入队或标记失败
  L3: WorkflowEngine 转换失败 → ERROR state → RETRY event
  L4: 安全围栏拦截 → team:safety.pending → 等待人类 → 超时自动拒绝
  L5: EventBus 背压 → InMemoryBus 标准 backpressure → overflow-to-disk
```

Agent 执行失败时：
```
Agent 异常 → 写入 activity_log (status=failed)
          → work item 回到上一个 stage (或用 stage_history 回滚)
          → 通知通信空间: "@coder 执行 #42 失败: [原因]。等待人类决策"
```

---

## 十二、总结

Team 架构的核心设计决策：

1. **Plugin 优先，不内置。** Team 作为 `crates/plugins/team/` 下的 InProcess 插件存在，依赖两个轻量级 Plugin trait 扩展（`routes()` 方法 + UI pages 端点），不修改内核。插件被禁用时零成本。

2. **复用而非重造。** EventBus、WorkflowEngine、AgentRegistry、AgentHarness、WorkSystem（含 Hook 机制）、persistence、notification 全部复用，Team 只写适配层 + 业务逻辑。

3. **事件驱动一切。** 从消息发送到 Agent 执行到安全拦截，全链路通过 EventBus 的 `Custom("team:*")` 事件解耦，三个 SubscriptionFilter 各自负责消息处理、work item 流转、安全围栏。

4. **通信空间是第一界面。** 人类和 Agent 的消息对称处理——人类发消息是 `team:message.sent`，Agent 回复是 `team:agent.response`，Agent 的 thought/action 以折叠形式回显但可展开。

5. **安全围栏是最小拦截。** 不是审批流，只在危险操作 pattern 匹配、低置信度、权限越界时介入。拦截后通过 `safety_log` 持久化，等待人类决策。

6. **Agent 24h 在线。** 不需要 Cycle/Sprint 时间盒，Agent 超时用分钟级，work item 流转即时发生——上一个 Agent 完成立刻进入下一 stage，调度器立刻分配给下一个 Agent。
