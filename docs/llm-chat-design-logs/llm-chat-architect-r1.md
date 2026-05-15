# llm-chat-architect.md 架构评审 (R1)

> 评审日期：2026-05-13
> 评审人：Project Director
> 评审对象：`/Users/jerin/projects/aman/docs/llm-chat-architect.md`
> 结论：Phase 1 可以开工。Phase 2 前需补齐 6 个缺口。

---

## 完整性评分卡

| 维度 | 得分 | 评价 |
|------|------|------|
| 问题定义 | 9/10 | 核心矛盾清晰，三元对立表干净有力 |
| 设计决策 | 8/10 | 三个决策有理有据，抉择标准可追溯 |
| 状态机 & 并发 | 8/10 | 会话状态机完整，并发模型有背压协调 |
| 安全架构 | 8/10 | Input/Output/Tool/Key 四层齐全 |
| 桥接层 | 7/10 | IPC 接口清单完整，契约表清晰 |
| 恢复 & 持久化 | 7/10 | WAL 重放去重细致，断线重连有协议 |
| 运维 & 可观测 | **3/10** | 仅散落几个指标名，无体系 |
| 测试策略 | **0/10** | 完全缺失 |
| 容量 & 性能 | 5/10 | 有目标数字（100 并发）无推导过程 |
| 实现路径 | 7/10 | 三阶段拆分合理，但 Phase 3 项目过多 |

---

## 已做好的部分

- **§3 会话状态机**：状态转移全覆盖，约束条件具体（5 次重试上限、trace_id 继承、终态补偿路径）
- **§4 并发模型**：session 级串行 + 跨 session 并行 + consistent hashing 分片，设计干净
- **§9 WAL 重放去重**：event_id UUID v7 + processed_events 集合 + 会话 CLOSED 检查，三层防护到位
- **§12 全局配置参数表**：参数、类型、默认值、作用域、说明五列齐全，snake_case 规范明确
- **§5 SOUL 热更新边界**：完整交互单元快照锁定，避免了 system prompt 中途变化的权限/身份跳变

---

## 关键缺口（按严重程度排序）

### 缺口 1：无测试策略 — 阻塞 Phase 2

文档没有任何测试架构说明。对于状态机 + 并发队列 + 事件总线这种组合，测试策略必须在架构阶段确定。

缺失内容：

- 会话状态机的 Property-based Testing 策略（用 proptest 遍历所有合法/非法转移）
- 并发队列的正确性测试（两个 session 消息交错到达，验证串行化）
- Event Bus + WAL 重放的集成测试（模拟 Phase 2 启动时序）
- 前端流式渲染的确定性测试（固定 LLM_STREAM_CHUNK 序列，快照比对 DOM）

建议在 §13（非功能需求）后新增 **§测试架构** 章节。

---

### 缺口 2：可观测性体系缺失 — 影响运维

文档散落了几个指标名（`wal_disk_usage_percent`、`session_lock_contention_count`、queue_stall），但没有体系化。

缺失内容：

- **LLM 调用链追踪**：trace_id 在 §11.6 已定义，但 trace 如何跨 Tool Calling 循环、跨 WAL 重放传递？OpenTelemetry 集成点在哪？
- **关键 RED 指标**（Rate/Error/Duration）：每个 IPC 命令的延迟分布、LLM Provider 调用的首 token 延迟、错误率按 provider 分组
- **健康检查端点清单**：`/health/validator` 已提及，但 ChatPlatformSource、LLM Skill、Tool Runner 的 readiness probe 未定义
- **告警规则**：什么条件触发告警？（OutputValidator fail_closed > 3 次/分钟？queue_stall > 60s？WAL > 80%？）

建议新增 **§可观测性架构** 章节，覆盖：
- 指标（Metrics）：指标名、类型（counter/gauge/histogram）、标签维度
- 追踪（Tracing）：trace context 传播路径（MESSAGE_RECEIVED → Dispatcher → LLM Skill → LLM API → Tool Runner）
- 日志（Logging）：日志级别约定、结构化字段规范
- 告警（Alerting）：告警规则、严重等级、通知渠道

---

### 缺口 3：Rate Limiting 架构空白

§12 有 `rate_limit: 10 条/分钟`，但实现策略完全缺失：

- Token bucket 还是 sliding window？
- 全局还是 per-user？per-session？
- 前端如何处理 429？（UI 倒计时？禁用输入框？）
- 限流状态在 Agent 重启后如何恢复？

建议在 §4（并发与队列模型）中新增 **§4.5 限流模型** 子节。

