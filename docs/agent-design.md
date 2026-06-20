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

## 2.5 Agent 生命周期（启动顺序）

Agent 启动时必须按明确定义的阶段序列初始化各组件，以确保依赖关系正确、事件不丢失、健康检查不误报。

### 2.5.1 启动阶段

```
Phase 0 [基础设施] ── Event Bus 初始化（内存/持久化） → 背压系统就绪
Phase 0.5 [密钥解析] ── 扫描所有配置中的 ${VARIABLE} 模式 → Secret Store 解析
                          ⚠ 必须在 Phase 2（组件注册）之前完成，因为插件加载需要 Secret
                          ⚠ Secret Store 不可用时的重试策略：
                             重试次数（secret_retry_count: 3，可配置，默认 3 次）
                             退避间隔（retry_backoff: "sequence:2s,5s,15s"，可配置）
                             每次重试输出进度日志："Secret Store 不可用，正在重试 (N/M)..."
                             所有重试用尽后才进入"拒绝启动"路径
                             可选降级：若配置了缓存代理（如 Vault Agent Sidecar），
                             允许 secret_cache_fallback: true 读取本地缓存作为临时降级
                             ⚠ secret_cache_fallback 安全约束：
                               - 存储加密：缓存文件必须加密存储（AES-256-GCM），
                                 密钥绑定到 Agent 实例（TPM/HSM key wrapping 或启动时获取的一次性密钥）
                               - 文件权限：默认 600（仅 Agent 进程可读写）
                               - 缓存 TTL：可配置 secret_cache_ttl_sec，默认 300s（5 分钟），
                                 与常见 Vault token TTL 对齐
                               - 进程 dump 保护：缓存文件不存储明文，读取后立即在内存中解密
                                 ——与\"解析结果缓存在内存加密区\"策略一致
                               - Phase 1 可用性：声明 secret_cache_fallback 在 Phase 1
                                 （WAL 恢复阶段）不可用——插件加载（Phase 2）前
                                 Secret Store 必须可达
                          ⚠ 解析结果缓存在内存加密区，仅使用时解密
Phase 1 [持久化恢复] ── WAL 校验 → checkpoint 加载 → 待重试队列重建
                          ⚠ 此阶段不产生新事件，仅恢复持久化状态
Phase 2 [组件注册] ── 插件加载（拓扑序 + 环检测）→ Skill 注册 → Dispatcher 路由注入
                          ⚠ 所有注册必须在 Phase 3 之前完成，确保事件处理器就绪
Phase 3 [状态恢复] ── Workflow 实例从 State Store 加载
                          ⚠ Dispatcher 路由已就绪，Workflow 状态可立即接收转移事件
                          ⚠ 超时约束：workflow_recovery_timeout: 120s（可配置，默认 120 秒）
                             超时行为：已恢复的实例提交 checkpoint，未恢复的实例标记为\"下次恢复\"，
                             避免 Agent 在 Phase 3 无限卡死
                          ⚠ 恢复进度通过日志/metrics 输出：\"Workflow 恢复进度: X/Y 实例\"
Phase 4 [源激活] ── Event Source 注册 → 文件监听/Timer/Cron/Webhook 启动
                          ⚠ Timer/Cron 在此阶段才开始产生事件，避免"处理器未注册先有事件"
Phase 5 [就绪] ── 控制接口开放 → health endpoint 返回 HTTP 200 ready
```

**核心安全约束：**
- Phase 2（组件注册）必须在 Phase 4（源激活）之前完成——防止 Timer/Cron 在 Skill/Dispatcher 就绪前产生事件
- WAL 重放（Phase 1）产生的恢复事件不进入 Event Bus 主队列，而是暂存在内部缓冲区中
  - 缓冲区大小上限：`wal_replay_buffer_max: 5000`（可配置，默认 5000 个事件）
  - 超限行为：缓冲区满后 WAL 重放暂停，Phase 1 标记为"部分完成"
    - 已读取的事件在 Phase 2 完成后正常注入 Event Bus
    - 未读取的事件记录断点偏移量，下次启动时从断点继续重放
    - 断点持久化方式：
      - 保存在 WAL 所在目录的独立状态文件：`{wal_path}/replay_checkpoint`
      - 每次缓冲区满暂停时，同步写入偏移量（fsync 确认持久化）
      - Phase 1 启动时：先读取 `replay_checkpoint` → 从该偏移量开始重放
      - 如果 `replay_checkpoint` 不存在或损坏 → 退回到从 WAL 头部（即 checkpoint 记录的偏移量）开始重放
      - Phase 2 完成后（缓冲区成功注入 Event Bus）→ 删除 `replay_checkpoint` 文件
    - 记录告警日志：`"WAL 恢复缓冲区已满（上限 5000），N 个事件将在下次启动时继续重放"`
  - Phase 2（组件注册）应在合理时间内完成。如果 Phase 2 超时（`plugin_load_timeout: 30s`，可配置），视为启动失败，触发紧急告警
  - 此设计避免了"WAL 重放先于插件加载 → 事件发给不存在的处理器"的竞态
- Phase 3 的 Workflow 状态恢复必须检查恢复后的状态是否有对应的处理器存在
  - 如果 Workflow 转移事件对应的 Skill/Plugin 已不存在 → 标记为 UNRECOVERABLE → 告警 + 人工接管

### 2.5.2 Readiness Probe

| 端点 | 用途 | Phase 5 前 | Phase 5 后 |
|------|------|-----------|-----------|
| `GET /health/live` | 存活检查（进程是否存活） | HTTP 200（只需进程启动） | HTTP 200 |
| `GET /health/ready` | 就绪检查（是否可接收流量） | HTTP 503 Service Unavailable | HTTP 200 |
| `GET /health` | 兼容端点（= ready） | HTTP 503 | HTTP 200 |

Load balancer / orchestrator 应将 `GET /health/ready` 作为就绪探针，而非 `GET /health`。

### 2.5.3 优雅关闭顺序（反向）

```
Phase 5 [停止接收] ── 控制接口关闭 → 负载均衡移除该实例
Phase 4 [源停止] ── Event Source 关闭 → Webhook 返回 503 → Timer/Cron 停止
Phase 4.5 [排水] ── Pipeline/Skill inflight 执行等待：
    ├── 通知所有活跃 Pipeline/Skill 实例：shutdown 即将到来，完成当前步骤后停止
    ├── 等待正在执行的 Tool 返回（drain_timeout_sec: 30，默认可配置）
    ├── 如果正在进行补偿：等待补偿完成或超时
    ├── 超时仍未完成的实例 → 记录警告日志（含 trace_id + 当前步骤 + 已补偿/未补偿状态）→ 强制终止
    └── 待重试队列进入停止重试模式
         ├── 允许已调度但尚未执行的重试继续执行（属于 inflight 工作，由排水等待覆盖）
         ├── 不产生新的重试调度
         └── 排水超时后，队列中所有未执行的重试事件标记为"shutdown_abandoned"，
             在重启后的启动序列中（Phase 1 待重试队列重建时）重新入队列 —— 不静默丢弃，符合 AT_LEAST_ONCE 原则

排水超时与 Tool 自身超时的交互（⚠ 关键规则）：
  - 排水超时（drain_timeout_sec: 30）独立于 Tool 自身超时工作，两者取其先
  - 如果 Tool 的 timeout < drain_timeout_sec：Tool 自身超时先触发，执行 Tool 层自我清理逻辑
  - 如果 Tool 的 timeout > drain_timeout_sec：排水超时先触发，框架强制终止 Tool
  - **无论哪种方式，Tool Runner 的 Step 6（清理 → 释放临时资源）必须执行**
  - 框架级清理和 Tool 级清理是独立的层次：框架保证临时目录/文件描述符等资源释放，
    Tool 层保证业务级清理（如通知外部系统\"操作取消\"）
Phase 3 [状态持久化] ── Workflow 活跃实例落盘 → State Store checkpoint
Phase 2 [组件卸载] ── 插件卸载（反向拓扑序）→ Skill 反注册 → Dispatcher 清空
Phase 1 [WAL 刷盘] ── 待重试队列清空 → WAL checkpoint 最终写入
Phase 0 [基础设施关闭] ── Event Bus 关闭 → 背压系统关闭
```

**关于关闭过程中断的 Pipeline/Skill：**
- 关闭中断的 Pipeline 不会被自动恢复（Pipeline 无持久化状态，不同于 Workflow）
- 如果 Pipeline 执行的操作有副作用，依赖 Tool 层的**幂等性**保证重复安全性
- 对于 mid-compensation 中断，排水阶段已记录详细的补偿状态日志（"step X 已补偿，step Y 未补偿"），供人工恢复时参考
- 开发者应在 Tool 层设计时确保：关闭后重启时重复执行同一操作不会产生负面影响

### 2.5.4 生命周期入口/出口边界规则

启动过程中收到 shutdown 信号时的行为：

```
shutdown 信号在 Phase 0~3（就绪前阶段）到达 → 立即进入关闭序列（从当前 Phase 的关闭等价阶段开始）：
  - Phase 0 [基础设施初始化中]：直接中断（Event Bus 还没完备，无资源需要清理）
  - Phase 1 [WAL 恢复中]：
    - 已读取到缓冲区的事件标记为 shutdown_abandoned（同排水阶段规则）
    - replay_checkpoint 文件保留（下次启动从断点继续）
    - 未读取的 WAL 部分：下次 Phase 1 启动时从 replay_checkpoint 继续
  - Phase 2 [组件注册中]：
    - **全加载的插件**（on_load 已完成，业务资源已就绪）：按反向拓扑序走卸载流程（on_unload + on_dependency_unloading）
    - **半加载的插件**（拓扑排序+版本检查已通过，但 on_load 执行中或未开始）：
      - 不调用 on_unload（函数引用尚未注册，调用会访问未初始化内部状态）
      - 资源回收策略（按隔离模式区分，见 §4.4）：
        - **子进程/容器模式**：OS 自动回收子进程/容器资源（文件描述符、内存）—— 进程退出即清理
        - **进程内模式（默认）**：
          - 框架必须主动追踪插件已分配的资源（on_load 开始前注册资源追踪句柄）
          - 中断时：释放已追踪的文件描述符、关闭已打开的 DB 连接、释放已分配的内存
          - 建议：Plugin 的 on_load 实现应使用框架提供的资源追踪 API（如 `context.track_fd()`、`context.track_db()`），使框架可在中断时自动清理
          - 如果 on_load 未使用资源追踪 API：记录警告日志，依赖插件作者的 on_load 幂等性和可重入性保证
        - **WASM 模式**：由 WASM 运行时沙箱回收
      - 记录告警日志：`"插件 X 在 on_load 阶段被中断，on_unload 跳过"`
    - 未加载的插件（尚未到达拓扑排序阶段）：跳过（从未初始化，无需清理）
  - Phase 3 [Workflow 状态恢复中]：
    - 已恢复的 Workflow 实例写入 State Store checkpoint（可用当前状态）
    - 未恢复的实例从 State Store 的 last_checkpoint 恢复（下次启动时）
shutdown 信号在 Phase 4 [源激活中]到达 → 等同于正常关闭序列（Phase 5→4→4.5→3→2→1→0），
  因为 Phase 4 已接近就绪，源已开始激活但控制接口未开放
shutdown 信号在 Phase 5 [已就绪]到达 → 正常关闭——这是唯一已定义的路径
```

