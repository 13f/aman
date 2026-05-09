# Agent Design: 事件响应式 Agent 框架

> 基于现有 event-responsive-agent 设计的演化，从"Rust 具体实现"抽象为语言无关的通用框架设计，融合主流 Agent 框架（如 Hermes、CrewAI、LangChain）的实用特性。

---

## 1. 核心哲学

```
"万物皆事件，响应即行为。"
```

**两条设计公理：**

1. **一切外部输入都是事件**：文件修改、定时器到期、Socket 消息、用户输入、第三方 Webhook、数据变更、系统信号——没有"调用"，只有"事件到达"。
2. **一切内部行为都是响应**：Agent 不做"主动轮询等待"，只做"事件到达后响应"。即使定时任务，也是 Timer 事件源发出的周期性事件引发的响应。

> 这与传统 Agent 框架（以 Chat 循环为中心）的根本区别：不再有一个"主循环等待用户输入"，而是有一个**统一事件循环**等待任意事件源就绪。

---

## 2. 整体架构

```
┌─────────────────────────────────────────────────────────────────────┐
│                    Agent Runtime (事件循环引擎)                        │
│                                                                     │
│  ┌──────────┐  ┌──────────┐  ┌──────────┐  ┌──────────┐           │
│  │  Timer   │  │   File   │  │ Webhook  │  │  Chat    │  ...       │
│  │ Source   │  │ Watcher  │  │ Listener │  │ Platform │  EventSrc  │
│  └────┬─────┘  └────┬─────┘  └────┬─────┘  └────┬─────┘           │
│       └──────────────┴──────────────┴──────────────┘               │
│                             │                                       │
│                    ┌────────▼────────┐                              │
│                    │   Event Bus     │  ← 统一的事件通道              │
│                    │  (优先级队列)    │                              │
│                    └────────┬────────┘                              │
│                             │                                       │
│                    ┌────────▼────────┐                              │
│                    │ Event Dispatcher│  ← 路由 + 过滤 + 转换         │
│                    └────────┬────────┘                              │
│                             │                                       │
│        ┌────────────────────┼────────────────────┐                  │
│        ▼                    ▼                    ▼                  │
│  ┌──────────┐       ┌──────────┐        ┌──────────┐              │
│  │ Pipeline │       │   Skills │        │ Workflow │              │
│  │ (链式)    │       │  (独立)   │        │ (状态机)  │              │
│  └────┬─────┘       └────┬─────┘        └────┬─────┘              │
│       └──────────────────┴───────────────────┘                       │
│                             │                                       │
│                    ┌────────▼────────┐                              │
│                    │   Tool Runner   │  ← 执行具体操作               │
│                    └─────────────────┘                              │
└─────────────────────────────────────────────────────────────────────┘
```

---

## 3. 核心概念与模块

### 3.1 Event（事件）—— 系统的一等公民

事件是框架的"数据原子"。所有模块之间只通过事件通信。

```
Event {
    id:         全局唯一标识（UUID）
    source:     事件来源标识（"timer:heartbeat", "fs:watchdir", "webhook:github"）
    type:       事件类型（enum: FILE_CHANGED | TIMER_TICK | MESSAGE_RECEIVED | ...）
    timestamp:  事件发生时间
    priority:   优先级（HIGH | NORMAL | LOW）—— 用于队列调度
    payload:    事件数据（结构化，与 type 对应）
    metadata:   可选元数据（追踪链、重试次数、TTL）
}
```

事件的生命周期：

```
产生 → [Event Bus 排队] → [Dispatcher 过滤] → [Pipeline/Skill/Workflow 处理]
                                                          ↓
                                                    可能产出新事件 → 重新进入 Event Bus
```

### 3.2 Event Source（事件源）—— 外部世界的适配器

每个事件源适配一种外部输入，实现统一的接口：

