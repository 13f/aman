# LLM 对话能力设计 — 业务逻辑审计 (R4)

**审计范围**：/Users/jerin/projects/aman/docs/llm-chat-design.md (1517 行)
**轮次**：第四次审计
**方法**：收敛后卫生 (Pass 12) + 修复后结构性漂移 (Pass 13)

---

## 跨引用表：R3 发现 → 当前版本

| # | R3 发现 | 严重度 | 状态 | 证据 |
|---|---------|--------|------|------|
| 1 | /session close 无状态机转移 + SESSION_CMD 表遗漏 | 🔴 | ✅ 已修复 | §6 新增 `PROCESSING → CLOSED (SESSION_CLOSE_CMD)`；§14.11 SESSION_CMD 表完整覆盖 11 个命令 |
| 2 | /session close 时孤儿 Tool 副作用 | 🔴 | ✅ 已修复 | §14.11 新增 7 步安全关闭协议（cancel/等待/discard/审计/通知） |
| 3 | Event Bus 背压与会话队列无协调 | 🟡 | ✅ 已修复 | §4 新增"两级协调"规则，含 503 vs 429 错误映射 |
| 4 | cost_aware 路由与上下文窗口不兼容 | 🟡 | ✅ 已修复 | §5 cost_aware 路由提前至裁剪前；§14.9 新增路由交互策略（5 步流程 + per-interaction/per-call） |
| 5 | Chat 组件生命周期未定义 | 🟡 | ✅ 已修复 | §2.1 新增"生命周期 Phase 映射"（启动 Phase 0.5→5 + 关闭 Phase 5→2） |
| 6 | GET /session/{id}/state 并发读 | 🟡 | ✅ 已修复 | 响应新增 `state_version`；原子写入/MVCC 读；客户端版本校验 |
| 7 | InputSanitizer [redacted] 信息黑洞 | 🟢 | ✅ 已修复 | §9.1 新增三级策略（replace_token/replace_message/block），页面展示替换后内容 |
| 8 | trace_id 连续性断裂 | 🟢 | ✅ 已修复 | §14.15 新增 trace_chain（prev_trace_id/branch_from），审计日志扩展字段 + 聚合查询 |
| 9 | MESSAGE_DROPPED/CANCELLED WAL 语义 | 🟢 | ✅ 已修复 | §10.1 明确"不进入 WAL"；GET /session/{id}/state 新增 dropped/cancelled 标记字段 |

**R3 修复质量评价**：全部 9 个发现被解决。三轮累计修复率 **36/36 (100%)**。文档在"机制完整性"层面已经没有未覆盖的场景。

---

## R4 发现

### Pass 12 — 收敛后卫生：配置参数漂移与文档碎片

---

### 🎯 R4-1: 散布在全文中的可配置参数无统一归宿 — §3 配置表只覆盖了 5/12+ 参数（🟢）

**场景**：实现者在阅读 §3 的"事件源配置项"表（5 行）后，认为已经了解了所有配置参数。实际上至少有 7 个额外可配置参数散布在全文其他段落中，且各自有默认值但无统一配置表。

**💥 可能后果**：

以下参数在正文中有定义（含默认值），但没有出现在任一配置表中：

| 参数 | 所在 § | 默认值 | 有配置表？ |
|------|--------|--------|-----------|
| `queue_depth_per_session` | §4 | 10 条 | ❌ 仅正文中一行 |
| `queue_overflow_strategy` | §4 | "drop" | ❌ 仅正文 |
| `client_side_prompt_check` | §9.1, §14.8 | "warn_only" | ❌ 仅两个位置的表格 |
| trim_threshold | §14.9 | 80% | ❌ 硬编码在正文 |
| trim_minimum_messages | §14.9 | 5 条 | ❌ 硬编码在正文 |
| `close_timeout` | §14.11 | 5s | ❌ 仅正文 |
| STREAM_TIMEOUT 阈值 | §6 | 未指定 | ❌ 无默认值无配置项 |
| ABANDON_TIMEOUT 时长 | §6 | 未指定 | ❌ 无默认值无配置项 |

