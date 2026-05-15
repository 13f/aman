# LLM 对话能力设计 — 业务逻辑审计 (R2)

**审计范围**：/Users/jerin/projects/aman/docs/llm-chat-design.md (1206 行)
**轮次**：第二次审计
**方法**：恢复的恢复 (Pass 3) + 并发与时序 (Pass 4) + 非对称边界 (Pass 5) + 新机制边界 (Pass 8)

---

## 跨引用表：R1 发现 → 当前版本

| # | R1 发现 | 严重度 | 状态 | 证据 |
|---|---------|--------|------|------|
| 1 | Workflow 缺少 ERROR 态 | 🔴 | ✅ 已修复 | §6 新增 ERROR/RETRYING 状态，含完整转移边和约束条件 |
| 2 | 并发消息冲突 | 🔴 | ✅ 已修复 | §4 新增"并发策略：会话级串行，跨会话并行"，含队列深度和 MESSAGE_ENQUEUED 事件 |
| 3 | /edit 历史篡改语义 | 🔴 | ✅ 已修复 | §14.11 新增 /edit 精确语义（replace/branch 双模式 + 审计日志） |
| 4 | 流式与 tool_call 交错渲染 | 🟡 | ✅ 已修复 | §14.4 新增 position_hint 字段和插入时机规则表 |
| 5 | 断线恢复事件丢失 | 🟡 | ✅ 已修复 | §10.1 新增断线重连恢复协议、GET /session/{id}/state API、WAL 持久化 |
| 6 | 双重输入消毒不一致 | 🟡 | ✅ 已修复 | §9.1 明确"服务器端是唯一安全屏障"，§14.8 补充安全角色声明 |
| 7 | 历史裁剪脱节 | 🟡 | ✅ 已修复 | §14.9 新增 HISTORY_TRIMMED 事件 + 裁剪策略语义表 |
| 8 | WAITING_INPUT 悬态 | 🟡 | ✅ 已修复 | WAITING_INPUT 已从状态机中移除 |
| 9 | 共享会话多用户竞态 | 🟡 | ✅ 已修复 | §14.3 新增乐观锁机制 + shared-sub 降级方案 |
| 10 | SOUL 热更新跨 Tool Calling 边界 | 🟢 | ✅ 已修复 | §8 新增"热更新生效边界"和版本快照规则 |
| 11 | /retry 重放边界 | 🟢 | ✅ 已修复 | §14.11 新增 /retry 精确语义（方式①/② + --full 标志） |
| 12 | /stop 中断语义 | 🟢 | ✅ 已修复 | §14.11 新增 /stop 精确语义（流式/Tool 两种场景 + 状态转移） |

**R1 修复质量评价**：全部 12 个发现均被解决，且新增内容的详细度超过了 R1 的建议范围（不仅包括约束级定义，还包含了 JSON 事件结构、页面交互细节）。文档质量显著提升。

**但修复引入了新的边界条件**（Pass 8 检查），以下 R2 发现中包含对这些新机制的深度审查。

---

## R2 发现

### Pass 3 — 恢复的恢复：防御机制自身的失效

---

### 🎯 R2-1: WAL 重放的去重防护缺失 — 重复消息可被重新注入事件总线（🔴）

**场景**：Agent 在处理一条 `MESSAGE_RECEIVED` 时崩溃。WAL 中已写入该事件（§10.1：入 Event Bus 前写 WAL），但 Event Bus 尚未 ACK 消费。重启后 WAL 重放该事件 → LLM Skill 重新处理 → 用户收到两次回复。

**💥 可能后果**：
- WAL 的写入时机是"入 Event Bus 前"——如果崩溃在事件已分发给 LLM Skill 之后、Event Bus ACK 回写 WAL 之前发生 → WAL 重放会**重复投递**
- LLM Skill 处理了重复消息 → 可能产生两笔 Tool 调用（如"支付订单"被调用两次）
- 对端 LLM Provider 在流式场景中可能产生双倍 token 消耗 + 双倍计费
- 乐观锁只能保护共享会话的状态历史写入（§14.3），无法阻止 LLM Skill 本身的重复处理

**🛠 建议**：
- 为每条 `MESSAGE_RECEIVED` 事件携带**全局唯一 ID**（event_id / message_id），在 LLM Skill 入口做**幂等检查**：如果该 event_id 已经被处理过（在 State Store 中有对应回复记录），跳过处理
- WAL 重放时增加**重放标记** `replay: true`，下游组件可根据标记决定是否重新执行副作用操作
- 或采用**二阶段提交**：WAL → Event Bus 分发 → 消费后标记 WAL 为已 ACK（需要 WAL 支持确认状态）

