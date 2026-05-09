# Agent Design Review (R16) — 第十六次审计报告

> 审计人：Business Logic Auditor
> 审计对象：`agent-design.md` (2113 行) — 事件响应式 Agent 框架设计
> 审计日期：2026-05-06
> 前置审计：`agent-design-r1.md` ~ `agent-design-r15.md`

---

## 第一部分：R15 问题修复验证

| # | R15 关注点 | 等级 | 修复状态 | 证据位置 |
|---|-----------|------|---------|---------|
| 1 | YAML 配置 ERROR_EVENT 与核心定义 ERROR 不一致 | 🟡 中 | **已修复 ✅** | §9.1 line 1725: `event: ERROR`（已删除 `_EVENT` 后缀） |
| 2 | GET /audit-log 无访问控制与查询参数 | 🟡 中 | **已修复 ✅** | §9.3 line 1846-1858: 游标分页 + type/time/operator 过滤 + 独立审计员权限 + fingerprint 时间戳替代 |
| 3 | POST /dlq/{id}/retry 重试语义未定义 | 🟢 低 | **已修复 ✅** | §9.3 line 1833-1842: 计数器重置 + original_retry_count 审计字段 + TTL 重置 + max_manual_retries: 5 + 操作历史 |
| 4 | Phase 0.5 Secret Store 失败无重试策略 | 🟢 低 | **已修复 ✅** | §2.5.1 line 69-76: secret_retry_count: 3 + 指数退避 + 进度日志 + 缓存降级 |
| 5 | ERROR 状态 RETRY vs CANCEL 优先级未定义 | 🟢 低 | **已修复 ✅** | §3.7 line 873-893: 隐式 guard + retry_cancel_conflict_defer_ms: 5000 + 时序规则 |
| 6 | /cron/add/update/remove 不在 reconfigure 鉴权范围内 | 🟢 低 | **已修复 ✅** | §9.3 line 1808-1812: 标注 "⚠ 受 §6.4 安全守卫约束" |

**结论：R15 全部 6 项已正确修复。** 风险清单 #64-#69 已同步新增。前 15 轮共 69 项问题全部关闭。

---

## 第二部分：R16 新发现关注点

文档 2113 行。R15 修复质量较高（6/6 正确修复），但修复本身引入了新的安全面和残留不一致。R16 聚焦在这些 R15 修复的完整性上。

---

### 🎯 R16 关注点 1：§3.7 大写状态名与 §9.1 小写状态名不一致，且无大小写敏感性声明 (🟡 中)

**📐 场景：**
§3.7 核心状态定义（line 769-775）全大写：
```
states: [ PENDING, REVIEWING, APPROVED, REJECTED, CANCELLED, ERROR, ARCHIVED ]
```

§9.1 YAML 配置示例（line 1697-1704）全小写：
```yaml
states:
  - pending
  - reviewing
  - approved
  - rejected
  - cancelled
  - error
  - archived
```

转移表 line 1725：`{ from: ANY, event: ERROR, to: error }`（事件名大写 `ERROR`，状态名小写 `error`）

并且**所有**转移表条目（line 1721-1726）都使用小写状态名：
```
{ from: pending,   event: SUBMIT,   to: reviewing, ... }
{ from: reviewing, event: APPROVE,  to: approved }
{ from: reviewing, event: REJECT,   to: rejected }
{ from: ANY,       event: CANCEL,   to: cancelled }
{ from: ANY,       event: ERROR,    to: error }
{ from: error,     event: RETRY,    to: :last_active_state, ... }
```

文档没有在任何位置声明状态名字段是大小写不敏感的，也没有强制大小写统一。

**💥 可能后果：**
如果框架实现中使用大小写敏感的比较（大多数生产系统的默认行为，如 Rust `==`、Java `equals()`、C++ `==`）：

- 配置文件中 `from: ANY, event: ERROR, to: error` 尝试转移到 `error`，但 Workflow 定义中的状态名为大写 `ERROR` → 转移失败（框架返回 "目标状态 error 未定义"）
- 不光是 ERROR——**ALL 状态名都受此影响**：`PENDING` vs `pending`、`APPROVED` vs `approved`
- 转移表 line 1721-1726 中，所有状态名 (`pending`, `reviewing`, `approved`, `rejected`, `cancelled`, `error`, `archived`) 都无法匹配核心定义中的大写状态名 → **整个 Workflow 转移表静默失效**
- 这比 R15 #1 的 `ERROR_EVENT` 问题**影响面更广**——R15 #1 只影响了一个事件名，这里是所有状态名不匹配

如果实现用大小写不敏感比较（如 PostgreSQL citext, MySQL utf8mb4_unicode_ci, Rust 的 `eq_ignore_ascii_case`）：不出现问题。但文档没说这是设计决策。

