# Agent Design Review (R2) — 再审计报告

> 审计人：Business Logic Auditor
> 审计对象：`agent-design.md` — 事件响应式 Agent 框架设计（最新版）
> 审计日期：2026-05-05
> 前置审计：`agent-design-r1.md`（10 项关注点）

---

## 第一部分：R1 问题修复状态

逐一核对了 R1 的 10 项关注点。

| # | R1 关注点 | 等级 | 修复状态 | 证据位置 |
|---|----------|------|---------|---------|
| 1 | 事件幂等性与重复交付 | 🔴 高 | **已修复 ✅** | §3.1 delivery 语义表 + §3.3 去重策略 + 处理器幂等契约 + §9 dedup 配置 |
| 2 | Pipeline 失败回滚黑洞 | 🔴 高 | **已修复 ✅** | §3.5 Saga 补偿 + DLQ 完整定义 + §8 示例展示了补偿路径 |
| 3 | 背压 drop_lowest = 静默丢数据 | 🔴 高 | **已修复 ✅** | §3.3 Level 1-5 分层策略 + delivery 与 priority 正交（决策 6） |
| 4 | Workflow 幽灵状态 | 🟡 中 | **已修复 ✅** | §3.7 ERROR/ARCHIVED 态 + state_timeouts + guard.on_fail + on_enter/on_leave/on_final |
| 5 | Cron 动态频率无守卫 | 🟡 中 | **已修复 ✅** | §6.4 min_interval:1s 硬编码 + reconfigure 鉴权 + 审计日志 + 速率限制 |
| 6 | 时区语义未定义 | 🟡 中 | **已修复 ✅** | §6.2 UTC 统一约定 + DST 三策略配置 + 配置示例含 timezone |
| 7 | 插件依赖无环检测 | 🟡 中 | **已修复 ✅** | §4.3 拓扑排序 + 环检测 + on_dependency_unloading 通知 |
| 8 | State Store 无并发语义 | 🟡 中 | **已修复 ✅** | §5.2 三种并发模式 + CAS + 命名空间隔离 + 共享权限声明 |
| 9 | 风险章节自身疏漏 | 🟡 中 | **已修复 ✅** | 风险 #8 数据完整性 / #9 配置泄露 / #10 审计缺失 + 定级矩阵 + RPO/RTO |
| 10 | Checkpoint 恢复不完整 | 🟡 中 | **已修复 ✅** | §3.3 WAL 模式 + RPO/RTO 目标 + 恢复检查清单 |

**结论**：R1 提出的 10 项问题全部被认真处理。Saga 补偿、分层背压、强制 TraceID 这些在 R1 中建议但原文缺失的机制，现在都有了实质性重构——不是贴标签式的补救。

---

## 第二部分：第二次评审 R2 新发现的关注点

第二遍阅读——带着"还有什么没想到的"的心态——发现了 8 个新问题。

---

### 🎯 新关注点 1：补偿操作自身的幂等性与失败路径（🔴 高）

**场景**：Pipeline 故障触发反向补偿。步骤 4 的补偿 `slack-delete-message` 成功，步骤 3 的补偿 `cleanup-temp-file` 失败（文件已被其他进程占用 / 磁盘故障）。然后整个补偿被标记为"已完成"。

**可能后果**：
- 文档说"补偿失败不会导致系统崩溃，只会记录告警"——但部分补偿成功 + 部分补偿失败 = 系统处于**半回滚**状态，这是最坏的状态（既不知道数据一致与否，也无法自动恢复）
- `slack-delete-message` 补偿本身不是幂等的——Slack 消息已被手动删除时，第二次调用会失败
- 补偿路径没有自己的重试策略和补偿的补偿

**建议**：
- 补偿操作强制要求幂等（如 delete-by-id 天然幂等，但需要框架验证）
- 补偿失败后的升级路径：半回滚 → 进入专门的"补偿失败告警"通道 → 标记 Pipeline 实例为 `COMPENSATION_FAILED` 状态 → 人工接管
- 补偿操作本身的超时保护（文档中只提了 Tool 超时，未提补偿执行超时）

---

### 🎯 新关注点 2：DLQ 到期事件静默消失（🟡 中）

**场景**：`dlq_ttl_days: 30`。30 天前有 3 条发票处理失败事件在 DLQ 中无人处理。

