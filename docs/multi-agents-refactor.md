# Multi-Agent 数据目录与配置重构 — 架构设计

> 基于 Aman 现有 20-crate 架构（event-driven, plugin-based, Tauri v2 desktop），重构 `~/.aman/` 数据目录、`config.yaml` 结构，在保留现有运行时核心的同时支持多 Agent 管理。

---

## 1. 核心设计约束

| 约束 | 说明 |
|------|------|
| 向后兼容 | 现有 `aman.yaml` 运行时配置（event_bus, persistence, sources, 等）不改变，仅在 `config.yaml` 中追加 `providers` / `agents` / `model` 顶层字段 |
| 最少跨 crate 改动 | config crate 新增 struct；tauri crate 新增 commands + pages；其他 crate 改动最小化 |
| 事件驱动 | 所有 Agent/Provider CRUD 通过 `EventBus` 发布事件（`agent:created`, `provider:created` 等），遵循现有事件架构 |
| 安全 | API keys 绝不在 config.yaml 中明文保存 — 通过操作系统密钥链或环境变量 |
| session 索引 | 不走 workflow engine 做 session 管理——用 `sessions.db`（SQLite/sled）做独立索引 |

---

## 2. ~/.aman/ 数据目录结构

```
~/.aman/
├── config.yaml                    # 主配置文件（providers + model + agents）
├── SOUL.md                        # Framework 级别的系统 SOUL（fallback）
├── agents/                        # Agent 数据目录
│   ├── cortana/                   # 每个 agent 一个子目录，key 为目录名
│   │   ├── SOUL.md                # Agent 的 SOUL identity
│   │   ├── memory/                # Agent 长期记忆
│   │   └── sessions/              # Agent 对话 session 数据
│   │       ├── sessions.db        # SQLite 索引（所有 session 的元数据）
│   │       ├── 2026-05/           # 月级分桶
│   │       │   ├── 2026-05-14-a1b2c3d4.jsonl
│   │       │   └── 2026-05-14-e5f6g7h8.jsonl
│   │       └── 2026-06/
│   └── coder/
│       ├── SOUL.md
│       ├── memory/
│       └── sessions/
│           ├── sessions.db
│           └── 2026-06/
├── skills/                        # 全局技能目录（保持不变）
├── plugins/                       # 插件目录（保持不变）
└── runtime/                       # 运行时数据（WAL, overflow 等，保持不变）
```

### 2.1 目录创建时机

- `~/.aman/` 根目录：在首次启动 Tauri app 或运行 `aman` CLI 时自动创建
- `~/.aman/agents/`：始终存在，默认空（无子目录）
- `~/.aman/agents/{agent_key}/`：当用户在 UI 中创建 Agent 时创建
- `~/.aman/agents/{agent_key}/memory/`：首次写入 memory 时创建（lazy init）
- `~/.aman/agents/{agent_key}/sessions/`：首次创建 session 时创建（lazy init）
- `~/.aman/skills/`：始终存在

### 2.2 Session 文件命名规则

```
sessions/{yyyy-MM}/{yyyy-MM-dd}-{short_id}.jsonl
```

- `short_id`：8 字符，取 UUID v7 的前 8 位 hex
- 同一天可创建多个 session 文件，用 short_id 区分
- JSONL 格式：每行一个完整的 JSON 消息对象
- `sessions.db`：SQLite 数据库，字段：
  - `session_id TEXT PRIMARY KEY`
  - `agent_key TEXT NOT NULL`
  - `title TEXT`
  - `file_path TEXT NOT NULL` — 指向具体的 JSONL 文件
  - `message_count INTEGER`
  - `created_at INTEGER`
  - `last_active_at INTEGER`

---

## 3. config.yaml 新的结构

### 3.1 完整示例

