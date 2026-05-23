# Idle Skill Execution Patch

> 目标：将每个 idle 子状态的 skill 从「只有 description 的空壳」落实为「可逐步执行的步骤」。
> 基准文档：`idle-design.md`，特别是 §14 Idle State Activity Catalog。
> 约束：本文只处理执行步骤 + 缺失组件指认，不修改 idle-design.md 的架构设计。
> **更新 (2026-05-23)**：MemoryProvider trait + YantrikdbProvider 已落地，本文据此重构核心交互。

---

## 8 个 Idle 状态全景

| 深度 | 状态 | 类型 | 核心动作 | 硬依赖 | 就绪度 |
|------|------|------|----------|--------|--------|
| 0 | **Daze** | Pipeline | 纯状态声明 (no-op)，idle 序列锚点 | 无 | ✅ 已完成 |
| 1 | **Boredom** | Pipeline | 扫描 pending 任务 + 随机浏览记忆 | deferred task queue / kanban | ❌ 等 kanban 机制 |
| 2 | **Waiting** | Pipeline | 等待外部条件满足（timer / async / 用户回复） | 无 | ✅ 立即可做 (no-op) |
| 3 | **Sleep** | Workflow | 长期记忆整合，consolidation，temporal housekeeping，索引/缓存清理 | MemoryProvider（已落地），think() 桥接 | ⚠️ 部分就绪 |
| 5 | **Exploration** | Workflow | 外部信息探索，产出 idea / 新闻 / 论文 | RSS/Atom 源，web search API，兴趣度评分器 | ❌ 等外部源 |
| 7 | **Reflection** | Pipeline | 即时复盘：连锁任务 + 错误分类 + 经验提取 + **session→YantrikDB 提取** | LLM config（已有） | ✅ 核心逻辑可做 |
| 8 | **Incubation** | Pipeline+Thread | 创意孵化 / 跨域联想，产出假设性洞察 | 综合记忆 + kanban + 新闻 + idea + 论文 | ❌ 等一切就绪 |
| 10 | **Meditation** | Workflow | 深度内省，提炼经验 → 更新启发式，think() deep scan | TraceStore，模式提取引擎，进化机制 | ❌ 等 trace store |

### 实现阶段

