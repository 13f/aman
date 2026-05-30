# Chat 插件化重构 & Plugin 系统升级

## 一、背景

当前 aman 存在两种功能实现模式：

| 功能 | 模式 | 路由注册 | UI 注册 |
|------|------|---------|--------|
| InfoHub | Plugin (InProcess) | 无（无 HTTP API） | 无 UI |
| Chat | 内置（硬编码） | `build_router()` 硬编码 | `App.svelte` 硬编码 |
| Workflow Board | 内置 | `build_router()` 硬编码 | `App.svelte` 硬编码 |
| Plugin Manager | 内置 | `build_router()` 硬编码 | `App.svelte` 硬编码 |
| **Team (计划)** | **Plugin** | ❌ 缺失 `Plugin::routes()` | ⚠️ `UiDeclaration.pages` 未被消费 |

Chat 作为最早的功能，在 Plugin 系统成熟之前就已实现，因此硬编码在 `build_router()` 和 `App.svelte` 中。InfoHub 作为后来的功能，遵循了 Plugin 模式，但它没有 HTTP 路由和独立 UI 页面，避开了当前 Plugin 系统的两个能力缺口。

Team 如果沿用 Plugin 模式，需要先补齐这两个缺口。补全后，Chat 也可以顺带重构为 Plugin，统一架构。

---

## 二、Plugin 系统升级（两个缺口）

### 升级 A：Plugin → HTTP 路由注册

**现状**：Plugin trait 不支持贡献 HTTP 路由。所有路由在 `build_router()` 中硬编码。

**方案**：在 `kernel::plugin::Plugin` trait 增加可选方法 `routes()`。

```rust
// crates/core/src/plugin.rs — Plugin trait 增加
#[async_trait]
pub trait Plugin: Send + Sync {
    // ... 现有方法 (name, version, hooks, tools, init, shutdown) ...

    /// 插件贡献的 HTTP 路由。默认返回空 Router。
    /// AgentRuntime 在插件初始化后调用，将返回的 Router
    /// 以 `/api/v1` 为前缀 merge 进主路由树。
    fn routes(&self) -> Option<axum::Router> {
        None
    }
}
```

AgentRuntime 消费侧（Phase 3 插件加载完成后）：

```rust
// crates/gateway/src/runtime/agent_runtime.rs
fn build_router(runtime: Arc<AgentRuntime>) -> Router {
    let mut app = Router::new()
        // ... 内置路由（health, metrics） ...

    // Phase 3 后：merge 每个插件的路由
    for plugin in &self.active_plugins {
        if let Some(plugin_router) = plugin.routes() {
            app = app.nest("/api/v1", plugin_router);
        }
    }

    app
}
```

**影响范围**：
- `crates/core/src/plugin.rs`：trait 加方法（向后兼容，默认返回 `None`）
- `crates/gateway/src/runtime/agent_runtime.rs`：`build_router()` 加插件路由 merge 逻辑
- 现有插件不受影响（都返回 `None`）

---

### 升级 B：Plugin → UI 导航注册

**现状**：`PluginManifest` 已有 `ui: Option<UiDeclaration>` 字段，`UiDeclaration` 已有 `pages: Vec<String>`。但 App.svelte 的导航栏是硬编码的 `menuGroups` 数组，不消费插件的 UI 声明。

**方案**：后端新增 `/ui/pages` 查询端点，App.svelte 动态拉取并渲染。

**后端**：

```rust
// crates/gateway/src/runtime/http.rs
.route("/ui/pages", get(ui_plugin_pages))

#[derive(Serialize)]
struct UiPageEntry {
    id: String,
    label: String,
}

async fn ui_plugin_pages(State(runtime): State<Arc<AgentRuntime>>) -> Json<Vec<UiPageEntry>> {
    let mut pages = Vec::new();
    for plugin in runtime.active_plugins() {
        if let Some(ui) = &plugin.manifest().ui {
            for page_id in &ui.pages {
                pages.push(UiPageEntry {
                    id: page_id.clone(),
                    label: match page_id.as_str() {
                        "team" => "Team".into(),
                        "chat" => "Chat".into(),
                        other => other.to_string(),
                    },
                });
            }
        }
    }
    Json(pages)
}
```