**replay_checkpoint 文件在 shutdown 后的状态：**
- 如果 shutdown 发生在 Phase 0~1（WAL 恢复阶段）：replay_checkpoint **保留**（下次启动继续断点）
- 如果 shutdown 发生在 Phase 2~5（恢复已完成）：replay_checkpoint **不存在或已被删除**（Phase 2 完成后已删除）
- 如果在 Phase 1 中 WAL 恢复已完成（缓冲区未超限）：replay_checkpoint 从未创建 → 等同于不存在

shutdown_abandoned 事件与 WAL checkpoint 的 offset 关系（⚠ 关键约束）：
- shutdown_abandoned 事件来源于**待重试队列**，与 WAL 偏移量正交
  - 待重试队列中的事件已经通过 WAL 确认（成功写入 WAL 但未送达处理器）
  - WAL checkpoint 追踪的是"已写入 WAL + 已确认处理"的偏移量
- 因此 shutdown_abandoned 事件不会出现在 WAL checkpoint 之后重放的范围中
  - 它们是在 WAL checkpoint 之前的已确认但未处理事件
  - Phase 1 重建时：如果同一事件同时出现在 WAL 重放和 shutdown_abandoned 列表中 → 去重机制（dedup_key）自动过滤

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
                       // ⚠ 缺省值涉及 payload_hash，对大型 payload 有可观的 CPU 开销：
                       //   - AT_MOST_ONCE 事件：框架应跳过 dedup_key 计算（不保证去重，计算浪费）
                       //   - 已知不会重复的事件：建议显式设置轻量 dedup_key（如 event.id 或 source+type+timestamp）
                       //   - UUID v7 标识的事件天然唯一：设置 dedup_key: <event.id> 避免 hash
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
                                    // ⚠ 预留接口声明，当前版本(v1.0)未实现自动清理
                                    //    计划 v2.0 实现：框架注册调度清理器追踪临时资源
                                    //    当前开发者需在 compensate/on_final 中自行清理
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
    backpressure_signal(level: int)  // 背压级别通知
                                     // Push 来源：收到信号后应暂停 publish() 并返回 503
                                     // Pull 来源：由框架自动阻塞 poll()，无需额外实现

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

> **信任等级：** ChatPlatformSource 等用户输入事件源默认 `trust_level: untrusted`（参见 §9.3 LLM 注入防护）。
> 所有 MESSAGE_RECEIVED 事件的 payload 在传递给 LLM-based Skill 前应经过输入消毒。

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
  check_open_files: auto               # 是否检查文件是否仍被打开（auto 自动检测 FS 类型）
  force_publish_on_timeout: mark_incomplete  # mark_incomplete | publish_anyway | none