```
Phase 1 (现在) ── 无外部依赖，立即可做
    Daze        纯状态声明 (no-op)，idle 序列锚点 ✅ 已完成
    Waiting     纯条件检查，no-op 即可
    Reflection  lessons_learned + session_extract → YantrikDB

Phase 2 (短期) ── 桥接完成后
    Sleep       phase 1: 回填 Reflection 遗漏的 session
                phase 2: temporal housekeeping
                phase 3: 缓存清理
                phase 4: 索引监控
                phase 5: think() consolidation ← 核心价值
                phase 6: 健康报告

Phase 3 (中期) ── 引入外部组件后
    Boredom     kanban / deferred task queue
    Exploration RSS/Atom + web search

Phase 4 (远期) ── 基础设施齐备后
    Meditation  TraceStore + 模式提取 + 进化机制
    Incubation  跨域联想引擎
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

> **状态：已实现。** `MemoryProvider` trait 定义在 `crates/core/src/memory.rs`，`YantrikdbProvider` 实现在 `crates/memory/src/yantrikdb.rs`，已在 `AgentHarness` 中默认注入。

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
- **认知循环** — 内置 `think()` 机制（trigger 检测、冲突扫描、consolidation、pattern mining）—— **当前 YantrikdbProvider 暂未桥接，`think()` 返回空结果，待后续设计**
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
| `think()` | `think()` (yantrikdb) | **暂未桥接** — yantrikdb 内置完整认知循环，Provider 默认返回空 |

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

> **设计注**：YantrikDB 的 `think()` 内部执行：trigger 过期 → decay/consolidation/conflict/temporal-drift/redundancy/relationship-insight/valence-trend/entity-anomaly 八种 trigger 检测 → 冲突扫描 → consolidation（合并相似记忆）→ pattern mining。当前 `YantrikdbProvider::think()` 使用 trait 默认空实现。桥接方案待后续确定——可能直接透传 `ThinkConfig` 参数到 yantrikdb 的 `think(&self, config: &ThinkConfig)`。

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

**路由**: `workflow:idle-sleep`
**类型**: Workflow（异步，附 cancel_token 监控）
**可打断**: 是（idle_cancel_token，checkpoint 保存）
**Arousal**: Engaged (×0.5)
**MemoryProvider 依赖**: 全部 CRUD + session + temporal + think + stats

> **核心变更 (2026-05-23)**：Sleep 的所有记忆操作现在通过 `MemoryProvider` trait 接口执行，默认后端为 `YantrikdbProvider`。YantrikDB 内置了 temporal decay、HNSW 向量搜索、知识图谱和 consolidation 引擎——原先需要手动实现的 STM/LTM 迁移、TTL 清理、去重（dedup）现在由 yantrikdb 内部处理。Sleep 的职责从「手动管理存储层」变为「编排 cognitive housekeeping 任务」。
>
> **职责调整**：session→YantrikDB 提取的主路径移到了 Reflection（QueueDrained 后即时触发，上下文最新鲜）。Sleep phase 1 仅做回填（Reflection 遗漏的 session）。Sleep 的核心价值现在集中在 consolidation（phase 5 think()）和 housekeeping（phase 2/3/4/6）。

### 执行步骤

```
┌─────────────────────────────────────────────────────┐
│ Sleep Workflow (max_cpu_seconds=60)                  │
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
        // 当前 YantrikdbProvider::think() 返回空 ThinkResult
        // 待桥接后，result 将包含:
        //   - consolidation_count: 合并了多少相似记忆
        //   - conflicts_found: 检测到多少矛盾
        //   - triggers_fired: 触发了多少认知 trigger
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
| `MemoryProvider.think()` 桥接 | **未实现** | YantrikdbProvider 的 `think()` 当前返回空结果。yantrikdb 内部有完整的 `think()` 实现（八种 trigger + consolidation + conflict scan + pattern mining）。需要后续确定桥接方案——直接透传 ThinkConfig 参数，或将 think 作为独立的后台线程运行 |
| CPU 时间追踪 | **未实现** | 需要 per-workflow 的 CPU 时间累加器。`max_cpu_seconds=60` 的约束需要 enforce |
| CacheStore（文件系统 TTL） | **未实现** | Phase 3 缓存清理操作文件系统，不经过 MemoryProvider。可用 `std::fs` + mtime 检查实现 |
| Health snapshot 存储 | **未实现** | Phase 6 的健康快照需要持久化。建议 SQLite 表或 `~/.aman/health/` 下 JSON 文件 |

### 优先级

**Phase 2** — MemoryProvider 已就绪。phase 1/2/4/6 可立即实现（不依赖 think 桥接）。phase 5（consolidation）需要 `think()` 桥接后才能发挥完整作用——当前可先记录空结果，桥接后自动提升。session 提取的主路径已移至 Reflection，Sleep 仅做回填。

---

## 5. Exploration

**路由**: `workflow:idle-exploration`
**类型**: Workflow（异步，附 cancel_token）
**可打断**: 是（断点保存）
**Arousal**: Engaged (×0.0)
**MemoryProvider 依赖**: `recall()` (语义搜索记忆缺口)、`search_entities()` (外部实体发现)、`stale_memories()` (找到遗忘的知识)

### 执行步骤

