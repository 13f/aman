# Aman Agent Framework — Milestone Roadmap

> 基于 [agent-design.md](./agent-design.md) 和 [architect-design.md](./architect-design.md)
> 每个里程碑包含可直接分配给开发者的具体任务。

---

## 里程碑总览

```
M1  基础骨架         ████████░░░░░░░░░░░░  第 1-2 周
M2  事件总线         ████████████░░░░░░░░  第 2-4 周
M3  事件源           ████████████████░░░░  第 4-5 周
M4  分发 + 管道      ████████████████████  第 5-7 周
M5  Skill + Tool     ████████████████████  第 7-9 周
M6  Workflow 状态机  ████████████████████  第 9-11 周
M7  插件系统         ████████████████████  第 11-13 周
M8  持久化层         ████████████████████  第 13-15 周
M9  安全与配置       ████████████████████  第 15-16 周
M10 运行时 + API     ████████████████████  第 16-18 周
M11 可观测性         ████████████████████  第 18-19 周
M12 Tauri 桌面端     ████████████░░░░░░░░  第 19-21 周
M13 集成与打磨       ████████░░░░░░░░░░░░  第 21-22 周
```

---

## M1: 基础骨架 (Foundation)

**目标**: 搭建 Cargo workspace，定义所有核心类型和 Trait，建立错误处理体系。
**项目名称**：aman
**项目介绍**：aman = a man, an agent man. 目标是创建一个以事件响应为核心的拟人 agent 框架。

### 1.1 Workspace 初始化
- [ ] 创建根 `Cargo.toml`，定义 `[workspace]` 成员
- [ ] 初始化 19 个 crate 骨架（见 architect-design.md §2）
- [ ] 配置 `rust-toolchain.toml`（stable channel）
- [ ] 配置 `.cargo/config.toml`（编译优化、deny unsafe）
- [ ] 添加 `Cargo.lock` 到版本控制

### 1.2 aman-core: 核心类型 (`crates/aman-core/`)
- [ ] 统一核心字段命名并冻结 schema（`Event.type` vs `event_type`、`EventMetadata.ttl` vs `ttl_ms`）
- [ ] 实现 `Event` 结构体（id, source, event_type, timestamp, priority, delivery, dedup_key, payload, metadata）
- [ ] 实现 `EventMetadata`（trace_id, parent_event_id, retry_count, max_retries, ttl, lifespan_ms, created_at）
- [ ] 实现 `EventType` 枚举（所有内置事件类型：file_created, cron_tick, message_received 等）
- [ ] 实现 `Priority` 枚举（High=0, Normal=1, Low=2）
- [ ] 实现 `DeliveryGuarantee` 枚举（AtMostOnce, AtLeastOnce, ExactlyOnce）
- [ ] 实现 `DedupKey` 类型（source+type+payload_hash 缺省算法）
- [ ] 实现 `TraceId` 和 `SourceId` newtype
- [ ] 实现 `Timestamp` newtype（UTC epoch 毫秒）
- [ ] 定义 `EventSource` trait（id, source_type, init, shutdown, poll, on_backpressure, health, pause, resume, reconfigure）
- [ ] 定义 `SourceType` 枚举（Timer, File, Network, Webhook, Data, Platform, Custom）
- [ ] 定义 `Pipeline` trait（id, concurrency, steps, execute）
- [ ] 定义 `PipelineStep` 结构体（id, step_type, tool, compensate, retry）
- [ ] 定义 `StepType` 枚举（Filter, Transform, Action）
- [ ] 定义 `ConcurrencyModel` 枚举（Serial, Parallel, Limited(N)）
- [ ] 定义 `Skill` trait（name, version, description, triggers, execute, on_load, on_unload）
- [ ] 定义 `TriggerCondition` 结构体
- [ ] 定义 `Tool` trait（name, mode, parameters, returns, execute）
- [ ] 定义 `ToolMode` 枚举（Local, Remote, Container, Sandbox）
- [ ] 定义 `Plugin` trait（name, version, dependencies, on_load, on_unload, on_dependency_unloading, event_sources, skills, tools）
- [ ] 定义 `Hook` trait（name, priority, hook_points, execute）
- [ ] 定义 `HookPoint` 枚举（全部 25+ 钩子点）
- [ ] 定义所有 Context 结构体（SkillContext, PipelineContext, ToolContext, HookContext, PluginContext, SourceContext）

### 1.3 aman-core: 错误处理
- [ ] 定义 `AmanError` 枚举（所有错误变体：BusFull, Timeout, VersionMismatch, CycleDetected, CompensationFailed, Unrecoverable, ConfigInvalid, SecretUnresolved 等）
- [ ] 定义 `AmanResult<T>` 类型别名
- [ ] 实现 `Display` + `Error` trait for `AmanError`
- [ ] 实现 `From` 转换（serde_json::Error, io::Error, 等）

### 1.4 aman-core: 共享工具类型
- [ ] 实现 `JsonSchema` 类型（参数/返回值 schema）
- [ ] 实现 `RetryPolicy` 结构体（max_attempts, retry_backoff）
- [ ] 实现 `RetryBackoff` 解析器（支持 exponential / fixed:N / sequence:N,N / immediate 四种格式，见决策 11）
- [ ] 实现 `BackpressureLevel` 枚举（Normal, L1, L2, L3, L4A, L4B, Critical）
- [ ] 实现 `HealthStatus` 枚举（Ok, Degraded, Failed）
- [ ] 实现 `CompensationStrategy` 枚举（reverse_order）
- [ ] 实现 `CompensationContract` 结构体（idempotent, timeout_sec, retry_count, retry_backoff, on_failure）

### 1.5 aman-macros: 过程宏
- [ ] 实现 `#[aman_skill]` 属性宏
- [ ] 实现 `#[aman_plugin]` 属性宏
- [ ] 编写宏的单元测试

### 1.6 验证
- [ ] 所有 crate 可编译 (`cargo build --workspace`)
- [ ] 无 clippy 警告 (`cargo clippy --workspace -- -D warnings`)
- [ ] 核心类型单元测试覆盖率 > 80%

---

## M2: 事件总线 (Event Bus)

**目标**: 实现内存总线 + 背压系统 + 去重 + 同源保序。

### 2.1 InMemoryBus (`crates/aman-event-bus/`)
- [ ] 定义 `EventBus` trait（publish, subscribe, unsubscribe, metrics, backpressure_level）
- [ ] 实现 `InMemoryBus` 结构体
- [ ] 实现优先级队列（BinaryHeap + per-source FIFO segments）
- [ ] 实现 `SubscriptionFilter` 结构体（event_types, sources, priorities, payload_match）
- [ ] 实现 `subscribe` / `unsubscribe` 逻辑
- [ ] 实现 `EventHandler` trait（async 处理函数）
- [ ] 实现 `SubscriptionId` 分配

