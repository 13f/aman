# Agent Design Review (R8) — 第八次审计报告

> 审计人：Business Logic Auditor
> 审计对象：`agent-design.md` — 事件响应式 Agent 框架设计（当前最新版）
> 审计日期：2026-05-05
> 前置审计：`agent-design-r1.md` ~ `agent-design-r7.md`

---

## 第一部分：R7 问题修复状态

逐一核对了 R7 的 3 项关注点在当前 `agent-design.md` 中的修复情况。

| # | R7 关注点 | 等级 | 修复状态 | 证据位置 |
|---|----------|------|---------|---------|
| 1 | 启动初始化顺序未定义 | 🟡 中 | **已修复 ✅** | 新增 §2.5 Agent 生命周期（lines 59-107），含 6 阶段启动序列 + 核心安全约束（WAL 缓冲、处理器就绪检查）+ 就绪探针（live vs ready）+ 优雅关闭反向 6 阶段 + 具体竞态防护 | Risk #41 |
| 2 | Priority 与同来源保序冲突 | 🟡 中 | **已修复 ✅** | §3.3 新增"优先级与保序的冲突规则"（lines 354-358）：同来源保序优先；新增**决策 9**（lines 1701-1708）：含理由、场景、代价、缓解 | Risk #42 |
| 3 | 补偿 retry 次数参数缺失 | 🟢 低 | **已修复 ✅** | §3.5 compensation_contract 新增 `retry_count: 3` 和 `retry_backoff: "exponential"`（lines 522-523） | Risk #43 |

**结论：R7 提出的 3 项问题全部被认真修复。** 前七轮一共 43 项问题全部关闭。

---

## 第二部分：第八次评审 R8 新发现的关注点

文档已达 1832 行。R7 将七个维度全部覆盖。R8 聚焦在新引入机制（启动序列）的完整性以及**新机制之间的衔接处**。

---

### 🎯 R8 关注点 1：优雅关闭序列未覆盖 Pipeline/Skill 的 inflight 执行和补偿（🟡 中）

**场景**：
§2.5.3 优雅关闭顺序定义了 6 个阶段（lines 98-105）：

```
Phase 5 [停止接收] ── 控制接口关闭 → 负载均衡移除该实例
Phase 4 [源停止] ── Event Source 关闭 → Webhook 返回 503 → Timer/Cron 停止
Phase 3 [状态持久化] ── Workflow 活跃实例落盘 → State Store checkpoint
Phase 2 [组件卸载] ── 插件卸载 → Skill 反注册 → Dispatcher 清空
Phase 1 [WAL 刷盘] ── 待重试队列清空 → WAL checkpoint 最终写入
Phase 0 [基础设施关闭] ── Event Bus 关闭 → 背压系统关闭
```

**Pipeline/Skill 执行和补偿操作在这个序列中没有出现。**

具体竞态路径：

```
时间线：
  t=0: shutdown 信号到达，Phase 5 开始
  t=1: Pipeline "invoice-processor" 正在执行 Step 4（insert-db），或正在执行补偿
  t=2: Phase 4 → Event Source 关闭（已无新事件）
  t=3: Phase 3 → Workflow 状态落盘（Pipeline 不是 Workflow，无状态可落盘）
  t=4: Phase 2 → 插件卸载 → Tool Runner 关闭 → Step 4 / 补偿操作被中途中止
  t=5: Phase 0 → Event Bus 关闭 → Pipeline 执行上下文被销毁
```

**💥 可能后果**：
- **补偿中途中止**：Pipeline 进入补偿后，compensate step 1 成功（delete-db-record），compensate step 2 被关闭中断（cleanup-temp-file 未执行）→ 部分补偿状态，未完全回滚
- **Tool 执行中途中止**：Step 4（insert-db）刚写完数据库但未记录 checkpoint → 数据库已有写入但 Pipeline 认为是失败状态 → 启动后重复执行（如果非幂等则数据重复）
- **没有恢复机制**：Pipeline 不是 Workflow，没有持久化状态。关闭后中断的 Pipeline 不会被恢复或重试
- 对比：Workflow 有关闭前持久化（Phase 3），Event Bus 有关闭前刷盘（Phase 1），但 Pipeline/Skill 执行没有任何关闭前处理

**🛠 建议**：
- 在 §2.5.3 关闭顺序的 Phase 4 和 Phase 3 之间增加一个**排水阶段（Drain Phase）**：