**🛠 建议：**
在 §3.7 状态定义节或设计决策中新增一条关于状态名大小写的规则。选择以下方案之一：

**方案 A（推荐）：显式声明大小写不敏感 + normalize 规则**
```
状态名约束：
  - 状态名在核心定义和配置中统一使用大写惯例
  - 运行时比较时大小写不敏感（由框架 normalize 为小写或大写再比较）
  - 配置校验阶段检测到大小写不一致时发出警告
```
此方案保留了 YAML 小写的可读性和 §3.7 大写的可读性，框架层做 bridge。

**方案 B：强制统一大小写**
将 §9.1 YAML 状态名改为大写：
```yaml
states:
  - PENDING
  - REVIEWING
  ...
```
以及对应转移表：
```yaml
- { from: PENDING,   event: SUBMIT,   to: REVIEWING, ... }
- { from: ANY,       event: ERROR,    to: ERROR }
```

---

### 🎯 R16 关注点 2：§6.4 reconfigure 权限模型未同步更新 `/cron/add/update/remove` 跨引用（残留不一致）(🟢 低)

**📐 场景：**
R15 #6 的修复只在 §9.3 标注了 "⚠ 受 §6.4 安全守卫约束"，但 §6.4 的安全守卫定义（line 1346-1350）**没有更新**。

**修复后的 §9.3（line 1808-1811）：**
```
POST /cron/add              # 添加定时任务
                            #   ⚠ 受 §6.4 安全守卫约束（min_interval 硬编码 clamp、rate_limit 全局限制、审计日志）
POST /cron/{id}/update      # 更新定时任务
                            #   ⚠ 同 /cron/add，受 §6.4 安全守卫约束
```

**未更新的 §6.4（line 1346-1350）：**
```
reconfigure 操作的权限模型：
  1. reconfigure 不是无防护的 API 端点
  2. 内部事件 SOURCE: "cron-manager", TYPE: "CONFIG_CHANGE" 才允许修改
  3. 所有 reconfigure 操作记录审计日志（old_interval, new_interval, caller, timestamp）
  4. 动态频率变更受速率限制：每个 cron job 每分钟最多变更 1 次
```
仍然只提 `reconfigure`（对应 `PUT /event-source/{id}/config`），没有提到 `POST /cron/add/update/remove`。

**💥 可能后果：**
- 读 §6.4 的开发者认为 cron 安全守卫只覆盖 `reconfigure`（PUT 端点），不知 §9.3 的 POST /cron/add 也受约束
- §9.3 和 §6.4 之间的交叉引用是**单向的**——§9.3 引用 §6.4 但 §6.4 不知道 §9.3 的存在
- 实现者如果按 §6.4 实现安全守卫，可能只实现了 PUT /event-source/{id}/config 的保护，遗漏了 POST /cron/add/update/remove
- 本质上 R15 #6 修复了"文档的外观"（改了 §9.3 注释），但没修复解释"为什么是这样"的 §6.4 定义

**🛠 建议：**
将 §6.4 的权限模型标题和范围扩大：

```diff
- reconfigure 操作的权限模型：
+ 运行时 cron 变更操作的权限模型（覆盖 PUT /event-source/{id}/config + POST /cron/add/update/remove）：
   1. ...
   2. 内部事件 SOURCE: "cron-manager", TYPE: "CONFIG_CHANGE" 才允许修改
   3. 所有运行时变更操作（含 POST /cron/add/update/remove）记录审计日志
   4. 动态频率变更受速率限制
```

---

### 🎯 R16 关注点 3：状态图未反映 ERROR→CANCEL 路径 (🟢 低)

**📐 场景：**
转移表 line 1724：`{ from: ANY, event: CANCEL, to: cancelled }` → CANCEL 是 ERROR 状态的合法出口。

R16 #5 新增的 ERROR 双出口优先级规则（line 873-893）详细定义了 CANCEL 在 ERROR 状态上的延迟 guard 机制。

但状态图（line 926-934）只显示了 ERROR 的两条路径：
```
任何状态的 ERROR 事件 ──→ ERROR
                            │
                      RETRY │ (total_retry_count < 3)
                            ▼
                    last_active_state
                            │
                      RETRY 第4次失败 ──→ ARCHIVED
                            │
                      (7天无操作自动) ──→ ARCHIVED
```

**CANCEL 从 ERROR 出去的箭头不存在。**

