# Agent Design Review (R17) — 第十七次审计报告

> 审计人：Business Logic Auditor
> 审计对象：`agent-design.md` (2141 行) — 事件响应式 Agent 框架设计
> 审计日期：2026-05-06
> 前置审计：`agent-design-r1.md` ~ `agent-design-r16.md`

---

## 第一部分：R16 问题修复验证

| # | R16 关注点 | 等级 | 修复状态 | 证据位置 |
|---|-----------|------|---------|---------|
| 1 | 核心大写状态名与 YAML 小写状态名不一致，无大小写敏感性声明 | 🟡 中 | **已修复 ✅** | §3.7 line 793-799: 6 条规则——运行时 normalize 为统一大写、大小写不敏感比较、配置校验发警告、事件名同理 |
| 2 | §6.4 reconfigure 权限模型未同步更新 `/cron/add/update/remove` 跨引用 | 🟢 低 | **已修复 ✅** | §6.4 line 1368-1372: 标题改为"运行时 cron 变更操作（覆盖 PUT + POST 端点）"，第 3 条明确含三个端点 |
| 3 | 状态图未反映 ERROR→CANCEL 路径 | 🟢 低 | **已修复 ✅** | §3.7 line 955-956: 新增 `CANCEL ──→ CANCELLED` 箭头及 `(隐式 guard: 见 §3.7 双出口规则)` 注释 |
| 4 | secret_cache_fallback 本地缓存安全性未定义 | 🟢 低 | **已修复 ✅** | §2.5.1 line 76-87: 5 项约束——AES-256-GCM 加密、文件权限 600、TTL 300s 可配、不存储明文、Phase 1 不可用 |
| 5 | §9.1 workflow 状态名小写问题影响全部 transition（R15 #1 修复不彻底） | 🟢 低 | **已修复 ✅** | 由 #1 大小写不敏感规则统一解决，风险 #74 标注"已由 §3.7 状态名字段约束统一解决" |
| 6 | force_publish_on_timeout 枚举值混合 boolean 和 string | 🟢 低 | **已修复 ✅** | §3.2 line 365: `mark_incomplete | publish_anyway | none`（纯字符串枚举）|

**结论：R16 全部 6 项已正确修复。** 风险清单 #70-#75 已同步新增。前 16 轮共 75 项问题全部关闭。

---

## 第二部分：R17 新发现关注点

文档 2141 行。R16 修复质量极高（6/6 全部到位无残留）。R17 聚焦在命名标准化、文档组织、运维维度和跨模块组合语义上——这是文档经过多轮修复后从"正确"向"优雅"演进过程中的自然层面。

---

### 🎯 R17 关注点 1：retry_backoff 命名和值格式在五处重试机制中不统一 (🟢 低)

**📐 场景：**
文档定义了五处独立的重试机制，使用了四种不同的字段命名方式和值格式：

| # | 机制 | 字段名 | 值格式 | 示例 | 位置 |
|---|------|--------|--------|------|------|
| A | WAL→内存队列重试 | *无命名（描述性文字）* | 隐式固定序列 | `100ms → 500ms → 2s` | §3.3 line 514 |
| B | Pipeline step 重试 | `backoff` | 单值 string enum | `"exponential"` | §3.5 line 614 |
| C | Compensation 重试 | `retry_backoff` | 单值 string enum | `"exponential"` | §3.5 line 636 |
| D | error_recovery 重试 | `retry_backoff` | enum + 参数 | `"immediate" \| "delay(Ns)" \| "exponential"` | §3.7 line 859 |
| E | Phase 0.5 Secret 重试 | `secret_retry_backoff` | CSV 逗号分隔 | `"2s,5s,15s"` | §2.5.1 line 71 |

**不一致清单：**

| 维度 | 说明 |
|------|------|
| 字段名 | A 无字段名（硬编码）；B 用 `backoff`；C/D 用 `retry_backoff`；E 用 `secret_retry_backoff`——三种命名风格 |
| 值格式 | B/C 单值 enum；D enum + 参数（`delay(15s)`）；E CSV 序列（`2s,5s,15s`）；A 硬编码不可配置 |
| "exponential" 含义 | 在 B/C 中指数退避是固定的（基数和倍率未定义）；在 D 中同样叫 `"exponential"` 但上下文不同 |
| A 的"指数退避" | 实际是 3 台阶固定序列（100ms, 500ms, 2s），不是真正的指数退避（2x 倍率应是 100ms, 200ms, 400ms...） |
| 自定义间隔 | D 支持 `delay(15s)`；E 支持 CSV 序列；A/B/C 不可自定义 |

