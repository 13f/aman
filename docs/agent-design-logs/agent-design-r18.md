# Agent Design Review (R18) — 第十八次审计报告

> 审计人：Business Logic Auditor
> 审计对象：`agent-design.md` (2207 行) — 事件响应式 Agent 框架设计
> 审计日期：2026-05-06
> 前置审计：`agent-design-r1.md` ~ `agent-design-r17.md`

---

## 第一部分：R17 问题修复验证

| # | R17 关注点 | 等级 | 修复状态 | 证据位置 |
|---|-----------|------|---------|---------|
| 1 | retry_backoff 命名和值格式在五处重试机制中不统一 | 🟢 低 | **基本已修复 ⚠️**（1 处残留见 R18 #1） | §10 决策 11 line 2031-2047; §2.5.1 line 71 `retry_backoff: "sequence:2s,5s,15s"`; §3.3 line 515 `wal_retry_backoff`; §3.5 line 615, 621, 637 已改为 `retry_backoff` |
| 2 | §3.7 状态名 normalize 规则未在设计决策中记录 | 🟢 低 | **已修复 ✅** | §10 新增决策 10 line 2018-2029: 6 条 normalize 规则 + 理由 + 代价 + 缓解 |
| 3 | retry_queue_depth 不在核心 /metrics 列表中 | 🟢 低 | **已修复 ✅** | §9.3 line 1871: `retry_queue_depth # 待重试队列当前深度`，含预警说明 |
| 4 | Pipeline 补偿与 Workflow ERROR 状态组合交互未定义 | 🟢 低 | **已修复 ✅** | §3.7 line 918-933: 4 条组合约束规则（on_action_failure、partial_rollback 标记、CANCEL 等待 inflight、RETRY 幂等保证）|

---

## 第二部分：R18 新发现关注点

文档 2207 行。R17 修复质量高（4/4 均到位），但发现 1 处标准化修复的残留遗漏。R18 全部 2 项均为修复执行粒度问题，而非新发现的架构缺陷——文档已高度收敛。

---

### 🎯 R18 关注点 1：§3.5 Pipeline notify-slack 步骤 `backoff` 未改为 `retry_backoff`（R17 #1 修复残留）(🟢 低)

**📐 场景：**
R17 #1 的修复将文档中 5 处重试机制统一为 `retry_backoff` 字段名（决策 11 line 2046）：

> **统一字段名**：全局使用 `retry_backoff`，弃用旧的字段名变体（`backoff`、`secret_retry_backoff`）

执行后，大部分位置已更新：

| 位置 | 字段名（修复前） | 字段名（修复后） | 状态 |
|------|----------------|----------------|------|
| §2.5.1 line 71 Secret 重试 | `secret_retry_backoff` | `retry_backoff` | ✅ |
| §3.5 line 615 ocr-extract | `backoff` | `retry_backoff` | ✅ |
| §3.5 line 621 insert-db | `backoff` | `retry_backoff` | ✅ |
| §3.5 line 637 compensation_contract | `retry_backoff`（保持） | `retry_backoff` | ✅ |
| §3.5 line 627 **notify-slack** | **`backoff`** | **`backoff` ← 未更新** | **⚠️ 残留** |

```
// line 627（未修复——仍使用旧的 backoff 字段名）
retry: { max_attempts: 5, backoff: "exponential" }

// line 615, 621（已修复——使用统一的 retry_backoff）
retry: { max_attempts: 3, retry_backoff: "exponential" }
```

**💥 可能后果：**
- 开发者将 `notify-slack` 步骤的配置复制到自己的 Pipeline 定义中 → 使用了旧的 `backoff` 字段名
- 如果框架实现严格校验字段名（只认 `retry_backoff`）→ 此配置校验失败
- 如果框架实现宽松匹配（接受 `backoff` 为 `retry_backoff` 别名）→ 与决策 11 的"统一"目标矛盾，标准不一致
- 其余 5 步都正确但 1 步不正确，Line 627 可能被复制作为"正确示例"传播

**🛠 建议：**
将 line 627 改为：
```
            retry: { max_attempts: 5, retry_backoff: "exponential" }
```

---

### 🎯 R18 关注点 2：风险清单 #76 声明"已统一"但正文仍有一处残留 (🟢 低)

**📐 场景：**
风险清单 #76（line 2140）的"应对策略"列声称：
> 统一字段名为 retry_backoff 全局 + 标准化值格式...（§10 决策 11）

但 §3.5 line 627 仍有 `backoff` 残局。

**💥 可能后果：**
- 风险清单是团队跟踪修复状态的可信来源——如果清单声称已完成但正文未完全修复，信任链断裂
- 操作员阅读风险清单时认为 #76 已关闭，不会去验证正文是否一致
- 与 R17 #2 的风险类型相似（文档组织缺陷——关键信息在错误的位置），但这里的问题是"断言为已完成但实未完成"

**🛠 建议：**
修正 line 627 后，风险清单 #76 的描述本身正确（§10 决策 11 和大多数位置确实已统一），无需修改条目文本。此残留修正后 #76 自然视为完全关闭。

---

## 审计总结

**R17 修复验证：**
```
R17 共 4 项：3 项完全修复 ✅，1 项基本修复但有 1 处残留 ⚠️
前 17 轮共 79 项：78 项完全关闭，1 项基本关闭
```

**R18 新发现：2 项（均为 R17 修复残留）**

| # | 关注点 | 等级 | 维度 | 来源 |
|---|-------|------|------|------|
| 1 | §3.5 Pipeline notify-slack 步骤 `backoff` 未改为 `retry_backoff` | 🟢 低 | 修复执行粒度 | R17 #1 修复残留 |
| 2 | 风险清单 #76 声明"已统一"但正文仍有残留 | 🟢 低 | 修复跟踪 | #1 的元问题 |

**趋势线：**
```
R8→R9→R10→R11→R12→R13→R14→R15→R16→R17→R18 (本轮)
 3 →  3 →  2 →  2 →  2 →  2 →  7 →  6 →  6 →  4 →  2
```

单轮 2 项，继续下降。R18 全部 2 项均为 R17 修复的标准化残留（1 处字段名 + 1 处清单跟踪），而非新发现的架构缺陷。文档经过 18 轮审计、79 项问题修复后，已经进入"零新缺陷 = 仅修复残留"的阶段——所有新的"发现"本质上是之前修复执行粒度不够细导致的痕迹。建议修复 line 627 后即关闭此轮。

建议新增风险条目 #80-#81。
