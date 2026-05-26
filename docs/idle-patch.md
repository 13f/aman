# Idle State Execution

> 目标：将每个 idle 子状态从「只有 description 的空壳」落实为「可逐步执行的步骤」。
> 基准文档：`idle-design.md`，特别是 §14 Idle State Activity Catalog。
> **架构更新 (2026-05-24)**：idle skill 系统已移除（idle-system plugin + 7 个 YAML skill 文件 + workflow.rs stubs）。真正有执行逻辑的 idle 状态全部使用 **EventHandler** 模式（SleepRunner / ReflectionRunner / ExplorationRunner），通过 OnceLock 注入依赖，直接在 global event bus 上订阅 Idle 事件。不经过 Skill trait 路径，避免 Plugin/Skill trait 的依赖注入改造。
> **更新 (2026-05-23)**：MemoryProvider trait + YantrikdbProvider 已落地；Reflection session_extract 已实现（QueueDrained → LLM → YantrikDB）；MemoryStore 作为 in-memory 备选；QueueDrained 由 AgentIdleManager 在 busy→empty 转换时产生；**Sleep 已实现**（SleepRunner EventHandler，phase 2/3/4/6 完整实现，phase 1/5 stub）。
> **更新 (2026-05-26)**：**Meditation 和 Incubation 已实现。** MeditationRunner (`crates/gateway/src/runtime/meditation.rs`) — EventHandler，7 phases；IncubationRunner (`crates/gateway/src/runtime/incubation_runner.rs`) — EventHandler + 后台线程，5 phases。TraceStore（JsonlTraceStore）和 think() YantrikDB 桥接也已落地。think() 桥接完成后 Sleep phase 5 consolidation 已可获得真实 ThinkResult。

---

## 8 个 Idle 状态全景

| 深度 | 状态 | 类型 | 核心动作 | 实现位置 | 就绪度 |
|------|------|------|----------|----------|--------|
| 0 | **Daze** | no-op | 纯状态声明，idle 序列锚点 | IdleDetector 内存跟踪 | ✅ 已实现 |
| 1 | **Boredom** | no-op | 后续通过 kanban / deferred task queue | 无（待 kanban 机制） | ❌ 未实现 |
| 2 | **Waiting** | no-op | 等待外部条件满足 | 无（被动状态） | ⚠️ 不需要 |
| 3 | **Sleep** | EventHandler | 长期记忆整合，consolidation，temporal housekeeping，索引/缓存清理 | `crates/gateway/src/runtime/sleep.rs` | ✅ 已实现 |
| 5 | **Exploration** | EventHandler | 外部信息探索：memory gap → info-hub 搜索 → 存储发现 | `crates/gateway/src/runtime/exploration.rs` | ✅ 已实现 |
| 7 | **Reflection** | EventHandler | 即时复盘：session→YantrikDB 提取 + 过期记忆检查 | `crates/gateway/src/runtime/reflection.rs` | ✅ 已实现 |
| 8 | **Incubation** | EventHandler + 后台线程 | 创意孵化 / 跨域联想：跨域采样 → 联想 → 灵感评分 → 种子演进 | `crates/gateway/src/runtime/incubation_runner.rs` + `crates/idle/src/incubation.rs` | ✅ 已实现 |
| 10 | **Meditation** | EventHandler | 深度内省，提炼经验 → 更新启发式：TraceStore → KG 内省 → 模式提取 → think → 报告 | `crates/gateway/src/runtime/meditation.rs` | ✅ 已实现 |

### 实现阶段

```
Phase 1 (已完成) ── 核心基础设施
    Daze        no-op（IdleDetector 内存跟踪）
    Waiting     no-op（被动状态，depth+arousal 机制已覆盖）
    Reflection  ✅ session_extract (QueueDrained → LLM → YantrikDB)
    Sleep       ✅ phase 2/3/4/6 完整实现

Phase 2 (已完成) ── Sleep 完善 + Exploration
    Sleep       phase 1: session 回填 (stub)
                phase 5: think() consolidation (stub，YantrikDB 桥接已就绪)
    Exploration ✅ ExplorationRunner EventHandler
                phase 1: memory gap → 查询生成
                phase 2: info-hub adapters 外部搜索
                phase 3: 评分过滤 → MemoryProvider 存储
                phase 4: 本地 fallback

Phase 3 (中期) ── 引入外部组件后
    Boredom     kanban / deferred task queue

Phase 4 (已完成) ── 深度认知
    Meditation  ✅ MeditationRunner EventHandler
                phase 1: 前置检查 (min_interval_ticks, cooldown)
                phase 2: TraceStore 加载经验链
                phase 3: KG 内省 (entity_profile, get_edges, pending_conflicts)
                phase 4: 模式提取 (surface_procedural, store_procedural)
                phase 5: 认知循环 (think, consolidation + conflict scan)
                phase 6: 冥想报告 (atomic write)
                phase 7: 收尾 (cooldown, depth reset)
    Incubation  ✅ IncubationRunner EventHandler + IncubationManager 后台线程
                phase 1: 跨域记忆采样 (4 query perspectives × 25 limit)
                phase 2: 跨域联想 (entity 共现 + procedural analogies)
                phase 3: 灵感评分 (novelty × 0.6 + feasibility × 0.4)
                phase 4: 种子演进 (what-if 变体)
                phase 5: 认知循环 (light think, no consolidation)
```

### 职责关系

```
Session 结束 → QueueDrained 事件
    │
    ▼
Reflection (即时，per-task)          Sleep (批量，idle depth=3)
├─ chain_tasks                      ├─ 回填 Reflection 遗漏的 session
├─ immediate_errors                 ├─ temporal housekeeping
├─ lessons_learned                  ├─ 索引/缓存清理
└─ session_extract ★                ├─ think() consolidation
    └─ LLM 读 JSONL → 提取摘要      └─ 健康报告
       → YantrikDB.store()
       → YantrikDB.relate()
```

Reflection 承接 session 提取的原因：刚结束的对话上下文最热，LLM 提取意图/决策/产出/错误最准确。Sleep 不再做批量 session 压缩，改为专注 cognitive housekeeping（consolidation + think）。

---

## 0. MemoryProvider 架构（基础层）

> **状态：已实现。** `MemoryProvider` trait 定义在 `crates/core/src/memory.rs`，`YantrikdbProvider` 实现在 `crates/memory/src/yantrikdb.rs`，`MemoryStore` 作为 in-memory 备选。根据 `memory.provider` 配置自动选择（`"yantrikdb"` → YantrikdbProvider，其他 → MemoryStore）。

### 0.1 MemoryProvider trait（当前实现）

```rust
#[async_trait]
pub trait MemoryProvider: Send + Sync {
    // ── Identity ──
    fn name(&self) -> &str;
    fn is_available(&self) -> bool { true }

    // ── CRUD ──
    fn store(&self, agent_id: &str, content: &str, tags: Vec<String>) -> String;
    async fn recall(&self, agent_id: &str, query: &str, limit: usize) -> Vec<MemoryRecord>;
    fn list(&self, agent_id: &str, filter: Option<&MemoryFilter>) -> Vec<MemoryRecord>;
    fn forget(&self, agent_id: &str, rid: &str) -> bool;

    // ── Session management ──
    async fn session_start(&self, agent_id: &str, session_type: &str) -> AmanResult<String>;
    async fn session_end(&self, agent_id: &str, session_id: &str) -> AmanResult<SessionSummary>;
    async fn session_history(&self, agent_id: &str, limit: usize) -> AmanResult<Vec<SessionSummary>>;

    // ── Knowledge graph ──
    async fn relate(&self, from: &str, to: &str, rel_type: &str) -> AmanResult<()>;
    async fn get_edges(&self, entity: &str) -> AmanResult<Vec<(String, String, String)>>;
    async fn search_entities(&self, query: &str, limit: usize) -> AmanResult<Vec<String>>;
    async fn entity_profile(&self, entity: &str) -> AmanResult<Option<EntityProfile>>;

    // ── Temporal queries ──
    async fn stale_memories(&self, agent_id: &str, days: u32) -> AmanResult<Vec<MemoryRecord>>;
    async fn upcoming_memories(&self, agent_id: &str, days: u32) -> AmanResult<Vec<MemoryRecord>>;

    // ── Procedural memory ──
    async fn store_procedural(&self, agent_id: &str, name: &str, schema: &str, kind: &str) -> AmanResult<String>;
    async fn surface_procedural(&self, agent_id: &str, context: &str, limit: usize) -> AmanResult<Vec<MemoryRecord>>;

    // ── Cognitive processing ──
    async fn think(&self, agent_id: &str, config: &ThinkConfig) -> AmanResult<ThinkResult>;

    // ── Health & lifecycle ──
    async fn stats(&self, agent_id: &str) -> AmanResult<MemoryStats>;
    async fn initialize(&self, opts: &MemoryInitOpts) -> AmanResult<()> { Ok(()) }
    async fn shutdown(&self) -> AmanResult<()> { Ok(()) }
}
```