```
EventSource {
    // === 生命周期 ===
    id() -> String
    type() -> SourceType          // TIMER | FILE | NETWORK | WEBHOOK | DATA | PLATFORM | CUSTOM
    init(config) -> Result        // 注册到事件循环
    shutdown() -> Result          // 优雅关闭

    // === 事件产出 ===
    poll() -> Vec<Event>          // 非阻塞收割待发事件
    // 或：事件源内部自行 Push 事件到 Event Bus

    // === 运行时控制 ===
    pause()
    resume()
    reconfigure(config)           // 动态调整参数（如修改轮询间隔）
}
```

事件源的两种工作模式：

| 模式 | 说明 | 适用场景 |
|------|------|---------|
| **Pull** | 事件循环定期调用 `poll()` 收割事件 | 文件轮询、数据库变更检查、RSS 拉取 |
| **Push** | 事件源内部监听，主动注入事件到 Bus | Webhook 服务器、Socket 连接、消息队列消费者 |

常见事件源：

| 事件源 | 产出事件类型 | 实现方式 |
|--------|------------|---------|
| TimerSource | TIMER_TICK | 定时器（cron 表达式或固定间隔） |
| FileWatchSource | FILE_CHANGED / CREATED / DELETED | 文件系统通知（inotify/FSEvents/ReadDirectoryChanges） |
| WebhookSource | WEBHOOK_RECEIVED | HTTP 服务器监听回调 URL |
| ChatPlatformSource | MESSAGE_RECEIVED | SDK 回调 / WebSocket 监听 |
| DataChangeSource | DATA_INSERTED / UPDATED / DELETED | 轮询 diff 或应用层主动通知 |
| SocketSource | NETWORK_DATA / CONNECTION_EVENT | TCP/UDP/WebSocket 监听 |
| PollSource | POLL_RESULT | 定时拉取外部 API（股票价格、天气、RSS） |
| SignalSource | SYSTEM_SIGNAL | OS 信号（SIGTERM, SIGHUP, SIGUSR1） |
| CronSource | CRON_TICK | 类似 cron 的精确调度，支持秒级 |

### 3.3 Event Bus（事件总线）—— 框架的中枢神经系统

```
Event Bus {
    // 核心功能
    publish(event)                // 发布事件（异步非阻塞）
    subscribe(filter, handler)    // 订阅事件（按类型/来源/内容匹配）
    
    // 调度策略
    priority_queue:               // 高优先级事件先处理
    backpressure:                 // 队列满时降级或丢弃低优先级事件
    ordering_guarantee:           // 同一来源的事件保序
    
    // 监控
    metrics:                      // 吞吐量、延迟、队列深度
}
```

事件总线的两种实现选择：

| 方案 | 适用场景 | 优势 | 劣势 |
|------|---------|------|------|
| **内存总线** | 单进程 Agent | 零延迟，无序列化开销 | 不跨进程，丢失重启 |
| **持久化总线** | 分布式/高可靠 | 持久化，可回溯，跨进程 | 增加延迟和复杂度 |

### 3.4 Event Dispatcher（事件分发器）—— 路由与转换引擎

Dispatcher 的核心职责：**决定一个事件应该交给哪些处理器**。

```
Dispatcher {
    // 路由规则
    routes: [
        { match: {type: "FILE_CHANGED", source: "watchdir/docs"},
          target: "pipeline:doc-sync" },
        { match: {type: "TIMER_TICK", source: "cron:backup"},
          target: "skill:backup" },
        { match: {type: "MESSAGE_RECEIVED", priority: HIGH},
          target: ["workflow:urgent-reply", "skill:log"] },
    ]
    
    // 转换规则（事件 → 新事件）
    transforms: [
        { match: {type: "WEBHOOK_GITHUB_PUSH"},
          transform: (event) -> [Event{type: "CODE_UPDATED"}, Event{type: "TRIGGER_CI"}] }
    ]
    
    // 过滤规则
    filters: [
        { match: {type: "TIMER_TICK"},
          filter: rate_limit(per: "5s") }  // 防抖/限频
    ]
}
```

### 3.5 Pipeline（响应管道）—— 链式处理

Pipeline 是一系列"处理单元"的链式组合，事件依次经过每个单元。