```yaml
# =========================================================
# 第 1 部分：运行时配置（向后兼容现有 aman.yaml）
# =========================================================
event_bus:
  mode: in_memory

persistence:
  wal_sync: fsync

skills:
  dir: ~/.aman/skills

# ... 其他 运行时相关字段保持不变 ...

# =========================================================
# 第 2 部分：LLM Provider 配置
# =========================================================
providers:
  openai:                             # provider key（唯一标识）
    display_name: OpenAI              # UI 显示名
    base_url: https://api.openai.com/v1
  deepseek:
    display_name: DeepSeek
    base_url: https://api.deepseek.com/v1
  anthropic:
    display_name: Anthropic Claude
    base_url: https://api.anthropic.com/v1

# =========================================================
# 第 3 部分：默认 LLM 模型
# =========================================================
model:
  default: deepseek-v4-pro
  provider: deepseek
  base_url: https://api.deepseek.com/v1

# =========================================================
# 第 4 部分：Agent 配置
# =========================================================
agents:
  cortana:
    display_name: Cortana             # UI 显示名
    provider: openai
    model: gpt-5.4-flash
    system_prompt_override: null      # 可选：覆盖 SOUL.md 的 system prompt
  coder:
    display_name: Coder
    provider: deepseek
    model: deepseek-v4-pro
    system_prompt_override: null
```

### 3.2 API Key 存储设计

**Config.yaml 中不包含 API key**。有两种存储方案：

**方案 A（推荐）：macOS Keychain**
```
aman config set openai:api_key sk-xxx
```
→ 写入 macOS Keychain，key 为 `aman.providers.openai.api_key`

**方案 B（备选）：环境变量**
```
export AMAN_PROVIDER_OPENAI_API_KEY=sk-xxx
```

**Crate 层面**：`secret` crate 已支持多后端密钥解析。新增 `ProviderSecretResolver` 在 `config` crate 中：

```
ProviderSecretResolver
  .resolve("openai", "api_key")
  → Keychain → Env 回退 → None
```

### 3.3 Provider 唯一性约束

- `provider key` 只能在英文大小写字母、数字、下划线 `_`、短横线 `-` 中选择
- `agent key` 同上
- provider key 不可重复

### 3.4 Config 解析流程

```
┌─────────────┐     ┌─────────────────┐     ┌───────────────────┐
│ config.yaml │────▶│ AmanMultiConfig  │────▶│ Validation        │
│ (YAML)      │     │                 │     │ - provider 不重名  │
└─────────────┘     │ + providers: {}  │     │ - agent 不重名    │
                    │ + model: {}      │     │ - agent.provider  │
                    │ + agents: {}     │     │   必须存在于      │
                    │ + runtime: ...   │     │   providers 中    │
                    └─────────────────┘     └───────────────────┘
```

---

## 4. Config Crate 改动

### 4.1 新增 Struct

```rust
// crates/config/src/lib.rs

/// 单个 LLM Provider 配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderConfig {
    pub display_name: String,
    pub base_url: String,
}

/// 默认 LLM 模型配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DefaultModelConfig {
    pub default: String,       // 模型名
    pub provider: String,      // provider key
    pub base_url: String,
}

/// 单个 Agent 配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentEntryConfig {
    pub display_name: String,
    pub provider: String,      // 引用 providers 中的 key
    pub model: String,
    pub system_prompt_override: Option<String>,
}

/// 多 Agent 全量配置（包含运行时 + providers + agents）
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AmanConfig {
    // 运行时配置（保持现有 AgentConfig 不变）
    #[serde(flatten)]
    pub runtime: AgentConfig,

    // LLM Provider 列表（key → ProviderConfig）
    #[serde(default)]
    pub providers: HashMap<String, ProviderConfig>,

    // 默认模型
    pub model: Option<DefaultModelConfig>,

    // Agent 列表（key → AgentEntryConfig）
    #[serde(default)]
    pub agents: HashMap<String, AgentEntryConfig>,
}
```

### 4.2 Validator 扩展

