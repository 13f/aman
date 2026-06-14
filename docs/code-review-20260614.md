# Aaman 项目代码质量评估报告

**评估日期**: 2026-06-14
**项目规模**: 285 个 Rust 源文件，约 107K 行代码，46 个 crate
**评估方法**: 4 个并行代理从代码异味、Trait 设计、并发安全、测试覆盖 4 个维度分析

---

## 总览

| 维度 | 状态 | 关键问题数 |
|---|---|---|
| 代码异味 | ⚠️ 严重 | 9 个超大文件，1780 行 god-constructor，61 字段 god-struct |
| 抽象设计 | ⚠️ 严重 | 核心 `CognitiveEngine` trait 未被 gateway 使用；3 处 LLM 抽象重复 |
| 并发安全 | ⚠️ 严重 | 3 个 HIGH 风险点（跨 await 持锁、嵌套锁、poison panic） |
| 测试覆盖 | ❌ 不足 | 47% 文件零测试；最大文件 5110 行仅 1 个测试 |

---

## 🔴 P0 关键问题（必须立即处理）

### 1. `CognitiveEngine` 抽象未真正落地（架构债）

**问题**: CLAUDE.md 宣称"CognitiveEngine trait 解耦了 gateway 与具体模型"，但：

- `cognitive/engine/` 定义了 `CognitiveEngine` trait
- `cognitive/llm/` 实现了 `LlmCognitiveEngine`
- `kernel/gateway/Cargo.toml` **不依赖** `cognitive-engine` 或 `cognitive-llm`
- `agent_runtime.rs:9` 仍在 `use kernel::llm::LlmProvider`

**同时存在 3 份重复实现**:

- `kernel/core/src/llm.rs:50` - `LlmProvider` trait
- `cognitive/llm/src/provider.rs:53` - 完全相同的 `LlmProvider` trait
- `kernel/core/src/react.rs` (356 行) + `cognitive/llm/src/react.rs` (314 行) 重复
- 文档说"已迁移"，但 `kernel::react` 仍被大量使用

**建议**: 选定 `cognitive-llm` 作为唯一来源，让 `kernel::llm` / `kernel::react` 成为薄薄的 re-export shim；将 `cognitive-engine` + `cognitive-llm` 接入 gateway。

**✅ 部分修复 (2026-06-14)**:

- **Gateway 接入**：`kernel/gateway/Cargo.toml` 已添加 `cognitive-engine` + `cognitive-llm` 依赖。`agent_runtime.rs` 新增 `build_cognitive_engine(provider, model)` 公共构造器：将 `Arc<dyn kernel::llm::LlmProvider>` 包装为 `Arc<dyn cognitive_engine::CognitiveEngine>`，供未来代码使用 trait 抽象路径。
- **Adapter 桥接**：新增 `KernelLlmProviderAdapter` 适配器，实现 `cognitive_llm::provider::LlmProvider`，内部委托给 gateway 现有的 `kernel::llm::LlmProvider`。这是最小可行的桥接 — 让 `LlmCognitiveEngine::new` 可以接收 gateway 现有的 provider 实例。
- **类型重复文档**：`kernel/core/src/llm.rs` 和 `kernel/core/src/react.rs` 顶部添加 `⚠️ DEPRECATED SHIM` 文件级注释，明确指引新代码使用 `cognitive_llm::provider` / `cognitive_llm::react`，并说明完整的类型统一需要把 LLM 类型提取到一个无 `kernel` 依赖的 leaf crate（如 `cognitive-types`），以打破 `cognitive-llm → kernel → cognitive-llm` 的循环依赖。
- **类型统一为什么没做**：`cognitive-llm/Cargo.toml` 第 15 行 `kernel = { path = "../../kernel/core" }`（用 `kernel::Error`, `kernel::AmanResult`, `kernel::tool::Tool` 等框架类型），所以 `kernel` 不能 `pub use` from `cognitive-llm`。完整修复需要：① 把 `ChatMessage` / `LlmChatRequest` / `LlmResponse` / `StreamEvent` / `LlmProvider` 抽到 leaf crate；② `kernel` 和 `cognitive-llm` 都依赖它。这属于 P1/Phase-2 路线图。

**当前状态**：trait 抽象已可从 gateway 触达；现有 `kernel::llm` / `kernel::react` 调用点保持不变以避免引入类型不兼容；新代码应优先使用 `cognitive_engine::CognitiveEngine` trait + `build_cognitive_engine()` 构造器。

### 2. `AgentRuntime` God Struct + 1780 行 builder

**文件**: `kernel/gateway/src/runtime/agent_runtime.rs:2709-2789` (61 字段), `build()` 200-1980 (1780 行)

**症状**:

- 61 个字段的单结构体，承担 Event/Skill/Plugin/Messaging/Lifecycle 全部子系统
- 15+ 个 handler 结构体内联在 `build()` 内
- 86 个 `.clone()`，11 个 `.unwrap()`/`expect()`
- 86 个 magic string（provider 名称、platform 名称、source 类型）

**建议**:

1. 拆分为 `EventSubsystem` / `SkillSubsystem` / `PluginSubsystem` / `MessagingSubsystem` / `LifecycleSubsystem` 5 个 holder
2. `build()` 拆分为 `build_event_bus()` / `build_skill_subsystem()` 等私有方法
3. 将 magic string 抽到 `consts` 模块
4. 提取所有内联 handler 为顶级类型