### 2.2 背压系统 (`backpressure.rs`)
- [ ] 实现 `BackpressureController` 结构体
- [ ] 实现 Level 1（80% → 降低 AT_MOST_ONCE 注入优先级）
- [ ] 实现 Level 2（90% → 丢弃 AT_MOST_ONCE + 记日志）
- [ ] 实现 Level 3（95% → 阻塞 poll() + Push 来源暂停 + Webhook 返回 503）
- [ ] 实现 Level 4A（98% → AT_LEAST_ONCE 溢出到磁盘）
- [ ] 实现 Level 4B（溢出目录 ≥80% → 紧急告警 + 回退 Level 3）
- [ ] 实现 Level 5（100% → 停止低优先事件源）
- [ ] 实现 `backpressure_signal` 通知机制（Push 来源接收信号暂停 publish）
- [ ] 实现溢出磁盘管理（overflow_max_bytes, 溢出目录扫描与重放）
- [ ] 实现溢出重启恢复（重启时自动扫描 overflow/ 目录 → 排序注入 → 去重）

### 2.3 去重窗口 (`dedup.rs`)
- [ ] 实现 `DedupWindow` 结构体（BloomFilter + LRU）
- [ ] 实现 `BloomFilter` 快速拒绝
- [ ] 实现 `LruCache<DedupKey, Uuid>` 精确去重（30s 窗口）
- [ ] AT_MOST_ONCE 事件跳过 hash 计算（优化）
- [ ] UUID v7 事件使用 event.id 作为 dedup_key（避免 hash）

### 2.4 同源保序 (`ordering.rs`)
- [ ] 实现 `OrderedQueue` 结构体（per-source VecDeque + global BinaryHeap）
- [ ] 实现 push → 按 source 分段入队
- [ ] 实现 pop → 从各段头部收集候选 → 跨源优先级排序 → 同源 FIFO 不变
- [ ] 实现优先级与保序冲突规则：同源保序优先，跨源优先级生效
- [ ] 单元测试：验证同源 HIGH 不跳过同源 NORMAL

### 2.5 待重试队列
- [ ] 实现 `RetryQueue` 结构体（独立于主队列）
- [ ] 实现 WAL 确认后内存投递失败 → 入待重试队列
- [ ] 实现重试退避（100ms → 500ms → 2s，最大 5 次）
- [ ] 实现 `retry_queue_max: 1000` 上限
- [ ] 实现队列满时阻塞 WAL checkpoint 推进（三级联锁）

### 2.6 总线指标
- [ ] 实现 `BusMetrics` 结构体（queue_depth, throughput, backpressure_level, discarded_count）
- [ ] 实现指标采集（实时更新）

### 2.7 验证
- [ ] 单元测试：publish/subscribe 基本流程
- [ ] 集成测试：5 级背压触发与恢复
- [ ] 集成测试：去重窗口（同 dedup_key 30s 内重复 → 丢弃）
- [ ] 集成测试：同源保序（A→B→C 出队顺序不变）
- [ ] 压力测试：10K events/s 吞吐

---

## M3: 事件源 (Event Sources)

**目标**: 实现所有内置事件源类型。

### 3.1 EventSource 基础设施 (`crates/aman-source/`)
- [ ] 实现 `SourceRegistry` 结构体（注册/查找/管理）
- [ ] 实现 `SourceMode` 标记（Pull vs Push）
- [ ] 实现统一的事件源生命周期管理（init → running → pause/resume → shutdown）
- [ ] 实现统一 `trust_level` 配置（trusted | untrusted | sandboxed，默认 `untrusted`）
- [ ] 实现 `trust_level` 向 Dispatcher / LLM 防护链路传递（路由阶段自动附加安全约束）

### 3.2 TimerSource (`timer.rs`)
- [ ] 实现固定间隔定时器（tokio::time::interval）
- [ ] 实现 `heartbeat: true` 心跳模式（产出 heartbeat 事件）
- [ ] 实现 `catch_up: skip`（默认，跳过错过的）
- [ ] 实现 `reconfigure`（动态调整间隔）
- [ ] 测试：间隔精度验证

### 3.3 CronSource (`cron.rs`)
- [ ] 集成 `cron` crate 解析 cron 表达式
- [ ] 支持 5 字段（标准）和 6 字段（秒级）
- [ ] 实现时区支持（`timezone` 配置，默认 UTC）
- [ ] 实现夏令时策略（skip | repeat_once | wall_clock）
- [ ] 实现 `catch_up` 策略（skip | latest | all）
- [ ] 实现 `rate_limit` 安全守卫（最小间隔 1s，每秒最多 100 个 CRON_TICK）
- [ ] 实现 `rate_limit_overflow: delay`（超额延迟而非丢弃）
- [ ] 实现 `leader_election` 支持（可选，主备模式防重复）
- [ ] 实现运行时管理接口（CronManager: add/remove/update/pause/resume/list/get_next_run）
- [ ] 实现 `cron_override.yaml` 持久化（见 §6.4.1 合并语义）
- [ ] 实现审计日志（每次 cron 变更记录 old_interval, new_interval, caller, timestamp）
- [ ] 测试：时区转换正确性
- [ ] 测试：夏令时边界行为
- [ ] 测试：catch_up 恢复事件注入限速

### 3.4 FileWatchSource (`file_watch.rs`)
- [ ] 集成 `notify` crate 实现跨平台文件监控
- [ ] 实现"稳定确认"机制（debounce 500ms + 文件锁检测）
- [ ] 实现 `check_open_files` 三值模式（auto | true | false）
- [ ] 实现 `force_publish_on_timeout` 枚举（mark_incomplete | publish_anyway | none）
- [ ] 实现远程文件系统检测（auto 模式自动跳过锁检测）
- [ ] 测试：debounce 正确（快速连续写入只触发一次）
- [ ] 测试：incomplete 标记（文件超 max_stable_wait 仍未关闭）

### 3.5 WebhookSource (`webhook.rs`)
- [ ] 实现 HTTP 服务器监听（axum）
- [ ] 实现 `path` 配置（回调 URL 路径）
- [ ] 实现 `port` 配置
- [ ] 实现背压时返回 HTTP 503
- [ ] 实现 `trust_level` 配置（trusted | untrusted | sandboxed）
- [ ] 测试：Webhook 事件正确注入 Event Bus