**可能后果**：
- TTL 到期后事件被清理——但没有任何通知说"有 3 条事件即将被删除"
- 如果操作员休假 30 天，回来后 DLQ 空了，他根本不知道曾经有 3 张发票没处理
- §9 的运行时接口提供了 `GET /dlq` 和 `POST /dlq/{id}/retry`，但完全是**拉模式**——没有主动告警推送

**建议**：
- DLQ TTL 到期前不应直接删除：默认策略改为"到期前 7 天/3 天/1 天发送告警"
- 或改为"归档到冷存储 + 删除活跃记录"而非直接删除
- 建议 DLQ 有定期摘要通知（"过去 24h 有 N 条事件进入 DLQ，M 条即将到期"）

---

### 🎯 新关注点 3：ERROR 状态的静默终结（🟡 中）

**场景**：Workflow 进入 ERROR 状态。7 天后 `state_timeouts` 自动转到 ARCHIVED。

**可能后果**：
- 进入 ERROR 状态一定有原因（DB 不可用、外部 API 挂了、数据损坏）。状态机**自动**从 ERROR 转到 ARCHIVED，意味着"业务异常被静默归档"
- 没有任何告警会在 ENTER_ERROR 时触发——operator 只有 7 天后归档时才会知道（如果那时还在关注的话）
- §3.7 的 `on_enter(state)` 和 `on_leave(state)` 生命周期钩子可以在此场景用于告警，但文档没有为 ERROR 状态约定任何标准行为

**建议**：
- `ERROR` 状态的 `on_enter` 应默认触发**告警**（不再给 operator 7 天沉默期）
- 建议工作流定义中 ERROR 状态必须有 `on_enter: alert` 的默认行为，而不是纯靠开发者自觉实现
- `ERROR → ARCHIVED` 的超时转换前也应触发"即将归档"告警

---

### 🎯 新关注点 4：parent_event_id 循环链路（🟡 中）

**场景**：事件 A 处理产生事件 B，B 处理产生事件 A（通过不同 route/transform 来由）。parent_event_id 链：A → B → A' → B' → ...

**可能后果**：
- §11 风险表 #2 定义了"循环事件"检测（TTL + 最大传递次数 + 循环检测规则），但只检了 **type+source** 的循环，没有检查 **parent_event_id 链路本身的循环**
- 链路追踪工具 `/events/trace/{trace_id}` 如果追溯经过同一 event id 的循环，会导致显示层无限滚动或栈溢出
- 即使事件被 TTL 停止，链路树在日志中可能有环，人工排查时极为困难

**建议**：
- parent_event_id 链路跟踪中应检测循环：同一个 event id 不能在 parent chain 中出现两次
- 链路追踪 API 返回时应截断循环支路并以 `"[truncated - cycle detected]"` 标记
- 考虑将"循环检测"提升到 Event Bus 层（不限于 Pipeline），任何链路中出现重复 event id 就触发 alert

---

### 🎯 新关注点 5：内存 Event Bus + 持久化配置的幽灵保证（🟡 中）

**场景**：配置 `event_bus.type: in_memory`，但同一配置块仍包含 `persistence.wal_path` / `checkpoint_interval`。

**可能后果**：
- 配置不会报错——WAL 配置在内存模式下被静默忽略
- 运维人员切换 type 从 persistent → in_memory 时，WAL 等配置仍然存在但无效，容易误以为系统有持久化保护
- 内存模式的去重表在重启时完全重置——WAL 重放的事件经过空白的去重窗口可能全部通过

**建议**：
- type: in_memory 时，配置校验层应拒绝 `persistence` 相关字段，或至少发出结构化告警日志
- 去重表的状态应与 Event Bus 模式绑定：persistent 模式下去重状态也持久化（WAL + checkpoint），in_memory 模式下去重表是纯内存的

---

### 🎯 新关注点 6：控制接口的认证盲区（🟡 中）

**场景**：§9.3 列出了一整套运行时控制接口——`POST /agent/shutdown`、`POST /cron/{id}/update`、`POST /plugin/{name}/disable`、`POST /dlq/{id}/retry`——但没有任何认证/授权定义。

**可能后果**：
- 如果控制接口绑定到 0.0.0.0（或 Docker 默认暴露端口），任何能访问该端口的第三方都可以 shutdown agent、禁用插件、修改 cron 配置
- `POST /inject-event` 在调试模式下存在——但如果暴露到生产环境，外部可以伪造任意事件注入系统
- cronsource 的 reconfigure 在 §6.4 说需要鉴权，但控制接口本身没定义鉴权机制

