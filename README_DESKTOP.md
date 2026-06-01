# Aman Desktop — 你的 AI 伙伴

<p align="center">
  <img src="desktop/icons/128x128@2x.png" alt="Aman" width="128" height="128" />
</p>

**Aman Desktop** 是一款为普通人设计的桌面 AI 应用。它不是冷冰冰的「助手」或「工具」，而是一个有性格、有节奏、会主动关心你的**拟人化 AI 伙伴**。

> 底层基于 [Aman](https://github.com/13F/aman) 多智能体引擎，但你不必关心什么是「智能体」—— 打开它，就像认识一个新朋友。

---

## 为什么选择 Aman Desktop？

### 🤖 不是工具，是伙伴

大多数 AI 产品定位为「提高效率的工具」。Aman 不同：它有**情绪维度**（arousal / boredom / valence），有**空闲模式**，会主动观察、主动建议。它不会在你不需要的时候打扰你，也不会在你需要的时候沉默。

### 🧠 比你想象的更懂你

Aman 支持多个 AI 模型（OpenAI、Claude、Gemini 等），你可以创建**多个 Agent**，每个有不同的「灵魂」（SOUL）。比如：

- **工作 Aman**：严谨、结构化，熟悉你的项目
- **生活 Aman**：轻松、幽默，记得你的喜好
- **理财 Aman**：冷静、数据驱动，跟踪市场动态

### 🏠 完全本地，你的数据你做主

所有数据存储在本地（`~/.aman/`），API Key 存在系统钥匙串（macOS Keychain）。Aman 不会上传你的数据到任何地方。它是**你的**伙伴，不是云端的服务。

---

## 主要功能

### 💬 聊天 —— 不止是对话

- **实时流式回复**：看到 Aman 「思考」的每一个字
- **工具调用**：Aman 可以搜索网页、读写文件、执行脚本，并展示每一步在做什么
- **会话管理**：像聊天软件一样管理多个会话，支持分支、重命名、导出
- **聊天输入增强**：输入 `/` 自动弹出技能菜单，支持 Markdown

### 🎯 主动智能

- **空闲模式**：当你不说话，Aman 会观察、学习、准备建议
- **智能通知**：重要事件主动推送（可配置严重级别）
- **例程支持**：定时任务、日常提醒，像一个靠谱的朋友

### 🔌 插件 & 集成

- **多平台即时通讯**：接入 Telegram、Slack、Discord、Matrix
- **第三方搜索**：Tavily、Brave、DuckDuckGo、Google 可编程搜索
- **插件系统**：可扩展的 WASM / 子进程插件

### 🛠 开发者友好

- **代码 Agent**：一键在终端启动 Claude Code、Codex、OpenCode、Gemini CLI、Kimi、Grok
- **工作流看板**：可视化工作流状态机，实时追踪每个执行步骤
- **调试面板**：事件日志、DLQ（死信队列）、指标监控

---

## 安装

### 系统要求

- **操作系统**：macOS 12+（Windows / Linux 计划中）
- **架构**：Apple Silicon（M1/M2/M3/M4）或 Intel Mac

### 从源码构建

```bash
# 克隆仓库
git clone https://github.com/13F/aman.git
cd aman

# 安装前端依赖
cd desktop && npm install && cd ..

# 构建桌面应用
cargo build --release -p aman-desktop

# 运行
./target/release/aman-desktop
```

---

## 快速开始

### 1. 添加 AI 提供商

打开 Aman Desktop，进入 **Services → Providers**，点击 **Add Provider**：

| 提供商 | 需要 |
|--------|------|
| OpenAI | API Key（在 platform.openai.com 获取） |
| Anthropic | API Key（在 console.anthropic.com 获取） |
| 其他兼容 OpenAI 接口的服务 | API Key + Base URL |

### 2. 创建你的第一个 Agent

进入 **Services → Agents**，点击 **Create Agent**：
- 选择一个提供商和模型
- 为 Agent 起个名字（比如 「小安」）
- 编写他的「灵魂」（SOUL）—— 定义他的性格、说话方式、擅长领域

### 3. 开始对话

回到 **Workspace → Chat**，选择你的 Agent，开始聊天。

---

## 界面导览

```
┌──────────────────────────────────────────────────┐
│  ┌──────────┐  ┌────────────────────────────────┐ │
│  │          │  │                                │ │
│  │  Sidebar │  │        Main Content            │ │
│  │          │  │                                │ │
│  │ • Home   │  │  ┌──────────┐ ┌──────────┐    │ │
│  │ • Chat   │  │  │  Agent   │ │   Code   │    │ │
│  │          │  │  │  Card    │ │  Agents  │    │ │
│  │ Services │  │  └──────────┘ └──────────┘    │ │
│  │ • Agents │  │                                │ │
│  │ • Provid.│  │  ┌─────────────────────────┐   │ │
│  │ • Integr.│  │  │    Finance Cards        │   │ │
│  │ • Dashb. │  │  └─────────────────────────┘   │ │
│  │          │  │                                │ │
│  │ Managem. │  │                                │ │
│  │ • Workfl.│  │                                │ │
│  │ • Plugins│  │                                │ │
│  │ • Mainte.│  │                                │ │
│  │ • Settin.│  │                                │ │
│  │          │  │                                │ │
│  │ ──────── │  │                                │ │
│  │ ● Online │  │                                │ │
│  │ Agent    │  │                                │ │
│  └──────────┘  └────────────────────────────────┘ │
└──────────────────────────────────────────────────┘
```

| 区域 | 功能 |
|------|------|
| **Home** | Agent 一览，查看空闲状态、启动代码 Agent、理财卡片 |
| **Chat** | 对话界面，会话列表、消息区、输入框 |
| **Agents** | 创建和管理多个 AI Agent |
| **Providers** | 配置 AI 模型提供商（OpenAI、Anthropic 等） |
| **Integration** | 第三方服务 API Key、IM 渠道配置 |
| **Dashboard** | 运行时状态、启停控制、指标面板 |
| **Workflow Board** | 工作流执行状态、重试/取消操作 |
| **Plugin Manager** | 插件启用/禁用管理 |
| **Maintenance** | 调试工具：事件日志、DLQ、指标 |
| **Settings** | 设置（持续重构中） |

---

## 设计理念

### 「无声陪伴」

Aman 的设计灵感来源于**真正的伙伴关系**：他不会在你专注时打断你，也不会在你需要时缺席。侧边栏的状态环（Idle Ring）可视化 Agent 的「无聊度」和「唤醒度」，让你直观感知他的状态。

### 「灵魂优先」

在 Aman 的世界里，每个 Agent 都有**灵魂**（SOUL）。这不是一个提示词模板，而是 Agent 的性格、记忆、价值观的综合。SOUL 文件（`~/.aman/agents/*/SOUL.md`）是纯文本，你可以随时编辑。

### 「对你透明」

- 工具调用的每一步都有展开式卡片
- SSE 事件流显示在维护面板
- 所有配置存储在本地，可随时查看和修改
- 没有遥测，没有数据收集

---

## 技术架构

```
┌─────────────────────────────────────────────┐
│               Aman Desktop                  │
│  ┌───────────────────────────────────────┐  │
│  │         Svelte 5 前端 (WebView)       │  │
│  │  ┌─────┐ ┌────┐ ┌────┐ ┌──────────┐  │  │
│  │  │Chat │ │Home│ │Dash│ │Settings.. │  │  │
│  │  └─────┘ └────┘ └────┘ └──────────┘  │  │
│  └──────────────┬────────────────────────┘  │
│                 │ Tauri IPC (237 commands)   │
│  ┌──────────────┴────────────────────────┐  │
│  │       Rust 后端 (tokio async)         │  │
│  │  ┌──────────┐ ┌──────────┐           │  │
│  │  │ Gateway  │ │ SSE      │           │  │
│  │  │ HTTP     │ │ Listener │           │  │
│  │  │ Client   │ │          │           │  │
│  │  └──────────┘ └──────────┘           │  │
│  │  ┌──────────┐ ┌──────────┐           │  │
│  │  │Keychain  │ │ Config   │           │  │
│  │  │Backend   │ │ Reader   │           │  │
│  │  └──────────┘ └──────────┘           │  │
│  └──────────────┬────────────────────────┘  │
│                 │ HTTP REST + SSE            │
│  ┌──────────────┴────────────────────────┐  │
│  │       Aman Gateway (子进程)           │  │
│  │  ┌──────────────────────────────────┐ │  │
│  │  │ 30+ core crates, 8+ plugins     │ │  │
│  │  │ • Event Bus • Pipeline • Workflow│ │  │
│  │  │ • Skill • Tool • LLM API       │ │  │
│  │  │ • Memory • Notification • ...   │ │  │
│  │  └──────────────────────────────────┘ │  │
│  └────────────────────────────────────────┘  │
└─────────────────────────────────────────────┘
```

- **前端**：Svelte 5 + TypeScript + Vite
- **后端**：Rust + Tokio + Tauri v2
- **桌面框架**：Tauri v2（比 Electron 更轻量，不捆绑 Chromium）
- **引擎**：Aman Gateway 作为子进程运行
- **安全**：API Key 存储在 macOS Keychain；输出内容经过安全滤波器

---

## 开发

### 目录结构

```
desktop/
├── src/
│   ├── main.rs              # 二进制入口
│   ├── lib.rs               # Tauri 应用构建 & 生命周期
│   ├── commands.rs           # 237 个 Tauri IPC 命令
│   ├── gateway_client.rs    # 与 Aman Gateway 的 HTTP 通信
│   ├── sse_client.rs        # SSE 事件流监听
│   ├── models.rs            # 数据模型类型
│   ├── state.rs             # 共享应用状态
│   ├── agent_fs.rs          # Agent 文件系统操作
│   ├── code_agents.rs       # 代码 Agent 启动器
│   ├── finance_cards.rs     # 理财卡片管理
│   ├── rate_limiter.rs      # 滑动窗口限流器
│   ├── App.svelte           # 根组件（侧边栏 + 路由）
│   ├── app.css              # 全局样式系统（暗色/亮色主题）
│   └── pages/               # 所有页面组件
│       ├── Home.svelte
│       ├── Chat.svelte
│       ├── Dashboard.svelte
│       ├── Agents.svelte
│       ├── Providers.svelte
│       ├── Integration.svelte
│       ├── Settings.svelte
│       ├── WorkflowBoard.svelte
│       ├── PluginManager.svelte
│       ├── Maintenance.svelte
│       └── ...
├── icons/                   # 应用图标
├── capabilities/            # Tauri 安全能力声明
├── Cargo.toml
├── package.json
├── tauri.conf.json
└── vite.config.ts
```

### 开发命令

```bash
# 进入桌面目录
cd desktop

# 安装前端依赖
npm install

# 开发模式（热重载前端）
cargo tauri dev

# 构建发布版
cargo tauri build

# 仅构建 Rust 后端
cargo build --release -p aman-desktop
```

---

## 路线图

- [ ] Windows & Linux 支持
- [ ] 移动端应用（iOS / Android）
- [ ] 语音对话
- [ ] 本地模型支持（Ollama / llama.cpp）
- [ ] Agent 市场（分享和发现 Agent 灵魂）
- [ ] 多人协作 Agent

---

## 许可证

Aman Desktop 使用 [AGPL-3.0](LICENSE) 许可证。

Copyright © 2026 13F

---

<p align="center">
  <sub>Made with ❤️ by <a href="https://github.com/13F">13F</a> — 用 Aman，交个朋友。</sub>
</p>