### 0.2 YantrikdbProvider — 默认实现

**YantrikDB** 是一个嵌入式多模态认知记忆引擎，提供：
- **HNSW 向量搜索** — 语义召回（`recall` → `recall_text`）
- **知识图谱** — 实体关系（`relate`, `get_edges`, `search_entities`, `entity_profile`）
- **时间衰减** — 内置 half-life 衰减，自动老化记忆（`stale_memories`, `upcoming_memories`）
- **会话管理** — 按 session 分组记忆（`session_start`, `session_end`, `session_history`）
- **程序记忆** — 策略/模式存储与召回（`store_procedural`, `surface_procedural`）
- **认知循环** — 内置 `think()` 机制（trigger 检测、冲突扫描、consolidation、pattern mining）—— **已桥接**，通过 mpsc channel + `spawn_blocking` 调用 yantrikdb `think()`
- **双模式 Embedding**：`Remote`（云端 API，零本地下载，默认）或 `Download`（`potion-multilingual-128M`，256-dim，101 语言）

**YantrikDB 内置能力 vs Provider 方法对照：**

| Provider 方法 | YantrikDB API | 说明 |
|---|---|---|
| `store()` | `record_text()` | 11 参数：text, memory_type, importance, valence, half_life, meta, namespace, certainty, domain, source, emotional_state |
| `recall()` | `recall_text()` | HNSW 语义搜索，按 namespace 过滤 |
| `forget()` | `forget()` | 软删除记忆 |
| `session_start()` | `session_start()` | 创建 session 记录 |
| `session_end()` | `session_end()` | 关闭 session，返回 SessionSummary |
| `session_history()` | `session_history()` | 按 namespace + client_id 查询历史 |
| `relate()` | `relate()` | 创建有向边，带权重 |
| `get_edges()` | `get_edges()` | 查询实体的出边 |
| `search_entities()` | `search_entities()` | 模糊匹配实体名 |
| `entity_profile()` | `entity_profile()` | 90 天窗口的实体活跃度画像 |
| `stale_memories()` | `stale()` | 超过 days 天未访问的高重要性记忆 |
| `upcoming_memories()` | `upcoming()` | 即将到期的记忆 |
| `store_procedural()` | `embed()` + `record_procedural()` | 先 embedding 再存储 |
| `surface_procedural()` | `embed()` + `surface_procedural()` | embedding 上下文后召回匹配策略 |
| `stats()` | `stats()` | 活跃记忆数、图谱节点/边数、冲突数 |
| `think()` | `think()` (yantrikdb) | **已桥接** — mpsc channel + spawn_blocking 透传 ThinkConfig，返回 ThinkResult |

### 0.3 think() — 认知处理接口

```rust
/// 配置一次 think pass。
pub struct ThinkConfig {
    pub importance_threshold: f64,   // 默认 0.5
    pub run_consolidation: bool,     // 默认 true
    pub run_conflict_scan: bool,     // 默认 true
}

/// think pass 的结果摘要。
pub struct ThinkResult {
    pub triggers_fired: usize,
    pub consolidation_count: usize,
    pub conflicts_found: usize,
    pub duration_ms: u64,
}
```

> **设计注**：YantrikDB 的 `think()` 内部执行：trigger 过期 → decay/consolidation/conflict/temporal-drift/redundancy/relationship-insight/valence-trend/entity-anomaly 八种 trigger 检测 → 冲突扫描 → consolidation（合并相似记忆）→ pattern mining。**桥接已实现**：`YantrikdbProvider::think()` 通过 mpsc channel + `spawn_blocking` 调用 yantrikdb 的 `think()`，将 `ThinkConfig` 透传并返回 `ThinkResult`（triggers_fired, consolidation_count, conflicts_found, patterns_new, patterns_updated, duration_ms）。

### 0.4 各 idle 状态对 MemoryProvider 的依赖矩阵

| Idle 状态 | store | recall | forget | session | graph | temporal | procedural | think | stats |
|-----------|-------|--------|--------|---------|-------|----------|------------|-------|-------|
| Daze | | | | | | | | | |
| Boredom | | ✓ | | | ✓ | ✓ | | | |
| Waiting | | | | | | | | | |
| **Sleep** | ✓ | ✓ | ✓ | ✓ | | ✓ | | ✓ | ✓ |
| **Exploration** | ✓ | ✓ | | | ✓ | ✓ | | | |
| **Meditation** | ✓ | ✓ | | | ✓ | | ✓ | ✓ | ✓ |
| Reflection | ✓ | ✓ | | ✓ | | ✓ | | | |
| **Incubation** | ✓ | ✓ | | | ✓ | | ✓ | ✓ | |

---

## 1. Daze ✅ 已完成

**路由**: `pipeline:idle-daze`
**类型**: Pipeline（同步，<1ms）
**可打断**: 否（同步执行完成）
**MemoryProvider 依赖**: 无

### 执行步骤

```
step 1: 声明当前状态为 Daze（IdleDetector 内存中已记录 current_kind、depth、duration_secs）
step 2: return None（纯 no-op，不产出事件）
```

IdleDetector 已在内存中跟踪所有 idle 指标（`current_kind`、`depth`、`kind_durations[]`、`arousal`），并实时同步到 UI。不需要独立的 `IdleMetrics` 持久化存储——idle metrics 是运行时快照，重启后重置，没有离线分析需求。

### 无缺失组件

Daze 是 idle 序列的深度 0 锚点。它的存在意义是区分"刚空闲"和"空闲了一段时间"——这个区分由 IdleDetector 完成，Daze skill 本身不需要额外逻辑。

### 验证

- [x] idle→Daze→skill dispatch 链路通畅
- [x] IdleDetector 正确追踪 current_kind = Daze
- [x] UI 实时显示 idle 状态

---

## 2. Boredom

**路由**: `pipeline:idle-boredom`
**类型**: Pipeline（同步，<10ms 正常模式，<1ms 聊天模式）
**可打断**: 否
**MemoryProvider 依赖**: `recall()` (随机浏览)、`stale_memories()` (标记 stale 条目)

### 执行步骤

```
step 1: 从 IdleEvent 读取 from_chat_mode
        if from_chat_mode:
            log!("Boredom skipped — chat mode")
            return None  // 纯 no-op，<1ms

step 2: 扫描 deferred_task_queue
        pending = deferred_task_queue.count(status=Pending)
        overdue = deferred_task_queue.count(status=Pending, scheduled_at < now)

step 3: if overdue > 0:
            取优先级最高的 overdue task → 包装为 Event(priority=Medium)
            → 发布到 Agent Local EventBus
            log!("Boredom emitted overdue task: {task_id}")
            return Some(event)

step 4: if pending > 0:
            不做操作（让正常调度处理），仅记录 boredom_pending_seen 计数

step 5: 随机浏览 —— 使用 MemoryProvider 随机召回
        samples = provider.recall(agent_id, "*", n=min(3, pending_count * 2)).await
        for item in samples:
            快速回顾 item.content（不展开完整上下文）
            如果 item.importance < 0.3 且 item.domain 为 stale → 产出低优先级 review Event

step 6: 检查 stale 标记
        stale = provider.stale_memories(agent_id, days=30).await
        if stale.len() > 0:
            取第一条 → 包装为 Event { type: "memory.review", payload: stale[0] }
            return Some(event)

step 7: 检查定时器注册表
        expired_timers = timer_registry.get_expired()
        if expired_timers.len() > 0:
            取第一个 → 包装为 Event
            return Some(event)

step 8: 任务饥饿度评估
        starvation_score = pending_count * 10 + overdue_count * 50
        if starvation_score > config.boredom.starvation_threshold:
            emit AlertEvent { severity: Low, message: "task starvation detected" }
```

