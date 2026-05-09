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
    id:               全局唯一标识（UUID v7，按时间有序）
    source:           事件来源标识（"timer:heartbeat", "fs:watchdir", "webhook:github"）
    type:             事件类型（enum: FILE_CHANGED | TIMER_TICK | MESSAGE_RECEIVED | ...）
    timestamp:        事件产生时间（UTC Unix epoch 毫秒，由框架强制注入）
    priority:         优先级（HIGH | NORMAL | LOW）—— 用于队列调度
    delivery:         交付保证（AT_MOST_ONCE | AT_LEAST_ONCE | EXACTLY_ONCE）—— 与 priority 正交
    dedup_key:        去重键 [可选] — 缺省 = (source, type, payload_hash)，用于 30s 窗口去重
    payload:          事件数据（结构化，与 type 对应）
    metadata:         元数据（框架强制注入，详见下文）
}

EventMetadata {
    trace_id:         UUID          // 框架强制注入，贯穿事件全生命周期
    parent_event_id:  UUID?         // 产生此事件的父事件 ID（用于链路追踪）
    retry_count:      int = 0       // 已重试次数
    max_retries:      int = 3       // 最大重试次数（超过则进入 DLQ）
    ttl_ms:           int           // 事件生存时间（毫秒），超时未处理则丢弃
    lifespan_ms:      int?          // 关联临时资源的生命周期（如 OCR 中间文件）
    created_at:       Timestamp     // 框架强制注入
}

> **parent_event_id 循环检测**：Event Bus 在接收事件时检查 parent_event_id 链是否重复，检测到循环则标记 `[cycle_detected]` 并拒绝发布。Trace API 返回时截断循环支路。
```

> **设计决策**：TraceID 由框架强制注入而非可选。任何插件或 Skill 都无权跳过 TraceID——这是可观测性的底线。父事件 ID 辅助构建完整的事件链路树，用于调试和审计。

**交付保证语义：**

| 等级 | 含义 | 适用场景 |
|------|------|---------|
| AT_MOST_ONCE | 最多一次，允许丢 | 心跳、metrics、非关键通知 |
| AT_LEAST_ONCE | 至少一次，框架保证重试到成功或进入 DLQ | 文件处理、数据变更、业务通知 |
| EXACTLY_ONCE | 恰好一次，要求处理器幂等 + 框架去重 | 支付回调、审计日志、资金操作 |

事件的产生 → 交付生命周期：

```
产生 → 强制注入 trace_id + timestamp → [Event Bus 排队] → [Dispatcher 过滤]
    → [Pipeline/Skill/Workflow 处理]
        ↓
    可能产出新事件 → 继承 parent_event_id = 当前事件 id → 重新进入 Event Bus
        ↓
    ⚠ 循环检测：框架层检查 parent_event_id 链是否重复
       ├── 无循环 → 正常加入队列
       └── 检测到循环 → 标记 `[cycle_detected]` → 非法发布 → 触发告警
        ↓
    处理失败 → 重试 N 次 → 仍失败 → 进入 Dead Letter Channel
        ↓
    处理完成 → 记录事件完成状态（用于 checkpoint）
```

事件的生命周期管理：

```
产生 → [Event Bus 排队] → [Dispatcher 过滤] → [Pipeline/Skill/Workflow 处理]
                                                          ↓
                                                    可能产出新事件 → 重新进入 Event Bus
                                                          ↓
                                                    处理失败 N 次 → Dead Letter Channel
                                                          ↓
                                                    处理完成 → Checkpoint 记录
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

    // === 健康 ===
    health() -> HealthStatus      // 事件源自检状态
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

**FileWatchSource——文件监听稳定性保证：**

文件系统事件（inotify/FSEvents）在文件被打开写入时就会触发 `CREATED`，此时内容可能不完整。解决方案：

```
FileWatchSource 的"稳定确认"机制:

1. 收到文件系统通知 → 不立即发布事件
2. 启动静默计时器（debounce，默认 500ms，可配置）
3. 若在计时器内再次收到同一个文件的变更 → 重置计时器
4. 计时器到期 → 检查文件是否仍然打开（lsof / 文件锁检测）
5. 文件已关闭且静默期已过 → 发布 FILE_{CREATED|MODIFIED}
6. 若超过 max_stable_wait（默认 30s）文件仍未关闭 → 发出告警并强制发布
   └── 强制发布的事件 payload 附加 `incomplete: true` 标志，下游可据此判断文件可能不完整

参数：
  debounce_ms: 500                     # 去抖窗口
  max_stable_wait_ms: 30000            # 最长等待写入完成的时间
  check_open_files: true               # 是否检查文件是否仍被打开
  force_publish_on_timeout: mark_incomplete  # mark_incomplete | false | true
```

### 3.3 Event Bus（事件总线）—— 框架的中枢神经系统

```
Event Bus {
    // === 核心功能 ===
    publish(event)                // 发布事件（异步非阻塞）
    subscribe(filter, handler)    // 订阅事件（按类型/来源/内容匹配）

    // === 调度策略 ===
    priority_queue:               // 高优先级事件先处理
    backpressure:                 // 队列满时的多层降级策略
    ordering_guarantee:           // 同一来源的事件保序

    // === 去重 ===
    dedup_window_ms: 30000        // 去重时间窗口
    dedup_strategy: window        // 去重策略：window | exactly_once

    // === 持久化 ===
    persistence:                  // 持久化配置（WAL / 快照）
    checkpoint:                   // 恢复点管理

    // === 监控 ===
    metrics:                      // 吞吐量、延迟、队列深度、丢事件计数
}
```

**交付语义与去重策略：**

Event Bus 不提供全局 exactly-once（分布式系统做不到），它提供的是可组合的去重能力：

| 交付模式 | 去重机制 | 适用场景 |
|---------|---------|---------|
| AT_MOST_ONCE | 不保证去重 | 心跳、metrics |
| AT_LEAST_ONCE | 窗口去重：30s 内相同 `dedup_key`（缺省 = source+type+payload_hash）丢弃重复 | 文件处理、消息通知 |
| EXACTLY_ONCE | 窗口去重 + 处理器幂等契约（见下文） | 支付、审计、资金 |

处理器幂等契约（EXACTLY_ONCE 模式下要求）：
- 处理器必须对相同 `id` 的重复交付产生相同效果
- 建议在处理器状态存储中使用 `processed_events` 集合记录已处理的 event id
- 重放时检测已处理则直接 skip

**背压（Backpressure）—— 分层降级策略：**

替代单一的 `drop_lowest_priority`，采用分层策略：