---

### 缺口 4：Phase 4.5 排水逻辑未定义

§15.1 风险表提到 "Phase 4.5 排水逻辑"，但文档中从未定义 Phase 4.5。这意味着：

- 插件热卸载时 in-flight LLM 请求的取消/等待策略是未知的
- Drain timeout 是多少？与 `session.close_timeout`(5s) 的关系？
- Drain 期间新到达的消息是排队还是拒绝？

建议在 §9.4（插件卸载时的数据安全）中补充排水时序：
```
Phase 4.5 排水流程:
  1. 标记插件为 draining（拒绝新请求）
  2. 等待 in-flight 请求完成（timeout: drain_timeout）
  3. 超时后强制 cancel 未完成的请求
  4. 写入 checkpoint 到 State Store
  5. 执行 Phase 4 卸载
```

---

### 缺口 5：历史裁剪架构无独立章节

§12 配置表有 `trim` 参数组（`threshold_ratio`、`minimum_messages`、`unit_pairs`），但裁剪触发时机、与状态机的交互、裁剪算法全在配置表里隐含。§15.2 未解决问题 #5 也承认了裁剪后 UI 一致性问题。

应有独立章节说明：

- 裁剪触发时机（LLM Skill 每次调用前？达到 context_window 80% 时？）
- 裁剪粒度（消息对 / 单独消息 / 按 token 数？）
- 裁剪策略（FIFO / 摘要保留 / 重要度加权？）
- 裁剪后 WAL/State Store 的一致性
- 裁剪事件（HISTORY_TRIMMED）在重启后的恢复机制

---

### 缺口 6：交叉引用断裂

| 位置 | 引用 | 问题 |
|------|------|------|
| L1080 | `§11.5`（调试面板） | §11 无 11.5 子节 |
| L1082 | `§11.6`（SOUL 感知层显示） | §11 无 11.6 子节。SOUL 感知层内容实际在 §5 |
| §11 开头 | `llm-chat-design.md §14` | 需验证该文件 §14 是否仍然存在且结构一致 |

---

## 次要问题

| # | 问题 | 位置 |
|---|------|------|
| 7 | 多插件能力共享语义（§10）中，部分卸载时的降级判定标准未定义——是一个插件 Running 就算能力健康，还是需要特定比例？ | §10 |
| 8 | `get_capabilities()` 的返回时机：如果 Phase 2 尚未完成、前端已调用此 IPC，行为未定义（返回空？block？返回部分？） | §6.1, §7.1 |
| 9 | §8.2 OutputValidator 验证 LLM_STREAM_CHUNK 事件——流式 chunk 是按 N 个 token 发送的，每个 chunk 都验证还是验证完整回复？per-chunk 验证的延迟影响未说明 | §8.2, §8.6 |
| 10 | context_window 管理和 trim 仅以配置参数形式出现在 §12，无独立架构决策 | §12 |
| 11 | §5.2 SOUL 热更新对 Tool 权限的影响只说"不一致"，没有说明 Tool 权限快照是否也应与 SOUL 快照绑定 | §5.2 |
| 12 | §11.5 `/model switch` 和 `/provider switch` 如果 PROCESSING 态执行，行为未定义——是否影响当前 in-flight 请求？ | §11.5 |

---

## 建议补充的章节

按优先级排序：

### P0（Phase 2 前必须）

1. **测试架构** — 状态机 property test + 并发正确性 + 集成测试 + 前端快照测试
2. **可观测性** — RED 指标 + 健康检查 + 告警规则 + trace 传播

### P1（Phase 2 期间）

3. **Rate Limiting 架构** — 算法选择 + 多维度限流 + 前端协调
4. **Phase 4.5 排水逻辑** — 完整的 dump/drain/shutdown 时序
5. **历史裁剪策略** — 触发时机 + 算法 + 一致性保障

### P2（Phase 3 前）

6. **上下文窗口管理** — context_window 计算 + trim 触发 + 与 token 计量的整合

---

## 总体判断

这份文档作为 **Phase 1 的开工基础是够的**。路由守卫、导航栏动态、capability 注册、基础 IPC 命令都有明确规格。前端骨架可以参照 §6.1 的 IPC 清单和 §7 的激活逻辑直接开始。

但 **Phase 2 的完整聊天需要补上测试策略和可观测性**。没有这两个，并发队列 + 状态机 + WAL 重放的组合会成为调试黑洞——出问题时没人知道是状态机死锁还是队列溢出还是 WAL 重放竞态。