```
phase 1: 收集好奇心查询
    CHECKPOINT
    1.1 memory_gaps — 通过 MemoryProvider 找出知识缺口:
        // 查询近期（7d）内 recall 失败或低分的历史
        stale = provider.stale_memories(agent_id, days=7).await
        gaps = stale.iter()
            .filter(|m| m.importance > 0.4)  // 重要但长期未访问
            .map(|m| format!("latest information about: {}", m.content))
            .collect()
        queries += gaps

    1.2 entity_gaps — 搜索知识图谱中的孤立实体:
        // 发现 degree=0 或缺少最新信息的实体
        entities = provider.search_entities("*", limit=10).await
        for entity in entities:
            profile = provider.entity_profile(&entity).await
            if profile.edge_count == 0 || profile.related_entities.is_empty():
                queries += f"what is {entity} and how does it relate to other things?"

    1.3 skill_audit:
        skills = skill_registry.list_all()
        for skill in skills:
            freshness = check_upstream_freshness(skill.upstream_url, skill.last_checked)
            if freshness == Stale:
                queries += f"latest {skill.name} documentation changes"

    1.4 recent_failures:
        failures = error_log.query(range=recent_7d, limit=10)
        for f in failures:
            signatures = extract_error_signature(f)
            queries += signatures.iter().map(|s| f"{s} solution fix").collect()

    1.5 去重 + 截断：queries.unique().truncate(30)

phase 2: 执行外部查询
    CHECKPOINT
    2.1 初始化 rate_limiter(api_rate_per_minute=10)
    2.2 results = []
    2.3 for query in queries:
        rate_limiter.acquire()  // 阻塞直到配额可用
        response = external_search(query)  // → web_search / API call
        if response.error:
            if response.is_rate_limit:
                break  // 停止本轮，保留 queries 给下一轮
            continue
        2.4 兴趣度评分:
        relevance = semantic_similarity(query, response.title + response.snippet)
        freshness = days_since(response.published_date)
        actionability = contains_actionable_info(response.content)
        score = relevance * 0.4 + (1.0 / (freshness + 1)) * 0.3 + actionability * 0.3
        2.5 if score > config.exploration.min_interest_score:
            results.push({ query, response, score })
    2.6 按 score 降序，取 top max_results(20)

phase 3: 处理结果 — 写入 MemoryProvider
    CHECKPOINT
    3.1 分类: MemoryGapResolution | SkillUpdate | FailureSolution | GeneralSignal
    3.2 对高价值信号 (score > high_value_threshold):
        provider.store(agent_id,
            format!("[Exploration] {}: {}", category, response.summary),
            vec!["exploration".into(), category.into()]
        )
        包装为 Event { priority: Low, source: "idle.exploration", payload }
        → publish to Agent Local EventBus
    3.3 对 MemoryGapResolution:
        provider.store(agent_id, resolution_content, vec!["memory_gap_resolved".into()])
    3.4 对 SkillUpdate: 写入 skill_audit_report（文件系统）
    3.5 对 FailureSolution:
        provider.store_procedural(agent_id, &solution_name, &solution_schema, "fix").await

phase 4: 降级模式 (on_quota_exhausted = fallback)
    CHECKPOINT
    4.1 如果 phase 2 因速率限制提前中断:
        切换到本地探索模式:
            - 从 MemoryProvider 做语义搜索:
              local = provider.recall(agent_id, "interesting new development", limit=20).await
            - 搜索 Tantivy 索引中未读或标记为 "待深入" 的条目
            - 产出 LocalDiscovery 事件（价值通常低于外部信号）
```

### 缺失组件

| 组件 | 状态 | 说明 |
|------|------|------|
| `ExternalSearchEngine` | **未实现** | 统一的外部查询接口。需要支持多种后端：(a) web_search (Brave/Google API)，(b) RSS feed reader，(c) GitHub API，(d) 自定义 API endpoint。建议放在 `crates/tool/` 下作为 built-in tool |
| `InterestScorer` | **未实现** | 三维评分函数（相关性、新鲜度、可操作性）。相关性需要语义相似度——yantrikdb 的 embedding 可复用于此 |
| `SkillRegistry::list_all()` | **部分实现** | `crates/skill/` 有技能加载，但不确定是否有 `list_all()` API + `upstream_url` 元数据字段 |
| `UpstreamFreshnessChecker` | **未实现** | 检查 upstream URL（GitHub release、docs RSS、npm/pip registry）是否有更新。需要 HTTP client + 版本比较 |
| `ErrorSignatureExtractor` | **未实现** | 从错误日志中提取可搜索的错误签名 |
| `ErrorLog` 查询接口 | **未实现** | `crates/persistence/` 有 WAL/DLQ，但需要按时间范围 + 类型过滤的错误查询 API |
| `RateLimiter` | **未实现** | 令牌桶或滑动窗口速率限制器。需要支持 per-minute 配置 |
| `exploration.min_interest_score` 配置 | **未实现** | 需新增到 IdleConfig。默认值建议：0.4 |

### 优先级

**Phase 3** — MemoryProvider 能力已就绪（`recall`, `search_entities`, `entity_profile`, `stale_memories`）。主要缺失是 `ExternalSearchEngine`（网络 I/O）和 RSS/Atom 源。

---

## 6. Meditation