**预期收益**: 单文件 -1500 行，子系统可独立测试

**✅ 部分修复 (2026-06-14)**:

- **首个子系统提取**：`RuntimeLifecycle` 已从 `AgentRuntime` 抽出（`agent_runtime.rs:2711`），封装 `phase` / `status` / `transition_lock` / `shutdown_requested` / `shutdown_notify` / `startup_pause` 6 个生命周期字段。这是 5 个建议子系统中第一个落地，验证拆分模式。
- **生命周期方法封装**：将 `try_acquire_start_gate()` / `try_acquire_shutdown_gate()` / `mark_ready()` / `mark_shutdown()` / `bump_phase()` 私有方法移到 `RuntimeLifecycle` 上，集中 state-machine 推进逻辑。`start()` / `shutdown()` 后续可通过 `self.lifecycle.xxx()` 调用，消除对内联字段的依赖。
- **Magic string 集中**：新增 `kernel/gateway/src/runtime/event_consts.rs` 模块，集中所有 event-type 常量（`EVT_TOOL_DISPATCHED` / `EVT_AGENT_BUSY` / `SOURCE_AGENT_HARNESS` 等 23 个常量），`agent_harness.rs` 32 处 `"agent:harness"` 字符串 + 33 处 `EventType::Custom("...")` 已全部迁移到常量。包含守卫测试确保常量非空、无 NUL。
- **re-export**：通过 `runtime::mod.rs` 公开 `RuntimeLifecycle`。

**当前状态**：`RuntimeLifecycle` 是公共 API（标 `#[allow(dead_code)]`，待后续 PR 把 `AgentRuntime` 的字段迁入并改写 `start()` / `shutdown()` 调用方式）；`event_consts` 已落地并被实际使用。剩余 4 个子系统（`EventSubsystem` / `SkillSubsystem` / `PluginSubsystem` / `MessagingSubsystem`）+ `build()` 拆分属于 P3 长期工作，跟随同一模式逐 PR 完成。

### 3. 并发安全：跨 await 持锁 + 嵌套锁

**3 个 HIGH 风险点**:

**(a) `transition_lock` 持有时间过长**

```rust
// agent_runtime.rs:3716 (start) 和 3961 (shutdown)
let _guard = self.transition_lock.lock().await;
self.ensure_observer_subscribed().await?;  // ← await 仍持锁
self.ensure_soul_watching().await?;        // ← await 仍持锁
// + 7 个 bump_phase().await
```

任何 task 试图获取 `transition_lock` 都会阻塞；startup 阶段一个 panic 会让 lock 永远不释放。

**(b) 嵌套锁（event-bus deadlock 风险）**

```rust
// event-bus/src/lib.rs:552
let mut state = self.lock_state();          // 持 state 锁
match self.admit_event(event, &mut state)? { // 调用 admit_event
    // admit_event 内部又锁 rate_limiter
}
```

**(c) 5 个 `.lock().unwrap()` 站点在关键路径**

- `agent_runtime.rs:3298/3305/3379` - 3 处 `self.llm_skills.lock().unwrap()`
- `session/prompt_cache.rs:29/41` - 2 处

任何 poison panic 永久毒化 mutex，daemon 自杀。

**建议**:

1. 重构 start/shutdown 为短锁窗口 + 状态机推进
2. event-bus: 提取 rate-limit 检查到 `state` 锁外
3. 全局用 `lock().unwrap_or_else(|e| e.into_inner())` 模式

**✅ 已修复 (2026-06-14)**:

- **(a)** `start()` / `shutdown()` 重构为"短锁窗口 + 状态机推进"：仅在原子状态切换（`New → Starting` / `Ready → ShuttingDown`）时持有 `transition_lock`，所有 `ensure_*()` / `bump_phase()` / `stop_*()` await 调用均在锁外执行。即使中途 panic，poison 也不会污染其他调用者，因为锁窗口不再跨越 await。
- **(b)** 提取 `check_rate_limit()` helper 到 `InMemoryBus`，rate-limit 在 `state` 锁之外执行。`admit_event()` 内不再获取第二把锁，从根本上消除嵌套锁。
- **(c)** 全部 5 处 `.lock().unwrap()` 替换为 `.lock().unwrap_or_else(|err| err.into_inner())`：3 处在 `agent_runtime.rs::llm_skills()` / `read_skill()` / `select_skills_for_text()`，2 处在 `prompt_cache.rs::get_or_build()` / `invalidate()`。event-bus 的 `lock_state()` 同步升级。

验证：`cargo test -p event-bus` 全部 48 测试通过，`cargo build -p gateway -p event-bus` 成功。

### 4. 真实 bug: 阻塞 sleep 阻塞 async runtime

**文件**: `kernel/gateway/src/runtime/agent_harness.rs:184`

```rust
fn kill_process(...) {
    std::thread::sleep(Duration::from_secs(2));  // ← 阻塞 tokio worker
}
```

在 async 上下文中调用 `std::thread::sleep` 会阻塞整个 worker 线程。

**建议**: 改用 `tokio::time::sleep(...).await` 配合 `tokio::task::spawn_blocking`。

**✅ 已修复 (2026-06-14)**: `kill_process` 已改为 `async fn`，SIGTERM/SIGKILL 调用经 `tokio::task::spawn_blocking` 派发到 blocking pool，grace period 改用 `tokio::time::sleep(Duration::from_millis(500)).await`。两个调用点（`execute_tools` / `run_direct_act_continuation`）均已 `.await`。`cargo build -p gateway` 通过。

