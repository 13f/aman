# Idle State System — 业务逻辑审计 R6（收敛报告）

> 审计目标：R5 修复验证 + 全路径收敛确认
> 审计方法：逐条验证 R5 修复 + 4 个关键场景的全链条追踪 + 跨轮修复的交互一致性
> 审计结论：**设计已收敛。最后一项观察为 P4（建议级），不需要结构变更。**

---

## 第一部分：R5 修复验证

| R5 # | 核心问题 | 修复方式 | 验证结果 |
|------|---------|---------|:-------:|
| R5-1 ⚠️ | Reflection 连锁任务覆盖 `last_source_type` | `is_from_external_source()` 守卫——仅外部事件写入源类型 | ✅ 正确 |

**R5-1 执行路径验证（4 场景追踪）：**

| 场景 | 路径 | 结果 |
|------|------|:----:|
| 正常聊天 → Reflection 连锁任务 | Chat 事件 → `last_source_type=Chat` → 连锁任务不覆盖 → ChatMode 保持 | ✅ |
| 快速连锁任务链（A→B→C） | 每个连锁任务触发 `cancel_idle_workflows()`（token 旋转），但不覆盖 `last_source_type` | ✅ 正确（token 旋转多余但无害） |
| 聊天中 Timer 事件 | Timer 是外部 Source → 覆盖 `last_source_type=Timer` → 切换到完整人格（正确行为——Timer 不是对话的一部分） | ✅ |
| shutdown 时连锁任务未完成 | Phase 4.5 → IdleDetector 停止 → 连锁任务完成 → 正常关闭 | ✅ |

---

## 第二部分：跨轮修复交互验证

经过 5 轮审计、30+ 项修复，最重要的不是单点修复正确性，而是**多个修复之间的交互是否有一致性**。

以下是跨轮修复交互矩阵：

| 交互点 | 涉及修复 | 交互一致性 |
|--------|---------|:---------:|
| `last_source_type` 写入 vs `is_from_external_source()` | R2-1 + R5-1 | ✅ 一致——外部事件写入，内部连锁任务不写入 |
| `real_event_seen` 写入 vs chained task 处理 | R4-3 + R5-1 | ✅ 一致——`cancel_idle_workflows()` 仍然为所有 `is_real` 事件调用，所以 chained tasks 也会触发 depth 重置。这是安全行为 |
| `effective_personality()` 的 `was_chat` 条件 vs `last_source_type` 保护 | R3-1 + R5-1 | ✅ 一致——连锁任务不覆盖 `last_source_type`，所以 `is_chat` 在会话期间保持 true |
| `cancel_idle_workflows()` 的 token 替换 vs Workflow 启动 | R2-2 + R4-3 | ✅ 一致——`real_event_seen` 在 token 替换前设置，IdleDetector poll 先看到标志并返回空，不会启动新的 Workflow |
| `from_chat_mode` 字段 vs Pipeline 行为 | R3-2 + R4-1 | ✅ 一致——Boredom Pipeline 读取 `from_chat_mode`，true = no-op，false = 随机浏览 |
| `real_event_seen.swap(false)` 防止重复消费 | R4-3 | ✅ 正确——`swap` 的原子语义保证标志被恰好一个 poll 消费 |
| `busy_reflecting` 在 `abort()` 之前的 store/release 顺序 | R2-9 | ✅ 正确——先 abort 再 store(false)，无 yield 点 |

---

## 第三部分：R6 观察

经过 5 轮完全审计，已没有 P1/P2/P3 级别的问题。以下是一项 P4 观察。

---

### 🎯 R6-1 (P4): `cancel_idle_workflows()` 为内部连锁任务不必要地旋转 `CancellationToken`

`is_from_external_source()` 守卫保护了 `last_source_type.store()`，但 `cancel_idle_workflows()` 本身没有被守卫。

```rust
if is_real {
    if event.is_from_external_source() {
        coord.last_source_type.store(...);         // 🛡 已守卫
    }
    coord.cancel_idle_workflows();                   // ❌ 未守卫
    self.dispatch(event).await;
}
```

对连锁任务链（A→B→C）的影响：
- 每条连锁任务都创建一个新的 `CancellationToken`（T1→T2→T3→T4）
- 每条连锁任务设置 `real_event_seen=true`（被下一次 IdleDetector poll 消耗）
- 行为上完全正确：深度在每条连锁任务后被重置，ChatMode 保持激活

这不是一个bug——它不会导致错误行为。只是一个不必要的 `CancellationToken` 分配和 `RwLock` 写锁获取。在连锁任务长度>3（熔断阈值之前最大10）时，最多浪费10次 token 分配。

🛠 建议：在 `if is_real` 分支中增加 `is_from_external_source()` 守卫的扩展：
```rust
if is_real {
    if event.is_from_external_source() {
        coord.last_source_type.store(...);
        coord.cancel_idle_workflows();   // 只需要对外部事件调用
    }
    self.dispatch(event).await;
}
```

**不需要。——当前的实现正确，性能影响可忽略。**

---

## 第四部分：审计收敛报告

### 5 轮审计的生命周期

```
R1: 11 项发现 (3×P0/P1)     — 类型系统 + 时序模型 + 结构盲区
R2: 11 项发现 (2×P1)         — 传播链断裂 + 中断机制缺失
R3:  5 项发现 (2×P1)         — 执行路径条件错误 + 信息传递断裂
R4:  3 项发现 (0×P0/P1)     — 文档语义偏差 + 时序边界漏洞
R5:  1 项发现 (0×P0/P1)     — 因果链语义断裂
R6:  0 项发现 (0×P0/P1/P2/P3) — 收敛确认
```

### 发现数量收敛曲线

```
R1: ████████████ 11
R2: ████████████ 11
R3: █████           5
R4: ███             3
R5: █               1 (P2)
R6:                 0 (P4 observation only)
```

### 问题类型演进

| 轮次 | 主导问题类型 | 修复深度 |
|------|------------|---------|
| R1 | 缺失结构类型 | 新增 enum/struct |
| R2 | 传播路径断裂 | 新增字段 + 伪代码重写 |
| R3 | 执行条件错误 | 改逻辑条件 |
| R4 | 文档-语义偏差 | 修正注释 |
| R5 | 因果链断裂 | 加守卫条件 |
| R6 | **无** | — |

### 设计成熟度评估

| 维度 | 评分 | 说明 |
|------|:----:|------|
| **类型系统完整性** | ★★★★★ | 所有 struct/enum/field 定义完整，Serialization 约束明确 |
| **时序/并发正确性** | ★★★★★ | AtomicBool+CancellationToken+RwLock，所有竞态窗口已闭合 |
| **状态机完备性** | ★★★★★ | 状态转换定义清晰，无不可达状态，人格切换边界正确 |
| **聊天适应性** | ★★★★★ | ChatMode+grace_period+from_chat_mode+context_isolation |
| **文档-实现一致性** | ★★★★★ | 伪代码与文本描述对齐，打断矩阵准确 |
| **错误/恢复路径** | ★★★★☆ | Workflow checkpointing 定义，Incubation CT 管理。Meditation 文件安全。但 Workflow step error 后的重试策略未定义（属于 Workflow 引擎设计) |

**结论：设计已准备好进入实现阶段。**

---

*审计人：业务逻辑审计器 R6*
*审计范围：R5 修复验证 + 跨轮交互一致性 + 收敛确认*
*审计日期：2026-05-16*
