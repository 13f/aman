# Agent Design Review (R3) — 第三次审计报告

> 审计人：Business Logic Auditor
> 审计对象：`agent-design.md` — 事件响应式 Agent 框架设计（最新版）
> 审计日期：2026-05-05
> 前置审计：`agent-design-r1.md`（10 项关注点）、`agent-design-r2.md`（8 项关注点）

---

## 第一部分：R2 问题修复状态

逐一核对了 R2 的 8 项关注点在当前 `agent-design.md` 中的修复情况。

| # | R2 关注点 | 等级 | 修复状态 | 证据位置 |
|---|----------|------|---------|---------|
| 1 | 补偿操作自身的幂等性与失败路径 | 🔴 高 | **已修复 ✅** | §3.5 补偿契约(idempotent+timeout_sec+COMPENSATION_FAILED)；§9.1 配置示例；§10 决策8；风险 #14 |
| 2 | DLQ 到期事件静默消失 | 🟡 中 | **已修复 ✅** | §3.5 dlq_ttl_config(pre_expiry_alert_days, archive_on_expiry)；§9.1 配置示例；风险 #15 |
| 3 | ERROR 状态的静默终结 | 🟡 中 | **已修复 ✅** | §3.7 ERROR on_enter 默认触发 alert+log（强制行为）；on_timeout 前 1d/6h/1h 告警；风险 #16 |
| 4 | parent_event_id 循环链路 | 🟡 中 | **已修复 ✅** | §3.1 parent_event_id 循环检测；Trace API 截断；`[cycle_detected]` 标记；风险 #17 |
| 5 | 内存 Event Bus + 持久化配置的幽灵保证 | 🟡 中 | **已修复 ✅** | §3.3 配置校验(总线模式绑定)：in_memory 下拒绝 persistence 字段；EXACTLY_ONCE 退化为 AT_LEAST_ONCE |
| 6 | 控制接口的认证盲区 | 🟡 中 | **已修复 ✅** | §9.3 控制接口安全守卫：localhost 默认绑定 + API Token/mTLS 认证 + 敏感操作二次确认 + /inject-event 生产环境禁用；风险 #18 |
| 7 | Skill trigger 缺少 payload 级匹配 | 🟢 低 | **已修复 ✅** | §3.6 match: { payload.task: "report" } 语法；§5.1 声明式 payload 匹配示例 |
| 8 | FileWatchSource 强制发布无标记 | 🟢 低 | **已修复 ✅** | §3.2 force_publish_on_timeout: mark_incomplete + incomplete: true 标志；配置参数化 |

**结论：R2 提出的 8 项问题全部被认真处理。** 最让我关注的是：
- 补偿操作不再"自欺欺人"——`COMPENSATION_FAILED` 中间态说明设计者承认了补偿也可能失败
- ERROR 状态的 `on_enter: alert` 被标记为"框架强制"（不是可选项），这是正确的态度
- 内存模式拒绝 persistence 配置的校验策略，防止了配置误导

---

## 第二部分：第三次评审 R3 新发现的关注点

第三遍阅读——前两轮解决了基础防御和"防御的防御"，这一轮聚焦在"恢复的恢复"和"边界条件的边界"。

---

### 🎯 新关注点 1：Workflow ERROR 状态无恢复路径（🔴 高）

**场景**：Workflow 实例因为一次临时故障（网络抖动、DB 短暂不可用、第三方 API 超时后自愈）进入 ERROR 状态。Operator 收到告警，排查后发现是临时故障，想恢复工作流。

**💥 可能后果**：
- 检查当前 Workflow 的转移表：ERROR 只有一条出边——`{ from: ANY, event: ERROR, to: ERROR }` 负责进入，但没有任何从 ERROR 回到活跃状态的转移
- `state_timeouts` 定义了 `ERROR → (7天后) → ARCHIVED`，这是唯一的退出路径
- 即使 `CANCEL` 事件有 `{ from: ANY, event: CANCEL, to: CANCELLED }`，但 CANCELLED 是终态，不能恢复执行
- **Operator 唯一的选择：** 让工作流归档，然后手动另起一个实例，重新走一遍所有步骤
- 这等于说 ERROR 状态只有"死"或"等死"两条路，没有"复活"的路径

**🛠 建议**：
- 新增 `RETRY` 事件，允许从 ERROR 状态恢复到上一次活跃状态（或指定的恢复状态）：
  ```yaml
  { from: ERROR, event: RETRY, to: (last_active_state) }
  ```