### 缺失组件

| 组件 | 状态 | 说明 |
|------|------|------|
| `DeferredTaskQueue` | **未实现** | 需要一个支持 priority + scheduled_at 的任务队列。建议放在 `crates/persistence/` 或新建 `crates/task-queue/`。存储后端：初期用 SQLite（与 WAL 同 store），后期可换 Redis。API：`enqueue(task, priority, scheduled_at)` / `dequeue()` / `count(filter)` |
| `TimerRegistry` | **部分实现** | `crates/source/` 有 Timer source，但不清楚是否有注册表 API。需要：`register(timer_id, fire_at)` / `get_expired()` → Vec / `cancel(timer_id)` |
| `boredom.starvation_threshold` 配置 | **未实现** | 需在 `IdleConfig` 的 boredom section 新增配置项。默认值建议：50 |

### 优先级

**Phase 3** — 聊天模式已可用（纯 no-op）。完整模式需要 `DeferredTaskQueue`（kanban 机制）和 `TimerRegistry`。MemoryProvider 随机浏览能力已就绪（`recall` with wildcard query）。

---

## 3. Waiting

> **注意**：`depth_schedule` 和 `resolve_with_arousal()` 均未涉及 Waiting，当前 idle 状态机永远不会产生 `IdleKind::Waiting`。保留此状态仅为未来可能的条件驱动入口（如 pending operations tracking），目前只需 no-op 桩。

**路由**: `pipeline:idle-waiting`
**类型**: Pipeline（同步，条件检查 <1ms）
**可打断**: 否
**MemoryProvider 依赖**: 无

### 执行步骤

```
step 1: 从 IdleContext.last_event_type 判断等待源
        等待源类型：AsyncCall / FileWatch / Timer / UserReply / ExternalCondition

step 2: match 等待源类型:
        AsyncCall:
            handle = pending_async_calls.get(last_trace_id)
            if handle.is_ready():
                result = handle.try_take()
                emit WakeEvent { source: "async_call_complete", result }
                pending_async_calls.remove(last_trace_id)
                return

        FileWatch:
            // FileWatch 由 Source 层主动产事件，Waiting 只是占位
            // 真正的事件到达时 Dispatcher 会中断 idle 序列
            // 此处仅记录 wait_duration
            return

        Timer:
            remaining = timer_registry.remaining(last_trace_id)
            if remaining <= 0:
                emit WakeEvent { source: "timer_fired" }
                return

        UserReply:
            // 聊天场景：等待用户回复
            // grace_period 到期后退出 ChatMode → 完整模式
            // 不做操作，让 IdleDetector 的自然拨号序列处理
            return

step 3: 检查全局超时
        if elapsed > config.waiting.max_wait_secs:
            emit TimeoutEvent { trace_id: last_trace_id }
            return

step 4: 仍未满足 → no-op（下一次 poll 继续检查）
```

### 缺失组件

| 组件 | 状态 | 说明 |
|------|------|------|
| `PendingAsyncCalls` 注册表 | **未实现** | 需要一个全局的 async call handle 注册表。当 Agent 发起异步操作（HTTP、LLM call）时注册 handle，Waiting 轮询 readiness。建议放在 `crates/runtime/` 的 AgentHarness 层 |
| `waiting.max_wait_secs` 配置 | **未实现** | 需在 `IdleConfig` 新增 waiting section。默认值建议：300s |
| `TimerRegistry::remaining()` | **未实现** | 见 Boredom 的 TimerRegistry 条目 |

### 优先级

**Phase 1** — 条件触发的中间态，不是深度驱动的主路径。先 stub 为 no-op 即可跑通链路。

---

## 4. Sleep

> **状态：已实现。** `SleepRunner` 实现在 `crates/gateway/src/runtime/sleep.rs`，作为 EventHandler 订阅全局 Idle 事件（kind="sleep"）。采用与 `ReflectionRunner` 相同的 OnceLock 依赖注入模式。Phase 2 (temporal housekeeping)、Phase 3 (缓存清理)、Phase 4 (索引监控)、Phase 6 (健康报告) 完整实现；Phase 1 (session 回填) 为 stub，Phase 5 (think() consolidation) 桥接已就绪。
>
> **架构选择**：Sleep 不通过 idle-system plugin skill 执行（skill 仅记录日志），而是由独立的 SleepRunner EventHandler 在全局 event bus 上并行处理。这遵循了 ReflectionRunner 的模式——基础设施性质的 background housekeeping 不经过 Skill trait，避免对 Plugin/Skill trait 的依赖注入改造。

**路由**: `event_handler:IdleEvent{kind="sleep"}`
**类型**: EventHandler（异步，附 cancel_token 监控，通过 AgentRegistry 获取）
**可打断**: 是（idle_cancel_token，checkpoint 保存）
**Arousal**: Engaged (×0.5)
**MemoryProvider 依赖**: 全部 CRUD + session + temporal + think + stats

> **核心变更 (2026-05-23)**：Sleep 的所有记忆操作现在通过 `MemoryProvider` trait 接口执行，默认后端为 `YantrikdbProvider`。YantrikDB 内置了 temporal decay、HNSW 向量搜索、知识图谱和 consolidation 引擎——原先需要手动实现的 STM/LTM 迁移、TTL 清理、去重（dedup）现在由 yantrikdb 内部处理。Sleep 的职责从「手动管理存储层」变为「编排 cognitive housekeeping 任务」。
>
> **职责调整**：session→YantrikDB 提取的主路径移到了 Reflection（QueueDrained 后即时触发，上下文最新鲜）。Sleep phase 1 仅做回填（Reflection 遗漏的 session）。Sleep 的核心价值现在集中在 consolidation（phase 5 think()）和 housekeeping（phase 2/3/4/6）。

### 执行步骤

```
┌─────────────────────────────────────────────────────┐
│ Sleep Workflow (max_cpu_seconds=300, 可配置)          │
│ 每步前检查 cancel_token，被取消时 checkpoint → 退出  │
│ 所有 provider 调用通过 Arc<dyn MemoryProvider> 接口  │
└─────────────────────────────────────────────────────┘

phase 1: 会话压缩回填（Reflection 遗漏的 session）
    CHECKPOINT
    1.1 从 provider 获取近期 session 列表:
        sessions = provider.session_history(agent_id, limit=20).await
    1.2 筛选出未被 Reflection 提取过的 session:
        eligible = sessions.filter(|s| !s.is_compressed)
        // 正常情况下 Reflection 已提取绝大部分，eligible 通常为空或很短
    1.3 对每个 eligible session（通常 ≤ 3）:
        a. 加载完整对话日志（从 SessionStore，非 MemoryProvider 范畴）
        b. LLM 提取: 意图、关键决策、产出、错误
        c. 压缩为结构化摘要 { intent, decisions[], outputs[], errors[], tags[] }
    1.4 写入压缩摘要:
        provider.store(agent_id, summary_json, vec!["session_compressed".into(), session_id]).await
    1.5 标记原会话为 compressed（SessionStore 操作）
    1.6 track_cpu_time()
    // 注：Reflection 是 session→YantrikDB 提取的主路径（即时、上下文最热）。
    //     Sleep 仅回填遗漏（崩溃 / Reflection timeout / 重启等边缘情况）。
    //     正常情况下此 phase 退化为一次 session_history 查询 + 0 条处理。

phase 2: Temporal housekeeping（替代原 STM→LTM 迁移）
    CHECKPOINT
    2.1 查询 stale 记忆:
        stale = provider.stale_memories(agent_id, days=7).await
        // YantrikDB 的 temporal decay 自动标记长期未访问的记忆
    2.2 对每条 stale 记忆:
        - importance >= 0.6: 标记为需要 review（不是删除——yantrikdb 的 half_life 机制保留它）
        - importance < 0.3: provider.forget(agent_id, rid)
        - 中间值: 仅日志记录，不做操作（让 temporal decay 自然处理）
    2.3 track_cpu_time()

phase 3: 缓存清理
    CHECKPOINT
    3.1 扫描 cache_store: 找出 created_at < (now - cache_expiry_days=30) 的条目
    3.2 批量删除过期缓存
    3.3 统计释放空间 (bytes_freed)
    3.4 track_cpu_time()
    // 注：此 phase 操作文件系统缓存，不经过 MemoryProvider

phase 4: 索引优化
    CHECKPOINT
    4.1 调用 provider.stats(agent_id).await:
        获取 index_size_bytes, graph_nodes, graph_edges
    4.2 如果 index_size_bytes > threshold:
        触发 yantrikdb 内部 vacuum（通过 stats 监控，yantrikdb 自行管理索引）
    4.3 track_cpu_time()

phase 5: 认知 consolidation（替代原去重 phase）
    CHECKPOINT
    5.1 运行 think pass:
        config = ThinkConfig {
            importance_threshold: 0.5,
            run_consolidation: true,   // yantrikdb 内置 consolidation 引擎
            run_conflict_scan: true,   // 检测矛盾记忆
        }
        result = provider.think(agent_id, &config).await
        // YantrikdbProvider::think() 已桥接，返回真实 ThinkResult:
        //   - consolidation_count: 合并了多少相似记忆
        //   - conflicts_found: 检测到多少矛盾
        //   - triggers_fired: 触发了多少认知 trigger
        //   - patterns_new / patterns_updated: 模式发现/更新数
    5.2 记录 think 结果:
        log!("Sleep think: consolidated={}, conflicts={}, triggers={}",
             result.consolidation_count, result.conflicts_found, result.triggers_fired)
    5.3 track_cpu_time()

phase 6: 健康报告
    CHECKPOINT
    6.1 采集 provider 指标:
        stats = provider.stats(agent_id).await
        // stats.total_entries, stats.index_size_bytes,
        // stats.graph_nodes, stats.graph_edges, stats.pending_conflicts
    6.2 采集 session 指标:
        sessions = provider.session_history(agent_id, limit=100).await
        recent_memory_count = sessions.iter().map(|s| s.memory_count).sum()
    6.3 写入 health_snapshot:
        snapshot = {
            timestamp: now,
            total_entries: stats.total_entries,
            index_size_bytes: stats.index_size_bytes,
            graph_nodes: stats.graph_nodes,
            graph_edges: stats.graph_edges,
            pending_conflicts: stats.pending_conflicts,
            recent_memory_count,
            compressions_done,
            bytes_freed
        }
    6.4 写入 health_snapshot 表 (timestamped)
    6.5 log!("Sleep complete: {snapshot}")
```