```
Pipeline: Event → [Filter] → [Transform] → [Action] → [Output]

// 示例：文件变更 → 自动同步到远端
Pipeline "doc-sync" {
    trigger: FILE_CHANGED in "/watchdir/docs"
    steps: [
        1. Filter: 只关注 .md 文件
        2. Transform: 读取文件内容
        3. Action: 调用 Tool "git push"
        4. Output: 产出事件 NOTIFY_SYNC_DONE
    ]
}
```

Pipeline 的特性：

- **可组合**：一个 Pipeline 的输出可作为另一个的输入
- **可中断**：任一步骤返回"中断"则终止
- **可分支**：条件判断后走不同路径
- **可重试**：失败步骤可配置重试策略

### 3.6 Skill（技能）—— 能力的独立单元

Skill 是 Agent 的行为能力单元，每个 Skill 可响应特定事件，完成特定任务。

```
Skill {
    // === 元信息 ===
    name:           string
    description:    string
    version:        string
    
    // === 事件绑定 ===
    triggers: [     // 触发条件
        { event_type: TIMER_TICK, source: "cron:daily-report" },
        { event_type: MESSAGE_RECEIVED, match: content contains "/weather" },
    ]
    
    // === 执行体 ===
    execute(event, context) -> Result
    
    // === 生命周期 ===
    on_load()       // 加载时初始化
    on_unload()     // 卸载时清理
}
```

Skills 的加载模式：

| 模式 | 说明 | 示例 |
|------|------|------|
| **声明式** | 通过配置文件声明，框架自动加载 | YAML/JSON 配置中的 skill 列表 |
| **发现式** | 扫描指定目录，自动发现并注册 | 类似 Hermes skills 目录扫描 |
| **热加载** | 运行时监控 skill 文件变更，自动重载 | 开发时修改即生效 |

### 3.7 Workflow（工作流/状态机）—— 有状态的业务流程

对于需要追踪状态的复杂流程，使用状态机模型：

```
Workflow {
    // 状态定义
    states: [PENDING, RUNNING, APPROVED, REJECTED, CANCELLED]
    initial: PENDING
    
    // 转移表
    transitions: [
        { from: PENDING,  event: START,   to: RUNNING,  guard: hasPermission },
        { from: RUNNING,  event: APPROVE, to: APPROVED, action: notifyUser },
        { from: RUNNING,  event: REJECT,  to: REJECTED, action: notifyUser },
        { from: ANY,      event: CANCEL,  to: CANCELLED },
    ]
    
    // 生命周期钩子
    on_enter(state)     // 进入状态时
    on_leave(state)     // 离开状态时
}
```

每个 Workflow 实例维护自己的状态，事件驱动其状态迁移。适合：审批流、任务流转、订单处理。

### 3.8 Tool Runner（工具执行器）—— 操作外部世界

Tools 是 Agent 执行具体操作的"手"。与 Skills 的区别：Skill 是**逻辑单元**，Tool 是**执行单元**。

```
Tool {
    // === 元信息 ===
    name:           string
    description:    string
    parameters:     schema   // 参数定义
    returns:        schema   // 返回值定义
    
    // === 执行 ===
    execute(params) -> Result<Output>
    
    // === 执行模式 ===
    mode: LOCAL | REMOTE | CONTAINER | SANDBOX
}
```

Tools 的类型：

| 类型 | 说明 | 示例 |
|------|------|------|
| **内置工具** | 框架内置 | 文件读写、HTTP 请求、数据库查询 |
| **脚本工具** | 调用外部脚本 | shell script, Python script, binary |
| **API 工具** | 调用外部 API | REST, GraphQL, gRPC |
| **管道工具** | 组合多个工具 | A→B→C 串行，或 A+B 并行 |
| **容器工具** | 隔离执行 | Docker container, WASM sandbox |

Tool 的沙箱与安全：
- 每个 Tool 可以配置执行权限（允许访问的文件路径、网络端点、命令白名单）
- 脚本工具应有超时和资源限制
- 容器工具提供更强的隔离

---

## 4. 插件系统（Plugin System）

