# Agent Design Review (R4) — 第四次审计报告

> 审计人：Business Logic Auditor
> 审计对象：`agent-design.md` — 事件响应式 Agent 框架设计（目前最新版）
> 审计日期：2026-05-05
> 前置审计：`agent-design-r1.md`（10 项关注点）、`agent-design-r2.md`（8 项关注点）、`agent-design-r3.md`（8 项关注点）

---

## 第一部分：R3 问题修复状态

逐一核对了 R3 的 8 项关注点在当前 `agent-design.md` 中的修复情况。

| # | R3 关注点 | 等级 | 修复状态 | 证据位置 |
|---|----------|------|---------|---------|
| 1 | Workflow ERROR 无恢复路径 | 🔴 高 | **已修复 ✅** | §3.7 error_recovery 块（RETRY + last_active_state + max_retry_count）；转移表 ERROR→RETRY→last_active_state；状态图完整展示恢复流；风险 #19 |
| 2 | Cron/Timer 重启 catch-up 策略 | 🔴 高 | **已修复 ✅** | §6.5 新增 catch_up: skip\|latest\|all；Timer 默认 skip，Cron 默认 latest；重启恢复 5 步流程；跨实例 leader_election；恢复事件注入限速；风险 #20 |
| 3 | 背压 Level 4 溢出磁盘满无兜底 | 🟡 中 | **已修复 ✅** | §3.3 Level 4B（≥80%告警+回退Level 3）；overflow_max_bytes 硬上限；overflow_warn_threshold: 0.8；建议独立磁盘分区；溢出重启恢复流程；风险 #21 |
| 4 | 插件卸载依赖方确认无超时 | 🟡 中 | **已修复 ✅** | §4.3 on_dependency_unloading 硬超时 30s；超时强制卸载+日志告警；主关闭不阻塞；3 次超时标记 unstable；风险 #22 |
| 5 | Secret 热更新竞态条件 | 🟡 中 | **已修复 ✅** | §9.2 grace_period_sec: 60；活跃连接旧密钥+新连接新密钥；审计日志含新旧指纹；两步提交策略；连接池滚动更新+drain超时；风险 #23 |
| 6 | State Store 命名空间安全声明矛盾 | 🟡 中 | **已修复 ✅** | §5.2 namespace=命名冲突保护非安全隔离；warning "不会阻止 scan(*)"；framework 层 permissions 约束；physical 用于真正隔离；澄清"只防误操作不防恶意攻击"；风险 #24 |
| 7 | Pipeline/Skill 并发模型未定义 | 🟡 中 | **已修复 ✅** | §3.5 Pipeline concurrency serial\|parallel\|limited(N)；parallel 强制 optimistic_lock+独立临时目录；Skill 并发独立配置；风险 #25 |
| 8 | WAL→内存队列投递缺口 | 🟢 低 | **已修复 ✅** | §3.3 待重试队列（指数退避 100ms→500ms→2s）；积压告警；独立队列；重启自动检查；恢复检查清单新增项；风险 #26 |

**结论：R3 提出的 8 项问题全部被认真处理。** R3 的"修复状态总结"已经不再有积压，前三轮一共 26 项问题全部关闭。

---

## 第二部分：第四次评审 R4 新发现的关注点

第四遍阅读——前三次解决了"功能完整性 → 防御的防御 → 恢复的恢复"，这一轮聚焦在"并发与时序的竞态条件"和"不对称边界"。

---

### 🎯 新关注点 1：背压 Level 3 阻塞 Poll 来源但不阻塞 Push 来源 —— 队列永远无法排空（🔴 高）

**场景**：Event Bus 队列 95% 满，触发 Level 3。系统阻塞所有 Poll 事件源的 `poll()` 调用，停止新事件注入。但一个 Push 事件源（例如 Webhook HTTP 服务器）仍在独立线程上接收外部请求并调用 `bus.publish()`。

**💥 可能后果**：
- Push 来源不受 `poll()` 阻塞的影响 —— 它们直接调用 `publish()`
- Level 3 期间 Push 来源继续注入事件，队列深度不降反升
- 系统从 Level 3（95%）径直升到 Level 4（98%）和 Level 5（100%）
- **Level 3 的设计意图（通过阻塞 poll 来排空队列）对 Push 来源完全无效**
- Webhook 接收线程自身也可能 OOM（队列满时 publish 在等待/阻塞/丢弃，但线程仍在接收请求）

**🛠 建议**：
- 在 §3.3 背压 Level 3 描述中明确定义 Push 来源的行为：
  - 方案 A：Level 3 时也暂停 Push 来源的接收（让 webhook 返回 503 Service Unavailable）
  - 方案 B：Push 来源的 `publish()` 在 Level 3+ 时也阻塞或降级