### YantrikDB 内置能力覆盖情况

| Sleep 需求 | 原方案 (BuiltinMemory) | 现方案 (YantrikdbProvider) |
|---|---|---|
| STM→LTM 迁移 | 手动 SQLite 迁移 + TTL 列 | yantrikdb 内置 half-life 衰减 + `stale_memories()` 查询 |
| 去重 (dedup) | 手动 cosine similarity O(n²) | yantrikdb 内置 consolidation 引擎（`think()` 触发） |
| 索引优化 | 手动 Tantivy merge + vacuum | yantrikdb 内部管理，`stats()` 监控 |
| 会话压缩 | SessionStore + SessionCompressor | `session_start/end` + `store()` 写入压缩摘要 |
| 质量评分 | 五维手动评分函数 | yantrikdb 内置 importance/valence/certainty 元数据 |
| 健康报告 | 手动聚合各 store 指标 | `stats()` + `session_history()` 聚合 |

### 缺失组件

| 组件 | 状态 | 说明 |
|------|------|------|
| `MemoryProvider.think()` 桥接 | ✅ **已实现** | YantrikdbProvider 的 `think()` 通过 mpsc channel + `spawn_blocking` 调用 yantrikdb 认知引擎（八种 trigger + consolidation + conflict scan + pattern mining） |
| CPU 时间追踪 | ✅ **已实现** | `CpuTracker` 实现在 `sleep.rs`，track wall-clock time against `max_cpu_seconds` budget。每个 phase 前后调用 `start_phase()` / `end_phase()`，`budget_remaining()` 在 phase 间检查 |
| CacheStore（文件系统 TTL） | ✅ **已实现** | Phase 3 通过 `std::fs::read_dir` 递归遍历 `~/.aman/agents/{agent_id}/cache/`，按 mtime + `cache_expiry_days` 删除过期文件 |
| Health snapshot 存储 | ✅ **已实现** | Phase 6 写 JSON 到 `~/.aman/agents/{agent_id}/health/sleep_{timestamp_ms}.json` |

### 优先级

**Phase 2** — ✅ 已完成。phase 2/3/4/6 完整实现。phase 1 (session 回填) 为 stub——Reflection 处理主路径，回填需 `SessionSummary.is_compressed` 字段 + LLM wiring。phase 5 (consolidation) — think() YantrikDB 桥接已就绪，Sleep phase 5 调用 `provider.think()` 并获取真实 ThinkResult。

---

## 5. Exploration

> **状态：已实现。** `ExplorationRunner` 实现在 `crates/gateway/src/runtime/exploration.rs`，作为 EventHandler 订阅全局 Idle 事件（kind="exploration"）。采用与 `SleepRunner` / `ReflectionRunner` 相同的 OnceLock 依赖注入模式。使用 info-hub 插件的 adapter 层执行外部搜索。

**路由**: `event_handler:IdleEvent{kind="exploration"}`
**类型**: EventHandler（异步，附 cancel_token，通过 AgentRegistry 获取）
**可打断**: 是（idle_cancel_token，每 phase 前检查）
**Arousal**: Engaged (×0.0)
**MemoryProvider 依赖**: `stale_memories()` (memory gaps)、`search_entities()` (孤立实体发现)、`entity_profile()` (实体关联度)、`store()` (写入发现)
**外部搜索**: info-hub adapters（API / CLI / DB 三种适配器）

### 架构

```
IdleDetector → IdleEvent{kind="exploration"} → ExplorationRunner.handle()
                                                   ├─ MemoryProvider (OnceLock 注入)
                                                   ├─ InfoHubConfig → adapters (外部搜索)
                                                   └─ ExplorationConfig (速率/阈值配置)
```

### 执行步骤

```
phase 1: 查询生成（纯本地，基于 MemoryProvider）
    1.1 memory_gaps — 从 stale memories 生成查询:
        stale = provider.stale_memories(agent_id, days=7).await
        for mem in stale:
            if mem.importance > 0.4:
                queries += "latest information about: {content_snippet}"
        // max 15 queries from this source

    1.2 entity_gaps — 知识图谱孤立实体:
        entities = provider.search_entities("*", limit=10).await
        for entity in entities:
            profile = provider.entity_profile(&entity).await
            if profile.edge_count == 0:
                queries += "what is {entity} and how does it relate to other things?"

    1.3 去重 + 截断到 top 30

phase 2: 外部搜索（通过 info-hub adapters）
    2.1 从 InfoHubConfig 创建 adapters（API/CLI/DB）
    2.2 速率限制: sleep(60000 / api_rate_per_minute) ms between queries
    2.3 逐个 query → 并发查询所有 sources → merge (dedup by url, sort by date)
    2.4 取 top 20 结果
    // 如遇 cancel_token → 中断，保留已完成结果

phase 3: 评分 & 存储
    3.1 简单启发式评分（v1）:
        - title_length (0.3) + summary_length (0.3)
        - has_url (0.2) + has_date (0.2)
        - threshold: 0.3
    3.2 score > 0.3:
        provider.store(agent_id,
            "[Exploration] {title}\nURL: {url}\n{summary}\nSource: {source} | Published: {date}",
            ["exploration", source_name, ...]
        )
    3.3 score > 0.7: 追加 "high_value" tag
    // 注：LLM 深度评分（tag/score/summarize）留给 Agent 醒来后通过 info-hub tools 自行调用

phase 4: 降级模式
    4.1 如果 info_hub_config 未配置 或 查询结果为空:
        → provider.recall(agent_id, "interesting developments new information", 10)
        → 本地探索
```

### 与 info-hub 的关系

ExplorationRunner **不**通过 Tool trait 调用 info-hub。它直接使用 info-hub 的 adapter 层：

```
ExplorationRunner
  ├─ info_hub::adapters::build_adapter() — 创建搜索适配器
  ├─ info_hub::types::{InfoItem, InfoSearchInput} — 数据结构
  └─ info_hub::merge::merge() — 去重排序
```

