# Team 插件业务逻辑设计

## 一、概述

Team 是一个 **人类 + Agent 混编团队的协作空间**。工作不从表单创建，而是从对话中浮现 — 人类和 Agent 在通信空间里 @mention、讨论、决策，对话自动产出可追踪的 work item。看板调度器将 work item 推送给对应 Agent，Agent 24 小时在线被动接收并自主执行。

核心理念：
- **通信优先**：工作始于对话，@mention 就是派活
- **被动推送**：看板调度器决定谁做什么，Agent 不认领、不竞争，只负责消费队列执行
- **Agent 自主**：人类定义目标与约束，Agent 自主执行、流转
- **安全围栏**：不放任，关键操作需人类确认 — 但这不是审批流程
- **极简内核**：YAML 配置 + SQLite 数据，单进程自托管

## 二、核心概念

### Team
顶层容器。一个 Team = 一组成员 + 一个通信空间 + 一组工作流配置。一个人可以管理多个 Team。

### 成员 (Member)
Team 中的参与者，分两类：

- **Human**：人类用户。Owner 是唯一的，负责定义 Team 配置和安全围栏。
- **Agent**：AI 代理。有自己的能力标签、自主级别、调度器可分配的 stage 范围。

每个 Agent 是 aman 的一个 agent profile，携带完整的 SOUL.md 人格定义。

### 通信空间 (Chat)
Team 的核心界面。类似聊天室，所有成员（人类和 Agent）都可以发言。关键机制：
- **@mention Agent** → 触发该 Agent 响应
- **对话线程** → 可被提升为 work item
- **Agent 执行过程** → 以 thought/action 形式回显到通信空间（可见但折叠）
- **人类随时插话** → 改变方向、追加约束、叫停

### 工作流 (Workflow)
work item 状态流转的有向图。沿袭原 kanban 的 stage 设计：
- Stage 数量、名称、流转关系完全由 YAML 定义
- 看板调度器根据 Agent 能力标签和自主级别，将 work item 推送到对应 Agent 的 Work 队列
- Agent 只负责消费队列、执行步骤、完成后触发流转
- 无需 sprint/cycle — Agent 24 小时在线，上一个完成立刻流转

### 安全围栏 (Safety Gate)
不是审批流程。是对特定操作的硬约束：
- **危险操作**：文件删除、外部写入、生产发布 → 需人类确认
- **低置信度**：Agent 自评置信度低于阈值 → 升级给人类
- **超出权限**：Agent 无权操作的 stage → 人类接管

### 上下文 (Context)
Team 级别的共享知识：架构文档、编码规范、决策记录。Agent 收到被分配的 work item 时自动加载相关上下文。

---

## 三、配置设计（YAML）

### 3.1 完整配置示例