```

> **文件锁检测的局限性：** 上述稳定确认机制依赖本地文件系统特性。在 NFS/CIFS/SMB/FUSE 等远程或虚拟文件系统上，文件锁（flock）支持有限，`lsof` 跨网络可能无法检测到远端打开句柄。云对象存储挂载（s3fs/gcsfuse）没有传统"打开文件"概念。在这些环境中：
> - `check_open_files: true` 可能始终返回"文件未打开" → 锁检测失效 → debounce 完成后直接发布，不额外等待确认
> - 或锁检测误报 → 触发 `incomplete: true` 强制发布的频率增高
> - **建议**：在远程文件系统上适当增大 `debounce_ms`（如 2000ms）和 `max_stable_wait_ms`（如 60000ms），补偿锁检测缺失带来的不确定性
>
> `check_open_files` 配置支持三值：`auto | true | false`（默认 `auto`）
> - `auto`：自动检测文件系统类型，本地 FS 启用锁检测，远程/虚拟 FS 跳过
> - `true`：强制启用（仅在确认底层 FS 支持锁检测时使用）
> - `false`：强制禁用（纯 debounce 模式）

### 3.3 Event Bus（事件总线）—— 框架的中枢神经系统

```
Event Bus {
    // === 核心功能 ===
    publish(event)                // 发布事件（异步非阻塞）
    subscribe(filter, handler)    // 订阅事件（按类型/来源/内容匹配）
    wait_for_event()              // 异步等待新事件到达（不取走），用于 select! 并发模式
                                  // 保证：仅队列从空→非空时触发通知（edge-triggered），无假唤醒

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
| Level 3 [95% 满]  → 阻塞所有事件源的 poll() 调用
|                      → 通知所有 Push 事件源（backpressure_signal(3)），暂停 publish()
|                      → Webhook HTTP 来源返回 503 Service Unavailable
| Level 4A [98% 满] → 溢出 AT_LEAST_ONCE 事件到磁盘缓冲区（overflow_to_disk），溢出目录设硬上限（overflow_max_bytes）
| Level 4B [溢出目录≥80%满] → 触发紧急告警 → 回退到 Level 3（阻塞 poll + 暂停 Push），不再溢出到磁盘
|                              离开条件：溢出目录使用率降至 ≤50%（含滞回，防止振荡）
| Level 5 [100%满]  → 触发紧急模式：停止低优事件源，发系统告警

关键原则：
- AT_MOST_ONCE 事件在任何级别都允许丢，但必须记日志
- AT_LEAST_ONCE 事件在 Level 3 阻塞，Level 4A~4B 尝试溢出到磁盘（失败则退回阻塞），绝不静默丢弃
- EXACTLY_ONCE 事件不可丢、不可溢出——发布者必须等待
- **Push 来源同步原则**：Level 3+ 时 Push 来源的 `publish()` 与 Pull 来源的 `poll()` 同等阻塞
  - Webhook 来源暂停接收 → 返回 503 Service Unavailable
  - Socket 来源暂停读取 → 接收缓冲区堆积，框架层丢弃老数据（TcpUserTimeout）
  - 消息队列消费者暂停拉取（reconnect_on_resume）
- 所有丢弃事件统一写入丢弃日志（包含 event.id, source, type, reason, timestamp）

溢出磁盘管理：
  overflow_max_bytes: 1073741824       # 1GB 硬上限，达到 80% 触发提前告警
  overflow_warn_threshold: 0.8          # 进入 Level 4B 的阈值（≥80%）
  overflow_hysteresis_leave: 0.5       # 离开 Level 4B 的阈值（≤50%），避免振荡
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

**优先级与保序的冲突规则（⚠ 关键权衡）：**
同一 Event Source 的事件：**保序优先于优先级**
  - 优先级在同来源事件之间**仅跨来源场景生效**（Source A 的 HIGH 先于 Source B 的 NORMAL）
  - HIGH 事件不会跳过同一来源的已排队 NORMAL 事件
  - 示例：同一 FileWatchSource 先后产生 NORMAL("删除文件") 和 HIGH("修改文件") → 先 NORMAL 再 HIGH（顺序不变）
  - 如需突破此约束：使用**不同 Event Source** 发出不同优先级的消息（如 FileWatchSource 产 NORMAL，TimerSource 产 HIGH）

跨 Event Source 的事件：**优先级正常生效**
  - Source A 的 HIGH 事件先于 Source B 的 NORMAL 事件处理
  - 但每条来源内部保持顺序

此规则在设计决策 9 中有正式记录。

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
                      重试间隔：100ms → 500ms → 2s（序列退避，最大重试 5 次）
                      可配置：wal_retry_backoff: "sequence:100ms,500ms,2s"（默认值）
                                    ↓
                       ┌─── 重试成功 → 正常入队处理
                       └─── 持续失败（5 次后）→ 触发"事件积压"告警
                                            → 事件保留在待重试队列
                                            → 队列空间释放后自动恢复重试
```

约束：
  - 待重试队列与主队列独立（不占用主队列容量）
  - 待重试队列长度受限于 `retry_queue_max: 1000`（可配置）
  - **待重试队列满行为**：
    - 新事件到达时待重试队列已满 → 阻塞 WAL 确认（即新事件的 WAL 写入后不推进 checkpoint）
    - 此阻塞与背压 Level 3 等效——Event Bus 队列和待重试队列双双满时，所有事件源最终被阻塞
    - 符合 AT_LEAST_ONCE 的"绝不静默丢弃"原则
    - 阻塞条件解除后（待重试队列释放空间），WAL checkpoint 自动恢复推进
    - ⚠ 阻塞期间新建 WAL 段持续累积——与背压 Level 4 溢出磁盘的机制联动，
      溢出目录满后回退到 Level 3 阻塞 → 此时待重试队列也阻塞 → WAL 写入被积压事件停止
      形成三级联锁：主队列满 → 待重试队列满 → WAL 写入阻塞
    - 建议：配置 overflow_disk_path 与 WAL 路径使用不同磁盘分区，确保 WAL 满不会与溢出争抢磁盘空间
  - 重启时自动检查待重试队列 + WAL 中是否有已确认但未投递的事件
  - 恢复后检查清单增加一项："待重试队列清空"
  - ⚠ checkpoint 与队列状态同步约束：待重试队列清空前不推进 checkpoint

**配置校验（总线模式绑定）：**
- `type: in_memory` 下 `persistence.*` 字段必须不存在，否则拒绝启动；去重表纯内存，EXACTLY_ONCE 退化为 AT_LEAST_ONCE
- `type: persistent` 下 persistence 使用默认值；去重表通过 WAL+checkpoint 持久化，重启后去重窗口继续生效
```

### 3.4 Event Dispatcher（事件分发器）—— 路由与转换引擎

Dispatcher 的核心职责：**决定一个事件应该交给哪些处理器**。

次要职责：**在事件处理完毕、队列清空时产生 `system.queue_drained` 事件**，
触发 Reflection 复盘（详见 idle-design.md）。Dispatcher 通过内部标志
`recently_processed_real_event` 防止 QueueDrained 的无限循环：
只有处理过真实事件（非 QueueDrained、非 IdleEvent）后才产生一次 QueueDrained。

处理 QueueDrained 时使用 `select!` 并发模式：同时等待 Reflection Pipeline 完成和
Event Bus 的新事件到达（通过 `wait_for_event()` 异步通知）。
新事件抢先到达 → 取消 Reflection → 立即处理新事件。
此机制依赖 Event Bus 提供异步的新事件到达通知（`wait_for_event()`），
以及 IdleCoordination 共享状态（`busy_reflecting` AtomicBool）协调 IdleDetector。

处理真实事件时，调用 `IdleCoordination.cancel_idle_workflows()` 中断所有正在运行的
空闲 Workflow（Sleep/Exploration/Meditation），并通过 `last_source_type` 原子变量
传递事件源类型，供 IdleDetector 的聊天模式检测使用。

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
            retry: { max_attempts: 3, retry_backoff: "exponential" }
        },
        {
            id: "insert-db"
            action: Tool "db-insert"
            compensate: Tool "db-delete-by-invoice-id" // 补偿：删除已插入的数据库记录
            retry: { max_attempts: 3, retry_backoff: "exponential" }
        },
        {
            id: "notify-slack"
            action: Tool "slack-send"
            compensate: Tool "slack-delete-message"   // 补偿：删除已发送的 Slack 消息
            retry: { max_attempts: 5, retry_backoff: "exponential" }
        }
    ]

    // 全局补偿策略
    compensation_strategy: reverse_order   // 反向顺序执行补偿（C3 → C2 → C1）
    compensation_contract: {               // 补偿操作契约
        idempotent: true,                  //   强制幂等
        timeout_sec: 30,                   //   独立超时 30s
        retry_count: 3,                    //   补偿操作本身的重试次数（可配置，默认 3）
        retry_backoff: "exponential",      //   补偿重试退避策略（默认 exponential）
        on_failure: compensation_failed    //   重试超限后进入 COMPENSATION_FAILED
    }
                                         // parallel 模式下补偿操作的额外约束见 §3.5 并发模型

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
2. 补偿 step3 (cleanup-temp) → 失败（文件被占用，按补偿合同配置重试仍失败）
3. Pipeline 进入 COMPENSATION_FAILED（非终态）
4. 记录："step4 已补偿，step3 未补偿" → 推送 HIGH 级别告警 → 人工接管
5. 补偿重试配置参见 compensation_contract.retry_count（默认 3 次）和 retry_backoff（默认 exponential）
```

> **无需补偿的步骤：** 纯计算/只读步骤（如 Filter、校验）不需要补偿定义，但框架也会跳过它们。
> **Transform 注意：** Transform 步骤可能产生副作用（临时文件、缓存写入、状态变更），
> 应和支持 Action 一样支持定义 `compensate`。框架不区分 Transform 和 Action 的补偿能力——
> 每个步骤都可选声明 `compensate`。纯只读 Transform（如格式转换、数据校验）无需补偿。

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
      d) 补偿操作必须按实例数据 scope 隔离：
         - 补偿使用的 Tool 必须以实例级标识（如 invoice_id、order_id）操作，不得使用无 scope 的全局操作
         - 补偿写入必须遵守 optimistic_lock 要求（条件 a 同样适用于补偿路径）
         - 推荐优先使用幂等 + 按 ID 回滚的操作（DELETE /orders/{id}），避免共享计数器回滚（counter DECR）
         - 框架级保障：parallel 模式下触发补偿时，框架自动为补偿工具注入实例隔离上下文（隔离 key 前缀、独立 API 客户端）
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

    // 状态名字段约束（⚠ 关键规则）：
    //   1. 状态名在核心定义和配置中统一使用大写惯例（如 PENDING, REVIEWING, ERROR）
    //   2. 运行时比较时大小写不敏感——框架对所有状态名做 normalize（统一转为大写再比较）
    //   3. YAML 配置可使用小写以保持可读性（pending, reviewing, error），框架自动 normalize
    //   4. 配置校验阶段检测到大小写不一致时发出警告，但不拒绝启动
    //   5. transfer 表中的事件名（如 SUBMIT, APPROVE, CANCEL, RETRY, ERROR）同理大小写不敏感
    //   6. 此规则确保 §3.7 核心定义（大写）和 §9.1 YAML 配置示例（小写）在运行时行为一致

    // 超时配置
    state_timeouts: {
        REVIEWING: { timeout: 7 days, on_timeout: REJECTED }   // 7 天无操作自动拒绝
        PENDING:   { timeout: 30 days, on_timeout: CANCELLED } // 30 天不提交自动取消
        ERROR:     { timeout: 7 days, on_timeout: ARCHIVED,   // 错误后 7 天自动归档
                     on_timeout_alert: true }                  // 归档前 1d/6h/1h 分别告警
        APPROVED:  { timeout: 30 days, on_timeout: ARCHIVED } // 终态：30 天后自动归档
        REJECTED:  { timeout: 30 days, on_timeout: ARCHIVED } // 终态：30 天后自动归档
        CANCELLED: { timeout: 30 days, on_timeout: ARCHIVED } // 终态：30 天后自动归档
        // 以上三条终态超时与状态图（\"(30天后自动)\"→ARCHIVED）一致
    }

    // 超时与用户事件竞态处理
    // Timeout 事件优先级低于同一实例的用户事件（用户主动操作优先于自动超时）
    // 实现要求：
    //   1. Timeout 事件在 Event Bus 中的优先级标记低于用户事件
    //   2. Timeout 触发时检查事件队列中是否有同实例的待处理用户事件
    //   3. 如有 → 延迟执行超时（timeout_defer_ms: 5000），等待用户事件先处理
    //   4. 延迟窗口内待处理事件被消费后 → 不再触发超时（用户事件已改变状态）
    //   5. 延迟窗口超时后仍未消费 → 重新检查状态并决定是否执行超时
    // 受影响状态：PENDING→SUBMIT vs PENDING→CANCELLED, REVIEWING→APPROVE vs REVIEWING→REJECTED

    // 超时时钟跨状态退出语义（⚠ 关键安全约束）
    // 当 Workflow 离开某个状态（如 REVIEWING→ERROR），该状态的超时时钟行为：
    //
    //   pause（推荐）：状态退出时超时时钟暂停，重新进入时恢复剩余计时
    //     例如: REVIEWING(7天) → Day3 进入 ERROR → Day5 RETRY 回到 REVIEWING
    //           剩余 4 天计时继续，Day5+4=Day9 触发超时
    //     ✅ 用户实际可用时间符合预期
    //     ✅ 无法通过 ERROR→RETRY 循环来重置审批计时器
    //
    //   reset：状态退出时超时时钟重置，重新进入时重新开始计时
    //     例如: 上述场景中获得 Day9+7=Day16 总窗口
    //     ⚠ 可被 ERROR→RETRY 循环利用来无限延长超时
    //
    //   continue：状态退出时超时时钟继续走，重新进入时计时可能已过期
    //     例如: 上述场景中 Day3 进入 ERROR，Day5 RETRY 时 REVIEWING 已过期
    //     ⚠ 恢复后即刻触发超时 → 用户体验为\"刚恢复就过期\"
    //
    // 框架默认采用 pause 语义。
    // 配置项：state_timeout_behavior_on_exit: pause | reset | continue（默认 pause）
    // 安全约束：reset 模式下必须确保与 ERROR 恢复路径的 max_retry_count 配合，
    //           避免通过 ERROR→RETRY 循环无限重置超时。

    // ERROR 状态默认行为（框架强制）
    // on_enter: 保存 last_active_state → 默认触发 alert+log（告警级别 HIGH）
    // on_enter: 重置 session_retry_count = 0（当前 ERROR 会话内的重试追踪）
    //            total_retry_count 保持不变（累计全局重试次数，永不被重置）
    // ⚠ guard 必须检查 total_retry_count 而非 session_retry_count，
    //   否则进入 ERROR 时 session_retry_count 每次被重置为 0，
    //   max_retry_count guard 永远无法触发 → 无限重试循环
    // on_timeout: ERROR→ARCHIVED 前 1d/6h/1h 分别告警

    // ERROR 恢复配置
    error_recovery: {
        retry_event: RETRY                    // 从 ERROR 恢复的事件名
        retry_to: last_active_state           // 恢复到进入 ERROR 前的状态
        max_retry_count: 3                    // 最多重试恢复 3 次（基于 total_retry_count），超过后进入 ARCHIVED
        retry_backoff: "immediate"            // 恢复前的延时策略（标准化格式）：
                                             //   "immediate"            — 立即重试（无延迟）
                                             //   "fixed:5s"             — 固定间隔
                                             //   "exponential"          — 默认指数退避（base=100ms, factor=2, max=30s）
                                             //   "exponential:1s:2:30s" — 自定义指数退避（base, factor, max_delay）
                                             //   "sequence:1s,2s,4s"   — 显式序列（按顺序各级间隔）
        // —— 语义解释 ——
        // RETRY 事件的触发来源由 retry_backoff 与 auto_retry_count 配合决定：
        //   auto_retry_count: 0（默认值）：框架不自动发送 RETRY，超限后操作员手动发送
        //                       此时 retry_backoff 表示"操作员发送 RETRY 后，执行恢复前是否额外等待"
        //   auto_retry_count: N（>0）：框架在 ERROR 进入后自动发送 RETRY，最多 N 次
        //                       此时 retry_backoff 表示自动 RETRY 之间的间隔
        //                       示例：auto_retry_count: 2 + "immediate" → 立即尝试恢复，失败后立即再试 1 次
        //                       示例：auto_retry_count: 3 + "fixed:15s" → 进入 ERROR 后等 15s 自动 RETRY，
        //                             失败后等 15s 再试，再失败等 15s 最后试
        auto_retry_count: 0                   // 框架自动重试次数（0 = 仅手动，>0 = 自动重试 N 次）
        auto_retry_interval: "5s"             // 自动重试间隔（auto_retry_count > 0 时生效）
        // ⚠ 安全约束：
        //   - auto_retry_count + retry_backoff + max_retry_count 三者之和不能使 ERROR 在短时间内烧光所有重试机会
        //   - 建议：auto_retry_count ≤ max_retry_count / 2，为手动重试预留空间
        //   - 自动重试与手动重试共享 total_retry_count（同一计数器）
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
        { from: ERROR,     event: RETRY,    to: :last_active_state, guard: total_retry_count < max_retry_count,
          on_fail: ARCHIVED },                                 // 恢复失败超过上限则归档（基于 total_retry_count）
    ]

    //
    // ERROR 状态双出口路径的优先级规则（⚠ 关键约束）：
    //   ERROR 状态有两条并发合法出口：
    //     RETRY → 恢复到进入前的状态，继续业务流程
    //     CANCEL → 直接放弃实例，进入 CANCELLED 终态（from: ANY 覆盖 ERROR）
    //
    //   当 RETRY 和 CANCEL 事件同时到达或时间窗口紧密时：
    //
    //     优先级规则（按事件到达顺序）：
    //       RETRY 先于 CANCEL 处理时 → 状态变为 last_active_state（如 REVIEWING），
    //         CANCEL 事件到达时 from: ANY 触发 → 刚恢复的实例被取消
    //       CANCEL 先于 RETRY 处理时 → 进入 CANCELLED 终态，
    //         RETRY 事件到达时状态不匹配 → 事件静默丢弃
    //       => 框架行为：在 ERROR 状态，CANCEL 事件附加隐式 guard：
    //          has_pending_retry → 如队列中有待处理的 RETRY 事件，CANCEL 延迟执行
    //          （类似 timeout_defer_ms 机制），等待 RETRY 事件先被消费
    //       => 如果延迟窗口（retry_cancel_conflict_defer_ms: 5000）内 RETRY 未到达，
    //          则 CANCEL 正常执行
    //
    //   设计意图：在 ERROR 状态，恢复（RETRY）比放弃（CANCEL）具有更高的业务优先级
    //   ——临时性故障时优先尝试恢复，只有确认恢复失败或操作员明确选择放弃时才进入 CANCELLED
    //   1. Pipeline 作为 action 失败时，Workflow 默认进入 ERROR 状态
    //      { from: A, event: E, to: B,
    //        action: Pipeline "processor",
    //        on_action_failure: ERROR,         // Pipeline 失败 → Workflow 进入 ERROR
    //        on_compensation_failure: EMERGENCY // 补偿失败 → 更高告警级别
    //      }
    //      补偿成功 ≠ Pipeline 成功——补偿是回滚操作，Pipeline 依然失败，不应继续业务流程。
    //
    //   2. 补偿失败的 Workflow 实例应有额外标记
    //      Pipeline 补偿进入 COMPENSATION_FAILED + Workflow 在 ERROR 状态
    //        → Workflow 实例标记为 partial_rollback: true
    //        → 告警级别高于普通 ERROR（⚠ 数据可能处于半回滚状态）
    //        → 人工恢复时优先检查此标记
    //
    //   3. CANCEL 与 inflight Pipeline 的交互
    //      如果在 Pipeline 作为 action 正在执行时收到 Workflow 的 CANCEL 事件：
    //        → 等待 Pipeline 完成（或补偿完成）后再执行 CANCEL 转移
    //        → CANCEL 不中断正在执行的 Pipeline/补偿（与 §2.5.3 排水阶段语义一致）
    //   4. RETRY 恢复后重新执行 Pipeline 的幂等性要求
    //      从 ERROR RETRY 到 last_active_state 后重新执行该状态关联的 Pipeline action：
    //        → Pipeline 的每个步骤必须满足幂等性（之前补偿回滚 + 现在重试可安全重复执行）
    //        → 建议：Pipeline 步骤使用业务键（如 invoice_id）做幂等检查，而非自增序列
    //

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
                      RETRY │ (total_retry_count < 3)
                            ▼
                    last_active_state ──→ 恢复原流程
                            │
                      RETRY 第4次失败（total_retry_count ≥ 3）──→ ARCHIVED
                            │
                      (7天无操作自动) ──→ ARCHIVED
                            │
                      CANCEL ──→ CANCELLED
                      (隐式 guard: 见 §3.7 双出口规则)
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
    //
    // 物理存储清理语义（⚠ 关键约束）：
    //   Plugin/Skill disable 或卸载时，对应的物理存储空间按以下规则处理：
    //     cleanup_policy: retain | delete_on_disable | delete_on_uninstall（可配置，默认 retain）
    //       - retain（默认）：禁用/卸载后物理存储保留，重新启用时可继续使用
    //         ✅ 数据不丢失，适合快速启用/禁用场景
    //         ⚠ 长期运行后累积废弃存储碎片
    //       - delete_on_disable：禁用时立即删除物理存储
    //         ✅ 节省空间
    //         ⚠ 重新启用时需要全新初始化（数据不保留）
    //       - delete_on_uninstall：仅在插件彻底卸载（从配置中移除）时删除
    //         ✅ 兼顾禁用/启用的热切换和最终清理
    //         ⚠ 卸载操作不可逆
    //   框架级钩子：
    //     - Plugin/Skill 的 on_unload 执行后，框架按 cleanup_policy 执行清理
    //     - 清理前触发 `before_storage_cleanup` 事件 → 允许其他模块做最后读取
    //     - 清理操作产生审计日志：affected_storage_path, policy, operator, timestamp
    //   安全约束：
    //     - 如果物理存储包含用户数据（PII/GDPR 受保护数据）：
    //       cleanup_policy 不得为 retain（避免未授权数据残留）
    //       → 框架在 config_schema 中增加 pii_classification 标记，
    //          标记为 pii 的插件强制 cleanup_policy >= delete_on_disable

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

### 6.4 运行时管理

`CronSource` 是一个精简的 `EventSource`（~210 行），通过 `SourceRegistry` 统一管理生命周期。
调度由 `SourceRegistry` 的后台 `poll_loop` 驱动——不需要独立的 cron daemon。

```
// CronSource 作为普通 EventSource 注册到 SourceRegistry
sources.register(Box::new(CronSource::new(id, expression)?), SourceMode::Pull, TrustLevel::Untrusted).await?;
sources.start(id).await?;

