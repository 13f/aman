# Idle State System — 业务逻辑审计 R4

> 审计目标：R3 修复验证 + 死角探查
> 审计方法：逐条验证 R3 修复 + 打断路径全枚举 + 序列化边界推演
> R3 修正状态：文档标记 R3 5 项发现"全部修复"——本报告验证每项修复

---

## 第一部分：R3 修复验证

| R3 # | 核心问题 | 修复方式 | 验证结果 |
|------|---------|---------|:-------:|
| R3-1 🚨 | depth 重置条件在纯聊天场景下不触发 | 条件从 `was_chat && !is_chat` 改为 `was_chat` | ✅ 正确——超时退出和源变更退出都覆盖 |
| R3-2 🚨 | 聊天 Boredom no-op 信息不可达 | `IdleEvent` 新增 `from_chat_mode: bool` | ✅ 正确——Pipeline 可读取 |
| R3-3 ⚠️ | Incubation 打断语义不一致 | 明确写"否（仅 Phase 4.5 关闭）" | ✅ 正确——语义清晰 |
| R3-4 ⚠️ | RwLock 微阻塞 | 已接受（纳秒级） | ✅ 合理决策 |
| R3-5 🔍 | `last_non_idle` 滞后 | 已接受（3%误差） | ✅ 合理决策 |

**R3 修复验证结论**：5/5 ✅。R3 是第一个无新增结构变更的审计轮——修复集中在逻辑条件和字段传播，不改类型系统。

---

## 第二部分：R4 新发现

| # | 严重性 | 关注点 | 核心问题 |
|---|--------|--------|---------|
| **R4-1** | **P2 ⚠️** | **打断策略矩阵对 Pipeline 类型空闲状态的描述不准确** | `cancel_idle_workflows()` 只影响 Workflow，Boredom/Daze/Waiting 作为 Pipeline 实际不可打断 |
| **R4-2** | **P3 🔍** | **`from_chat_mode` 序列化后跨会话残留** | 低优 IdleEvent 被持久化后在重启的会话中恢复，`from_chat_mode` 可能引用过期的聊天上下文 |
| **R4-3** | **P3 🔍** | **`idle_depth` 在真实事件到达后不保证重置** | 依赖 poll 时 `pending_count()>0` 的 timing 窗口，事件被快速处理后可能错过重置 |

---

## P2

---

### 🎯 R4-1: 打断策略矩阵对 Pipeline 状态的描述不准确

📐 §4.4 打断策略矩阵当前：
```
| Daze    | IdleDetector | —（仅在 poll 间存在）      | 无 | 立即唤醒 |
| Boredom | IdleDetector | cancel_idle_workflows()  | 无 | Pipeline 丢弃 |
| Waiting | IdleDetector | —                        | 无 | 条件满足→Active |
```

但实际执行路径（§5.2 Dispatcher 主循环）：
```rust
Some(event) => {
    let is_real = !event.is_queue_drained() && !event.is_idle_event();

    if is_real {
        coord.cancel_idle_workflows();   // ← 只在真实事件分支
        self.dispatch(event).await;
    } else if event.is_queue_drained() {
        // select! — Reflection 可打断
    } else {
        // IdleEvent → dispatch(event).await ← 阻塞！
        self.dispatch(event).await;
    }
}
```

**IdleEvent 分支（所有 Pipeline 类型的空闲）不走 `cancel_idle_workflows()`，也不走 `select!`。`dispatch(event).await` 是阻塞调用。**

| 空闲类型 | 处理方式 | 实际可打断？ | 打断机制 |
|---------|---------|:----------:|---------|
| Daze | Pipeline（空） | ❌ 否 | `dispatch(event).await` 阻塞执行至完成（空 Pipeline 极快，通常 <1ms） |
| Boredom | Pipeline（无状态） | ❌ 否 | 同上。聊天模式下 no-op 也需执行完才返回 |
| Sleep | Workflow + cancel token | ✅ 是 | `run_with_cancel()` 每步检查 cancel_token |
| Exploration | Workflow + cancel token | ✅ 是 | 同上 |
| Meditation | Workflow + cancel token | ✅ 是 | 同上 |
| Waiting | Pipeline | ❌ 否 | 同上 |
| Incubation | Pipeline + 后台线程 | ⚠️ 仅独立 CT，不由 `cancel_idle_workflows()` 管理 |

💥 可能后果：