### 3.6 SignalSource (`signal.rs`)
- [ ] 监听 OS 信号（SIGTERM, SIGINT, SIGHUP, SIGUSR1）
- [ ] 信号到达 → 产出 `SYSTEM_SIGNAL` 事件
- [ ] 测试：SIGTERM 事件产出的 pipeline 响应

### 3.7 SocketSource (`socket.rs`)
- [ ] 实现 TCP/UDP/Unix Domain Socket 监听
- [ ] 实现 Push 模式（接收数据 → publish）
- [ ] 实现背压时暂停接收（TcpUserTimeout）

### 3.8 验证
- [ ] 集成测试：所有事件源注册 → 启动 → 产事件 → Event Bus 接收
- [ ] 集成测试：事件源 pause/resume
- [ ] 集成测试：背压 Level 3 时 Push 来源暂停 + Webhook 返回 503

---

## M4: 分发器 + Pipeline 引擎

**目标**: 实现事件路由分发和链式处理管道。

### 4.1 Dispatcher (`crates/aman-dispatcher/`)
- [ ] 实现 `Dispatcher` 结构体
- [ ] 实现 `RouteRule` 路由规则表
- [ ] 实现 `MatchCondition` 匹配引擎（Type, Source, TypeAndSource, Priority, PayloadMatch, All, Any, Custom）
- [ ] 实现 `DispatchTarget` 枚举（Pipeline, Skill, Workflow, Hook, FanOut）
- [ ] 实现 `TransformRule` 转换引擎（Event → Vec<Event>）
- [ ] 实现 `FilterRule` 过滤规则（rate_limit 防抖）
- [ ] 实现路由优先级（同事件命中多条规则时按 priority 字段排序）
- [ ] 实现 `rebuild_routes` 动态重建路由表（插件/Skill 变更时）
- [ ] 实现 `SubscriptionFilter` → `MatchCondition` 转换

### 4.2 Pipeline 引擎 (`crates/aman-pipeline/`)
- [ ] 实现 `PipelineEngine` 执行引擎
- [ ] 实现 `PipelineInstance` 运行时（id, compensation_stack, temp_dir）
- [ ] 实现步骤执行循环：Filter → Transform → Action
- [ ] 实现步骤级别重试（RetryPolicy: max_attempts + retry_backoff）
- [ ] 实现输出事件产出（全部成功 → publish Output Event）

### 4.3 补偿引擎 (`compensation.rs`)
- [ ] 实现 `CompensationEngine` 结构体
- [ ] 实现 `reverse_order` 补偿（C_N → C_(N-1) → ... → C_1）
- [ ] 实现补偿操作的独立重试（compensation_contract.retry_count）
- [ ] 实现补偿超时保护（compensation_contract.timeout_sec: 30）
- [ ] 实现 `CompensationResult`（FullyCompensated | PartiallyCompensated）
- [ ] 实现 COMPENSATION_FAILED 中间态 + 告警
- [ ] 实现补偿状态日志（记录哪些步骤已补偿、哪些失败）

### 4.4 并发控制 (`concurrency.rs`)
- [ ] 实现 `ConcurrencyController` 结构体
- [ ] 实现 Serial 模式（单实例队列）
- [ ] 实现 Parallel 模式（强制 optimistic_lock + 独立 temp_dir）
- [ ] 实现 Limited(N) 模式（AsyncSemaphore）
- [ ] 实现 parallel 模式的安全条件校验：
  - StateStore 使用 optimistic_lock
  - 每个实例独立临时目录
  - 补偿操作按实例 scope 隔离
  - 框架自动注入实例隔离上下文

### 4.5 Dead Letter Channel
- [ ] 实现 Pipeline 失败 → 事件入 DLQ
- [ ] 记录 DLQ 原因（PipelineFailed, CompensationFailed）

### 4.6 验证
- [ ] 集成测试：Dispatcher 路由分发（event → pipeline/skill/workflow）
- [ ] 集成测试：Pipeline 正常执行（3 步全成功 → 产出输出事件）
- [ ] 集成测试：Pipeline 失败 + 补偿全部成功
- [ ] 集成测试：Pipeline 失败 + 补偿部分失败 → COMPENSATION_FAILED
- [ ] 集成测试：Serial / Parallel / Limited(N) 并发模型

---

## M5: Skill 系统 + Tool Runner

**目标**: 实现技能注册/发现/执行和工具执行框架。

### 5.1 Skill 系统 (`crates/aman-skill/`)
- [ ] 实现 `SkillRegistry` 结构体（注册/查询/启用/禁用）
- [ ] 实现声明式 Skill 加载（YAML → Skill 实例）
- [ ] 实现发现式 Skill 加载（扫描 `~/.aman/skills/` 目录）
- [ ] 实现 SKILL.md 格式解析器
- [ ] 实现 `TriggerCondition` 匹配引擎
- [ ] 实现 Skill 并发模型（serial / parallel / limited(N)）

### 5.2 Skill 检索 (`search.rs`)
- [ ] 集成 `tantivy` 全文检索引擎
- [ ] 实现 `SkillSearch` 结构体
- [ ] 实现 `index_skill`（索引 Skill 元信息）
- [ ] 实现 `search`（关键词/字段/语义标签/模糊匹配）
- [ ] 实现 `remove_skill`
- [ ] 实现 `SkillMatch` 结果（name, version, score, snippet, matched_field）

### 5.3 热加载 (`hot_reload.rs`)
- [ ] 实现 `HotReloadManager` 结构体
- [ ] 集成 `notify` 监控 skills/ 目录
- [ ] 实现 debounce 500ms → 完整性检查 → 解析
- [ ] 实现版本比较（同版本 Arc::swap / 新版本注册 + 旧版 drain）
- [ ] 实现 Search Index 更新
- [ ] 实现 Dispatcher 路由刷新通知

### 5.4 版本管理 (`version.rs`)
- [ ] 实现 `SkillVersionManager` 结构体
- [ ] 实现版本历史存储（`~/.aman/skills/history/`）
- [ ] 实现 `rollback` 回滚到指定版本
- [ ] 实现 `history` 查看历史
- [ ] 实现 `diff` 比较版本差异

### 5.5 Tool Runner (`crates/aman-tool/`)
- [ ] 实现 `ToolRegistry` 结构体（注册/查找）
- [ ] 实现 `ToolRunner` 6 步执行流程：
  1. 参数校验
  2. 安全检查（白名单路径/网络/命令）
  3. 资源分配（超时/内存/临时目录）
  4. 执行（Builtin/Script/API/Container）
  5. 输出转换（统一格式）
  6. 清理（释放临时资源）