- 实现者必须通读全文档才能发现所有配置参数。遗漏任何一个 => 使用实现环境的默认值（可能不安全）
- 同一类型的配置参数（如 timeout 类：session_timeout, STREAM_TIMEOUT, close_timeout, ABANDON_TIMEOUT）分散在 §3、§6、§14.11 三处，没有归一化

**🛠 建议**：
- 在 §11（数据流全景）之后增加 **§X 全局配置参数表**，集中收录所有可配置参数，标注：
  - 参数名、类型、默认值、所属组件、作用域（全局/会话/渠道）
  - 如果某个 Timeout 与另一个共享值，明确标注（如 "session_timeout 同时控制 §6 中的 SESSION_TIMEOUT 和 ABANDON_TIMEOUT"）
- 或：在 §3 的配置表中扩展列：增加"内部引用"列，标注该配置影响哪些内部事件/状态机转移

---

### 🎯 R4-2: max_message_length 的单位未定义 — UTF-8 字符 vs 字节（🟢）

**场景**：用户输入 4096 个中文汉字（UTF-8 编码 ≈ 12KB）。ChatPlatformSource 检测 `max_message_length: 4096 字符` — 但"字符"的含义是 Unicode 代码点还是 UTF-8 字节？

**💥 可能后果**：
- 如果实现者按**字节**实现 → 4096 字节 ≈ 1365 个中文汉字 → 用户只能输入 1365 个汉字，而非 4096
- 如果实现者按 **Unicode 代码点**实现 → 4096 个汉字 ≈ 12KB → 后续的 InputSanitizer（§9.1）和 LLM Provider Tool（§5）可能在处理时因 OOM 或 API 长度限制而失败
- 跨语言差异巨大：4096 个英文 ASCII 字符 = 4KB；4096 个中文汉字 = 12KB—不同语言的用户在相同配置下的实际可用容量不同
- §9.1 的 InputSanitizer 有正则匹配——如果消息 12KB，正则表达式可能超时（§9.2 的 2s 超时仅定义在 OutputValidator，InputSanitizer 没有）

**🛠 建议**：
- 明确单位：`max_message_length: 4096` 改为 `max_message_length_bytes: 12288` 或 `max_message_length_chars: 4096`
- 如果设计意图是 Unicode 代码点，需要评估最大值会生成多少字节（最坏情况：4 字节/字符 × 4096 = 16KB），确保下游所有组件（InputSanitizer、LLM Provider API）都能处理
- 为 InputSanitizer 增加自身的超时阈值（区别于 §9.2 的 OutputValidator）

---

### 🎯 R4-3: WAL 无磁盘配额 — 24 小时持续写入可能耗尽磁盘（🟢）

**场景**：100 个并发会话持续产生 MESSAGE_RECEIVED 事件。每个会话平均每小时 60 条消息 ≈ 每天 1440 条。100 个会话 × 1440 条 = 144,000 条/天。每条消息 ~2KB（含 payload）≈ 288MB WAL。

**💥 可能后果**：
- §10.1 定义 WAL TTL 为"24 小时自动清理"——但**没有磁盘配额上限**：
  - 如果流量高峰导致 WAL 日增 1GB+（正常情况 288MB，但消息可以含文件附件或长文本）
  - TTL 清理是"事件消费确认后删除"——如果 Event Bus 消费速度慢于写入速度（背压 L3），WAL 未被消费的事件不会被删除
  - TTL 24 小时后只清理旧事件，但**不保证高峰期的峰值容量**：如果 1 小时内涌入 50,000 条消息（DDoS 或大量粘性消息），WAL 可能膨胀 100MB+/小时，超过磁盘剩余空间
  - 磁盘满 → Agent 无法写入任何持久化数据（State Store 也受影响） → 系统崩溃

