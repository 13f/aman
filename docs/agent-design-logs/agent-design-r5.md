# Agent Design Review (R5) — 第五次审计报告

> 审计人：Business Logic Auditor
> 审计对象：`agent-design.md` — 事件响应式 Agent 框架设计（当前最新版）
> 审计日期：2026-05-05
> 前置审计：`agent-design-r1.md`（10 项关注点）、`agent-design-r2.md`（8 项关注点）、`agent-design-r3.md`（8 项关注点）、`agent-design-r4.md`（5 项关注点）

---

## 第一部分：R4 问题修复状态

逐一核对了 R4 的 5 项关注点在当前 `agent-design.md` 中的修复情况。

| # | R4 关注点 | 等级 | 修复状态 | 证据位置 |
|---|----------|------|---------|---------|
| 1 | 背压 Level 3 阻塞 Poll 但不阻塞 Push 来源 | 🔴 高 | **已修复 ✅** | §3.3 Level 3 明确"通知所有 Push 事件源（backpressure_signal(3)），暂停 publish()"；关键原则"Push 来源同步原则"（line 267）；config 中 `level3_block_push: true`（line 1264）；Risk #27 |
| 2 | State Timeout 与用户事件竞态 | 🟡 中 | **已修复 ✅** | §3.7 新增"超时与用户事件竞态处理"块（lines 592-600），含 5 条实现要求（优先级、队列检查、5s 延迟窗口、重新检查）；Risk #28 |
| 3 | Pipeline Transformer 副作用补偿缺失 | 🟡 中 | **已修复 ✅** | §3.5 Transform 注意段（lines 474-477）："框架不区分 Transform 和 Action 的补偿能力——每个步骤都可选声明 compensate"；Risk #29 |
| 4 | 背压 Level 4A↔4B 滞回缺失 | 🟢 低 | **已修复 ✅** | `level4b_hysteresis_leave: 0.5`（line 1267）、`overflow_hysteresis_leave: 0.5`（line 276）；离开条件 ≤50%（30% 滞回区间）；Risk #30 |
| 5 | lifespan_ms 字段无约束机制 | 🟢 低 | **已修复 ✅** | 标注为"预留接口，v1.0 未实现自动清理"+"计划 v2.0 实现"+"当前开发者需在 compensate/on_final 中自行清理"（lines 85-87）；Risk #31 |

**结论：R4 提出的 5 项问题全部被认真处理。** 前四轮一共 31 项问题全部关闭。

---

## 第二部分：第五次评审 R5 新发现的关注点

第四遍把文档每一个机制从"触发 → 执行 → 恢复"闭环走通，发现了 5 项新问题。核心转向了**设计目标与默认行为的自相矛盾**和**文档内部承诺不一致**。

---

### 🎯 R5 关注点 1：ERROR 恢复路径中 retry_count 被重置导致无限重试循环（🔴 高）

**场景**：
§3.7 Workflow 定义了两条关于 retry_count 的规则，它们在逻辑上是矛盾的：

```
// （A）on_enter ERROR：重置 retry_count = 0（在 ERROR 状态内可用于追踪重试次数）
// （B）RETRY 转移守卫：retryCount < max_retry_count（max = 3）
```

具体流程：
1. PENDING → `REVIEWING`（某步出错）→ **ERROR**（retry_count 被**重置为 0**）
2. `RETRY` 事件到达 → guard 检查 `retryCount (0) < max_retry_count (3)` → 通过
3. 恢复到 `last_active_state`（REVIEWING）
4. 如果根因未修复 → 再次出错 → 回到 **ERROR**（retry_count **又被重置为 0**）
5. 回到步骤 2 —— **无限循环**

**💥 可能后果**：
- retry_count 永远不会累积到 max_retry_count，guard 永远不会触发 `on_fail: ARCHIVED`
- 唯一逃生路径是 7 天 `ERROR→ARCHIVED` 超时 —— 但这意味着每次 RETRY 失败都要等 7 天才能自动归档
- 如果故障是瞬时性的（网络抖动）且每次 RETRY 恰好都失败，系统在 7 天内反复跳进跳出 ERROR，每次产生告警日志
- 更糟的是：如果 root cause **不可自动修复**（如第三方 API 永久下线），Agent 会在 7 天内不断进行 RETRY 尝试（取决于 retry_backoff 策略），产生大量重复请求和日志