### 5.6 内置工具 (`builtin/`)
- [ ] 实现 `file` 工具（文件读写/删除/移动）
- [ ] 实现 `http` 工具（HTTP 请求，支持 REST/GraphQL）
- [ ] 实现 `exec` 工具（执行外部命令，安全约束：超时 + 资源限制 + 白名单命令）
- [ ] 实现 `db` 工具（数据库查询，SQL 参数化防注入）

### 5.7 沙箱 (`sandbox.rs`)
- [ ] 实现 `SandboxConfig`（allowed_paths, network_allowed, max_memory）
- [ ] 实现子进程隔离（std::process::Command + 超时 kill）
- [ ] 实现容器工具接口（Docker SDK 预留）
- [ ] 实现 WASM 工具接口（wasmtime 预留）

### 5.8 验证
- [ ] 单元测试：Skill 触发条件匹配
- [ ] 集成测试：Skill 加载 → 事件触发 → execute
- [ ] 集成测试：Skill 热加载（修改 SKILL.md → 自动重载）
- [ ] 集成测试：Tool 6 步执行 + 安全约束验证
- [ ] 集成测试：Tool 执行超时 → 资源清理

---

## M6: Workflow 状态机

**目标**: 实现完整的状态机引擎，支持状态转移、超时、ERROR 恢复。

### 6.1 Workflow 定义 (`crates/aman-workflow/`)
- [ ] 实现 `WorkflowDef` 结构体（name, states, initial_state, final_states, error_state, transitions, state_timeouts, error_recovery）
- [ ] 实现 `StateDef` 结构体
- [ ] 实现 `Transition` 结构体（from, event, to, guard, on_fail, action, on_action_failure）
- [ ] 实现 `TransitionFrom` 枚举（Specific(state) | Any）
- [ ] 实现 `TransitionTo` 枚举（Specific(state) | LastActiveState）
- [ ] 实现 `StateTimeout` 结构体（timeout, on_timeout, on_timeout_alert）

### 6.2 Workflow 实例 (`instance.rs`)
- [ ] 实现 `WorkflowInstance` 结构体（id, workflow_name, current_state, last_active_state, total_retry_count, session_retry_count, state_entered_at, timeout_clock, data, partial_rollback）
- [ ] 实现 `TimeoutClock` 跨状态暂停计时器
- [ ] 实现状态名 normalize（大小写不敏感，统一转大写比较）

### 6.3 状态转移引擎
- [ ] 实现 `WorkflowEngine` 结构体
- [ ] 实现 `handle_event` 核心流程：
  1. 提取 workflow_instance_id
  2. 加载实例
  3. 匹配 Transition（normalize 大写比较）
  4. 检查 guard（total_retry_count < max_retry_count 等）
  5. guard 失败 → on_fail 策略
  6. 执行 action（Pipeline/Skill）
  7. action 失败 → on_action_failure（默认 ERROR）
  8. 状态转移（on_leave → update → on_enter → 终态 → on_final）
  9. 持久化到 StateStore
  10. 发布 Workflow 状态变更事件

### 6.4 Guard 条件 (`guard.rs`)
- [ ] 实现 `Guard` 接口（接受 instance + event → bool）
- [ ] 实现内置 guard：hasPermission, total_retry_count < max_retry_count
- [ ] 实现自定义 guard 注册

### 6.5 超时管理 (`timeout.rs`)
- [ ] 实现 `TimeoutManager` 结构体
- [ ] 实现 `on_state_enter` 启动超时计时器
- [ ] 实现 `on_state_exit` 处理（pause | reset | continue，默认 pause）
- [ ] 实现超时触发 → 自动转移（如 REVIEWING→REJECTED）
- [ ] 实现超时事件与用户事件竞态规则（用户事件优先，超时事件延迟窗口内二次检查）
- [ ] 实现超时与用户事件竞态处理（timeout_defer_ms: 5000）
- [ ] 实现 ERROR 状态超时前分级告警（1d/6h/1h）

### 6.6 ERROR 恢复
- [ ] 实现 ERROR on_enter 默认行为（保存 last_active_state + 告警）
- [ ] 实现 session_retry_count 重置 / total_retry_count 累计
- [ ] 实现 RETRY 事件 → 恢复到 last_active_state
- [ ] 实现 auto_retry_count（0=手动，>0=自动重试 N 次）
- [ ] 实现 retry_backoff 延时策略（immediate | fixed:N | exponential | sequence:N,N）
- [ ] 实现 total_retry_count ≥ max_retry_count → on_retry_failure（archive | manual_only）
- [ ] 实现 ERROR→CANCEL 双出口优先级（CANCEL 附加隐式 guard: has_pending_retry + defer 5000ms）

### 6.7 Pipeline 与 Workflow 组合
- [ ] 实现 Pipeline 作为 transition action 失败 → Workflow 进入 ERROR
- [ ] 实现补偿失败标记（partial_rollback: true）
- [ ] 实现 CANCEL 等待 inflight Pipeline 完成
- [ ] 实现 RETRY 恢复后重新执行 Pipeline 的幂等性要求

### 6.8 终态回收
- [ ] 实现 ARCHIVED 状态 30 天后自动清理/归档冷存储
- [ ] 实现终态超时（APPROVED/REJECTED/CANCELLED → 30d → ARCHIVED）

### 6.9 验证
- [ ] 单元测试：状态转移基本流程（PENDING → SUBMIT → REVIEWING → APPROVE → APPROVED）
- [ ] 单元测试：guard 失败留在原状态
- [ ] 单元测试：超时自动转移
- [ ] 单元测试：ERROR → RETRY → 恢复 → 再 ERROR → 超过上限 → ARCHIVED
- [ ] 单元测试：状态名大小写不敏感
- [ ] 集成测试：Workflow + Pipeline 组合（Pipeline 失败 → Workflow ERROR → RETRY）
- [ ] 集成测试：超时时钟 pause 语义（REVIEWING→ERROR→RETRY→REVIEWING，剩余时间继续）

---

## M7: 插件系统

**目标**: 实现插件加载、生命周期管理、依赖解析、隔离策略。

### 7.1 插件基础设施 (`crates/aman-plugin/`)
- [ ] 实现 `PluginManifest` 结构体（plugin.yaml 解析）
- [ ] 实现 `plugin.yaml` 格式定义（name, version, depends_on, lifecycle, exports, config_schema）
- [ ] 实现 `PluginDependency` 结构体（name, version_range）
- [ ] 实现 SemVer 范围匹配（>=2.0 <3.0 格式）