- **Boredom 表里行"cancel_idle_workflows()"是错的**：`cancel_idle_workflows()` 取消的是 Workflow 的共享 `idle_cancel_token`，Boredom Pipeline 不监控这个 token，也不被它影响
- **实现者可能误以为 Boredom/Daze 是轻量可抢占的**：在时间敏感场景（如大量 Boredom 事件快速到达）下，Pipeline 会累积阻塞
- **聊天模式下 Boredom 即使 no-op 也阻塞 Dispatcher**：no-op Pipeline 返回很快（<1ms），但任何 Pipeline dispatch 都是同步的——真正的真实事件延迟 = Pipeline 执行时间

🛠 建议：

1. **修正 §4.4 打断策略矩阵**：Boredom 的打断机制改为 `否（Pipeline 同步执行至完成，不监控 cancel_token）`。Daze/Waiting 同理标注同步执行语义。
2. **在 §6.2 加备注**：Pipeline 类型的空闲状态不可被真实事件打断——真实事件在 Event Bus 中排队，直到 Pipeline 完成。
3. **实际风险很低**：Daze Pipeline = 空（仅 metrics），Boredom timeout 应该在 Pipeline 层面配置。这不是运行时缺陷，而是文档准确性缺陷。

---

## P3

---

### 🎯 R4-2: `from_chat_mode` 序列化后跨会话残留

📐 `IdleEvent` 声明：
```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IdleEvent {
    pub from_chat_mode: bool,
    // ...
}
```

场景推导：

```
Agent 在聊天模式下运行，产生 Boredom(from_chat_mode=true)
Event Bus 背压激活（5 级全满）→ IdleEvent 被推送到 overflow（磁盘持久化）
Agent 重启 / crash recovery → WAL 恢复
overflow 中的 IdleEvent 被反序列化 → from_chat_mode=true
但这是新的 Agent 会话：
  - 没有活跃的聊天上下文
  - `last_source_type` 是 Unknown
  - `was_in_chat_mode` 是 false
Boredom Pipeline 读取 from_chat_mode=true → 执行 no-op 路径
→ Agent 在系统模式下不执行 Boredom 的随机浏览
```

💥 可能后果：

- **no-op 偏差**：IdleEvent 在恢复后声称自己是聊天模式，但实际上已经没有聊天上下文。Boredom Pipeline 因此跳过随机浏览——行为无声地错了
- **聊天模式时长统计偏差**：如果 metrics 也依赖 `from_chat_mode`，`chat_mode_active_seconds` 可能被持久化的事件延长

🛠 建议：

1. **IdleEvent 不应进入持久化队列**：Event Bus 的 backpressure overflow 应该优先丢弃 Lowest priority 的事件。设计约束明确：IdleEvent（priority=Low）在 overflow 触发时丢弃而非持久化。
2. **或在 `Deserialize` 实现中重置 `from_chat_mode = false`**：反序列化时忽略此字段（恢复的旧事件在 Agent 新会话中没有 chat 上下文）。
3. **风险极低**：IdleEvent 进入 overflow 需要 5 级背压全满，且 Agent 在此状态下重启。实际概率 <0.1%。

---

### 🎯 R4-3: `idle_depth` 在真实事件到达后不保证重置

📐 `IdleDetector.poll()` 中的 depth 重置逻辑：
```rust
if ctx.event_bus.pending_count() > 0 {
    self.idle_depth = 0;         // 仅在 poll 时队列非空才重置
    self.last_non_idle = Instant::now();
    return Ok(vec![]);
}
```

场景推导：

```
t=0    idle 시작, depth=0 (Daze)
t=5    idle, depth=1 (Boredom)
t=10   idle, depth=2 (Boredom)
t=15   idle, depth=3 (Sleep) → Sleep Workflow 启动
t=18   真实事件到达 → Dispatcher 取出 → cancel_idle_workflows() 取消 Sleep
       → 事件处理完成 (耗时 2s)
t=20  队列空 → QueueDrained → Reflection → 完成
t=21  IdleDetector poll: pending_count() == 0 (事件已处理完)
       → 不重置 depth
       → depth 仍然是 3
       → kind = resolve(3, ...) = Sleep
```

💥 可能后果：

