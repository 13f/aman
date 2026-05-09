# Agent Design Review (R6) — 第六次审计报告

> 审计人：Business Logic Auditor
> 审计对象：`agent-design.md` — 事件响应式 Agent 框架设计（当前最新版）
> 审计日期：2026-05-05
> 前置审计：`agent-design-r1.md` ~ `agent-design-r5.md`

---

## 第一部分：R5 问题修复状态

逐一核对了 R5 的 5 项关注点在当前 `agent-design.md` 中的修复情况。

| # | R5 关注点 | 等级 | 修复状态 | 证据位置 |
|---|----------|------|---------|---------|
| 1 | ERROR 恢复 retry_count 重置致无限重试循环 | 🔴 高 | **已修复 ✅** | §3.7 拆分 session_retry_count（重置）+ total_retry_count（不重置）；guard 检查 total_retry_count；状态图同步更新为 `total_retry_count < 3`（lines 638-666）；Risk #32 |
| 2 | State Timeout 时钟跨 ERROR 语义未定义 | 🟡 中 | **已修复 ✅** | §3.7 新增"超时时钟跨状态退出语义"整块（lines 616-636），定义 pause/reset/continue 三种语义+安全约束；可配置 `state_timeout_behavior_on_exit`；Risk #33 |
| 3 | 状态图终态归档路径与配置不一致 | 🟡 中 | **已修复 ✅** | state_timeouts 新增 APPROVED/REJECTED/CANCELLED 各 + timeout:30d→ARCHIVED（lines 600-603）；YAML 配置同步更新（lines 1433-1435）；注释声明与状态图一致；Risk #34 |
| 4 | FileWatchSource 非本地 FS 锁检测失效 | 🟢 低 | **已修复 ✅** | §3.2 新增"文件锁检测的局限性"节（lines 205-213）；check_open_files 支持 auto|true|false（默认 auto，自动检测 FS 类型）；远程 FS 建议增大参数；Risk #35 |
| 5 | Cron rate_limit 超额语义未定义 | 🟢 低 | **已修复 ✅** | §6.4 新增 `rate_limit_overflow: "delay"` 及三种行为说明（lines 1091-1095）；§6.5 补充 catch_up+rate_limit 交互说明（lines 1118-1120）；Risk #36 |

**结论：R5 提出的 5 项问题全部被认真修复。** 前五轮一共 36 项问题全部关闭。

---

## 第二部分：第六次评审 R6 新发现的关注点

文档经过五轮迭代后已经非常成熟——防御机制齐全、边界覆盖到位、文档之间一致。R6 发现的问题集中在**机制之间的交互盲区**和**文档内部仍存的一处残留不一致**。

---

### 🎯 R6 关注点 1：待重试队列满后的溢出行为未定义（🟡 中）

**场景**：
§3.3 WAL 恢复机制中，待重试队列的长度限制为 `retry_queue_max: 1000`（line 348）。但文档只定义了"队列内的事件在空间释放后自动恢复重试"，未定义**新到达的事件在队列已满时的行为**。

具体流程：
```
事件到达 → WAL 写入成功 → 尝试入待重试队列
                                     ↓
                              队列已满（1000 事件）
                                     ↓
                              行为未定义 ← ⚠
```

**💥 可能后果**：
- 新事件无处可去——不能入队列（满），不能丢弃（AT_LEAST_ONCE 承诺不可丢）
- WAL checkpoint 无法前进（事件尚未被确认处理）
- **WAL 无限增长**——如果事件风暴持续、主队列和重试队列双双阻塞，WAL 段文件持续累积 1GB 的 rotate 也无法释放，因为未确认事件在 WAL 段中
- 重启时：WAL 从头开始重放 → 前 1000 个事件在重试队列中 + 后续所有积压事件一起涌入 → **二次风暴**
- 最坏情况：如果磁盘上的 WAL 段占满磁盘 → 系统崩溃或拒绝写入新的 WAL 事件

**对比已处理的类似场景**：
- 背压 Level 4A 溢出到磁盘有完整定义（包括 1GB 硬上限、80% 告警、50% 滞回离开）
- 背压 Level 4B 阻塞有完整定义（回退到 Level 3 + 紧急告警）
- **但 WAL→待重试队列路径上，队列满的溢出行为完全没有定义**

**🛠 建议**：
- 在 §3.3 的待重试队列约束段中补充定义队列满的行为，例如：
  - **方案 A（推荐）**：队列满时阻塞 WAL 确认（待重试队列也有背压信号），这符合 AT_LEAST_ONCE 的"绝不静默丢弃"原则
  - **方案 B**：队列满时将新事件溢出到与 overflow/ 同目录的次级缓存（`retry_overflow_path`），类似背压 Level 4A 的溢出机制
  - **方案 C**：队列满时触发 HIGH 级告警 + 阻塞 WAL 写入，直到队列释放空间（需要明确阻塞是否会导致 WAL 所在磁盘 I/O 停滞）
- 无论哪种方案，必须确保 checkpoint 逻辑与队列状态同步——待重试队列清空前不应推进 checkpoint
- 风险清单新增 #37 项

---