```rust
impl AmanConfig {
    pub fn validate_full(&self) -> AmanResult<Vec<String>> {
        // 1. 原有的 runtime 验证
        let mut warnings = self.runtime.validate()?;

        // 2. Provider key 合法性
        for key in self.providers.keys() {
            if !is_valid_identifier(key) {
                return Err(Error::config_invalid(
                    format!("Provider key '{key}' 只能包含英文字母、数字、下划线、短横线")
                ));
            }
        }

        // 3. Agent key 合法性
        for key in self.agents.keys() {
            if !is_valid_identifier(key) {
                return Err(Error::config_invalid(
                    format!("Agent key '{key}' 只能包含英文字母、数字、下划线、短横线")
                ));
            }
        }

        // 4. Agent 引用的 provider 必须存在
        for (agent_key, agent) in &self.agents {
            if !self.providers.contains_key(&agent.provider) {
                warnings.push(format!(
                    "Agent '{agent_key}' 的 provider '{}' 未在 providers 中定义",
                    agent.provider
                ));
            }
        }

        Ok(warnings)
    }
}

fn is_valid_identifier(s: &str) -> bool {
    !s.is_empty()
        && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}
```

### 4.3 Config Loader 改动

```rust
// 新增加载函数
impl AmanConfig {
    /// 从 ~/.aman/config.yaml 加载完整配置
    pub fn from_default_path() -> AmanResult<Self> {
        let path = default_config_path();
        Self::from_file(&path)
    }

    pub fn from_file(path: &Path) -> AmanResult<Self> {
        let content = fs::read_to_string(path)?;
        let config: AmanConfig = serde_yaml::from_str(&content)
            .map_err(|e| Error::config_invalid(format!("解析 config.yaml 失败: {e}")))?;
        config.validate_full()?;
        Ok(config)
    }

    /// 保存配置到文件（保留注释的难度高，用最小侵入式序列化）
    pub fn save(&self, path: &Path) -> AmanResult<()> {
        let content = serde_yaml::to_string(self)
            .map_err(|e| Error::config_invalid(format!("序列化配置失败: {e}")))?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(path, &content)?;
        Ok(())
    }
}
```

---

## 5. Tauri Backend 新增 IPC Commands

新的 command 放在 `crates/tauri/src/commands.rs`，按域分组：

### 5.1 Provider Commands

| Command | 签名 | 说明 |
|---------|------|------|
| `list_providers` | → `Vec<ProviderEntry>` | 列出所有 provider（不含 api_key） |
| `create_provider` | `key, display_name, base_url` → 成功/失败 | 创建新 provider 并保存到 config.yaml |
| `update_provider` | `key, display_name?, base_url?` → 成功/失败 | 更新 provider 配置 |
| `delete_provider` | `key` → 成功/失败 | 删除 provider（检查是否有 agent 引用） |
| `set_provider_api_key` | `key, api_key` → 成功/失败 | 将 API key 保存到密钥链（不写入 config.yaml） |
| `has_provider_api_key` | `key` → `bool` | 检查 provider 是否已配置 API key |

**ProviderEntry 模型**（新增在 `models.rs`）：
```rust
#[derive(Debug, Clone, Serialize)]
pub struct ProviderEntry {
    pub key: String,
    pub display_name: String,
    pub base_url: String,
    pub has_api_key: bool,        // 由 ProviderSecretResolver 决定
}
```

### 5.2 Agent Commands

| Command | 签名 | 说明 |
|---------|------|------|
| `list_agents` | → `Vec<AgentEntry>` | 列出所有 agent |
| `create_agent` | `key, display_name, provider, model, soul_content` → 成功/失败 | 创建 agent 目录、SOUL.md、写入 config.yaml |
| `update_agent` | `key, display_name?, provider?, model?, soul_content?` → 成功/失败 | 更新 agent |
| `delete_agent` | `key` → 成功/失败 | 删除 agent 目录和 config.yaml 中的条目 |
| `get_agent_soul` | `key` → `String` | 读取 agent 的 SOUL.md 内容 |
| `select_agent` | `key` → 成功/失败 | 设定当前活动的 agent（触发 `agent:selected` 事件）|
| `get_active_agent` | → `Option<AgentEntry>` | 获取当前选中的 agent |