**前端 — App.svelte 动态导航**：

```svelte
<script lang="ts">
  let pluginPages = $state<{id: string, label: string}[]>([]);

  onMount(async () => {
    try {
      pluginPages = await invoke<{id: string, label: string}[]>("get_ui_plugin_pages");
    } catch { /* gateway 未运行 */ }
  });
</script>

<!-- 导航栏中，在静态菜单项下方动态追加插件页面 -->
{#each pluginPages as pg}
  <button class="nav-btn" class:active={currentPage === pg.id}
    onclick={() => navigateTo(pg.id)}>
    <span class="status-dot running"></span>
    {pg.label}
  </button>
{/each}
```

**前端 — 页面组件分发**：

```svelte
<!-- App.svelte <main> 区域 -->
{#if currentPage === "home"}
  <Home ... />
{:else if currentPage === "dashboard"}
  <Dashboard ... />
<!-- ... 其他内置页面 ... -->
{:else if pluginPageComponents[currentPage]}
  <svelte:component this={pluginPageComponents[currentPage]} />
{/if}
```

组件映射表：

```typescript
// src/pages/plugin-pages.ts
import Team from "./plugins/Team.svelte";
import Chat from "./plugins/Chat.svelte";

export const pluginPageComponents: Record<string, any> = {
  "team": Team,
  "chat": Chat,
};
```

**影响范围**：
- `crates/gateway/src/runtime/http.rs`：新增 `/ui/pages` 端点
- `crates/tauri/src/App.svelte`：导航栏动态化 + 页面分发改为映射表
- `crates/tauri/src/commands.rs`：新增 `get_ui_plugin_pages` 命令
- 现有内置页面 Home/Dashboard/WorkflowBoard 等不受影响（仍在 `{#if}` 链中）

---

## 三、Chat 插件化重构

### 3.1 当前状态

Chat 功能分散在以下几个位置：

```
硬编码点:
├── crates/gateway/src/runtime/http.rs
│   └── /chat/sessions             (7 个路由，build_router 中硬编码)
│   └── /chat/session/{id}/send
│   └── /chat/session/{id}/close
│   └── ...
│
├── crates/gateway/src/runtime/session_store.rs
│   └── SessionStore (Chat 专用，但定义在 gateway 而非 plugin)
│
├── crates/gateway/src/runtime/agent_harness.rs
│   └── run_react_loop() (Chat 和未来 Team 共用 — 放在 gateway 没问题)
│
├── crates/tauri/src/App.svelte
│   └── menuGroups[0].items[1]: { id: "chat", label: "Chat" }  (硬编码)
│   └── {:else if currentPage === "chat"} <Chat />              (硬编码路由)
│
├── crates/tauri/src/lib.rs
│   └── 20+ chat_* Tauri commands                              (硬编码)
│
└── crates/tauri/src/pages/Chat.svelte
    └── Chat UI 组件
```

### 3.2 目标状态

```
crates/plugins/chat/
├── plugin.yaml        # PluginManifest
├── Cargo.toml
└── src/
    ├── lib.rs         # ChatPlugin: Plugin trait impl
    ├── api.rs         # /chat/* HTTP 路由
    ├── session.rs     # SessionStore 迁移至此
    ├── commands.rs    # Tauri commands (chat_send_message 等)
    └── bridge.rs      # 与 AgentHarness 的桥接

crates/tauri/src/pages/plugins/Chat.svelte  # UI 组件移至 plugins 子目录
```

### 3.3 重构步骤

**Step 1：创建 ChatPlugin crate 骨架**