```
Phase 4.5 [排水] ── Pipeline/Skill inflight 执行等待：
    ├── 通知所有活跃 Pipeline/Skill 实例：shutdown 即将到来，完成当前步骤后停止
    ├── 等待正在执行的 Tool 返回（drain_timeout_sec: 30，默认可配置）
    ├── 如果正在进行补偿：等待补偿完成或超时
    ├── 超时仍未完成的实例 → 记录警告日志（含 trace_id + 当前步骤）→ 强制终止
    └── 待重试队列进入停止重试模式
```

- 明确声明：**关闭过程中断的 Pipeline 不会被自动恢复**。如果 Pipeline 的操作有副作用，依赖 Tool 层的幂等性来保证重复安全性
- 对于 mid-compensation 中断，记录详细的结构化日志以便人工恢复时了解"哪步已补偿、哪步未补偿"
- 风险清单新增 #44 项

---

### 🎯 R8 关注点 2：WAL 恢复事件缓冲区大小上限未定义（🟢 低）

**场景**：
§2.5.1 Phase 1 中（lines 80-82）引入了新的 WAL 恢复事件缓冲机制：

```
WAL 重放（Phase 1）产生的恢复事件不进入 Event Bus 主队列，而是暂存在内部缓冲区中
  - Phase 2 完成后，缓冲区中的恢复事件注入 Event Bus → 此时所有处理器已就绪
```

文档未定义：
- 缓冲区大小上限是多少？
- 如果 Phase 2（插件加载）很慢（复杂插件的拓扑排序 + 依赖解析可能耗费数秒），缓冲区会累积多少事件？
- 缓冲区满了怎么办？溢出到磁盘？OOM？阻塞 WAL 重放？

**具体场景**：
- Agent 停机 2 小时后重启
- WAL 中有约 7200 个错过的事件（高频事件源）
- Phase 1 读取 WAL 到缓冲区 → 7200 个事件驻留内存
- Phase 2：10 个插件逐一加载，每个耗时 500ms → 总共 5s
- 缓冲区持有 7200 事件 × ~1KB = 7.2MB → 可能还好
- 但如果停机 24 小时、事件频率更高？或者 WAL 中还有大 payload 事件？

**💥 可能后果**：
- 如果缓冲区是内存队列且有上限，超限后 WAL 重放被阻塞 → Phase 1 永远无法完成
- 如果没有上限 → OOM 风险
- 如果缓冲区溢出到磁盘 → 需要定义溢出路径（但文档没提）
- 如果缓冲区使用与 Event Bus 主队列相同的内存池 → 两个队列争抢内存

**🛠 建议**：
- 在 §2.5.1 的缓冲区描述中增加约束定义：

```
WAL 恢复事件缓冲区约束：
  - 缓冲区大小上限：wal_replay_buffer_max: 5000（可配置，默认 5000）
  - 超限行为：超过上限后，WAL 重放暂停，Phase 1 标记为"部分完成"
    - 已读取的事件正常注入 Phase 2 后的流程
    - 未读取的事件下次启动时从 WAL 断点继续重放
    - 记录告警日志：\"WAL 恢复缓冲区已满，X 个事件将在下次启动时继续重放\"
  - Phase 2 应在合理时间内完成。如果 Phase 2 超时（如 plugin_load_timeout: 30s），
    视为启动失败，触发紧急告警
```

- 或更简化的方案：取消缓冲区，改为"WAL 事件延迟到 Phase 2 完成后再开始重放"——Phase 1 只做检查不加载事件，Phase 2 完成后 Phase 1 再注入
- 风险清单新增 #45 项

---

### 🎯 R8 关注点 3：dedup_key 缺省 payload_hash 对大型 payload 的性能影响未说明（🟢 低）

**场景**：
§3.1 Event 定义中（line 123）：

```
dedup_key: [可选] — 缺省 = (source, type, payload_hash)，用于 30s 窗口去重
```

缺省去重键对 payload 进行 hash。对于包含大型 payload 的事件（如 FILE_CHANGED 携带完整文件内容、大 JSON 数据），`payload_hash` 的计算在入队列时产生显著的 CPU 开销。

**具体场景**：
- FileWatchSource 检测到 100MB 的文件变更 → FILE_CHANGED 事件包含文件路径（而非文件内容），payload 较小 → hash 不贵 ✅
- 但如果某个事件源产生了携带大 payload 的事件（如 WebhookSource 收到的 10MB JSON body）→ 入队时计算 hash → CPU 峰值
- 或者自定义事件源的 payload 包含 base64 编码的二进制数据

**💥 可能后果**：
- 高吞吐场景下，每个入队事件的 payload_hash 计算消耗大量 CPU
- 去重窗口是 30s，但 hash 成本在入队时立即产生，无论最终是否被去重
- 对于 AT_MOST_ONCE 事件（不保证去重），hash 计算是浪费的