**AgentEntry 模型**（新增在 `models.rs`）：
```rust
#[derive(Debug, Clone, Serialize)]
pub struct AgentEntry {
    pub key: String,
    pub display_name: String,
    pub provider: String,
    pub model: String,
    pub soul_summary: String,     // SOUL.md 的前几行摘要
    pub session_count: u64,
    pub is_active: bool,
}
```

### 5.3 Agent Session Commands

| Command | 签名 | 说明 |
|---------|------|------|
| `agent_list_sessions` | `agent_key` → `Vec<AgentSessionSummary>` | 列出 agent 的所有 session |
| `agent_get_session` | `agent_key, session_id` → `Vec<ChatMessage>` | 读取 session 内容 |
| `agent_delete_session` | `agent_key, session_id` → 成功/失败 | 删除 session |

### 5.4 Config/Status Commands

| Command | 签名 | 说明 |
|---------|------|------|
| `get_aman_config` | → `AmanConfig` | 读取完整配置（不含 API keys） |
| `has_any_provider` | → `bool` | 检查是否有至少一个 provider |
| `has_any_agent` | → `bool` | 检查是否有至少一个 agent |
| `get_default_model` | → `Option<DefaultModelConfig>` | 获取默认模型 |

---

## 6. Tauri Frontend 新增 Pages

### 6.1 页面路由

```
App.svelte 导航栏新增：
├── Dashboard        (现有)
├── Providers        ← 新增：provider 管理页面
├── Agents           ← 新增：agent 管理页面（+ 选择进入 Chat）
├── [Chat]           ← 现有（改为 agent 绑定模式）
├── Skills           (现有)
└── ...
```

### 6.2 Onboarding 流程

用户在 `sidebar` 中看到 Providers 和 Agents 页面。点击 Providers 进入 provider 管理。

```
┌──────────────────────────────────────────┐
│  启动 Tauri App                           │
│                                          │
│  ┌─── 检查 config.yaml ──────────────┐   │
│  │ has_any_provider() = false?       │   │
│  │  → 弹窗/横幅: "请先配置 LLM       │   │
│  │    Provider 以开始使用 Aman"      │   │
│  │  → "去配置 Provider" 按钮         │   │
│  │  → 导航到 /providers 页面         │   │
│  └─────────────────────────────────┘   │
│                                         │
│  ┌─── 启动 Runtime ────────────────┐   │
│  │ runtime 启动后，检查 agents      │   │
│  │ has_any_agent() = false?        │   │
│  │  → 左侧 Agents 标签显示         │   │
│  │    "创建第一个 Agent" 按钮       │   │
│  │  → 导航到 /agents/new 页面      │   │
│  └─────────────────────────────────┘   │
│                                         │
│  ┌─── Agent 选定后 ───────────────┐   │
│  │ → Chat 页面以 selected agent    │   │
│  │   的身份和 SOUL 进行对话        │   │
│  └─────────────────────────────────┘   │
└──────────────────────────────────────────┘
```

### 6.3 Providers 页面（`Providers.svelte`）

```
┌─────────────────────────────────────────────┐
│  Providers                      [+ 新增]    │
│                                             │
│  ┌──────────────────────────────────────┐   │
│  │ openai                               │   │
│  │ base_url: https://api.openai.com/v1   │   │
│  │ API Key: ●●●●●●●● [编辑] [删除]      │   │
│  └──────────────────────────────────────┘   │
│                                             │
│  ┌──────────────────────────────────────┐   │
│  │ deepseek                              │   │
│  │ base_url: https://api.deepseek.com/v1  │   │
│  │ API Key: [设置] [编辑] [删除]         │   │
│  └──────────────────────────────────────┘   │
└─────────────────────────────────────────────┘
```