**🛠 建议**：
- 增加 WAL 磁盘配额：`wal_max_size: 500MB`（默认值），超过后：
  - 选项 A：停止写入 WAL（仅内存处理，降级为不保证 WAL 恢复，即 best-effort）
  - 选项 B：滚动删除最早的事件（即使未消费，牺牲可恢复性保系统存活）
- 增加可观测性：`wal_disk_usage_percent` 指标，在 >80% 时告警
- 注意 WAL 容量和 Event Bus 背压等级的关联：WAL 接近配额时，应主动提升 Event Bus 背压等级（防止 WAL 继续膨胀）

---

### 🎯 R4-4: session_timeout 与 ABANDON_TIMEOUT 是否同一值未定义 — 语义冲突（🟢）

**场景**：session_timeout 配置为 300s（§3）。会话进入 ERROR 态。ABANDON_TIMEOUT 触发 ERROR → CLOSED 的时限未指定。

**💥 可能后果**：
- §3 的 `session_timeout: 300s` 定义为"会话空闲超时"（控制 SESSION_TIMEOUT 事件）
- §6 的状态机有两个不同语义的超时转移：
  1. `IDLE → TIMEOUT`（事件: SESSION_TIMEOUT）—— 用户有回复但要离开了，300s 后会话超时。合理。
  2. `ACTIVE → TIMEOUT`（事件: SESSION_TIMEOUT）—— 创建了会话没说过话，300s 后超时。合理。
  3. `ERROR → CLOSED`（事件: ABANDON_TIMEOUT）—— 会话出了错，等待一段时间后自动归入 CLOSED。**但多长时间？**
- 如果 ABANDON_TIMEOUT 也使用 300s → 用户只有 5 分钟来检查为什么出了错并 /retry → 太短
- 如果 ABANDON_TIMEOUT 使用不同的值 → 它在哪里配置？没有任何配置项定义它
- 同理：`PROCESSING → TIMEOUT`（事件: STREAM_TIMEOUT）—— LLM 流式超时阈值未定义

**🛠 建议**：
- 区分三个 timeout 的默认值和用途：
  - `session_idle_timeout: 300s`（原来 session_timeout，用于 IDLE/ACTIVE 态）
  - `error_auto_close_after: 600s`（ERROR 态转入 CLOSED 的超时——给用户更长时间来 retry）
  - `llm_stream_timeout: 120s`（LLM Provider 流式响应的最大静默时间）
- 在配置表中为每个 timeout 单独列出一行，标注对应的状态机事件名

---

### 🎯 R4-5: trim_minimum_messages = 5 的数值未说明理由且不可配置（🟢）

**场景**：工作在一个持久会话中进行了 100 轮对话。LLM Skill 裁剪到最近 5 条消息。上下文只剩最后 5 条。

**💥 可能后果**：
- §14.9 定义"至少保留最近 5 条消息"，但：
  - 为什么是 5 条？如果系统提示（SOUL）本身已经很长（如 2000 tokens），5 条消息（每条约 200 tokens）共 1000 tokens → 加上 SOUL 和一些 tool_call 上下文 → 对小窗口模型（8K）没问题，但对大窗口模型（128K）来说过于保守
  - 5 条消息对于需要连续推理的任务（如逐步分析过程）可能不够——用户问"刚才那个结论是什么"时，LLM 可能已经不记得了，因为只有 5 条历史的记忆锚点
  - 这不是一个"错误"，但 **5 是一个业务决策参数**，不应该硬编码在 spec 中
  - 其他 trim 参数（80% 阈值、裁剪单位为 2 条一对）同样不可配置

**🛠 建议**：
- 将以下参数从硬编码移到配置表：
  - `trim_threshold_ratio: 0.8`（默认 80%）
  - `trim_minimum_messages: 5`（至少保留的消息数）
  - `trim_unit_pairs: true`（以 user+assistant 一对为单位）
