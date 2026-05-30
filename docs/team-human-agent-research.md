# Human+Agent Team Design Research
## 基于 Plane 调研的 aman team.md 扩展方案

---

## 一、Plane 调研摘要

### 1.1 Plane 是什么

Plane 是 GitHub 上 Star 最多的开源项目管理平台 (AGPL-3.0, 46k+ stars)，定位为 Jira/Linear/Monday/ClickUp 替代品。技术栈：React Router 前端 + Django 后端 + Node.js 运行时。自托管支持 Docker/Kubernetes。

### 1.2 核心概念映射

| Plane 概念 | aman team.md 对应 | 说明 |
|-----------|------------------|------|
| Workspace | (无) | 顶层组织容器，含多个 Project |
| Project | Project (YAML+DB) | 单一 kanban 看板 |
| Work Item | Task | 可指派的工作单元 |
| Cycle | (无) | 时间盒迭代 (类似 Sprint) |
| Module | (无) | 项目子模块拆分 |
| State | Stage | 工作流状态列 |
| View | 看板视图 | List/Kanban/Calendar/Gantt/Spreadsheet |
| Page/Wiki | (无) | 共享知识文档 |
| Member | 用户 (claim 机制中的执行者) | 人类成员 |
| Agent (bot user) | **(缺失)** | AI 代理，作为特殊成员 |

### 1.3 Plane 的 Agent 能力 (核心差异化)

Plane 是目前唯一将 AI Agent 作为一等公民内建的项目管理工具：

```
人类 + Agent 共享同一个 workspace
    │
    ├── 人类: Owner / Admin / Member / Guest
    │     └── 通过 UI 操作、评论、指派
    │
    └── Agent (bot user): 不计费席位
          ├── @mention 触发 → AgentRun 生命周期
          ├── 可被指派为 work item 的 assignee
          ├── 可读项目/cycle/page 上下文
          ├── 可创建/更新/关闭 work item
          ├── 所有操作审计日志与人类一致
          └── 权限可细粒度控制
```

**AgentRun 生命周期：**
```
created → in_progress → awaiting (等用户输入) → completed
                    ↘ failed / stopped / stale (5分钟无更新)
```

**AgentRunActivity 类型：**
| 类型 | 可见性 | 用途 |
|------|-------|------|
| prompt | 可见 | 用户发给 agent 的消息 |
| thought | 不可见 | agent 内部推理 (类似 Chain-of-Thought) |
| action | 不可见 | agent 工具调用 |
| response | 创建评论 | agent 最终回复 |
| elicitation | 创建评论 | agent 向用户提问 |
| error | 可见 | 错误信息 |

**MCP Server：** Plane 提供官方 MCP Server (`plane-mcp-server`)，支持 stdio/HTTP+OAuth/HTTP+PAT 三种传输模式，Agent 可通过 MCP 协议创建/查询/更新 work item。

**Agent Dev Kit (ADK)：** 声明式框架，YAML 定义 agent 行为，支持版本管理、干跑测试、作用域限制。

---

## 二、aman team.md 现状分析

当前 team.md 设计了一个纯人类的简约 kanban：

### 优势
- **简洁**：YAML + SQLite，无外部依赖
- **配置自由**：Stage 数量、顺序、流转图完全由 YAML 定义
- **ID 稳定**：无语义 shortUUID，避免重命名破坏引用
- **认领+超时**：人性化的任务分配与回收机制
- **多项目隔离**：每个项目独立配置文件

### 关键缺失 (面向 Human+Agent 场景)

| 维度 | 当前状态 | Agent 场景需求 |
|------|---------|---------------|
| **身份模型** | 仅有 `creator`/`assignee` 文本字段 | 需区分 Human vs Agent，Agent 需携带能力描述、信任级别 |
| **权限模型** | 无 | Agent 需要细粒度权限：哪些 Stage 可操作、能否创建任务、能否修改配置 |
| **事件系统** | 无 | Agent 需要订阅事件 (任务创建/状态变更/评论) 以触发自主行为 |
| **上下文共享** | 无 | Agent 需要读取项目文档、历史决策、团队规范 |
| **多 Agent 协调** | 单一 assignee | 需要：Agent 协作池、技能路由、审查门控、冲突检测 |
| **审计追踪** | 仅 stage_history | 需要完整活动日志，记录 Agent 的推理+工具调用+决策依据 |
| **Agent 自主性** | 纯被动操作 | Agent 需主动扫描、认领、升级、通知 |