info-hub 的 5 个 Tool（info_search / info_tag_articles / info_score_articles / info_summarize_articles / info_generate_highlights）仍然可用，但它们由 **Agent 主动调用**（例如 Agent 醒来后审查 Exploration 发现时），而非在自主探索阶段自动运行。

### 依赖注入

```rust
// agent_runtime.rs 中的 wiring:
let exploration_runner = Arc::new(ExplorationRunner::new());
exploration_runner.set_agent_registry(Arc::clone(&agent_registry));
exploration_runner.set_memory_provider(Arc::clone(&memory_store));
exploration_runner.set_info_hub_config(info_cfg);  // 从 aman config 解析
exploration_runner.set_exploration_config(exploration_cfg);
// 订阅 Idle 事件到 global bus
```

### v1 不做（留给后续）

| 项目 | 原因 |
|---|---|
| skill_audit（检查上游 skill 更新） | 需 SkillRegistry + upstream URL 元数据 |
| recent_failures 错误签名提取 | 依赖 ErrorLog 查询接口，属于 Reflection 范畴 |
| LLM 深度评分（tag/score/summarize） | Agent 醒来后通过 info-hub tools 自行处理 |
| 高价值事件发布到 Agent EventBus | Agent 醒来后通过 `memory.recall("exploration")` 主动拉取 |
| 多主题并行 Exploration | 单 runner 已覆盖当前需求 |

### 缺失组件

| 组件 | 状态 | 说明 |
|------|------|------|
| `ExternalSearchEngine` | ✅ **已实现** | info-hub adapters 层（API/CLI/DB 三种适配器） |
| `InterestScorer` | ✅ **已实现** | 简单启发式评分（title/summary/url/date），LLM 深度评分留给 Agent 主动调用 |
| `RateLimiter` | ✅ **已实现** | 基于 `api_rate_per_minute` 的 inter-query sleep |
| `exploration.min_interest_score` 配置 | ⚠️ 硬编码 | 当前阈值 0.3 硬编码，可从 ExplorationConfig 扩展 |
| `SkillRegistry::list_all()` | ❌ 不做 | skill audit 留给后续 |
| `UpstreamFreshnessChecker` | ❌ 不做 | 同上 |
| `ErrorSignatureExtractor` | ❌ 不做 | 需要 ErrorLog + TraceStore |
| `ErrorLog` 查询接口 | ❌ 不做 | 同上 |

### 优先级

**Phase 2 (已完成)** — ExplorationRunner v1 已实现：memory gap → info-hub 搜索 → 启发式评分 → MemoryProvider 存储。LLM 深度评分留给 Agent 主动调用 info-hub tools。skill audit / error review 属于 Reflection 和后续阶段的范畴。

---

## 6. Meditation

> **状态：已实现。** `MeditationRunner` 实现在 `crates/gateway/src/runtime/meditation.rs`，作为 EventHandler 订阅全局 Idle 事件（kind="meditation"）。采用与 SleepRunner / ReflectionRunner 相同的 OnceLock 依赖注入模式。通过 AgentRegistry 获取 per-agent 的 MemoryProvider 和 TraceStore。支持 cancel_token 中断（深度内省可被真实事件打断），cooldown 机制防止连续触发。

**路由**: `event_handler:IdleEvent{kind="meditation"}`
**类型**: EventHandler（异步，附 cancel_token，通过 AgentRegistry 获取）
**可打断**: 是（idle_cancel_token，丢弃当前产出，temp+rename 保证上一个完成的报告安全）
**Arousal**: Engaged (×0.0)
**MemoryProvider 依赖**: `entity_profile()` (内省实体)、`get_edges()` (关系分析)、`surface_procedural()` (策略回顾)、`think()` (认知循环)、`stats()` (健康检查)、`store_procedural()` (策略存储)
**TraceStore 依赖**: `load_recent()` (加载经验链)

### 执行步骤

```
phase 1: 前置检查
    1.1 检查 min_interval_ticks:
        if now - last_meditation_at < min_interval_ticks * poll_interval:
            log!("Meditation skipped — min_interval not met")
            return Skipped
    1.2 更新 last_meditation_at（先占位，防止并发触发）

phase 2: 加载经验链
    CHECKPOINT
    2.1 从 trace_store 加载最近 N 条 trace（N = config.meditation.review_depth，默认 20）
    2.2 每条 trace 包含:
        - task 描述、输入/输出
        - 决策点（branch taken / not taken）
        - 错误及恢复路径
        - 工具调用链
        - 耗时
    2.3 如果 trace_store 为空 → 跳过本轮，return Empty

phase 3: 知识图谱内省
    CHECKPOINT
    3.1 获取 provider 健康快照:
        stats = provider.stats(agent_id).await
        // stats.graph_nodes, stats.graph_edges, stats.pending_conflicts

    3.2 分析图谱中的关键实体:
        从近期 trace 中提取涉及的 entity 名称
        for entity in trace_entities:
            profile = provider.entity_profile(&entity).await
            edges = provider.get_edges(&entity).await
            // 分析: 哪个实体最近最活跃？哪个被孤立了？

    3.3 冲突检测:
        如果 stats.pending_conflicts > 0:
            这些冲突需要冥想解决 → 加载冲突涉及的 entity pair
            // YantrikDB 的 claim_conflicts 机制自动检测矛盾声明

phase 4: 模式提取
    CHECKPOINT
    4.1 查询 procedual memory 中匹配近期 trace 的策略:
        for trace in traces:
            context = format!("{}: {} -> {}", trace.task_type, trace.input, trace.outcome)
            strategies = provider.surface_procedural(agent_id, &context, limit=5).await
            // surface_procedural 用 embedding 找到相关策略

    4.2 成功模式:
        如果 trace.outcome == Success 且匹配到已知策略:
            标记为 recurring_success_pattern
            提取: trigger_conditions[], action_sequence[], success_rate
        如果 trace.outcome == Success 但无匹配策略:
            可能发现了新模式 → 候选策略

    4.3 失败模式:
        如果 trace.outcome == Failure:
            按 error_type 聚类
            提取: error_signature, root_cause_candidates[], failed_approaches[]
            // 查询是否有已知的 procedural memory 可以避免此错误

    4.4 策略更新:
        for pattern in recurring_success_patterns:
            provider.store_procedural(agent_id,
                &pattern.name,
                &pattern.schema,
                "strategy"
            ).await

phase 5: 认知循环（替代原启发式更新 + KG 修剪）
    CHECKPOINT
    5.1 运行 think pass 进行深度内省:
        config = ThinkConfig {
            importance_threshold: 0.4,   // 降低阈值以捕获更多记忆
            run_consolidation: true,     // 合并相似经验
            run_conflict_scan: true,     // 检测矛盾启发式
        }
        result = provider.think(agent_id, &config).await
        // think() 已桥接到 YantrikDB，返回:
        //   - triggers_fired: decay/conflict/relationship_insight 等 trigger 数量
        //   - consolidation_count: 合并了多少经验
        //   - conflicts_found: 检测到多少矛盾
        //   - patterns_new / patterns_updated: 模式发现/更新数

    5.2 记录 think 结果用于报告

phase 6: 冥想报告
    CHECKPOINT
    6.1 生成报告内容:
        title: "Meditation Report — {date}"
        sections:
            - Executive Summary (3–5 句)
            - Knowledge Graph Status (stats.graph_nodes 节点, stats.graph_edges 边,
              stats.pending_conflicts 待解决冲突)
            - Entity Introspection (分析了哪些实体，发现什么关系变化)
            - Patterns Discovered (成功 × N, 失败 × M)
            - Procedural Memory Updates (新增/更新策略 × A)
            - Think Pass Summary (triggers: N, consolidated: M, conflicts: K)
            - Next Meditation Suggestions
    6.2 写入格式: Markdown
    6.3 写入路径: temp_file = report_path + ".tmp.{uuid}"
                  write(temp_file, report_content)
                  fsync(temp_file)
                  rename(temp_file, report_path + "{timestamp}.md")  // atomic
    6.4 清理失败的 temp 文件（如果 rename 前崩溃）

phase 7: 收尾
    7.1 更新 last_meditation_at 为 NOW（精确值）
    7.2 meditations_completed += 1
    7.3 log!("Meditation complete: {patterns} patterns, {updates} strategy updates")
```

### YantrikDB 内置能力覆盖