**路由**: `workflow:idle-meditation`
**类型**: Workflow（异步，附 cancel_token）
**可打断**: 是（丢弃当前产出，temp+rename 保证上一个完成的报告安全）
**Arousal**: Engaged (×0.0)
**MemoryProvider 依赖**: `entity_profile()` (内省实体)、`get_edges()` (关系分析)、`surface_procedural()` (策略回顾)、`think()` (认知循环)、`stats()` (健康检查)

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
        // 当前返回空结果，桥接后:
        //   - triggers_fired: decay/conflict/relationship_insight 等 trigger 数量
        //   - consolidation_count: 合并了多少经验
        //   - conflicts_found: 检测到多少矛盾

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
| 模式提取 | 手动统计聚类 | `surface_procedural()` embedding 匹配 + `think()` pattern mining (待桥接) |
| KG 冲突检测 | 手动矛盾检测 | yantrikdb 内置 claim_conflicts + entity_conflicts (think 触发) |
| 内省报告 | MeditationReportWriter | 聚合 `stats()` + `entity_profile()` + `think()` 结果 |

### 缺失组件

| 组件 | 状态 | 说明 |
|------|------|------|
| **Trace Store** | **不存在** | Meditation 和 Reflection 的核心依赖。需要记录每条 task 的完整执行路径（决策点、工具调用、错误恢复）。建议放在 `crates/persistence/` 中独立实现 |
| `MemoryProvider.think()` 桥接 | **未实现** | 同 Sleep phase 5。yantrikdb 有完整 `think()`，Provider 层暂未桥接 |
| `MeditationReportWriter` | **未实现** | Markdown 报告生成 + atomic write。atomic write 逻辑可复用为一个通用工具函数 |
| Narrative 报告目录 | **未实现** | `~/.aman/narrative/meditation/` 目录创建 + 权限检查 |
| `meditation.review_depth` 配置 | **未实现** | 需新增到 IdleConfig。默认值建议：20 |

### 优先级

**Phase 4** — 需要 TraceStore（独立基础设施）+ 模式提取引擎 + 进化机制。MemoryProvider 能力已就绪（entity_profile, get_edges, surface_procedural, stats）。`think()` 桥接后将大幅提升价值（冲突检测 + pattern mining）。

---

## 7. Reflection

**路由**: `pipeline:reflection`
**类型**: Pipeline（select! 模式，60s timeout）
**可打断**: 是（select! 被新事件抢先）
**触发**: Dispatcher 发布 QueueDrained 事件
**MemoryProvider 依赖**: `store()` (写入提取摘要)、`relate()` (创建实体关联)、`session_history()` (回顾近期会话)、`stale_memories()` (找到未处理项)

### 执行步骤