```yaml
# Team 基本信息
team:
  name: "Aman Core Team"
  description: "Aman agent framework development"

# 成员定义
members:
  # --- 人类 ---
  - id: "jerin"
    type: human
    name: "Jerin"
    roles: [owner]                   # owner | admin
    timezone: "Asia/Shanghai"

  # --- Agent ---
  - id: "coder"
    type: agent
    name: "Coder"
    profile: "coder"                 # 对应 aman agent profile
    capabilities: [code, refactor, fix]
    autonomy: autonomous             # autonomous | supervised | on_mention
    allowed_stages: ["wip", "review_fix"]
    queue_max_size: 5                # Work 队列最大长度，超过后调度器停止推送
    context_hint: "You are a senior Rust developer. Follow docs/coding-standards.md"

  - id: "reviewer"
    type: agent
    name: "Reviewer"
    profile: "code-auditor"
    capabilities: [review, audit, security_check]
    autonomy: autonomous
    allowed_stages: ["review"]
    queue_max_size: 3

  - id: "tester"
    type: agent
    name: "Tester"
    profile: "tester"
    capabilities: [test, integration_test, e2e]
    autonomy: autonomous
    allowed_stages: ["testing"]
    queue_max_size: 3

# 工作流阶段定义
stages:
  - id: "backlog"
    name: "待办"
    order: 1
    allowed_next: ["wip"]
    description: "人类 triage 后的 work item 池"

  - id: "wip"
    name: "处理中"
    order: 2
    allowed_next: ["review", "backlog"]
    assignment_policy:
      auto_assign: true              # work item 进入此 stage 时调度器自动分配
      required_capabilities: [code, refactor, fix]
      execution_timeout_minutes: 120 # 单个 work item 执行超时
      dispatch_strategy: "best_match" # best_match | least_loaded | random_idle

  - id: "review"
    name: "审核"
    order: 3
    allowed_next: ["wip", "testing"]
    assignment_policy:
      auto_assign: true
      required_capabilities: [review, audit]
      execution_timeout_minutes: 60
      dispatch_strategy: "least_loaded"

  - id: "testing"
    name: "测试"
    order: 4
    allowed_next: ["wip", "done"]
    assignment_policy:
      auto_assign: true
      required_capabilities: [test, integration_test]
      execution_timeout_minutes: 90
      dispatch_strategy: "best_match"

  - id: "done"
    name: "完成"
    order: 5
    allowed_next: []

# 安全围栏
safety_gates:
  # 危险操作：需人类确认
  dangerous_actions:
    - pattern: "rm -rf"
      require_human: true
    - pattern: "git push --force"
      require_human: true
    - pattern: "publish|deploy|release"
      require_human: true
    - pattern: "DROP |DELETE FROM|TRUNCATE"
      require_human: true

  # Agent 自评置信度低于此值时升级给人类
  min_confidence: 0.7

  # 人类不在线时 Agent 的最大自主操作次数
  max_autonomous_actions_without_human: 20

# 初始阶段
initial_stage: "backlog"

# 上下文文件（Agent 收到 work item 时自动加载）
context_files:
  - "docs/architecture.md"
  - "docs/coding-standards.md"
  - "docs/decision-log.md"

# 项目工作目录
work_dir: "/Users/jerin/projects/aman"
```

### 3.2 设计原则

- **Stage ID 不可变**：使用系统生成的 shortUUID（如 `X7fK9p`），无业务含义，永不修改
- **显示名称可随意修改**：`name` 字段纯粹用于展示
- **成员 ID 不可变**：Agent 的历史执行记录依赖稳定的成员 ID
- **安全围栏白名单**：默认拒绝危险操作，只放行明确允许的

---

## 四、通信空间设计

### 4.1 核心交互模式

```
通信空间 (Chat)
│
├── 人类: "@coder 看一下 src/event-bus 的 backpressure 实现，OOM 风险"
│       │
│       ├── 自动创建 work item (title 从消息提取)
│       ├── Agent 响应: [thought] 分析代码结构...
│       ├── Agent 响应: [action] 读取 src/event-bus/...
│       └── Agent 响应: "发现两处问题：1. overflow 阈值过高 2. 缺少降级策略。
│                        建议方案...[详细分析]。要我开始修吗？"
│
├── 人类: "开始修，但不要改 disk overflow 那块"
│       │
│       └── Agent 追加约束，开始执行 → 自动移到 wip stage
│
├── 人类: "/promote 这条对话 → work item"     ← 显式提升
│       或: 系统自动检测到 actionable 内容 → 自动提升
│
└── Agent 执行完成 → 自动通知通信空间 → 流转到 review
```

### 4.2 对话 → Work Item 转换

两种触发方式：

1. **显式提升**：人类用 `/promote` 命令将某条消息或线程转为 work item
2. **自动识别**：Agent 检测到人类消息中包含明确的「动作意图」（修复/实现/调查），自动创建 work item

转换时自动提取：
- 标题：从消息首句或 @mention 后的描述提取
- 描述：完整消息 + 上下文对话
- 来源引用：链接回原始对话

### 4.3 Agent 在通信空间的行为

Agent 的消息分三层可见性：

| 类型 | 可见性 | 示例 |
|------|--------|------|
| response | **完全可见** | 最终回复、方案建议、提问 |
| thought | 折叠（可展开） | "正在分析 backpressure.rs 的阈值逻辑..." |
| action | 折叠（可展开） | "已读取 src/event-bus/backpressure.rs:45-120" |