**建议**：
- 控制接口默认绑定到 localhost / Unix socket
- 如果暴露到网络，必须定义认证机制（API Token / mTLS / OAuth2）
- 敏感操作（shutdown、disable plugin、dlq retry）应有操作审计和二次确认
- `POST /inject-event` 应在生产环境默认禁用

---

### 🎯 新关注点 7：Skill trigger 缺少 payload 级匹配（🟢 低）

**场景**：Skill trigger 定义 `{ event_type: TIMER_TICK, source: "cron:daily-tasks" }`。该 cron job 可能发送 `payload: { task: "report" }` 和 `payload: { task: "backup" }`。

**可能后果**：
- 同一 source 的 TIMER_TICK 即使 payload 不同，也无法区分路由——Skill 必须在 execute() 内自己 if/else 判断
- 失去了声明式路由的优势，增加 Skill 内部逻辑复杂度
- 对比 Dispatcher 的 `match: {type, source, priority}` 支持多字段匹配，Skill trigger 的匹配能力明显较弱

**建议**：
- Skill trigger 增加 payload 条件匹配，与 Dispatcher 的 match 语法对齐：
  ```yaml
  triggers:
    - event_type: TIMER_TICK
      source: "cron:daily-tasks"
      match: { payload.task: "report" }
  ```

---

### 🎯 新关注点 8：FileWatchSource 强制发布的数据完整性窗口（🟢 低）

**场景**：一个 2GB 的大文件写入耗时超过 `max_stable_wait_ms: 30000`（30s）。FileWatchSource 强制发布 `FILE_CREATED`。

**可能后果**：
- 文档明确说了这种情况会"强制发布"——但强制发布的事件没有标记
- 下游 Pipeline（如 `invoice-processor`）拿到一个不完整的文件开始 OCR，结果是"成功处理了半份文件"
- 这是设计中的"已知危险窗口"，但缺少防御信号：下游无法区分"稳定到达的文件"和"强制发布的不完整文件"

**建议**：
- 强制发布的事件应在 payload 中添加 `incomplete: true` 标志
- 下游 Pipeline 可选择：推迟处理（等待后续 MODIFIED 事件）、标记"来源可疑"、或切换降级策略
- 或增加配置选项：`force_publish_on_timeout: true | false | mark_incomplete`

---

## 修复状态总结

```
R1 问题：10/10 已修复 ✅
R2 新发现问题：8 项
  - 补偿失败路径 (🔴 高)     → 新增
  - DLQ 到期消失 (🟡 中)     → 新增
  - ERROR 静默终结 (🟡 中)    → 新增
  - parent_event_id 循环 (🟡 中)  → 新增
  - 内存模式+持久化配置 (🟡 中)  → 新增
  - 控制接口认证 (🟡 中)       → 新增
  - Skill trigger payload 匹配 (🟢 低)  → 新增
  - 强制发布不完整标记 (🟢 低)  → 新增
```

---

## 实施建议优先级

```
P0（阻止上线）
  └── 补偿操作幂等性与失败升级路径（新#1）

P1（上线前必须解决）
  ├── DLQ 到期前告警 + 归档冷存储（新#2）
  ├── ERROR 状态自动触发告警（新#3）
  └── parent_event_id 循环检测（新#4）

P2（beta 前建议解决）
  ├── 控制接口认证 + 默认 localhost 绑定（新#6）
  └── 内存模式拒绝 persistence 配置（新#5）

P3（持续改进）
  ├── Skill trigger payload 级匹配（新#7）
  └── 强制发布事件标记 incomplete（新#8）
```

---

## 最终评价

R1 到当前版本的迭代质量很高。10 项关注点全部得到实质性修复——不是贴标签式的补救，而是重新设计了事件模型（delivery/priority 正交）、补偿机制（Saga）、和可观测性（强制 TraceID）。这说明设计者认真对待了上轮反馈。

新发现的问题主要是第二层深度的问题：**补偿的补偿**、**告警的告警**、**恢复的恢复**。这些问题在 R1 的文档版本中甚至不配出现（因为第一层问题还没解决），现在第一层问题消除了，第二层问题才浮出水面。

这是好事——说明设计文档在变厚、防御在加深、思考在递进。