```
当队列深度达到阈值时：

Level 1 [80% 满]  → 告警 + 降低 AT_MOST_ONCE 事件的注入优先级
Level 2 [90% 满]  → 丢弃 AT_MOST_ONCE 事件（记结构化日志：id + source + type + timestamp）
| Level 3 [95% 满]  → 阻塞所有事件源的 poll() 调用，等待队列 drain
| Level 4A [98% 满] → 溢出 AT_LEAST_ONCE 事件到磁盘缓冲区（overflow_to_disk），溢出目录设硬上限（overflow_max_bytes）
| Level 4B [溢出目录≥80%满] → 触发紧急告警 → 回退到 Level 3（阻塞 poll），不再溢出到磁盘
| Level 5 [100%满]  → 触发紧急模式：停止低优事件源，发系统告警

关键原则：
- AT_MOST_ONCE 事件在任何级别都允许丢，但必须记日志
- AT_LEAST_ONCE 事件在 Level 3 阻塞，Level 4A~4B 尝试溢出到磁盘（失败则退回阻塞），绝不静默丢弃
- EXACTLY_ONCE 事件不可丢、不可溢出——发布者必须等待
- 所有丢弃事件统一写入丢弃日志（包含 event.id, source, type, reason, timestamp）

溢出磁盘管理：
  overflow_max_bytes: 1073741824       # 1GB 硬上限，达到 80% 触发提前告警
  overflow_warn_threshold: 0.8          # 空间使用率达到此阈值时提前告警
  overflow_disk_and_wal_separate: true  # （配置建议）溢出目录与 WAL 目录使用不同磁盘分区

溢出重启恢复：
  Agent 重启时自动扫描 overflow/ 目录：
    1. 读取溢出目录中的事件文件清单
    2. 按时间戳排序后重新注入 Event Bus
    3. 注入时走标准去重逻辑（dedup_key 窗口），避免 WAL 中已处理事件重复
    4. 注入成功的事件从溢出目录删除
    5. 注入失败的事件保留在原位，记录告警日志
  ⚠ 安全约束：
    - 重启恢复期间不产生新溢出（防止复活瞬间再次触发背压）
```

**同一来源事件保序（设计决策 5）：**

Event Bus 保证来自同一 Event Source 的事件按产生顺序处理。跨来源的事件顺序不做保证——这是分布式系统中诚实的设计。如需跨来源序，使用 Workflow 状态机协调。

**事件总线的两种实现选择：**

| 方案 | 适用场景 | 优势 | 劣势 |
|------|---------|------|------|
| **内存总线** | 单进程 Agent | 零延迟，无序列化开销 | 不跨进程，重启丢失 |
| **持久化总线** | 分布式/高可靠 | 持久化，可回溯，跨进程 | 增加延迟和复杂度 |

**预写日志（WAL）模式（持久化总线）：**

```
崩溃恢复的关键机制：

1. 事件到达 → 写入 WAL（磁盘）→ 确认写入成功 → 投递到内存队列
2. 处理完成后 → 记录 checkpoint（已处理到 WAL 的哪个偏移量）
3. 崩溃重启 → 从 WAL 头部重放未确认事件
4. 对比 checkpoint 偏移量：已处理的事件（processor 幂等检查）skip

WAL 配置：
  wal_path: "/var/lib/agent/wal/"
  wal_sync: fsync              # 每次写入 fsync（最高可靠）| batch（性能优先）
  wal_rotate_bytes: 1GB        # 超过后归档旧段
  checkpoint_interval: 500     # 每处理 500 个事件写一次 checkpoint（可配置）

**WAL 提交 → 内存投递失败的恢复机制：**

WAL 提交成功但内存队列投递失败时的处理：

```
事件到达 → WAL（fsync 确认）→ 投递到内存队列
                                    ↓
                              投递失败（队列满/背压 Level 3+）
                                    ↓
                          进入[待重试队列]而非静默丢弃
                                    ↓
                      重试间隔：100ms → 500ms → 2s（指数退避，最大重试 5 次）
                                    ↓
                       ┌─── 重试成功 → 正常入队处理
                       └─── 持续失败（5 次后）→ 触发"事件积压"告警
                                            → 事件保留在待重试队列
                                            → 队列空间释放后自动恢复重试
```

约束：
  - 待重试队列与主队列独立（不占用主队列容量）
  - 待重试队列长度受限于 `retry_queue_max: 1000`（可配置）
  - 重启时自动检查待重试队列 + WAL 中是否有已确认但未投递的事件
  - 恢复后检查清单增加一项："待重试队列清空"

**配置校验（总线模式绑定）：**
- `type: in_memory` 下 `persistence.*` 字段必须不存在，否则拒绝启动；去重表纯内存，EXACTLY_ONCE 退化为 AT_LEAST_ONCE
- `type: persistent` 下 persistence 使用默认值；去重表通过 WAL+checkpoint 持久化，重启后去重窗口继续生效
```

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

**Pipeline 失败补偿 —— Saga 模式：**

Pipeline 是多步骤操作，任何一步失败后，前面步骤已经产生的副作用（临时文件、部分数据库写入、外部 API 调用）需要清理。

```
每个 Pipeline 步骤可选的补偿定义：

Pipeline "invoice-processor" {
    trigger: FILE_CREATED in "/watch/invoices"

    steps: [
        {
            id: "ocr-extract"
            action: Tool "ocr-engine"
            compensate: Tool "cleanup-temp-file"    // 补偿：删除 OCR 产生的临时文件
            retry: { max_attempts: 3, backoff: "exponential" }
        },
        {
            id: "insert-db"
            action: Tool "db-insert"
            compensate: Tool "db-delete-by-invoice-id" // 补偿：删除已插入的数据库记录
            retry: { max_attempts: 3, backoff: "exponential" }
        },
        {
            id: "notify-slack"
            action: Tool "slack-send"
            compensate: Tool "slack-delete-message"   // 补偿：删除已发送的 Slack 消息
            retry: { max_attempts: 5, backoff: "exponential" }
        }
    ]

    // 全局补偿策略
    compensation_strategy: reverse_order   // 反向顺序执行补偿（C3 → C2 → C1）
    compensation_contract:                 // 补偿操作契约：强制幂等 + 独立超时 30s + 失败进 COMPENSATION_FAILED

    // Dead Letter Channel（死信通道）
    dlq:
        max_retries: 3
        on_dlq: alert + log
        dlq_storage: "/var/lib/agent/dlq/"
        dlq_ttl_days: 30
        dlq_ttl_config:
            pre_expiry_alert_days: [7, 3, 1]  // 到期前 7d/3d/1d 告警
            archive_on_expiry: true             // 到期归档冷存储而非直接删除
        }
}
```

**补偿执行流程（正常路径）：**

```
步骤 3 (ocr-extract) 成功 → 步骤 4 (insert-db) 失败 → 触发补偿

1. 确定补偿顺序: reverse_order → 先补偿 step4 (insert-db)，再补偿 step3 (ocr-extract)
2. 执行 step4 补偿: db-delete-by-invoice-id → 成功
3. 执行 step3 补偿: cleanup-temp-file → 成功
4. 所有补偿完成 → 记录失败事件到 DLQ
5. 发出告警: "Pipeline invoice-processor 失败，已回滚所有副作用"
```

**补偿执行流程（补偿本身失败路径）：**