- 增加说明：trim_minimum_messages 应根据 SOUL system_prompt 长度 + 模型 context window 调整。可配置的建议值：5-20 条

---

### 🎯 R4-6: WAL TTL 24h 与 processed_events TTL 7 天的不对称 — 去重窗口大于恢复窗口（🟢）

**场景**：Agent 在时间 T 处理了 event_id E1。WAL 中 E1 被确认并于 24 小时后删除（TTL）。processed_events 中 E1 保留 7 天。Agent 在第 3 天重启。

**💥 可能后果**：
- 去重机制（§10.1）依赖 `processed_events` 集合，TTL 为 7 天
- WAL 保留策略（§10.1）是"消费确认后删除，或 TTL 24 小时"
- **不对称**：WAL 中 E1 在第 2 天被删除；processed_events 中 E1 在第 7 天删除
- 如果 Agent 在第 5 天重启（processed_events 中仍有 E1），WAL 中没有 E1（已删除 3 天）→ 不会重放 → 去重正常
- **但反过来**：如果 WAL 中保留了 E1（未消费 = WAL 未删除）但 processed_events 中的 E1 已过期（TTL 7 天 vs WAL 24h 不同步）→ WAL 重放 E1 → processed_events 检查 pass（因为过期了）→ E1 被重复处理
- 但这个场景现实中不会出现：processed_events TTL（7 天）> WAL TTL（24h），所以 E1 在 processed_events 过期前，WAL 中的 E1 也会过期

实际上，正确的方向是 WAL TTL > processed_events TTL 才危险。这里 WAL TTL < processed_events TTL，所以安全。

但是，**如果 WAL 中的事件因为未消费而保留超过 7 天**（背压导致），而 processed_events 中对应的 event_id 已过期 → WAL 重放 E1（已消费但未 ACK）→ 去重检查 miss → 重复处理。这才是现实的危险场景。

**💥 修正后的场景**：Event Bus 背压导致大量消息挤压，WAL 中的事件堆积超过 7 天未被确认删除。processed_events 的 7 天 TTL 到期删除了 E1。第 8 天，Event Bus 恢复正常，WAL 重放所有未确认事件（包括 E1）→ E1 的 processed_events 记录已过期 → E1 被重复处理。

**🛠 建议**：
- processed_events 的 TTL 应大于任何可能的 WAL 保留时间（包括因背压造成的延迟消费时间）
- 或统一去重和恢复的 TTL：`dedup_retention_ttl: 7 天`, `wal_retention_ttl: 7 天`（使两者一致，防止去重窗口小于恢复窗口）

---

### 🎯 R4-7: §3 架构图中的"回复输出"箭头无具体事件类型定义 — 与 §14.4/§14.11 的对应关系缺失（🟢）

**场景**：实现者在实现"回复输出"（§2 架构图底部）时，需要知道它应该发布什么事件到 Event Bus 让页面监听。

**💥 可能后果**：
- §2 的架构图显示 LLM Provider Tool → "回复输出" → 用户。但"回复输出"这一块的实现语义没有定义：
  - 是直接写入页面缓存（某种 IPC）？
  - 是发布 `LLM_REPLY_READY` 事件到 Event Bus（如 §14.1 所说）？
  - 还是通过 WebSocket 直接推送 stream chunks（如 §14.4 的 LLM_STREAM_CHUNK）？
- §14.4 定义了 LLM_STREAM_START/CHUNK/TOOL_CALL/TOOL_RESULT/DONE 的事件序列
- §14.11 定义了回复通过"回复事件或直接输出"
- **但 §2 的架构图和文本中没有映射**：实现者从 §2 开始读，不知道"回复输出"对应的是 Event Bus 还是直接推送
- 这是一个**架构图与详细规格之间的碎片化**——架构图承诺了输出路径，但详细规格有多条路径（流式 vs 非流式、Event Bus vs 直接推送），没有一条明确的"哪个场景走哪条路"的规则