- 或定义明确的恢复转移，如 `ERROR → PENDING` 恢复初始状态重新开始
- ERROR 状态的 `on_enter` 应保存 `last_active_state`（进入 ERROR 前所在的状态），以便 RETRY 时回到正确的位置
- 恢复应有次数限制—例如 max_retry_count: 3，超过后只能进入 ARCHIVED

---

### 🎯 新关注点 2：Cron/Timer 重启后 catch-up 策略未定义（🔴 高）

**场景**：Agent 因维护关闭 5 分钟，期间 Cron "daily-report" 原定在 09:00 触发。09:05 Agent 重启完成，CronSource 重新开始计时。

**💥 可能后果**：
- 文档没有定义错过定时触发后的行为——是静默跳过？是立即补跑所有错过的触发？还是只跑最近一次？
- 如果 Agent 配置了主备切换，两个实例可能各产生一次 CRON_TICK，导致重复执行
- 如果 Agent 关闭了 2 小时而 cron 本身是每 5 分钟一次——重启后若全部补跑，瞬间涌入 24 个 CRON_TICK 事件，造成微风暴
- TimerSource（固定间隔）和 CronSource（cron 表达式）的 catch-up 语义不同——固定间隔倾向于"跳过错过的"，cron 倾向于"如果很重要则补跑"

**🛠 建议**：
- CronSource/TimerSource 增加 catch-up 策略配置：
  ```
  catch_up: skip | latest | all
    - skip: 错过的不补（默认，适合高频心跳类）
    - latest: 只执行错过的最近一次触发（适合日报这类"只需最新结果"的任务）
    - all: 全部补跑（适合对时间敏感的数据采集——但要加上 rate limit）
  ```
- 主备模式下，cron 事件应通过分布式锁或 leader 选举避免重复触发
- 跨实例的场景，建议在 §11 风险表新增："Cron 重启风暴"风险

---

### 🎯 新关注点 3：Event Bus 背压 Level 4 溢出磁盘满——溢出失败无兜底（🟡 中）

**场景**：事件风暴触发背压 Level 4（98% 满），AT_LEAST_ONCE 事件开始溢出到 `overflow_disk_path: /var/lib/agent/overflow/`。但该磁盘分区的可用空间也是有限的。

**💥 可能后果**：
- 磁盘满了 → 溢出写入失败 → 该事件无处可去 → 文档未定义此情况下的兜底
- 如果溢出目录所在磁盘同时是 WAL 文件所在磁盘，磁盘满会同时影响正常 WAL 写入，双重打击
- 系统重启后，溢出目录中的事件如何重放？文档没有 checkpoint 机制适用于溢出磁盘的事件
- 溢出目录旧事件长期堆积未清理——积压到磁盘满才被发现

**🛠 建议**：
- 定义溢出失败时的升级路径：
  - Level 4A：溢出到磁盘（正常）
  - Level 4B：磁盘空间不足时 → 触发紧急告警 → 阻塞或回退到 Level 3
- 给溢出目录设定上限（`overflow_max_bytes`），达到 80% 时提前告警
- 重启时扫描溢出目录，重放溢出事件到 Event Bus（或将其作为恢复检查清单的一项）
- 考虑溢出目录和 WAL 目录应使用不同的磁盘分区（配置层面建议）

---

### 🎯 新关注点 4：插件卸载依赖方等待确认无超时（🟡 中）

**场景**：插件 B 卸载。系统通知依赖方插件 A 的 `on_dependency_unloading` 钩子。插件 A 的钩子实现有问题——在等待一个永远不会返回的外部资源。

**💥 可能后果**：
- 文档说"等待确认"——但没有超时保护
- 插件 A 的 `on_dependency_unloading` 导致卸载线程卡死
- 插件 B 永远无法完成卸载
- 如果插件 B 的卸载在主关闭链条中（`agent.shutdown` → 卸载所有插件），则整个 Agent 无法优雅关闭

**🛠 建议**：
- `on_dependency_unloading` 的确认等待必须有硬超时（建议 30s，可配置）
- 超时后强制卸载依赖方，同时写入告警日志："插件 X 的 on_dependency_unloading 超时，已强制继续卸载"
- 在插件健康检查中加入 on_dependency_unloading 钩子的超时监控
- 主关闭时，超时不应阻止 Agent 退出（但应记录异常供事后排查）

---