### 7.2 插件加载器 (`loader.rs`)
- [ ] 实现 `PluginLoader` 结构体
- [ ] 实现 `DependencyGraph` 构建（DAG）
- [ ] 实现拓扑排序 + 环检测（有环 → 加载失败 + 报告环路径）
- [ ] 实现按拓扑序加载
- [ ] 实现版本兼容性检查（运行版本 vs 声明的 range）
- [ ] 实现依赖缺失/版本不匹配 → 整链加载失败（不半加载）
- [ ] 实现卸载（反向拓扑序 + on_dependency_unloading 通知 + 30s 硬超时）

### 7.3 插件生命周期 (`lifecycle.rs`)
- [ ] 实现生命周期状态机：Loaded → Enabled → Running → Paused/Disabled → Shutdown
- [ ] 实现 on_load 钩子
- [ ] 实现 on_unload 钩子
- [ ] 实现 on_dependency_unloading 通知
- [ ] 实现 `PluginContext` 资源追踪 API（如 `track_fd` / `track_db` / `track_path`），供进程内插件 `on_load` 使用
- [ ] 实现连续 3 次卸载超时标记 unstable

### 7.4 插件隔离 (`isolation.rs`)
- [ ] 实现四种隔离模式：
  - InProcess（Arc<dyn Plugin> 接口隔离）
  - Subprocess（stdin/stdout JSON-RPC IPC）
  - Container（Docker SDK）
  - Wasm（wasmtime runtime）
- [ ] 实现 WASM 插件加载（wasmtime::Module + Instance）
- [ ] 实现 WASM 导出的函数接口（aman_skill_execute, aman_skill_on_load, aman_skill_on_unload）

### 7.5 半加载插件中断处理
- [ ] 区分三种加载状态：全加载 / 半加载 / 未加载
- [ ] 全加载 → 正常走卸载流程
- [ ] 半加载 → 跳过 on_unload + 按隔离模式回收资源：
  - 子进程/容器 → OS 自动回收
  - 进程内 → 框架主动追踪（context.track_fd/track_db）+ 中断时释放
  - WASM → 运行时回收
- [ ] 实现 `on_load` 中断时的告警与资源释放审计日志
- [ ] 记录告警日志

### 7.6 插件安装/卸载
- [ ] 实现插件安装（POST /plugin/install, multipart: plugin.tar.gz）
- [ ] 实现插件卸载（on_unload → 清理注册 → 删除文件）

### 7.7 SOUL 系统 (`crates/aman-soul/`)
- [ ] 实现 `Soul` 结构体（name, identity, core, expertise, boundaries, vibe, preferences, raw）
- [ ] 实现 `SOUL.md` 解析器（`from_file` / `from_str`）
- [ ] 实现 `to_system_prompt`（生成运行时 System Prompt）
- [ ] 实现 `check_boundary`（运行前边界检查）
- [ ] 实现 SOUL 注入到 `SkillContext` / `PipelineContext`
- [ ] 实现 SOUL 文件热更新 → 发布 `SoulChanged` 事件 → 运行时刷新引用

### 7.8 验证
- [ ] 单元测试：拓扑排序正确（A→B→C）
- [ ] 单元测试：环检测正确（A→B→A → Err）
- [ ] 单元测试：版本不匹配拒绝加载
- [ ] 集成测试：插件加载 → 注册 Skills + Tools + EventSources
- [ ] 集成测试：插件卸载 → 通知依赖方 → 清理注册
- [ ] 集成测试：WASM 插件执行 Skill
- [ ] 集成测试：SOUL 热更新后新执行上下文拿到最新约束

---

## M8: 持久化层

**目标**: 实现 WAL、StateStore、DLQ、溢出管理、Checkpoint。

### 8.1 WAL (`crates/aman-persistence/`)
- [ ] 实现 `WriteAheadLog` 结构体
- [ ] 实现 `append`（事件 → WAL → fsync → 返回偏移量）
- [ ] 实现 `checkpoint`（记录已处理偏移量）
- [ ] 实现 `replay_from_checkpoint`（崩溃恢复：从 checkpoint 偏移量重放）
- [ ] 实现 `final_checkpoint`（关闭前最终写入）
- [ ] 实现 WAL 段轮转（rotate_bytes: 1GB）
- [ ] 实现 `wal_sync` 模式（Fsync | Batch）
- [ ] 实现 `replay_checkpoint` 文件（断点持久化，见 §2.5.1）
- [ ] 实现 `wal_replay_buffer_max: 5000` 缓冲区上限
- [ ] 实现 `wal_retry_backoff` 配置（WAL→内存投递失败重试）
- [ ] 测试：崩溃恢复正确性（模拟杀进程 → 重启 → WAL 重放）

### 8.2 PersistentBus
- [ ] 实现 `PersistentBus` 结构体（包装 InMemoryBus + WAL + RetryQueue + Overflow）
- [ ] 实现事件到达 → WAL 写入 → 确认 → 内存投递 完整流程
- [ ] 实现 WAL 确认后投递失败 → 入待重试队列

### 8.3 StateStore (`state_store.rs`)
- [ ] 定义 `StateStore` trait（get, put, put_cas, delete, scan, isolation_mode, write_consistency）
- [ ] 实现 `SledStore`（默认嵌入式实现）
- [ ] 实现 namespace 隔离模式（key 前缀 + scan 权限约束）
- [ ] 实现 physical 隔离模式（独立文件/表/桶）
- [ ] 实现 `cleanup_policy`（retain | delete_on_disable | delete_on_uninstall）
- [ ] 实现乐观锁（CAS：put_cas with expected_version）
- [ ] 实现悲观锁接口（lock → put → unlock）
- [ ] 实现写一致性（last_write_wins | optimistic_lock | pessimistic_lock）
- [ ] 实现读已提交（read_committed）
- [ ] 实现跨 Skill 共享（shared 声明 + 访问权限）

### 8.4 Dead Letter Queue (`dlq.rs`)
- [ ] 实现 `DeadLetterQueue` 结构体
- [ ] 实现 `enqueue`（事件入 DLQ + 记录 reason）
- [ ] 实现 `list`（支持 DlqFilter 筛选）
- [ ] 实现 `retry`（手动重试，重置 retry_count + 保留 original_retry_count 审计字段）
- [ ] 实现 `discard`（确认丢弃）
- [ ] 实现 `run_expiry`（TTL 到期处理：归档冷存储而非直接删除）
- [ ] 实现到期前分级告警（7d/3d/1d）
- [ ] 实现 `max_manual_retries: 5` 全局上限
- [ ] 实现手动 retry 操作历史（operator, timestamp, reason）