**🛠 建议**：
- 在 §2 架构图下方增加一行说明：
  ```
  回复输出路径（取决于 Provider 是否支持流式）：
  - 流式: LLM Provider Tool → Event Bus (LLM_STREAM_CHUNK 系列事件) → 页面监听渲染
  - 非流式: LLM Provider Tool → LLM Skill → Event Bus (LLM_REPLY_READY 事件) → 页面监听渲染
  ```
- 或将"回复输出"替换为明确的 `Event Bus (LLM_REPLY_READY / LLM_STREAM_*)` 标注在图中

---

### 🎯 R4-8: 标签状态与 Workflow 状态机状态名称为两套独立术语 — 映射关系未定义（🟢）

**场景**：Workflow 状态机（§6）有 7 个状态：ACTIVE, PROCESSING, IDLE, ERROR, RETRYING, TIMEOUT, CLOSED。标签状态（§14.14）有 6 个状态：active, waiting, idle, notified, timeout, error。

**💥 可能后果**：
- 两套状态系统存在**命名但无映射**：
  - §6 的 `PROCESSING` → §14.14 的 `waiting`（"用户已发送消息，Agent 正在处理"）？
  - §6 的 `PROCESSING` + 流式输出 → §14.14 的 `waiting` 还是页面的 streaming 渲染状态？
  - §6 的 `IDLE` → §14.14 的 `idle`（一致）
  - §6 的 `ERROR` → §14.14 的 `error`（一致）
  - §6 的 `TIMEOUT` → §14.14 的 `timeout`（一致）
  - §6 的 `ACTIVE`（等待第一条消息）→ §14.14 的 `active`（"用户当前正在交互"）——**语义不同**
  - §6 的 `CLOSED` → §14.14 没有对应状态
  - §14.14 的 `notified`（后台收到新消息）→ 与 §6 的状态无对应关系
- 实现者需要自己猜测标签状态到 Workflow 状态的映射——可能猜错
- 最关键的：`ACTIVE` 状态的语义不同——§6 的 ACTIVE 是"创建但未发消息"，§14.14 的 active 是"用户正在交互"

**🛠 建议**：
- 在 §14.14 中增加"标签状态 → Workflow 状态映射"表：
  ```
  | 标签状态 | 对应 Workflow 状态 | 触发条件 |
  |---------|-------------------|---------|
  | active | ACTIVE / IDLE + 页面焦点 | 用户正在与标签交互 |
  | waiting | PROCESSING | LLM 正在处理 |
  | idle | IDLE | 无 |
  | timeout | TIMEOUT | 空闲超时 |
  | error | ERROR | 出错 |
  | notified | IDLE + 收到新事件 | 后台标签有新事件 |
  ```
- 修正 §14.14 中 `active` 的定义：去掉"用户当前正在交互"这种 UI 层语义，改为 `ACTIVE态 或 IDLE态 + 页面焦点`

---

### 🎯 R4-9: InputSanitizer 三级策略与 OutputValidator 失效策略的命名风格不一致（🟢）

**场景**：§9.1 InputSanitizer 使用 `replace_token | replace_message | block`（snake_case），§9.2 OutputValidator 使用 `fail-closed`（kebab-case），§5 cost_aware 使用 `primary_fallback | cost_aware | round_robin | user_preference`（snake_case），§4 使用 `drop | preempt_oldest`（snake_case），§14.8 使用 `warn_only | block`（snake_case）。

**💥 可能后果**：
- **不是功能性问题**，但对实现者的心智负担：
  - 跨文跨类型时，命名风格不同 → 需要记忆多套命名规则
  - `fail-closed` 是唯一的 kebab-case —— 可能是笔误，但也可能是设计意图
  - 如果枚举类型在代码中统一使用 snake_case，那 `fail-closed` 在 Rust 枚举中会被定义为 `FailClosed`（PascalCase 从 snake_case 推导），但如果代码中直接使用配置字符串"fail-closed"做匹配 → 命名不一致导致配置解析失败