### 🎯 R6 关注点 2：Pipeline parallel 模式下补偿操作的并发隔离未定义（🟡 中）

**场景**：
§3.5 Pipeline 定义了 parallel 模式的三个安全前置条件（lines 504-507）：

```
a) State Store 使用 optimistic_lock（而非 last_write_wins）
b) 每个实例使用独立临时目录隔离文件系统资源
c) Pipeline 步骤不依赖外部排序
```

但这些条件**只覆盖了正常运行路径**，未覆盖补偿路径。当多个实例在 parallel 模式下同时触发补偿时：

```
并发执行路径：
  Instance A: 步骤 3 失败 → 触发补偿 C3 → C2 → C1
  Instance B: 步骤 3 失败 → 触发补偿 C3 → C2 → C1
  时间上可能有重叠

补偿操作可能访问的共享资源：
  - 数据库 DELETE（两个实例删除不同 ID 的数据 → 可能没问题）
  - 计数器 DECR（共享 counter，乐观锁冲突 → 补偿本身失败）
  - API 回滚（两个实例发 DELETE /orders/{id} → 不同 ID，但共享 API rate limit）
  - 无独立临时目录概念的资源（外部系统、共享的 State Store key）
```

**💥 可能后果**：
- 补偿操作**不受 parallel 模式前置条件保护**——补偿的 Tool Runner 没有"独立临时目录"隔离
- 如果两个补偿同时写同一个共享 State Store key（如全局 counter），即使使用 optimistic_lock，**补偿的乐观锁冲突可能导致补偿本身失败**
- 补偿失败 → Pipeline 进入 `COMPENSATION_FAILED` → 需要人工接管
- 在高吞吐 parallel Pipeline 中，如果多个文件因为同一个原因失败（如 OCR 引擎故障），多个补偿同时失败的概率很高

**🛠 建议**：
- 在 §3.5 Pipeline 并发模型的 parallel 模式要求中增加第 (d) 条：
  - **补偿操作必须实例隔离**：补偿使用的 Tool 必须按实例数据 scope（如 invoice_id、order_id）操作，不得使用无 scope 的全局操作
  - 或框架级保证：parallel 模式下触发补偿时，框架自动为补偿工具注入实例隔离上下文（隔离 key 前缀、独立 API 客户端）
- 在 compensation_contract 中明确：补偿操作运行时的隔离语义与正常操作一致（optimistic_lock 要求同样适用于补偿写入）
- 强烈建议：parallel 模式的 Pipeline 的补偿操作优先使用幂等 + 按 ID 回滚（而非计数器回滚），避免乐观锁冲突

---

### 🎯 R6 关注点 3：YAML 配置示例的 RETRY guard 仍使用旧变量名 `retryCount`（🟢 低）

**场景**：
§9.1 完整 YAML 配置示例中，Workflow 的 error→RETRY 转移守卫（line 1442）：

```yaml
- { from: error, event: RETRY, to: :last_active_state, guard: retryCount < 3, on_fail: archived }
```

但 §3.7 代码块中（line 665）使用的名称是：

```
{ from: ERROR, event: RETRY, to: :last_active_state, guard: total_retry_count < max_retry_count, on_fail: ARCHIVED }
```

更关键的是，§3.7（lines 640-644）专门用 5 行注释强调：
- `total_retry_count` 永不被重置 ✔
- guard 必须检查 `total_retry_count` ✔
- 否则 `max_retry_count` guard 永远无法触发 → 无限重试循环

**💥 可能后果**：
- 如果 YAML DSL 中的 `retryCount` 关键字映射到 `session_retry_count`（每次进入 ERROR 重置为 0），则 YAML 示例中的 guard 仍然是**不正确的**——R5 #1 修复的同一个 bug 以不同形式残留
- 如果 `retryCount` 映射到 `total_retry_count`，则只是命名不一致，但文档未声明此映射关系
- 拷贝 YAML 配置的开发者可能误以为 `retryCount` = 旧版本的单一计数器，从而错误地认为 guard 检查的是被重置的计数器

**🛠 建议**：
- 方案 A：将 YAML 配置示例中的 `retryCount` 改为 `total_retry_count`，与代码块一致
- 方案 B：如果 DSL 关键字确实是 `retryCount`，在 §3.7 或 §9.1 中增加一行映射说明：`# YAML DSL 关键字 retryCount 对应框架的 total_retry_count（非 session_retry_count）`
- 推荐方案 A——同名同义最清晰，减少阅读者的认知负担

---

### 🎯 R6 关注点 4：ERROR 恢复的 `retry_backoff` 语义未定义——自动 vs 手动重试不明确（🟢 低）

**场景**：
§3.7 error_recovery 配置（line 652）：

```
retry_backoff: "immediate"            // 立即重试 | 可配置延时策略
```

文档未定义：
- RETRY 事件由谁发送？框架自动调度，还是操作员手动发送？
- 如果自动调度：第一次 RETRY 失败（工作流重新进入 ERROR），框架是否自动安排第二次 RETRY？间隔是多少？
- 如果手动：`retry_backoff` 参数的实际作用是什么？

