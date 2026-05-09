# Agent Design Review (R15) — 第十五次审计报告

> 审计人：Business Logic Auditor
> 审计对象：`agent-design.md` (2053 行) — 事件响应式 Agent 框架设计
> 审计日期：2026-05-06
> 前置审计：`agent-design-r1.md` ~ `agent-design-r14.md`

---

## 第一部分：R14 问题修复验证

| # | R14 关注点 | 等级 | 修复状态 | 证据位置 |
|---|-----------|------|---------|---------|
| A | §3.2 参数表 check_open_files 示例值与默认值 auto 不一致 | 🟢 低 | **已修复 ✅** | §3.2 line 347: `check_open_files: auto` |
| 1 | POST /agent/start 无超时建议，与 shutdown 不对称 | 🟢 低 | **已修复 ✅** | §9.3 line 1764-1765: 已补充超时建议及 workflow_recovery_timeout 参考 |
| 2 | Secret 解析在启动序列中位置未锚定 | 🟢 低 | **已修复 ✅** | §2.5.1 line 67-70: 新增 Phase 0.5 密钥解析阶段 |
| 3 | Cron 静态配置与运行时修改的合并语义未定义 | 🟢 低 | **已修复 ✅** | §6.4.1 line 1323-1357: override 独立存储层 + 按 id 合并规则 |
| 4 | Physical isolation 模式下清理语义缺失 | 🟢 低 | **已修复 ✅** | §5.2 line 1136-1156: cleanup_policy (retain/delete_on_disable/delete_on_uninstall) + PII 强制规则 |
| 5 | Metrics 格式未定义，可观测性互操作性受限 | 🟢 低 | **已修复 ✅** | §9.3 line 1784-1794: Prometheus exposition format + 8 项核心指标列表 |
| 6 | Chat 类事件源的 LLM 注入无防护 | 🟡 中 | **已修复 ✅** | §9.3 line 1813-1834: 信任等级 (trusted/untrusted/sandboxed) + 4 项加固措施 + 审计日志 |
| 7 | Phase 3 Workflow 恢复无超时，与 Phase 2 不对称 | 🟢 低 | **已修复 ✅** | §2.5.1 line 77-80: workflow_recovery_timeout: 120s + 超时后 checkpoint 提交 + 进度日志 |

**结论：R14 的 7 项问题全部被正确修复。** 前 14 轮共 63 项问题全部关闭。风险清单 #58-#63 已同步新增。

---

## 第二部分：R15 新发现关注点

文档 2053 行，前 14 轮覆盖了 63 项风险。R15 聚焦在跨模块集成边界、配置一致性和 R14 修复引入的新接缝。

---

### 🎯 R15 关注点 1：§9.1 YAML 中 workflow transition 事件名 ERROR_EVENT 与核心定义 ERROR 不一致 (🟡 中)

**📐 场景：**
§3.7 核心转移表（line 861）定义：
```
{ from: ANY, event: ERROR, to: ERROR }
```
事件名是 `ERROR`，目标状态名为大写 `ERROR`。

但 §9.1 YAML 配置示例（line 1696）：
```
{ from: ANY, event: ERROR_EVENT, to: error }
```
事件名使用 `ERROR_EVENT`，目标状态 `to: error`（小写）。两者均与核心定义不一致。

**💥 可能后果：**
开发者从 §9.1 YAML 拷贝配置部署。外部系统或框架内部发送 `ERROR` 事件时——YAML 中定义的是 `ERROR_EVENT` transition——转移表匹配失败。Workflow 遇到错误时永远不会进入 ERROR 状态：

- guard 检查 `total_retry_count < max_retry_count`（在 ERROR 状态上定义）永不触发
- `ERROR→ARCHIVED` timeout 永不开始计时
- RETRY 恢复路径（仅 ERROR 状态有定义）不可达
- 实例卡在当前状态永久等待，或进入未定义行为

事件名和目标状态名双重不一致，是典型的"拷贝后未同步"故障模式。

**🛠 建议：**
将 §9.1 line 1696 改为与 §3.7 核心定义完全对齐：
```yaml
{ from: ANY, event: ERROR, to: ERROR }
```
同时建议在框架启动时做配置校验——校验 §9.1 YAML 中的 transition 事件名是否在 Workflow 定义中有对应处理——可以类似 Phase 2 的超时（`plugin_load_timeout`）机制，作为启动时瞬态校验。

---

### 🎯 R15 关注点 2：GET /audit-log 无访问控制与查询参数 (🟡 中)

**📐 场景：**
§9.3 line 1804 定义：
```
GET /audit-log                # 审计日志（配置变更、权限操作、事件丢弃）
```
无过滤、无分页、无访问控制说明。