**🛠 建议**：
- 在 §3.1 的 dedup_key 描述中增加性能注释：

```
⚠ dedup_key 缺省值为 (source, type, payload_hash)。payload_hash 对大型 payload 有可观的 CPU 开销：
  - 对于已知不会重复的事件，建议显式设置 dedup_key 为轻量字段（如 id 或 source+type+timestamp）
  - 对于 AT_MOST_ONCE 事件，框架应跳过 dedup_key 计算（因为不保证去重）
  - 对于已知唯一来源（如 UUID v7 标识的事件），可设置 dedup_key: <event.id>
```

- 或在框架实现层面：AT_MOST_ONCE 事件直接跳过 dedup_key 计算
- 风险清单新增 #46 项

---

## 修复状态总结

```
R1 问题：10/10 已修复 ✅
R2 问题：8/8  已修复 ✅
R3 问题：8/8  已修复 ✅
R4 问题：5/5  已修复 ✅
R5 问题：5/5  已修复 ✅
R6 问题：4/4  已修复 ✅
R7 问题：3/3  已修复 ✅
R8 新发现问题：3 项
  - 关闭序列未覆盖 Pipeline/Skill inflight 执行和补偿 (🟡 中)  → 新 #1
  - WAL 恢复事件缓冲区大小上限未定义 (🟢 低)                  → 新 #2
  - dedup_key payload_hash 大型 payload 性能影响未说明 (🟢 低) → 新 #3
```

---

## 实施建议优先级

```
P1（上线前必须解决）
  └── 关闭序列未覆盖 Pipeline/Skill inflight（新 #1）
       └── 补偿中断 → 部分回滚 → 不可恢复
           Pipeline 不是 Workflow，无持久化状态兜底

P2（beta 前建议解决）
  ├── WAL 恢复缓冲区上限（新 #2）
  │    └── 新引入机制（R7 修复）自身的边界未定义
  │        大量 WAL 事件 + 慢插件加载 → OOM 或启动卡死
  └── dedup_key payload_hash 性能（新 #3）
       └── 大型 payload 下入队 O(n) hash 成本
           AT_MOST_ONCE 事件的 hash 完全浪费
```

---

## 最终评价

**R7 的全部 3 个问题已被认真修复。** 启动序列的定义（6 阶段 + 安全约束 + 就绪探针 + 反向关闭）是七轮审计中结构最完整的新增章节。Priority 与保序冲突的决策 9 每条都有理由、场景、代价、缓解——这是设计文档成熟度的标志。补偿 retry 参数的 `retry_count: 3` + `retry_backoff: "exponential"` 补充了图中所示与合同所定义的缺口。

R8 发现的 3 个问题集中在**新引入机制的自身边界**和**机制间的衔接处**：

- **#1 号（关闭序列的 Pipeline 排水缺失）**是§2.5 这面新增的"盾牌"上唯一的缝隙。启动序列很完整，关闭序列覆盖了 Event Source、Workflow、WAL、Event Bus，但**漏了最活跃的执行层**——Pipeline 和 Skill。在"源已停止"和"组件已卸载"之间，如果还有一个正在做补偿的 Pipeline，它会被硬切断。
- **#2 号（WAL 恢复缓冲区上限）**是 R7 新引入的缓冲机制自身的边界——文档说"暂存缓冲区"，但没定义"装不下怎么办"。这是 R1 风格的"边界未定义"问题，但在文档第 8 轮才出现，说明新引入的机制仍然需要自己的边界定义。
- **#3 号（dedup_key 性能）**是性能维度的第一个审计发现——前七轮都在逻辑正确性和安全性，没有触及性能影响。

```
文档迭代成熟度趋势（R1→R8）：
                    R1     R2     R3     R4     R5     R6     R7     R8
  功能完整性       10✅    0      0      0      0      0      0      0
  防御的防御        0     8✅     0      0      0      0      0      0
  恢复的恢复        0     0     8✅     0      0      0      0      0
  并发与时序        0     0     0     5✅     0      0      0      0
  逻辑闭环自洽      0     0     0     0     5✅     0      0      0
  机制交互盲区      0     0     0     0     0     4✅     0      0
  生命周期+权衡    0     0     0     0     0     0     3✅     0
  新机制边界+性能  0     0     0     0     0     0     0     3项
```

八轮下来，46 项问题关闭，3 项新发现。新问题数量持续减少（10→8→8→5→5→4→3→3），新问题的性质从"功能缺失"进化到"防御纵深"到"交互盲区"到"生命周期"到"性能提示"。文档已经进入了一个非常成熟的阶段——剩下的问题不是"哪里没做"，而是"新做的地方边界在哪里"。