// 运行时管理复用 SourceRegistry 的标准 API
sources.reconfigure(id, config).await?;   // 修改 cron 表达式或时区
sources.pause(id).await?;                  // 暂停
sources.resume(id).await?;                 // 恢复
sources.shutdown(id).await?;               // 关闭
sources.unregister(id).await?;             // 移除
```

**CronSource 简化原则**：
- 去掉 DST 策略、leader election、rate limiting——这些属于部署层或已由 EventBus 背压覆盖
- 去掉复杂的 catch-up 模式——poll 时只发射最近一次到期 tick（skip 语义），agent 定时器不需要补跑历史事件
- `reconfigure` 支持动态修改 `expression` 和 `timezone`，立即生效

**与 TimerSource 的区别**：

| | CronSource | TimerSource |
|---|-----------|------------|
| 触发方式 | cron 表达式（时间点语义） | 固定间隔 ms |
| 典型场景 | "每天 9:00"、"每周五 17:00" | 心跳、轮询 |
| 事件类型 | `CronTick` | `Heartbeat` / `TimerTick` |

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
    level3_threshold: 0.95           # 95% 满 → 阻塞 poll + 暂停 Push 来源
    level3_block_push: true          # Level 3+ 时是否通知 Push 来源暂停 publish()
    level4_threshold: 0.98           # 98% 满 → 溢出 AT_LEAST_ONCE 到磁盘
    level4b_overflow_enter: 0.8      # 溢出目录使用率 ≥80% → 进入 Level 4B
    level4b_hysteresis_leave: 0.5    # 溢出目录使用率 ≤50% → 离开 Level 4B（含滞回）
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
        check_open_files: auto

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
      compensation_contract: { idempotent: true, timeout_sec: 30, retry_count: 3, retry_backoff: "exponential", on_failure: compensation_failed }
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
      approved:  { timeout: "30d", on_timeout: archived }
      rejected:  { timeout: "30d", on_timeout: archived }
      cancelled: { timeout: "30d", on_timeout: archived }
    transitions:
      - { from: pending,   event: SUBMIT,   to: reviewing, guard: hasPermission, on_fail: pending }
      - { from: reviewing, event: APPROVE,  to: approved }
      - { from: reviewing, event: REJECT,   to: rejected }
      - { from: ANY,       event: CANCEL,   to: cancelled }
      - { from: ANY,       event: ERROR,    to: error }
      - { from: error,     event: RETRY,    to: :last_active_state, guard: total_retry_count < 3, on_fail: archived }
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
POST /agent/start              # 启动事件循环（同步阻塞直到 Phase 5 就绪后返回）
                               #   - 成功：HTTP 200 OK
                               #   - 被 shutdown 中断：HTTP 409 Conflict
                               #     body: {"status": "interrupted_by_shutdown", "phase": "中断时所在阶段"}
                               #   - 不可恢复错误：HTTP 500 + 错误详情
                               #   调用者应处理 409：检查 /health 决定是否重新 start
                               #   幂等性：如果 Agent 已在 Phase 5（运行中），立即返回 HTTP 200 OK
                               #   （"启动"在已运行状态下是安全的空操作）
                               #   超时建议：调用者应设置大于 plugin_load_timeout + 60s 的客户端超时
                               #   Phase 3（Workflow 恢复，workflow_recovery_timeout: 120s 默认）可能耗时较长
POST /agent/shutdown           # 优雅关闭
                               #   同步阻塞直到关闭序列完成（Phase 5→0）后返回 HTTP 200 OK
                               #   可能耗时较长（等待排水超时 drain_timeout_sec + WAL 刷盘）
                               #   幂等性：如果 Agent 已经关闭或正在关闭中，返回 200 OK（空操作）
                               #   超时建议：调用者应设置大于 drain_timeout_sec + 30s 的客户端超时

POST /event-source/{id}/pause  # 暂停事件源
POST /event-source/{id}/resume # 恢复事件源
PUT  /event-source/{id}/config # 动态重配置

POST /plugin/{name}/enable     # 启用插件
POST /plugin/{name}/disable    # 禁用插件

POST /cron/add                 # 添加定时任务
                               #   ⚠ 受 §6.4 安全守卫约束（min_interval 硬编码 clamp、rate_limit 全局限制、审计日志）
POST /cron/{id}/update         # 更新定时任务
                               #   ⚠ 同 /cron/add，受 §6.4 安全守卫约束
POST /cron/{id}/remove         # 删除定时任务

GET  /metrics                  # 运行指标
                               #   输出格式：至少支持 Prometheus exposition format（行业标准）
                               #   核心暴露指标（所有格式必须包含）：
                               #     event_bus_queue_depth    # 当前队列深度（按优先级分）
                               #     event_throughput_total   # 总计事件吞吐（count/s）
                               #     backpressure_level       # 当前背压级别（0-5）
                               #     events_discarded_total   # 丢弃事件累计计数
                               #     retry_queue_depth        # 待重试队列当前深度
                               #                              # 接近 retry_queue_max（默认 1000）时预警
                               #                              # 此队列满会阻塞 WAL checkpoint 推进
                               #     inflight_pipelines       # 当前运行中 Pipeline 数量
                               #     inflight_skills          # 当前运行中 Skill 实例数
                               #     plugin_health            # 按插件名的健康状态（1/0）
                               #     dlq_depth                # 死信队列深度
                               #   可选：同时支持 JSON 格式用于自定义消费
GET  /health                   # 健康检查
POST /inject-event             # 手动注入事件（调试用）

GET  /events/trace/{trace_id}  # 按 TraceID 追踪事件链路
GET  /events/dump/{id}         # 导出事件详细信息（含 metadata）
GET  /dlq                      # 查看死信队列
POST /dlq/{id}/retry           # 手动重试死信事件
                               #   重试语义（⚠ 关键规则）：
                               #     1. 计数器语义：手动 retry 视为管理员介入，重置 retry_count 为 0
                               #        但保留原始计数字段：original_retry_count（审计用）
                               #     2. 再次失败路径：重新入 DLQ，retry_count 从重置后的值重新累计
                               #     3. TTL 行为：重新入 DLQ 后 TTL 重置（dlq_ttl_days 重新计时）
                               #        但加上全局手动重试上限：max_manual_retries: 5（默认）
                               #        超限后事件标记为 unrecoverable → 不再可手动 retry
                               #     4. 操作历史：每次手动 retry 记录至 dlq_storage：
                               #        operator, timestamp, reason（可选）
                               #   可配置：dlq.manual_retry_reset_counters: true | false（默认 true）
POST /dlq/{id}/discard         # 确认丢弃死信事件

GET  /audit-log                # 审计日志（配置变更、权限操作、事件丢弃）
                               #   分页参数（至少支持 cursor 游标分页）：
                               #     &cursor=&page_size= 或 &offset=&limit=
                               #   过滤参数：
                               #     &since=&until=（时间范围，ISO 8601）
                               #     &type=config_change|permission|discard|secret_rotation|dlq_operation
                               #     &operator=（按操作员身份筛选）
                               #   访问约束：
                               #     - 默认需要比普通控制接口更高的权限等级（只读审计员 vs 操作员）
                               #     - 生产环境默认绑定 localhost / Unix socket（同通用约束）
                               #     - 审计日志包含 Secret 指纹哈希 → 暴露密钥轮换时间线
                               #       建议：不在 audit-log 中暴露 _raw_ fingerprint，
                               #       改为暴露 fingerprint_created（时间戳）
                               #     - 不提供全量批量导出接口（最小可读单元：单条记录）
```