### 8.5 溢出管理 (`overflow.rs`)
- [ ] 实现 `OverflowDir` 管理
- [ ] 实现溢出写入（AT_LEAST_ONCE 事件 → 磁盘文件）
- [ ] 实现 `overflow_max_bytes` 硬上限
- [ ] 实现溢出目录使用率监控
- [ ] 实现重启恢复（扫描 overflow/ → 排序注入 → 去重）

### 8.6 验证
- [ ] 集成测试：PersistentBus 崩溃恢复（事件不丢）
- [ ] 集成测试：WAL 段轮转
- [ ] 集成测试：StateStore CAS 乐观锁竞争
- [ ] 集成测试：DLQ 生命周期（入队 → 到期 → 归档）
- [ ] 集成测试：溢出磁盘 → 重启恢复

---

## M9: 安全与配置

**目标**: 实现 Secret 管理、配置加载/校验、LLM 注入防护。

### 9.1 Secret 管理 (`crates/aman-secret/`)
- [ ] 实现 `SecretResolver` 结构体
- [ ] 实现 ${VARIABLE} 模式扫描（递归遍历配置 JSON）
- [ ] 实现多后端支持（Vault / AWS Secrets Manager / 1Password CLI / Env）
- [ ] 实现 `SecretBackend` trait（get, priority）
- [ ] 实现 `SecretCache` 内存加密缓存（AES-256-GCM）
- [ ] 实现 `EncryptedMemory<T>`（seal / open，使用后立即 drop）
- [ ] 实现 Secret 热更新（带宽限期 grace_period_sec: 60）
- [ ] 实现两步提交策略（高影响 Secret：预告 → 等待确认 → 切换）
- [ ] 实现连接池滚动更新（数据库连接串变更时避免风暴）
- [ ] 实现审计日志（affected_keys, old/new fingerprint_created timestamp, trigger_source）
- [ ] 实现 Secret Store 不可用时的重试（secret_retry_count + 退避 + 本地缓存降级）
- [ ] 实现 `secret_cache_fallback` 安全约束（AES-256-GCM 加密 + 600 权限 + TTL 300s）

### 9.2 配置系统 (`crates/aman-config/`)
- [ ] 实现 `AgentConfig` 完整配置结构体
- [ ] 实现 `ConfigLoader` 多层加载：
  - Layer 1: 框架默认值（硬编码）
  - Layer 2: 配置文件（aman.yaml）
  - Layer 3: 环境变量覆盖（AMAN_*）
  - Layer 4: 运行时 override（cron_override.yaml）
- [ ] 实现 `validate` 配置校验：
  - 总线模式绑定（in_memory 不允许 persistence.* 字段）
  - 超时合理性（drain_timeout < Tool timeout 检查）
  - Plugin 依赖环检测
  - Workflow initial_state 必须在 states 中
  - 状态名大小写不一致警告
  - 互斥字段检查（notify_on_complete vs watch_patterns）
- [ ] 实现配置热更新（ConfigChanged 事件）

### 9.3 LLM 注入防护
- [ ] 实现 `InputSanitizer` 结构体
- [ ] 实现 `TrustLevel` 分类（Trusted | Untrusted | Sandboxed）
- [ ] 实现已知注入模式匹配（Regex 规则集）
- [ ] 实现 System Prompt 加固接口
- [ ] 实现输出校验接口
- [ ] 实现敏感操作隔离（LLM 不直接执行，通过 Tool 沙箱）
- [ ] 实现注入检测审计日志

### 9.4 验证
- [ ] 单元测试：Secret 解析（${VAR} → 实际值）
- [ ] 单元测试：配置多层覆盖优先级
- [ ] 单元测试：配置校验拒绝非法配置
- [ ] 单元测试：输入消毒（已知注入模式检测）
- [ ] 集成测试：Secret 热更新 + 宽限期

---

## M10: 运行时生命周期 + HTTP API + CLI

**目标**: 实现启动/关闭编排、HTTP 控制接口、CLI 命令。

### 10.1 运行时编排 (`crates/aman-runtime/`)
- [ ] 实现 `AgentRuntimeBuilder`（构建器模式）
- [ ] 实现 `AgentRuntime` 结构体
- [ ] 实现 `with_soul` 加载 `SOUL.md`，并在运行时向 Skill/Pipeline/Workflow 上下注入 `Arc<Soul>`
- [ ] 实现 Phase 0→5 启动序列：
  - Phase 0: Event Bus 初始化 + 背压系统就绪
  - Phase 0.5: Secret 解析（重试 + 降级）
  - Phase 1: WAL 校验 → checkpoint 加载 → 待重试队列重建
  - Phase 2: 插件加载（拓扑序）→ Skill 注册 → Dispatcher 路由注入 + WAL 恢复事件注入
  - Phase 3: Workflow 实例恢复（超时 workflow_recovery_timeout: 120s）
  - Phase 4: Event Source 激活
  - Phase 5: 健康端点标记 ready
- [ ] 实现 Phase 5→0 优雅关闭序列：
  - Phase 5: 停止接收（health → 503）
  - Phase 4: Event Source 关闭 + Webhook 返回 503
  - Phase 4.5: 排水（等待 inflight Pipeline/Skill + 待重试队列停止重试模式）
  - Phase 3: Workflow 实例 checkpoint
  - Phase 2: 插件卸载（反向拓扑序）
  - Phase 1: WAL 最终 checkpoint + 待重试队列落盘
  - Phase 0: Event Bus 关闭
- [ ] 实现 `drain_timeout_sec: 30` 排水超时
- [ ] 实现排水超时与 Tool 超时交互（两者取其先 + 框架保证 Step 6 清理）
- [ ] 实现 shutdown 在启动中途到达的边界行为（§2.5.4）
- [ ] 实现半加载插件中断的资源回收（按隔离模式区分）
- [ ] 实现 `SoulChanged` 事件广播后的运行时引用刷新

### 10.2 健康检查 (`health.rs`)
- [ ] 实现 `GET /health/live`（进程存活，Phase 0+ 返回 200）
- [ ] 实现 `GET /health/ready`（就绪，Phase 5 返回 200，否则 503）
- [ ] 实现 `GET /health`（兼容端点 = ready）