```
step 0: 从 QueueDrained 事件读取 reflection_consecutive_count
        if count >= 10:
            log!("Reflection breaker: full skip + cooldown")
            return Empty  // 完全跳过
        skip_lessons = count >= 5

step 1: chain_tasks (始终执行)
    1.1 从 trace_store 加载 last_trace_id 对应的 trace
    1.2 分析 trace 中的 task 完成状态:
        - task 类型是什么？（deploy / codegen / research / review / ...）
        - 是否有未完成的 sub-task？
        - 输出是否触发了隐式依赖？（如 "部署到 staging → 谁来部署到 prod？"）
    1.3 生成连锁任务候选:
        for candidate in chain_candidates:
            task = {
                parent_trace_id: last_trace_id,
                description: candidate.description,
                priority: candidate.is_blocking ? High : Medium,
                dedup_key: hash(candidate.description)
            }
    1.4 每个候选去重（dedup_key 在最近 N 条任务中已存在 → 跳过）
    1.5 将连锁任务发布为 Event 到 Local EventBus

step 2: immediate_errors (始终执行)
    2.1 从 trace_store 加载 last_trace_id 的 error 子记录
    2.2 错误分类:
        - Recovered: 已恢复的错误（如 retry 成功）→ 标记为 "已验证恢复路径"
        - Unrecovered: 未恢复的错误 → 升级 priority
        - Warning: 非致命警告 → 聚合统计
        - Silent: 工具返回了成功但输出异常 → 标记为需要人工审查
    2.3 对 Unrecovered 错误:
        emit ErrorEvent { severity: High, trace_id, error_summary }
    2.4 对 Silent 异常:
        emit ReviewEvent { severity: Medium, trace_id, anomaly_description }

step 3: lessons_learned (count < 5 时执行)
    3.1 从 trace_store 加载 last_trace_id 完整 trace
    3.2 提取经验:
        - 决策质量: 选择了非最优路径？为什么？
        - 惊喜发现: 发现了没想到的解决方案？
        - 可复用模式: 这个 task 的解决模式是否可以泛化？
    3.3 写入 MemoryProvider:
        provider.store(agent_id,
            format!("[Lesson] {}: {}", lesson_type, extracted_lesson),
            vec!["lesson".into(), "reflection".into(), domain.into()]
        )
    3.4 产出: 无 Event（经验写入 store 即完成，不需要立即处理）

step 4: session_extract ★ (当次 session 结束后触发，主提取路径)
    4.1 从 SessionStore 加载当次 session 的完整 JSONL
    4.2 使用 memory.llm 配置的模型提取结构化摘要:
        - 对话意图 (intent)
        - 关键决策 (decisions[])
        - 产出 (outputs[])
        - 错误及恢复 (errors[])
        - 标签 (tags[])
    4.3 写入 YantrikDB:
        provider.store(agent_id, summary_json, vec!["session_compressed".into(), session_id])
    4.4 对摘要中涉及的实体创建知识图谱关联:
        for entity in extracted_entities:
            provider.relate(&entity, &session_id, "appears_in")
    4.5 标记 session 为 compressed（SessionStore 操作）
    4.6 如果 60s timeout 不够（长对话）→ spawn 后台任务继续
    // 注：这是 session→YantrikDB 的主路径。Reflection 在 QueueDrained 后立即触发，
    //     对话上下文最新鲜，LLM 提取质量最高。Sleep 仅回填遗漏。

step 5: session_review (可选，仅当 reflection_consecutive_count == 0)
    5.1 回顾近期 session 摘要:
        sessions = provider.session_history(agent_id, limit=5).await
    5.2 检查是否有未跟进的 stale 记忆:
        stale = provider.stale_memories(agent_id, days=14).await
        if !stale.is_empty():
            log!("Reflection: {} stale memories need attention", stale.len())
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
| `TraceStore` | **未实现** | 同 Meditation。Reflection 和 Meditation 共享同一套 trace 存储 |
| `ChainTaskDetector` | **未实现** | 分析 task 完成状态 → 检测连锁任务。需要 task 类型分类器 + 隐式依赖知识库。初期可用硬编码规则表（task_type → possible_chain_tasks） |
| `ErrorClassifier` | **未实现** | 错误四分类（Recovered/Unrecovered/Warning/Silent）。依赖 trace 中的 error 元数据 richness |
| `SilentAnomalyDetector` | **未实现** | 检测 "工具返回成功但输出异常" 的情况。需要 per-tool 的输出 schema + 异常检测规则 |
| `DomainClassifier` | **未实现** | 任务领域分类。初期可用简单的关键词匹配（deploy → ops, codegen → dev, review → qa） |
| `SessionExtractor` (step 4) | **已配置，未实现** | 使用 `memory.llm` 配置的模型提取 session 结构化摘要。LLM config 已就绪，需实现 JSONL 加载 + LLM 调用 + 结果写入 YantrikDB 的逻辑。timeout 超时后 spawn 后台任务 |

### 优先级

**Phase 1** — 空闲系统的入口（QueueDrained 后立即触发）。`session_extract`（step 4）和 `lessons_learned`（step 3）是核心价值，LLM config 已就绪可直接实现。`chain_tasks` 和 `immediate_errors` 初期可用 stub（无 TraceStore 时用规则表），完整实现等 Phase 4。

---

## 8. Incubation

**路由**: `pipeline:idle-incubation` + 独立后台线程
**类型**: Pipeline 触发 → 启动后台线程 → Pipeline 立即返回
**可打断**: 否（纯后台，仅 shutdown 时取消）
**Arousal**: Engaged (×0.1)
**MemoryProvider 依赖**: `recall()` (随机跨域采样)、`search_entities()` (跨域实体发现)、`relate()` (创建跨域链接)、`surface_procedural()` (策略联想)、`think()` (认知循环)

> **注意**：idle-design.md §13 Open Questions 明确说 Incubation 的灵感机制在 idle-design 的 Phase 1 跳过。本文的 Phase 1 指 Daze/Waiting/Reflection。Incubation 实现排在 Phase 4（远期），以下为完整设计规格供远期参考。

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
| 跨域推论 | manual analogy | yantrikdb 内置 `generate_candidate_inferences()` (待 think 桥接) |

### 缺失组件

| 组件 | 状态 | 说明 |
|------|------|------|
| `IncubationManager` | **已设计，未实现** | `crates/idle/src/incubation.rs` 已设计结构体。需要实现 spawn/shutdown 逻辑 |
| `FeasibilityEstimator` | **未实现** | 假设可行性评估。初期可用简单规则（有已知跨域成功案例 → 高，有已知障碍 → 低） |
| Cross-domain pair generation | **未实现** | 从 recall 结果中按 domain 字段分组后生成跨域对。MemoryRecord.domain 已提供分组依据 |
| `MemoryProvider.think()` 桥接 | **未实现** | 同 Sleep/Meditation。Incubation 的 think 用于 relationship_insight trigger |
| `incubation.incubation_threshold` 配置 | **未实现** | 需新增到 IdleConfig。默认值建议：0.7 |
| `incubation.high_value_threshold` 配置 | **未实现** | 同上。默认值建议：0.85 |

### 优先级

**Phase 4** — 依赖综合记忆 + kanban + 新闻 + idea + 论文等全部数据源就绪后才能实施。MemoryProvider 能力已就绪（`recall`, `search_entities`, `relate`, `surface_procedural`）。

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
 ── Phase 2 (短期 — 桥接 + Sleep + Reflection) ──
 7       SessionExtractor (Reflection)    LLM config 已就绪       memory.llm + SessionStore Reflection
 8       think() 桥接 (yantrikdb→Provider) 未实现                YantrikdbProvider        Sleep, Meditation, Incubation
 9       CacheStore (文件系统 TTL)         未实现                  无                       Sleep (phase 3)
10       Health snapshot 存储              未实现                  SQLite 或 JSON 文件       Sleep (phase 6)
11       CPU time tracker                 未实现                  无                       Sleep, Exploration, Meditation
12       AtomicWrite                      未实现                  无                       全局复用
 ── Phase 3 (中期 — 外部组件) ──
13       DeferredTaskQueue (kanban)       未实现                  无                       Boredom
14       TimerRegistry                    部分实现                无                       Boredom, Waiting
15       RateLimiter                      未实现                  无                       Exploration
16       ExternalSearchEngine             未实现                  HTTP client              Exploration
17       UpstreamFreshnessChecker         未实现                  HTTP client + 版本比较    Exploration
18       InterestScorer                   未实现                  embedding / BM25         Exploration
19       ErrorSignatureExtractor          未实现                  ErrorLog 查询             Exploration
20       SkillAuditReport                 未实现                  文件系统                   Exploration
 ── Phase 4 (远期 — 深度认知) ──
21       TraceStore                       未实现                  无 (crates/persistence/) Reflection, Meditation
22       ErrorClassifier                  未实现                  TraceStore               Reflection
23       ChainTaskDetector                未实现                  TraceStore + 规则表       Reflection
24       SilentAnomalyDetector            未实现                  TraceStore + tool schema  Reflection
25       DomainClassifier                 未实现                  关键词规则表               Reflection, Incubation
26       PendingAsyncCalls 注册表         未实现                  无                       Waiting (完整模式)
27       HeuristicStore                   部分 (procedural mem)  MemoryProvider            Meditation
28       MeditationReportWriter           未实现                  TraceStore + atomic write Meditation
29       Narrative 报告目录               未实现                  文件系统                   Meditation
30       IncubationManager                已设计，未实现          MemoryProvider + embedding Incubation
31       FeasibilityEstimator             未实现                  DomainClassifier          Incubation
32       IdleConfig 各子项配置            部分实现                 config crate             Daze, Boredom, Waiting, Sleep, Exploration, Meditation, Incubation
```