- 在 EventSource 接口中增加 `backpressure_signal(level)` 方法，让 Push 来源可以响应背压
- 风险清单新增 #27 项

---

### 🎯 新关注点 2：State Timeout 与用户事件的竞态条件（🟡 中）

**场景**：Workflow 实例处于 `PENDING` 状态，`state_timeouts` 定义 `PENDING: { timeout: 30 days, on_timeout: CANCELLED }`。第 30 天 23:59:59.500，用户提交 `SUBMIT` 事件。同时，Timeout 定时器也触发了。两个事件同时到达 Event Bus —— 顺序不确定。

**💥 可能后果**：
- 如果 Timeout 事件先被处理：`PENDING → CANCELLED`，然后 `SUBMIT` 到达但状态已是 CANCELLED —— `{ from: PENDING, event: SUBMIT }` 不匹配，事件被丢弃
- 用户提交的表单因毫秒级竞态条件被取消，用户感知为"提交了但被莫名其妙取消了"
- **文档没有为 Timeout 事件定义优先级**，它可能与用户事件在同一优先级队列中，顺序取决于调度器实现
- 同样的问题存在于 `REVIEWING → 7天超时 → REJECTED` vs 用户在最后一天 `APPROVE`

**🛠 建议**：
- 在 Workflow 中定义 Timeout 事件的优先级或处理顺序：
  - 建议：**Timeout 事件优先级低于用户事件**（用户主动操作优先于自动超时）
  - 或者：状态转移表中对超时路径增加 `race_on: SUBMIT` 标记，使超时在检测到用户事件时退让
- 或者在状态机实现中，timeout 触发时检查事件队列中是否有同实例的待处理用户事件，有则延迟超时
- 风险清单新增 #28 项

---

### 🎯 新关注点 3：Pipeline Transformer 步骤的副作用在补偿链中缺失（🟡 中）

**场景**：Pipeline "doc-processor" 有 4 步：`[Transform: 读取文件 → OCR 提取]` → `[Action: insert-db]` → `[Action: notify-slack]` → `[Action: archive-file]`。Transformer 产生中间文件（OCR 缓存、提取出的图像切片）。第三步 `notify-slack` 失败 → 触发补偿。补偿链 `reverse_order` 覆盖了 Step 4 (archive-file) → Step 3 (notify-slack) → Step 2 (insert-db)。**Step 1 (Transformer) 的补偿被遗漏了**。

**💥 可能后果**：
- Transformer 产生的中间文件（OCR 缓存、图像切片、临时 PDF 副本）没有被清理
- 这些文件累积占用磁盘空间 —— 对于高吞吐 Pipeline，可能在短期内产生 GB 级别的残留
- 文档在 §3.5 说"纯计算/只读步骤（如 Filter、校验）不需要补偿定义" —— 但 Transformer 不是只读的，它可能产生副作用
- Transformer 和 Action 之间的边界模糊：如果 Transform 的操作被 Action 代码隐式依赖，补偿顺序也需要注意

**🛠 建议**：
- 在 §3.5 Pipeline 定义中明确说明：**Transform 步骤也可能有副作用（临时文件、缓存写入、状态变更），应支持定义补偿**
- 框架应不区分 Transform 和 Action 的补偿能力 —— 统一对待，每个步骤都可选声明 `compensate`
- 或者在框架层面增加"步骤自动清理"机制：任何步骤分配的临时文件/资源，框架自动追踪并在补偿时清理
- 风险清单新增 #29 项

---

### 🎯 新关注点 4：背压 Level 4A→4B 转换缺少滞回（Hysteresis），可能来回振荡（🟢 低）

**场景**：队列 98% 满 → Level 4A 溢出到磁盘。溢出目录使用率达到 80% → Level 4B 触发：阻塞 poll，不再溢出到磁盘。然后事件风暴减弱，队列排空到 90% → 系统退出 Level 4 区域（按 §9.1 配置，Level 4 threshold 是 0.98，Level 3 是 0.95）。但此时溢出目录使用率仍接近 80% —— 如果队列很快再次满到 98%，立即回到 Level 4A，然后溢出目录 80% → 4B。

**💥 可能后果**：
- 队列快满→溢出→磁盘快满→阻塞→队列下降→解除阻塞→队列快满→溢出→...
- 没有滞回（hysteresis）设计，背压在 Level 4A 和 Level 4B 之间来回振荡
- 系统日志充满背压级别转换记录
- 溢出目录在"满"和"不满"边界处产生大量小周期

**🛠 建议**：
- 为 Level 4A→4B 和 Level 4B→4A 的转换增加滞回区间：
  - 进入 Level 4B：溢出目录 ≥80%
  - 离开 Level 4B（回到 4A）：溢出目录降到 ≤60%（而非刚降到 79% 就切回）