```
步骤 3 成功 → 步骤 4 失败 → 触发补偿

1. 反向顺序：补偿 step4 (db-delete) → 成功
2. 补偿 step3 (cleanup-temp) → 失败（文件被占用，重试 3 次仍失败）
3. Pipeline 进入 COMPENSATION_FAILED（非终态）
4. 记录："step4 已补偿，step3 未补偿" → 推送 HIGH 级别告警 → 人工接管
```

**无需补偿的步骤：** 纯计算/只读步骤（如 Filter、校验）不需要补偿定义，但框架也会跳过它们。

**Pipeline 并发模型：**

同一 Pipeline 收到多个事件的并发处理策略。默认 serial（最安全）。

```
concurrency: serial | parallel | limited(N)
  - serial（默认）：一次只处理一个事件，前一个完成再处理下一个
    - ✅ 无 State Store 竞态——天然有序
    - ✅ 无文件系统冲突——一次只访问一个临时文件
    - ⚠ 慢 Pipeline 会阻塞后续同 Pipeline 事件
    - 适用场景：大多数情况，尤其是依赖 State Store 或文件系统的 Pipeline

  - parallel：每个事件创建独立处理实例
    - ✅ 高吞吐——独立事件可以并行处理
    - ⚠ 必须满足以下条件才安全：
      a) State Store 使用 optimistic_lock（而非 last_write_wins）
      b) 每个实例使用独立临时目录隔离文件系统资源
      c) Pipeline 步骤不依赖外部排序
    - 适用场景：纯计算/纯 API 调用 Pipeline（如 OCR 多个文档、PDF 批量生成）

  - limited(N)：最多 N 个并发实例
    - ✅ 兼顾吞吐与资源控制
    - 适用场景：受外部 API 限流约束的 Pipeline（如 Slack 通知、ERP 同步）
```

框架约束：
  - concurrency: parallel 模式下，框架强制 StateStore 使用 optimistic_lock
  - concurrency: parallel 模式下，框架为每个实例分配独立临时目录
  - 并发上限受资源配额约束（CPU / 内存 / 文件描述符）

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
        { event_type: TIMER_TICK, source: "cron:daily-tasks",
          match: { payload.task: "report" } },          // payload 级匹配（与 Dispatcher match 语法对齐）
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
|| **热加载** | 运行时监控 skill 文件变更，自动重载 | 开发时修改即生效 |

**Skill 并发模型：**

Skill 的并发策略与 Pipeline 一致，默认 serial。

```
skill_concurrency: serial | parallel | limited(N)
  - serial（默认）：一次只处理一个事件
    - ✅ Skill 内 State Store 读写天然安全
    - ⚠ 一种事件的慢处理会延迟后续同类型事件
  - parallel：每个事件触发一个独立的 Skill 实例
    - ✅ 多事件同时到达时互不阻塞
    - ⚠ 需要 State Store 使用 optimistic_lock
    - ⚠ 每个实例的临时目录隔离由框架自动处理
  - limited(N)：最多 N 个并发的 Skill 实例
    - 适合受外部限流约束的 Skill
```

Skill 级和 Pipeline 级的 concurrency 独立配置，互不影响。

### 3.7 Workflow（工作流/状态机）—— 有状态的业务流程

对于需要追踪状态的复杂流程，使用状态机模型：

```
Workflow {
    // 状态定义
    states: [
        PENDING,           // 初始状态
        REVIEWING,         // 审批中
        APPROVED,          // 已通过（终态）
        REJECTED,          // 已拒绝（终态）
        CANCELLED,         // 已取消（终态）
        ERROR,             // 错误状态 —— 不可恢复错误的终点
        ARCHIVED,          // 归档状态 —— 可回收的终止状态
    ]
    initial: PENDING
    final_states: [APPROVED, REJECTED, CANCELLED, ARCHIVED]
    error_state: ERROR

    // 超时配置
    state_timeouts: {
        REVIEWING: { timeout: 7 days, on_timeout: REJECTED }   // 7 天无操作自动拒绝
        PENDING:   { timeout: 30 days, on_timeout: CANCELLED } // 30 天不提交自动取消
        ERROR:     { timeout: 7 days, on_timeout: ARCHIVED,   // 错误后 7 天自动归档
                     on_timeout_alert: true }                  // 归档前 1d/6h/1h 分别告警
    }

    // ERROR 状态默认行为（框架强制）
    // on_enter: 保存 last_active_state → 默认触发 alert+log（告警级别 HIGH）
    // on_enter: 重置 retry_count = 0（在 ERROR 状态内可用于追踪重试次数）
    // on_timeout: ERROR→ARCHIVED 前 1d/6h/1h 分别告警

    // ERROR 恢复配置
    error_recovery: {
        retry_event: RETRY                    // 从 ERROR 恢复的事件名
        retry_to: last_active_state           // 恢复到进入 ERROR 前的状态
        max_retry_count: 3                    // 最多重试恢复 3 次，超过后只能进入 ARCHIVED
        retry_backoff: "immediate"            // 立即重试 | 可配置延时策略
        on_retry_failure: archive             // 重试恢复再次失败后的行为：archive | manual_only
    }

    // 转移表
    transitions: [
        { from: PENDING,   event: SUBMIT,   to: REVIEWING, guard: hasPermission,
          on_fail: PENDING },                                  // guard 失败时留在原状态
        { from: REVIEWING, event: APPROVE,  to: APPROVED, action: notifyUser },
        { from: REVIEWING, event: REJECT,   to: REJECTED, action: notifyUser },
        { from: REVIEWING, event: ,         to: REJECTED },  // 超时自动转移（见 state_timeouts）
        { from: ANY,       event: CANCEL,   to: CANCELLED },
        { from: ANY,       event: ERROR,    to: ERROR },      // 任何状态都可进入错误状态
        { from: ERROR,     event: RETRY,    to: :last_active_state, guard: retryCount < max_retry_count,
          on_fail: ARCHIVED },                                 // 恢复失败超过上限则归档
    ]

    // 生命周期钩子
    on_enter(state)     // 进入状态时
    on_leave(state)     // 离开状态时
    on_final(state)     // 进入终态时（用于清理/归档）
}
```

**状态图（完整生命周期）：**

```
       SUBMIT
PENDING ──────→ REVIEWING ──────→ APPROVED
   ↑                │                  │
   │                │ APPROVE          │
   │                │                  │ (30天后自动)
   │          ┌─────┴─────┐            ▼
   │          │           │        ARCHIVED
   │          │           │ REJECT
   │          │           ▼
   │          │       REJECTED
   │          │           │
   │          │           │ (30天后自动)
   │          │           ▼
   │          │       ARCHIVED
   │          │
|          │ (7天无操作)
│          ▼
│      REJECTED (超时)
│
└── CANCEL ──→ CANCELLED ──→ (30天后自动) ──→ ARCHIVED

任何状态的 ERROR 事件 ──→ ERROR
                            │
                      RETRY │ (max_retry_count=3)
                            ▼
                    last_active_state ──→ 恢复原流程
                            │
                      RETRY 第4次失败 ──→ ARCHIVED
                            │
                      (7天无操作自动) ──→ ARCHIVED
```