### YantrikDB 已覆盖的能力（原方案中需要手动实现的组件）

| 原组件 | YantrikDB 替代 |
|---|---|
| ShortTermStore (SQLite 7d TTL) | yantrikdb 内置 half-life 衰减 |
| LongTermStore (SQLite + embedding) | yantrikdb HNSW 向量索引 |
| SessionStore + SessionCompressor | `session_start/end` + `session_history` |
| DedupEngine (cosine similarity) | `think()` consolidation (待桥接) |
| QualityScorer (五维评分) | 内置 importance/valence/certainty 元数据 |
| CacheStore (文件系统 TTL) | 仍需要（文件系统操作，非 memory 范畴） |
| MemoryHealthReporter | `stats()` 方法 |
| KnowledgeGraph (手动实现) | yantrikdb 内置 knowledge graph |
| EmbeddingEngine (外部 API) | `RemoteEmbedder` — 云端 embedding API，或 `potion-multilingual-128M` 本地下载 (dim=256, 101 语言) |
| PatternExtractor (统计聚类) | `think()` pattern mining (待桥接) |
| Conflict detection (手动) | `think()` conflict scan (待桥接) |

---

## 附录 B: 建议里程碑（按实现阶段）

**Milestone 0: 基础设施**（✅ 已完成）
- [x] `MemoryProvider` trait 定义（`crates/core/src/memory.rs`）
- [x] `MemoryRecord` / `MemoryStats` / `SessionSummary` / `ThinkConfig` / `ThinkResult` 等类型
- [x] `YantrikdbProvider` 实现（`crates/memory/src/yantrikdb.rs`）
- [x] `AgentHarness` / `AgentRuntime` 注入 YantrikdbProvider 作为默认 MemoryProvider
- [x] `memory.llm` / `memory.embedding` 配置（config crate）
- [x] `RemoteEmbedder` — 云端 embedding（零本地下载）
- [x] workspace build 通过