**控制接口安全守卫：**
安全约束：
- 默认绑定 localhost/Unix socket，暴露到网络必须配置认证（API Token / mTLS / OAuth2）
- 敏感操作（shutdown、disable plugin、dlq retry）需要二次确认 + 操作审计日志
- `POST /inject-event` 生产环境默认禁用，需 `force_enable_debug_endpoints: true` 显式开启

**LLM 注入防护（ChatPlatformSource）：**

| ChatPlatformSource 等用户输入事件源可能将不可信内容传递给 LLM-based Skill，
| 存在 prompt injection / jailbreak 风险。

| 防护要求：
|   1. 输入信任等级：所有 ChatPlatformSource 事件 payload 视为不可信（untrusted）
|   2. LLM-based Skill 必须实施以下加固措施：
|      a. System Prompt 加固：明确声明"忽略用户指令覆盖"的安全边界指令
|      b. 输入过滤：框架层对用户消息进行基础过滤（已知注入模式匹配）
|      c. 输出校验：LLM 输出在发送给用户前，框架层校验是否包含敏感信息泄露
|      d. 敏感操作隔离：任何涉及写/执行的操作必须通过 Tool 执行，Tool 层实施权限校验，
|          不依赖 LLM 的"自我约束"
|   3. 建议：敏感 Skill 对 ChatPlatformSource 启用 sandbox 模式（限制 LLM 可调用的 Tool 范围）
|   4. 审计日志：LLM 注入尝试（匹配到已知模式）记录至审计日志，不影响正常用户消息

| > **信任等级配置：**
| > 每个 EventSource 可配置 `trust_level: trusted | untrusted | sandboxed`（默认 untrusted）
| > - trusted：Payload 可直接用于 LLM context（内部系统事件/受信任的内部 API）
| > - untrusted：Payload 需经过输入消毒后才能传递给 LLM（用户消息/Webhook）
| > - sandboxed：Payload 传递给额外受限的 LLM 沙箱（高风险来源如匿名公网 Webhook）
| > 框架在 Dispatcher 路由时根据 trust_level 自动附加安全约束

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

### 决策 9：优先级与同来源保序的权衡——保序优先

同一 Event Source 的事件：保序优先于优先级。跨来源的事件：优先级正常生效。

- **理由**：同来源事件的顺序语义通常承载业务逻辑（前一个事件改变状态，后一个事件依赖该状态）。如果 HIGH 事件跳过队列中的 NORMAL 事件，可能导致顺序敏感的全局状态不一致。
- **场景**：同一 FileWatchSource 产出 NORMAL("文件删除") 随后 HIGH("文件修改")。若优先级优先，先修改后删除 → 业务结果错误。若保序优先，先删除后修改 → 修改操作发现文件已不存在 → 正确处理。
- **代价**：同来源场景下，优先级配置看起来不生效（跨来源时仍生效），可能让开发者误以为 priority 字段有 bug。
- **缓解**：在 §3.3 的保序段中明确写入冲突规则和示例。如需同一来源内的优先级突破，使用不同 Event Source 发出不同优先级消息。

### 决策 10：状态名大小写不敏感

Workflow 状态名（PENDING, REVIEWING, ERROR, ...）在运行时统一 normalize 为大写再比较。

- **理由**：§3.7 核心定义使用大写惯例（PENDING, ERROR），而 §9.1 YAML 配置使用小写（pending, error）以提高可读性。两种风格各有合理性，框架应兼容不惩罚用户选择。
- **规则**：
  1. 状态名在运行时比较时大小写不敏感——框架对所有状态名做 normalize（统一转为大写再比较）
  2. YAML 配置可使用小写（pending, reviewing, error），框架自动 normalize
  3. 配置校验阶段检测到大小写不一致时发出警告，但不拒绝启动
  4. transition 表中的事件名（SUBMIT, APPROVE, CANCEL, RETRY, ERROR）同理大小写不敏感
- **代价**：运行时多一次 normalize 操作（O(1) 字符串转换，性能可忽略）
- **缓解**：normalize 在配置加载阶段完成，不影响运行时事件处理路径

### 决策 11：重试退避标准化约定

所有重试机制统一使用 `retry_backoff` 字段名和标准化的值格式。

- **理由**：文档中五处独立的重试机制（WAL→内存重试、Pipeline step、Compensation、error_recovery、Secret 重试）使用了不同的字段名（`backoff` / `retry_backoff` / `secret_retry_backoff`）和值格式（隐式序列 / CSV / enum + 参数），开发者需要在五种语境中学习四种不同的配置方式。
- **标准化格式**：

  | 标准值 | 语义 | 示例 |
  |--------|------|------|
  | `"exponential"` | 默认指数退避（base=100ms, factor=2, max=30s） | |
  | `"exponential:1s:2:30s"` | 自定义指数退避（base, factor, max_delay） | `"exponential:200ms:3:10s"` |
  | `"fixed:5s"` | 固定间隔 | `"fixed:10s"` |
  | `"sequence:1s,2s,4s"` | 显式序列（按顺序各级间隔） | `"sequence:100ms,500ms,2s"` |
  | `"immediate"` | 立即重试（无延迟） | |