### 10.3 HTTP API (axum)
- [ ] 实现 `POST /agent/start`（幂等：运行时返回 200；被 shutdown 中断返回 409）
- [ ] 实现 `POST /agent/shutdown`（同步阻塞到完成；幂等）
- [ ] 实现 `POST /event-source/{id}/pause`
- [ ] 实现 `POST /event-source/{id}/resume`
- [ ] 实现 `PUT /event-source/{id}/config`
- [ ] 实现兼容别名：`POST /source/{id}/pause` / `POST /source/{id}/resume` / `PUT /source/{id}/config`
- [ ] 实现 `POST /plugin/{name}/enable`
- [ ] 实现 `POST /plugin/{name}/disable`
- [ ] 实现 `POST /plugin/install`
- [ ] 实现 `POST /plugin/{name}/uninstall`
- [ ] 实现 `POST /cron/add`
- [ ] 实现 `POST /cron/{id}/update`
- [ ] 实现 `POST /cron/{id}/remove`
- [ ] 实现 `POST /inject-event`（生产环境默认禁用，需 force_enable_debug_endpoints）
- [ ] 实现 `GET /events/trace/{trace_id}`
- [ ] 实现 `GET /events/dump/{id}`
- [ ] 实现 `GET /dlq`（游标分页 + 过滤）
- [ ] 实现 `POST /dlq/{id}/retry`
- [ ] 实现 `POST /dlq/{id}/discard`
- [ ] 实现 `GET /metrics`（Prometheus exposition format）
- [ ] 实现 `GET /audit-log`（游标分页 + type/time/operator 过滤 + 审计员权限）

### 10.4 控制接口安全
- [ ] 实现默认绑定 localhost/Unix socket
- [ ] 实现 API Token 认证中间件
- [ ] 实现 mTLS 支持（可选）
- [ ] 实现敏感操作审计日志
- [ ] 实现二次确认机制（shutdown, disable plugin, dlq retry）

### 10.5 CLI (`crates/aman-cli/`)
- [ ] 实现 `aman run` 命令（--config, --soul, --daemon, --log-level）
- [ ] 实现 `aman skill` 子命令组（list, search, info, enable, disable, version, rollback）
- [ ] 实现 `aman plugin` 子命令组（list, enable, disable, install, uninstall）
- [ ] 实现 `aman event` 子命令组（inject, trace, dump）
- [ ] 实现 `aman workflow` 子命令组（list, show, retry, cancel）
- [ ] 实现 `aman config` 子命令组（show, validate, set）
- [ ] 实现 `aman dlq` 子命令组（list, retry, discard）
- [ ] 实现 `aman health ready`
- [ ] 实现信号处理（SIGTERM/SIGINT → 优雅关闭）

### 10.6 验证
- [ ] 集成测试：完整启动序列 Phase 0→5
- [ ] 集成测试：完整关闭序列 Phase 5→0
- [ ] 集成测试：shutdown 在启动中途到达的行为
- [ ] 集成测试：HTTP API 所有端点
- [ ] 集成测试：CLI 所有子命令
- [ ] 集成测试：/health/live vs /health/ready 分阶段差异

---

## M11: 可观测性

**目标**: 实现 Tracing、Metrics、审计日志。

### 11.1 OpenTelemetry Tracing
- [ ] 集成 `tracing` + `opentelemetry` crates
- [ ] 实现事件处理自动创建 span（event_processing → dispatcher_route → skill/pipeline/workflow_execute → tool_execute）
- [ ] 实现 TraceID 框架强制注入（所有事件自动携带）
- [ ] 实现 parent_event_id 链路追踪（事件链路树）
- [ ] 实现循环链路检测（parent_event_id 链重复 → 截断 + 标记 [cycle_detected]）
- [ ] 实现 Trace API（`GET /events/trace/{trace_id}` 返回完整 span 树）

### 11.2 Prometheus Metrics
- [ ] 集成 `prometheus` crate
- [ ] 实现 `MetricsEndpoint`（Prometheus exposition format）
- [ ] 暴露核心指标：
  - `aman_event_bus_queue_depth{priority="high|normal|low"}`
  - `aman_event_throughput_total`
  - `aman_backpressure_level`
  - `aman_events_discarded_total{reason="backpressure_l2"}`
  - `aman_retry_queue_depth`
  - `aman_inflight_pipelines`
  - `aman_inflight_skills`
  - `aman_plugin_health{plugin="...", status="ok|degraded|failed"}`
  - `aman_dlq_depth`
- [ ] 实现 `GET /metrics` 端点

### 11.3 审计日志
- [ ] 实现 `AuditLogger` 结构体
- [ ] 审计事件类型：
  - 配置变更（what, old, new, operator, timestamp）
  - Secret 轮换（affected_keys, fingerprint_created, trigger_source）
  - DLQ 操作（retry/discard, operator）
  - Cron 变更（add/update/remove, interval diff）
  - LLM 注入尝试
  - 插件操作（load/unload/enable/disable）
  - 事件丢弃（id, source, type, reason, timestamp）
- [ ] 实现 `GET /audit-log` 端点（游标分页 + 过滤 + 独立权限）
- [ ] Secret 指纹安全：日志中不暴露明文指纹哈希 → 改为 fingerprint_created 时间戳

### 11.4 验证
- [ ] 集成测试：TraceID 贯穿事件全生命周期
- [ ] 集成测试：Prometheus metrics 端点输出格式正确
- [ ] 集成测试：审计日志记录所有操作类型

---

## M12: Tauri v2 桌面应用

**目标**: 实现跨平台桌面应用，提供可视化 Dashboard、编辑器。

### 12.1 Tauri 项目初始化 (`crates/aman-tauri/`)
- [ ] 创建 Tauri v2 项目骨架
- [ ] 配置 `tauri.conf.json`
- [ ] 配置 `capabilities/` 权限
- [ ] 初始化前端项目（Svelte 5 + Vite）

### 12.2 Tauri State 管理 (`src-tauri/src/state.rs`)
- [ ] 实现 `AppState` 结构体（runtime, metrics_store, soul）
- [ ] 实现 Tauri 启动流程（构建 AgentRuntime → 注册 commands + state → 前端加载）

### 12.3 Tauri Commands (IPC 桥)
- [ ] 实现 `start_runtime` command（加载配置 + 构建 + 启动）
- [ ] 实现 `stop_runtime` command（优雅关闭）
- [ ] 实现 `get_metrics` command（实时指标快照）
- [ ] 实现 `search_skills` command（全文检索）
- [ ] 实现 `inject_event` command（调试事件注入）
- [ ] 实现 `get_event_trace` command（TraceID 链路追踪）
- [ ] 实现 `get_workflow_instances` command（实例列表）
- [ ] 实现 `retry_workflow` / `cancel_workflow` command
- [ ] 实现 `update_soul` command
- [ ] 实现 `preview_system_prompt` command