| Meditation 需求 | 原方案 | 现方案 (YantrikdbProvider) |
|---|---|---|
| 知识图谱修剪 | 手动扫描孤立节点 + 90d/180d 规则 | `entity_profile()` + `get_edges()` 查询，yantrikdb 内部管理图谱生命周期 |
| 启发式更新 | HeuristicStore CRUD + 冲突检测 | `store_procedural()` / `surface_procedural()` + `think()` conflict scan |
| 模式提取 | 手动统计聚类 | `surface_procedural()` embedding 匹配 + `think()` pattern mining (已桥接) |
| KG 冲突检测 | 手动矛盾检测 | yantrikdb 内置 claim_conflicts + entity_conflicts (think 触发) |
| 内省报告 | MeditationReportWriter | 聚合 `stats()` + `entity_profile()` + `think()` 结果 |

### 缺失组件

| 组件 | 状态 | 说明 |
|------|------|------|
| `TraceStore` | ✅ **已实现** | `JsonlTraceStore` 实现在 `crates/persistence/src/trace_store.rs`。MeditationRunner 通过 `AgentRegistry::get_trace_store()` 获取 per-agent 实例 |
| `MemoryProvider.think()` 桥接 | ✅ **已实现** | `YantrikdbProvider::think()` 通过 mpsc channel + `spawn_blocking` 调用 yantrikdb 认知引擎 |
| `MeditationReportWriter` | ✅ **已实现** | `MeditationRunner::write_meditation_report()` — Markdown 报告 + atomic write (temp → fsync → rename) + stale temp 清理 |
| Narrative 报告目录 | ✅ **已实现** | `~/.aman/narrative/meditation/{agent_id}/` — `fs::create_dir_all` 自动创建 |
| `meditation.review_depth` 配置 | ✅ **已实现** | `MeditationConfig::review_depth`，默认 20，serde default |

### 已知局限

| 局限 | 说明 |
|------|------|
| Partial/Cancelled trace 跳过 | `TraceOutcome::Partial` 和 cancelled 的 trace 在模式提取中被跳过（代码注 "skip for now"），中断频繁的 agent 可能遗漏模式 |
| Entity 提取依赖上游 | entity introspection 的质量取决于 trace 记录时 `trace.entities` 的完整性，无 fallback 提取逻辑 |
| `check_cancel!` 不一致 | Phase 3 entity introspection 直接检查 `cancel_token.is_cancelled()`，未使用 `check_cancel!` 宏 |

### 优先级

**Phase 4 (已完成)** — ✅ MeditationRunner 已实现：7 phases 完整实现。TraceStore + think() bridge + MeditationConfig 全部落地。

---

### 0.6 QueueDrained → Reflection 触发机制

> **状态：已实现。** `AgentIdleManager` 的后台循环检测 agent local bus 的 busy→empty 转换，在转换点上产生 `QueueDrained` 事件（含 `agent_id` 和 `reflection_consecutive_count`），发布到 global bus。`ReflectionRunner` 订阅全局 QueueDrained 事件，执行反射逻辑。断路器在连续 20 次无实际事件时冷却。

```
AgentIdleManager 后台循环
  │
  ├─ pending > 0  → was_busy = true, depth = 0
  │
  └─ pending == 0 && was_busy
       │
       ├─ 断路器: reflection_count >= 20 → skip
       │
       └─ 产生 QueueDrained { agent_id, reflection_consecutive_count, arousal_level }
            │
            ▼
       Global EventBus → ReflectionRunner.handle()
            │
            ├─ count >= 10 → full skip (circuit breaker)
            ├─ session_extract (always)
            └─ session_review (count == 0 only)
```

`ReflectionRunner` 实现在 `crates/gateway/src/runtime/reflection.rs`，依赖通过 `OnceLock` 注入（SessionStore, MemoryProvider, LlmProvider, MemoryLlmConfig），模式与 `ReadSkillTool` 一致。

---

## 7. Reflection

**路由**: `ReflectionRunner` (EventHandler，订阅 QueueDrained)
**类型**: 事件处理器（非 Pipeline，避免依赖注入复杂度）
**可打断**: 否（同步执行，session_extract 仅处理一个 session 以控制延迟）
**触发**: `AgentIdleManager` 在 busy→empty 转换时产生 QueueDrained 事件
**MemoryProvider 依赖**: `store()` (写入提取摘要)、`relate()` (创建实体关联)、`stale_memories()` (找到未处理项)

### 执行步骤（✅ = 已实现，🔧 = stub，⏳ = 待 ChainTaskDetector/ErrorClassifier）

```
step 0: ✅ circuit breaker
        从 QueueDrained 事件读取 reflection_consecutive_count
        if count >= 10: full skip
        (AgentIdleManager 层面: count >= 20 → 不产生 QueueDrained)

step 1: ⏳ chain_tasks (TraceStore 已就绪，待 ChainTaskDetector)
        ...

step 2: ⏳ immediate_errors (TraceStore 已就绪，待 ErrorClassifier)
        ...

step 3: ⏳ lessons_learned (TraceStore 已就绪，待实现)
        ...

step 4: ✅ session_extract ★ (主提取路径)
        4.1 从 SessionStore 列出所有 session，找最近有消息的
        4.2 使用 memory.llm 配置的模型提取结构化 JSON:
            - intent, decisions[], outputs[], errors[], tags[], entities[]
        4.3 写入 MemoryProvider:
            provider.store(agent_id, summary_json, ["session_extract", session_id])
        4.4 对提取的实体创建 KG 关联:
            for entity in entities:
                provider.relate(entity, session_id, "appears_in")
        4.5 每次 QueueDrained 只处理一个 session（控制延迟）

step 5: ✅ session_review (count == 0 时)
        检查 stale 记忆:
        stale = provider.stale_memories(agent_id, days=14)
```

### YantrikDB 内置能力覆盖

| Reflection 需求 | 原方案 | 现方案 (YantrikdbProvider) |
|---|---|---|
| 经验教训存储 | Manual lesson store | `store()` 直接写入，带 tags |
| 会话压缩 → YantrikDB | SessionStore + 手动压缩 | `store()` + `relate()` 创建 KG 关联 |
| session 回顾 | SessionStore 查询 | `session_history()` |
| stale 记忆检查 | manual query | `stale_memories()` |

### 缺失组件

| 组件 | 状态 | 说明 |
|------|------|------|
| `ReflectionRunner` | ✅ **已实现** | `crates/gateway/src/runtime/reflection.rs` — 订阅 QueueDrained，session_extract + session_review |
| `TraceStore` | ✅ **已实现** | `JsonlTraceStore` (`crates/persistence/src/trace_store.rs`)。Reflection 和 Meditation 共享同一套 trace 存储。chain_tasks / immediate_errors / lessons_learned 待实现 |
| `ChainTaskDetector` | **未实现** | 分析 task 完成状态 → 检测连锁任务。需要 task 类型分类器 + 隐式依赖知识库 |
| `ErrorClassifier` | **未实现** | 错误四分类（Recovered/Unrecovered/Warning/Silent） |
| `SessionExtractor` (step 4) | ✅ **已实现** | JSONL 加载 + LLM 调用 + structured JSON 解析 + YantrikDB store/relate。每次 QD 处理一个 session |

### 优先级

**Phase 1** — `session_extract`（step 4）已实现：QueueDrained 触发 → SessionStore 加载 JSONL → LLM 提取 → YantrikDB。`chain_tasks` / `immediate_errors` / `lessons_learned` 待 ChainTaskDetector + ErrorClassifier 实现。

---

## 8. Incubation

> **状态：已实现。** `IncubationRunner` 实现在 `crates/gateway/src/runtime/incubation_runner.rs`，作为 EventHandler 订阅全局 Idle 事件（kind="incubation"）。EventHandler filter 通过后 spawn 后台任务（via `IncubationManager`）并立即返回。`IncubationManager` 实现在 `crates/idle/src/incubation.rs`，强制 max_concurrent=1，auto-clear on completion/panic。后台任务执行 5 个 phase 的跨域创意合成。
>
> **架构选择**：IncubationRunner EventHandler 在 filter 通过后 spawn 后台任务并立即返回（<1ms），遵循 idle-patch.md 原设计 "Pipeline 触发 → 启动后台线程 → Pipeline 立即返回"。后台任务不因真实事件中断（纯后台），仅 Agent shutdown 时通过 `IncubationManager::shutdown_all()` 取消。