**新增 Provider 对话框**：
```
┌─── Provider 创建 ─────────────────┐
│ Key: [deepseek]                     │
│ Display Name: [DeepSeek]            │
│ Base URL: [https://api.deepseek...] │
│ API Key: [****************]         │
│                                     │
│ [取消]            [创建 Provider]   │
└─────────────────────────────────────┘
```

**关键行为**：
- 创建成功后触发 `provider:created` 事件
- 创建后更新 config.yaml

### 6.4 Agent 管理页面（`Agents.svelte`）

```
┌─────────────────────────────────────────────┐
│  Agents                         [+ 新增]    │
│                                             │
│  ┌──────────────────────────────────────┐   │
│  │ ▸ Cortana                            │   │
│  │   openai · gpt-5.4-flash · 12 sessions│   │
│  │   [选择并聊天] [编辑] [删除]          │   │
│  └──────────────────────────────────────┘   │
│                                             │
│  ┌──────────────────────────────────────┐   │
│  │ ▸ Coder                              │   │
│  │   deepseek · deepseek-v4-pro · 5 sessions│   │
│  │   [选择并聊天] [编辑] [删除]          │   │
│  └──────────────────────────────────────┘   │
└─────────────────────────────────────────────┘
```

**创建 Agent 表单**：
```
┌─── 创建新 Agent ─────────────────────┐
│ Key: [cortana]                        │
│ Display Name: [Cortana]              │
│ Provider: [▼ openai ...]             │
│ Model: [gpt-5.4-flash]               │
│                                       │
│ ┌─ SOUL.md ─────────────────────────┐ │
│ │ # Cortana                          │ │
│ │                                     │ │
│ | ## Identity                        │ │
│ | I am Cortana...                     │ │
│ └───────────────────────────────────┘ │
│                                       │
│ [取消]            [创建 Agent]        │
└───────────────────────────────────────┘
```

**关键行为**：
- Agent Key 校验：仅允许英文大小写字母、数字、下划线、短横线
- 创建时在 `~/.aman/agents/{key}/` 下创建目录
- 创建时写入 `SOUL.md` 文件
- 创建后更新 `config.yaml`
- 创建后触发 `agent:created` 事件
- 用户无法选择不存在的 provider（下拉框只呈现 config.yaml 中已有的 provider）

### 6.5 Chat 页面改动

现有 `Chat.svelte` 增加 agent 选择头部：

```
┌───── Agent: [Cortana ▼] ───── Runtime: ● ──┐
│                                               │
│  [≡ Messages...]                              │
│                                               │
│  [Input box...]                   [Send]      │
└───────────────────────────────────────────────┘
```

- Agent 下拉框列出所有 agents
- 切换 agent 时：
  - 当前对话保存（如果存在活跃会话）
  - 调用 `select_agent(key)` IPC
  - 触发 `agent:selected` 事件
  - SOUL 加载新 agent 的身份进 Runtime

---

## 7. 事件系统

新增事件类型，全部通过现有的 EventBus 发布：

### 7.1 Provider 事件

```rust
// kernel::event::EventType 新增
EventType::Custom("provider:created".into())
EventType::Custom("provider:updated".into())
EventType::Custom("provider:deleted".into())
EventType::Custom("provider:api_key_set".into())
```

Payload 示例（provider:created）：
```json
{
    "key": "deepseek",
    "timestamp_ms": 1747234500000
}
```

### 7.2 Agent 事件

```rust
EventType::Custom("agent:created".into())
EventType::Custom("agent:updated".into())
EventType::Custom("agent:deleted".into())
EventType::Custom("agent:selected".into())
```

Payload 示例（agent:created）：
```json
{
    "key": "cortana",
    "timestamp_ms": 1747234500000
}
```

### 7.3 事件消费方