---

## 三、设计升级方案

### 3.1 核心理念

> 不替换 Plane，而是在 aman 的轻量级 YAML+SQLite 内核上，引入 Plane 验证过的 Agent 协作模式。保持简洁可自托管，同时支持 Human+Agent 混编团队。

### 3.2 身份模型扩展

```yaml
# team.yaml 新增
members:
  - id: "jerin"
    type: human
    name: "Jerin"
    roles: [owner, admin]

  - id: "coder-agent"
    type: agent
    name: "Coder Agent"
    provider: "anthropic/claude-sonnet-4"   # 或指向 aman agent profile
    capabilities: [code, review, test]       # 技能标签
    autonomy_level: supervised               # autonomous | supervised | manual_trigger
    allowed_stages: ["xY8zW2", "pL9mN4"]    # 可操作的 stage
    max_concurrent: 3                         # 最多同时认领任务数
    context_sources:                          # 可读取的上下文
      - "docs/architecture.md"
      - "docs/coding-standards.md"
```

### 3.3 Stage 级别的 Agent 策略

扩展现有 Stage 配置，增加 agent 行为策略：

```yaml
stages:
  - id: "aB3cD5"
    name: "待办"
    order: 1
    allowed_next: ["xY8zW2"]
    agent_policy:
      auto_claim: false              # Agent 不可自动认领
      require_human_triage: true     # 需要人类分类

  - id: "xY8zW2"
    name: "处理中"
    order: 2
    allowed_next: ["pL9mN4"]
    agent_policy:
      auto_claim: true               # Agent 可自动认领
      skill_match: ["code", "test"]  # 按能力匹配
      claim_timeout_minutes: 60      # Agent 超时更短 (人类是3天)
      max_agents: 2                  # 最多2个 agent 同时在该 stage

  - id: "pL9mN4"
    name: "审核"
    order: 3
    allowed_next: ["xY8zW2", "qR2vT7"]
    agent_policy:
      auto_claim: true
      skill_match: ["review"]
      require_human_signoff: true    # Agent 审核后需人类确认
```

### 3.4 事件系统 (Agent 触发器)

```yaml
# team.yaml 新增
events:
  - trigger: task.created
    actions:
      - agent: "triage-agent"
        action: "auto_classify"      # 自动分类+打标签

  - trigger: task.stage_changed
    condition: "to_stage == 'xY8zW2'"   # 进入"处理中"
    actions:
      - agent: "coder-agent"
        action: "auto_claim_if_skill_match"

  - trigger: task.claim_timeout
    actions:
      - agent: "notifier-agent"
        action: "escalate_to_human"

  - trigger: task.completed
    condition: "current_stage == 'pL9mN4' AND completed_by.type == 'agent'"
    actions:
      - agent: "reviewer-agent"
        action: "request_human_signoff"
```

### 3.5 上下文共享 (Pages/Wiki)

新增 `pages` 表，类似 Plane 的 Pages：

```sql
CREATE TABLE pages (
    id INTEGER PRIMARY KEY,
    title TEXT NOT NULL,
    content TEXT,
    category TEXT,              -- 'architecture', 'coding_standard', 'decision_log'
    created_at DATETIME,
    updated_at DATETIME,
    author TEXT                 -- 人类或 agent
);

CREATE TABLE page_attachments (
    id INTEGER PRIMARY KEY,
    page_id INTEGER,
    task_id INTEGER,            -- 可关联到任务
    content TEXT,               -- agent 的执行总结、review 意见等
    created_by TEXT,
    created_at DATETIME
);
```

### 3.6 审计日志

扩展 `stage_history` 为完整的活动日志：