---

### 🎯 R2-2: 乐观锁 3 次重试耗尽后的兜底缺失（🟡）

**场景**：共享会话（§14.3）中，多个渠道同时高并发写入。乐观锁连续 3 次 version 冲突，重试耗尽。

**💥 可能后果**：
- §14.3 定义了乐观锁和"最多 3 次，指数退避"，但没有定义**3 次失败后的行为**
  - 是静默丢弃消息？（用户发送了消息但未收到回复）
  - 是返回 409 给页面？（页面需要显示什么？）
  - 是自动降级为 shared-sub 模式？（但 shared-sub 需要会话创建时配置，不是运行时动态切换）
  - 是写入失败队列人工处理？（但用户交互是实时的）
- 重试退避期间，用户页面上的消息可能已经显示为 enqueued 或 sent 状态，但最终是写入失败 → **无声的数据丢失**

**🛠 建议**：
- 定义乐观锁重试耗尽后的**最后救助手段**：
  - **自动降级**：将当前写入请求降级为 shared-sub 模式（以 `session_id:{channel}:{user}` 独立存储，UI 层融合），不影响用户交互
  - **失败通知**：如果自动降级不可行，向页面返回明确的错误消息"会话写入冲突，请稍后重试"，消息不回滚
- 增加可观测性指标：`session_lock_contention_count`，当频繁冲突时，运维可决策是否切换为 shared-sub 配置

---

### 🎯 R2-3: OutputValidator 自身失效 — 崩溃/超时时 failing open 还是 failing closed？（🔴）

**场景**：OutputValidator 内部发生 crash，或 LLM 回复内容过大导致验证超时。

**💥 可能后果**：
- 文档定义了 OutputValidator "拦截违规回复"的行为（§9.2），但**没有定义 Validator 自身崩溃时的行为**
  - **Fail open**：Validator 挂了，LLM 回复直接通过 → 绕过所有输出安全检查 → Secret 泄漏/系统提示泄漏
  - **Fail closed**：Validator 挂了，所有回复被拦截 → 用户体验为"Agent 不说话"的完全不可用
  - 两种选择都有严重代价，但文档未明确选择哪一种
- Validator 超时：如果回复很长（如 4096 tokens），Validator 的正则匹配可能在 5 秒内无法完成的场景未定义

**🛠 建议**：
- 明确 OutputValidator 的**失效策略**：
  - **推荐 fail closed**：Validator 不可用时，所有回复被阻止，页面显示"安全检查组件异常，请联系管理员"
  - 故障时，触发审计告警（Alert severity: critical），立即通知运维
  - 增加 Validator 健康检查端点，用于 Deployment 的 readiness probe
- 为 Validator 定义**超时阈值**（如 2 秒），超过后视为验证失败（fail closed），而非无限等待

---

### 🎯 R2-4: 会话级队列溢出后的静默丢弃 — 用户感知断裂（🟡）

**场景**：用户连续快速发送 11 条消息进入同一会话。第 11 条超过 §4 定义的队列深度（10 条/会话），被"丢弃"。

**💥 可能后果**：
- §4 定义队列容量 10 条，超过后"返回'消息被丢弃'错误"——但问题在于是**谁返回这个错误**
  - 如果丢在 Event Bus 入口 → 页面仍然显示消息已发送（因为 page 的 WebSocket 发送成功了），但推送的 MESSAGE_ENQUEUED 事件中没有这个消息 → **无声丢失**
  - 如果丢在 ChatPlatformSource 层（在产生 MESSAGE_RECEIVED 之前拦截）→ 页面应在发送时就获得错误响应
  - 文档未定义这个错误是在哪一层、以何种方式到达页面
- 队列中的前 10 条消息在排队期间，用户可能已经关标签页或超时放弃 → **消费时上下文已无效**

**🛠 建议**：
- 明确队列溢出的**检测层和反馈路径**：
  - ChatPlatformSource 在构造 MESSAGE_RECEIVED 事件前检查队列深度 → 如果已满，同步返回"发送失败：当前会话队列已满"到页面（WebSocket error 或 HTTP 429）
  - 如果 MESSAGE_RECEIVED 已经入 Event Bus，但 Dispatcher 发现队列深度溢出 → 丢弃并发布 `MESSAGE_DROPPED` 事件到页面