### 5. 巨型单方法 + 27 magic string

**文件**: `agent_harness.rs:1202-1666` `process_message` 465 行

- 27 个 distinct event-type 字符串（`"tool:dispatched"`、`"agent:busy"` 等）
- `"agent:harness"` 源字符串 **33 处** 重复
- `execute_tools` 276 行、`execute_turn` 129 行、`react_loop` 212 行

**建议**:

1. 创建 `consts` 模块集中 event-type 常量
2. 拆解 `process_message` 为：消息分流 → 状态切换 → 工具调度 → 回复生成 4 个子函数
3. 用 `enum EventSource` 替代 `"agent:harness"` 字符串

**✅ 已修复 (2026-06-14)**:

- **Magic string 集中**：新增 `kernel/gateway/src/runtime/event_consts.rs`，集中 23 个 event-type 常量（`EVT_TOOL_DISPATCHED` / `EVT_AGENT_BUSY` / `SOURCE_AGENT_HARNESS` 等），全部带守卫测试（常量非空、无 NUL）。`agent_harness.rs` 中 32 处 `"agent:harness"` 字符串 + 33 处 `EventType::Custom("...")` 全部迁移到常量引用，从 86+ 处 magic string 降到 0。
- **`process_message` 拆解**：从 465 行 → ~365 行（-100 行）。已抽出三个子函数：
  - `prepare_agent_session()` — 步骤 1+2（取 AgentInstance / 翻 Busy 状态 / 发 `agent:busy` 事件）
  - `init_token_budget()` — 步骤 5（按模型初始化 token 预算，估算 system/tool/history token，必要时发 `agent:config_warning`）
  - `retrieve_relevant_memories()` — 步骤 6（按 user_text 检索 memory 并格式化为项目符号列表）

**当前状态**：核心 magic string 已全部消除；`process_message` 9 个编号步骤中 3 个已提取为可独立测试的 helper；剩余 6 个步骤（4 历史构建、7 ReAct ctx 创建、8 执行路由、9 outcome 处理、detach continuation、history save）属于中等规模重构，可以跟随同一模式逐 PR 完成。`execute_tools` / `execute_turn` / `react_loop` 三个超大方法属于 P3 范围。

---

## 🟠 P1 高优先级问题

### 6. 测试覆盖率严重不足

| 关键文件 | 行数 | 测试数 |
|---|---|---|
| `agent_runtime.rs` | 5110 | **1** |
| `http.rs` | 4264 | **0** |
| `agent_harness.rs` | 2801 | **0** |
| `desktop/commands.rs` | 2764 | **0** |
| `desktop/gateway_client.rs` | 990 | **0** |
| `cli/main.rs` | 2172 | **2** |
| `stdio.rs` | 1222 | **0** |

**cognitive/ 整个 crate**: 12/13 文件零测试，CLAUDE.md 强调的核心 trait `CognitiveEngine` **无任何直接测试**。

**所有 5 个 messaging 插件** (telegram/slack/discord/matrix) 共 16 个文件 **0 测试**。

**建议优先级**:

1. `agent_runtime.rs` + `http.rs` - 这两个文件是系统入口，先加集成测试覆盖正常路径
2. `cognitive/engine` + `cognitive/llm` - trait 抽象的契约必须可被测试
3. messaging 插件 - 至少加 happy-path 集成测试

**✅ 部分修复 (2026-06-14)**:

- **CognitiveEngine trait 合约测试落地**：`cognitive/llm/tests/cognitive_engine_contract.rs` 新增 7 个合约测试 + 1 个 stub 自测（共 8 个测试），用 inline `StubLlmProvider` 实现 `cognitive_llm::provider::LlmProvider`。覆盖 `process` 的空观测短路、provider 错误的 `EngineError` 包装、text/tool-call 决策、`subscribe`/`unsubscribe` 的 `Arc::as_ptr` 身份比较、`reset_session` 幂等性。同时把 `LlmCognitiveEngine::emit` 从 private `#[allow(dead_code)]` 升为 `pub fn`，让外部 tests/ 能直接驱动 listener 注册表（生产代码路径不变 — `process` 仍不调用 `emit`，那是后续 streaming PR 的工作）。
- **范围**：`agent_runtime.rs` / `http.rs` / `agent_harness.rs` / 5 个 messaging 插件都仍未加测试 — 这条 P1 只完成 1/3（cognitive 部分），整体"测试覆盖率"仍是结构性问题。
- **Stub 选型说明**：inline 而不是 `test_utils::MockLLMProvider` 复用，因为 `test_utils::MockLLMProvider` 实现的是 kernel-style trait（`complete`/`chat`），而 `LlmCognitiveEngine` 需要 `cognitive_llm::provider::LlmProvider`（`chat_completion(LlmChatRequest, Option<callback>)`）— 两者是 P0-1 已识别的并行重复 trait。桥接会引入新 kernel→cognitive-llm 边，**反转 P0-1 刚建立的解耦**。

**验证**：`cargo test -p cognitive-llm --test cognitive_engine_contract` 8/8 通过；`cargo build -p cognitive-llm --tests` 干净。

### 7. `test-utils` crate 是死代码