```yaml
# crates/plugins/chat/plugin.yaml
name: "chat"
version: "0.1.0"
capabilities: ["chat", "llm_conversation"]
isolation: InProcess
exports:
  tools:
    - "chat.send_message"
    - "chat.create_session"
  hooks:
    - "chat.on_message"
ui:
  pages: ["chat"]
```

```rust
// crates/plugins/chat/src/lib.rs
pub struct ChatPlugin {
    session_store: Arc<SessionStore>,
    agent_registry: Arc<AgentRegistry>,
    event_bus: Arc<dyn EventBus>,
}

impl Plugin for ChatPlugin {
    fn name(&self) -> &str { "chat" }
    fn version(&self) -> Version { Version::new(0, 1, 0) }

    fn routes(&self) -> Option<axum::Router> {
        Some(self.chat_api_routes())
    }

    fn hooks(&self) -> Vec<Box<dyn Hook>> {
        vec![Box::new(ChatMessageHook::new(self.session_store.clone()))]
    }

    fn tools(&self) -> Vec<Box<dyn Tool>> {
        vec![Box::new(ChatSendMessageTool)]
    }
}
```

**Step 2：迁移 SessionStore 到 Chat 插件**

`SessionStore` 从 `crates/gateway/src/runtime/session_store.rs` 移到 `crates/plugins/chat/src/session.rs`。

`AgentRuntime` 通过 `PluginContext` 把 `EventBus`、`AgentRegistry`、`ToolRegistry` 引用传给 ChatPlugin，SessionStore 不再需要放在 gateway 层。

**Step 3：迁移 HTTP 路由**

从 `build_router()` 中删除所有 `/chat/*` 路由。ChatPlugin 通过 `routes()` 返回相同的 Router。路由路径不变（仍在 `/api/v1/chat/*`），对前端 Tauri commands 透明。

```rust
// crates/plugins/chat/src/api.rs
pub fn chat_api_routes() -> Router {
    Router::new()
        .route("/chat/sessions", get(chat_sessions))
        .route("/chat/session/create", post(chat_session_create))
        .route("/chat/session/{id}/state", get(chat_session_state))
        .route("/chat/session/{id}/history", get(chat_session_history))
        .route("/chat/session/{id}/send", post(chat_session_send))
        .route("/chat/session/{id}/close", post(chat_session_close))
        .route("/chat/session/{id}/stop", post(chat_session_stop))
        .route("/chat/session/{id}/retry", post(chat_session_retry))
        .route("/chat/session/{id}/edit", post(chat_session_edit))
        .route("/chat/session/{id}", delete(chat_session_delete))
}
```

**Step 4：迁移 Tauri Commands**

当前 `lib.rs` 中 20+ 个 chat 命令硬编码在 `invoke_handler` 中。需要支持插件注册 Tauri commands，或者 ChatPlugin 在初始化时注册命令到全局 handler。

方案：在 `PluginContext` 中注入一个 `CommandRegistrar`：

```rust
// crates/core/src/context.rs 增加
pub struct PluginContext {
    // ... 现有字段 ...
    pub command_registrar: Arc<dyn CommandRegistrar>,
}

pub trait CommandRegistrar: Send + Sync {
    fn register(&self, name: &str, handler: Box<dyn Fn(...) + Send + Sync>);
}
```

ChatPlugin 在 `init()` 中注册所有命令：

```rust
impl Plugin for ChatPlugin {
    async fn init(&mut self, ctx: PluginContext) -> AmanResult<()> {
        ctx.command_registrar.register("chat_send_message", /* handler */);
        ctx.command_registrar.register("chat_stop_generation", /* handler */);
        ctx.command_registrar.register("chat_session_list", /* handler */);
        // ... 其余命令 ...
        Ok(())
    }
}
```

**Step 5：迁移 UI 组件**

`crates/tauri/src/pages/Chat.svelte` → `crates/tauri/src/pages/plugins/Chat.svelte`。