**Milestone 1: Phase 1 实现 — Daze + Waiting + Reflection**（1–2 周）
- [x] Daze skill: 纯状态声明 (no-op)，idle 序列锚点。IdleDetector 内存跟踪 metrics + UI 同步
- [x] idle→Daze→skill dispatch 链路验证通过
- [ ] Waiting skill: 纯条件检查 (no-op stub)
- [ ] Reflection step 4 (`session_extract`): JSONL → LLM 提取 → YantrikDB.store() + relate()
- [ ] Reflection step 3 (`lessons_learned`): 经验提取 → YantrikDB.store()
- [ ] Reflection step 1 (`chain_tasks`): stub 实现（无 TraceStore 时用规则表）
- [ ] Reflection step 2 (`immediate_errors`): stub 实现
- [ ] 验证: QueueDrained → Reflection → session JSONL → LLM 提取 → YantrikDB 可见

**Milestone 2: think() 桥接 + Sleep**（1–2 周）
- [ ] 确定 YantrikdbProvider::think() 桥接方案
- [ ] 实现桥接代码
- [ ] Sleep phase 1: 会话压缩回填
- [ ] Sleep phase 2: temporal housekeeping
- [ ] Sleep phase 3: 缓存清理
- [ ] Sleep phase 4: 索引监控
- [ ] Sleep phase 5: think() consolidation
- [ ] Sleep phase 6: 健康报告
- [ ] 验证: think() → consolidation_count > 0 → 记忆合并
- [ ] 验证: Sleep → provider.stale_memories() → 清理/标记

**Milestone 3: Exploration**（1–2 周）
- [ ] `ExternalSearchEngine` v0（web_search only）
- [ ] `InterestScorer` v0（复用 yantrikdb embedding 做语义相似度）
- [ ] `RateLimiter`
- [ ] Exploration skill 全流程（memory_gaps + entity_gaps + 外部搜索 + 结果存储）
- [ ] 验证: Exploration → provider.recall() → 外部搜索 → provider.store()

**Milestone 4: Boredom + Waiting 完整**
- [ ] `DeferredTaskQueue`（kanban 机制）
- [ ] `TimerRegistry`
- [ ] Boredom 完整模式、Waiting 条件等待

**Milestone 5: Meditation + Incubation（远期）**
- [ ] `TraceStore`（`crates/persistence/`）
- [ ] `ErrorClassifier`, `ChainTaskDetector`
- [ ] `MeditationReportWriter`
- [ ] Meditation 全流程（trace 加载 + KG 内省 + 模式提取 + think + 报告）
- [ ] `IncubationManager` + `FeasibilityEstimator`
- [ ] Incubation 跨域采样 + 联想 + 灵感生成
- 时间待定，依赖基础设施齐备