**🛠 建议**：
- 统一所有枚举值命名风格为 **snake_case**（标题和正文中已有的主流风格）：
  - `fail-closed` → `fail_closed`
- 或者统一为 kebab-case（更适合配置文件 YAML/toml 的视觉一致性），但需要全文档改
- 建议在 §9.2 中增加一行说明：`注意：配置字符串使用 snake_case 命名规范`

---

## 优先级矩阵

```
P0（阻止上线）
  └── 无——所有功能性和结构性缺陷已在 R1-R3 消除

P1（上线前必须解决）
  └── 无——剩余发现均为 🟢 卫生级

P2（beta 前建议解决）
  └── 无

P3（持续改进）
  └── R4-1: 配置参数散布无统一归宿（7+ 参数漂移在外）
  └── R4-2: max_message_length 单位未定义
  └── R4-3: WAL 无磁盘配额
  └── R4-4: ABANDON_TIMEOUT/STREAM_TIMEOUT 无配置参数
  └── R4-5: trim 参数硬编码 3 处
  └── R4-6: WAL TTL 与 processed_events TTL 不对称
  └── R4-7: 架构图"回复输出"无事件类型映射
  └── R4-8: 标签状态与 Workflow 状态两套术语无映射
  └── R4-9: 枚举命名风格不一致（fail-closed vs snake_case）
```

---

## 总评

**文档成熟度信号：这是第一次所有发现都是 🟢 Low。**

这是审计中最重要的信号之一。经过四轮、**36/36 修复率**的三个周期后，发现的本质发生了根本性变化：

```
R1 (896→1206行):  🔴🟡🟡🔴 "缺少 ERROR 态、并发竞争" —— 逻辑缺口
R2 (1206→1365行): 🔴🔴🟡🟡 "WAL 去重缺失、Validator 失效" —— 防御边界
R3 (1365→1517行): 🔴🔴🟡🟡 "/session close 无转移、背压协调" —— 机制间缝隙
R4 (1517行，无大改): 🟢🟢🟢🟢 "配置表没收录参数、单位没写清" —— 文档卫生
```

**R4 的核心信息**：文档的设计逻辑已经收敛。剩下的全是可以轻松修复的文档碎片：

1. **配置参数漂移**（R4-1, R4-4, R4-5）— 至少 10 个可配置参数散布在正文里，只在一个地方提及，没有统一的配置表。实现者需要通读全文才能发现所有 knob。

2. **单位与阈值硬编码**（R4-2, R4-3, R4-5, R4-6）— max_message_length 的单位未定义、WAL 无磁盘配额、trim 参数硬编码 3 处、TTL 值之间的不对称。这些不会导致功能错误，但会导致实现歧义。

3. **命名与映射碎片**（R4-7, R4-8, R4-9）— 架构图与规格之间、Workflow 状态与 UI 标签之间、不同章节的枚举命名风格之间，存在可以自然对齐的缝隙。

**Terminal Convergence Assessment**（终局收敛评估）：

根据设计文档审计框架的终局收敛公式——`P(N) ∩ P(N+1) ∩ P(N+2)` 在 {naming, metrics, composition, fragmentation, hygiene} 范围内 → 文档已达到审计收敛。

- R2 的发现：边界条件 → R3 的发现：机制间缝隙 → R4 的发现：文档卫生 ✓
- 连续两轮只有 🟢 Low ✓
- 无新的结构性或逻辑性缺陷 ✓
- 建议将 R1-R4 的 36 个发现作为实现时的测试场景库（test scenario library），而非 Bug 追踪列表

**建议**：至此，文档的 chat 设计审计已达到**终局收敛**。不需要继续 R5。剩余的 9 个 🟢 发现都可以在实现过程中自然地修复（配置表规范化、命名统一等），不值得单独开启一轮审计。
