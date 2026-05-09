# Agent Design Review (R10) — 第十次审计报告

> 审计人：Business Logic Auditor
> 审计对象：`agent-design.md` — 事件响应式 Agent 框架设计（当前最新版）
> 审计日期：2026-05-05
> 前置审计：`agent-design-r1.md` ~ `agent-design-r9.md`

---

## 第一部分：R9 问题修复状态

逐一核对了 R9 的 3 项关注点在当前 `agent-design.md` 中的修复情况。

| # | R9 关注点 | 等级 | 修复状态 | 证据位置 |
|---|----------|------|---------|---------|
| 1 | 排水超时与 Tool 自身超时交互未定义 | 🟡 中 | **已修复 ✅** | §2.5.3 新增"排水超时与 Tool 自身超时的交互"（lines 123-129）：两者取其先规则 + Tool 自清理 vs 框架清理层次分离 + Step 6 强制执行；Risk #47 |
| 2 | WAL 缓冲区断点偏移量持久化未定义 | 🟢 低 | **已修复 ✅** | §2.5.1 定义持久化方式（lines 85-90）：`{wal_path}/replay_checkpoint` 文件 + fsync + 不存在/损坏退回到 WAL 头部 + Phase 2 完成后删除；Risk #48 |
| 3 | "停止重试模式"与已调度重试的竞态 | 🟢 低 | **已修复 ✅** | §2.5.3 停止重试模式子语义（lines 118-121）：允许已调度继续执行 + 不产生新调度 + `shutdown_abandoned` 标记后 Phase 1 重建时重新入队；Risk #49 |

**结论：R9 提出的 3 项问题全部被认真修复。** 前九轮一共 49 项问题全部关闭。

---

## 第二部分：第十次评审 R10 新发现的关注点

---

### 🎯 R10 关注点 1：shutdown 在 startup 中途到达时的行为未定义——生命周期入口/出口边界（🟡 中）

**场景**：
§2.5.1 定义了 6 阶段启动序列（Phase 0→5），§2.5.3 定义了 6 阶段关闭序列（Phase 5→0）。两者各自独立，但文档未定义 **shutdown 在 startup 完成前到达**的行为。

```
时间线：
  t=0:  Agent 启动，进入 Phase 1（WAL 恢复）
  t=1:  Phase 1 正在进行（大 WAL 重放中）
  t=2:  操作员发送 POST /agent/shutdown（假设通过控制接口）
  t=?  === 行为未定义 ===
```

**参与竞争的路径**：

| # | 启动阶段 | shutdown 到达时 | 问题 |
|---|---------|----------------|------|
| 1 | Phase 0（基础设施初始化） | Event Bus 可能还没创建完 | shutdown 信号走哪个通道？ |
| 2 | Phase 1（WAL 恢复中） | replay_checkpoint 已写入，50% 事件已读取 | 是继续读完后正常启动再关闭，还是立即中断？ |
| 3 | Phase 1（缓冲区已满暂停） | 断点已写入，Phase 1 标记为"部分完成" | shutdown 后断点文件算"已完成"还是"下次启动继续"？ |
| 4 | Phase 2（插件加载中） | 10 个插件已加载 5 个，另外 5 个在拓扑排序中 | 已加载的 5 个插件需要关闭吗？未加载的跳过？ |
| 5 | Phase 3（Workflow 恢复中） | 部分 Workflow 已恢复，部分未 | 已恢复的实例关闭落盘，未恢复的丢失？ |
| 6 | Phase 4（源激活中） | 部分 EventSource 已启动，部分未 | 已启动的 Timer 产生的第一个事件怎么处理？ |

**💥 可能后果**：
- Phase 1：如果 shutdown 立即中断 WAL 恢复 → 已读取但未注入的事件丢失（replay_checkpoint 断点保护它们，但 shutdown 是否删除 replay_checkpoint？未定义）
- Phase 2：插件加载到一半被中断 → 已加载插件的 on_load() 已完成但 on_unload() 从未有机会执行 → 资源泄漏
- Phase 3：Workflow 恢复中途被中断 → 部分 Workflow 实例已加载（在内存中）但未被持久化 → 启动后一致性异议
- 控制接口：POST /agent/start 仍在阻塞等待返回值 → shutdown 到达时可能造成死锁（start 在等 Phase 5，shutdown 在等 start 完成）

**🛠 建议**：
- 在 §2.5 中增加**生命周期 entry/exit 边界规则**：