**路由**: `event_handler:IdleEvent{kind="incubation"}` → spawn 后台任务
**类型**: EventHandler（filter + spawn，<1ms 返回） + 独立后台线程（via IncubationManager）
**可打断**: 否（纯后台，仅 shutdown 时取消。chat mode 下跳过）
**Arousal**: Engaged (×0.1)
**MemoryProvider 依赖**: `recall()` (随机跨域采样)、`search_entities()` (跨域实体发现)、`relate()` (创建跨域链接)、`surface_procedural()` (策略联想)、`think()` (认知循环)、`store()` (写入灵感)

### 执行步骤

```
pipeline 部分 (<1ms):
    step 1: 检查 incubation_manager.active_count < max_concurrent_threads (1)
            如果已满 → 跳过，return
    step 2: incubation_manager.spawn(incubation_task)
    step 3: 立即返回（Pipeline 不等待线程完成）

后台线程 incubation_task:
    phase 1: 跨域记忆采样
        1.1 使用 MemoryProvider 从不同 domain 召回记忆:
            // 使用通配符或跨 domain 查询
            domain_a_mems = provider.recall(agent_id, "interesting patterns", limit=25).await
            domain_b_mems = provider.recall(agent_id, "unexpected connections", limit=25).await
            all_mems = [domain_a_mems, domain_b_mems].concat()

        1.2 按 domain 分组:
            // MemoryRecord.domain 提供 domain 标签
            domains = group_by(|m| m.domain.clone().unwrap_or_default())

        1.3 选取至少 2 个不同 domain 的记忆对

    phase 2: 跨域联想
        2.1 使用 entity 搜索发现隐藏关联:
            for domain, mems in domains:
                entities = extract_entities(mems)
                for entity in entities:
                    related = provider.search_entities(&entity, limit=10).await
                    // 发现跨 domain 的实体共现

        2.2 查询 procedual memory 中的类比:
            for (mem_a, mem_b) in cross_domain_pairs:
                context = format!("{} vs {}", mem_a.content, mem_b.content)
                analogies = provider.surface_procedural(agent_id, &context, limit=3).await
                // 找到以前存储的策略/模式

        2.3 生成假设性问题:
            for each surprising_pair:
                "Could {mem_a.pattern} from {mem_a.domain} apply to {mem_b.domain}?"
                "What if {mem_b.approach} were used for {mem_a.problem}?"

    phase 3: 灵感评分
        3.1 对每个假设:
            novelty = 1.0 - max_similarity_to_existing_inspirations(hypothesis)
            feasibility = estimate_feasibility(hypothesis)
            score = novelty * 0.6 + feasibility * 0.4

        3.2 score >= INCUBATION_THRESHOLD (默认 0.7) → 写入:
            provider.store(agent_id,
                format!("[Inspiration] {}", hypothesis),
                vec!["inspiration".into(), "incubation".into()]
            )

            // 同时创建 KG 关联
            provider.relate(&mem_a.entity, &mem_b.entity, "cross_domain_inspiration").await

        3.3 score >= HIGH_VALUE_THRESHOLD (默认 0.85):
            包装为 Event { priority: VeryLow, source: "idle.incubation" }
            → 发布到 Local EventBus（Agent 醒来后可审查）

    phase 4: 种子演进
        4.1 从 inspiration 记忆中加载最近的 seeds:
            seeds = provider.recall(agent_id, "inspiration incubation", limit=5).await

        4.2 对每个 seed:
            发散推演: 生成 2–3 个 "what-if" 变体
            变体评分: novelty × feasibility

        4.3 高分变体追加为新的 inspiration seed:
            provider.store(agent_id, variant_content,
                vec!["inspiration".into(), "seed_evolution".into(), parent_seed_id])

    phase 5: 认知循环（可选）
        5.1 运行轻量 think pass:
            config = ThinkConfig {
                importance_threshold: 0.3,
                run_consolidation: false,  // Incubation 不合并——保留多样性
                run_conflict_scan: false,
            }
            // 仅触发 relationship_insight 和 entity_anomaly trigger
            result = provider.think(agent_id, &config).await

        5.2 更新 incubation_threads_spawned 计数
        5.3 线程退出（或等待 cancel_token 被触发）
```

### YantrikDB 内置能力覆盖

| Incubation 需求 | 原方案 | 现方案 (YantrikdbProvider) |
|---|---|---|
| 跨域随机采样 | `LongTermStore::random_sample()` | `recall()` 语义搜索（查询多样性 + 随机 domain 过滤） |
| 跨域联想 | 手动 embedding_distance + 假设生成 | `search_entities()` 跨域实体发现 + `surface_procedural()` 类比检索 |
| 灵感存储 | InspirationStore | `store()` + `relate()` 创建 KG 跨域边 |
| 种子演进 | 手动变体生成 | `store()` 追加 seed + parent_seed_id 关联 |
| 认知循环 | 无 | `think()` trigger detection (relationship_insight, entity_anomaly) |
| 跨域推论 | manual analogy | `surface_procedural()` 类比检索 + `think()` relationship_insight trigger (已桥接) |

### 缺失组件

| 组件 | 状态 | 说明 |
|------|------|------|
| `IncubationManager` | ✅ **已实现** | `crates/idle/src/incubation.rs` — max_concurrent=1，auto-clear on completion/panic，7 个单元测试 |
| `IncubationRunner` | ✅ **已实现** | `crates/gateway/src/runtime/incubation_runner.rs` — EventHandler + 5 phase 后台任务 |
| `FeasibilityEstimator` | ✅ **已实现** | `estimate_feasibility()` — 基于 shared_entity_count + analogy_count 的启发式评分 |
| Cross-domain pair generation | ✅ **已实现** | `by_domain` HashMap 分组 → 跨 domain pair 遍历，4 种 query perspective |
| `MemoryProvider.think()` 桥接 | ✅ **已实现** | Phase 5 通过 mpsc channel 调用 YantrikDB think()（light pass: no consolidation, no conflict scan） |
| `incubation.incubation_threshold` 配置 | ✅ **已实现** | `IncubationConfig::incubation_threshold`，默认 0.7 |
| `incubation.high_value_threshold` 配置 | ✅ **已实现** | `IncubationConfig::high_value_threshold`，默认 0.85 |

### 已知局限

| 局限 | 说明 |
|------|------|
| `extract_entities()` 过于简单 | 基于 whitespace split + `search_entities()`，无 NLP/LLM 提取。长于 3 字符的常见词（"this", "that", "with"）也会被查询 |
| Phase 4 种子演进评分硬编码 | 变体的 novelty/feasibility 固定为 0.8/0.5，不做实际计算 |
| `Hypothesis.pair_index` 未使用 | `#[allow(dead_code)]` — 字段计算后从未读取 |
| 单 domain 时静默降级 | `by_domain.len() < 2` 时跳过 Phase 2-4，仅执行 think pass，无 warning 日志 |
| 无 cancel token | 后台任务一旦 spawn 就运行至完成，不响应真实事件（符合设计意图：纯后台） |

### 优先级

**Phase 4 (已完成)** — ✅ IncubationRunner + IncubationManager 已实现：5 phases 完整实现。MemoryProvider 能力全部桥接。

---

## 附录 A: 缺失组件汇总（按构建顺序）

下表按依赖关系排列——**先构建下层基础设施，再构建上层消费者**。标记 "✅" 表示已由 YantrikdbProvider 覆盖。