插件是打包了"一个或多个 EventSource + Skill + Tool"的独立模块，可热插拔。

### 4.1 插件结构

```
plugin/
├── plugin.yaml           # 插件元数据和声明
├── skills/               # 插件提供的技能
│   └── weather.skill
├── tools/                # 插件提供的工具
│   └── http.tool
├── event_sources/        # 插件提供的事件源
│   └── weather-api.source
└── assets/               # 静态资源
```

### 4.2 插件声明

```yaml
# plugin.yaml
name: "weather-plugin"
version: "1.0.0"
description: "天气查询与通知"

depends_on:       # 依赖
  - "http-toolkit >= 2.0"

lifecycle:
  on_load: "init_db()"           # 加载时执行
  on_unload: "close_connections()" # 卸载时清理

exports:
  skills:
    - "weather-query"           # 响应 MESSAGE → 查天气
    - "weather-alert"           # 响应 TIMER → 检查极端天气
  tools:
    - "get-weather"             # 可被其他 skill 调用
  event_sources:
    - "weather-poll"            # 定时轮询天气预警
    
config_schema:                  # 运行时配置界面
  api_key: { type: string, required: true }
  city: { type: string, default: "Beijing" }
  poll_interval: { type: integer, default: 300 }
```

### 4.3 插件生命周期

```
        发现 / 注册
            │
        ┌───▼───┐
        │ Loaded │──→ resolve dependencies
        └───┬───┘
            │
        ┌───▼────┐
        │ Enabled │──→ register event-sources, skills, tools
        └───┬────┘
            │
        ┌───▼────┐
        │ Running │──→ 正常运作
        └───┬────┘
            │
    ┌───────┼───────┐
    ▼       ▼       ▼
  Paused  Disabled Shutdown
```

### 4.4 插件隔离策略

| 策略 | 说明 | 适用 |
|------|------|------|
| **进程内** | 同一进程，通过接口隔离 | 可信任插件 |
| **子进程** | 独立进程，IPC 通信 | 不信任插件 |
| **容器** | Docker 容器隔离 | 高风险插件 |
| **WASM** | WebAssembly 沙箱 | 需要轻量隔离 |

---

## 5. Skills 系统（Skills System）—— 即装即用的行为模块

### 5.1 Skill 声明

```yaml
# skills/backup.skill
name: "auto-backup"
version: "2.0.0"
description: "定时备份指定目录到远端存储"

triggers:
  - event_type: CRON_TICK
    source: "cron:backup-hourly"
    
  - event_type: SIGNAL_RECEIVED
    match: signal == "SIGUSR1"
    
capabilities: ["file.read", "archive.zip", "s3.upload"]

tools_required:
  - "archive-tool"    # 必须依赖的工具

config:
  source_dir: { type: path, default: "/data" }
  target: { type: string }        # s3://bucket/path
  retention_days: { type: integer, default: 7 }
```

### 5.2 Skill 的执行上下文

每个 Skill 执行时获得一个 Context，提供：

```
SkillContext {
    event:          Event        // 触发事件
    state_store:    KeyValueStore // 持久化存储（该 skill 的私有空间）
    event_bus:      Publisher    // 可产出新事件
    tools:          ToolRegistry  // 可调用的工具集
    logger:         Logger
    lifespan:       Scope        // 执行周期的生命周期管理
}
```

### 5.3 Skill 间协作

```
// 方式一：事件链
Skill A 执行后 publish(Event) → Event Bus → Skill B 响应

// 方式二：共享状态
Skill A 写入 state_store → Skill B 读取 state_store

// 方式三：工具共享
Skill A 和 Skill B 共用同一个 Tool
```

---

## 6. Cron 定时任务系统

### 6.1 架构

Cron 系统本质上是一个**特殊的 Timer EventSource**，它：

1. 解析 cron 表达式（支持标准 5 字段 + 秒级 6 字段）
2. 在指定时间点向 Event Bus 发布 CRON_TICK 事件
3. 支持动态增删改定时任务（无需重启 Agent）

### 6.2 Cron 配置