**💥 可能后果：**
- 开发者要在五种语境中学习四种不同的重试配置方式——学习成本高，交叉引用时容易用错
- D 的 context 中 `retry_backoff: "15s"` 是"固定 15s 间隔"；而 C 的 `retry_backoff: "exponential"` 是"指数退避"——开发者从 D 移到 C 可能误以为 `"exponential"` 是唯一可用的值，从 C 移到 D 可能误以为 `"delay(15s)"` 在 C 中也有效
- A（WAL→memory）是唯一没有配置入口的机制——运维人员无法在 retry_queue 背压场景下调优重试间隔
- 如果组织想统一所有重试策略（如全部用 1s, 2s, 4s, 8s 退避），需要修改五处的语法

**🛠 建议：**
规范化为统一的 retry_backoff 接口。推荐方案：

**字段名统一：** 全局使用 `retry_backoff`（弃用 B 的 `backoff` 和 E 的 `secret_retry_backoff` 前缀变体）

**值格式统一：** 3 种标准化模式：

| 标准值 | 语义 | 替代 | 示例 |
|--------|------|------|------|
| `"exponential"` | 框架默认指数退避（base=100ms, factor=2, max=max_retries×base） | 替代 B/C 的 `"exponential"` |
| `"exponential:200ms:2:10s"` | 自定义指数退避（base, factor, max_delay） | 替代 D 的 `"delay(Ns)"` | `"exponential:1s:2:30s"` |
| `"fixed:5s"` | 固定间隔 | 替代 D 的 `"delay(5s)"` | `"fixed:5s"` |
| `"sequence:100ms,500ms,2s"` | 显式序列（按顺序各级间隔） | 替代 A 硬编码 + E 的 CSV | `"sequence:1s,2s,4s,8s"` |

**为 A 增加配置入口：** `wal_retry_backoff: "sequence:100ms,500ms,2s"`（可配置，默认值与当前硬编码一致）

**在 §10 设计决策中记录：** 新增"决策 10：重试退避标准化约定"记录此统一接口。

---

### 🎯 R17 关注点 2：§3.7 状态名 normalize 规则未在设计决策中记录 (🟢 低)

**📐 场景：**
R16 #1 的修复在 §3.7 line 793-799 的 Workflow 代码块注释中新增了 6 条状态名字段约束规则。这些规则是 Workflow 配置系统正确性的**关键架构约束**——没有它们，§9.1 YAML 全小写风格与 §3.7 核心大写状态定义之间的所有转移都会破裂。

但这些 6 条规则位于 Workflow 伪代码块的 // 注释内部，而不是在：

- §10 设计决策中
- §3.7 的正式规范段落（状态定义块之前）
- §9.1 YAML 配置前的使用说明中

**💥 可能后果：**
- 实现者通常从结构化的规范段落（§3.7 正文、§10 设计决策）阅读设计意图，而非代码块注释
- 如果实现者只读到了 `states: [PENDING, REVIEWING, ...]` 定义，未注意到 `// 状态名字段约束（⚠ 关键规则）：` 注释 → 使用大小写敏感比较 → 整个 Workflow 转移表静默破裂
- §9.1 YAML 配置示例没有任何注释告诉开发者"状态名大小写不敏感，你可以放心用小写"
- 对比：§10 中的设计决策"决策 5：同一来源事件保序""决策 9：优先级与同来源保序的权衡"等——都是跨模块的关键架构规则。状态名 normalize 的跨模块重要性（§3.7 + §9.1 + 所有用户 YAML 配置）与它们同等

**🛠 建议：**
至少执行以下之一（推荐同时执行）：

1. **在 §10 设计决策中新增"决策 10：状态名大小写不敏感"**：
   ```
   决策 10：状态名大小写不敏感
     状态名在运行时统一 normalize 为大写再比较。
     - YAML 配置允许使用小写以提高可读性（pending, reviewing, error）
     - 核心定义保持大写惯例（PENDING, REVIEWING, ERROR）
     - 框架确保运行时比较大小写不敏感
     - 配置校验阶段检测到大小写不一致时发出警告，但不拒绝启动
     - 此规则同样适用于转移表中的事件名（SUBMIT, APPROVE, CANCEL, RETRY, ERROR）
   ```
2. 将 §3.7 的 6 条规则从代码注释提升为规范级别的 bullet list，放在 `states: [...]` 块之前
3. 在 §9.1 workflow 配置示例前加注释行：
   ```
   # 状态名和事件名大小写不敏感，框架运行时统一 normalize。
   # 可使用小写（pending, submit）提高 YAML 可读性。
   ```