- 增加配置项 `queue_overflow_strategy`：`drop`（默认，丢弃新消息）| `preempt_oldest`（丢弃队列中最旧的未处理消息，为新消息让位）

---

### Pass 4 — 并发与时序：竞态条件和时间窗口

---

### 🎯 R2-5: /retry 与并发到达的新消息的时序竞态 — 两条操作路径冲突（🟡）

**场景**：用户在 IDLE 状态快速执行两个操作：`/retry`（重试上一次回复）和一条新的自然语言消息（MESSAGE_RECEIVED），其中第二个操作在第一个操作尚未完成时发出。

**💥 可能后果**：
- /retry 导致 Workflow 从 IDLE → PROCESSING（重试中）。在新消息到达时，两种可能：
  - **新消息进入会话队列**（正确的按 §4 行为），但队列中排在"正在重试"的后面 → 用户看到自己的消息在排队等待重试完成，体验奇怪
  - **新消息合并到重试上下文**——LLM Skill 将新消息作为"在重试基础上继续对话"来处理（隐含假设上下文已包含新消息），但重试是基于旧消息重新生成的——新消息和旧消息混在一起，LLM 生成混乱的回复
- 文档的 §4 并发规则说队列针对 MESSAGE_RECEIVED，但 /retry 产生的是 SESSION_CMD 事件（§14.11）——SESSION_CMD 是否也会进入队列？如果 SESSION_CMD 不排队（跳过队列直接执行），则 SESSION_CMD 可能领先于排队的 MESSAGE_RECEIVED，造成顺序反转

**🛠 建议**：
- 定义 **SESSION_CMD 的队列行为**：
  - 与 LLM 调用无关的命令（如 /session list、/help、/debug）→ 跳过队列，立即执行
  - 涉及 LLM 调用的命令（如 /retry、/edit）→ 进入队列，与 MESSAGE_RECEIVED 同队列同顺序
  - /stop → 特殊处理：无需排队，直接注入 PROCESSING 态的取消信号
- 或更简单的设计：队列中 SESSION_CMD 和 MESSAGE_RECEIVED 共用 FIFO，所有操作按到达顺序处理

---

### 🎯 R2-6: /stop 与 LLM_STREAM_DONE 同时到达的竞态（🟡）

**场景**：用户点击 /stop 的同时，LLM 刚好完成了最后一段文本生成并发出 LLM_STREAM_DONE 事件。两个事件在 Event Bus 中交错。

**💥 可能后果**：
- §14.11 定义 /stop 后"Workflow 从 PROCESSING → IDLE"，而 LLM_STREAM_DONE 到达后也是 "PROCESSING → IDLE"（§6）。如果两个事件同时到达：
  - Event Bus 处理顺序不确定 → Workflow 可能先收到 LLM_STREAM_DONE（标记为 completed），然后收到 /stop（标记为 interrupted）→ 状态自相矛盾
  - 或先收到 /stop（标记为 interrupted），后收到 LLM_STREAM_DONE（LLM 回复已经完成，内容是完整正确的）→ 用户看到"已中断"标签，但实际回复是完整的
  - 页面上的消息同时有 "interrupted" 和 "completed" 两个属性 → 渲染不确定性

**🛠 建议**：
- 定义**竞态解决规则**：
  - LLM_STREAM_DONE 比 /stop "更重"：如果 /stop 和 LLM_STREAM_DONE 在同一个事件循环中到达（差 < 100ms），以 LLM_STREAM_DONE 为准，标记为 completed
  - 提供明确的**仲裁窗口**：当 /stop 信号发出时，给当前流式请求 500ms 的"缓存窗口"，如果在这个窗口内收到 LLM_STREAM_DONE，视为正常完成；超过窗口未收到，视为已中断
- 或采用**无竞态设计**：/stop 发送后，LLM Skill 对当前流式请求发出 cancel 后**等待** Provider 确认取消（而不是立即认为已终止），在确认后再切换状态

---

### 🎯 R2-7: 双标签页同会话的并发路径绕过队列（🟡）

**场景**：用户在浏览器中打开了两个标签页，连接到同一个 session_id。Tab A 发送一条消息 → MESSAGE_RECEIVED → LLM Skill 开始处理。与此同时 Tab B 发送另一条消息。