### 🎯 新关注点 5：Secret 热更新与活跃 Tool 的竞态条件（🟡 中）

**场景**：Agent 运行中，Secret Store 中的 API Key 被主动轮换。Agent 触发热更新（重新解析 `${VARIABLE}`）。此时有一个正在执行的长 Tool（如 OCR 容器执行 60s）正在使用旧 Key。

**💥 可能后果**：
- 正在使用旧 Key 的长 Tool 可能在执行过程中因密钥变更而失败（如果它有多个 API 调用步骤）
- Secret 热更新不留宽限期，**旧 Key 和新 Key 同时生效的窗口期未定义**
- 审计日志中可能出现新旧密钥混乱，排查时难以判断逐笔调用的真实凭证
- 如果 Secret 是数据库连接串，热更新可能导致连接池风暴（所有连接同时断开并重连）

**🛠 建议**：
- Secret 热更新应支持定义宽限期（`grace_period_sec`），在这期间旧密钥仍可用于活跃连接
- 可采用连接级别的滚动更新逻辑：已有连接使用旧密钥直到自然释放，新连接使用新密钥
- 审计日志应记录：affected_keys、old_key_fingerprint（哈希，非明文）、new_key_fingerprint、timestamp
- 数据库连接串这类高影响 Secret 的热更新建议使用两步提交：先发布"即将变更"事件，等待所有活跃 Tool 完成，再实际切换

---

### 🎯 新关注点 6：State Store 命名空间隔离的安全声明与实际能力矛盾（🟡 中）

**场景**：State Store 使用 `namespace` 隔离模式（共享存储，key 前缀区分），命名空间规则为 `skill:{skill_name}`。文档声称"Skill A 无法枚举 Skill B 的 key"。

**💥 可能后果**：
- 如果隔离仅靠 key 前缀 `技能：A：` vs `技能：B：` 来实现，那么支持 `scan("*")` 或 `list_keys("*")` 操作的 State Store 实现（Redis、etcd、文件系统目录）可以轻易遍历全局 key
- Skill A 只要能调用 `state_store.scan("skill:*")` 就能枚举所有 Skill 的 key
- 文档的安全声明("无法枚举")与实现机制("前缀区分")之间存在矛盾
- **这要么是一个虚假的安全承诺**（若 StateStore 没有权限过滤层），**要么缺少安全过滤层的设计描述**

**🛠 建议**：
- 澄清安全边界：如果确实需要"无法枚举"的隔离，必须增加层级的权限校验——StateStore 需要知道当前调用者是谁，并拒绝扫描非自己命名空间的 key
- 或者诚实修正声明：namespace 隔离提供的是**命名空间碰撞保护**（防止 key 冲突）而非**安全隔离**
- 在 `§5.2 StateStore` 中明确区分这两个概念：
  - `namespace：` → 仅防止 key 冲突（推荐）
  - `physical：` → 真正的安全隔离
- 或者增加 `permissions` 层：`{ scan: ["skill:*"] }` 由框架自动限制到当前 Skill 命名空间

---

### 🎯 新关注点 7：Pipeline/Skill 并发执行模型未定义（🟡 中）

**场景**：Event Bus 同时收到两个 FILE_CREATED 事件，都路由到同一个 Pipeline "invoice-processor"。一个处理耗时 30s（OCR + DB 写入），另一个紧接着到达。

**💥 可能后果**：
- 文档没有定义：同一 Pipeline/Skill 实例是串行处理（一个完成再处理下一个）还是并行处理（每个事件开启独立实例）
- **如果串行**：慢 Pipeline 会阻塞后续同 Pipeline 事件——发票 B 必须等发票 A 处理完才能开始，即使它们完全独立
- **如果并行**：State Store 的 `last_write_wins` 可能导致两个实例写入同一个 key 互相覆盖——`last_write_wins` 在 §5.2 有定义，但文档没有说 Pipeline 默认用哪种并发策略
- 文件系统层面的并发操作：两个 Pipeline 实例同时读取/写入同一个临时文件 → 数据损坏

**🛠 建议**：
- 明确定义 Pipeline/Skill 的默认并发模型：`concurrency: serial | parallel | limited(n)`
  - `serial`：一次只处理一个事件（默认，最安全）
  - `parallel`：每个事件创建独立处理实例（需确保资源隔离）
  - `limited(3)`：最多 3 个并发实例