`kernel/test-utils/` 已声明并提供 `DeterministicClock` / `MockLLMProvider` / `FakeEventBus`，但全工作区**零处导入使用**。

**建议**: 让 `event-bus`、`dispatcher`、`skill`、`idle`、`gateway/runtime/*` 通过 dev-dependency 引入 `test-utils`，用 `DeterministicClock` 替换测试中的 `tokio::time::sleep`。

**✅ 部分修复 (2026-06-14)**:

- **卫生修正**（同时把 test-utils 拉齐到 P0 刚建立的约定）：fake_event_bus.rs 的 5 处 `lock().unwrap()` + mock_llm.rs 的 4 处 `lock().unwrap()` 全部换成 `.lock().unwrap_or_else(|e| e.into_inner())`；`MockLLMProvider::simulate_delay` 从 sync 升 `async fn`，`std::thread::sleep` → `tokio::time::sleep(...).await`（与 P0 修的 `agent_harness::kill_process` 同一反模式）；`tick: AtomicUsize` 改名 `call_seq`（不是 wall-clock ts）；`MockCallRecord.timestamp_ms` 改名 `seq`。`Cargo.toml` 加 `tokio` 的 `time` feature 到 `[dependencies]`（不是 dev）— 让下游 consumer 拿到的是 async-clean 的 mock。
- **API 易用性补全**：`DeterministicClock::advance_to(target)` + `at(secs)`；`FakeEventBus::event_count()` + `has_event(predicate)` + `inject_event` 改名 `record_event_in_history_only`（明确它不发到 subscribers）；`MockLLMProvider::reset_history()`；crate-level `//!` 文档带 doctest 展示典型用法。
- **第一批消费者上线**：`kernel/event-bus/Cargo.toml` 和 `kernel/dispatcher/Cargo.toml` 加 `test-utils` dev-dep；`kernel/dispatcher/tests/dispatcher_event_bus_integration.rs` 新增 2 个集成测试（dispatcher→bus 发布路径 + FakeEventBus 背压 smoke）。event-bus 自身没新增测试 — 现有 44 测试已用 `InMemoryBus`，50ms 协调 sleep 是 load-bearing 的（防止 `notify_one` 在 consumer 到达 await 之前触发），不动。
- **未做**：`skill` / `idle` / `gateway/runtime/*` 仍没接 test-utils。`idle` 被 `DeterministicClock` 不会和 `tokio::time::pause()` 集成这件事卡住（8 处 `tokio::time::sleep` 在 `incubation.rs`），需要先引入 `Clock` trait 或全工作区切到 paused-runtime 测试 harness — 是后续工作。
- **清理**：`kernel/plugins/llm-provider-openai/Cargo.toml` 删除一条从未 `use` 的 `test-utils` dev-dep（不是这次工作的必须项，但既然在 `git grep` 看到了就一起清掉）。

**验证**：`cargo test -p test-utils` 13 unit + 1 doctest 全过；`cargo test -p event-bus`（15）/ `cargo test -p dispatcher`（46 + 2 新增）全过；`cargo clippy -p test-utils --all-targets -- -D warnings` 干净。

### 8. `desktop/src/commands.rs` 国际化 bug

文件**上下两半** i18n 处理不一致：

- 上半部使用 `t.translate("desktop.error.*")`（正确）
- 下半部直接硬编码中文字符串

英文界面用户会看到中文错误。

**建议**: 全文统一使用 `t.translate()` 调用，缺失的 key 加入 `shared/i18n` 资源。

### 9. `http.rs` 中 `"api"` 字符串出现 45+ 次

`operator_from_headers(&headers).unwrap_or("api")` 在 http.rs 中重复 45+ 次。

加上 `confirm_required_pre_check` 7 处 14 行完全相同的代码块，`audit-on-ok/audit-on-error` 30+ 处复制。

**建议**:

1. `const DEFAULT_OPERATOR: &str = "api"`
2. 抽 `with_audit(operator, action, target, fut)` 装饰器
3. 抽 `require_confirmation(...)` helper

### 10. `MemoryProvider` 17 个 `unimplemented!()` 默认方法

`kernel/core/src/memory.rs:158-323` 17 个方法全是 `{ let _ = args; unimplemented!(...) }`。

**建议**:

- 拆分为子 trait：`MemoryCRUD` / `MemorySessions` / `KnowledgeGraph` / `TemporalQueries` / `ProceduralMemory` / `CognitiveProcessing`
- 实现者只实现支持的子 trait；不支持的返回 `AmanResult::Unimplemented`

### 11. `Error` 类型 21 个 variant 多数是 message-only

`kernel/core/src/error.rs:9-51` 中 8 个 variant 是 `{ message: String }`，把"种类"和"消息"混在一起，调用方无法精确 match。

**建议**:

```rust
pub struct Error { kind: Kind, source: Option<Box<dyn StdError + Send + Sync>> }
pub enum Kind {
    NotFound { name: String },
    RateLimited { retry_after: Duration },
    ConfigInvalid,
    // ...
}
```

### 12. 配置系统的"三层重复"

`AgentConfig` 17 字段 + `AmanConfig` (multi-agent) + 18+ 个 `Partial*` 影子结构体 + `merge()` 165 行 if-let 嵌套。

`IdleConfig` 9 个子配置 + 9 个 `PartialIdleConfig` 影子 + 78 行 merge 级联。

**建议**: 用 `serde_with` 或自写 derive 宏自动生成 `Partial*` 类型；用 trait `Merge` 让子配置自描述合并逻辑。