### 12.4 前端页面 (Svelte)
- [ ] **Dashboard 页面**：事件吞吐截图、队列深度仪表盘、背压实时状态、inflight Pipeline/Skill 数
- [ ] **Skill Editor 页面**：YAML 声明编辑、热加载实时预览、版本历史 Diff
- [ ] **Event Viewer 页面**：实时事件流、TraceID 链路追踪、事件详情展开
- [ ] **Workflow Board 页面**：状态机可视化（Mermaid/sm)，实例列表/操作（retry/cancel）
- [ ] **SOUL Editor 页面**：Markdown 编辑器、实时 SystemPrompt 预览
- [ ] **Plugin Manager 页面**：插件列表/状态、安装/卸载/启用/禁用
- [ ] **DLQ 页面**：死信事件列表、retry/discard 操作

### 12.5 实时事件流
- [ ] 实现 Tauri EventEmitter 推送
- [ ] 实现 `metrics:updated` 事件（Hook → Tauri emit → 前端更新）
- [ ] 实现 `event:processed` 事件流

### 12.6 验证
- [ ] 功能测试：Dashboard 实时刷新
- [ ] 功能测试：Skill Editor 热加载
- [ ] 功能测试：Workflow Board 状态机可视化
- [ ] 跨平台测试：macOS / Linux / Windows

---

## M13: 集成测试、文档与发布

**目标**: 端到端测试、性能基准、开发者文档、发布准备。

### 13.1 端到端集成测试
- [ ] 场景 1：文件变更 → Pipeline → 通知（FileWatch → OCR Pipeline → Slack）完整链路
- [ ] 场景 2：Pipeline 失败 + Saga 补偿 + DLQ
- [ ] 场景 3：Workflow 审批流（PENDING→REVIEWING→APPROVED）+ 超时自动拒绝
- [ ] 场景 4：Workflow ERROR → RETRY 恢复 → 成功
- [ ] 场景 5：事件风暴触发背压 Level 1→5 完整降级链
- [ ] 场景 6：崩溃恢复（杀进程 → 重启 → WAL 重放 → 事件不丢）
- [ ] 场景 7：插件热插拔（安装 → 启用 → Skill 执行 → 禁用 → 卸载）
- [ ] 场景 8：Secret 热更新（API Key 轮换 → 宽限期 → 新 Key 生效）

### 13.2 性能基准
- [ ] 基准：Event Bus 吞吐（目标 > 50K events/s 内存模式）
- [ ] 基准：Pipeline 端到端延迟（P50 < 10ms, P99 < 100ms）
- [ ] 基准：WAL 写入吞吐（目标 > 10K events/s fsync 模式）
- [ ] 基准：背压溢出磁盘（100K events → 溢出 → 恢复 全链路）
- [ ] 基准：启动时间（Phase 0→5，目标 < 5s 空配置）
- [ ] 基准：Workflow 实例恢复（10K 实例，目标 < 120s）

### 13.3 开发者文档
- [ ] README.md（项目简介、快速开始）
- [ ] CONFIG.md（完整配置参考）
- [ ] SKILL.md（Skill 开发指南 + 示例）
- [ ] PLUGIN.md（Plugin 开发指南）
- [ ] WORKFLOW.md（Workflow 定义指南 + 状态图）
- [ ] API.md（HTTP API 参考）
- [ ] CLI.md（CLI 命令参考）
- [ ] ARCHITECTURE.md（架构概述，链接到 agent-design.md 和 architect-design.md）
- [ ] 代码内文档（所有 pub API 的 rustdoc）

### 13.4 aman-sdk
- [ ] 实现 `aman-sdk` crate（prelude 重新导出核心类型）
- [ ] 提供外部 Skill/Plugin 开发者的最小依赖
- [ ] 提供示例 Skill 项目模板

### 13.5 发布准备
- [ ] `cargo test --workspace` 全部通过
- [ ] `cargo clippy --workspace -- -D warnings` 零警告
- [ ] `cargo doc --workspace --no-deps` 生成文档
- [ ] 配置 CI/CD（GitHub Actions：test + clippy + build）
- [ ] 版本号策略（SemVer）
- [ ] CHANGELOG.md

---

## 附录 A: Crate 依赖关系（开发顺序约束）

```
阶段 1 (M1)：       aman-core, aman-macros
阶段 2 (M2-M3)：    aman-event-bus, aman-persistence, aman-source
阶段 3 (M4-M5)：    aman-dispatcher, aman-pipeline, aman-skill, aman-tool, aman-hook
阶段 4 (M6)：       aman-workflow
阶段 5 (M7)：       aman-plugin, aman-soul
阶段 6 (M8)：       aman-persistence (完整)
阶段 7 (M9)：       aman-secret, aman-config
阶段 8 (M10)：      aman-runtime, aman-cli
阶段 9 (M12)：      aman-tauri
```

## 附录 B: 关键配置参数速查

| 参数 | 默认值 | 位置 |
|------|--------|------|
| `event_bus.max_queue_size` | 10000 | §3.3 |
| `event_bus.backpressure.*.threshold` | 0.80/0.90/0.95/0.98 | §3.3 |
| `event_bus.dedup.window_ms` | 30000 | §3.3 |
| `persistence.wal_sync` | fsync | §3.3 |
| `persistence.checkpoint_interval` | 500 | §3.3 |
| `persistence.wal_rotate_bytes` | 1GB | §3.3 |
| `overflow_max_bytes` | 1GB | §3.3 |
| `retry_queue_max` | 1000 | §3.3 |
| `wal_replay_buffer_max` | 5000 | §2.5.1 |
| `plugin_load_timeout` | 30s | §2.5.1 |
| `workflow_recovery_timeout` | 120s | §2.5.1 |
| `drain_timeout_sec` | 30 | §2.5.3 |
| `secret_retry_count` | 3 | §2.5.1 |
| `secret_cache_ttl_sec` | 300 | §2.5.1 |
| `dlq_ttl_days` | 30 | §3.5 |
| `compensation_contract.timeout_sec` | 30 | §3.5 |
| `compensation_contract.retry_count` | 3 | §3.5 |
| `max_manual_retries` (DLQ) | 5 | §9.3 |
| `cron min_interval` | 1s | §6.4 |
| `cron rate_limit` | 100/s | §6.4 |
| `debounce_ms` (file_watch) | 500 | §3.2 |
| `max_stable_wait_ms` (file_watch) | 30000 | §3.2 |
| `grace_period_sec` (secret rotation) | 60 | §9.2 |
| `timeout_defer_ms` (workflow) | 5000 | §3.7 |
| `retry_cancel_conflict_defer_ms` | 5000 | §3.7 |