**💥 可能后果**：
- **自动重试 + "immediate"** 解读：进入 ERROR 立即发送 RETRY → 失败 → 立即再发送 → ... → 在几秒内烧光 max_retry_count，然后归档。如果根因是瞬时抖动（2秒网络恢复），"immediate" 可能在抖动恢复前就用完了所有重试机会
- **手动重试** 解读：操作员看到告警，手动发送 RETRY。"immediate" 意味着"操作员发送 RETRY 后立即尝试恢复，不额外等待"——这有意义，但 "retry_backoff" 命名暗示的是重试间隔，而非恢复延迟
- 两种解读在 "immediate" 值下行为一致（立即执行），但在非 "immediate" 值（如 "5s", "exponential"）下行为完全不同

**🛠 建议**：
- 在 error_recovery 配置段增加说明：**RETRY 事件的触发来源**：
  - 框架自动调度？（需要定义首次触发时机 + 后续重试间隔策略）
  - 操作员手动发送？（retry_backoff 应重命名为 retry_delay 或 recovery_delay，表示 RETRY 到达后执行恢复前是否额外等待）
- 或者更明确：将 `retry_backoff` 改为 `auto_retry_count: 0`（默认 0 = 手动，>0 = 自动重试 N 次）+ `auto_retry_interval: "5s"`（自动重试间隔）

---

## 修复状态总结

```
R1 问题：10/10 已修复 ✅
R2 问题：8/8  已修复 ✅
R3 问题：8/8  已修复 ✅
R4 问题：5/5  已修复 ✅
R5 问题：5/5  已修复 ✅
R6 新发现问题：4 项
  - 待重试队列满后溢出行为未定义 (🟡 中)      → 新 #1
  - Pipeline parallel 模式补偿无并发隔离 (🟡 中) → 新 #2
  - YAML 配置 retryCount 未同步更新 (🟢 低)   → 新 #3
  - retry_backoff 语义未定义 (🟢 低)           → 新 #4
```

---

## 实施建议优先级

```
P1（上线前必须解决）
  ├── 待重试队列满后的溢出行为（新 #1）
  │    └── WAL checkpoint 卡死 + WAL 无限增长 + 重启二次风暴
  │        背压体系已有 5 级完整定义，但 WAL→待重试队列路径是缺口
  │
  └── Pipeline parallel 补偿无并发隔离（新 #2）
       └── parallel 模式的前置条件只覆盖正常路径，补偿操作不受保护
           多个补偿同时失败 → COMPENSATION_FAILED + 人工接管风暴

P2（beta 前建议解决）
  ├── YAML 配置 retryCount 未同步（新 #3）
  │    └── 与代码块不一致，可能让开发者重新踩 R5 #1 的坑
  └── retry_backoff 语义未定义（新 #4）
       └── 自动/手动不明确，影响实现者理解 ERROR 恢复的生命周期
```

---

## 最终评价

**R5 的全部 5 个问题已被认真修复**。计数器拆分、超时时钟语义、终态归档、文件锁检测、cron 超额——每一个都在文档正文章节（而非仅风险清单）中有结构化的定义。这是五轮修复中执行最彻底的一轮。

R6 发现的 4 个问题标志着文档进入了一个新阶段：**机制间交互盲区**。

- **#1 号（待重试队列满）**是背压体系中最后一个未定义边界——背压 5 级定义了 Event Bus 满的所有行为，背压 Level 4A→4B 定义了溢出磁盘满的行为，但 WAL→待重试队列的溢出行为却是空白。如果背压体系是一个堡垒墙，这个缺口的位置在"内城门"上——队列满了，城外的 WAL 还在源源不断地送入。
- **#2 号（Pipeline parallel 补偿隔离）**是 parallel 模式条件的"另一半"——正常执行有条件保护（optimistic_lock + 独立目录），但补偿执行不受这些条件约束。在并发模式下，补偿不是稀有事件——如果一个 bug 让 10 个 parallel 实例同时失败，10 个补偿同时运行，compensation_contract 只保证了幂等和超时，但没保证彼此不冲突。
- **#3 号和 #4 号**是文档成熟度的"残留毛刺"——#3 是 YAML 配置示例没有随代码块一起更新的疏忽，恰好是 R5 修过的 bug 的同源残留；#4 是一个定义模糊的参数，在"immediate"时没有歧义，但一旦有人配置其他值就会暴露。

```
文档迭代成熟度趋势（R1→R6）：
                    R1     R2     R3     R4     R5     R6
  功能完整性       10✅    0      0      0      0      0
  防御的防御        0     8✅     0      0      0      0
  恢复的恢复        0     0     8✅     0      0      0
  并发与时序        0     0     0     5✅     0      0
  逻辑闭环自洽      0     0     0     0     5✅     0
  机制交互盲区      0     0     0     0     0     4项
```

六轮迭代下来，36 项问题已关闭，4 项新发现。`agent-design.md` 已经从一个"设计初稿"进化成了一个在**功能完整性、防御纵深、恢复闭环、竞态覆盖、自洽一致性**五个维度上都经过反复审计的成熟设计文档。剩下的问题不再是"缺什么"，而是"当两个完整机制相遇时，那个接口处是否光滑"。