**💥 可能后果**：
- §4 的会话级队列假设所有消息通过**同一个** ChatPlatformSource/Dispatcher 路由。但两个标签页各自有独立的 WebSocket 连接 → 两条 MESSAGE_RECEIVED 从不同的路径进入 Event Bus
- Dispatcher 需要按 session_id 聚合路由到 LLM Skill。如果 Dispatcher 没有 session-level sharding，两个 MESSAGE_RECEIVED 可能被不同的 Worker 消费 → **两条消息的 MESSAGE_RECEIVED 并行处理同一个 session**，直接违反 §4 的"会话级串行"规则
- 结果：历史写入竞态（两条回复写入同一 session 的 data.history），工具调用混乱，最终回复可能混合两个问题的答案

**🛠 建议**：
- Dispatcher 或 LLM Skill 必须实现 **session-level 分布式锁**（或 actor 模型），确保同一 session 的多个 MESSAGE_RECEIVED 不会同时被处理
- 如果使用 actor 模型：每个 session 映射到一个 actor，所有该 session 的消息路由到同一个 actor，actor 内部串行处理
- 在 §4 的并发策略中补充："即使是来自不同连接的同一条会话，也必须保证全局串行——Dispatcher 应在路由阶段对 session_id 做分片（consistent hashing），同一 session 始终路由到同一个 Worker"

---

### 🎯 R2-8: cost_aware 路由策略的共享预算计数器竞态（🟢）

**场景**：两个并发会话同时触发 cost_aware 路由策略（§5），各自评估请求复杂度后决定使用高成本模型（如 GPT-4）。Agent 配置了"每分钟最多 10 次 GPT-4 调用"的预算。

**💥 可能后果**：
- 两个会话的路由决策同时读取剩余预算计数器 → 都看到"剩余 5 次" → 都选择 GPT-4 → 两个都调用成功 → 预算计数器从 5→3（减少 2），但实际消耗了 2 次预算，符合预期
- 但更危险的场景：剩余预算为 1，两个会话同时读取到 1 → 都选择 GPT-4 → 各自尝试调用 → 实际执行了 2 次 GPT-4 调用 → 预算超支 1 次
- 如果预算绑定到计费账户（如 OpenAI API 月度限额），超支可能导致 API 被限或产生意外费用

**🛠 建议**：
- 路由决策必须使用**原子操作**：读取剩余预算 + 预估下一次消耗 → 决策 → 扣减预算，整个操作为一个原子事务
- 或：路由决策后增加**二次确认**——在选择模型后，检查当前剩余预算是否仍然充足，如果不足则降级到低成本模型
- 记录 `routing_decision` 到审计日志，包含：session_id、selected_model、budget_before、budget_after、decision_reason

---

### Pass 5 — 非对称边界：渠道/模式间的隐式假设

---

### 🎯 R2-9: CLI 渠道的防御空白 — stdin 关闭、stdout 管道断裂、SIGPIPE（🟡）

**场景**：用户通过 CLI 终端使用聊天。终端窗口被关闭，或 stdout 被重定向到 `head -n 5`（读取前 5 行后关闭管道），或 stdin 被 EOF 关闭。

**💥 可能后果**：
- §10 定义了 CLI 渠道使用"读取 stdin / 输出 stdout"，但：
  - **stdin 关闭**：CLI 模式下 stdin 是 `MESSAGE_RECEIVED` 的唯一来源。如果 stdin 被 EOF（管道输入结束、CTRL+D），ChatPlatformSource 没有定义如何检测和恢复——是重试打开 stdin？还是优雅退出？
  - **stdout 管道断裂**：LLM 回复写入 stdout 时，若下游管道（`| head` 等）关闭了 stdout → SIGPIPE 信号（除非 Agent 处理该信号，否则进程终止）
  - **无断线恢复**：WebSocket 渠道有完整的断线重连协议（§10.1）和 WAL 持久化，但 CLI 渠道什么都没有——如果 CLI 进程被 SIGTERM 杀死，当前正在处理的 LLM 回复丢失，且没有 WAL 重放
  - **会话持久化不对称**：WebSocket 的会话可以通过 State Store 恢复；CLI 的 stdin 流式输入无 session 概念——重启后无法恢复之前的 CLI 会话