每个 Workflow 实例维护自己的状态，事件驱动其状态迁移。适合：审批流、任务流转、订单处理。

**守卫条件（guard）失败处理：**
- Guard 失败不等于异常：它是有意义的业务决策结果
- 每个 `guard` 必须定义 `on_fail`：通常是留在原状态，也可以跳转到特定状态

**终态回收：**
- 处于 `final_states` 的工作流实例是垃圾回收的候选
- `ARCHIVED` 状态的实例可被安全删除或迁移到冷存储
- 默认回收策略：ARCHIVED 状态停留超 30 天的实例 -> 归档到长期存储 -> 从活跃存储删除

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

depends_on:       # 依赖声明
  - name: "http-toolkit"
    version: ">=2.0 <3.0"    # SemVer 范围，而非 >=2.0

lifecycle:
  on_load: "init_db()"           # 加载时执行
  on_unload: "close_connections()" # 卸载时清理
  on_dependency_unloading: "stop_dependent_services()" # 依赖卸载前通知

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
        │ Loaded │──→ 1. 拓扑排序 + 环检测（加载前）
        └───┬───┘       2. 版本兼容性检查（SemVer 范围匹配）
            │            3. 如果依赖缺失 → 加载失败 + 明确错误消息
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
              │
              ▼
        通知依赖方 on_dependency_unloading → 卸载被依赖者
```

**插件依赖加载算法：**

```
1. 构建依赖图（DAG），每个节点 = 插件
2. 拓扑排序：如果存在环 → 加载失败，报告环路径
3. 按拓扑顺序加载每个插件
4. 加载时验证 SemVer 范围：运行时插件版本是否在声明的 range 内
5. 如依赖缺失或版本不匹配 → 加载整个链失败（不半加载）

卸载顺序（反向）：
1. 通知所有直接依赖方：on_dependency_unloading
2. 等待确认（硬超时 30s，可配置）
   ├── 所有依赖方在超时内确认 → 继续卸载
   └── 任一依赖方超时未确认 → 强制继续卸载 + 记录告警日志：
       \"插件 X 的 on_dependency_unloading 超时，已强制继续卸载\"
3. 卸载被依赖的插件

框架约束：
  - on_dependency_unloading_timeout_ms: 30000   # 超时硬编码默认值（可配置）
  - 主关闭（agent.shutdown）时，超时不阻止 Agent 退出
  - 超时监控：框架记录每个依赖方的响应耗时（用于健康检查仪表盘）
  - 连续 3 次卸载超时的插件自动标记为 \"unstable\"，下次热加载前拒绝加载
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
    
  - event_type: CRON_TICK
    source: "cron:daily-tasks"
    match: { payload.task: "report" }    # payload 级条件匹配（声明式路由，无需 Skill 内部 if/else）
    
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

**State Store 并发语义与隔离：**

```
StateStore {
    // 存储隔离
    isolation: namespace | physical   // 命名空间隔离——命名冲突保护，非安全隔离
                                      //   namespace：共享存储，key 前缀区分
                                      //     适用：防止 key 冲突，不提供安全边界
                                      //     ⚠ 不会阻止 scan(*) 等遍历操作
                                      //   physical：独立文件/表/桶
                                      //     适用：真正的安全隔离

    // 命名空间规则（仅 namespace 模式有效）
    namespace: "skill:{skill_name}"   // 自动添加 skill 名前缀
                                      // namespace 模式提供命名冲突保护（防止 key A 覆盖 key B）
                                      // 非安全隔离：支持 scan/iterate 的存储后端仍可遍历全局 key

    // 安全隔离模式（physical）
    // physical 模式下：
    //   - 每个 Skill 获得独立的存储空间（独立文件/数据库表/对象存储桶）
    //   - Skill A 没有任何接口可以访问 Skill B 的存储
    //   - 需要底层存储支持多个独立命名空间（如 SQLite 多个文件、S3 不同 prefix/bucket）

    // 权限控制层（namespace 模式下补充安全隔离）
    permissions: {
        scan: ["skill:{name}:*"]      // 自动限制 scan 到当前 Skill 命名空间
        read: "own_namespace"         // 默认只能读取自己的命名空间
        write: "own_namespace"
    }
    // 框架在 namespace 模式下自动追加 permissions 约束：
    //   - state_store.scan("*") -> 实际执行 scan("skill:{当前skill名}:*")
    //   - state_store.get("skill:other:x") -> 拒绝（除非在 shared 中声明）
    //   - 此约束是框架层的强制行为，不受插件/Skill 绕过
    // ⚠ 重要澄清：namespace 模式只防误操作，不防恶意攻击
    //   physical 模式才是真正的安全隔离

    // 并发控制
    write_consistency: last_write_wins | optimistic_lock | pessimistic_lock

    // 乐观锁接口
    put(key, value, expected_version?) -> Result
    // 如果传 expected_version，当前版本不匹配则失败（CAS 操作）

    // 读-写隔离
    read_level: read_committed       // 读已提交：不会读到未提交的写

    // 跨 Skill 共享
    shared: {
        "workflow:approval-123": { access: "readwrite" },
        "global:counter":        { access: "read" }
    }
    // 默认：其他 Skill 私有 Key 不可读
    // 显式声明共享的 Key 可配置访问权限 (read|readwrite)
}
```

**竞态条件示例与解决：**

```
场景：
  Skill A 处理事件 X： state_store.put("counter", 5)
  Skill B 处理事件 Y： state_store.put("counter", 10)
  （同时发生）

last_write_wins（默认）：
  → 结果取决于谁的写后到达。适用于：配置写入、非关键计数器

optimistic_lock：
  → A: put("counter", 5, expected_version=1) → 成功 (version→2)
  → B: put("counter", 10, expected_version=1) → 失败（版本不匹配）
  → B 必须重新读取新值后重试

pessimistic_lock：
  → A: lock("counter") → put("counter", 5) → unlock
  → B: 等待 A 释放锁 → put("counter", 10)
  → 适用于：长时间的关键操作
```

### 5.3 Skill 间协作

```
// 方式一：事件链
Skill A 执行后 publish(Event) → Event Bus → Skill B 响应

// 方式二：共享状态（通过 StateStore.shared 显式声明）
Skill A 写入 state_store → Skill B 读取（需在 shared 中声明权限）

// 方式三：工具共享
Skill A 和 Skill B 共用同一个 Tool

// 方式四：事件联合
两个 Skill 监听同一个事件，各自处理不同方面
```

---

## 6. Cron 定时任务系统

### 6.1 架构

Cron 系统本质上是一个**特殊的 Timer EventSource**，它：

1. 解析 cron 表达式（支持标准 5 字段 + 秒级 6 字段）
2. 在指定时间点向 Event Bus 发布 CRON_TICK 事件
3. 支持动态增删改定时任务（无需重启 Agent）