```yaml
cron_jobs:
  - id: "daily-report"
    schedule: "0 9 * * *"        # 每天 9:00
    timezone: "Asia/Shanghai"
    event:
      type: CRON_TICK
      payload: { task: "generate-report" }
    enabled: true
    
  - id: "market-price-poll"
    schedule: "*/5 * * * *"       # 每 5 分钟
    enabled: true

  - id: "adaptive-poll"
    schedule: "dynamic"           # 动态频率
    initial_interval: "30s"
    # 运行时可通过 API：reconfigure("adaptive-poll", {interval: "10s"})
```

### 6.3 运行时管理

```
CronManager {
    add_job(id, schedule, event_template)
    remove_job(id)
    update_job(id, new_schedule)
    pause_job(id)
    resume_job(id)
    list_jobs() -> [JobStatus]
    get_next_run(id) -> Timestamp
}
```

---

## 7. Tools 系统（工具/运行时）

### 7.1 工具分类与架构

```
ToolRunner
├── BuiltinTools       # 内置：file, http, exec, db, crypto...
├── ScriptTools        # 脚本：shell, python, ruby, node...
├── APITools           # API：REST, GraphQL, gRPC, WebSocket...
├── PipelineTools      # 组合：序列(seq) / 并行(par) / 条件(if)
└── ContainerTools     # 容器：docker, wasm...
```

### 7.2 工具声明

```yaml
# tools/pdf-report.tool
name: "generate-pdf"
description: "根据模板和数据生成 PDF 报告"
mode: container           # 使用容器隔离执行
image: "report-gen:latest"
timeout: 60s

parameters:
  template: { type: string, required: true, description: "模板 ID" }
  data:     { type: object, required: true }
  
returns:
  type: file
  description: "生成的 PDF 文件路径"
  
security:
  allowed_paths: ["/tmp/reports"]
  network: false           # 禁止网络访问
  max_memory: "512MB"
```

### 7.3 工具执行流程

```
Tool.execute(params)
    │
    ├── 1. 校验参数 → 不符合则返回校验错误
    │
    ├── 2. 安全检查 → 检查参数是否在白名单内
    │
    ├── 3. 资源分配 → 分配超时、内存、临时目录
    │
    ├── 4. 执行 → 同步或异步（返回 Future）
    │       ├── Builtin: 本进程内调用
    │       ├── Script:  启动子进程
    │       ├── API:     HTTP 请求
    │       └── Container: Docker run
    │
    ├── 5. 输出转换 → 统一输出格式
    │
    └── 6. 清理 → 释放临时资源
```

---

## 8. 完整的事件响应流示例

### 场景：文件变更 → 自动处理 → 通知

```
时间线：

[FileWatchSource]
    │ 检测到 /watch/invoices/invoice-1024.pdf 被创建
    │ publish(
    │   Event{
    │     type: FILE_CREATED,
    │     source: "watch:invoices",
    │     payload: {path: "/watch/invoices/invoice-1024.pdf", size: 1MB}
    │   }
    │ )
    ▼

[Event Bus]
    │ 按优先级入队
    ▼

[Dispatcher]
    │ 匹配 route: {type: FILE_CREATED, source: "watch:invoices"}
    │ → target: pipeline:"invoice-processor"
    ▼

[Pipeline "invoice-processor"]
    │ Step 1 (Filter): .pdf 结尾 → 通过
    │ Step 2 (Transform): 读取文件 → OCR 识别文本
    │ Step 3 (Action): 调用 Tool "extract-invoice-data"
    │   └── Tool Runner → 启动 OCR 容器 → 返回结构化数据
    │ Step 4 (Action): 调用 Tool "insert-db" 写入数据库
    │ Step 5 (Output): publish(
    │   Event{
    │     type: INVOICE_PROCESSED,
    │     payload: {invoice_id: 1024, amount: "¥3,200"}
    │   }
    │ )
    ▼

[Event Bus] ← 新事件重新入队
    ▼

[Dispatcher]
    │ 匹配 route: {type: INVOICE_PROCESSED}
    │ → target: [skill:"slack-notification", skill:"accounting-sync"]
    ▼

[Skill "slack-notification"]
    │ 发送 Slack 消息："已处理发票 #1024，金额 ¥3,200"
    ▼

[Skill "accounting-sync"]
    │ 调用 Tool "api:erp-sync" → 同步到财务系统
```