---

### 🎯 R17 关注点 3：retry_queue_depth 不在核心 /metrics 列表中 (🟢 低)

**📐 场景：**
§9.3 line 1817-1824 定义的核心指标列表包含 8 项：

```
event_bus_queue_depth   # 当前队列深度（按优先级分）
event_throughput_total  # 总计事件吞吐（count/s）
backpressure_level      # 当前背压级别（0-5）
events_discarded_total  # 丢弃事件累计计数
inflight_pipelines      # 当前运行中 Pipeline 数量
inflight_skills         # 当前运行中 Skill 实例数
plugin_health           # 按插件名的健康状态（1/0）
dlq_depth               # 死信队列深度
```

缺失：**`retry_queue_depth`**（待重试队列当前深度）

**💥 可能后果：**
待重试队列（retry_queue_max: 1000）的满状态是 WAL checkpoint 阻塞的**三级联锁**关键环节（§3.3 line 515-522）：

```
形成三级联锁：主队列满 → 待重试队列满 → WAL 写入阻塞
```

当待重试队列接近上限时：
- WAL checkpoint 停滞 → WAL 段无限累积 → 磁盘写满 → 系统崩溃
- 没有 `retry_queue_depth` 指标 → 操作员看不到"待重试队列正在接近极限（如 950/1000）"的预警信号
- 只能从 WAL 段大小的异常增长（间接指标）推断问题，但那时已来不及
- `dlq_depth` 有、`event_bus_queue_depth` 有，但 `retry_queue_depth` 没有——**三条联锁链路中两条可观测，被遗漏的恰是中间那条（WAL→内存的瓶颈点）**

**🛠 建议：**
在 §9.3 核心指标列表中新增：
```
retry_queue_depth            # 待重试队列当前深度
                             # 接近 retry_queue_max（默认 1000）时预警
                             # 此队列满会阻塞 WAL checkpoint 推进
```
同时建议：为 `retry_queue_depth` 在背压 Level 2-3 的触发条件中增加告警阈值，让待重试队列在 80% 满时就发出预警。

---

### 🎯 R17 关注点 4：Pipeline 补偿与 Workflow ERROR 状态组合交互未定义 (🟢 低)

📐 **场景：**
文档定义了两种独立的错误处理机制：

| 机制 | 适用 | 失败后 | 恢复路径 |
|------|------|--------|---------|
| **Pipeline 补偿**（§3.5） | Pipeline 步骤失败 | reverse_order 执行 compensate | COMPENSATION_FAILED → 人工接管 |
| **Workflow ERROR 状态**（§3.7） | Workflow 转移失败 | 进入 ERROR 状态，可 RETRY 恢复 | RETRY → last_active_state |

如果 **Pipeline 作为 Workflow 状态转移的 action**（一种自然的组合模式）——例如审批流程中通过 Pipeline 执行"发邮件通知→写入审计表→更新外部 ERP"：

```
状态 A ──→ [event] ──→ 状态 B
                          (action: Pipeline "invoice-processor")
                                   ↓
                          Pipeline step 2 失败
                                   ↓
                          Pipeline 触发补偿
                                   ↓
                    ┌──── 补偿成功 ────┐
                    │                  │
             状态 B 已有数据但被回滚     │
             但 Workflow 已到状态 B     │
                                  补偿失败
                                      │
                              COMPENSATION_FAILED
                              + Workflow 停留在状态 B
                              = 半回滚 + 错误状态
```

**具体未定义的组合变体：**

| # | 组合场景 | 未定义的问题 |
|---|---------|-------------|
| 4a | Pipeline 作为 action 失败，补偿成功回滚所有副作用 | Workflow 已在目标状态 B，但 B 的数据被回滚→状态 B 无有效数据。Workflow 应该回退到前一个状态吗？ |
| 4b | Pipeline 作为 action 失败，补偿失败（COMPENSATION_FAILED） | 状态 B 已有部分数据 + 部分回滚 + COMPENSATION_FAILED → Workflow 应该进入 ERROR 吗？还是留在状态 B？ |
| 4c | Workflow 从 ERROR RETRY 恢复后重新执行同一个 Pipeline | Pipeline 的幂等性是否足够？之前补偿回滚 + 现在重试 = 业务操作的原子性如何保证？ |
| 4d | Pipeline 作为 action 正在执行时收到 Workflow CANCEL 事件 | Pipeline 中断 + 补偿触发 vs Workflow 直接转移到 CANCELLED——谁先执行？ |