### 6.2 统一时区约定

```
核心原则：
  - 所有事件时间戳（Event.timestamp）统一使用 UTC Unix epoch 毫秒
  - 框架内部所有计算使用 UTC
  - timezone 仅在"计算下次触发点"时做时区转换
  - 显示层（日志、UI、通知）做时区转换

Cron 表达式中的时区：
  timezone: "Asia/Shanghai"
  → 框架将 "0 9 * * *" 在当前时区转换为 UTC 时间计算下次触发
  → 存储记录仍为 UTC

夏令时策略：
  Spring Forward（02:00→03:00）：
    - 02:30 的 cron 任务 → 该时间不存在 → 跳过
  Fall Back（02:00→01:00）：
    - 01:00 的 cron 任务 → 出现两次 → 默认只执行一次（标准时间优先）
    - 可配置 daylight_saving: skip | repeat_once | wall_clock

配置示例：
  cron_jobs:
    - id: "daily-report"
      schedule: "0 9 * * *"
      timezone: "Asia/Shanghai"
      daylight_saving: wall_clock  # 墙上时间优先（夏令时不变更触发时间）
```

### 6.3 Cron 配置

```yaml
cron_jobs:
  - id: "daily-report"
    schedule: "0 9 * * *"        # 每天 9:00（按 timezone 换算后）
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

### 6.4 运行时管理与安全守卫

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

// 安全守卫
CronSource 硬编码约束：
  - min_interval: "1s"       // 最小间隔硬限制，即使上层传更小值也要 clamp
  - max_interval: "365d"     // 最大间隔硬限制
  - rate_limit: 100          // 每秒最多产生 N 个 CRON_TICK 事件
  - dynamic_interval_auth: true  // reconfigure 需要鉴权

reconfigure 操作的权限模型：
  1. reconfigure 不是无防护的 API 端点
  2. 内部事件 SOURCE: "cron-manager", TYPE: "CONFIG_CHANGE" 才允许修改
  3. 所有 reconfigure 操作记录审计日志（old_interval, new_interval, caller, timestamp）
  4. 动态频率变更受速率限制：每个 cron job 每分钟最多变更 1 次
```

### 6.5 Cron 重启 Catch-Up 策略

Agent 重启或长时间停机后，错过的定时事件需要明确的回补策略。不同类型的 cron 任务有不同的 catch-up 需求。

```yaml
catch_up: skip | latest | all
  - skip（默认）：错过的不补跑，从下次触发时间开始正常调度
    - 适合：高频心跳类、状态轮询（skip 意味着"下次轮询自然会更新"）
  - latest：只执行错过的最近一次触发
    - 适合：日报、定时报告（只需要最新的一份结果）
  - all：全部补跑，但受 rate limit 约束
    - 适合：对时间敏感的数据采集、审计日志回填
    - 注意：多个错过事件同时涌入可能产生"微风暴"，框架自动施加 rate_limit（见 §6.4）

CronSource 重启恢复流程：
  1. 读取持久化的 last_trigger_timestamp
  2. 计算当前时间与 last_trigger_timestamp 之间的错过的触发点
  3. 按 catch_up 策略生成恢复事件
  4. 恢复事件继承 original_trigger_timestamp（在 payload 中附加）
  5. 即使是 all 模式，恢复事件的注入也受 rate_limit（每秒最多 N 个 CRON_TICK）

跨实例防重复：
  - 主备模式下，cron 事件应通过分布式锁或 leader 选举避免重复触发
  - 可选配置: leader_election: true | false（默认 false，单机无需设置）
  - 如果设置了 leader_election: true，只有持有锁的实例才产 CRON_TICK

TimerSource（固定间隔）与 CronSource（cron 表达式）的 catch-up 区别：
  - TimerSource 默认 catch_up: skip（固定间隔本身意味着"跳过错过的"）
  - CronSource 默认 catch_up: latest（cron 按时间点触发，通常希望补最近一次）
  - 两者均可通过 catch_up 配置覆盖默认值
```

### 6.6 运行时管理

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
    │ 等待 500ms 静默 → 文件未再变化 → 检查文件已关闭
    │ 注入 TraceID: "abc-123-def-456"
    │ publish(
    │   Event{
    │     id: "evt-001",
    │     type: FILE_CREATED,
    │     source: "watch:invoices",
    │     timestamp: 1714896000000 (UTC epoch ms),
    │     delivery: AT_LEAST_ONCE,
    │     dedup_key: "watch:invoices:FILE_CREATED:/watch/invoices/invoice-1024.pdf",
    │     payload: {path: "/watch/invoices/invoice-1024.pdf", size: 1MB},
    │     metadata: {
    │       trace_id: "abc-123-def-456",
    │       retry_count: 0,
    │       max_retries: 3,
    │       ttl_ms: 300000
    │     }
    │   }
    │ )
    ▼