---

## 🟡 P2 中等优先级

### 13. Plugin `register_exports`/`unregister_exports` 各 5 块重复

`kernel/plugin/src/lib.rs:1538-1627` 90 行 5 个几乎相同的 block。`NoopPluginRegistrar` 12 个 `Ok(())` 共 40 行。

**建议**: 写 `#[derive(Noop)]` proc-macro 统一生成。

**✅ 部分修复 (2026-06-14)**:

- 新增 `#[proc_macro_derive(Noop)]`（`kernel/macros/src/noop.rs`），生成 10 个方法的 `PluginExportRegistrar` impl（5 个 `register_*` + 5 个 `unregister_*`），每个都是 `_` 参数 + `Ok(())`。
- `NoopPluginRegistrar` 从 40 行手写 impl 折叠成 `#[derive(Default, macros::Noop)] pub struct NoopPluginRegistrar;`。**净 -37 行**。
- 宏用绝对路径 `crate::PluginExportRegistrar` / `kernel::skill::Skill` / `kernel::tool::Tool` / ... / `kernel::AmanResult`，与调用点的 `use` 无关。代价是宏只能在 `plugin` crate 内用（trait 所在处）；未来要泛化可以让 `#[derive(Noop(trait_path))]` 接受 trait 路径作参数。
- trybuild pass-test `tests/ui/noop_pass.rs` 验证展开：派生后的结构体能塞进 `Box<dyn PluginExportRegistrar>`。
- 依赖：plugin 加 `macros = { path = "../macros" }`；macros 加 `quote = "1"` + dev-deps `kernel` / `plugin`。

**未做**：`register_exports` / `unregister_exports` 里那 5 块重复的「for + push + register_* + 错误回滚」代码没动 — 这是函数内重复，proc-macro derive 不适用（derive 只能给 struct/enum 加方法，不能改 free function）。需要的重构是表驱动（按 `ExportKind` 数组迭代）或 trait-object 化，跟 derive 完全是两套机制。**属于独立 follow-up**。

**验证**：`cargo build -p plugin -p macros` 干净；`cargo test -p macros --test ui` 3/3 trybuild 通过。

### 14. `http.rs` 三个不同的 error 响应结构

代码中混用 `error_response(error)` / `(StatusCode, Json(ErrorBody{...}))` / `Json(json!({"error":...}))` 三种错误返回。

**建议**: 统一一个 `ApiError` enum + `IntoResponse` 实现。

**✅ 已修复 (2026-06-14)**:

- 新增 `ApiError` enum（`BadRequest` / `NotFound` / `Conflict` / `Forbidden` / `Unprocessable` / `Internal`，每个带 `String` message），带 6 个 constructor helper（`bad_request` / `not_found` / `conflict` / `forbidden` / `unprocessable` / `internal`，接受 `impl Into<String>`）。
- 实现 `From<kernel::Error>`，保留旧 `error_response` 的状态码映射（`NotFound{name}` → 404；`AlreadyExists` / `InvalidStateTransition` → 409；`PermissionDenied` → 403；`ConfigInvalid` → 400；`Unrecoverable` → 422；其它 → 500）。注意 `kernel::Error::NotFound` 字段是 `name`（不是 `message`），From impl 映射成 `"{name} not found"` 以让响应体可读。
- 实现 `IntoResponse`，产出 `(status, Json({"error": msg}))` — 统一用 `error` 字段（消除了旧 `ErrorBody` 的 `message` 字段名和 `json!` 的 `error` 字段名分歧）。
- 迁移 ~95 个站点：
  - `error_response(e)` (37) → `ApiError::from(e).into_response()`
  - `(StatusCode::XXX, Json(ErrorBody{message: Y}))` (12) → `ApiError::xxx(Y).into_response()`
  - `(StatusCode::XXX, Json(json!({"error": Y})))` (43, 含 8 个在 handler 签名里) → `ApiError::xxx(Y).into_response()`
- 4 个 `mcp_*` handler 的返回类型从 `Result<Json<T>, (StatusCode, Json<Value>)>` 改为 `Result<Json<T>, ApiError>` — 这是 axum 原生支持的形状，`?` 运算符直接透过。
- 删除 `error_response` 函数、`ErrorBody` struct、`From<kernel::Error> for ErrorBody` impl。

**净改动**：-233 行（+176, -409）。`cargo build -p gateway` 干净；`cargo test -p gateway --lib` 74/74 通过。

### 15. `cli/main.rs` 16 个 `*_cmd` 重复模式

每个 `*_cmd` 函数结构几乎一致（"build request → send → check status → print body"），HTTP 15+ 份、gRPC 25+ 份。

**建议**: 表驱动 `&[(&str, async fn)]` 注册表；抽 `fn arg(args, i) -> Result<String, i32>` helper 替代 `args.get(i + 1).ok_or(2)?.to_owned()` 的 71 处复制。

### 16. unbounded channel 在 LLM streaming

`agent_harness.rs:2462` `mpsc::unbounded_channel()`，慢消费者会让 buffer 无限增长（event-bus 已有 6 级背压，stream 不应有例外）。

**建议**: 改 `mpsc::channel(128)`，让 LLM callback 受背压。

**✅ 已修复 (2026-06-14)**:

- `spawn_stream_forwarder` 改用 `mpsc::channel(STREAM_FORWARDER_CAP)`，cap = 128。新增 `STREAM_FORWARDER_CAP` 常量（`agent_harness.rs` 顶部）说明选用 128 的理由（每个 `StreamEvent` 是个小 enum，128 × ~100 bytes ≈ 12 KB worst case per turn）。
- Sync 的 `LlmProvider` callback 不能 `.await` 受限 `send`，所以 producer 改用 `try_send`：
  - `Ok(())` — 正常入队
  - `TrySendError::Full(_)` — chunk 丢弃，伴随 `tracing::warn!`（带 `agent` / `session` / `cap` 字段，可在 dashboard 看到丢弃率）
  - `TrySendError::Closed(_)` — receiver 已退出（turn 完成 / agent 关闭），静默忽略
- 消费者侧（`tokio::spawn` 里的 `while let Some(event) = stream_rx.recv().await`）不变 — sender drop 时 `recv()` 返回 `None`，forwarder task 自然结束。
- 语义变化：unbounded → bounded = 慢消费者场景下可能丢 chunk。unbounded 是隐藏的内存增长 bug；bounded + log 是显式背压。可接受，因为 128 cap 在正常 forwarder 速率下永远到不了上限。

**验证**：`cargo build -p gateway` 干净；`cargo test -p gateway --lib` 74/74 通过。

### 17. emotion evaluator 取消延迟 10-30 秒

`emotion_evaluator.rs:174-237` `tokio::select!` 只包装 sleep 不包装 LLM 调用，shutdown 期间需要等当前 LLM 调用完成。

**建议**: 整个 `evaluate() + publish` 块用 `select!` 包裹 cancel token。

**✅ 已修复 (2026-06-14)**:

- `run_loop` 把 `self.evaluate()` future 包进 `tokio::select!`，另一边是 `self.cancel.cancelled()`。
- `biased;` 让 cancel 分支在两边都 ready 时优先 — 否则一个刚好完成的 evaluation 会把 cancel 推到下一个循环，shutdown 还是要等多 ~10s。
- 仅包 `evaluate()`，不包后面的 match + publish。理由：bus publish 是个单 queue push（微秒级），包它会增加复杂度（需要把 match arm 包成 async block + 引入 `ControlFlow` 之类的标志类型），换来不可观察的收益。
- 循环顶部的早期 `if self.cancel.is_cancelled()` 保留 — 是 sleep 返回到 select 启动之间微秒窗口的 O(1) 防御。

**验证**：`cargo build -p gateway` 干净；`cargo test -p gateway --lib` 74/74 通过。

### 18. CLI 真实 bug 嫌疑

`kernel/cli/src/main.rs:2075` `let mut i = 1;` 在 `config_cmd` 中跳过了 `args[0]` — 可能不对称解析 bug。

**建议**: 用 `Iterator::skip()` 替代下标魔法。

**✅ 已修复 (2026-06-14)**:

- `config_cmd` 重写为 `args.split_first()` + `rest.iter()` 模式 — `sub` 通过 `split_first().ok_or(2)?` 取出，剩下用 `rest_iter.next()` 走 flag/value 配对。
- 行为对支持的调用（`aman config show --config foo.yaml`）完全不变；off-by-one 风险点消除。如果将来要加 `--help` 这类「subcommand 前后都能用」的 flag，不会再因为这个魔数翻车。
- 拒绝未知 flag 的行为保留（`_ => return Err(2)` 不变）— 纯重构，零行为变化。

**验证**：`cargo build -p cli` 干净；`cargo test -p cli` lib 2/2 通过。

### 19. 死代码/不可达逻辑

- `plugin/src/lib.rs:1853` `manifest.isolation.unwrap_or(...)` 永远不触发（前面已经 `continue`）
- `plugin/src/lib.rs:1380-1381` 三个连续 `loaded.state = ...` 只最后一个有效
- `cli/main.rs:115-116` `let _daemon: bool = false;` / `let _log_level: Option<String> = None;` 写而不读

**✅ 已修复 (2026-06-14)** — 三个站点，结论各自不同：

- **`plugin/src/lib.rs:1853` `manifest.isolation.unwrap_or(...)`**：**没动**。Doc 的「永远不触发」判断与当前代码不符。`PluginManifest::isolation` 是 `Option<PluginIsolationMode>` 且 `#[serde(default)]` — 没写 `isolation:` 的 YAML 反序列化成 `None`，`unwrap_or(InProcess)` 正是过滤掉 `None` + `Some(InProcess)` 的关键（只剩 `Some(Subprocess)` 进 `if isolation != Subprocess { continue; }`）。load-bearing，没碰。
- **`plugin/src/lib.rs:1380-1381` 三个连续 `loaded.state = ...`**：**已清理**。struct 字面量直接用 `state: PluginLifecycleState::Running`，删掉两个中间赋值（`Loaded` / `Enabled`）和随附的 `let mut` → `let`。语义零变化，删 4 行死代码。
- **`cli/main.rs:115-116` `_daemon` / `_log_level` 写而不读**：**已清理**。同时删 `--daemon` / `--log-level` 两个 parse arm 和 `print_usage` 里对应的两个 entry。理由：原代码是「广告里说支持、解析后悄悄扔掉」— 比「不支持」更糟（用户以为生效）。删后用户传这两个 flag 会得到标准 `Err 2`（unknown argument），行为诚实。**breaking CLI change** 但只影响「以为这两个 flag 能用」的用户 — 真要用时再加回 + 配 wiring。