**💥 可能后果：**
- 开发者分别遵循 Pipeline 补偿规则和 Workflow ERROR 规则各自正确，但组合后行为不可预测
- 最严重场景：金融审批流程中 Pipeline "扣款+发通知+写日志"，step 1 成功（已扣款），step 2 失败 → 补偿执行回滚（退款成功）。但 Workflow 已经走到 APPROVED 并通知用户"审批通过"——用户看到"审批通过"但未实际生效
- COMPENSATION_FAILED + Workflow 在状态 B = "数据处于半回滚不一致状态，但系统无告警"
- 操作员从 Workflow 角度看到"在状态 B"，从 Pipeline 角度看到"已补偿"，无法确定业务数据的真实状态

**🛠 建议：**
在 §3.7 Workflow 的转移表或 §3.5 Pipeline 的组合约束段新增以下定义：

**规则 1：Pipeline 作为 action 失败时，Workflow 默认进入 ERROR 状态**
```
{ from: A, event: E, to: B,
  action: Pipeline "processor",
  on_action_failure: ERROR,       // Pipeline 失败 → Workflow 进入 ERROR
  on_compensation_failure: EMERGENCY  // 补偿失败 → 更高告警级别
}
```
补偿成功 ≠ Pipeline 成功——补偿是回滚操作，Pipeline 依然失败，不应继续业务流程。

**规则 2：补偿失败的 Workflow 实例应有额外标记**
```
Pipeline 补偿进入 COMPENSATION_FAILED + Workflow 在 ERROR 状态
  → Workflow 实例标记为 partial_rollback: true
  → 告警级别高于普通 ERROR（⚠ 数据可能处于半回滚状态）
  → 人工恢复时优先检查此标记
```

**规则 3：CANCEL 与 inflight Pipeline 的交互**
```
如果在 Pipeline 作为 action 正在执行时收到 Workflow 的 CANCEL 事件：
  → 等待 Pipeline 完成（或补偿完成）后再执行 CANCEL 转移
  → CANCEL 不中断正在执行的 Pipeline/补偿（与 §2.5.3 排水阶段语义一致）
```

**规则 4：建议在 §11 风险清单新增组合模式风险条目**
```
#76 Pipeline 补偿与 Workflow ERROR 组合交互未定义：
  Pipeline 作为 Workflow action 失败时，两套错误处理机制各自独立运行，
  组合后可能出现"补偿已回滚但 Workflow 已转移到目标状态"的不一致。
```

---

## 审计总结

**R16 修复验证：**
```
R16 共 6 项：全部已修复 ✅
前 16 轮共 75 项：全部关闭
```

**R17 新发现：4 项**

| # | 关注点 | 等级 | 维度 | 来源 |
|---|-------|------|------|------|
| 1 | retry_backoff 命名和值格式在五处机制中不统一（4 种不同语义） | 🟢 低 | 命名标准化/配置一致性 | 跨轮次累积 |
| 2 | §3.7 状态名 normalize 规则未在设计决策中记录 | 🟢 低 | 文档组织 | R16 #1 修复关联 |
| 3 | retry_queue_depth 不在核心 /metrics 列表中 | 🟢 低 | 可观测性盲区 | 运维维度 |
| 4 | Pipeline 补偿与 Workflow ERROR 状态组合交互未定义 | 🟢 低 | 跨模块组合语义 | 架构组合维度 |

**按影响维度分类：**

| 维度 | R17 新发现 | 说明 |
|------|-----------|------|
| 命名标准化 | #1 | 五处重试机制用 `backoff` / `retry_backoff` / `secret_retry_backoff` + 四种值格式 |
| 文档组织 | #2 | 关键架构约束（大小写 normalize）藏在代码块 // 注释中，设计决策中无记录 |
| 可观测性 | #3 | 三条联锁链路中 retry_queue 是唯一无指标的一环 |
| 跨模块组合 | #4 | Pipeline 补偿 + Workflow ERROR 状态是最自然的组合模式但行为未定义 |

**趋势线：**
```
R8→R9→R10→R11→R12→R13→R14→R15→R16→R17 (本轮)
 3 →  3 →  2 →  2 →  2 →  2 →  7 →  6 →  6 →  4
```

单轮 4 项，持续下降。文档经过 17 轮审计、75 项问题修复后，浅层和明显的逻辑缺陷已基本清零。R17 的 4 项均已进入"优雅化"层面——命名标准化（#1）、文档组织优化（#2）、运维完备性（#3）和组合语义定义（#4）。🟡 中风险已连续三轮为零。新发现项的严重性在降低，说明文档已接近收敛。

建议新增风险条目 #76-#79。