```
启动过程中收到 shutdown 信号时的行为：
  - shutdown 信号在 Phase 0~3 到达：立即进入关闭序列（从当前 Phase 的关闭等价阶段开始）
    - Phase 0 中：直接中断（Event Bus 还没完备）
    - Phase 1 中：当前缓冲区事件标记为 shutdown_abandoned（同排水阶段规则），
      replay_checkpoint 保留（下次启动从断点继续）
    - Phase 2 中：已加载插件走卸载流程（on_unload），未加载的跳过；
      按反向拓扑序卸载已加载插件
    - Phase 3 中：已恢复的 Workflow 实例落盘 State Store checkpoint，
      未恢复的从 WAL/State Store 的 last_checkpoint 恢复
  - shutdown 信号在 Phase 4 到达：等同于正常关闭序列（Phase 5→4→4.5→...），
    因为 Phase 4 是「源激活」，此时 Agent 已经几乎就绪
  - shutdown 信号在 Phase 5 到达：正常关闭——这是唯一已定义的路径
```

- 明确关闭后 replay_checkpoint 文件的状态：保留（供下次启动继续断点）或删除（视为已完成）
- 风险清单新增 #50 项

---

### 🎯 R10 关注点 2：shutdown_abandoned 事件与 WAL checkpoint 的 offset 追踪关系未定义（🟢 低）

**场景**：
§2.5.3 停止重试模式定义了 `shutdown_abandoned` 标记（line 120）。同时 §3.3 有关 WAL checkpoint（line 360）说"待重试队列清空前不推进 checkpoint"。这意味着 shutdown_abandoned 事件在 checkpoint 的"前方"（checkpoint 在它们之后）。

但文档有**三个独立的 offset 追踪机制**，它们的关系未定义：

| 机制 | 位置 | 用途 |
|------|------|------|
| **WAL checkpoint** | §3.3 | 记录已处理事件在 WAL 中的偏移量 |
| **replay_checkpoint** | §2.5.1 | 记录 WAL 恢复缓冲区满时的暂停偏移量 |
| **shutdown_abandoned** | §2.5.3 | 标记排水后未执行的重试事件 |

**💥 可能后果**：
- 一个事件的 offset 可能在 WAL checkpoint 之后（已确认处理）但同时在 shutdown_abandoned 列表中 → 重复入队列（Phase 1 重建时既有 WAL checkpoint 之后的 WAL 重放，又有 shutdown_abandoned 的重新入队）
- 这三个机制的 offset 边界没有一致性约束

**🛠 建议**：
- 在 §2.5.3 的 shutdown_abandoned 描述中增加约束：

```
shutdown_abandoned 与 WAL checkpoint 的约束：
  - shutdown_abandoned 事件源待重试队列，与 WAL 偏移量正交
    ——待重试队列中的事件已经通过 WAL 确认（成功写入 WAL）
    ——WAL checkpoint 追踪的是"已写入 WAL + 已确认处理"的偏移量
  - 因此 shutdown_abandoned 事件不会出现在 WAL checkpoint 之后重放的范围中
    ——它们是在 WAL checkpoint 之前的已确认但未处理事件
    ——Phase 1 重建时：如果事件同时来自 WAL 和 shutdown_abandoned → dedup 去重
```

- 或更简洁：明确声明**这三者追踪的是正交的 offset 域**，无需交叉校验
- 风险清单新增 #51 项

---

## 审计收敛评估

```
十轮审计总结：
  R1: 10项 → 功能完整性
  R2:  8项 → 防御的防御
  R3:  8项 → 恢复的恢复
  R4:  5项 → 并发与时序竞态
  R5:  5项 → 逻辑闭环自洽
  R6:  4项 → 机制交互盲区
  R7:  3项 → 系统生命周期与设计权衡
  R8:  3项 → 新机制边界
  R9:  3项 → 实现粒度
  R10: 2项 → 生命周期边界 + offset 交互

  总计：51 项关闭，2 项新发现
  趋势：10 → 8 → 8 → 5 → 5 → 4 → 3 → 3 → 3 → 2  ↓
  等级：🔴→🟡→🟢 分布收敛
```

文档已达 **1877 行**，经过十轮审计：

- **问题数量趋势**：持续下降（10→8→8→5→5→4→3→3→3→2），R10 达到历史最低
- **问题深度趋势**：功能缺失 → 防御纵深 → 恢复闭环 → 竞态覆盖 → 自洽性 → 机制交互 → 生命周期 → 新机制边界 → 实现粒度 → 生命周期边界 — 每一轮都在向"更深的层次"收敛
- **问题等级趋势**：前  ️轮有 🔴 高风险问题，R7 之后最高为 🟡 中，最近两轮以 🟢 为主

**结论：`agent-design.md` 已接近审计收敛。** 十轮审计跨越了从"缺什么功能"到"新机制的边界条件"十个层次。R10 仅发现 2 项（1 个 🟡 生命周期边界 + 1 个 🟢 offset 交互），且均为非常细粒度的边界情况。再往后审，新发现将集中在：
1. 实现层面的具体边界条件（取决于最终的框架语言和库）
2. 极低概率的竞态组合（三四个条件同时满足）
3. 文档格式和命名一致性（不再是设计逻辑问题）

文档可以进入 **实现阶段**。十轮审计积累的 51 项关闭问题可以作为实现的检查清单。