```sql
CREATE TABLE activity_log (
    id INTEGER PRIMARY KEY,
    task_id INTEGER,
    actor_id TEXT,              -- 人类或 agent ID
    actor_type TEXT,            -- 'human' | 'agent'
    action TEXT,                -- 'claimed', 'completed', 'commented', 'auto_triaged'
    stage TEXT,                 -- 可选
    detail JSON,                -- {"reasoning": "...", "tools_used": [...], "confidence": 0.9}
    created_at DATETIME
);
```

### 3.7 多 Agent 协调模式

```
                 ┌─────────────┐
                 │   待办       │  ← 人类 triage
                 └──────┬──────┘
                        │
           ┌────────────┼────────────┐
           ▼            ▼            ▼
    ┌──────────┐ ┌──────────┐ ┌──────────┐
    │ coder-1  │ │ coder-2  │ │ designer │  ← Agent 池，按技能认领
    └────┬─────┘ └────┬─────┘ └────┬─────┘
         │            │            │
         └────────────┼────────────┘
                      ▼
               ┌──────────┐
               │   审核    │  ← Agent 审核 + 人类签字
               └────┬─────┘
                    │
              ┌─────┴─────┐
              ▼           ▼
        ┌────────┐  ┌────────┐
        │  完成  │  │  拒绝  │
        └────────┘  └────────┘
```

关键协调规则：

1. **技能路由**：Agent 只能认领 `skill_match` 匹配的任务
2. **并发上限**：每个 Agent 有 `max_concurrent` 限制
3. **Agent 审查门控**：`require_human_signoff` 阶段，Agent 完成操作后进入等待人类确认状态
4. **冲突检测**：多人/多 Agent 竞相认领时使用乐观锁
5. **超时差异化**：Agent 超时远短于人类 (分钟级 vs 天级)
6. **回退路径**：Agent 失败时任务回退到上一阶段或标记 `needs_human`

---

## 四、与 Plane 的对比与定位

| 维度 | Plane | aman team (扩展后) |
|------|-------|-------------------|
| 部署 | Cloud/Self-host (Docker/K8s) | 单文件/轻量进程 |
| UI | 完整的 Web UI + 移动端 | CLI-first，可选简单 UI |
| Agent 开发 | ADK (YAML声明式) + 市场 | 直接对接 aman agent profiles |
| 权限粒度 | 4级系统角色 + 自定义角色 | YAML 配置的角色/Stage级权限 |
| 适合场景 | 10-1000人团队，AI-native 协作 | 1-10人 + 若干 Agent 的开发团队 |
| 依赖 | Django + Node + PostgreSQL + Redis | Python/Rust 单进程 + SQLite |
| 数据控制 | 自托管可选，Cloud 默认 | 完全本地，所有数据在用户机器上 |

**核心决策：aman 不应复制 Plane，而是吸收其 Agent 协作模式，保持轻量级内核。**

---

## 五、实施路线图建议

### Phase 1: 身份 + 权限 (1-2周)
- 扩展 YAML 配置支持 `members` 和 `agent_policy`
- Stage 级别权限控制
- Agent 身份识别 (human vs agent type)

### Phase 2: 事件 + 上下文 (2-3周)
- 事件触发器系统
- Pages/上下文共享
- 完整 activity_log

### Phase 3: 多 Agent 协调 (2-3周)
- 技能路由
- 并发控制
- Agent 审查门控
- 乐观锁冲突检测

### Phase 4: 自主 Agent (3-4周)
- Agent 自主扫描+认领
- 超时升级
- 失败回退
- 与 aman agent runtime 深度集成

---

## 六、参考资源

- Plane GitHub: https://github.com/makeplane/plane
- Plane AI: https://plane.so/ai
- Plane Agents: https://plane.so/agents
- Plane Agent Dev Docs: https://developers.plane.so/dev-tools/agents/overview
- Plane MCP Server: https://github.com/makeplane/plane-mcp-server
- Plane Permissions: https://docs.plane.so/roles-and-permissions/overview
- aman 项目: /Users/jerin/projects/aman/