从 `App.svelte` 中删除 Chat 的硬编码导航项和路由分支。改为：
- 升级 B 完成后：Chat 通过 `UiDeclaration.pages: ["chat"]` 自动出现
- 过渡期：在 `pluginPageComponents` 映射表中添加 `"chat": Chat`

**Step 6：删除旧代码**

确认 Chat 插件功能正常后：
- 从 `build_router()` 删除 chat 路由
- 从 `App.svelte` 的 `menuGroups` 删除 chat 项
- 从 `lib.rs` 的 `invoke_handler` 删除 chat 命令
- 删除 `session_store.rs`（已迁移）

---

## 四、实施顺序

```
Phase 0: 升级 A — Plugin::routes()    (1天)
         ├── core/plugin.rs 加方法
         └── agent_runtime.rs 加 merge 逻辑

Phase 1: 升级 B — UI 动态导航           (1-2天)
         ├── http.rs 加 /ui/pages 端点
         ├── commands.rs 加 get_ui_plugin_pages
         └── App.svelte 动态导航 + 映射表

Phase 2: Team Plugin 开发               (依赖 A+B)
         └── crates/plugins/team/ 全部

Phase 3: Chat 插件化重构                (依赖 A+B，可与 Team 并行或后置)
         ├── Step 1-2: 创建 ChatPlugin + 迁移 SessionStore
         ├── Step 3: 迁移 HTTP 路由（通过 routes()）
         ├── Step 4: 迁移 Tauri commands（需 CommandRegistrar）
         ├── Step 5: 迁移 UI 组件
         └── Step 6: 删除旧代码
```

**Phase 0 和 Phase 1 的顺序**：升级 A（`routes()`）是 Team 和 Chat 插件化的硬依赖，必须最先做。升级 B（UI 动态导航）可以先于或与 Phase 2 并行——Team 在过渡期可以硬编码导航项。

**Phase 2 和 Phase 3 的关系**：Team 开发需要 A 完成，建议 B 也完成。Chat 重构可以与 Team 开发并行，也可以等到 Team 上线后再做——Chat 作为内置功能目前运行正常，不急。

---

## 五、风险与缓解

| 风险 | 缓解 |
|------|------|
| `routes()` 方法签名限制 axum Router 类型，可能不兼容未来非 axum 后端 | 当前只有 axum 后端，且 `Option<Router>` 是可选方法，默认 None 不强制依赖 axum |
| Tauri CommandRegistrar 接口设计可能不完美 | 先不抽象，Chat 插件在 init 中直接调用一个全局注册函数。后续有第二个插件需要注册命令时再抽象 trait |
| Chat 重构引入回归 | Phase 3 的 Step 2-5 逐步做，每步验证。SessionStore 迁移保持 API 签名不变 |
| App.svelte 改动影响所有页面 | 映射表方案向后兼容——内置页面仍在 `{#if}` 链中，插件页面走映射表，互不干扰 |

---

## 六、结束状态

```
所有"业务功能"都是 Plugin:

crates/plugins/
├── info-hub/        # 已有
├── team/            # 新增 (Phase 2)
└── chat/            # 重构 (Phase 3)

App.svelte 导航:
  ┌─ Workspace ─────────┐
  │ Home (内置)          │
  │ [插件页面动态注入]    │  ← Team, Chat 等不存在于 App.svelte 源码中
  └──────────────────────┘

build_router():
  ┌─ /health, /metrics   (内核) ─┐
  │ /skills, /workflows  (内核)   │
  │ /plugins, /agents    (内核)   │
  │                               │
  │ + plugin.routes() merge ──    │  ← /team/*, /chat/* 不在此函数中
  └───────────────────────────────┘
```

Chat 不再硬编码在任何地方。Team 和 Chat 对内核来说是同一种东西：一个有 routes、有 UI pages、有 hooks、有 tools 的 Plugin。