- **统一字段名**：全局使用 `retry_backoff`，弃用旧的字段名变体（`backoff`、`secret_retry_backoff`）
- **配置入口统一**：为 WAL→内存队列重试增加 `wal_retry_backoff` 配置项（默认 `"sequence:100ms,500ms,2s"`，与当前硬编码行为一致）

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
|| 27 | **背压 Level 3 Push 来源绕过**：Level 3 阻塞 poll() 但 Push 来源（Webhook）直接调用 publish() 不受影响 | 🔴 高 | 有 Push 事件源（Webhook/Socket/消息队列消费者）时触发背压 Level 3 | Push 来源继续注入事件，队列深度不降反升，Level 3→4→5 快速升级，Webhook 线程 OOM | EventSource 增加 backpressure_signal 接口 + Level 3 时 Push 来源暂停 publish() + Webhook 返回 503 + 框架关键原则中明确定义 Push 来源行为（§3.3 关键原则） |
|| 28 | **State Timeout 与用户事件竞态**：超时定时器和用户事件同时到达，顺序不确定导致用户操作被超时覆盖 | 🟡 中 | 工作流实例在超时边界（最后几毫秒）收到用户事件 | 用户提交的表单被毫秒级竞态条件取消，用户体验为"提交了但被莫名其妙取消了" | Timeout 事件优先级低于用户事件 + Timeout 触发时检查队列中是否有同实例待处理用户事件 + 延迟超时窗口（timeout_defer_ms: 5000）（§3.7 竞态处理） |
|| 29 | **Pipeline Transformer 副作用补偿缺失**：Transform 步骤产生的临时文件/缓存不在补偿链中 | 🟡 中 | Transform 步骤有副作用（OCR 缓存、图像切片、临时 PDF）且后续 Action 失败触发补偿 | 临时文件累积占用磁盘，高吞吐 Pipeline 产生 GB 级残留 | 框架不区分 Transform 和 Action 补偿能力，每个步骤都可选声明 compensate + 纯只读 Transform 无需补偿（§3.5 Transform 注意） |
|| 30 | **背压 Level 4A↔4B 滞回缺失导致振荡**：Level 4A（溢出到磁盘）和 4B（磁盘满回退）在边界处来回切换 | 🟢 低 | 队列在 98% 附近波动 + 溢出目录使用率在 80% 附近 | 系统日志充满背压级别转换记录 + 溢出目录在"满"和"不满"边界产生大量小周期 | Level 4B 离开条件设为 ≤50%（含 30% 滞回区间），而非 ≤80%（§3.3 Level 4B 离开条件） |
|| 31 | **lifespan_ms 字段无自动清理机制**：字段定义生命周期但框架未实现自动清理，承诺与实现分离 | 🟢 低 | 开发者使用 lifespan_ms 声明临时资源寿命但框架什么也不做 | 开发者以为框架会自动清理，但临时文件/资源永久残留 | 文档标注为"预留接口，v1.0 未实现自动清理"+ 开发者当前需在 compensate/on_final 中自行清理（§3.1 lifespan_ms 注释） |
|| 32 | **ERROR retry_count 重置致无限重试循环**：进入 ERROR 时重置 retry_count，但 RETRY guard 检查同一个计数器，循环永不终止 | 🔴 高 | ERROR on_enter 重置 retry_count + guard 检查 retryCount < max_retry_count | 唯一逃生路径是 7 天 ERROR→ARCHIVED 超时；每次 RETRY 失败等待 7 天才能自动归档；root cause 不可自动修复时产生大量重复请求 | 拆分 session_retry_count（ERROR 入时重置）和 total_retry_count（永不被重置）；guard 检查 total_retry_count（§3.7 ERROR 默认行为 + 恢复配置） |
|| 33 | **State Timeout 时钟跨 ERROR 语义未定义**：Workflow 从 REVIEWING→ERROR→RETRY→REVIEWING 过程中，REVIEWING 的超时时钟行为无定义 | 🟡 中 | ERROR 状态退出/再进入时，被暂停状态的超时时钟未定义 | continue 语义下恢复后即刻过期（"刚恢复就过期"）；reset 语义下可被 ERROR→RETRY 循环无限延长超时 | 默认采用 pause 语义（状态退出时钟暂停，重新进入恢复计时）；可配置 state_timeout_behavior_on_exit: pause | reset | continue（§3.7 超时时钟跨状态退出语义） |
|| 34 | **状态图终态归档路径与配置不一致**：状态图显示 APPROVED/REJECTED/CANCELLED → (30天后自动) → ARCHIVED，但 state_timeouts 无对应定义 | 🟡 中 | 终态实例无超时定义 | 终态实例在 State Store 永久累积 → 高吞吐系统内存暴涨 → 查询性能下降 → OOM | 在 state_timeouts 中为三个终态各添加 timeout: 30d, on_timeout: ARCHIVED（§3.7 + §9.1 YAML 配置已同步修复） |
|| 35 | **FileWatchSource 锁检测在非本地 FS 失效**：NFS/CIFS/FUSE/s3fs 上 flock/lsof 不可用，稳定确认机制退化 | 🟢 低 | 监听目录位于远程/虚拟文件系统 | check_open_files: true 始终返回"未打开"→ 发布不完整文件；或锁检测永远不返回 → 文件变更事件永不发布 | check_open_files 支持 auto | true | false（默认 auto 自动检测 FS 类型）；远程 FS 建议增大 debounce_ms 和 max_stable_wait_ms（§3.2 文件锁检测局限性） |
|| 36 | **Cron rate_limit 超额语义未定义**：同一秒内触发的 cron job 超过 rate_limit 时，超额事件行为未指定 | 🟢 低 | 多个 cron job 同时触发（如每小时整点 150 个 job 在首秒涌入）或 catch_up=all 大量恢复事件 | 超额事件可能被静默丢弃（部分 cron job 漏跑）；或延迟到下一秒并级联堆积；操作员不知情 | rate_limit_overflow: delay（默认）超额事件延迟到下一秒注入并记录延迟日志（§6.4 安全守卫）；catch_up 节补充说明与 rate_limit 交互（§6.5） |
|| 37 | **待重试队列满阻塞 WAL checkpoint**：AT_LEAST_ONCE 事件进入待重试队列，队列满后新事件无处可去 | 🟡 中 | 事件持续失败 → 待重试队列堆积至 retry_queue_max(1000) | WAL checkpoint 停滞 → WAL 段无限累积 → 磁盘写满 → 系统崩溃；重启时 WAL 从头重放引发二次事件风暴 | 队列满时阻塞 WAL 确认（不静默丢弃）；与背压 Level 4 溢出磁盘联动形成三级联锁；分级建议 overflow_disk_path 与 WAL 路径独立磁盘分区（§3.3 待重试队列满行为） |
|| 38 | **Pipeline parallel 模式补偿并发冲突**：parallel 模式下多个实例同时触发补偿，补偿操作无并发保护 | 🟡 中 | parallel Pipeline 中多个实例因同类故障同时失败 | 多个补偿同时操作同一共享 State Store key → optimistic_lock 冲突 → 补偿本身失败 → COMPENSATION_FAILED → 人工接管风暴 | parallel 模式安全条件新增第(d)条：补偿操作必须按实例数据 scope 隔离；遵守 optimistic_lock；推荐幂等 + 按 ID 回滚；框架注入实例隔离上下文（§3.5 parallel 条件 + §3.5 compensation_contract 交叉引用） |
|| 39 | **YAML 配置 retryCount 与代码块不一致**：YAML 配置示例的 RETRY guard 使用旧变量名 retryCount，与 §3.7 代码块的 total_retry_count 不同 | 🟢 低 | 开发者从 §9.1 YAML 配置拷贝示例 | 若 retryCount 映射到 session_retry_count（每次重置），则 R5 #1 修复的无限重试 bug 以不同形式残留 | YAML 配置示例改为 total_retry_count，与代码块保持同名同义（§9.1 YAML 配置已修复） |
|| 40 | **retry_backoff 自动/手动语义未定义**：文档未说明 RETRY 事件由谁发送，retry_backoff 在自动 vs 手动模式下含义不同 | 🟢 低 | 开发者配置自动重试（non-immediate 值）时会发现歧义 | 自动模式下立即烧光重试机会（抖动恢复前 max_retry_count 耗尽）；手动模式下 retry_backoff 参数含义不明确 | 新增 auto_retry_count（0=手动）和 auto_retry_interval 参数；retry_backoff 专门描述恢复前延时；安全约束限制自动重试 ≤ max_retry_count/2（§3.7 error_recovery 配置） |
|| 41 | **组件初始化顺序竞态**：Agent 启动时 WAL 重放、插件加载、Event Source 激活的顺序未定义 | 🟡 中 | 崩溃重启后 WAL 重放先于插件加载 | WAL 中 AT_LEAST_ONCE 事件重放到空 Event Bus → 无处理器 → 事件静默丢弃违反交付保证；Workflow 状态恢复后无处理器接收转移事件 → 实例孤岛；health check 在未就绪时返回 200 | 定义 6 阶段启动序列（§2.5.1）+ WAL 恢复事件暂存缓冲区直到 Phase 2 完成 + 分阶段健康检查端点（§2.5.2） |
|| 42 | **Priority 与同来源保序的冲突**：相同 Event Source 的 NORMAL 后跟 HIGH 事件，两条设计规则冲突 | 🟡 中 | 同一 Event Source 产生不同优先级事件 | 选择优先级优先 → 业务顺序反转（先修改后删除）；选择保序优先 → 同来源内优先级不生效 | 明确规则：同来源保序优先于优先级，跨来源优先级正常生效（§3.3 冲突规则 + 设计决策 9） |
|| 43 | **补偿操作 retry 次数无配置入口**：补偿失败路径图显示"重试 3 次"，但 compensation_contract 无对应参数 | 🟢 低 | 补偿执行本身失败（文件被占用、网络超时） | 开发者以为补偿会重试但不知重试几次或如何修改；不同的补偿操作（本地文件删除 vs 远程 API 回滚）无法差异化设置 | compensation_contract 新增 retry_count（默认 3）和 retry_backoff（默认 exponential）参数（§3.5 compensation_contract + YAML 同步更新） |
|| 44 | **优雅关闭未覆盖 Pipeline/Skill inflight 执行**：关闭序列定义了 Source→Workflow→WAL 但漏了 Pipeline/Skill 正在进行的执行和补偿 | 🟡 中 | shutdown 信号在 Pipeline 执行步骤或补偿中到达 | 补偿 step 1 成功、step 2 被中断 → 部分回滚不可恢复；Tool 执行后被中断 → 结果已写出但 Pipeline 认为失败 → 重启重复执行 | 新增 Phase 4.5 排水阶段：等待 inflight Pipeline/Skill 完成或 drain_timeout_sec: 30 超时 → 记录补偿状态日志后强制终止（§2.5.3 关闭顺序 + 关闭中断说明） |
|| 45 | **WAL 恢复缓冲区大小上限未定义**：R7 新增的恢复事件暂存缓冲区只定义了存在性，未定义容量和超限行为 | 🟢 低 | 大量 WAL 恢复事件（停机数小时）+ Phase 2 插件加载耗时较长 | 缓冲区无上限 → OOM；缓冲区有隐式上限但满后 WAL 重放卡死 → 启动无法完成 | 定义 wal_replay_buffer_max: 5000（可配置）；超限时暂停重放 + 记录断点 + 下次启动继续（§2.5.1 WAL 缓冲约束） |
|| 46 | **dedup_key payload_hash 对大型 payload 的 CPU 开销**：缺省去重键包含 payload_hash，入队时立即计算，不论事件是否会被去重 | 🟢 低 | 事件 payload 较大（10MB+ JSON、base64 二进制）且吞吐较高 | 每个事件的入队流程增加 O(n) hash 计算成本；AT_MOST_ONCE 事件的 hash 完全浪费 | AT_MOST_ONCE 事件跳过 dedup_key 计算；轻量 dedup_key 指引（event.id / source+type+timestamp）；UUID v7 天然唯一事件显式设置 dedup_key（§3.1 Event dedup_key 字段注释） |
|| 47 | **排水超时与 Tool 自身超时优先级冲突**：drain_timeout_sec: 30 可能在 Tool 自身 timeout（如 120s）之前强制终止它，Tool Runner Step 6 清理可能被跳过 | 🟡 中 | Shutdown 信号在 Tool 执行中到达，且 Tool timeout > drain_timeout_sec | Tool 清理逻辑被跳过 → 残留临时文件/未释放资源；两个超时机制独立作用但有谁负责清理的歧义 | 定义两者取其先规则：Tool timeout < drain → Tool 自清理优先；Tool timeout > drain → 框架强制终止并保证 Step 6 清理执行；框架级和 Tool 级清理层次分离（§2.5.3 排水超时与 Tool 超时交互） |
|| 48 | **WAL 恢复缓冲区断点偏移量持久化未定义**：文档说"记录断点偏移量"但未指定存哪里、如何持久化、崩溃后如何恢复 | 🟢 低 | 缓冲区超限暂停 + Phase 1 标记"部分完成"后崩溃 | 断点丢失 → 下次启动从头重放 WAL → 缓冲区再次超限 → 递归循环；暂停兜底策略无效 | 保存在 {wal_path}/replay_checkpoint 文件，fsync 确认；Phase 1 启动时先读取；不存在/损坏则退回到 WAL 头部；Phase 2 完成后删除（§2.5.1 断点持久化方式） |
|| 49 | **排水阶段"停止重试模式"与已调度重试的竞态**：待重试队列进入停止重试模式时，可能已有已调度但未执行的重试，可能启动新的 Tool 执行与排水冲突 | 🟢 低 | 排水开始时待重试队列中有已调度重试 | 已调度重试在"停止"模式下启动新 Tool → 排水等待永无止境；简单取消所有已调度重试 → 违反 AT_LEAST_ONCE 原则 | 定义三层语义：允许已调度重试继续执行（属 inflight）；不产生新调度；排水超时后未执行重试标记 shutdown_abandoned → 下次启动 Phase 1 重建时重新入队（§2.5.3 停止重试模式子语义） |
|| 50 | **shutdown 在 startup 中途到达行为未定义**：6 阶段启动序列定义了正常启动，6 阶段关闭序列定义了正常关闭，但 shutdown 在 Phase 0~4 到达时无定义 | 🟡 中 | 操作员在 Agent 启动完成前发送 shutdown | Phase 1 WAL 恢复中断 → 事件丢失；Phase 2 插件半加载 → on_unload 未执行泄露资源；Phase 3 Workflow 半恢复 → 实例丢失；start 阻塞与 shutdown 可能死锁 | 定义生命周期入口/出口边界规则（§2.5.4）：Phase 0~3 立即进入各阶段对应的关闭等价阶段；Phase 4 等同正常关闭；replay_checkpoint 保留策略 |
|| 51 | **shutdown_abandoned 与 WAL checkpoint offset 关系未定义**：三种 offset 追踪机制（WAL checkpoint、replay_checkpoint、shutdown_abandoned）的关系和边界无定义 | 🟢 低 | 排水阶段产生 shutdown_abandoned 事件后重启 | 同一事件既出现在 WAL checkpoint 之后的重放范围中，又出现在 shutdown_abandoned 列表中 → 重复入列 | 声明三者为正交 offset 域：shutdown_abandoned 源待重试队列（WAL 确认但未处理），不会出现在 WAL checkpoint 之后；Phase 1 重建时 dedup_key 自动去重（§2.5.4 offset 关系） |
|| 52 | **Phase 2 中断时"已加载插件"边界模糊**：§2.5.4 定义"已加载的插件走卸载流程"，但与 §4.3 插件生命周期状态模型不匹配——on_load 执行中的半加载插件无法安全调用 on_unload | 🟡 中 | Shutdown 在插件 on_load 执行中到达 | 对半加载插件调 on_unload → 崩溃/未定义行为；跳过半加载插件不清理 → 资源泄漏（DB 连接、文件句柄部分初始化） | 精细化为三级：全加载（on_load 完成 → 正常卸载）、半加载（on_load 中 → 跳 on_unload + OS 回收资源 + 告警日志）、未加载（跳过）。§2.5.4 Phase 2 已更新 |
|| 53 | **POST /agent/start 在 shutdown 中断期间返回值未定义**：启动过程中被 shutdown 中断时，/agent/start 的 HTTP 响应无定义 | 🟢 低 | 启动途中收到 shutdown | 返回 200 误导编排工具；返回 500 语义含糊；阻塞直到 shutdown 完成则调用者不知被中断 | 定义三种返回值：200 OK（正常就绪）、409 Conflict（被 shutdown 中断，body 含 status+phase）、500（不可恢复错误），调用者应检查 /health（§9.3 POST /agent/start 注释） |
|| 54 | **半加载插件资源回收在进程内模式下失效**：§2.5.4 半加载插件的"OS 在进程退出时自动回收"只在子进程/容器模式下成立，默认的进程内模式中 Agent 继续运行 | 🟡 中 | 进程内隔离模式下半加载插件 on_load 分配了 DB 连接/文件句柄后被中断 | 文件描述符泄漏 → ulimit 耗尽 → 所有文件操作失败；DB 连接泄漏 → 连接池耗尽；脏状态影响其他插件 | 按隔离模式区分策略：子进程/容器 → OS 回收；进程内（默认）→ 框架主动追踪（context.track_fd/track_db）+ 中断时释放；WASM → 运行时回收（§2.5.4 半加载插件资源回收） |
|| 55 | **POST /agent/start 幂等性未定义**：Agent 已在 Phase 5 运行时再次调用 start 的行为和返回值无定义 | 🟢 低 | 操作员误操作或编排脚本在 Agent 已运行时调用 start | 返回非 200 触发 CI/CD 误报；编排脚本需要额外状态检查逻辑 | 定义为幂等：已在 Phase 5 则立即返回 HTTP 200 OK，"启动"是安全的空操作（§9.3 POST /agent/start 注释） |
|| 56 | **YAML 示例 check_open_files 使用 true 而非推荐的 auto**：§3.2 文档节明确推荐 auto，但 §9.1 YAML 示例使用 true | 🟢 低 | 开发者在远程 FS 上部署时复制示例配置 | 锁检测在远程 FS 上不可靠 → incomplete 事件增多或事件丢失 | YAML 示例改为 check_open_files: auto，与文档节默认值一致（§9.1 YAML 配置已修复） |
|| 57 | **POST /agent/shutdown 行为未定义，与 start 不对称**：§9.3 为 start 定义了返回值/幂等性，相邻的 shutdown 只有一行注释 | 🟢 低 | 编排脚本在关闭后检查健康状态 | shutdown 是同步还是异步未知；幂等性未知；多次 shutdown 行为未知 | 补充同步阻塞语义、200 OK 返回值、幂等性（已关闭/关闭中返回 200）、超时建议（§9.3 POST /agent/shutdown 注释） |
|| 58 | **Chat 类事件源的 LLM 注入无防护**：ChatPlatformSource 用户消息直接传递给 LLM-based Skill，无输入消毒/输出校验 | 🟡 中 | 用户发送包含 prompt injection 指令的消息 | LLM-based Skill 被注入 → 敏感信息泄露 / 越权操作 | 输入信任等级分类（trusted/untrusted/sandboxed）+ System Prompt 加固 + 输入过滤 + 输出校验 + 敏感操作通过 Tool 执行（§9.3 LLM 注入防护） |
|| 59 | **Secret 解析未锚定到启动序列**：Secret 解析在启动序列中无对应 Phase，插件加载（Phase 2）时 Secret 可能尚未就绪 | 🟢 低 | Secret Store 网络延迟 / 配置中存在大量 ${VARIABLE} | 插件加载时 Secret 未就绪 → 启动失败 / 运行时动态解析增加延迟 | 启动序列新增 Phase 0.5 密钥解析阶段，锚定在 Phase 2 之前完成（§2.5.1 Phase 0.5） |
|| 60 | **静态配置与运行时 cron job ID 冲突未定义**：YAML 定义 id="daily-report"，运行时 API 也可添加/修改相同 id，重启后行为不确定 | 🟢 低 | 操作员通过运行时 API 修改 cron 配置后重启 Agent | 运行时修改在重启后丢失 / 或静态配置被运行时修改意外覆盖 | 定义 override 独立存储层 + 按 id 合并规则（运行时修改优先）+ 重启后合并生效（§6.4.1 静态配置与运行时修改的合并语义） |
|| 61 | **Physical isolation 模式下清理语义缺失**：Plugin/Skill disable/uninstall 后，独立的 SQLite 文件或 S3 prefix 永久残留 | 🟢 低 | 长期运行后禁用/卸载插件 | 数十个废弃存储碎片累积；S3/GCS 持续计费；PII 数据在 GDPR 合规角度须删除 | 定义 cleanup_policy（retain / delete_on_disable / delete_on_uninstall）+ 含 PII 数据强制清理 + 审计日志记录（§5.2 物理存储清理语义） |
|| 62 | **Metrics 格式未定义，可观测性互操作性受限**：GET /metrics 无输出格式说明，不同实现使用的 Prometheus/JSON 不一致 | 🟢 低 | 监控系统接入时发现格式不统一 | 无法统一接入 Grafana/Datadog/Prometheus；每个实现自定格式 | 定义至少支持 Prometheus exposition format + 核心指标列表（队列深度、吞吐量、背压级别、丢弃事件计数、inflight Pipeline 数等）（§9.3 GET /metrics） |
|| 63 | **Phase 3 Workflow 恢复无超时，与 Phase 2 不对称**：Phase 2 有 plugin_load_timeout: 30s，Phase 3 无对应超时定义 | 🟢 低 | State Store 负载高或积累了数十万 Workflow 实例 | Agent 在 Phase 3 无限卡死 → health/ready 一直 503 → 编排器误判杀掉重启 → 循环 | 定义 workflow_recovery_timeout: 120s（可配置）+ 超时后已恢复实例提交 checkpoint + 未恢复标记下次恢复 + 进度日志（§2.5.1 Phase 3） |
|| 64 | **YAML 配置 ERROR_EVENT 与核心定义 ERROR 不一致**：§9.1 YAML 示例使用 ERROR_EVENT 事件名，与 §3.7 核心转移表的 ERROR 事件名不一致 | 🟡 中 | 开发者从 §9.1 拷贝配置部署 | 转移表匹配失败 → Workflow 遇到错误时永不进入 ERROR 状态 → RETRY 恢复路径不可达 + ERROR→ARCHIVED 超时不触发 → 实例永久卡死 | YAML 示例事件名改为 `ERROR` 与 §3.7 核心定义对齐；状态名保持 YAML 小写风格（`error`）与同文件其他状态名一致（§9.1 YAML 已修复） |
|| 65 | **GET /audit-log 无访问控制与查询参数**：审计日志端点无分页/过滤/权限说明，与同节其他端点不对称 | 🟡 中 | 控制接口暴露到网络后访问 audit-log | 数十万条日志一次性返回 → OOM；操作员无法按类型/时间/操作员筛选；Secret 指纹哈希暴露密钥轮换时间线；攻击面侦查 | 定义游标分页 + type/time/operator 过滤 + 审计员级独立权限 + fingerprint 暴露改为时间戳 + 禁止全量导出（§9.3 GET /audit-log） |
|| 66 | **POST /dlq/{id}/retry 重试语义未定义**：手动重试后再次失败的路径、计数器语义、TTL 重置行为均未定义 | 🟢 低 | 操作员手动重试进入 DLQ 的事件，再次失败 | retry_count 累计超限后静默丢弃；TTL 重置导致 pre_expiry_alert 告警噪声；系统无法准确区分"管理员介入"和"自动重试" | 定义手动 retry 重置计数器 + 保留 original_retry_count 审计字段 + 重新入 DLQ 后 TTL 重置 + max_manual_retries: 5 全局上限（§9.3 POST /dlq/{id}/retry） |
|| 67 | **Phase 0.5 Secret Store 失败无重试策略**：Secret Store 短暂不可用时 Agent 硬终止，与编排器（K8s/Supervisor）构成无限重启循环 | 🟢 低 | Secret Store（Vault/AWS Secrets Manager）短暂网络抖动或维护窗口 | 进程退出 → 编排器自动重启 → Phase 0.5 再次失败 → 无限重启循环；Secret Store 可能在 5-30 秒内恢复，但 Agent 无等待机制 | 配置 secret_retry_count: 3 + 指数退避间隔 + 进度日志 + 所有重试用尽后才拒绝启动 + 可选本地缓存降级（§2.5.1 Phase 0.5） |
|| 68 | **ERROR 状态 RETRY vs CANCEL 优先级未定义**：CANCEL 的 from:ANY 覆盖 ERROR，与 RETRY 形成双出口竞态 | 🟢 低 | RETRY 和 CANCEL 事件同时到达或时间窗口紧密 | RETRY 先处理 → 恢复后立刻被 CANCEL 取消；CANCEL 先处理 → RETRY 事件静默丢弃 | 定义 CANCEL 在 ERROR 状态下附加隐式 guard（has_pending_retry 检查）+ retry_cancel_conflict_defer_ms: 5000 延迟窗口（§3.7 ERROR 双出口优先级规则） |
|| 69 | **/cron/add/update/remove 不在 reconfigure 鉴权范围内**：重新配置 §6.4 安全守卫只覆盖 PUT /event-source/{id}/config，遗漏了运行时添加/修改/删除 cron job 的 API | 🟢 低 | 通过 /cron/add 添加大量高频 cron jobs | 绕过 min_interval 和 rate_limit 守卫 → CRON_TICK 微风暴 → 事件风暴；无审计日志 → 变更不可追踪 | 明确三个端点受 §6.4 安全守卫约束（min_interval clamp + rate_limit + 审计日志）；将安全守卫范围从"reconfigure"扩大为"所有运行时 cron 变更操作"（§9.3 POST /cron/add/update/remove） |
|| 70 | **核心大写状态名与 YAML 小写状态名不一致，无大小写敏感性声明**：§3.7 全大写状态名（PENDING/ERROR）与 §9.1 YAML 全小写（pending/error）不一致，文档未声明比较策略 | 🟡 中 | 框架实现使用大小写敏感比较（如 Rust ==、Java equals） | §9.1 YAML 中所有转移表全部使用小写状态名 → 无法匹配 §3.7 大写状态定义 → 整个 Workflow 转移表静默失效 | 在 §3.7 明确定义状态名字段约束：运行时大小写不敏感 + 框架 normalize 为统一大写 + 配置校验发警告（§3.7 状态名字段约束） |
|| 71 | **§6.4 reconfigure 权限模型未同步更新 /cron/add/update/remove 跨引用**：R15 #6 在 §9.3 标注了安全守卫约束，但 §6.4 定义未同步扩大范围 | 🟢 低 | 实现者按 §6.4 开发安全守卫 | 只实现了 PUT /event-source/{id}/config 的保护，遗漏 POST /cron/add/update/remove | 将 §6.4 权限模型标题改为"运行时 cron 变更操作"，范围从 reconfigure 扩大至覆盖 PUT + POST 端点（§6.4 运行时 cron 变更操作的权限模型） |
|| 72 | **状态图未反映 ERROR→CANCEL 路径**：转移表允许 CANCEL 从 ERROR 退出，状态图未画出对应箭头 | 🟢 低 | 开发者通过状态图理解 Workflow 行为 | 以为 CANCEL 在 ERROR 上不合法 → 不确定是否可绕过 RETRY 直接放弃实例 | 状态图 ERROR 框下新增 CANCEL→CANCELLED 箭头 + 注释"隐式 guard: 见 §3.7 双出口规则"（§3.7 状态图） |
|| 73 | **secret_cache_fallback 本地缓存安全性未定义**：Phase 0.5 允许读取本地缓存作为 Secret Store 降级，但存储加密/文件权限/TTL/进程 dump 保护均未定义 | 🟢 低 | 开发者启用 secret_cache_fallback: true | 缓存文件明文存储 → 攻击者通过文件系统漏洞读取所有 Secret；core dump 包含 Secret 缓存 → 调试中泄露；缓存 TTL 与 Secret Store token TTL 不对齐 → 认证失败 | 补充 5 项安全约束：AES-256-GCM 加密 + 文件权限 600 + 默认 TTL 300s + 不存储明文 + Phase 1 不可用（§2.5.1 Phase 0.5 secret_cache_fallback 安全约束） |
|| 74 | **§9.1 workflow 状态名小写问题影响全部 transition**：R15 #1 修复了 ERROR 事件名对齐但保留了状态名体系大写 vs 小写的根因矛盾 | 🟢 低 | 框架大小写敏感比较时 | 所有 transition 的状态名不匹配，不仅是 ERROR 一条 | 已由 §3.7 状态名字段约束统一解决——声明大小写不敏感 + 框架 normalize（R16 #1 修复同时覆盖此问题） |
|| 75 | **force_publish_on_timeout 枚举值混合 boolean 和 string 类型**：三值枚举跨 boolean（true/false）和 string（mark_incomplete）两种类型 | 🟢 低 | YAML 解析器将 true/false 解析为原生 boolean | 配置校验需要特殊处理（区分 bool/str 类型）；true 和 mark_incomplete 行为差异不直观 | 统一为纯字符串枚举：mark_incomplete | publish_anyway | none（§3.2 force_publish_on_timeout 参数） |
|| 76 | **retry_backoff 命名和值格式在五处重试机制中不统一**：WAL→内存重试、Pipeline step、Compensation、error_recovery、Secret 重试使用不同的字段名和值格式（backoff / retry_backoff / secret_retry_backoff + 隐式序列 / CSV / enum） | 🟢 低 | 开发者需要在五种语境中学习四种不同的配置方式 | cross-context 复制错误（如把 retry_backoff: "15s" 从 error_recovery 误用于 Compensation）；WAL→内存重试无配置入口不可调优 | 统一字段名为 retry_backoff 全局 + 标准化值格式（exponential / exponential:base:factor:max / fixed:duration / sequence:csv / immediate）+ 为 WAL→内存增加 wal_retry_backoff 配置项（§10 决策 11） |
|| 77 | **§3.7 状态名 normalize 规则未在设计决策中记录**：关键架构约束（状态名大小写不敏感 + 框架 normalize）藏在 Workflow 代码块 // 注释中，实现者可能只读 §3.7 规范段落和 §10 设计决策而错过此规则 | 🟢 低 | 实现者未注意到代码块内的 // 注释 | 使用大小写敏感比较 → §3.7 大写状态与 §9.1 小写 YAML 不匹配 → 全部转移表破裂 | 在 §10 新增决策 10（状态名大小写不敏感）记录 normalize 规则 + 理由 + 代价 + 缓解（§10 决策 10） |
|| 78 | **retry_queue_depth 不在核心 /metrics 指标列表中**：三级联锁（主队列满 → 待重试队列满 → WAL 阻塞）中，待重试队列是唯一无直接可观测指标的环节 | 🟢 低 | 待重试队列接近 retry_queue_max（默认 1000） | WAL checkpoint 停滞 → WAL 段无限累积 → 磁盘写满 → 系统崩溃；操作员只能从 WAL 段大小异常增长间接推断 | 在核心指标列表中新增 retry_queue_depth + 接近上限时预警 + 建议在背压 Level 2-3 触发条件中增加 80% 预警阈值（§9.3 GET /metrics） |
|| 79 | **Pipeline 补偿与 Workflow ERROR 状态组合交互未定义**：Pipeline 作为 Workflow transition 的 action 执行失败时，两套错误处理机制各自独立运行，组合后可能出现"补偿已回滚但 Workflow 已转移到目标状态"的不一致 | 🟢 低 | Workflow transition 的 action 使用 Pipeline（最自然的组合模式） | 金融审批流程中 Pipeline step 1 扣款成功 → step 2 失败 → 补偿退款成功 → Workflow 已在 APPROVED 并通知用户"审批通过" — 用户看到通过但未生效 | 定义 4 条组合规则：Pipeline 失败 → Workflow 默认进入 ERROR + 补偿失败实例标记 partial_rollback: true + CANCEL 等待 inflight Pipeline 完成 + RETRY 恢复后重新执行需幂等保证（§3.7 Pipeline 与 Workflow 组合约束） |
|| 80 | **§3.5 Pipeline notify-slack 步骤 backoff 字段名残留**：R17 #1 标准化修复执行后，notify-slack 步骤的 `backoff` 字段未随其他步骤一起改为 `retry_backoff` | 🟢 低 | 开发者复制 notify-slack 步骤配置作为模板 | 使用旧的 backoff 字段名 → 框架严格校验时配置失败；或用宽松匹配接受别名 → 与决策 11 的"统一"目标矛盾 | 将 line 627 `backoff` 改为 `retry_backoff`，与其余 5 处统一（§3.5 notify-slack 步骤已修复） |
|| 81 | **风险清单 #76 声明"已统一"但正文仍有残留**：风险清单 #76 的应对策略列声称字段名已统一，但 §3.5 line 627 仍有 `backoff` 残留 | 🟢 低 | 操作员阅读风险清单时认为 #76 已关闭 | 断言为已完成但实未完成 → 信任链断裂；后续新开发者参考 #76 应对策略时以为全部完成 | line 627 修复后此残留自然消除；#76 无需修改条目文本（R18 #1 修复同时解决此问题） |

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