**💥 可能后果：**
- 状态图是开发者可视化理解 Workflow 行为的首要入口。图中没有 ERROR→CANCEL → 开发者以为 CANCEL 在 ERROR 状态上不合法
- 与转移表行为矛盾：CANCEL 在 ERROR 上合法且定义了复杂的 guard 规则，但状态图不画
- 新增的延迟 guard 规则（line 873-893）与状态图脱节——图中没有对应标注或注释

**🛠 建议：**
在状态图的 ERROR 框下增加 CANCEL 路径：

```
任何状态的 ERROR 事件 ──→ ERROR
                            │
                      RETRY │ (total_retry_count < 3)
                            ▼
                    last_active_state
                            │
                      RETRY 第4次失败 ──→ ARCHIVED
                            │
                      (7天无操作自动) ──→ ARCHIVED
                            │
                      CANCEL ──→ CANCELLED   ← 新增
                      (隐式 guard: 见 §3.7 双出口规则)
```

或在状态图上方加注释：
```
注：ERROR 状态还支持 CANCEL 转移（见 §3.7 双出口优先级规则），从状态图简洁性出发未画出完整路径。
```

---

### 🎯 R16 关注点 4：Phase 0.5 secret_cache_fallback 本地缓存安全性未定义 (🟢 低)

**📐 场景：**
§2.5.1 line 74-76（R15 #4 修复新增）：
```
可选降级：若配置了缓存代理（如 Vault Agent Sidecar），
允许 secret_cache_fallback: true 读取本地缓存作为临时降级
```

文档没有定义以下安全属性：

| 维度 | 问题 | 风险等级 |
|------|------|---------|
| 存储加密 | 缓存文件是否加密？明文？AES-GCM？ | 如果明文 → 攻击者通过文件系统漏洞读取所有 Secret |
| 文件权限 | 默认权限 600/640/644？ | 如果 644 → 其他进程可读 |
| 缓存 TTL | 缓存生存期？与 Secret Store 的 token TTL 对齐？ | 过期缓存导致认证失败 |
| 进程 dump | Agent 崩溃时 core dump 是否包含缓存 Secret？ | 诊断过程中泄露 |
| Phase 1 可用性 | WAL 恢复阶段（Phase 1）是否可读缓存？ | WAL 事件中的 Secret 与缓存可能有时间差 |

**💥 可能后果：**
- 开发者启用 `secret_cache_fallback: true` 提升可用性，但未意识到缓存文件中的 API Key/DB 密码以明文写入磁盘
- 攻击者通过路径遍历、容器卷挂载、备份文件泄露读取缓存文件 → 获取所有 Secret
- Agent 崩溃时 core dump 包含 Secret 缓存 → 调试诊断过程中泄露
- 与 line 76 "解析结果缓存在内存加密区" 的保护力度不一致——内存加密了，磁盘缓存可能没加密

**🛠 建议：**
补充 Phase 0.5 secret_cache_fallback 的安全约束：

1. **存储加密**：缓存文件至少应加密存储（AES-256-GCM），密钥绑定到 Agent 实例（如基于 TPM/HSM 的 key wrapping, 或 Agent 启动时从 Secret Store 获取的一次性密钥）
2. **文件权限**：默认 600（仅 Agent 进程可读写）
3. **缓存 TTL**：可配置 `secret_cache_ttl_sec`，默认建议 300s（5分钟），与常见 Vault token TTL 对齐
4. **进程 dump 保护**：缓存读取后立即在内存中解密，缓存文件本身不存储明文——与 line 76 "内存加密区" 策略一致
5. **Phase 1 可用性**：明确声明 secret_cache_fallback 在 Phase 1（WAL 恢复阶段）**不可用**——WAL 中的事件使用缓存 Secret 可能存在时效差，插件加载（Phase 2）前 Secret Store 必须可达

---

### 🎯 R16 关注点 5：§9.1 workflow 状态名小写问题影响全部 transition（R15 #1 修复不彻底）(🟢 低)

**📐 场景：**
R15 #1 修复将事件名从 `ERROR_EVENT` 改为 `ERROR`（line 1725），但留下了状态名 `to: error`（小写）。

R15 对修复的描述：
> YAML 示例事件名改为 `ERROR` 与 §3.7 核心定义对齐；**状态名保持 YAML 小写风格与同文件其他状态名一致**

这个决定的问题是：R15 解决了"事件名不一致"的**表象**，但没解决"状态名体系不一致"的**根因**。§3.7 核心定义全大写、§9.1 YAML 全小写的矛盾**在所有状态上**都存在，不仅 ERROR 一个状态。

**💥 可能后果：**
与 R16 #1 相同。即使 R15 修复了事件名对齐，如果框架使用大小写敏感状态名比较，1721-1726 行的所有转移在运行时全部失效。这是一个**修复了表象但没解决根因**的问题。