**🛠 建议**：
- 在 §3.7 ERROR 状态默认行为中，将"重置 retry_count = 0"改为"递增全局重试计数器"或"递增 per-instance 的 retry_attempt_count"
- 或者明确拆分两个计数器：
  - `session_retry_count`：当前 ERROR 会话内的重试次数（进入 ERROR 时重置）—— 用于单次 ERROR 会话内追踪
  - `total_retry_count`：累积重试次数（不重置）—— 用于 guard 上限判断
- guard 应检查 `total_retry_count` 而非 `session_retry_count`
- 验证方式：状态图（line 663-668）中的 `RETRY │ (max_retry_count=3) │ last_active_state` 路径必须实际可达，不因计数器重置而无限循环
- 风险清单新增 #32 项

---

### 🎯 R5 关注点 2：State Timeout 时钟在 ERROR 状态中的语义未定义（🟡 中）

**场景**：
Workflow 处于 `REVIEWING` 状态，`state_timeouts` 定义 `REVIEWING: { timeout: 7 days, on_timeout: REJECTED }`。

时间线：
- Day 0：进入 REVIEWING（7 天倒计时开始）
- Day 3：出错 → 过渡到 ERROR
- Day 5：根因修复 → RETRY 恢复到 REVIEWING

**未定义的语义**：当 workflow 在 Day 3~5 处于 ERROR 状态时，`REVIEWING` 的超时时钟发生了什么？

| 可能性 | 行为 | 后果 |
|--------|------|------|
| **时钟继续走** | Day 7 触发超时（从 Day 0 开始计算） | 用户只有 4 天有效工作时间（Day 0~3 + Day 5~7），实际可用时间被 ERROR 消耗 |
| **时钟暂停** | 恢复到 REVIEWING 时，剩余倒计时 4 天 | 留存的 4 天是用户预期的时间窗口 |
| **时钟重置** | 恢复到 REVIEWING 时，重新开始 7 天倒计时 | 用户实际获得 9 天总窗口（3+2+7），可能过长 |

**💥 可能后果**：
- 开发者不知道框架采用哪种语义，实现时可能产生意外行为
- 如果实际实现是"时钟继续走"，ERROR 恢复回来的 workflow 可能在几小时内就超时，给用户的感觉是"我刚恢复就又过期了"
- 如果"时钟重置"，审批流程可能被无限延长（反复通过 ERROR→RETRY 来重置审批计时器）—— 结合 \#1 的无限循环问题，计时器重置意味着用户可以无限期地绕开超时

**🛠 建议**：
- 在 §3.7 state_timeouts 中明确定义：**状态退出时超时时钟暂停，重新进入时恢复计时**（暂停语义）
- 或定义可配置策略：`timeout_behavior_on_state_exit: pause | reset | continue`
- 需要在不同策略下评估安全性：
  - `continue` 太激进（恢复后即刻过期）
  - `reset` 太宽松（可被 ERROR→RETRY 无限重置超时）
  - `pause` 是折中（推荐）
- 风险清单新增 #33 项

---

### 🎯 R5 关注点 3：状态图与 state_timeouts 配置不一致——终态归档路径缺失（🟡 中）

**场景**：
§3.7 状态图（lines 638-658）展示了三条自动归档路径：

```
APPROVED ──→ (30天后自动) ──→ ARCHIVED
REJECTED ──→ (30天后自动) ──→ ARCHIVED
CANCELLED ──→ (30天后自动) ──→ ARCHIVED
```

但检查两处 state_timeouts 定义：

```
// 代码块 (lines 585-590):
state_timeouts: {
    REVIEWING: { timeout: 7 days, on_timeout: REJECTED }
    PENDING:   { timeout: 30 days, on_timeout: CANCELLED }
    ERROR:     { timeout: 7 days, on_timeout: ARCHIVED }
}

// YAML 配置 (lines 1381-1384):
state_timeouts:
  reviewing: { timeout: "7d", on_timeout: rejected }
  pending:   { timeout: "30d", on_timeout: cancelled }
  error:     { timeout: "7d", on_timeout: archived }
```