对比同节其他端点：`/metrics` 有详细指标定义（line 1784-1794），`POST /inject-event` 有 `force_enable_debug_endpoints` 生产和保护（line 1811），敏感操作（shutdown/disable plugin）有二次确认要求（line 1810）。

各节散落的审计日志包含内容：
- Secret 轮换记录受影响的密钥名、新旧指纹哈希（§9.2 line 1731-1735）
- 操作员身份和操作（控制接口安全守卫 line 1810）
- 事件丢弃记录（§3.3 line 410, 427）
- DLQ 操作记录
- Plugin disable/unload 操作

**💥 可能后果：**
- 无分页：数十万条审计日志一次性返回 → OOM / HTTP 500
- 无过滤：操作员无法按时间、操作类型、操作员身份筛选
- 审计日志包含 Secret 指纹哈希（非明文但可暴力比对）→ 暴露密钥轮换时间线
- 同节的敏感操作二次确认要求是为了防护关键操作，但操作后的审计日志却可能被任意读取——防护链在日志环节断裂
- 如果控制接口暴露到网络（即使有 auth），攻击者通过 audit-log 可获取系统配置变更历史、操作员IP/时间窗口——侦查攻击面

**🛠 建议：**
在 §9.3 的 `GET /audit-log` 注释中补充：

1. 分页参数（`&offset=&limit=` 或 `&cursor=&page_size=`）
2. 过滤参数（`&since=&until=`, `&type=config_change|permission|discard`, `&operator=`）
3. 访问约束：audit-log 默认需要比普通控制接口更高的权限（只读审计员 vs 操作员）
4. 纳入 `force_enable_debug_endpoints` 保护范围？至少明确生产环境的默认访问策略
5. 建议：审计日志的最小可读单元是一行/一条，不暴露批量全量导出接口无限制使用

---

### 🎯 R15 关注点 3：POST /dlq/{id}/retry 重试语义未定义 (🟢 低)

**📐 场景：**
§9.3 line 1801-1802 定义：
```
POST /dlq/{id}/retry           # 手动重试死信事件
POST /dlq/{id}/discard         # 确认丢弃死信事件
```
没有定义重试后的行为路径。

**关键未定义问题：**

| 问题 | 场景 | 影响 |
|------|------|------|
| 重试再次失败的路径 | 手动 retry → 再次失败 → 重新入 DLQ？还是永久标记 unrecoverable？ | 如果重新入 DLQ，TTL 重置 → 同一事件反复触发到期前告警 |
| 重试计数器语义 | 手动 retry 重置计数器？还是与原始 max_retries 累计？ | 如果累计且超限后静默丢弃 → 操作员以为已重试 |
| TTL 重置行为 | 重新入 DLQ 后 TTL（dlq_ttl_days: 30）重置还是继续？ | TTL 重置 → pre_expiry_alert_days 重新生效 → 告警风暴 |

**💥 后果：**
操作员手动重试一个进入 DLQ 的事件，再次失败后重新入 DLQ：
- `dlq_ttl_days: 30` 重新计时 → 30 天后再过期
- `pre_expiry_alert_days: [7, 3, 1]` 重新触发 → 到期前 7 天/3 天/1 天再次告警
- 如果该事件业务流程已变更，操作员每次手动重试都产生 30 天的残留告警噪声
- 对比：如果手动 retry 被框架拦截（计数器累计超限），操作员无法执行"问题已修复，让我重试一次"

**🛠 建议：**
在 §9.3 或 §3.5 DLQ 段补充：

1. 手动重试的计数器语义定义：
   - 建议：手动 retry 视为管理员介入，**重置** retry_count（但保留原始计数审计字段 `original_retry_count`）
   - 这样同一问题的多次手动重试不会累积到上限
2. 再次失败路径：
   - 建议：再次失败**重新入 DLQ**，但 `retry_count` 从重置后的值累计
   - 可配置：`dlq.manual_retry_reset_counters: true | false`（默认 true）
3. TTL 行为：
   - 建议：重新入 DLQ 后**TTL 重置**，但加上 `max_manual_retries` 全局上限（默认 5 次手动重试）防止无限循环
4. 手动 retry 后 `dlq_storage` 中保留事件的操作历史（谁在何时重试了）

---

### 🎯 R15 关注点 4：Phase 0.5 Secret Store 不可用无重试/等待策略 (🟢 低)

**📐 场景：**
§2.5.1 line 69 定义了 Phase 0.5 失败的唯一路径：
```
⚠ Secret Store 不可用 → Agent 拒绝启动（不泄露哪个变量缺失）
```
没有任何重试机制或等待策略。