人类看到的是干净的对话，噪音自动折叠。需要时可以展开查看 Agent 的思考过程。

---

## 五、数据设计（SQLite）

每个 Team 对应一个 `{team}.db` 文件。

### 5.1 Work Item 表（tasks）

| 字段          | 类型      | 说明                         |
| ------------- | --------- | ---------------------------- |
| id            | INTEGER   | 自增主键                     |
| title         | TEXT      | Work item 标题               |
| description   | TEXT      | Work item 描述               |
| source_type   | TEXT      | 来源: 'chat' | 'manual'      |
| source_ref    | TEXT      | 来源引用 (chat message ID 等) |
| created_at    | DATETIME  | 创建时间                     |
| creator       | TEXT      | 创建者 member_id             |
| creator_type  | TEXT      | 'human' | 'agent'             |
| current_stage | TEXT      | 当前 stage ID                |
| priority      | INTEGER   | 优先级 0-3 (可选)            |
| tags          | TEXT      | JSON 数组标签 (可选)         |
| deleted       | BOOLEAN   | 软删除标记                   |

### 5.2 阶段历史表（stage_history）

| 字段          | 类型      | 说明                               |
| ------------- | --------- | ---------------------------------- |
| id            | INTEGER   | 自增主键                           |
| task_id       | INTEGER   | 关联 tasks.id                      |
| stage         | TEXT      | stage ID                           |
| entered_at    | DATETIME  | 进入该阶段的时间                   |
| assignee      | TEXT      | 执行者 member_id（未分配时为 NULL）|
| assignee_type | TEXT      | 'human' | 'agent'                  |
| assigned_at   | DATETIME  | 调度器分配时间（未分配时为 NULL）  |
| completed_at  | DATETIME  | 完成时间（未完成时为 NULL）        |
| confidence    | REAL      | Agent 自评置信度 (0.0-1.0)，人类为 NULL |

**约束与规则**：
- work item 当前阶段的记录是 `completed_at IS NULL` 的行
- 调度器分配时写入 `assignee`、`assignee_type`、`assigned_at`
- 调度器分配前检查 Agent 的 `allowed_stages` 和 `queue_max_size`
- Agent 完成时填写 `confidence` 字段，低于 `safety_gates.min_confidence` 的 work item 暂停流转

### 5.3 通信空间消息表（messages）

| 字段          | 类型      | 说明                               |
| ------------- | --------- | ---------------------------------- |
| id            | INTEGER   | 自增主键                           |
| sender        | TEXT      | 发送者 member_id                   |
| sender_type   | TEXT      | 'human' | 'agent'                  |
| content       | TEXT      | 消息文本                           |
| msg_type      | TEXT      | 'response' | 'thought' | 'action' | 'system' |
| parent_id     | INTEGER   | 父消息 ID（线程回复）              |
| thread_id     | INTEGER   | 线程根消息 ID                      |
| created_at    | DATETIME  | 发送时间                           |
| metadata      | TEXT      | JSON: {mentions: [], tool_calls: [], confidence: 0.9} |

### 5.4 对话-WorkItem 映射表（chat_to_work）

| 字段          | 类型      | 说明                               |
| ------------- | --------- | ---------------------------------- |
| id            | INTEGER   | 自增主键                           |
| message_id    | INTEGER   | 触发消息 ID                        |
| thread_id     | INTEGER   | 触发线程 ID                        |
| task_id       | INTEGER   | 生成的 work item ID                |
| created_at    | DATETIME  | 映射创建时间                       |

### 5.5 安全围栏日志表（safety_log）

| 字段          | 类型      | 说明                               |
| ------------- | --------- | ---------------------------------- |
| id            | INTEGER   | 自增主键                           |
| task_id       | INTEGER   | 关联work item                           |
| agent_id      | TEXT      | 触发 agent member_id               |
| action        | TEXT      | 被拦截的操作                       |
| reason        | TEXT      | 拦截原因: 'dangerous_action' | 'low_confidence' | 'permission_denied' |
| human_decision| TEXT      | 'approved' | 'denied' | 'modified' (NULL 表示待处理) |
| decided_by    | TEXT      | 决策人 member_id                   |
| created_at    | DATETIME  | 触发时间                           |
| resolved_at   | DATETIME  | 决策时间                           |