**🛠 建议**：
- 为 CLI 渠道增加**信号处理**：SIGPIPE → 优雅关闭写入流但不终止进程（使用信号掩码或 `write` 替代 `printf`）；SIGTERM → 在终止前将当前交互单元写入 State Store 作为 checkpoint
- 明确声明 CLI 渠道是**尽力而为交付**（best-effort），不保证断线恢复和会话持久化（与 WebSocket 的 RPO/RTO 不同）
- 或者在 CLI 渠道中分配持久 session_id（基于终端名称或 PID 哈希），支持 State Store 恢复

---

### 🎯 R2-10: ad-hoc 会话的 WAL 残留与重启重放冲突（🟢）

**场景**：用户发起一个 ad-hoc 会话（一次性问答），发送消息后获得回复，会话被关闭（"关闭即丢弃"）。Agent 重启。WAL 中仍然有该会话的 MESSAGE_RECEIVED 事件（TTL 24 小时才清理）。

**💥 可能后果**：
- §10.1 的 WAL 持久化对所有 ChatPlatformSource 事件生效，不区分会话类型。ad-hoc 会话的事件也写入 WAL
- Agent 重启后 WAL 重放 → 尝试将 MESSAGE_RECEIVED 注入 Event Bus → LLM Skill 收到重放请求 → 但会话已关闭（State Store 中没有对应 session 或状态为 CLOSED）
- LLM Skill 收到一个已经关闭的会话的消息：是拒绝处理（session CLOSED），还是创建新会话（产生意料之外的回复）？
- §6 的状态机中 CLOSED 是终态，没有定义"CLOSED 状态下收到 MESSAGE_RECEIVED"的转移——**WAL 重放可能突破状态机边界约束**

**🛠 建议**：
- WAL 重放前应检查会话状态：如果会话已 CLOSED 或不存在，跳过该事件的 WAL 重放
- 或在 §6 状态机中增加转移：CLOSED → ACTIVE（事件：MESSAGE_RECEIVED），此时创建一个新的会话实例（但需要告知用户这是一个新会话）

---

### 🎯 R2-11: 不同渠道的事件顺序保证不一致（🟢）

**场景**：用户在 Slack 渠道发送消息，同时在桌面端 WebSocket 渠道发送另一条消息，都指向同一个共享 session_id。

**💥 可能后果**：
- §13 说"同源保序：同一用户的 MESSAGE_RECEIVED 按到达顺序处理"——但**不同渠道的消息到达顺序不可保证**
  - Slack 消息经过 webhook → 可能比本地 WebSocket 消息延迟 200-500ms
  - 用户可能在 Slack 上发了"查北京天气"，然后在桌面端发了"查上海天气"——两条消息在 Event Bus 中到达的顺序与用户操作的顺序可能相反
  - §4 的会话级队列按 Event Bus 到达顺序处理，而不是按用户操作时间
- 用户预期是"先北京后上海"，但 LLM 实际处理是"先上海（桌面端）后北京（Slack 延迟到达）"→ **回复错位**

**🛠 建议**：
- 在 MESSAGE_RECEIVED payload 中增加 `client_timestamp`（用户操作时的本地时间），结合 `server_arrival_timestamp`（Event Bus 到达时间）做**有序仲裁**
- 仲裁规则：如果两条消息的 `client_timestamp` 差 > 网络延迟阈值（如 5 秒），以 `client_timestamp` 为准；如果差 < 阈值，以 `server_arrival_timestamp` 为准
- 这个规则应在 §4 的并发策略中明确定义

---

### Pass 8 — 新机制边界：R1 修复引入的新机制自身的边界条件

---

### 🎯 R2-12: MESSAGE_ENQUEUED 的 estimated_wait_ms 估算不可靠 — 虚期望（🟡）

**场景**：用户发送一条消息，页面显示 "MESSAGE_ENQUEUED: position 2, estimated 15s"。但当前正在处理的第一条消息触发了 Tool Calling 循环（3 个工具，每个耗时 4-8 秒），加上 LLM 二次生成耗时，总耗时 45 秒。

**💥 可能后果**：
- §4 的 MESSAGE_ENQUEUED 事件包含 `estimated_wait_ms` 字段。但这个估算需要知道：
  - 当前正在处理的消息还剩多少时间（Tool Calling 循环长度不可预测）
  - 队列中前面各条消息的预估处理时间
  - LLM Provider 的延迟方差（GPT-4 的 p50/p99 延迟可能相差 3-5 倍）
- 如果估算值为 15s 但实际为 45s → 用户在第 16 秒开始困惑（"为什么还没好？"），用户体验比不估算更差
- 实现者面临困境：是低估（用户期望高估）还是高估（用户提前放弃）？