对比其他 Phase 的容错设计：

| Phase | 容错机制 |
|-------|---------|
| Phase 1 | 缓冲区满暂停 + 断点记录 + 下次继续 |
| Phase 2 | plugin_load_timeout: 30s + 紧急告警 |
| Phase 3 | workflow_recovery_timeout: 120s + 已恢复/未恢复分流 |
| Phase 4 | 源激活失败可降级 |
| **Phase 0.5** | **硬终止，无重试** |

**💥 后果：**
Secret Store（Vault / AWS Secrets Manager）因短暂网络抖动或维护窗口 503：
- Agent 启动 → Phase 0.5 失败 → 拒绝启动 → 进程退出
- 编排器（K8s/Docker Compose/Supervisor）检测到进程退出 → 自动重启
- 重启后 Phase 0.5 再次失败 → **无限重启循环**
- 实际 Secret Store 可能在 5-30 秒内恢复，但 Agent 没有任何等待机制

此外生产环境中的 Vault 通常有优雅降级策略（缓存代理、Read-through cache），但 Phase 0.5 的硬终止设计不与这些降级策略配合。

**🛠 建议：**
在 §2.5.1 Phase 0.5 中补充失败重试策略：

1. 可配置的重试次数和间隔：
   ```
   secret_retry_count: 3           # 默认重试 3 次
   secret_retry_backoff: "2s,5s,15s"  # 默认退避间隔（指数递增）
   ```
2. 重试期间输出进度日志：
   ```
   "Secret Store 不可用，正在重试 (1/3)..."（含已恢复/未恢复的密钥数）
   ```
3. 所有重试用尽后才进入"拒绝启动"路径
4. 可选：如果 Secret Store 有缓存代理（如 Vault Agent Sidecar），允许配置读本地缓存作为临时降级
5. 重试间隔建议不要写死到 Phase 0.5 的定义中——应该在 §9.1 配置中可设置

---

### 🎯 R15 关注点 5：ERROR 状态同时存在 RETRY 和 CANCEL 两条出口路径的优先级未定义 (🟢 低)

**📐 场景：**
§3.7 转移表（line 855-864）定义了两条从 ERROR 可用的路径：
```
{ from: ANY,       event: CANCEL, to: CANCELLED },
{ from: ERROR,     event: RETRY,  to: :last_active_state, guard: total_retry_count < max_retry_count, on_fail: ARCHIVED },
```
CANCEL 的 `from: ANY` 覆盖 ERROR。所以 ERROR 状态有两条并发合法出口：
- **RETRY** → 恢复到进入 ERROR 前的状态，继续业务流程
- **CANCEL** → 直接放弃实例，进入 CANCELLED 终态

**💥 后果：**
如果两个事件同时到达或时间窗口紧密（如操作员同时点了"重试"和"取消"）：

- **RETRY 先处理** → 状态变为 last_active_state（如 REVIEWING）→ CANCEL 事件到达时状态不匹配（非 ERROR）→ CANCEL 的 from: ANY 触发 → 刚恢复的实例立刻被取消

- **CANCEL 先处理** → 进入 CANCELLED 终态 → RETRY 事件到达时状态不匹配 → RETRY 的转移表找不到 ERROR 状态 → 事件静默丢弃或进入 DLQ

- 操作员意图通常是"先试恢复，不行再取消"，但无优先级规则时恢复和取消之间的竞态条件决定最后结果

**🛠 建议：**
在 §3.7 ERROR 恢复配置中补充：

1. 明确定义 ERROR 状态下 RETRY 与 CANCEL 的优先级规则：
   - 建议：**RETRY 优先于 CANCEL**（恢复比放弃更安全）
   - 或引入时序规则：按事件到达顺序，谁先谁后
2. 约束建议：ERROR 状态的 CANCEL 事件附加 guard `no_pending_retry`（无待处理的 RETRY 事件时才允许 CANCEL）
3. 在 §11 风险清单新增条目记录此竞态

---

### 🎯 R15 关注点 6：§9.3 POST /cron/add/update/remove 不在 §6.4 安全守卫的鉴权范围内 (🟢 低)

**📐 场景：**
§9.3 line 1779-1781 定义：
```
POST /cron/add                 # 添加定时任务
POST /cron/{id}/update         # 更新定时任务
POST /cron/{id}/remove         # 删除定时任务
```
无额外安全约束注释。

§6.4 安全守卫（line 1317-1321）定义 `reconfigure` 的权限模型：
```
reconfigure 操作的权限模型：
  1. reconfigure 不是无防护的 API 端点
  2. 内部事件 SOURCE: "cron-manager", TYPE: "CONFIG_CHANGE" 才允许修改
  3. 所有 reconfigure 操作记录审计日志
  4. 动态频率变更受速率限制
```