```
构建顺序  组件                             状态                    依赖                    消费者
──────────────────────────────────────────────────────────────────────────────────────────────────
 ── Phase 1 (✅ 已实现) ──
 1       MemoryProvider trait             ✅ 已实现              无 (crates/core/)        所有 provider
 2       MemoryRecord / MemoryStats 等    ✅ 已实现               core types              所有 consumer
 3       ThinkConfig / ThinkResult        ✅ 已实现               core types               Sleep, Meditation, Incubation
 4       YantrikdbProvider (默认)         ✅ 已实现               MemoryProvider trait     所有 idle skill
 5       memory.llm / memory.embedding 配置✅ 已实现               config crate             Reflection, Sleep
 6       RemoteEmbedder (云端 embedding)  ✅ 已实现               reqwest                   YantrikdbProvider
 6a      MemoryStore MemoryProvider 适配   ✅ 已实现              memory_store.rs          in-memory 备选
 6b      QueueDrained 生产 (AgentIdleMgr)  ✅ 已实现              idle crate               Reflection
 6c      ReflectionRunner (session_extract)✅ 已实现              reflection.rs            Reflection
 ── Phase 2 (短期 — 桥接 + Sleep) ──
 7       SessionExtractor (Reflection)    ✅ 已实现               ReflectionRunner         Reflection
 8       think() 桥接 (yantrikdb→Provider) ✅ 已实现              YantrikdbProvider        Sleep, Meditation, Incubation
 9       CacheStore (文件系统 TTL)         ✅ 已实现              std::fs (sleep.rs)       Sleep (phase 3)
10       Health snapshot 存储              ✅ 已实现              JSON 文件 (per-agent)     Sleep (phase 6)
11       CPU time tracker                 ✅ 已实现              CpuTracker (sleep.rs)     Sleep
12       AtomicWrite                      未实现                  无                       全局复用
13       ExplorationRunner                ✅ 已实现              info-hub adapters        Exploration
14       ExternalSearchEngine (info-hub)   ✅ 已实现              info-hub API/CLI/DB      Exploration
15       InterestScorer (启发式 v1)        ✅ 已实现              简单规则评分              Exploration
16       RateLimiter                       ✅ 已实现              inter-query sleep        Exploration
 ── Phase 3 (中期 — 外部组件) ──
17       DeferredTaskQueue (kanban)       未实现                  无                       Boredom
18       TimerRegistry                    部分实现                无                       Boredom, Waiting
19       UpstreamFreshnessChecker         未实现                  HTTP client + 版本比较    Exploration (后续)
20       ErrorSignatureExtractor          未实现                  ErrorLog 查询             Exploration (后续)
21       SkillAuditReport                 未实现                  文件系统                   Exploration (后续)
 ── Phase 4 (已完成 — 深度认知) ──
22       TraceStore                       ✅ 已实现               JsonlTraceStore (persistence) Reflection, Meditation
23       ErrorClassifier                  未实现                  TraceStore               Reflection
24       ChainTaskDetector                未实现                  TraceStore + 规则表       Reflection
25       SilentAnomalyDetector            未实现                  TraceStore + tool schema  Reflection
26       DomainClassifier                 未实现                  关键词规则表               Reflection, Incubation
27       PendingAsyncCalls 注册表         未实现                  无                       Waiting (完整模式)
28       HeuristicStore                   部分 (procedural mem)  MemoryProvider            Meditation
29       MeditationReportWriter           ✅ 已实现              MeditationRunner::write_meditation_report() Meditation
30       Narrative 报告目录               ✅ 已实现              ~/.aman/narrative/meditation/{agent_id}/ Meditation
31       IncubationManager                ✅ 已实现              idle crate (7 tests)     Incubation
32       FeasibilityEstimator             ✅ 已实现              estimate_feasibility() (启发式) Incubation
33       IdleConfig 各子项配置            部分实现                 config crate             Daze, Boredom, Waiting, Sleep, Exploration, Meditation, Incubation
```

### YantrikDB 已覆盖的能力（原方案中需要手动实现的组件）

| 原组件 | YantrikDB 替代 |
|---|---|
| ShortTermStore (SQLite 7d TTL) | yantrikdb 内置 half-life 衰减 |
| LongTermStore (SQLite + embedding) | yantrikdb HNSW 向量索引 |
| SessionStore + SessionCompressor | `session_start/end` + `session_history` |
| DedupEngine (cosine similarity) | `think()` consolidation (已桥接) |
| QualityScorer (五维评分) | 内置 importance/valence/certainty 元数据 |
| CacheStore (文件系统 TTL) | 仍需要（文件系统操作，非 memory 范畴） |
| MemoryHealthReporter | `stats()` 方法 |
| KnowledgeGraph (手动实现) | yantrikdb 内置 knowledge graph |
| EmbeddingEngine (外部 API) | `RemoteEmbedder` — 云端 embedding API，或 `potion-multilingual-128M` 本地下载 (dim=256, 101 语言) |
| PatternExtractor (统计聚类) | `think()` pattern mining (已桥接) |
| Conflict detection (手动) | `think()` conflict scan (已桥接) |

---

## 附录 B: 建议里程碑（按实现阶段）

**Milestone 0: 基础设施**（✅ 已完成）
- [x] `MemoryProvider` trait 定义（`crates/core/src/memory.rs`）
- [x] `MemoryRecord` / `MemoryStats` / `SessionSummary` / `ThinkConfig` / `ThinkResult` 等类型
- [x] `YantrikdbProvider` 实现（`crates/memory/src/yantrikdb.rs`）
- [x] `MemoryStore` MemoryProvider 适配（in-memory 备选）
- [x] 根据 `memory.provider` 配置自动选择后端
- [x] `AgentHarness` / `AgentRuntime` 注入 MemoryProvider
- [x] `memory.llm` / `memory.embedding` 配置（config crate）
- [x] `RemoteEmbedder` — 云端 embedding（零本地下载）
- [x] `AgentIdleManager` 产生 QueueDrained（busy→empty 转换 + 断路器）
- [x] `ReflectionRunner` 实现（`crates/gateway/src/runtime/reflection.rs`）
- [x] workspace build 通过

**Milestone 1: Phase 1 实现 — Daze + Waiting + Reflection**（已完成）
- [x] Daze: 纯状态声明 (no-op)，IdleDetector 内存跟踪 metrics + UI 同步
- [x] Waiting: no-op（depth+arousal 机制已覆盖所有状态流转）
- [x] QueueDrained 生产：AgentIdleManager busy→empty 转换 + 断路器
- [x] Reflection step 4 (`session_extract`): JSONL → LLM 提取 → YantrikDB.store() + relate()
- [x] Reflection step 5 (`session_review`): 过期记忆检查
- [ ] Reflection step 1-3 (`chain_tasks`/`immediate_errors`/`lessons_learned`): TraceStore 已就绪，待实现 ChainTaskDetector + ErrorClassifier

**Milestone 2: Sleep + Exploration**（已完成）
- [x] 确定 YantrikdbProvider::think() 桥接方案 → **已桥接**
- [x] Sleep phase 1-6 完整实现 (phase 1/5 stub)
- [x] SleepRunner 架构：EventHandler + OnceLock + CancellationToken + CpuTracker
- [x] Health snapshot 存储 (phase 6)
- [x] ✅ **idle skill 系统已移除**（idle-system plugin + YAML 文件 + workflow.rs stubs）
- [x] ✅ **ExplorationRunner 实现**：EventHandler + info-hub adapters 外部搜索 + MemoryProvider 存储
- [x] info-hub `adapters` / `merge` 模块公开化，供 ExplorationRunner 使用
- [x] ExplorationRunner 单元测试通过（9 tests）
- [x] workspace build 通过（0 warnings）

**Milestone 3: Boredom + 深度 Exploration**（后续）
- [ ] `DeferredTaskQueue`（kanban 机制）
- [ ] Exploration 扩展：LLM 深度评分（通过 info-hub tools）
- [ ] Exploration 扩展：skill_audit + error_signature_extractor

**Milestone 4: Boredom + Waiting 完整**（Waiting 可能取消）
- [ ] `DeferredTaskQueue`（kanban 机制）
- [ ] `TimerRegistry`
- [ ] Boredom 完整模式
- [ ] Waiting 条件等待 — 需先确认是否有条件驱动入口的必要；若长期无此类场景，可考虑移除此状态

**Milestone 5: Meditation + Incubation**（✅ 已完成）
- [x] `TraceStore` — `JsonlTraceStore` (`crates/persistence/src/trace_store.rs`)
- [ ] `ErrorClassifier`, `ChainTaskDetector` (Reflection step 1-3，待实现)
- [x] `MeditationReportWriter` — `MeditationRunner::write_meditation_report()` + atomic write
- [x] Meditation 全流程 — 7 phases: 前置检查 + TraceStore + KG 内省 + 模式提取 + think + 报告 + 收尾
- [x] `IncubationManager` + `FeasibilityEstimator` — `crates/idle/src/incubation.rs` (7 tests)
- [x] Incubation 跨域采样 + 联想 + 灵感生成 — 5 phases (via IncubationRunner + 后台任务)
