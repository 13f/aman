# aman 插件 / 事件 / Hook 开发指南

---

## 目录

1. [概述](#1-概述)
2. [内置工具参考](#2-内置工具参考)
3. [Hook 系统——最简单的扩展方式](#3-hook-系统最简单的扩展方式)
4. [事件系统——核心数据流](#4-事件系统核心数据流)
5. [Plugin 系统——完整的扩展能力](#5-plugin-系统完整的扩展能力)
6. [实践指南](#6-实践指南)
7. [从外部推送事件](#7-从外部推送事件)

---

## 1. 概述

aman 提供三层扩展机制，从简单到复杂依次为：

| 机制 | 复杂度 | 用途 | 语言 |
|------|--------|------|------|
| **Hook** | 低 | 事件触发脚本，播放音效、通知、日志 | 任意脚本 (bash/python/node) |
| **Event Source** | 中 | 定时器、文件监控、webhook 等自定义事件源 | Rust |
| **Plugin** | 高 | 完整的工具/技能/事件源扩展包 | Rust / WASM / 子进程 |

---

## 2. 内置工具参考

aman 默认注册了 11 个内置工具，Agent 通过 ReAct 循环调用它们来完成任务。

### 2.1 文件操作工具

#### read — 读取文件

```json
{
  "path": "/path/to/file.txt"
}
```
返回 `content`（文件内容）和 `bytes`（字节数）。

#### write — 写入文件

```json
{
  "path": "/path/to/file.txt",
  "content": "文件内容"
}
```
自动创建父目录。敏感路径（`.ssh/*`、`.env` 等）受安全规则保护。

#### edit — 精确字符串替换

```json
{
  "file_path": "/path/to/file.rs",
  "old_string": "旧文本（必须唯一匹配）",
  "new_string": "新文本"
}
```
行为：
- 读取文件全部内容
- 查找 `old_string`：
  - **0 次匹配** → 报错，提示文本可能已被编辑
  - **多次匹配** → 报错，要求提供更多上下文使匹配唯一
  - **唯一匹配** → 替换为 `new_string` 并写回

### 2.2 搜索与目录浏览工具

#### list — 列出目录

```json
{
  "path": "/path/to/dir"
}
```
返回排序后的条目列表，目录在前、文件在后，各分组内按字母序排列。每个条目包含 `name`、`type`（`file`/`dir`/`symlink`）和 `size`。

#### find — 递归搜索文件

```json
{
  "pattern": "keyword",
  "base": "/path/to/search",
  "type": "file"       // 可选过滤：file | dir
}
```
递归搜索目录树，匹配文件名（大小写不敏感子串匹配），返回结果按路径排序。纯 Rust `std::fs` 实现，零外部依赖，适合快速文件名检索。

#### grep — 内容搜索（包装 ripgrep）

```json
{
  "pattern": "search_pattern",
  "path": "/path/to/search",
  "glob": "*.rs",           // 可选：文件 glob 过滤
  "max_results": 100,       // 可选：最多返回结果（默认 100，最大 500）
  "fixed_strings": false,   // 可选：纯文本搜索（不解释正则）
  "context_lines": 0        // 可选：上下文行数
}
```
包装 ripgrep（`rg`）实现多线程文件内容搜索，返回结构化结果，每条包含 `path`、`line_number`、`text`。支持大小写不敏感匹配、glob 过滤、上下文显示。**不经过 shell**，参数直接传递给 rg 子进程。

需要系统已安装 ripgrep（`brew install ripgrep`）。

**find vs grep 对比：**

| 维度 | find（文件名搜索） | grep（内容搜索） |
|------|-------------------|-----------------|
| 搜索范围 | 仅文件名（子串匹配） | 文件内容（正则匹配） |
| 依赖 | 纯 Rust `std::fs` | 需安装 `rg` 二进制 |
| 大项目性能 | 单线程 DFS，<10k 文件尚可 | 多线程 + SIMD，快 10-50x |
| .gitignore | 不支持过滤 | 内置 ig 库自动忽略 |
| 适用场景 | 快速定位文件名 | 搜索代码逻辑、日志、配置值 |

建议：小项目或纯文件名检索用 `find`；搜索代码逻辑、配置值、日志内容用 `grep`。

### 2.3 执行与网络工具

#### exec — 执行 Shell 命令

```json
{
  "command": "ls",
  "args": ["-la", "/tmp"]
}
```
通过 `SubprocessSandbox` 沙箱执行，有超时保护。高危命令（`rm -rf /`、fork 炸弹、关机等）被硬性拦截。

#### http — HTTP 请求

```json
{
  "url": "https://api.example.com/data",
  "method": "GET",
  "headers": {"Authorization": "Bearer ..."}
}
```
支持 GET/POST/PUT/DELETE 等方法。网络访问可通过安全配置禁用。

### 2.4 数据工具

#### db — SQLite 查询

```json
{
  "db_path": "/path/to/data.db",
  "sql": "SELECT * FROM users WHERE id = ?",
  "params": [1],
  "operation": "query"    // query | execute
}
```
`query` 返回行数组，`execute` 返回 `rows_affected`。DROP/TRUNCATE 和 DELETE 无 WHERE 被拦截。

#### web_search — 网页搜索

```json
{
  "query": "搜索关键词",
  "backend": "duckduckgo",   // tavily | brave | duckduckgo | google | x
  "count": 5
}
```
支持 5 种搜索后端，API key 从 macOS Keychain 读取。

### 2.5 知识工具

#### read_skill — 读取 Skill 指令

```json
{
  "skill": "skill-name"
}
```
加载并返回名为 `skill-name` 的完整 SKILL.md 指令。用于 Agent 的按需技能发现（Hermes 渐进式披露模型）。

### 2.6 工具注册与扩展

工具注册点在 `crates/tool/src/lib.rs`：

```rust
pub fn install_builtin_tools(registry: &ToolRegistry) -> amanResult<()> {
    registry.register(Arc::new(fs_tools::ReadTool))?;
    registry.register(Arc::new(fs_tools::WriteTool))?;
    registry.register(Arc::new(fs_tools::EditTool))?;
    // ...
}
```

Plugin 可以通过 `PluginExportRegistrar::register_tool()` 注册自定义工具，或通过 `Plugin::tools()` 声明。

---

## 3. Hook 系统——最简单的扩展方式

### 3.1 工作原理

Hook 是 aman 中最轻量的扩展机制。每个 Hook 是一个可执行脚本，当特定事件发生时，aman 将事件信息以 JSON 格式通过 stdin 传递给脚本。

```
事件发生 → ScriptHookRunner 匹配 → 脚本 stdin 收到 JSON → 脚本执行 → 完成
```

### 3.2 两种配置方式

#### 方式一：内联配置（aman.yaml）

```yaml
hooks:
  - name: webhook-alert
    on: tool:completed
    script: ./hooks/alert.py
    runtime: python3
    min_version: ">=3.8"
```

#### 方式二：目录自动发现（推荐）

Hook 通过目录位置区分作用域：

- **全局 Hook**：放在 `~/.aman/hooks/` 下，订阅全局事件总线，接收全局事件
- **Agent Hook**：放在 `~/.aman/agents/<agent-id>/hooks/` 下，订阅该 Agent 的本地事件总线，只接收该 Agent 的事件

```
~/.aman/
├── hooks/                          # 全局 Hook
│   └── webhook-alert/
│       ├── config.yaml
│       └── main.sh
└── agents/
    └── minmax/
        └── hooks/                  # Agent 专属 Hook
            └── openpeon/
                ├── config.yaml
                └── main.sh
```

项目 `samples/hooks/` 目录下提供了可直接使用的示例 Hook。全局 Hook 复制到 `~/.aman/hooks/`，Agent Hook 复制到对应 Agent 的 `hooks/` 目录下即可启用：

**config.yaml 格式：**

```yaml
name: openpeon
description: 描述信息
on:
  - session:started
  - agent:busy
  - tool:completed
runtime: bash
min_version: "3.2"          # 可选，版本约束
```

**字段说明：**

| 字段 | 必填 | 说明 |
|------|------|------|
| `name` | 是 | Hook 名称，需唯一 |
| `on` | 是 | 触发的事件类型，支持字符串或数组 |
| `runtime` | 是 | 解释器名称（bash/python3/node/deno） |
| `script` | 否 | 脚本路径，相对路径相对于 config.yaml 所在目录，默认为 `main.sh` |
| `min_version` | 否 | 解释器最低版本约束（semver 格式） |

> **作用域规则：** Hook 的作用域由目录位置决定，无需在 config.yaml 中配置。
> - `~/.aman/hooks/<name>/` → 全局 Hook，订阅全局事件总线
> - `~/.aman/agents/<id>/hooks/<name>/` → Agent Hook，订阅该 Agent 的本地事件总线
> 
> 不同 Agent 可以有同名 Hook，各自独立配置参数（如不同 Agent 使用不同的通知渠道）。

### 3.3 脚本协议

Hook 脚本从 stdin 接收一个 JSON 对象，格式如下：

```json
{
  "event_type": "agent:busy",
  "payload": {
    "agent_id": "...",
    "session_id": "..."
  }
}
```

- stdout 输出被收集（当前可忽略）
- stderr 输出会打印到 aman 日志中，可用于调试
- 脚本退出码非零不会影响主流程

### 3.4 支持的事件类型

事件分为两类，分别发布到不同的事件总线：

**全局事件** — 发布到全局事件总线，放在 `~/.aman/hooks/` 下的全局 Hook 可接收：

| 事件类型 | 触发时机 |
|----------|----------|
| `MessageReceived` | 收到用户消息 |
| `session:started` | 会话开始 |
| `session:closed` | 会话关闭 |
| `message:dispatch` | 技能分发开始 |
| `message:completed` | 技能分发完成 |
| `gateway:ready` | 网关就绪 |
| `gateway:starting` | 网关启动中 |
| `gateway:stopping` | 网关关闭中 |
| `agent:registered` | Agent 注册完成 |

**Agent 本地事件** — 发布到各 Agent 的本地事件总线，放在 `~/.aman/agents/<id>/hooks/` 下的 Agent Hook 可接收：

| 事件类型 | 触发时机 |
|----------|----------|
| `agent:busy` | Agent 开始处理 |
| `agent:reply_ready` | Agent 回复完成 |
| `agent:reply_stream_start` | 流式回复开始 |
| `agent:reply_stream_done` | 流式回复结束 |
| `agent:reply_stream_error` | 流式回复出错 |
| `agent:reply_interrupted` | 回复被中断 |
| `tool:completed` | 工具执行完成 |
| `tool:failed` | 工具执行失败 |
| `tool:dispatched` | 工具调用分发 |
| `llm:call_started` | LLM 调用开始 |
| `llm:call_ended` | LLM 调用结束 |

> **注意：** 如果 Hook 需要同时监听全局事件和 Agent 事件，需要在两个位置分别放置：全局事件 Hook 放在 `~/.aman/hooks/`，Agent 事件 Hook 放在 `~/.aman/agents/<id>/hooks/`。

### 3.5 完整示例：openpeon 音效 Hook

完整代码见 `samples/hooks/openpeon/`，包含两个文件：

```
samples/hooks/openpeon/
├── config.yaml        # Hook 配置
└── main.sh            # 脚本实现
```

**config.yaml** 定义了监听的事件类型和脚本 runtime。openpeon 监听 Agent 本地事件（`agent:busy`、`tool:completed` 等），因此应放在 Agent 的 hooks 目录下：

```yaml
name: openpeon
description: openpeon hooks for aman
on:
  - agent:busy
  - tool:completed
  - tool:failed
  - llm:call_started
runtime: bash
min_version: "3.2"
```

**main.sh** 从 stdin 读取事件 JSON，按类别映射到音效包，播放随机 WAV 文件：

```bash
INPUT=$(cat)
EVENT_TYPE=$(echo "$INPUT" | jq -r '.event_type // empty' 2>/dev/null || exit 0)
# ... 事件分类 → 音效播放
```

使用方式：将 `samples/hooks/openpeon` 复制到对应 Agent 的 hooks 目录下即可启用：

```bash
cp -r samples/hooks/openpeon ~/.aman/agents/minmax/hooks/
```

如果希望所有 Agent 共用同一 Hook，复制到每个 Agent 的 hooks 目录即可，各 Agent 可独立修改参数。

也可作为模板，修改 `config.yaml` 中的事件类型和脚本逻辑，快速创建自己的 Hook。

### 3.6 版本检测机制

`ScriptRuntime` 会检查解释器是否可用及版本是否满足要求：

```rust
// crates/core/src/script.rs
pub fn check_available(&self) -> amanResult<()> {
    // 1. which bash（检查 PATH）
    // 2. bash --version（获取版本）
    // 3. 验证版本约束（如 >=3.2）
}
```

版本号解析兼容多种格式：
- `3.8.0`, `v18.0.0` — 标准 semver
- `3.2.57(1)-release` — bash 风格（自动提取 `3.2.57`）

---

## 4. 事件系统——核心数据流

### 4.1 事件结构

```rust
// crates/core/src/event.rs
pub struct Event {
    pub id: Uuid,                    // UUID v7
    pub source: SourceId,            // 来源标识
    pub event_type: EventType,       // 事件类型
    pub timestamp: Timestamp,        // 毫秒时间戳
    pub priority: Priority,          // High | Normal | Low
    pub delivery: DeliveryGuarantee, // AtMostOnce | AtLeastOnce | ExactlyOnce
    pub dedup_key: Option<DedupKey>,
    pub payload: Value,              // 任意 JSON
    pub metadata: EventMetadata,
}
```

### 4.2 EventType 枚举

```rust
pub enum EventType {
    // 系统事件
    FileCreated, FileChanged, FileDeleted,
    CronTick, TimerTick, Heartbeat,
    MessageReceived, WebhookReceived, SystemSignal,
    WorkflowStateChanged, SkillLoaded, SkillReloaded,
    ConfigChanged, SecretRotated, InjectionDetected,
    Idle, QueueDrained, AgentMessage,
    // 自定义事件（推荐方式）
    Custom(String),
}

**创建事件：**

```rust
let event = Event::new(
    "my_source",                                    // source
    EventType::Custom("my_custom_event".to_owned()), // event_type
    json!({"key": "value"}),                         // payload
);
```

### 4.3 EventBus API

```rust
#[async_trait]
pub trait EventBus: Send + Sync {
    async fn publish(&self, event: Event) -> amanResult<()>;
    async fn subscribe(&self, filter: SubscriptionFilter, handler: Box<dyn EventHandler>) -> amanResult<SubscriptionId>;
    async fn unsubscribe(&self, id: SubscriptionId);
    fn try_dequeue(&self) -> Option<Event>;
    fn backpressure_level(&self) -> BackpressureLevel;
}
```

**订阅事件：**

```rust
bus.subscribe(
    SubscriptionFilter {
        event_types: Some(vec![EventType::Custom("my_event".to_owned())]),
        sources: None,
        priorities: None,
        payload_match: None,  // 可选：JSON 子集匹配
    },
    Box::new(MyEventHandler),
).await?;
```

### 4.4 自定义事件源

> **简单场景**：如果只需要从外部系统向 aman 推送事件，使用 `POST /events/push` HTTP API 或 `aman event push` CLI 即可，无需实现 `EventSource` trait。详见 [§7 从外部推送事件](#7-从外部推送事件)。

实现 `EventSource` trait 可以创建自定义事件源：

```rust
#[async_trait]
pub trait EventSource: Send + Sync {
    fn id(&self) -> &str;
    fn source_type(&self) -> SourceType;
    async fn init(&mut self, ctx: SourceContext) -> amanResult<()>;
    async fn poll(&mut self, ctx: &SourceContext) -> amanResult<Vec<Event>>;
    async fn shutdown(&mut self) -> amanResult<()>;
    async fn on_backpressure(&mut self, level: BackpressureLevel, ctx: &SourceContext) -> amanResult<()>;
    fn health(&self) -> HealthStatus;
}
```

内置事件源参考：

| 类型 | 文件 | 模式 | 说明 |
|------|------|------|------|
| `TimerSource` | `crates/source/src/timer.rs` | Pull | 定时触发 |
| `CronSource` | `crates/source/src/cron.rs` | Pull | Cron 表达式触发 |
| `FileWatchSource` | `crates/source/src/file_watch.rs` | Pull | 文件变化监控 |
| `WebhookSource` | `crates/source/src/webhook.rs` | Push | HTTP Webhook |
| `SocketSource` | `crates/source/src/socket.rs` | Push | TCP/UDP/Unix Socket |
| `SignalSource` | `crates/source/src/signal.rs` | Pull | Unix 信号处理 |

### 4.5 背压系统

事件总线有 5 级背压机制，基于队列使用率：

| 级别 | 阈值 | 行为 |
|------|------|------|
| Normal | <80% | 正常运行 |
| L1 | >=80% | AtMostOnce 事件降级优先级 |
| L2 | >=90% | 丢弃 AtMostOnce 事件 |
| L3 | >=95% | 阻塞保证送达事件，暂停 Push 源 |
| L4A | >=98% | 溢出保证事件到磁盘 |
| L4B | >=98%+ | 紧急状态 |

### 4.6 事件流向

```
Source → poll() → Event → publish(Event) → admit_event()
  → 去重检查 → OrderedQueue.push()
  → drain → 订阅过滤 → handler.handle(event)
```

---

## 5. Plugin 系统——完整的扩展能力

### 5.1 Plugin Trait

所有 Plugin 必须实现的核心 trait：

```rust
// crates/core/src/plugin.rs
#[async_trait]
pub trait Plugin: Send + Sync {
    fn name(&self) -> &str;
    fn version(&self) -> &Version;
    fn dependencies(&self) -> &[PluginDependency];

    // 生命周期
    async fn on_load(&mut self, ctx: PluginContext) -> amanResult<()>;
    async fn on_unload(&mut self) -> amanResult<()>;

    // 依赖通知
    async fn on_dependency_unloading(&self, dep_name: &str) -> amanResult<()>;

    // 导出
    fn event_sources(&self) -> Vec<Arc<dyn EventSource>>;
    fn skills(&self) -> Vec<Arc<dyn Skill>>;
    fn tools(&self) -> Vec<Arc<dyn Tool>>;
    fn hooks(&self) -> Vec<Arc<dyn Hook>>;
}
```

### 5.2 Plugin 清单（plugin.yaml）

每个插件目录必须包含 `plugin.yaml`：

```yaml
name: my-plugin
version: "1.0.0"
description: 我的插件
depends_on:
  - name: core
    version_range: ">=0.1.0"
exports:
  skills:
    - my-skill
  tools:
    - my-tool
  event_sources:
    - my-source
  hooks:
    - my-hook
lifecycle:
  on_load: init
  on_unload: shutdown
isolation: inprocess    # inprocess | subprocess | wasm
capabilities:
  - chat
```

### 5.3 三种隔离模式

| 模式 | 说明 | 适用场景 |
|------|------|----------|
| **InProcess** | 同进程内 `Box<dyn Plugin>` | Rust 原生插件，性能最优 |
| **Subprocess** | 子进程，JSON-RPC stdin/stdout 通信 | 多语言插件（Python/Node/Go） |
| **WASM** | wasmtime 运行时 | 沙箱隔离，安全敏感场景 |

**子进程插件配置：**

```yaml
isolation: subprocess
subprocess:
  command: python3
  args: ["my_plugin_server.py"]
  timeout_ms: 30000
```

**WASM 插件需要导出三个函数：**
- `aman_skill_on_load` -> i32（返回 0 表示成功）
- `aman_skill_on_unload` -> i32
- `aman_skill_execute` -> i32

### 5.4 依赖图与加载顺序

Plugin 系统使用 `DependencyGraph` 进行拓扑排序加载：

```rust
pub struct PluginDependency {
    pub name: String,
    pub version_range: String,   // semver 约束，如 ">=1.0 <2.0"
}
```

加载流程：
1. 递归扫描所有 `plugin.yaml`
2. 构建依赖图，检测循环依赖
3. 拓扑排序
4. 按序加载，失败时回滚已加载的插件

### 5.5 生命周期状态机

```
Loaded → Enabled → Running ←→ Paused
                      ↓
                  Disabled → Shutdown
```

### 5.6 安装插件

```bash
# 通过 API 安装
curl -X POST http://localhost:9999/plugin/install \
  -F "file=@my-plugin.tar.gz"
```

插件目录结构（打包为 tar.gz）：

```
my-plugin.tar.gz
└── my-plugin/
    ├── plugin.yaml
    ├── ... (插件代码)
```

---

## 6. 实践指南

### 6.1 如何选择扩展方式

| 需求 | 推荐方式 | 原因 |
|------|----------|------|
| 事件触发外部脚本 | **Hook** | 零编译，任意脚本语言 |
| 定时任务 / 轮询 | **Event Source** | 完整的事件生命周期管理 |
| 提供新的 LLM Tool | **Plugin (InProcess)** | 需要访问 Rust Tool trait |
| 多语言扩展 | **Plugin (Subprocess)** | 任意语言编写，JSON-RPC 通信 |
| 安全沙箱 | **Plugin (WASM)** | 内存安全，资源隔离 |
| 完整的 Agent 能力集 | **Plugin** | 可导出 skills+tools+events |

### 6.2 Hook 开发步骤

1. 在 `~/.aman/hooks/` 下创建目录
2. 编写 `config.yaml`（配置事件类型和 runtime）
3. 编写脚本（从 stdin 读 JSON）
4. 重启 aman 或等待热加载
5. 查看调试日志确认触发

### 6.3 Event Source 开发要点

- Pull 模式：实现 `poll()` 方法，返回事件列表
- Push 模式：启动服务器（HTTP/TCP），收到请求后通过 `SourceContext` 发布事件
- 实现 `on_backpressure()`：在背压时降级或暂停，恢复正常后继续
- 使用 `can_poll()` 检查是否应该轮询（背压 L3+ 时返回 false）

### 6.4 Plugin 开发要点

- 实现 `on_load()` 时注册资源，返回值表示成功/失败
- 实现 `on_unload()` 时释放资源（关闭连接、停止 goroutine）
- `on_dependency_unloading()` 在依赖即将卸载时调用，用于清理引用
- 导出 `event_sources` / `skills` / `tools` 供系统注册
- 使用 `capabilities` 声明所需能力（如 `chat`、`network`）

### 6.5 事件命名约定

- 使用 `EventType::Custom("namespace:action".to_owned())` 格式
- 命名空间用小写字母命名，如 `agent:reply_ready`
- 动作使用蛇形命名（snake_case）
- 避免与内置事件类型冲突

### 6.6 调试技巧

- **Hook 未触发**：检查事件类型名称是否匹配、runtime 是否存在、版本是否满足；检查 Hook 位置是否正确——Agent 事件（`agent:busy`、`tool:completed` 等）需放在 `~/.aman/agents/<id>/hooks/`，全局事件（`gateway:ready`、`session:started` 等）需放在 `~/.aman/hooks/`
- **Plugin 加载失败**：查看 `~/.aman/logs/` 下的日志，检查依赖图
- **事件未到达**：检查背压级别、订阅过滤条件、事件优先级
- **启用调试日志**：`RUST_LOG=debug cargo run --release --bin aman`

---

## 7. 从外部推送事件

aman 提供 HTTP API 和 CLI 两种方式，允许外部系统（浏览器插件、CI/CD、RSS 阅读器等）向 aman 推送事件。推送的事件可以是 aman 内置类型，也可以是任意的自定义类型。aman 内部的规则或 Agent LLM 决定是否处理、是否通知用户。

### 7.1 HTTP API

**推送事件 — `POST /events/push`**

```
需要认证：x-aman-token（与 aman run --token 一致）
不依赖 risky_capabilities_enabled
```

请求体：

```json
{
  "source": "browser-extension",
  "event_type": "ingest:page",
  "payload": {
    "url": "https://example.com/article",
    "title": "文章标题",
    "category": "tech"
  },
  "agent_id": "my-agent",
  "priority": "normal",
  "delivery": "at_least_once",
  "ttl_ms": 60000
}
```

| 字段 | 必填 | 说明 |
|------|------|------|
| `source` | **是** | 事件来源标识，如 `browser-extension`、`ci:github`、`rss:reader` |
| `event_type` | **是** | 事件类型。内置类型（`heartbeat`、`webhook_received` 等）或自定义（`ingest:*`、`myapp:*`）。未知字符串自动映射为 `EventType::Custom(value)` |
| `payload` | **是** | 任意 JSON 数据 |
| `agent_id` | 否 | 指定目标 Agent。有值时事件路由到该 Agent 的 Local EventBus，否则走全局 Bus |
| `priority` | 否 | 优先级：`low`、`normal`（默认）、`high` |
| `delivery` | 否 | 投递保证：`at_most_once`、`at_least_once`（默认）、`exactly_once` |
| `ttl_ms` | 否 | 事件生存时间（毫秒），超时后事件可被丢弃 |

响应：

```json
{
  "id": "019e4e62-bbdc-7b43-b83c-556c51ff2580",
  "event_type": "ingest:page",
  "target": "agent:my-agent"
}
```

`target` 字段指示事件发布目标：`"global"` 表示全局 Bus，`"agent:<id>"` 表示某个 Agent 的 Local Bus。

**用例：浏览器插件向 aman 推送发现的网页**

```bash
curl -X POST http://127.0.0.1:18080/events/push \
  -H "x-aman-token: your-token" \
  -H "Content-Type: application/json" \
  -d '{
    "source": "browser-extension",
    "event_type": "ingest:page",
    "payload": {
      "url": "https://example.com/article",
      "title": "值得阅读的文章",
      "text_snippet": "文章摘要...",
      "category": "ai"
    },
    "agent_id": "reader-bot"
  }'
```

**查看可用事件类型 — `GET /events/types`**

```bash
curl -H "x-aman-token: your-token" http://127.0.0.1:18080/events/types
```

返回内置类型列表和使用自定义类型的说明。

**事件注入（调试用） — `POST /inject-event`**

```
需要认证 + risky_capabilities_enabled = true
```

请求体与 `/events/push` 类似（无 `agent_id` 字段）。此端点仅用于调试，生产环境建议使用 `/events/push`。

### 7.2 CLI

**推送事件**

```bash
# 基本用法：推送自定义事件
aman event push \
  --source ci:github \
  --type myapp:deploy \
  --payload '{"status":"success","branch":"main"}' \
  --addr 127.0.0.1:18080 --token your-token

# 推送到指定 Agent
aman event push \
  --source rss:reader \
  --type ingest:article \
  --payload '{"title":"新文章"}' \
  --agent reader-bot \
  --addr 127.0.0.1:18080 --token your-token

# 携带优先级和投递保证
aman event push \
  --source monitoring \
  --type alert \
  --payload '{"msg":"disk full"}' \
  --priority high \
  --addr 127.0.0.1:18080 --token your-token

# 从 stdin 读取 payload（适合管道）
echo '{"key":"value"}' | aman event push \
  --source pipe \
  --type custom:data \
  --payload-stdin \
  --addr 127.0.0.1:18080 --token your-token
```

**查看可用事件类型**

```bash
aman event types --addr 127.0.0.1:18080 --token your-token
```

### 7.3 典型使用场景

**场景 1：浏览器插件发现内容**

```
浏览器插件检测到技术文章
  → POST /events/push { event_type: "ingest:page", payload: {url, title, category} }
  → Agent (reader-bot) 的 LLM 评估是否值得阅读
  → LLM 决定：保存到待读列表 / 发送通知 / 忽略
```

**场景 2：CI/CD 部署通知**

```
GitHub Actions 部署完成
  → aman event push --source ci:github --type deploy:completed --payload '{"env":"prod"}'
  → Hook 匹配 deploy:completed → 播放音效 / 发送 Slack 通知
```

**场景 3：RSS 阅读器**

```
RSS 抓取新文章
  → POST /events/push { event_type: "ingest:article", payload: {title, link, summary} }
  → Agent LLM 判断文章相关性
  → 高相关 → 自动摘要并推送通知
```

### 7.4 自定义事件类型命名建议

- 使用 `namespace:action` 格式：`ingest:page`、`deploy:completed`、`alert:disk`
- `ingest:*` 前缀表示外部数据摄入（网页、文章、RSS、日历事件等）
- 避免使用 `system.*`、`idle.*`、`agent:*` 前缀，这些为 aman 内部保留
- 自定义事件类型自动映射为 `EventType::Custom(String)`，无需预注册