### 5.6 上下文表（context）

| 字段          | 类型      | 说明                               |
| ------------- | --------- | ---------------------------------- |
| id            | INTEGER   | 自增主键                           |
| title         | TEXT      | 文档标题                           |
| file_path     | TEXT      | 文件路径（相对于 work_dir）        |
| content       | TEXT      | 缓存的内容快照                     |
| category      | TEXT      | 'architecture' | 'standard' | 'decision' | 'general' |
| updated_at    | DATETIME  | 文件最后修改时间                   |
| indexed_at    | DATETIME  | 内容索引时间                       |

---

## 六、业务行为与规则

### 6.1 通信空间生命周期

1. **发消息**  
   - 人类或 Agent 发送消息到通信空间  
   - 系统解析 @mention，触发对应 Agent  
   - 被 @mention 的 Agent 创建 AgentRun（推理+执行），过程回显到通信空间

2. **对话提升为 Work Item**  
   - 触发：人类 `/promote` 命令 或 Agent 检测到可执行意图  
   - 系统提取标题、描述、上下文，创建 work item（stage = initial_stage）  
   - 在 `chat_to_work` 表记录映射关系  
   - Agent 在通信空间确认："已创建 #42 '修复 event-bus backpressure OOM 风险' → 待办"

3. **约束追加**  
   - 人类在工作执行过程中发消息追加约束  
   - 约束自动追加到对应 work item 的 description  
   - Agent 读取更新后的上下文继续执行

### 6.2 调度器分配

v2 采用被动推送模型：看板调度器决定谁做什么，Agent 不主动认领、不参与竞争。

1. **触发分配**  
   - Work item 进入 `assignment_policy.auto_assign = true` 的 stage  
   - 调度器根据 `dispatch_strategy` 选择目标 Agent：
     - `best_match`：匹配 `required_capabilities` 与 Agent 的 `capabilities`，取交集最大的
     - `least_loaded`：当前队列最短的 Agent
     - `random_idle`：随机空闲 Agent
   - 目标 Agent 的队列长度 < `queue_max_size`  
   - 目标 Agent 的 `autonomy` 为 `autonomous` 或 `supervised`

2. **推送动作**  
   - 调度器调用 `WorkItemPushChannel::push(agent_id, item, source)`  
   - 写入 `stage_history` 的 `assignee`、`assignee_type`、`assigned_at`  
   - Agent 的 Work System 收到 `WorkItemAssigned` 事件 → IDLE → BUSY → 开始执行  
   - Agent 在通信空间通知："已收到 #42 '修复 event-bus backpressure OOM 风险'，开始处理"

3. **执行超时**  
   - Work item 携带 `execution_timeout_minutes`，超过后触发 `WorkItemFailed`  
   - 根据 `retryable` 决定是否重新入队或标记失败  
   - work item 回到待分配状态，调度器可重新分配  
   - 原 Agent 在通信空间通知："#42 执行超时，已释放"

4. **并发控制**  
   - 调度器在推送前检查 Agent 的 `queue_max_size`，队列满时不推送  
   - Agent 端 Work System 只负责 FIFO 消费，不感知并发

### 6.3 完成与流转

1. **Agent 完成**  
   - Work System 执行完所有步骤 → 发射 `WorkItemCompleted` 事件  
   - 更新 `stage_history.completed_at`，填写 `confidence`  
   - 若 `confidence < safety_gates.min_confidence`：work item 暂停流转，通知人类决策  
   - 否则：WorkflowEngine 自动流转到 `allowed_next` 中的下一阶段  
   - 若 `allowed_next` 有多个目标，WorkflowEngine 根据 stage 配置的默认转换路径自动选择

2. **人类完成**  
   - 同原 kanban 逻辑：指定目标 stage，必须符合 `allowed_next`  
   - `confidence` 填 NULL