- 或增加 `backpressure_stabilization_sec` 参数：在某个级别停留至少 N 秒才允许降级
- 或更简单：Level 4B 触发后，保持 Level 3 阻塞直到溢出目录清空到 50% 以下
- 风险清单新增 #30 项

---

### 🎯 新关注点 5：Event.lifespan_ms 字段定义但无约束机制（🟢 低）

**场景**：OCR Pipeline 产生中间文件 `/tmp/ocr-cache/abc.png`，在 Event payload 中通过 `metadata.lifespan_ms: 300000`（5分钟）声明了临时资源的生命周期。但框架没有为这个字段定义任何自动清理机制。

**💥 可能后果**：
- `lifespan_ms` 是一个纯粹的"文档声明" —— 没有守护进程或定时器在监控它
- 5 分钟后 OCR 缓存文件不会被自动删除
- 如果 Pipeline 高吞吐运行（每分钟处理 10 个文件），会产生大量 5 分钟寿命的临时文件
- 开发者以为框架会清理，但框架什么也没做 —— 承诺与实现分离
- 对比：§3.1 TTL（事务超时）有明确的 Event Bus 丢弃行为，但 `lifespan_ms` 完全没有行为定义

**🛠 建议**：
- 删除 `lifespan_ms` 字段（如果框架不打算实现自动清理），或者
- 明确实现机制：
  - 框架注册一个定时清理器：`schedule_cleanup(event_id, lifespan_ms, cleanup_action)`
  - 或者在 `Tool Runner` 层增加资源生命周期管理：Tool 完成后自动清理分配给它的临时目录
  - 或者在输出中声明"此字段为预留接口，当前版本未实现自动清理"
- 风险清单新增 #31 项

---

## 修复状态总结

```
R1 问题：10/10 已修复 ✅
R2 问题：8/8 已修复 ✅
R3 问题：8/8 已修复 ✅
R4 新发现问题：5 项
  - 背压 Level 3 Push 来源不受阻塞 (🔴 高)       → 新增 #1
  - State Timeout 与用户事件竞态 (🟡 中)          → 新增 #2
  - Pipeline Transformer 副作用的补偿缺失 (🟡 中)  → 新增 #3
  - 背压 Level 4A↔4B 滞回缺失 (🟢 低)            → 新增 #4
  - lifespan_ms 字段无约束机制 (🟢 低)            → 新增 #5
```

---

## 实施建议优先级

```
P0（阻止上线）
  └── 背压 Level 3 应同时阻塞 Push 来源（新 #1）
       └── 否则 Push 来源可以绕过背压，导致队列永远排不空

P1（上线前必须解决）
  ├── State Timeout 与用户事件竞态（新 #2）
  └── Pipeline Transformer 副作用的补偿（新 #3）

P2（beta 前建议解决）
  └── 背压 Level 4A↔4B 滞回设计（新 #4）

P3（持续改进）
  └── lifespan_ms 字段的清理机制定义或移除（新 #5）
```

---

## 最终评价

这是一个在防御纵深上做得非常好的设计文档。三次审查下来：

- **R1 层（基础功能）**：幂等性、补偿、背压、状态机完备 —— 全部覆盖
- **R2 层（防御的防御）**：补偿的补偿、告警的告警、去重的去重 —— 全部覆盖
- **R3 层（恢复的恢复）**：ERROR 恢复路径、Cron catch-up、溢出兜底 —— 全部覆盖
- **R4 层（并发与时序的竞态）**：Push 来源绕过背压、State Timeout 竞态、Transformer 副作用补偿缺失、背压滞回 —— 盲区已识别

五个新问题集中在**竞态条件**和**不对称边界**上。第 #1 号（Push 来源绕过背压）是最严重的 —— 它是为数不多的、能让文档中精心设计的 5 级背压体系失效的路径。Push 来源天然不受 `poll()` 阻塞影响，这个不对称性是目前设计中最大的防御缺口。

```
文档迭代成熟度趋势：
                    ┌─────────────────┐
                    │     R1: 基础     │  10 项 → 全部修复
                    │  功能完整性      │
                    └────────┬────────┘
                             │
                    ┌────────▼────────┐
                    │     R2: 防御    │  8 项 → 全部修复
                    │  的防御          │
                    └────────┬────────┘
                             │
                    ┌────────▼────────┐
                    │  R3: 恢复的恢复 │  8 项 → 全部修复
                    │  边界的边界      │
                    └────────┬────────┘
                             │
                    ┌────────▼────────┐
                    │  R4: 并发与时序 │  5 项 → 新发现
                    │  的不对称边界    │
                    └─────────────────┘
```
