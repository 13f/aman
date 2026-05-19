# Idle State System — 业务逻辑审计 R5

> 审计目标：R4 修复验证 + 深层因果链追踪
> 审计方法：逐条验证 R4 修复 + 追踪 Reflection 产出的连锁任务在源类型传播中的因果链断裂
> R4 修正状态：文档标记 R4 3 项发现"全部修复"——本报告验证每项修复

---

## 第一部分：R4 修复验证

| R4 # | 核心问题 | 修复方式 | 验证结果 |
|------|---------|---------|:-------:|
| R4-1 ⚠️ | Pipeline 打断矩阵描述不准确 | §4.4 新增"可被真实事件打断？"列；加语义解释段落 | ✅ 正确且清晰 |
| R4-2 🔍 | `from_chat_mode` 序列化残留 | 设计约束：IdleEvent overflow 时不持久化 | ✅ 正确 |
| R4-3 🔍 | `idle_depth` 事件后不保证重置 | `IdleCoordination.real_event_seen` + `swap` 消费 | ✅ 正确——atomic swap 干净地解决了 timing 依赖 |

**R4 修复验证结论**：3/3 ✅。R4 的三项发现全部正确修复。R4-3 的 `real_event_seen.swap(false)` 模式尤其干净——它不依赖 timing window，`swap` 的原子语义保证了标志不会被重复消费或丢失。

---

## 第二部分：R5 新发现

| # | 严重性 | 关注点 | 核心问题 |
|---|--------|--------|---------|
| **R5-1** | **P2 ⚠️** | **Reflection 产出的连锁任务覆盖 `last_source_type`，导致 ChatMode 提前退出** | 连锁任务的 source_type（如 pipeline:reflection）覆盖了用户 Chat 事件的 source_type，ChatMode 在对话期间被静默停用 |

---

## P2

---

### 🎯 R5-1: Reflection 产出的连锁任务覆盖 `last_source_type`

📐 当前 Dispatcher 伪代码（§5.2）：
```rust
Some(event) => {
    let is_real = !event.is_queue_drained() && !event.is_idle_event();

    if is_real {
        coord.last_source_type.store(
            event.source_type().to_u8(), Ordering::Relaxed   // ← 覆盖！
        );
        coord.cancel_idle_workflows();
        self.dispatch(event).await;
    }
```

所有 `is_real` 事件（包括 Reflection 产出的连锁任务）都会写入 `last_source_type`。连锁任务的 `source_type` 来自产生它的 Pipeline（如 `pipeline:reflection`），而非原始事件的源（如用户 Chat Source）。

💥 场景推导：

```
t=0    用户发送 Chat 消息 (source_type = Chat)
           → last_source_type = Chat
           → dispatch(消息)

t=1    消息处理完成
           → 队列空 → QueueDrained → Reflection

t=1.5  Reflection 产出连锁任务 "记录经验到 memory"
           连锁任务的 source_type = "pipeline:reflection" (系统内部)

t=1.6  Dispatcher 取出连锁任务:
             is_real = true
             last_source_type.store("pipeline:reflection")  ← 覆盖 Chat!

t=2    连锁任务处理完成，队列空，无更多产出
           开始空闲序列

t=3    IdleDetector poll
           读取 last_source_type = "pipeline:reflection"
           is_chat() = false (SourceType 不匹配 Chat)
           → 完整人格激活，ChatMode 停用
           → depth_schedule 从 depth=0 开始: Daze → Boredom → Sleep

t=10   depth=3 → Sleep Workflow 启动（记忆整理）
       ↑ 用户正在打字！
t=30   用户发送下一条消息 → cancel_idle_workflows() → Sleep 被中断
```

💥 可能后果：