| 事件 | 前端监听 | 后端监听 |
|------|---------|---------|
| `provider:created` | 刷新 Providers 页面列表 | — |
| `provider:updated` | 刷新 Providers 页面列表 | — |
| `provider:deleted` | 刷新 Providers 页面列表；检查是否有 agent 引用此 provider | — |
| `agent:created` | 刷新 Agents 页面列表；左侧 sidebar agents 标签更新 | — |
| `agent:selected` | Chat 页面加载 SOUL | AgentSelector plugin 切换 active agent 上下文 |
| `agent:deleted` | 刷新 Agents 页面列表 | — |

---

## 8. AppState 改动

```rust
// crates/tauri/src/state.rs

use config::AmanConfig;
use std::sync::Arc;
use tokio::sync::Mutex;
use std::time::Duration;
use crate::rate_limiter::SlidingWindowRateLimiter;

pub struct AppState {
    pub runtime: Arc<Mutex<Option<Arc<AgentRuntime>>>>,
    pub rate_limiter: SlidingWindowRateLimiter,

    // 新增：active agent tracking
    pub active_agent_key: Arc<Mutex<Option<String>>>,

    // 新增：config 文件的 in-memory cache
    pub config: Arc<Mutex<Option<AmanConfig>>>,
}

impl AppState {
    pub fn new() -> Self {
        Self {
            runtime: Arc::new(Mutex::new(None)),
            rate_limiter: SlidingWindowRateLimiter::new(
                Duration::from_secs(60), 10
            ),
            active_agent_key: Arc::new(Mutex::new(None)),
            config: Arc::new(Mutex::new(None)),
        }
    }

    /// 从磁盘加载 config.yaml
    pub async fn load_config(&self) -> AmanResult<AmanConfig> {
        let config = AmanConfig::from_default_path()?;
        let mut guard = self.config.lock().await;
        *guard = Some(config.clone());
        Ok(config)
    }

    /// 保存 config.yaml 并刷新缓存
    pub async fn save_config(&self, config: &AmanConfig) -> AmanResult<()> {
        let path = default_config_path();
        config.save(&path)?;
        let mut guard = self.config.lock().await;
        *guard = Some(config.clone());
        Ok(())
    }
}
```

---

## 9. 文件系统操作（新增 utility 模块）

`crates/tauri/src/agent_fs.rs` — Agent 文件系统操作的封装：

```rust
/// 创建 Agent 文件系统结构
pub fn init_agent_dir(
    base_dir: &Path,    // ~/.aman/agents
    key: &str,
    soul_content: &str,
) -> AmanResult<()> {
    let agent_dir = base_dir.join(key);
    if agent_dir.exists() {
        return Err(Error::config_invalid(format!(
            "Agent '{key}' 已存在"
        )));
    }

    // 创建目录结构
    fs::create_dir_all(agent_dir.join("memory"))?;
    fs::create_dir_all(agent_dir.join("sessions"))?;

    // 写入 SOUL.md
    fs::write(agent_dir.join("SOUL.md"), soul_content)?;

    // 初始化 sessions.db（SQLite 建表）
    init_sessions_db(&agent_dir.join("sessions").join("sessions.db"))?;

    Ok(())
}

/// 删除 Agent 文件系统结构
pub fn remove_agent_dir(base_dir: &Path, key: &str) -> AmanResult<()> {
    let agent_dir = base_dir.join(key);
    if !agent_dir.exists() {
        return Err(Error::config_invalid(format!(
            "Agent '{key}' 不存在"
        )));
    }
    fs::remove_dir_all(agent_dir)?;
    Ok(())
}

/// 写入一条 session 消息到 JSONL
pub fn append_session_message(
    base_dir: &Path,
    key: &str,
    timestamp: chrono::NaiveDateTime,
    message: &ChatMessage,
) -> AmanResult<PathBuf> {
    let month_dir = format!("{}", timestamp.format("%Y-%m"));
    let day_prefix = format!("{}", timestamp.format("%Y-%m-%d"));
    let short_id = &uuid::Uuid::now_v7().to_string()[..8];
    let filename = format!("{}-{}.jsonl", day_prefix, short_id);

    let session_file = base_dir
        .join(key)
        .join("sessions")
        .join(&month_dir)
        .join(&filename);

    // 确保月级目录存在
    if let Some(parent) = session_file.parent() {
        fs::create_dir_all(parent)?;
    }

    // append JSONL
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&session_file)?;
    writeln!(file, "{}", serde_json::to_string(&message)?)?;

    Ok(session_file)
}
```