3. **终态**  
   - `allowed_next = []` 的 stage 为终态，work item 不可再流转

### 6.4 安全围栏触发

1. **危险操作拦截**  
   - Agent 在执行过程中 plan 到匹配 `dangerous_actions` pattern 的操作  
   - 操作被挂起，记录到 `safety_log`  
   - Agent 在通信空间通知："#42 需要执行 `git push --force`，等待确认"  
   - 人类回复 "approve" / "deny" / "修改为..."

2. **低置信度升级**  
   - Agent 完成 stage 时 `confidence < safety_gates.min_confidence`  
   - work item不自动流转，通信空间通知人类审查  

3. **权限不足**  
   - Agent 尝试操作不在 `allowed_stages` 中的 stage  
   - 操作被拒绝，work item标记需要人类接管

### 6.5 上下文加载

- Agent 收到 work item 时，自动从 `context_files` 加载相关文档  
- `context` 表缓存文件内容，文件变更时自动更新  
- Agent 可在执行过程中请求额外上下文："请提供 src/event-bus 的测试文件"

### 6.6 Work System Hook 集成

Team 复用 Work System 的 Hook 机制（见 work-design.md §6），在 work item 执行生命周期的关键节点注入 Team 级行为：

| Hook 点 | Team 用途 |
|---------|----------|
| `BeforeExecution` | 检查安全围栏（危险操作预检）、加载上下文 |
| `BeforeStep` | 记录步骤开始日志 |
| `AfterStep` | 更新通信空间进度（如 "已完成 step 2/4"） |
| `AfterExecution` | 记录执行完成日志 |
| `OnSuccess` | 通知通信空间完成 → 触发 WorkflowEngine 流转 |
| `OnFailure` | 通知通信空间失败 → 触发重试或人工介入 |

Hook 配置在 `work.yaml` 中，Team 插件注册时自动注入 Team 专属 Hook（如 `emit_event` 到通信空间）。

---

## 七、与原 kanban.md 的核心差异

| 维度 | 原 kanban.md | 新 team.md |
|------|-------------|-----------|
| 核心交互 | 人类操作 task 流转 | 通信空间对话驱动工作 |
| 成员模型 | 无身份区分 | human/agent 各有能力+权限 |
| 工作来源 | 人类手动创建 | 对话浮现 + 手动创建 |
| 流转驱动 | 人类认领/完成 | 看板调度器推送 + Agent 被动执行 + 自动流转 |
| 审核 | 作为 workflow stage | 收窄为安全围栏（危险操作/低置信度） |
| 时间概念 | 人类 scale (3天超时) | Agent scale (分钟级超时) |
| Sprint/Cycle | 无 | 无（不需要） |
| 上下文 | 无 | 共享文档自动加载 |
| 审计 | 简单 stage_history | 完整通信+执行+安全日志 |

---

## 八、数据一致性

- 所有表操作使用 SQLite 事务保证原子性
- 调度器分配操作在事务内完成（写入 stage_history + 推送到 Agent Work 队列）
- 通信空间消息采用追加模式，不修改历史消息
- 安全围栏决策强制持久化到 `safety_log`

---

## 九、可扩展性

- **多 Team 支持**：每个 Team 独立 YAML + DB 文件
- **自定义 Stage 超时**：可在 `assignment_policy` 中为每个 stage 单独设置 `execution_timeout_minutes`
- **Agent 市场**：Agent 定义可导出/导入，跨 Team 复用
- **通知钩子**：安全围栏触发时可通过 webhook 通知外部系统
- **UI 层**：通信空间 + 看板双视图，可互相切换

---

## 十、总结

从 kanban.md 到 team.md 的根本变化：

> **不是给看板加上聊天，而是让工作从对话中长出来。**

- 通信空间是入口，@mention 就是派活
- Agent 不是工具调用，是有身份的队友
- 工作流不需时间盒，看板调度器推送，Agent 24 小时在线执行
- 安全围栏替代审批流程 — 只拦截真正危险的事
- 保持 YAML+SQLite 极简内核，不引入外部依赖