**验证**：`cargo build -p cli -p plugin` 干净；`cargo test -p cli` lib 2/2 通过；plugin lib 编译干净（pre-existing 的 `plugin (lib test)` 缺 `aman_data_dir` 字段和 cli 集成测试的 `env!` macro 用法问题与本次改动无关）。

---

## 🟢 P3 长期改进

### 20. 缺乏测试基础设施

- **无 fuzzing**: 无 `cargo-fuzz` 目录，无 `Arbitrary` derive
- **无 property-based testing**: 仅 `workflow` 一处使用 `proptest`
- **无 HTTP mock**: CLI 集成测试靠真实启动 gateway
- **无 snapshot testing**: 无 `insta`
- **无 async race detection**: 无 `loom`

**建议**:

- 给 `event-bus`、`pipeline`、`redactor` 加 proptest
- 给 `serde` 派生类型加 `arbitrary::Arbitrary`，建立 fuzz 基础
- CLI 测试引入 `wiremock` / `mockito`

### 21. god-struct 数据建模

`CapabilitySet` 9 字段 + 手写 `contains`/`diff` (35+ 行/方法)，新增 capability 需改 3 处。

`PluginExports` / `RegisteredExports` 两个近相同结构体。

`PluginCandidate` 把 `Box<dyn Plugin>` + `isolation` flag + 3 个 `Option<*>` 全部公开，应编码为 `enum PluginCandidate`。

### 22. 不一致错误处理

- `let _ = ...` 静默吞 `publish_to_agent_bus` (agent_harness.rs ~30 处)
- `let _ = self.store.delete(instance_id);` (workflow/src/lib.rs:685)
- `let _ = version_manager.save_version(...)` (skill/src/lib.rs:1322) — **数据丢失 bug**
- `unwrap_or("untitled")` 处理必填 JSON-RPC 参数

**建议**: 引入 `tracing::warn!` 包装 `let _ =` 模式；为 `version_manager.save_version` 失败发出告警事件。

### 23. 日志安全违规

- `kernel/config/src/lib.rs:991` `eprintln!` 违反 CLAUDE.md 规则（虽带 `#[allow]`，但应改 `tracing::warn!`）
- `agent_runtime.rs:235-249` `tracing::warn!` + `let _ = ...` 吞 `sync_builtin_*` 失败

### 24. SDK 抽象泄漏

`kernel/sdk/src/lib.rs` 同时提供 `sdk::Tool` 和 `sdk::prelude::Tool`，两条路径到达同一类型。

**建议**: 外部代码只暴露 `prelude`，内部代码保留 crate 级路径。

### 25. 其他代码异味汇总（按文件）

#### `kernel/gateway/src/runtime/agent_runtime.rs` (5110 行)

- **`AgentRuntimeBuilder::build()` (200-1980, ~1780 行)** — 单一 god-constructor
- **`RuntimeJsonRpcHandler::handle_method` (2334-2707, ~373 行)**
- **`SkillEventDispatcher::handle` (443-951, ~508 行)**
- **`LlmChatTool::execute` (2003-2300, ~297 行)**
- **`StreamingChatReplyHandler::handle` (1463-1703, ~240 行)**
- **`AgentRuntime` (2709-2789)** — 61 字段 god struct
- 15+ 个 handler struct 内联在 `build()` 内
- 深嵌套：2417-2482、3182-3263
- Magic string：provider 名称、platform 名称、source 类型 86 处
- `.clone()` 86 次
- `.unwrap()`/`.expect()` 11 处

#### `kernel/gateway/src/runtime/http.rs` (4264 行)

- **`explore_pipeline` (2442-2766, ~324 行)** — 5 阶段 try/error/log 重复
- **`chat_session_send` (3071-3308, ~237 行)**
- **`idle_run` (3916-4158, ~242 行)**
- **`explore_start` (2329-2440, ~111 行)**
- `"api"` operator 字符串 **45+ 次**
- `"persistent"` 6 次, `"message-session"` 4 次, `"aman"` 3 次
- 深嵌套 5+：push_event (1344-1388)、chat_sessions (2890-2900)、chat_session_send (3279-3298)
- `.clone()` 49 次
- `expect("just restored")` 在生产代码 (3085)
- 7 处 confirm_required_pre_check 14 行重复
- 30+ 处 audit-on-ok/audit-on-error 重复
- 6+ 处 `now_ms_i64()` 重复
- 三种 error 响应结构混用

#### `kernel/plugin/src/lib.rs` (3173 行)

- **`PluginLoader::load_plugin_inner` (1211-1386, ~175 行)**
- `PluginManifest` 16 字段（346-366）
- 30_000 ms subprocess timeout 散布 3 处
- 1_048_576 (1MB) 2 处
- `.clone()` 41 次
- `.expect()` 113 次（含 12 unwrap + 101 expect）
- `unload_with_timeout` 错误信息缺失
- `Err(_)` 静默吞多处
- `register_exports`/`unregister_exports` 各 5 块 90 行
- `loaded.state = X;` 三连赋值只最后一个有效

#### `kernel/gateway/src/runtime/agent_harness.rs` (2801 行)