- 默认应建议使用 `serial`，但告知开发者在什么条件下可以安全地开启 `parallel`
- 开启 `parallel` 时应强制要求 State Store 使用乐观锁而非 `last_write_wins`
- 如果使用文件系统资源，应通过临时目录隔离（每个实例分配独立工作目录）

---

### 🎯 新关注点 8：WAL → 内存队列投递失败时的数据缺口（🟢 低）

**场景**：持久化模式下，事件到达 → 写入 WAL（fsync 确认）→ 投递到内存队列。在"写入 WAL 成功"和"投递到内存队列"之间，内存队列因背压 Level 2+ 拒绝新事件。

**💥 可能后果**：
- WAL 已确认事件已持久化，但内存队列拒绝接收
- 事件"卡"在 WAL 和内存队列之间——WAL 以为事件已提交，但事件实际上从未被处理
- 重启后 WAL 重放该事件 → 去重窗口为空（内存去重表在重启时重置）→ 事件被重新投递
- **导致后果**：事件最终会被处理（重启后重放），但**处理延迟被拉长到下一次重启**——对实时性有要求的场景（定时任务、告警）可能错过时间窗口
- 更坏的情况：如果 Agent 长期不重启，这个事件永远停留在 WAL 中不被处理

**🛠 建议**：
- 明确定义：WAL 提交成功但内存队列投递失败 → 事件进入"待重试队列"而非静默丢弃
- 待重试队列有独立的重试间隔（建议指数退避，100ms → 500ms → 2s）
- 如果持续重试失败（如队列长期满），发出"事件积压"告警
- 恢复后检查清单应增加一项："检查是否有 WAL 中确认但未投递的事件"

---

## 修复状态总结

```
R1 问题：10/10 已修复 ✅
R2 问题：8/8 已修复 ✅
R3 新发现问题：8 项
  - Workflow ERROR 无恢复路径 (🔴 高)        → 新增
  - Cron/Timer 重启 catch-up (🔴 高)         → 新增
  - 溢出磁盘满无兜底 (🟡 中)                   → 新增
  - 插件卸载 ack 无超时 (🟡 中)                → 新增
  - Secret 热更新竞态 (🟡 中)                 → 新增
  - State Store 命名空间安全矛盾 (🟡 中)       → 新增
  - Pipeline/Skill 并发模型未定义 (🟡 中)      → 新增
  - WAL→内存队列投递缺口 (🟢 低)              → 新增
```

---

## 实施建议优先级

```
P0（阻止上线）
  └── Workflow ERROR 恢复路径 + Cron/Timer catch-up 策略（新 #1, #2）

P1（上线前必须解决）
  ├── 溢出磁盘满的升级路径（新 #3）
  ├── 插件卸载 ack 超时保护（新 #4）
  └── Secret 热更新宽限期（新 #5）

P2（beta 前建议解决）
  ├── State Store 安全声明的诚实性（新 #6）
  └── Pipeline/Skill 并发模型定义（新 #7）

P3（持续改进）
  └── WAL→内存队列投递失败重试（新 #8）
```

---

## 最终评价

从 R1 到 R2 到 R3，这个设计文档正在做一个经典的纵深防御演进：

- **R1 层**：基础功能完整性（幂等性、补偿、背压、状态机完备）
- **R2 层**：防御的防御（补偿的补偿、告警的告警、去重的去重）
- **R3 层**：恢复的恢复、边界的边界、声明的诚实性

新发现的 8 个问题不再是"你忘了什么"的级别——它们已经是"你想到了 A，但 A 没有完整的恢复路径"或"你声称了安全属性但没有配套的实现机制"的深度。

有三个趋势值得注意：

1. **从"有状态"到"状态恢复"的缺口**：ERROR 没有恢复路径、Cron 重启没有 catch-up——这是工作流隔离中最困难的 part，也是实际投产中最常出问题的区域。

2. **从"声明"到"实现"的不匹配**：State Store 的"无法枚举"声明是目前文档中最明显的安全承诺 vs 实现机制分离。如果框架按这个设计实现，开发者会寄望于一个不存在的安全边界。

3. **从"单机"到"分布式"的平滑性**：溢出磁盘、Secret 热更新、主备 cron——这些问题的共同特征是：它们只在分布式/长期运行场景下出现，但文档目前主要面向单机模型假设。

这是好事——文档的深度和诚实度在逐轮提升。这个设计在"防御纵深"上已经开始超越我看到的大多数 Agent 框架设计（包括 Hermes、CrewAI 和 LangChain 的公开设计文档）。