---

## 10. API Key 安全存储

### 10.1 当前 `secret` crate 能力

现有 `crates/secret` 支持多后端（env, vault, aws, 1password），但缺少 macOS Keychain。

### 10.2 新增 Keychain 后端

```rust
// crates/secret 新增 macOS Keychain 后端
// 使用 security-framework crate

pub struct KeychainBackend;

impl SecretBackend for KeychainBackend {
    fn get(&self, key: &str) -> Option<String> {
        // 通过 `security find-generic-password` CLI
        // 或 security_framework::osx::keychain 直接调用
    }

    fn set(&self, key: &str, value: &str) -> Result<()> {
        // 通过 `security add-generic-password` CLI
    }
}
```

### 10.3 API Key Identifier 规范

```
namespace: aman.providers.{provider_key}.api_key
account: aman-desktop
```

Keychain 条目示例：
```
service: aman
account: aman-desktop
label: aman.providers.deepseek.api_key
```

---

## 11. crate 依赖关系与改动范围总览

```
crates/config         — 新增 AmanConfig, ProviderConfig, AgentEntryConfig struct
                        新增 AMAN_* 环境变量支持（AMAN_PROVIDER_*）
crates/tauri          — 新增 commands（provider/agent CRUD）
                        新增 models（ProviderEntry, AgentEntry）
                        新增 agent_fs.rs（文件系统操作）
                        新增 pages（Providers.svelte, Agents.svelte, AgentCreate.svelte）
                        修改 App.svelte（路由 + 导航 + onboarding 逻辑）
                        修改 Chat.svelte（agent 选择器）
                        修改 AppState（active_agent_key, config cache）
                        修改 start_runtime command（依赖 agent 上下文）
crates/secret         — 可选新增 Keychain 后端
crates/core/kernel    — 可选新增事件类型常量（provider:created, agent:created 等）
crates/runtime        — 无需改动（仍接收事件）
其他 crate            — 无需改动
```

---

## 12. 启动与导航流程图

```
┌──────────────┐
│  App 启动     │
│  onMount()    │
└──────┬───────┘
       │
       ▼
┌──────────────────────┐     ┌──────────────────────┐
│ config.yaml 存在？    │ NO  │ 导航到 /providers    │
│ has_any_provider()   │────▶│ 显示引导提示          │
└──────┬───────────────┘     └──────────────────────┘
       │ YES
       ▼
┌──────────────────────────────┐
│ Sidebar 显示正常导航           │
│ Providers, Agents, Chat 可用 │
└──────┬───────────────────────┘
       │
       ▼ (用户点击 Start Runtime)
┌──────────────────────────────┐     ┌──────────────────────────┐
│ Runtime 启动后               │ NO  │ Agents 标签显示引导按钮   │
│ has_any_agent()             │────▶│ "创建第一个 Agent"        │
└──────┬───────────────────────┘     └──────────────────────────┘
       │ YES
       ▼
┌──────────────────────────────┐
│ Agents 标签可点击            │
│ 用户选择 agent → 进入 Chat  │
│ Chat 页面绑定 agent SOUL     │
└──────────────────────────────┘
```

---

## 13. session 数据格式

### 13.1 JSONL 消息格式

每行一个消息对象，与现有 Chat 的 `ChatMessageEntry` 兼容：