---

## 9. 配置与声明

### 9.1 完整配置结构

```yaml
agent:
  name: "my-event-agent"
  version: "1.0.0"

event_bus:
  type: in_memory
  max_queue_size: 10000
  backpressure: drop_lowest_priority

event_sources:
  - type: timer
    id: "heartbeat-30s"
    config:
      interval: "30s"
      heartbeat: true

  - type: file_watch
    id: "watch-invoices"
    config:
      paths: ["/watch/invoices"]
      recursive: true
      events: [created, modified]

  - type: cron
    id: "daily-tasks"
    config:
      jobs:
        - schedule: "0 9 * * *"
          event: { type: CRON_TICK, payload: { task: "daily" } }

  - type: webhook
    id: "github-webhook"
    config:
      port: 8080
      path: "/webhook/github"

plugins:
  - name: "weather"
    enabled: true
    config:
      api_key: "${WEATHER_API_KEY}"
  - name: "slack-connector"
    enabled: true

skills:
  - name: "auto-backup"
    triggers:
      - event_type: CRON_TICK
        source: "daily-tasks"
    config:
      source_dir: "/data"
      target: "s3://backup-bucket"

  - name: "invoice-processor"
    triggers:
      - event_type: FILE_CREATED
        source: "watch-invoices"
    config:
      ocr_engine: "tesseract"

tools:
  - name: "ocr-extract"
    type: container
    config:
      image: "ocr-engine:latest"
      timeout: 120

  - name: "slack-send"
    type: api
    config:
      endpoint: "https://slack.com/api/chat.postMessage"
      auth_token: "${SLACK_TOKEN}"

workflows:
  - name: "approval-flow"
    states: [pending, reviewing, approved, rejected]
    transitions:
      - { from: pending,  event: SUBMIT,   to: reviewing }
      - { from: reviewing, event: APPROVE,  to: approved }
      - { from: reviewing, event: REJECT,   to: rejected }
```

### 9.2 运行时接口

Agent 暴露以下控制接口（可通过 CLI / HTTP / IPC 调用）：

```
POST /agent/start              # 启动事件循环
POST /agent/shutdown           # 优雅关闭

POST /event-source/{id}/pause  # 暂停事件源
POST /event-source/{id}/resume # 恢复事件源
PUT  /event-source/{id}/config # 动态重配置

POST /plugin/{name}/enable     # 启用插件
POST /plugin/{name}/disable    # 禁用插件

POST /cron/add                 # 添加定时任务
POST /cron/{id}/update         # 更新定时任务
POST /cron/{id}/remove         # 删除定时任务

GET  /metrics                  # 运行指标
GET  /health                   # 健康检查
POST /inject-event             # 手动注入事件（调试用）
```

---

## 10. 设计决策记录

### 决策 1：事件优先于调用

选择了"一切皆事件"而非"模块间直接调用"。

- **理由**：模块间直接调用会引入紧耦合，违反"只通过事件通信"的原则。事件解耦使得任意模块可以添加、移除、替换而不影响其他模块。
- **代价**：增加了事件序列化和反序列化的开销，且调试时需要追踪事件流。
- **缓解**：提供事件追踪 ID（每个事件带 TraceID），调试模式下可记录完整事件流。

### 决策 2：Pipeline 和 Skill 双模型

同时支持链式的 Pipeline 和独立的 Skill。

- **理由**：Pipeline 适合"固定流程"（A→B→C），Skill 适合"单一职责的响应单元"（收到事件→做一件事）。二者互补而非互斥。
- **对比**：LangChain 以 Chain 为主，CrewAI 以 Agent 为主，Hermes 以 Skill 为主。本设计同时支持三种模式。

### 决策 3：配置驱动的声明式注册