但 `reconfigure` 对应的是 §9.3 的 `PUT /event-source/{id}/config`，而非 `POST /cron/add/update/remove`。两者是不同的 API 入口：

| API | 功能 | 安全守卫 |
|-----|------|---------|
| PUT /event-source/{id}/config | 重配置 EventSource（含 TimerSource 间隔） | ✅ §6.4 完整权限模型 |
| POST /cron/add/update/remove | 增删改 cron jobs | ❌ 无专用安全守卫 |

**💥 后果：**
1. 攻击者通过 `/cron/add` 添加 `min_interval: 1` 的 cron job → 每秒 1 个 CRON_TICK → 绕过了 `reconfigure` 的速率限制（每分钟最多变更 1 次），但直接添加新 job 的频次不受限
2. 攻击者添加 100 个高频 cron jobs → 即使每个受 `min_interval: "1s"` 约束，100 × 1/s = 100 CRON_TICK/s → 达到 `rate_limit: 100` 的上限，挤占正常 cron jobs 的执行机会
3. `/cron/add/update/remove` 操作无审计日志要求（line 1320 明确要求审计日志，但只对 reconfigure）
4. `/cron/add` 添加的 job 无 `min_interval` 校验保证——开发者可能最小间隔设 100ms

**🛠 建议：**
在 §9.3 的 `/cron/add` 和 `/cron/update` 注释中补充：

1. 添加/更新 cron job 同样受 §6.4 安全守卫约束：
   - `min_interval: "1s"` 硬编码 clamp
   - `rate_limit: 100` 全局约束
   - 所有变更记录审计日志
2. 新 API 端点（`/cron/add/update/remove`）也应纳入 `reconfigure` 的权限模型——或至少将 §6.4 的安全守卫范围从"reconfigure"扩大为"所有运行时 cron 变更操作"
3. 明确 `/cron/add` 新增的 job 也受 `min_interval` 守卫拦截，不得绕过

---

## 审计总结

**R14 修复验证：**
```
R14 共 7 项（含 1 项残留）：全部已修复 ✅
前 14 轮共 63 项：全部关闭
```

**R15 新发现：6 项**

| # | 关注点 | 等级 | 维度 | 影响模块 |
|---|-------|------|------|---------|
| 1 | YAML 配置 ERROR_EVENT 与核心定义 ERROR 不一致 | 🟡 中 | 配置一致性 | §3.7 ↔ §9.1 |
| 2 | GET /audit-log 无访问控制与查询参数 | 🟡 中 | 安全边界 | §9.3 |
| 3 | POST /dlq/{id}/retry 重试语义未定义 | 🟢 低 | 运行时语义 | §3.5 DLQ ↔ §9.3 |
| 4 | Phase 0.5 Secret 失败无重试策略 | 🟢 低 | 启动序列完整性 | §2.5.1 |
| 5 | ERROR 状态 RETRY vs CANCEL 优先级未定义 | 🟢 低 | 状态机竞态 | §3.7 |
| 6 | /cron/add/update/remove 不在 reconfigure 鉴权范围内 | 🟢 低 | 安全守卫完整性 | §6.4 ↔ §9.3 |

**按影响维度分类：**

| 维度 | R15 新发现 | 说明 |
|------|-----------|------|
| 配置一致性 | #1 | YAML 示例与核心定义的事件名不一致（🟡 中） |
| 安全边界 | #2, #6 | audit-log 无访问控制；cron API 绕过 reconfigure 鉴权 |
| 运行时语义 | #3 | DLQ 手动重试路径未定义，操作员已知行为不确定 |
| 启动序列 | #4 | Phase 0.5 硬终止无重试，与编排器无限重启循环耦合 |
| 状态机竞态 | #5 | ERROR 双出口路径优先级未定义 |

**趋势线：**
```
R8→R9→R10→R11→R12→R13→R14→R15 (本轮)
 3 →  3 →  2 →  2 →  2 →  2 →  7 →  6
```

单轮新发现 6 项，保持在高位。主要原因是文档扩展至 2053 行后，**跨模块的对齐问题**（§3.7 核心定义与 §9.1 YAML 配置不一致）、**全局安全边界的遗漏**（audit-log 访问控制、cron API 鉴权遗漏）、以及**R14 修复引入的新接缝**（Phase 0.5 有位置但无重试策略）开始冒出。🟡 中风险 2 项（#1 配置一致性、#2 安全边界），建议优先处理。

建议新增风险条目 #64-#69（共 6 项）。