[Event Bus]
    │ 去重检查：30s 窗口内相同 dedup_key 已存在？→ 否，入队
    │ 按优先级入队（NORMAL）
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
    │   ├── 成功 → 继续
    │   └── 失败（重试 3 次）→ 触发补偿
    │       ├── 补偿 Step 3: cleanup-temp-file
    │       └── 事件进入 Dead Letter Channel
    │ Step 5 (Output): publish(
    │   Event{
    │     id: "evt-002",
    │     type: INVOICE_PROCESSED,
    │     parent_event_id: "evt-001",
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
  type: in_memory                    # in_memory | persistent
  max_queue_size: 10000
  backpressure:
    strategy: layered                # layered | drop_lowest | block_all
    level1_threshold: 0.8            # 80% 满 → 降级 AT_MOST_ONCE 优先级
    level2_threshold: 0.9            # 90% 满 → 丢弃 AT_MOST_ONCE（记日志）
    level3_threshold: 0.95           # 95% 满 → 阻塞 poll
    level4_threshold: 0.98           # 98% 满 → 溢出 AT_LEAST_ONCE 到磁盘
    overflow_disk_path: "/var/lib/agent/overflow/"
  dedup:
    enabled: true
    window_ms: 30000
    strategy: window
  persistence:                       # 仅 persistent 模式
    wal_path: "/var/lib/agent/wal/"
    wal_sync: fsync
    checkpoint_interval: 500
    wal_rotate_bytes: 1073741824     # 1GB

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
      stability:
        debounce_ms: 500
        max_stable_wait_ms: 30000
        check_open_files: true

  - type: cron
    id: "daily-tasks"
    config:
      timezone: "UTC"
      daylight_saving: wall_clock
      min_interval_sec: 1
      jobs:
        - schedule: "0 9 * * *"
          timezone: "Asia/Shanghai"
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
      api_key: "${WEATHER_API_KEY}"     # 对标 Vault/Secret 管理
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
    pipeline:
      compensation_strategy: reverse_order
      compensation_contract: { idempotent: true, timeout_sec: 30, on_failure: compensation_failed }
      dlq:
        enabled: true
        max_retries: 3
        storage: "/var/lib/agent/dlq/"
        ttl_days: 30
        ttl_config: { pre_expiry_alert_days: [7, 3, 1], archive_on_expiry: true }

tools:
  - name: "ocr-extract"
    type: container
    config:
      image: "ocr-engine:latest"
      timeout: 120
    security:
      network: false
      max_memory: "1GB"

  - name: "slack-send"
    type: api
    config:
      endpoint: "https://slack.com/api/chat.postMessage"
      auth_token: "${SLACK_TOKEN}"       # 对标 Vault/Secret 管理

workflows:
  - name: "approval-flow"
    states:
      - pending
      - reviewing
      - approved
      - rejected
      - cancelled
      - error
      - archived
    initial: pending
    error_state: error
    final_states: [approved, rejected, cancelled, archived]
    error_recovery:
      retry_event: RETRY
      retry_to: last_active_state
      max_retry_count: 3
      on_retry_failure: archive
    state_timeouts:
      reviewing: { timeout: "7d", on_timeout: rejected }
      pending:   { timeout: "30d", on_timeout: cancelled }
      error:     { timeout: "7d", on_timeout: archived }
    transitions:
      - { from: pending,   event: SUBMIT,   to: reviewing, guard: hasPermission, on_fail: pending }
      - { from: reviewing, event: APPROVE,  to: approved }
      - { from: reviewing, event: REJECT,   to: rejected }
      - { from: ANY,       event: CANCEL,   to: cancelled }
      - { from: ANY,       event: ERROR_EVENT, to: error }
      - { from: error,     event: RETRY,    to: :last_active_state, guard: retryCount < 3, on_fail: archived }
```

### 9.2 敏感配置与密钥管理

**原则：** `${VAR_NAME}` 形式的配置在运行时从 Secret Store 解析，而非环境变量明文注入。

```
Secret 解析流程：
  Agent 启动时：
    1. 扫描所有配置中的 ${VARIABLE} 模式
    2. 按顺序尝试以下来源（可配置）：
       a. [推荐] 外部 Secret Store（Vault / AWS Secrets Manager / 1Password CLI）
       b. [次选] 加密的本地文件（~/.agent/secrets/）
       c. [最后] 环境变量（仅在开发环境允许）
    3. 解析失败 → Agent 拒绝启动（不泄露哪个变量缺失）

运行时安全：
  - Secret 仅在使用时从内存中读取，不在日志中输出
  - Agent 进程被 dump 时，Secret 在内存中也是加密的（仅在调用时解密）
  - 支持 Secret 热更新（重新解析 ${VARIABLE}）

Secret 热更新安全策略：

  ```
  热更新触发 → 扫描所有配置中的 ${VARIABLE} → 从 Secret Store 重新解析

  宽限期（Grace Period）：
    grace_period_sec: 60          # 默认 60s 宽限期，在这期间新旧密钥同时有效
                                  # - 已有的活跃连接/工具实例继续使用旧密钥
                                  # - 新建的连接/工具实例使用新密钥
                                  # - 超过宽限期后旧密钥不可用

  审计日志记录：
    - affected_keys:   ["SLACK_TOKEN", "WEATHER_API_KEY"]  # 受影响的密钥名列表
    - old_fingerprint: <sha256_hash_of_old_value>           # 旧密钥哈希（非明文）
    - new_fingerprint: <sha256_hash_of_new_value>           # 新密钥哈希（非明文）
    - timestamp:       <UTC_epoch_ms>
    - trigger_source:  "manual" | "scheduled_rotation" | "vault_callback"

  高影响 Secret 的两步提交策略（推荐用于数据库连接串等）：
    1. 发布 \"secret:about_to_change\" 事件到 Event Bus
    2. 等待所有活跃 Tool 完成当前执行或超时
    3. 等待确认窗口内无活跃连接引用旧密钥
    4. 执行实际切换
    5. 发布 \"secret:changed\" 事件通知已切换完成

  数据库连接串特殊处理：
    - 采用连接池级别滚动更新：已有连接使用旧串直到自然释放，新连接使用新串
    - 避免连接池风暴（所有连接同时断开并重连）
    - 连接池 drain 超时（连接_drain_timeout_sec: 30）后强制关闭旧连接
  ```
```

### 9.3 运行时接口

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

GET  /events/trace/{trace_id}  # 按 TraceID 追踪事件链路
GET  /events/dump/{id}         # 导出事件详细信息（含 metadata）
GET  /dlq                      # 查看死信队列
POST /dlq/{id}/retry           # 手动重试死信事件
POST /dlq/{id}/discard         # 确认丢弃死信事件

GET  /audit-log                # 审计日志（配置变更、权限操作、事件丢弃）
```

**控制接口安全守卫：**
安全约束：
- 默认绑定 localhost/Unix socket，暴露到网络必须配置认证（API Token / mTLS / OAuth2）
- 敏感操作（shutdown、disable plugin、dlq retry）需要二次确认 + 操作审计日志
- `POST /inject-event` 生产环境默认禁用，需 `force_enable_debug_endpoints: true` 显式开启

---

## 10. 设计决策记录

### 决策 1：事件优先于调用

选择了"一切皆事件"而非"模块间直接调用"。

- **理由**：模块间直接调用会引入紧耦合，违反"只通过事件通信"的原则。事件解耦使得任意模块可以添加、移除、替换而不影响其他模块。
- **代价**：增加了事件序列化和反序列化的开销，且调试时需要追踪事件流。
- **缓解**：TraceID 强制注入 + 事件链路树 + 调试模式记录完整事件流。

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

### 决策 6：交付保证与 priority 正交

`delivery` 字段与 `priority` 字段是正交的，独立作用于不同生命周期阶段。

- **理由**：低优先级 ≠ 低重要性。一个"低优先级"事件可能只是业务上的非实时要求，但数据本身不可丢。priority 只影响队列调度顺序，delivery 决定背压时的丢弃策略。
- **对比**：传统设计只有 priority，导致低优先级=可丢，隐含了错误的等价关系。

### 决策 7：TraceID 框架强制注入

TraceID 不由插件或 Skill 决定是否设置，而是框架层在每个事件产生时自动注入。

- **理由**：可观测性是所有运维的基础设施，不是可选项。可选 TraceID 意味着总有某些事件链路不可追踪。
- **代价**：每个事件多了 ~128 字节的固定开销。
- **缓解**：TraceID 只在调试/审计时读取，不参与主路径判断，性能影响可忽略。

### 决策 8：Pipeline 补偿采用 Saga reverse_order + 补偿失败升级路径

Pipeline 失败后的补偿默认采用反向顺序执行（C_N → C_(N-1) → ... → C_1）。

- **理由**：Saga 是分布式事务补偿的标准模式。反向顺序保证依赖顺序正确（后创建的副作用先清理）。
- **对比**：没有补偿 → 孤儿资源泄漏。全局 rollback（类似 2PC）→ 复杂度高，非每个 Tool 支持。
- **代价**：补偿本身可能失败（如已删除的 Slack 消息无法再删除），部分成功 + 部分失败 = **半回滚状态**。
- **缓解**：补偿操作强制幂等 + 独立超时（30s）+ 任一失败进入 `COMPENSATION_FAILED` 中间态（非终态）→ 人工接管。

---

## 11. 已知风险与兜底

### 风险定级矩阵

| 等级 | 影响范围 | 发生概率 | 检测难度 |
|------|---------|---------|---------|
| 高 | 数据丢失 / 资金损失 / 系统瘫痪 | > 5% / 必然触发 | 运行时可能无感知 |
| 中 | 业务中断 / 功能异常 / 局部故障 | 1%-5% | 需要特定条件触发 |
| 低 | 性能下降 / 错误信息不友好 | < 1% | 容易发现 |

### 风险清单

| # | 风险 | 等级 | 触发条件 | 爆发后果 | 应对策略 |
|---|------|------|---------|---------|---------|
| 1 | **事件风暴**：高吞吐事件源短时间内涌入大量事件 | 高 | 文件批量修改 / Webhook 突发 / DDoS | 队列满 → 事件丢失 / 系统 OOM | 分层背压（Level 1-5）+ AT_MOST_ONCE 可丢 + AT_LEAST_ONCE 溢出磁盘 |
| 2 | **循环事件**：事件 A → 处理 → 事件 B → 处理 → 事件 A（死循环） | 中 | 处理逻辑中产生了与触发事件相同的事件 | 无限循环耗尽资源 | TTL + 最大传递次数 + 循环检测规则（同 source+type 的历史 N 次） |
| 3 | **单点故障**：Event Bus 或主事件循环崩溃 | 中 | 进程 crash / OOM / 硬件故障 | 处理中断 → 未确认事件丢失 | WAL 模式 + checkpoint 恢复 + 多实例主备切换（需同步队列偏移 + Workflow 状态 + 进行中的 Future） |
| 4 | **插件故障**：不稳定的第三方插件拖垮整个 Agent | 高 | 插件内死循环 / OOM / 无限等待 | 主事件循环卡死 | 子进程/容器隔离 + 超时保护 + 看门狗自动重启 + 插件级健康检查 |
| 5 | **状态丢失**：Workflow 状态丢失导致业务异常 | 中 | 持久化失败 / 恢复时数据损坏 | 工作流状态不一致 | State Store 持久化 + 快照 + 预写日志 + 定期的状态完整性校验 |
| 6 | **资源泄漏**：Tool/Plugin 未正确释放资源 | 中 | 异常退出 / 超时未清理 | 临时文件堆积 / 连接泄漏 | 每个 Tool 有超时和资源配额 + Plugin on_unload 钩子 + 运行时资源监控 + 兜底清理守护 |
| 7 | **幂等失效**：AT_LEAST_ONCE 事件重复交付导致重复处理 | 中 | 队列重放 / WAL 重放 / 网络重传 | 重复通知 / 重复写入 / 重复扣费 | 去重窗口（30s dedup_key）+ 处理器幂等契约 + processed_events 集合 |
| 8 | **数据完整性**：文件刚写入到一半触发 FILE_CREATED | 中 | 大文件写入 / 网络文件系统 | 读到不完整的文件 → 处理结果错误 | FileWatchSource 稳定确认机制（debounce + 文件锁检测） |
| 9 | **配置泄露**：敏感配置通过环境变量明文注入 | 中 | 进程被 dump / 日志输出 / 调试信息 | API Key / 令牌泄露 | Secret Store 解析 + 内存加密 + 日志过滤 |
| 10 | **审计缺失**：事件链路无法追查 | 低 | 模块未正确传递 TraceID | 故障时无法定位问题根因 | TraceID 框架强制注入 + EventLink 链路树 + 不可变审计日志 |
| 11 | **Cron 自 DDoS**：动态频率被配置为极短间隔 | 低 | 配置错误 / 恶意 API 调用 | 每秒涌入大量 CRON_TICK → 级联故障 | min_interval 硬编码守卫 + reconfigure 鉴权 + 变更速率限制 |
| 12 | **跨时区定时错乱**：夏令时导致 cron 任务执行异常 | 低 | 夏令时转换日 / 容器时区与宿主机不一致 | 定时任务漏执行或重复执行 | UTC 内部统一 + 明确的夏令时策略配置 |
|| 13 | **插件循环依赖**：插件 A 依赖 B，B 依赖 A | 中 | 插件配置错误 | 加载死锁 / 栈溢出 | 加载前拓扑排序 + 环检测 + 版本兼容性检查 |
|| 14 | **补偿操作失败**：Pipeline 部分补偿成功 + 部分失败 → 半回滚状态 | 🔴 高 | 补偿操作本身失败（文件被占用、网络超时） | 数据一致状态不可知，无法自动恢复 | 补偿操作强制幂等契约 + 补偿超时保护 + COMPENSATION_FAILED 中间态 + 人工接管告警通道 |
|| 15 | **DLQ 到期事件静默消失**：TTL 到期后未处理事件直接删除 → 操作员不知曾发生过 | 🟡 中 | 操作员休假 / DLQ 无读者 | 关键事件永久丢失 | TTL 到期前 7d/3d/1d 分级告警 + 到期事件归档冷存储而非直接删除 + 定期 DLQ 摘要通知 |
|| 16 | **ERROR 状态静默归档**：Workflow ERROR 后 7 天自动转入 ARCHIVED → 无告警 | 🟡 中 | `state_timeouts` 默认行为 | 业务异常被自动归档，操作员无感知 | ERROR 状态 `on_enter` 默认触发告警 + `ERROR→ARCHIVED` 前发送"即将归档"告警 |
|| 17 | **parent_event_id 循环链路**：事件 A→B→A'→B' 形成无限链路 | 🟡 中 | 处理逻辑中产生了能重新触发自身的事件链 | 链路追踪工具无限循环、排查困难 | 框架层检测 parent_event_id 链中重复 event_id + Trace API 返回时截断循环支路并以标记提示 |
||| 18 | **控制接口未认证**：运行时控制接口无任何认证/授权定义 | 🟡 中 | 控制接口暴露到网络（0.0.0.0/Docker 映射） | 任意第三方可 shutdown agent、禁用插件、伪造事件注入 | 接口默认绑定 localhost + 敏感操作认证（API Token / mTLS）+ 操作审计 + `/inject-event` 生产环境默认禁用 |
|| 19 | **ERROR 状态无恢复路径**：Workflow ERROR 后 operator 无法恢复，只能等待归档另起新实例 | 🔴 高 | 临时故障（网络抖动/DB 短暂不可用/第三方 API 自愈）进入 ERROR | 临时故障的工作流无法复活，需要人工另起实例重走全部流程 | ERROR 恢复路径：RETRY 事件 + last_active_state 保存 + max_retry_count 上限 + 重试超限自动归档 |
|| 20 | **Cron 重启微风暴**：Agent 重启后错过多次 cron 触发，全部补跑瞬间涌入大量事件 | 🔴 高 | Agent 长时间停机（升级/维护/故障）后重启 | 错过次数 × 补跑事件 → 事件风暴 → 队列满/背压触发 | catch_up 策略（skip | latest | all）+ all 模式受 rate_limit 约束 + 恢复事件注入限速 |
|| 21 | **溢出磁盘满导致数据丢失**：背压 Level 4 溢出到磁盘，但磁盘空间有限，满后溢出失败 | 🟡 中 | 事件风暴持续 → 队列 long-term 满 → 溢出目录占满磁盘 | AT_LEAST_ONCE 事件无处可去 → 只能丢弃或阻塞 | Level 4B 溢出目录 ≥80% 告警 + 回退到 Level 3 阻塞 + overflow_max_bytes 硬上限 + 建议 WAL 和溢出目录独立磁盘分区 |
|| 22 | **插件卸载依赖等待死锁**：on_dependency_unloading 依赖方响应无限等待，阻塞整个卸载链 | 🟡 中 | 依赖方 on_dependency_unloading 钩子实现有 bug（等待永不返回的外部队件） | 卸载线程卡死 → 插件 B 无法卸载 → 主关闭阻塞 → Agent 无法优雅关闭 | on_dependency_unloading 硬超时 30s + 超时强制卸载 + 日志告警 + 连续 3 次超时标记 unstable |
|| 23 | **Secret 热更新竞态**：密钥轮换时，正在执行的长 Tool 中途密钥变更致调用失败 | 🟡 中 | 运行中触发 Secret 热更新（API Key 轮换/数据库连接串变更） | 长 Tool 在密钥变更后调用失败 / 连接池风暴 / 审计日志密钥混乱 | grace_period_sec 宽限期 + 连接池滚动更新 + 两步提交策略 + 审计日志记录新旧 key 指纹 |
|| 24 | **State Store 命名空间虚假安全隔离**：namespace 模式声明"无法枚举"，实际 scan(*) 可遍历全局 key | 🟡 中 | namespace 模式隔离但后端支持 scan/iterate 操作（Redis、etcd、文件系统） | 开发者以为 Skill 数据是隔离的，实际可被枚举 | 诚实修正声明（命名冲突保护 vs 安全隔离）+ framework 层自动追加 scan 权限约束 + physical 模式用于真正隔离 |
|| 25 | **Pipeline 并发竞态**：同一 Pipeline 收到多个事件，无并发定义导致 State Store 覆盖 / 文件冲突 | 🟡 中 | Pipeline "invoice-processor" 同时收到多个 FILE_CREATED 事件 | State Store last_write_wins 覆盖 / 临时文件相互覆盖 / 数据损坏 | concurrency 配置（serial | parallel | limited(N)）+ parallel 模式强制 optimistic_lock + 独立临时目录 |
|| 26 | **WAL→内存队列投递缺口**：WAL 已确认持久化但内存队列拒绝接收，事件卡在两者之间 | 🟢 低 | 持久化模式 + Event Bus 队列满（背压 Level 2+） | 事件已持久化但从未被处理（延迟到下次重启才重放） | 待重试队列（指数退避 100ms→500ms→2s）+ 积压告警 + 重启自动检查待重试队列 + 恢复检查清单新增项 |

### RPO / RTO 目标

```
持久化模式下：

RPO（Recovery Point Objective） — 可丢失多少数据？
  - AT_MOST_ONCE 事件：可丢失任意数量（设计意图）
  - AT_LEAST_ONCE 事件：最多丢失 checkpoint_interval 个事件（默认 500）
  - EXACTLY_ONCE 事件：零丢失
  - Workflow 状态：零丢失（每次状态变更同步写 WAL）

RTO（Recovery Time Objective） — 多快恢复？
  - 单进程崩溃：< 30s（WAL 重放 + checkpoint 恢复）
  - 主备切换：< 60s（含故障检测 + 状态同步 + 就绪检查）
  - 数据损恢复（需人工介入）：< 4h（DLQ 回放 + 数据校验）

恢复后检查清单：
  □ 事件队列偏移量已恢复
  □ 全部 Workflow 实例状态已恢复（含 ERROR → 检查能否 RETRY 恢复）
  □ 进行中的 Tool 执行已超时/cancel
  □ 插件状态已恢复
  □ DLQ 内容未丢失
  □ Cron/Timer catch-up 事件已按策略注入（检查 last_trigger_timestamp）
  □ 溢出目录（overflow/）已扫描重放
  □ 待重试队列（retry_queue）已清空
  □ WAL 中无积压的已确认未投递事件
```

---

## 12. 与现有 Agent 框架的异同

| 特性 | 本设计 | Hermes Agent | CrewAI | LangChain |
|------|--------|-------------|--------|-----------|
| 核心范式 | **事件驱动** | 聊天循环 + Cron | 多 Agent 协作 | Chain / Agent 调用 |
| 事件源 | 一等公民（文件/网络/定时器/Webhook） | 仅 Cron/Timer | 无内建 | 无内建 |
| Skills | 基于事件的响应单元 | 基于 Skills 目录 | 基于 Task | 基于 Tool |
| Plugins | 热插拔 + 依赖管理 + 环检测 | 插件钩子系统 | 无 | 无 |
|| Workflow | 内建 State Machine + 超时/错误/归档态 + ERROR 恢复路径 | 无 | 流程驱动 | 链式调用 |
| Tools | 多种执行模式 + Sandbox | 工具系统 | 工具注册 | 工具调用 |
| 配置 | 声明式 YAML | YAML | Python | Python |
| 交付保证 | AT_MOST_ONCE / AT_LEAST_ONCE / EXACTLY_ONCE | 无 | 无 | 无 |
| 失败处理 | Saga 补偿 + DLQ | 无 | 无 | 无 |
| 可观测性 | TraceID 强制注入 + 事件链路树 | 无 | 无 | 部分 |

**本设计的关键差异化**：它不是"以 Chat 为中心"的 Agent，而是"以事件为中心"的 Agent。传统 Agent 等你问问题，事件驱动 Agent 不等任何人——它在持续观察世界、响应变化。

---

## 13. 设计原则总结

```
1. 万物皆事件 —— 所有外部输入以统一的事件模型抽象
2. 响应即行为 —— Agent 不做"主动"，只做"响应"
3. 松耦合 —— 模块间只通过 Event Bus 通信
4. 可观测 —— 事件流可追踪、可记录、可回放（TraceID 强制注入）
5. 可演化 —— 插件热插拔 + 依赖管理，Skill 热加载，配置热更新
6. 安全边界 —— 插件和工具按信任等级隔离，Secret Store 管理密钥
7. 务实优先 —— 简单场景用 Pipeline，复杂场景用 State Machine
8. 交付保证正交 —— priority 管调度顺序，delivery 管丢弃策略
9. 失败有补偿 —— Pipeline 有 Saga，重试有上限，终极有 DLQ
10. 可恢复 —— WAL + checkpoint + RPO/RTO 定义
```