**🛠 建议：**
这是 R16 #1 的子集/引子。与 #1 统一处理——在 §3.7 的 size 约束中声明状态名大小写不敏感，框架运行时统一 normalize。

---

### 🎯 R16 关注点 6：force_publish_on_timeout 枚举值混合 boolean 和 string 类型 (🟢 低)

**📐 场景：**
§3.2 FileWatchSource 参数定义（line 348）：
```
  force_publish_on_timeout: mark_incomplete  # mark_incomplete | false | true
```

枚举值包含三种写法：
| 值 | 类型 | 语义 |
|-----|------|------|
| `mark_incomplete` | string | 超时后强制发布，附加 `incomplete: true` flag |
| `true` | boolean | 超时后强制发布，不标记 incomplete |
| `false` | boolean | 超时后不发布 |

**💥 可能后果：**
- YAML 解析器可能将 `true` / `false` 解析为原生 boolean 而非 string，配置校验需要特殊处理（pattern 匹配需区分 bool 和 str 类型）
- 开发者不清楚 `true` 和 `mark_incomplete` 的具体行为差异——两者都"强制发布"，一个有 incomplete flag 一个没有
- 文档唯一的值示例 `mark_incomplete` 与缺省建议冲突（默认是 `mark_incomplete` 但 line 348 就是示例行）

**🛠 建议：**
统一为纯字符串枚举（消除跨类型混杂）：

| 新值 | 替代 | 语义 |
|------|------|------|
| `mark_incomplete` | 替代自身 | 超时后强制发布，附加 `incomplete: true` flag |
| `publish_anyway` | 替代 `true` | 超时后强制发布，不标记 incomplete |
| `none` | 替代 `false` | 超时后不发布（等待下次 FS 事件） |

修改 line 348 为：
```
  force_publish_on_timeout: mark_incomplete  # mark_incomplete | publish_anyway | none
```

与 "check_open_files" 的 `auto | true | false` 三值不同——那里 `true`/`false` 的 boolean 语义清晰（开启/关闭锁检测），而这里 `true`/`false` 的语义是"强制发布/不强制发布"再加一个"附加 flag"的变化，三值语义不是 bool 的扩展，而是三个离散模式，用纯 string 更清晰。

---

## 审计总结

**R15 修复验证：**
```
R15 共 6 项：全部已修复 ✅
前 15 轮共 69 项：全部关闭
```

**R16 新发现：6 项**

| # | 关注点 | 等级 | 维度 | 来源 |
|---|-------|------|------|------|
| 1 | 核心大写状态名与 YAML 小写状态名不一致，无大小写敏感性声明 | 🟡 中 | 配置一致性 | §3.7 ↔ §9.1 |
| 2 | §6.4 reconfigure 权限模型未同步更新 `/cron/add/update/remove` 跨引用 | 🟢 低 | 残留不一致/交叉引用 | R15 #6 修复不完整 |
| 3 | 状态图未反映 ERROR→CANCEL 路径 | 🟢 低 | 文档可视化脱节 | R16 #5 新加规则未同步图 |
| 4 | secret_cache_fallback 本地缓存安全性未定义 | 🟢 低 | 安全边界 | R15 #4 修复引入 |
| 5 | §9.1 workflow 状态名小写问题影响全部 transition | 🟢 低 | 残留不一致 | R15 #1 修复不彻底 |
| 6 | force_publish_on_timeout 枚举值混合 boolean 和 string | 🟢 低 | 类型不一致 | 跨轮次残留 |

**按影响维度分类：**

| 维度 | R16 新发现 | 说明 |
|------|-----------|------|
| 配置一致性 | #1, #5 | 状态名体系（大写 vs 小写）未定义大小写敏感性，影响所有 workflow 配置 |
| 可视化脱节 | #3 | ERROR→CANCEL 真实存在的路径未在状态图中反映 |
| 交叉引用 | #2 | §9.3 引用 §6.4 但 §6.4 未回指 |
| 安全边界 | #4 | secret_cache_fallback 加密/权限/TTL 未定义 |
| 类型设计 | #6 | 三值枚举跨 boolean/string 类型 |

**趋势线：**
```
R8→R9→R10→R11→R12→R13→R14→R15→R16 (本轮)
 3 →  3 →  2 →  2 →  2 →  2 →  7 →  6 →  6
```

单轮 6 项，持平 R15。R15 修复质量高但存在**修复不彻底**现象（#2 §6.4 未同步，#5 状态名根因未解决）和**新引入安全面**（#4 cache fallback 安全性）。🟡 中风险 1 项（#1 状态名大小写体系），建议优先处理——这是配置一致性的根源问题，修了它 #5 自然关闭。

建议新增风险条目 #70-#75（共 6 项）。