**🛠 建议**：
- 移除 `estimated_wait_ms`，改为 `queue_position` 和一个**阶跃式指示器**：
  - position 1 → "当前正在处理你的消息"
  - position 2-3 → "前面还有 {N} 条消息"
  - position 4+ → "队列中有多条消息等待处理"
- 或者使用更保守的估算模型：基于该会话历史中的 LLM 调用的 p95 延迟作为基准
- 增加**队列等待计时器**：如果队列中等待超过 60 秒没有开始处理，发布 `QUEUE_STALLED` 事件到页面（可能 LLM Skill 挂了）

---

### 🎯 R2-13: /edit 的 IDLE 约束与会话级队列的矛盾（🟢）

**场景**：当前会话处于 IDLE 状态（上一条回复已完成），但会话级队列中仍有 3 条 pending 的 MESSAGE_RECEIVED（用户在 LLM 处理期间输入了多条，排在了队列里）。用户执行 `/edit` 编辑历史消息。

**💥 可能后果**：
- §14.11 要求 /edit 必须在 `"当前会话状态为 IDLE"` 下执行。会话当前是 IDLE（没有活跃的 PROCESSING），但队列中有消息
  - /edit 清除了历史中从编辑点之后的所有消息（包括已处理的和尚未处理的）
  - 队列中的 pending MESSAGE_RECEIVED **已经被放入 Event Bus**，不在 Workflow 的历史中
  - /edit 替换了历史 → LLM Skill 重新处理 → 但队列中还有旧消息，处理完成后队列中的旧消息会继续被处理 → **旧队列消息在编辑后的新上下文中执行** → 回复的内容基于不存在的上下文
  - 或者编辑创建了新的历史上下文 → 队列中的旧消息 arrival 进入路由，而编辑后的新上下文也产生新的回复 → 两种回复来自同一个 session，互相覆盖

**核心问题**：/edit 清除的是 Workflow 历史 + State Store，但**不能取消已经在 Event Bus 队列中或流过程中的消息**。Event Bus 和 Workflow 状态之间没有同步。

**🛠 建议**：
- /edit 的执行条件应扩展为：**会话状态为 IDLE 且会话级队列为空**（没有 pending 的 MESSAGE_RECEIVED）
- 如果队列非空，页面应提示"当前会话有未处理的消息，请等待完成后编辑"
- 或在 /edit 时同时清空会话级等待队列（发布 `MESSAGE_CANCELLED` 事件通知页面哪些消息被丢弃）

---

### 🎯 R2-14: HISTORY_TRIMMED 事件在 WAL 恢复后的重放语义（🟢）

**场景**：会话中触发了 HISTORY_TRIMMED 事件（在 LLM 调用前裁剪了历史）。Agent 重启。WAL 恢复。

**💥 可能后果**：
- HISTORY_TRIMMED 事件**是否进入 WAL**？文档没有定义
  - 如果 HISTORY_TRIMMED 进 WAL → WAL 重放后，页面收到裁剪事件 → 但 State Store 的完整历史不受影响（§14.9），页面灰化了 25 条消息，但实际上这些消息在 State Store 中是完整可用的——WAL 重放后，页面的裁剪状态和 State Store 的完整历史不一致
  - 如果 HISTORY_TRIMMED 不进 WAL → 重启后页面通过 GET /session/{id}/state 拉取完整历史 → 所有消息都正常显示（没有裁剪）→ 但 LLM Skill 在下一次调用时仍然会在裁剪规则下重新裁剪 → 用户看到的"未裁剪"只是暂时的

**🛠 建议**：
- HISTORY_TRIMMED 不应进入 WAL，它是一次性的 UI 提示事件。重启后页面应通过 GET /session/{id}/state 获得完整历史，然后 LLM Skill 在下次调用时重新决定裁剪
- 或：在 GET /session/{id}/state 中增加 `trim_info` 字段（非必须），让页面知道哪些消息在当前 LLM 上下文中不可见

---

### 🎯 R2-15: /retry --full 的幂等性依赖人工声明 — 系统级强制缺失（🟡）

**场景**：Agent 管理员配置了一个 Tool `send_email` 为非幂等（不可重放）。用户执行 `/retry --full`，触发完整重放流程，LLM 再次调用 `send_email`。