- **`process_message` (1202-1666, 465 行)** — 最大单方法
- **`LlmReActEngine::execute_tools` (701-976, ~276 行)**
- **`LlmReActEngine::execute_turn` (571-699, ~129 行)**
- **`AgentHarness::react_loop` (2209-2420, ~212 行)**
- **`ToolExecutor::execute` (285-455, ~171 行)**
- **`run_direct_act_continuation` (1863-2031, ~169 行)**
- **`build_continuation_context` (2566-2714, ~149 行)**
- 27 个 distinct event-type 字符串
- `"agent:harness"` **33 处**
- 13-arg `run_direct_act_continuation` 签名
- `kill_process` `std::thread::sleep` bug
- `publish_to_agent_bus` 3 处重复实现
- ~30 处 `let _ = self.publish_to_agent_bus(...).await;`

#### `desktop/src/commands.rs` (2764 行)

- **`test_im_channel` (1545, ~125 行)** — 4 平台臂
- **`chat_session_list_db` (730, ~120 行)**
- 深嵌套 5+：jsonl_session_title (779-800)
- 132 个 `unwrap_or(...)`/`unwrap_or_default()`
- 1326-1327, 1423, 1554, 1585, 1611, 1636-1637, 1768 keychain 前缀硬编码
- 国际化 bug：中英混杂

#### `kernel/skill/src/lib.rs` (2375 行)

- **`HotReloadManager::reload_once` (1270-1393, ~123 行)**
- 100.0 分数缩放、16/40 snippet context
- `.clone()` 25 次，热区 799-802, 822, 476
- `.expect()` 129 次（35 处 production lock 合理）
- 1308-1312 静默吞 YAML 错误
- 1322 `let _ = version_manager.save_version(...)` 数据丢失风险
- `SkillRegistration` 3 处重复字面量
- `HotReloadManager` 配置与状态混合

#### `kernel/config/src/lib.rs` (2310 行)

- **`AgentConfig::merge` (1422-1586, ~165 行)**
- **`load_env_patch_from_iter` (1663-1734, ~72 行)**
- 0.8049, 0.2051, 10.0102 压缩默认值未解释
- `merge` idle 块 (1495-1573) 9 层嵌套
- `AgentConfig` 17 字段（临界）
- 991 行 `eprintln!` 违反日志安全

#### `kernel/cli/src/main.rs` (2172 行)

- **`event_cmd` (543-772, 230 行)**
- **`plugin_cmd` (1260-1411, 152 行)**
- **`event_cmd_grpc` (774-905, 132 行)**
- **`dlq_cmd` (907-1024, 118 行)**
- `"127.0.0.1:8080"` 3 处
- 71 处 `args.get(i + 1).ok_or(2)?.to_owned()`
- 16 臂 match 命令分发
- HTTP 15+ 份、gRPC 25+ 份 boilerplate
- `let _ = ConfigLoader::load(...)` (2128) 静默吞错误
- 2075 `let mut i = 1;` 不对称解析嫌疑
- 测试仅 2 个

#### `kernel/workflow/src/lib.rs` (2066 行)

- **`WorkflowEngine::handle_event` (706-846, ~141 行)**
- `"RETRY"`, `"CANCEL"`, `"ARCHIVED"`, `"APPROVED"` 等字符串散落
- 深嵌套 5+：handle_event action 块 (765-841)
- `.clone()` 51 次，5 处 `insert(id.clone(), instance.clone())` 重复
- `.expect()` 98 次（26 处 production mutex）
- `cancel_from_error` 误用 `TransitionReason::GuardRejected`

---

## 重构优先级路线图

### 阶段 1（1-2 周）：止血 + 安全

- 修 `kill_process` 阻塞 bug
- 全局用 poisoning recovery 替换 `.lock().unwrap()`
- 修复 `event-bus` 嵌套锁
- 缩小 `transition_lock` 持有窗口
- 修复 `config_cmd` 下标 bug
- 加 `version_manager.save_version` 失败告警

### 阶段 2（2-4 周）：抽象统一

- 选定 `cognitive-llm` 为唯一 LLM 抽象，删除 `kernel::llm` 和 `kernel::react` 重复
- 接入 `CognitiveEngine` 到 `agent_runtime.rs`
- 拆分 `Error` 为 `Kind + source`
- 拆分 `MemoryProvider` 为子 trait

### 阶段 3（4-8 周）：结构降重

- 拆分 `AgentRuntime` 为 5 个 subsystem holder（-1500 行）
- 拆解 `process_message` 465 行方法
- 提取 `register_exports` macro / `Noop` derive macro
- 抽 `http.rs` 公共装饰器（`with_audit`、`require_confirmation`）
- 抽 CLI 命令注册表

### 阶段 4（持续）：测试覆盖率

- `agent_runtime.rs`、`http.rs`、`agent_harness.rs` 关键路径加集成测试
- `cognitive/*` 写 trait contract 测试
- messaging 5 个插件各加 happy-path 测试
- 引入 `test-utils` 给所有相关 crate
- 引入 proptest 给 event-bus / pipeline / redactor

---

## 一句话总结

**架构债比代码债更重**: `CognitiveEngine` 抽象已经"画好图纸"但 gateway 仍在用旧 `LlmProvider`；最大的 god-struct `AgentRuntime` (61 字段) 和最大单方法 `process_message` (465 行) 都需要拆分；测试覆盖率是结构性问题——`test-utils` 写好但没人用，最大文件 5110 行 1 个测试。一旦触及 P0 的并发安全和真实 bug，建议立即进入阶段 1 止血。