**`APPROVED`、`REJECTED`、`CANCELLED` 三个状态均无 timeouts 定义。**

§3.7 "终态回收"段（lines 677-680）只说：`ARCHIVED` 状态停留超 30 天 → 归档冷存储。这是针对 `ARCHIVED` 状态本身的管理机制，不是从 `APPROVED/REJECTED/CANCELLED` 到 `ARCHIVED` 的转移规则。

**💥 可能后果**：
- 所有终态实例在 state store 中永久累积
- 高吞吐系统（每天数千份订单/审批）→ State Store 线性膨胀 → 查询性能下降 → 最终 OOM
- 文档内部不一致：图说"30 天后自动归档"，配置没定义
- 实现时开发者可能只实现了 config 中的 timeouts，完全忽略图上三条路径

**🛠 建议**：
- 方案 A：在 state_timeouts 中为 `APPROVED/REJECTED/CANCELLED` 各添加一个超时到 `ARCHIVED` 的条目
- 方案 B：如果意图是这些状态有独立的 GC 机制而非 state_timeout，则：
  - 在状态图上标记此类转移为"GC 回收"而非"超时转移"，区分视觉样式（如虚线箭头 + GC 标签）
  - 在"终态回收"段明确描述回收策略和可配置参数（如 `final_state_gc_days: 30`）
- 无论如何，消除文档内部的图→配置不一致
- 风险清单新增 #34 项

---

### 🎯 R5 关注点 4：FileWatchSource 稳定确认在非本地文件系统上失效（🟢 低）

**场景**：
§3.2 FileWatchSource 的稳定确认机制（lines 188-203）依赖 `lsof` / 文件锁检测来判断文件写入是否完成：

```
4. 计时器到期 → 检查文件是否仍然打开（lsof / 文件锁检测）
```

但这个机制在非本地文件系统上可能失效：
- **NFS/CIFS/SMB**：文件锁（flock）支持有限，lsof 跨网络可能看不到远端打开句柄
- **FUSE**：某些 FUSE 实现不报告标准的文件锁状态（如 s3fs、gcsfuse）
- **云对象存储挂载（s3fs/gcsfuse）**：没有传统意义上的"打开文件"概念

**💥 可能后果**：
- 在 `/watch/invoices` 挂载了 NFS 共享的场景下，lsof 可能永远返回"文件未打开"
- 稳定确认一直失败 → 不断重置静默计时器 → 文件变更事件**永远不发布**
- 或者：max_stable_wait（30s）超时 → 强制发布 `incomplete: true` → 下游反复收到不完整文件
- 开发者可能根本没意识到自己的环境触发了这个退化路径

**🛠 建议**：
- 在 §3.2 的稳定性说明中增加一段：**"文件锁检测依赖于本地文件系统特性。在 NFS/CIFS/FUSE 等远程/虚拟文件系统上，锁检测行为可能退化，建议在远程文件系统上增大 debounce_ms 和 max_stable_wait_ms"**
- 提供配置选项：当 `check_open_files` 在特定文件系统类型上不可靠时，回退到纯 debounce 模式（例如 `check_open_files: auto | true | false`，`auto` 在本地 FS 启用，远程 FS 禁用）
- 风险清单新增 #35 项

---

### 🎯 R5 关注点 5：Cron rate_limit 聚合语义未定义（🟢 低）

**场景**：
§6.4 定义 `rate_limit: 100` 表示每秒最多产生 N 个 CRON_TICK 事件。

但文档未定义当同一秒内触发的 cron job 超过 rate_limit 时的行为：
- 如果有 150 个 cron job 都配置在 `"0 * * * *"`（每小时整点触发），第一秒内涌入 150 个 CRON_TICK
- rate_limit 100 → 50 个超额

类似地，在一个 cron job 的 catch_up = all 场景下（Agent 停机 2 小时后重启，某个每秒一次的 cron job 有 7200 个错过的触发点），这些事件以 rate_limit 100/s 注入，需要 >1 分钟才能全部注入。