**💥 可能后果**：
- §14.11 的 /retry --full 要求"需要相关的 Tool 是幂等的（由 Agent 管理员声明）"
  - 但**谁检查这个声明**？如果管理员误声明了一个非幂等的 Tool 为幂等，或忘记声明某个非幂等 Tool，/retry --full 会**重复执行所有副作用**
  - 幂等声明只在文档中写了一句"由 Agent 管理员声明"，没有运行时校验
  - 系统没有机制验证一个 Tool 是否真的幂等——这是业务语义，无法自动验证
  - 如果 `send_email` 被调用两次 → 收件人收到两封相同的邮件

**🛠 建议**：
- 在 Tool 注册接口中增加**幂等性标记**字段：
  ```rust
  struct ToolDescriptor {
      name: String,
      idempotent: bool,  // 默认为 false（非幂等）
      // ...
  }
  ```
- /retry --full 的运行时检查：如果涉及的任何 Tool（在重放路径中的 Tool）的 `idempotent == false`，返回错误"无法执行完整重放：包含非幂等 Tool，建议使用默认重试模式"
- 这需要跨 LLM Provider + Test Runner 两层的整合，但让 "声明" 变成可执行的约束

---

## 优先级矩阵

```
P0（阻止上线）
  └── R2-1: WAL 重放后重复投递 — 缺少去重防护（最终一致性与幂等性）
  └── R2-3: OutputValidator 自身崩溃 — fail open/closed 未定义（安全风险）

P1（上线前必须解决）
  └── R2-4: 队列溢出后静默丢弃 — 用户感知断裂
  └── R2-5: /retry 与新消息的时序竞态 — SESSION_CMD 队列行为未定义
  └── R2-6: /stop 与 LLM_STREAM_DONE 竞态 — 仲裁规则缺失
  └── R2-7: 双标签页同会话并行绕过队列 — Dispatcher 无 session 级分片
  └── R2-13: /edit 的 IDLE 约束与队列中消息冲突

P2（beta 前建议解决）
  └── R2-2: 乐观锁重试耗尽后兜底缺失
  └── R2-9: CLI 渠道防御空白 — SIGPIPE/stin EOF 未处理
  └── R2-12: estimated_wait_ms 估算不可靠 — 建议取消或替换
  └── R2-15: /retry --full 幂等性依赖人工声明 — 无系统级强制

P3（持续改进）
  └── R2-8: cost_aware 共享预算计数器竞态
  └── R2-10: ad-hoc 会话的 WAL 残留重放冲突
  └── R2-11: 跨渠道事件顺序仲裁规则
  └── R2-14: HISTORY_TRIMMED WAL 恢复语义
```

---

## 总评

**文档成熟度进步**：R1 到 R2 的提升幅度非常大。R1 的 12 个缺陷全部修复，新增内容的详细度超过了预期。文档在"功能完备性"（Pass 1）层面已经做得很好。

**R2 核心发现模式的变化**：与 R1 不同，R2 的缺陷不再是"缺少了什么"，而是**"已有机制的边界条件未定义"**和**"多个工凑间的竞态交互"**——这正是文档成熟度提升的标志：从"缺什么"到"有了但不够精确"。

**三个最危险的残余问题**：

1. **WAL 重放的去重缺失**（R2-1）— 这是 R1 修复引入的新机制（WAL 持久化）自身没有处理的问题。WAL 保证了"消息不丢"，但同时也可能"重复投递"。去重防护的缺失意味着：任何 10.1 说的 WAL 恢复路径中，Agent 重启可能导致用户收到重复回复，甚至重复 Tool 调用。这是系统中最隐蔽的"数据完整性"风险。

2. **OutputValidator 自身的失效模式**（R2-3）— 所有输出安全依赖这个单一组件，但其自身的崩溃行为（fail open/closed）没有定义。这是一个安全审计中经典的"信任最后一公里"问题——防御机制自身的失败需要被防御。

3. **多个半独立的事件路径在并发下的竞态**（R2-5, R2-6, R2-7）— 双标签页绕过队列、/stop 与 stream_done 的竞态、SESSION_CMD 与 MESSAGE_RECEIVED 的队列交互——这些都是并发规模增加到一定程度后必然出现的问题，但每个竞态的仲裁规则都没有定义。

**建议**：如果下一轮审计继续，重点应该放在 **Pass 6（内部一致性——两个独立正确但互斥的规则）** 和 **Pass 9（实现粒度——当前文档中哪些定义在实现层面会产生歧义）**。