模块的注册通过声明式配置（YAML/JSON）而非代码硬编码。

- **理由**：事件源的注册顺序、路由规则、Pipeline 步骤在运行时可变更。声明式配置便于版本管理和自动化部署。
- **代价**：复杂场景可能需要 DSL，纯粹 YAML 表达能力有限。
- **缓解**：支持"声明式 + 脚本扩展"混合模式。

### 决策 4：State Machine 作为一等公民

将状态机提升为与 Skill 平行的模块类型。

- **理由**：有状态的工作流是 Agent 常见场景（任务审批、工单流转、订单处理），用通用状态机来表达比在 Skill 内手工维护状态更可靠、更可视化。
- **对比**：大多数 Agent 框架没有内建状态机，需要开发者在 Skill 内自己维护状态。

### 决策 5：同一来源事件保序

Event Bus 保证来自同一 Event Source 的事件按产生顺序处理。

- **理由**：文件修改事件必须顺序处理（否则旧事件可能覆盖新事件）；数据库变更事件也必须顺序处理。
- **代价**：跨来源的事件顺序不做保证。
- **缓解**：如需跨来源序，使用 Workflow 状态机协调。

---

## 11. 已知风险与兜底

| 风险 | 等级 | 应对策略 |
|------|------|---------|
| **事件风暴**：高吞吐事件源短时间涌入大量事件 | 高 | 背压机制 + 优先级队列 + 丢弃最低优先级事件；可配置每秒事件上限 |
| **循环事件**：事件 A → 处理 → 事件 B → 处理 → 事件 A（死循环） | 中 | 每个事件携带 TTL（最大传递次数），超过则丢弃；可配置循环检测规则 |
| **单点故障**：Event Bus 或主事件循环崩溃 | 中 | 持久化模式下，重启后从上次 checkpoint 恢复；可选多实例主备切换 |
| **插件故障**：不稳定的第三方插件拖垮整个 Agent | 高 | 子进程/容器隔离插件；插件执行超时保护；看门狗自动重启 |
| **状态丢失**：Workflow 状态丢失导致业务异常 | 中 | State Store 持久化到磁盘/数据库；Workflow 支持快照与恢复 |
| **资源泄漏**：Tool/Plugin 未正确释放资源 | 中 | 每个 Tool 执行有超时和资源配额；Plugin 有 on_unload 钩子；运行时可监控资源泄漏 |

---

## 12. 与现有 Agent 框架的异同

| 特性 | 本设计 | Hermes Agent | CrewAI | LangChain |
|------|--------|-------------|--------|-----------|
| 核心范式 | **事件驱动** | 聊天循环 + Cron | 多 Agent 协作 | Chain / Agent 调用 |
| 事件源 | 一等公民（文件/网络/定时器/Webhook） | 仅 Cron/Timer | 无内建 | 无内建 |
| Skills | 基于事件的响应单元 | 基于 Skills 目录 | 基于 Task | 基于 Tool |
| Plugins | 热插拔 + 隔离策略 | 插件钩子系统 | 无 | 无 |
| Workflow | 内建 State Machine | 无 | 流程驱动 | 链式调用 |
| Tools | 多种执行模式 | 工具系统 | 工具注册 | 工具调用 |
| 配置 | 声明式 YAML | YAML | Python | Python |

**本设计的关键差异化**：它不是"以 Chat 为中心"的 Agent，而是"以事件为中心"的 Agent。传统 Agent 等你问问题，事件驱动 Agent 不等任何人——它在持续观察世界、响应变化。

---

## 13. 设计原则总结

```
1. 万物皆事件 —— 所有外部输入以统一的事件模型抽象
2. 响应即行为 —— Agent 不做"主动"，只做"响应"
3. 松耦合 —— 模块间只通过 Event Bus 通信
4. 可观测 —— 事件流可追踪、可记录、可回放
5. 可演化 —— 插件热插拔，Skill 热加载，配置热更新
6. 安全边界 —— 插件和工具按信任等级隔离
7. 务实优先 —— 简单场景用 Pipeline，复杂场景用 State Machine
```