**💥 可能后果**：
- 超额事件的 50 个 job 行为未定义——是被静默丢弃？延迟到下一秒？进入待发队列？
- 如果静默丢弃，某些 cron job 会"被漏跑"，操作员不知情
- 如果延迟到下一秒，下一波可能又超出 50 → 级联延迟
- catch-up + rate_limit + overflow 三层交互可能导致非预期行为

**🛠 建议**：
- 在 §6.4 中明确 rate_limit 超额的语义：`overflow_behavior: drop | delay | queue`
- 建议：`delay`（延迟到下一秒）+ 记录延迟日志，这样没有 job 被漏跑但操作员可以看到延迟
- 在 §6.5 catch_up 中也补充说明：catch_up + rate_limit 的交互（如果 rate_limit 较慢，catch_up 注入会被拉长）
- 风险清单新增 #36 项

---

## 修复状态总结

```
R1 问题：10/10 已修复 ✅
R2 问题：8/8  已修复 ✅
R3 问题：8/8  已修复 ✅
R4 问题：5/5  已修复 ✅
R5 新发现问题：5 项
  - ERROR retry_count 重置导致无限循环 (🔴 高)       → 新 #1
  - State Timeout 时钟在 ERROR 的语义未定义 (🟡 中)  → 新 #2
  - 状态图与配置不一致：终态归档路径缺失 (🟡 中)    → 新 #3
  - FileWatchSource 非本地 FS 锁检测失效 (🟢 低)    → 新 #4
  - Cron rate_limit 聚合语义未定义 (🟢 低)           → 新 #5
```

---

## 实施建议优先级

```
P0（阻止上线）
  └── ERROR retry_count 重置导致无限重试循环（新 #1）
       └── 计数器重置使 max_retry_count guard 形同虚设，
           唯一逃生路径只有 7 天超时归档

P1（上线前必须解决）
  ├── State Timeout 时钟在 ERROR 中的语义（新 #2）
  │    └── 四种语义（continue/pause/reset）导致完全不同的业务行为，
  │        且结合 #1 的无限循环可被利用来规避超时
  └── 状态图与配置的终态归档不一致（新 #3）
       └── 文档内部矛盾，实现一定会踩坑

P2（beta 前建议解决）
  └── FileWatchSource 非本地 FS 的锁检测退化（新 #4）

P3（持续改进）
  └── Cron rate_limit 超额语义（新 #5）
```

---

## 最终评价

**R4 的全部 5 个问题已被认真修复**，文档在背压完整性（Push vs Pull 对称性）、时间竞态处理（Timeout deferral）、补偿覆盖（Transform 副作用）和承诺管理（lifespan_ms 标记）上做到了知行合一。

R5 发现的 5 个新问题集中在**恢复路径的闭环完整性**和**文档内部一致性**上：

- **#1 号（ERROR retry_count 重置）**是这一轮最危险的——它不是一个缺失功能，而是一个**逻辑反模式**：文档一边说"限制最多重试 3 次"，一边在进入 ERROR 时重置计数器，让限制无法生效。这是 31 项之前的问题都未涉及的新维度——**设计目标与默认行为自相矛盾**。
- **#2 号和 #3 号**都是**文档承诺与定义之间的缝隙**——状态图展示了行为，但配置没有对应的定义；文档定义了超时行为，但没定义跨状态退出后的时钟语义。
- **#4 号和 #5 号**是边界的边界——在特定环境下（远程文件系统、大量 cron job）才会暴露，但影响是静默的（文件事件永远不发 / cron job 悄悄漏跑）。

```
文档迭代成熟度趋势（R1→R5）：
                    R1     R2     R3     R4     R5
  功能完整性       10✅    0      0      0      0
  防御的防御        0     8✅     0      0      0
  恢复的恢复        0     0     8✅     0      0
  并发与时序        0     0     0     5✅     0
  逻辑闭环自洽      0     0     0     0     5项
```

从 R1 到 R4，每一轮都在添加新的防御维度。R5 揭示了文档已经进入了一个新的阶段：**不再缺少防御机制，而是在已有机制的内部出现了自相矛盾的定义**。#1 号（retry_count 重置）尤其典型——不是"没防御"，而是"两个防御互相抵消"。这是设计文档成熟到一定阶段才会出现的问题。