- **每条用户消息后的第一个 Reflection 连锁任务都会静默退出 ChatMode**——不管用户是否还在会话中
- **深度空闲在不合适的时机启动**：用户刚问完一个问题，agent 去回答了（Reflection 顺便记了个日志），然后立刻开始 Sleep（记忆整理）。30 秒后用户下一条消息到达时，Sleep 刚整理了一半（checkpoint 保存），被中断
- **R2-1 的修复被连锁任务的因果链断裂绕过**：R2-1 修复了"Chat 事件→IdleDetector"的传播路径，但连锁任务在 IdleDetector 之前插了一脚，把 Chat 标记清掉了
- **与核心约束矛盾**：§1 "聊天场景约束——对话轮次之间不应触发完整空闲序列"——连锁任务的处理是对话轮次的一部分，但触发了完整空闲序列

🛠 建议（三选一，按推荐排序）：

**方案 A（推荐——最轻量，不改事件模型）：不要在 Dispatcher 中覆盖 `last_source_type`，除非事件来自外部 Source。**
```rust
// 只在外部事件（非内部连锁任务）时覆盖 last_source_type
if is_real && event.is_from_external_source() {
    coord.last_source_type.store(
        event.source_type().to_u8(), Ordering::Relaxed
    );
}
```
需要 `Event` 新增 `is_from_external_source()` 方法——区分直接来自 Source 的事件和 Pipeline/Workflow 产出的事件。

**方案 B（推荐——因果链语义更清晰）：连锁任务继承父事件的 source_type。**
Reflection Pipeline 在产出连锁任务时，将父事件（触发该 Reflection 的事件）的 source_type 传递给任务。这样连锁任务就像"用户发起的"一样携带 Chat 标记。
- 需要在 Pipeline output 机制中传递元数据
- 更接近"因果链"的语义

**方案 C（维持现状但调整 ChatMode 逻辑）：IdleDetector不单靠 `last_source_type` 判断 ChatMode，加入"短期内是否有 Chat 事件"的多重判断。**
```rust
// 不再仅依赖 last_source_type，而是 check if ANY recent event was chat
fn recent_chat_event_exists(&self) -> bool {
    self.last_chat_timestamp.map_or(false, |t| {
        t.elapsed() < Duration::from_secs(300)  // 5分钟内有过聊天
    })
}
```
但方案 C 引入了"5 分钟"的硬编码阈值，不如方案 A/B 干净。

---

## 修复验证补充

### 确认已修复的设计门槛

R5 之前，所有修复的验证状态：

| 审计轮 | 发现数 | P0/P1 | 修复验证 |
|--------|-------|-------|:-------:|
| R1 | 11 | 3 | 全部修复 ✅ |
| R2 | 11 | 2 | 9/11 ✅, 2 修复断裂 → R3 修复 ✅ |
| R3 | 5 | 2 | 5/5 ✅ |
| R4 | 3 | 0 | 3/3 ✅ |
| **R5** | **1** | **0** | **P2 ⚠️ — 新发现** |

R5 是第一个 P0/P1 数量为 0 的审计轮。唯一的 P2 发现（R5-1）不改变类型系统，只改变 Dispatcher 中一行 `store()` 的条件——加一个 `is_from_external_source()` 检查。

---

## 汇总

| 维度 | 数量 | 说明 |
|------|------|------|
| **R4 修复验证** | 3 项 | 3/3 ✅ |
| **R5 新发现** | 1 项 | 1×P2 |

**R5 的本质**：前四轮审计关注的每一个修复点都是"从外到内"的——外部事件如何正确传播到内部组件。R5-1 是一个"从内到内"的问题：**内部系统（Reflection）产出的事件会污染外部事件的源类型标记**。这是因果链断裂——连锁任务"为什么"被触发（因为用户消息）的信息在传递中丢失了。

**对实现的影响**：R5-1 的修复是 R2-1 修复的自然扩展。R2-1 建立了 `last_source_type` 传播路径，但没有定义"写保护"——哪些事件有权覆盖这个标记。补上这个写保护规则后，整个源类型传播子系统就完整了。

修复量：Dispatcher 中改一行条件（加 `is_from_external_source()` 检查），Event 模型加一个方法。

---

*审计人：业务逻辑审计器 R5*
*审计范围：R4 修复验证 + Reflection 连锁任务因果链追踪*
*审计日期：2026-05-16*