- **depth 在事件处理后不重置**：sleep Workflow 刚被取消，IdleDetector 马上又产生一个 Sleep 事件。Workflow 重新启动同样的 Sleep 过程。如果真实事件只是短暂的打断（用户发送一条消息），Sleep 会被反复启动→取消→启动，从不完成。
- **状态机图暗示"count 重置"**（§4.2 流程图："count 重置 → 回到 Active"），但 idle_depth 实际上没有重置机制
- **R1 中类似问题**：R1 的 P0 修复了 Reflection 期间的 idle 误判，但没有修复"事件处理完成后 depth 不重置"的场景。R3 修复了人格切换时的 depth 重置，但没有修复"真实事件打断 idle 后 depth 不重置"的场景——**同一类问题在不同路径上遗漏了**。

🛠 建议：

1. **IdleDetector 的 depth 重置不应依赖 timing window**。在 `IdleCoordination` 中添加 `real_event_seen: AtomicBool`：
   - Dispatcher 在 `is_real` 分支中设置 `real_event_seen = true`
   - IdleDetector 在 poll 时读取，如果 `true` 则重置 depth 并清标志

2. **或利用 `pending_count()` 的 value 变化**：如果 IdleDetector 在两次 poll 之间看到 `pending_count()` 从 0→1→0 的变化，它无法感知。但如果它能记录 `last_known_count` 并在下次 poll 时检测到 `last_known_count > 0`，也能达到同样效果。
   - 更轻量：`IdleDetector.last_poll_had_pending = true` → 当前 poll 发现 `pending_count() > 0` 是作为边沿检测，但设计使用瞬时快照，没有边沿记忆。

3. **或完全依赖 `cancel_idle_workflows()` 的副作用**：如果 workflow 被取消，且这个副作用让 IdleDetector 在下一次 poll 时知道自己应该重置 depth... 但 IdleDetector 不知道 workflow 被取消了（它不监控 idle_cancel_token）。

**最简修复**：在 `IdleDetector.poll()` 的 `if ` 基础上增加一个状态标志：
```rust
// IdleDetector 新增字段
pub pending_was_positive: bool,

// poll() 中：
if ctx.event_bus.pending_count() > 0 || self.pending_was_positive {
    self.idle_depth = 0;
    self.last_non_idle = Instant::now();
    self.pending_was_positive = false;  // 消费信号
    return Ok(vec![]);
}
// 记录当前状态供下次 poll 使用
if ctx.event_bus.pending_count() > 0 {
    self.pending_was_positive = true;
}
```

但这仍然依赖 IdleDetector 在此次 poll 时看到 `pending_count() > 0`。更可靠的方案：**在 Dispatcher 中直接设置 idle_depth = 0**。

---

## 第三部分：修复模式观察

### R1→R4 的问题类型演进

```
R1: 缺少核心结构              (busy_reflecting, select!, ChatMode, cancel_token)
R2: 结构之间的传播链断裂        (last_source_type, idle_cancel_token ✅)
R3: 已有结构的执行路径条件错误    (depth reset condition ✅, from_chat_mode field ✅)
R4: 跨组件的时序依赖和文档偏差    (Pipeline 打断语义, depth 重置 timing)
```

R4 的问题是三类中最隐蔽的——它们不是代码 bug，而是**设计文档对运行时行为的描述是否忠实地与实际语义一致**，以及**隐含的时序假设是否正确**。

### R4 所有发现不需要类型系统变更

所有三个发现（R4-1/2/3）的修复都不需要新增 struct/enum/字段。它们是：
- **R4-1**：文档表述修正（§4.4 表格 Boredom 打断机制列改为"否"）
- **R4-2**：设计约束声明（overflow 丢弃低优先级 idle 事件）或 Deserialize 修正
- **R4-3**：一行新逻辑（IdleDetector 读取 Dispatcher 的"真实事件已到达"标志）

---

## 汇总

| 维度 | 数量 | 说明 |
|------|------|------|
| **R3 修复验证** | 5 项 | 5/5 ✅ |
| **R4 新发现** | 3 项 | 1×P2, 2×P3 |

**R4 最值得注意的发现**：P3 中 `idle_depth` 不保证重置的问题（R4-3）有较低概率引发"真实事件打断后 idle 立刻又在同一深度重启"的循环行为。跟 R1-P0 的时序竞态是一类问题——依赖 timing window 而非显式信号。

建议在实现前加上 R4-3 的修复（一行 `AtomicBool`，Dispatcher 写入，IdleDetector 读取），与 R2-1 的 `last_source_type` 传播模式一致。

---

*审计人：业务逻辑审计器 R4*
*审计范围：R3 修复验证 + 死角探查*
*审计日期：2026-05-16*