```json
{
    "id": "uuid-v7",
    "role": "user",
    "content": "你好，帮我写一段代码",
    "timestamp_ms": 1747234500000,
    "trace_id": "uuid-v7",
    "session_id": "uuid-v7",
    "message_type": "user_text",
    "agent_key": "cortana"
}
```

```json
{
    "id": "uuid-v7",
    "role": "assistant",
    "content": "当然，请问你需要什么类型的代码？",
    "timestamp_ms": 1747234501000,
    "trace_id": "uuid-v7",
    "session_id": "uuid-v7",
    "message_type": "assistant_text"
}
```

### 13.2 sessions.db Schema

```sql
CREATE TABLE IF NOT EXISTS sessions (
    session_id    TEXT PRIMARY KEY,
    agent_key     TEXT NOT NULL,
    title         TEXT DEFAULT '',
    file_path     TEXT NOT NULL,         -- 指向具体 JSONL 文件
    message_count INTEGER DEFAULT 0,
    created_at    INTEGER NOT NULL,
    last_active_at INTEGER NOT NULL,
    status        TEXT DEFAULT 'idle'    -- idle / processing
);

CREATE INDEX idx_sessions_agent_key ON sessions(agent_key);
CREATE INDEX idx_sessions_last_active ON sessions(last_active_at);
```

---

## 14. 实施路线图

| Phase | Scope | Dependencies | 预估工作量 |
|-------|-------|-------------|-----------|
| **P1** | config crate: `AmanConfig`, `ProviderConfig`, `AgentEntryConfig`, `validate_full()` | 无 | crates/config: ~200 lines |
| **P1** | `AmanConfig::save()` + `is_valid_identifier()` | P1 config | ~50 lines |
| **P2** | Tauri backend: `list_providers`, `create_provider`, `delete_provider`, API Key 存储命令 | P1 config | crates/tauri: ~300 lines |
| **P2** | Tauri backend: `agent_fs.rs` — `init_agent_dir`, `remove_agent_dir`, session 写入 | P1 config | ~200 lines |
| **P2** | Tauri backend: `list_agents`, `create_agent`, `delete_agent`, `select_agent` | P2 agent_fs | ~250 lines |
| **P3** | Tauri frontend: `Providers.svelte` 页面 | P2 commands | ~200 lines |
| **P3** | Tauri frontend: `Agents.svelte` + `AgentCreate.svelte` 页面 | P2 commands | ~300 lines |
| **P3** | Tauri frontend: App.svelte onboarding 流程 + sidebar 改动 | P3 pages | ~100 lines |
| **P3** | Tauri frontend: Chat.svelte agent 选择器 | P3 pages | ~100 lines |
| **P4** | AppState: active_agent_key, config cache | P1 | ~80 lines |
| **P4** | session 写入 + sessions.db | P2 agent_fs | ~200 lines |
| **P5** | macOS Keychain backend in secret crate | 无 | ~150 lines |
| **P5** | CLI commands for provider/agent management | P1 | ~150 lines |

---

## 15. 风险与缓解

| 风险 | 概率 | 影响 | 缓解方案 |
|------|------|------|---------|
| config.yaml 并发写入冲突 | 低 | 高 | 使用 `AppState.config` 缓存 + 写入时加锁；避免多个 Tauri window 同时写入 |
| API Key 泄露（如果选择纯文件存储） | 中 | 高 | **强制**使用 Keychain 或环境变量；config.yaml 只存 base_url |
| Agent 数量多导致启动慢 | 低 | 低 | 使用 lazy init，首次启动只读取 config.yaml metadata |
| sessions.db 和 JSONL 文件不同步 | 中 | 中 | session 写入时使用事务（先写 db + 后写文件，逆序处理回滚） |
| 删除 agent 时 session 数据丢失 | 中 | 中 | 确认前弹窗警告；提供导出选项后再删除 |
