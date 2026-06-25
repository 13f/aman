# Aman Desktop — "更酷" 设计方向

> 状态：创意池，按优先级逐步实现
> 讨论日期：2026-06-24
> 当前 Desktop 技术栈：Tauri v2 + Svelte 5 + Plain CSS (frosted glass design system)

---

## 路线图总览

```
Phase 1 ─── 氛围感升级 ─── 打开 app 第一眼就不同
  ├── 1. 动态环境光 / Aurora 背景     ✅ 2026-06-24
  ├── 2. 粒子系统 (sub particle field) ✅ 2026-06-25
  └── 3. 深度感 / 视差玻璃层

Phase 2 ─── 动效语言 ─── 让 agent 感觉 "活着"
  ├── 4. Agent 心跳 / 呼吸 / 涟漪     ✅ 2026-06-24
  ├── 5. 页面转场动画                  ✅ 2026-06-25
  └── 6. Agent "思考空间" 中间态

Phase 3 ─── 交互升级 ─── 实用 + 酷
  ├── 12. 命令面板 ⌘K                  🥉
  ├── 13. 拖拽式 Workflow Builder
  └── 14. 多 Agent 圆桌视图

Phase 4 ─── 排版与视觉深度
  ├── 10. 变量字体 + 排版层级
  ├── 11. 自定义光标
  └── 12. 代码块展示升级 (语法高亮 + diff)

Phase 5 ─── Agent 可视化 ─── 最具区分度
  ├── 13. Agent 脑图 / Cognitive State Map  🟡 L1 done
  └── 14. Agent 角色卡片 (3D tilt + 姿态动画)

Phase 6 ─── 感官扩展
  ├── 15. 微妙的音效系统
  └── 16. 多主题系统 (Terminal / Paper / Midnight)
```

---

## Phase 1：氛围感升级（Atmospheric）

### 1. 动态环境光 / Aurora 背景 ✅ 2026-06-24

**已实现**。在 `AuroraBackground.svelte` 中实现，通过 `ui.style = "aurora"` 配置开关。

**实现细节**：
- 方案 A + B 混合：默认 agent-agnostic 缓慢流动，同时跟随 gateway 聚合 agent 状态（active/error 数量）做色调偏移
- `<canvas>` 2D + simplex noise 驱动极光渐变，`requestAnimationFrame` 渲染
- `App.svelte` 中通过 `{#if uiStyle === "aurora"}` 控制渲染
- 用 accumulated phase 防止状态切换时跳变（commit `1a18748`）

**原设计方案**（供参考）：

**⚠️ 多 Agent 约束**：Aman 是多 agent 系统，全局 UI 不适合绑定单个 agent 的状态。改为以下方案：

**方案 A：Gateway 级别聚合（推荐）**

不跟单个 agent，而是跟整个 gateway 的聚合状态：

| 聚合状态 | 色调 | 感觉 |
|---|---|---|
| 全部 idle / sleep | 深蓝紫，极缓慢流动 | 安静、休眠 |
| ≥1 个 agent active | 微妙暖色调偏移 | 有生命在活动 |
| ≥3 个 agent 同时 active | 更丰富的色彩流动 | 繁忙、热闹 |
| 有 agent 报错 | 极 subtle 的暖色脉冲（不是红色警告） | 需要关注但不刺眼 |

聚合逻辑在 desktop 端做：监听 `agent_states:updated` SSE，统计 active/error 数量，驱动颜色插值。

**方案 B：完全 Agent-Agnostic（最安全）**

不跟任何状态绑定，但**仍然是动态的** —— 缓慢流动的极光/光晕，像 Apple Music  lyrics 背景或 Spotify Canvas 那样，纯粹的氛围感，不求语义。色调跟随当前主题（Midnight → 蓝紫，Paper → 暖琥珀），用 simplex noise 驱动持续的、不可预测但平滑的色彩流动。打开 app 它就一直在 "呼吸"，无需任何事件驱动。

**方案 C：跟随当前选中的 Agent**

Sidebar 里选中的 agent（`agentId` prop 已在 `ActivityStateWidget` 中可用）决定背景色调。切换 agent 时背景平滑过渡。这样每个 agent 有自己的 "气场"，但不影响其他视图。

**推荐**：先做方案 B（最简单，零风险），后续可加入方案 A 的聚合逻辑。

**实现思路**：
- 在 `App.svelte` 最底层加一个 `<canvas>` 或 SVG filter 层
- 用 `requestAnimationFrame` 驱动 simplex noise 渐变（[simplex-noise](https://www.npmjs.com/package/simplex-noise) 包，gzip < 2KB）
- 颜色过渡用 `transition: background 1.5s ease`

**技术选型**：
- 方案 A：CSS `@property` + 多个 radial gradient 叠加，JS 更新 custom properties → GPU 加速，最轻量
- 方案 B：`<canvas>` 2D + simplex noise → 更灵活，适合后续加粒子
- 推荐：先用方案 A 快速出效果，后续升级到方案 B 以支持粒子

**适用范围**：全局背景，所有页面共享。

---

### 2. 粒子系统（Sub Particle Field） ✅ 2026-06-25

**已实现**。在 `ParticleField.svelte` 中实现，叠加在 Aurora canvas 之上（`z-index: 1`）。

**实现细节**：
- **Canvas 2D**：独立 `<canvas>`，全分辨率渲染（不下采样），`pointer-events: none`
- **粒子数量**：idle 时 30 个，≥3 agent active 时平滑增加到 50 个
- **颜色**：冷白蓝（idle）→ 暖白（active），跟随 activity 插值。配合 `shadowBlur` 做柔光效果
- **运动**：随机漂移 + 阻尼 + 速度钳制（idle 0.35px/frame, active 0.65px/frame）。边界 wrap 循环
- **引力效果**：暴露 `attractTo(x, y)` 方法，新消息到达时可调用，40% 粒子短暂向目标区域汇聚后散开
- **Activity 驱动**：监听 `agent_states:updated` SSE，复用 Aurora 相同的聚合逻辑
- **`prefers-reduced-motion`**：检测 OS 偏好，启用时停止动画循环，渲染静态帧
- **开关**：通过 `ui.style === "aurora"` 控制（与 Aurora 同生命周期）

**注意事项**：必须极其 subtle。如果用户注意到了粒子，说明太多了。好的粒子设计是 "关掉才发现少了什么"。

---

### 3. 深度感 / 视差玻璃层

**现状**：单层毛玻璃，所有 `backdrop-filter: blur(28px)` 同级。

**改进**：三层深度：

```
┌──────────────────────────────────────┐
│  L0: 动态背景 (aurora + 粒子)         │  blur: 0,  最远层
│  ┌──────────────────────────────┐    │
│  │ L1: 大面积玻璃 (sidebar, 主区) │   │  blur: 28px, 中层
│  │  ┌──────────────────────┐    │    │
│  │  │ L2: 卡片/按钮/input   │    │    │  blur: 12px, 近层
│  │  │  ┌──────────────┐    │    │    │
│  │  │  │ L3: modal/toast│   │    │    │  blur: 44px, 最前层
│  │  │  └──────────────┘    │    │    │
│  │  └──────────────────────┘    │    │
│  └──────────────────────────────┘    │
└──────────────────────────────────────┘
```

**CSS Token 调整**：
```css
--glass-blur-far: 8px;       /* L2: 卡片 */
--glass-blur-mid: 28px;      /* L1: sidebar, main */
--glass-blur-near: 44px;     /* L3: modal, toast */
--glass-blur-none: 0px;      /* L0: background */

--glass-opacity-far: 0.38;
--glass-opacity-mid: 0.58;
--glass-opacity-near: 0.78;
```

---

## Phase 2：动效语言升级（Motion Design）

### 4. Agent 心跳 / 呼吸 / 涟漪 ✅ 2026-06-24

**已实现**（commit `07d06f2`）。在 `desktop/src/pages/IdleRing.svelte` 中实现。

**实现细节**：

| 动效 | 类型 | 触发 | 实现 |
|---|---|---|---|
| **breathing** | continuous | `mode="idle"/"wakeup"` | CSS `@keyframes breathe` — 8s scale pulse (1.0 ↔ 1.025) |
| **ripple** | continuous | `mode="reflection"/"processing"` | `::before` + `::after` 伪元素，border-only 无 fill，从外环外侧向外扩散 (2.8s, 两道交错) |
| **pulse** | one-shot | `trigger="pulse"` | `.ring-svg` drop-shadow 短暂增强 (0.5s) |
| **shake** | one-shot | `trigger="shake"` | 水平衰减抖动 (0.35s) |
| **wakeup** | one-shot | `trigger="wakeup"` | "睁眼" scale(0.88→1.06→0.98→1.0) (0.7s) |

**关键设计决策**：
- **不遮盖双环**：涟漪伪元素 `inset: -20%` + `scale(0.65)` 起，border 始终在 SVG 外环外侧。SVG `z-index: 1`，中心 `z-index: 2`，涟漪在下层
- **Continuous vs one-shot**：continuous 由 `mode` 驱动（idle→breathing，reflection→ripple）；one-shot 由 `trigger` prop 驱动，播放后自动清除
- **one-shot 覆盖 continuous**：`effectClass` derived 值在 `activeEffect` 非 null 时返回 one-shot class，播放完毕后回退到 continuous
- **`trigger` 而非 `effect` prop 名**：避免与 Svelte 5 的 `$effect` rune 冲突
- **尊重 `prefers-reduced-motion`**：所有动画在 reduced-motion 下禁用
- **`active={false}` 时所有动效关闭**
- **现有调用方（ActivityStateWidget、Home）无需修改** — `mode` 已传入，continuous 效果自动生效

**原设计**（供参考）：

---

### 5. 页面转场动画 ✅ 2026-06-25

**已实现**。在 `App.svelte` 中用 `{#key currentPage}` + `fly` transition 实现。

**实现细节**：
- **方向感知**：`navigateTo()` 维护 `navHistory` 栈检测前进/后退。前进（深入导航）新页面从右侧滑入（`x: 80`），后退（返回）从左侧滑入（`x: -80`）
- **slide+fade**：使用 Svelte `fly` transition（同时处理 x 位移 + opacity），进入 250ms `cubicOut`，退出 200ms `cubicIn`
- **布局**：`.main` 设为 `position: relative; overflow: hidden` 作为过渡容器，`.page-wrapper` 使用 `position: absolute; inset: 0; overflow-y: auto` 承载页面滚动
- **`prefers-reduced-motion`**：检测 OS 偏好，启用时 `duration: 0` 禁用过渡
- **iframe 页面**：team/plugin 页面的 `{#key teamPageVersion}` 内层嵌套在外层 `{#key currentPage}` 中，同页面重复导航仍能正确重建 iframe

**Shared Element Transition**（进阶，未实现）：
- 从 Home 页的 agent card 点击进入 Chat 时，agent 的头像环形从它在 card 上的位置 "飞" 到 chat 顶栏
- 需要 FLIP 动画技术（First, Last, Invert, Play）
- Svelte 的 `crossfade` 或 `flip` 动画可以直接用

---

### 6. Agent "思考空间" 中间态

**现状**：agent 思考 → 回复直接出现在聊天流中。

**改进**：
- Agent 开始思考时，聊天区域出现一个朦胧的 "思考空间" 面板（半透明，浮在聊天流上方）
- 显示 ReAct loop 步骤：Observation → Thought → Action → Observation → ...
- 每步用打字机效果逐字显现
- Agent 回复完成后，"思考空间" 淡出，最终回复落入聊天流

**数据来源**：Gateway 已有的 SSE 事件 —— `tool:dispatched`, `llm_reply_ready`, `agent:reply_stream_start` 等。在 Chat.svelte 中已经有这些事件的处理。

> **与第 13 条的关系**：本条 "思考空间" 的 ReAct 步骤展示需求，已演化为第 13 条的 **Level 2 完整脑图**（Chat 侧面板）。两者共享数据管道（`agent:cognitive_state` SSE），本条侧重于 "思考中" 的临时浮层体验，第 13 条侧重于持久的图可视化。实现时统一考虑。

---

## Phase 3：交互升级（Interaction Patterns）

### 7. 命令面板 ⌘K 🥉

**效果**：类似 Linear / Raycast 的命令面板，毛玻璃 + 快速匹配。

```
┌─────────────────────────────────────────────┐
│  ⌘K                                         │
│  ┌──────────────────────────────────────┐   │
│  │  ▸ ask Claude to review the latest PR│   │
│  │  ▸ switch to Home                    │   │
│  │  ▸ agent: Claude (active)            │   │
│  │  ▸ reload plugins                    │   │
│  │  ▸ open workflow board               │   │
│  └──────────────────────────────────────┘   │
│  Actions   Agents   Pages   Commands         │
└─────────────────────────────────────────────┘
```

**功能**：
- 搜索 agent、切换页面、执行快捷操作
- 支持自然语言输入："ask claude to review the latest PR"
- 毛玻璃背景 + 快速模糊匹配 + 键盘导航
- 分组显示：Actions / Agents / Pages / Commands

**实现**：
- 独立的 `CommandPalette.svelte` 组件
- 全局键盘监听（⌘K / Ctrl+K）
- 注册机制：每个页面可以注册自己的命令
- 模糊搜索用简单的 substring + 打分（不需要 Fuse.js，自己写 30 行足够）

---

### 8. 拖拽式 Workflow Builder

**现状**：Workflow Board 是只读的查看器。

**改进**：可视化编排 ——
- 节点代表 skill / tool / agent
- 连线代表数据流
- 从面板拖入新节点，拖拽连线
- 实时预览 workflow 定义（生成 YAML）
- 保存后可直接运行

**技术选型**：Svelte flow 库（如 `@xyflow/svelte`）或纯手写 SVG drag。

---

### 9. 多 Agent "圆桌" 视图

**效果**：多个 agent 环形排列，像虚拟董事会会议。

```
         [Agent A]
    [Agent F]   [Agent B]
        \       /
    [Agent E]──┼──[Agent C]    ← 当前发言高亮 + 放大
        /       \
    [Agent D]   (observer)
```

- 当前"发言"（正在输出回复）的 agent 高亮 + 放大
- 对话内容在圆桌中央流动
- 其他 agent 显示微小的 "倾听" 动画（微微点头？）
- 点击 agent 可以静音/邀请发言

**适用场景**：多 agent 协作任务、brainstorming session。

---

## Phase 4：排版与视觉深度 ✅ 已完成

### 10. 变量字体 + 排版层级 ✅

**替换当前系统字体栈**：

```css
/* 当前 */
--font-ui: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, ...;
--font-mono: "SF Mono", "Fira Code", "Cascadia Code", ...;

/* 新方案 ✅ */
--font-ui: "Inter Variable", "Inter", -apple-system, ...;
--font-mono: "JetBrains Mono Variable", "JetBrains Mono", "Fira Code", ...;
```

**排版改进**：
- 标题用更窄的 tracking（`letter-spacing: -0.02em`）✅
- 关键数字（metrics 面板的延迟、token 数）用 tabular numbers (`font-variant-numeric: tabular-nums`) ✅
- Agent 回复的正文适当增大行高（1.7 → 更好的可读性）✅
- 代码块用 JetBrains Mono，连字（ligatures）开启 ✅

**字体加载**：从 Google Fonts 引入 Inter Variable + JetBrains Mono Variable，`font-display: swap`。✅

**实现文件**：`desktop/index.html`（preconnect + font link）、`desktop/src/app.css`（--font-ui, --font-mono, --line-height-relaxed, .tabular-nums）、`desktop/src/pages/Chat.svelte`（markdown-body line-height, heading tracking, code ligatures）

---

### 11. 自定义光标 ✅

- Agent "思考中"：光标变成小脉冲环 ✅
- 正常状态：细线光标 ✅
- 拖拽中：grab/grabbing 自定义光标 ✅

**与现有 emotions 系统联动**：Aman 已内置 emotions 系统（未设置 emotions 图片的 agent 会回退到 emoji）。光标可以跟 agent 当前 emotion 状态联动 —— 比如 agent 处于 "excited" 情绪时光标带金色微光，"reflective" 时变柔和蓝紫。emotion 数据已经通过 SSE 推送，直接订阅即可。✅

小众但酷。CSS `cursor: url()` 即可。

**实现文件**：`desktop/public/cursors/*.svg`（6 个光标 SVG）、`desktop/src/lib/cursor-store.ts`（光标状态管理 + emotion 映射）、`desktop/src/app.css`（body.cursor-* CSS 类）、`desktop/src/pages/Chat.svelte`（$effect 根据 isProcessing 自动切换）

---

### 12. 代码块展示升级 ✅

**现状**：`marked` 渲染，无语法高亮。

**改进**：
- 引入 highlight.js 做语法高亮（更轻量，支持 13 种常用语言）✅
- 代码块顶部标题栏：语言标签 + 复制按钮（hover 显示）✅
- Diff 视图：agent 建议的代码变更用 `+/- ` 绿色/红色背景展示 ✅
- 代码块最大高度（400px）+ 内部滚动 + 底部渐变 fade-out ✅

**技术**：选用 `highlight.js`（比 Shiki 更轻量，按需注册语言）。主题使用 `github-dark`，背景覆盖为透明以适配 glass aesthetic。

**实现文件**：`desktop/src/lib/markdown.ts`（renderMarkdown + postProcessCodeBlocks + diff 行高亮）、`desktop/src/pages/Chat.svelte`（事件委托 copy handler + updateCodeBlockFades + CSS）、`desktop/package.json`（highlight.js 依赖）

---

## Phase 5：Agent 可视化（最区分度）

### 13. Agent 脑图 / Cognitive State Map ⭐

**最具区分度的功能**。实时可视化 agent 的认知过程。

**⚠️ 多 Agent 约束**：Aman 是多 agent 系统，多个 agent 可能同时在执行 ReAct loop。脑图必须是 **per-agent** 的——每个 agent 独立展示自己的认知状态。采用**两级设计**：

---

#### Level 1：迷你认知指示器（替换 IdleRing）

**核心思路**：agent 做事时不需要显示 idle 双环——直接切换为认知环。

```
agent idle / reflecting          agent processing
         │                              │
         ▼                              ▼
   ┌───────────┐                ┌───────────┐
   │  IdleRing │                │CognitiveRing│
   │  双环      │   ──→ 或 ←──  │  单环       │
   │  breathing │                │  相位弧     │
   │  / ripple  │                │  + 步骤描述  │
   └───────────┘                └───────────┘
```

两者**互斥**，不是叠加。`mode` 决定显示哪个：

| mode | 显示 |
|---|---|
| `idle` / `wakeup` | IdleRing（breathing） |
| `reflection` | IdleRing（ripple） |
| `processing` | **CognitiveRing**（ReAct 相位弧） |

**CognitiveRing 设计**：

```
          ╭──────────╮
         ╱            ╲        ← 单环，四个象限分段着色
        │              │
        │      ●       │       ← 中心保持 agent emoji / 头像
        │              │
         ╲            ╱
          ╰──────────╯
              ↑
        "searching docs…"      ← 环下方一行当前步骤描述
```

- 环被分为 4 段（Observing / Thinking / Acting / Result），当前相位段**亮起**、其余三段暗淡
- 相位切换时，亮段沿环顺时针移动，带 `stroke-dashoffset` 平滑过渡（~0.6s）
- 中心保留现有的 agent emoji / emotion image（agent 身份不变）
- 环下方 `current_step` 文字淡入淡出切换

**相位 → 环上映射**：

| ReAct 相位 | 环上位置 | 色调 | 触发条件 |
|---|---|---|---|
| **Observing** | 0°–90°（右上弧） | 蓝 `#60A5FA` | 用户消息 / tool 结果到达 |
| **Thinking** | 90°–180°（左上弧） | 琥珀 `#F59E0B` | LLM 开始推理 |
| **Acting** | 180°–270°（左下弧） | 青 `#22D3EE` | tool 调用发出 |
| **Result** | 270°–360°（右下弧） | 紫 `#A78BFA` | tool 结果返回 |

> 颜色与现有设计系统的 accent 色保持一致，后续可通过 CSS 变量覆盖。

**实现**（✅ 2026-06-25）：
- `CognitiveRing.svelte` — 新组件，独立的 SVG `<circle>` + `stroke-dasharray`/`stroke-dashoffset`
- 4 段 arc 用 4 个 `<circle>` 各画 1/4 弧（`stroke-dasharray="69.115 207.345"`），当前段 opacity 1.0，其余 0.22
- `IdleRing.svelte` 不修改——由父组件（agent 卡片/ActivityStateWidget）根据 `isActive` 决定渲染 IdleRing 还是 CognitiveRing
- IdleRing ↔ CognitiveRing 切换时做 crossfade（Svelte `fade` transition 300ms）
- `current_step` 文字放在环下方，`font-size: 11px`，`opacity: 0.7`，单行截断
- **数据管道（纯 desktop 端推断，无 gateway 改动）**：
  - `desktop/src/lib/cognitive-state.ts` — 共享模块：`ReactPhase` 类型、`inferReactPhase()` 状态机、`inferStepText()` 步骤文本推导
  - `Home.svelte` — `handleIdleEvent` 扩展，从现有 `event:processed` SSE 事件推断 ReAct 相位
  - `ActivityStateWidget.svelte` — `onEvent` 重构，active 状态下切到 CognitiveRing
- 相位自动流转：`tool:completed` → Result → 1.5s → Observing；`agent:reply_ready` → Result → Idle
- 尊重 `prefers-reduced-motion`，禁用所有 transition 动画
- 替换了 Home.svelte 原有的 `.state-visual` 色圈 + `STATE_ANIM`，清理了不再使用的 CSS

**优势**：
- 状态切换语义清晰：idle 是 idle 的样子，做事是做事的样子
- 不增加 UI 复杂度——替换而非叠加
- agents 列表页可同时看到多个 agent 各自处于哪个 ReAct 相位
- 信息密度低，不分散注意力

---

#### Level 2：完整脑图（Chat 页侧面板）🔵 暂缓

点击某个 agent 进入 Chat 后，在右侧 split view 中展开完整 ReAct 节点图。

```
┌──────────────────────────┬─────────────────────────────┐
│  Chat (left)             │  🧠 Brain Map (right)       │
│                          │                             │
│  User: "review my PR"    │   Obs₁ ──→ Act₁ ──→ Res₁   │
│                          │              │              │
│  Claude: "Let me         │              ↓              │
│  look at the diff..."    │   Obs₂ ──→ Act₂ ──→ Res₂   │
│                          │              │              │
│                          │              ↓              │
│                          │   Obs₃ ──→ Final Reply      │
│                          │                             │
│                          │  ─────────────────────      │
│                          │  Memory: ep[342] sem[89]    │
│                          │  Context: 4.2k / 32k        │
│                          │  Loop: 3 rounds             │
└──────────────────────────┴─────────────────────────────┘
```

- 纵向时间线布局（类似 Git graph），Obs → Act → Res 循环展开
- 每个节点是发光小球 + 类型图标 + 一行摘要
- 当前激活节点脉动，已完成节点变暗
- 连线是流动光线（`stroke-dasharray` + `stroke-dashoffset` animation）
- 底部统计栏：memory 使用、context window、ReAct 轮数、P95 延迟
- **不是静态图 —— 是实时流动的**，新节点出现时自动向下滚动

**实现**：SVG + CSS animation。单个 ReAct loop 通常 < 20 步，SVG 完全够。

---

#### 数据管道

**推荐方案：Gateway 端维护 `CognitiveStateTracker`**

Gateway 内部的 ReAct 引擎已经知道每个 agent 处于哪个阶段。在 gateway 端加一个轻量的状态追踪器：

```rust
// 每个 agent 一个状态机
struct AgentCognitiveState {
    agent_id: String,
    react_phase: ReactPhase,       // Observing | Thinking | Acting | Idle
    current_step: String,          // 当前步骤描述
    loop_count: u32,               // 第几轮 ReAct
    memory_stats: MemoryStats,     // Level 2 用
    context_usage: f64,            // 0.0–1.0，Level 2 用
}
```

通过新的 SSE 事件 `agent:cognitive_state` 推送给 desktop：

```json
{
  "agent_id": "claude",
  "react_phase": "acting",
  "current_step": "searching docs",
  "loop_count": 3,
  "context_usage": 0.42
}
```

**备选方案：Desktop 端推断**

如果 gateway 改动成本高，desktop 端也可以根据现有 SSE 事件做规则推断：

| 收到事件 | 推断相位 |
|---|---|
| `agent:reply_stream_start` | → Thinking |
| `tool:dispatched` | → Acting |
| `tool:completed` | → Observing |
| `llm_reply_ready` | → Result |

但 gateway 端做更可靠（有完整的 ReAct 引擎上下文，不会漏判/误判）。

---

#### 实施顺序

1. **数据管道**：Gateway `CognitiveStateTracker` + `agent:cognitive_state` SSE 事件
2. **Level 1**：IdleRing 相位弧 + `current_step` 描述。先上 agent 卡片（Home 页），再考虑 ActivityStateWidget
3. **Level 2**：Chat 页右侧 `CognitiveMap.svelte` 面板（split view），含完整节点图 + 统计栏

**组件结构**：
```
CognitiveMap.svelte          ← Level 2 主组件，订阅 SSE + 维护节点/边状态
├── CogNode.svelte           ← 单节点（发光球 + 图标 + 摘要 + 脉冲）
├── CogEdge.svelte           ← 连线（流动光线）
└── CogStats.svelte          ← 底部统计栏

IdleRing.svelte              ← 现有组件，新增：
  └── ReAct 相位弧覆盖层     ← Level 1，mode="processing" 时显示
```

---

### 14. Agent 角色卡片升级 ✅ 2026-06-25

**已实现**（commit 待提交）。在 `Home.svelte` agent 卡片上实现 3D tilt + gloss + 姿态动画。**状态指示**跳过（卡片已有 status dot + label）。

**实现细节**：

**3D Tilt**：
- JS `mousemove` 计算鼠标相对卡片中心的偏移，映射为 ±10° 的 `rotateX`/`rotateY`
- 通过 CSS 自定义属性 `--tilt-x` / `--tilt-y` 驱动 `transform: perspective(800px) rotateX(...) rotateY(...)`
- 鼠标移出时 `cubic-bezier(0.23, 1, 0.32, 1)` 缓动平滑回弹（~0.6s）
- 悬停时叠加 `translateY(-4px)` 保留原有上浮效果
- `transform-style: preserve-3d` 确保 3D 空间正确渲染

**光照效果（Gloss）**：
- `::after` 伪元素 + `radial-gradient`：鼠标位置映射为高光中心（`--gloss-x` / `--gloss-y`）
- 白色半透明渐变（13% → 4% → 0%），悬停时 `opacity` 淡入
- `pointer-events: none` 不阻断点击

**姿态动画（Pose）**：
- `agentPose` 关键帧：6s 循环、8 个停顿点，不规则的 ±3.5px 上下浮动 + ±0.4° 旋转
- 应用到 `.state-emoji` / `.state-emotion-img`（`.avatar-pose` class）
- 通过 `:global(.ring-center)` / `:global(.ring-emotion-img)` 穿透 IdleRing 作用域
- 父选择器 `.agent-avatar-wrap` 携带 Home 的作用域 hash，仅影响 Home 卡片

**可访问性**：
- JS 端检测 `prefers-reduced-motion`：开启时跳过所有倾斜计算
- CSS `@media (prefers-reduced-motion: reduce)`：移除 3D 变换、光泽、姿态动画，回退为原始上浮效果

**未实现**：
- **状态指示**：跳过（卡片已有 status dot + label 展示当前状态，无需重复的活动摘要）

**原设计方案**（供参考）：

---

## Phase 6：感官扩展

### 15. 微妙的音效系统

| 事件 | 音效 | 描述 |
|---|---|---|
| Agent 开始思考 | 轻 "叮" | 类似 Siri 激活音，极 subtle |
| 消息到达 | 不同 agent 不同提示音 | 用户可自定义 |
| 思考完成 | Rising tone | 两个音符上升 |
| 错误发生 | 柔和的降调 | 不刺耳，只是提醒 |
| 后台 ambient | 可选白噪音/雨声/太空舱 | 长时间使用减轻疲劳 |

**实现**：Aman 已适配 OpenPeon，音频播放 infrastructure 已就绪，不需要重复造轮子。在现有 OpenPeon 集成基础上添加短音效触发即可 —— desktop 端只需根据 SSE 事件调用已有的 audio API，无需引入新的音频后端。音频文件用 base64 内嵌或放在 `public/` 目录。

---

### 16. 多主题系统

在当前的 Dark/Light 之上：

| 主题 | 感觉 | 关键特征 |
|---|---|---|
| **Midnight** | 当前暗色主题的极致版 | 更深蓝黑 + 金色 accent + 更高 blur |
| **Terminal** | 怀旧 hacker 风 | 黑底绿字、scan lines、CRT 扫描线效果、像素字体 |
| **Paper** | 精装书阅读体验 | 暖色米白底、衬线字体、纹理背景、低 blur |
| **Neon** | Cyberpunk | 暗紫底、粉色/青色双 accent、霓虹 glow 边框 |
| **Mono** | 极简 | 纯黑白灰，无色彩 accent，只有 typography 区分层级 |

**实现**：每个主题是一个 CSS 变量文件（`themes/midnight.css` 等），在 `app.css` 中 `@import`。用户选择保存在 localStorage，`<html data-theme="terminal">` 控制。

---

## 实现建议

### 开发原则

1. **每个 Phase 独立可发布** —— 不要做一半留一半
2. **Feature flag 控制** —— 新动效/主题用 `localStorage` key 开关，方便测试和回退
3. **优先性能** —— 所有动画用 `transform` + `opacity`（GPU 合成），避免 `width/height/top/left` 动画（触发 layout）
4. **尊重 `prefers-reduced-motion`** —— 动效系统必须有 reduced-motion fallback
5. **Tauri 原生能力善用** —— vibrancy、原生 menu、notification、音频都走 Tauri API

### 技术依赖增量

| Phase | 新增依赖 | 大小 |
|---|---|---|
| Phase 1 | `simplex-noise` (可选) | ~2KB gzip |
| Phase 2 | 无 | — |
| Phase 3 | 无（⌘K 手写） | — |
| Phase 4 | 变量字体文件 (Inter + JetBrains Mono) | ~400KB 自托管 |
| Phase 4 | `shiki` 或 `highlight.js` | ~30KB |
| Phase 5 | 无 | — |
| Phase 6 | 音频文件 | ~200KB |

保守估计总增量 < 1MB，完全可以接受。

---

## 参考灵感

- **Linear** — 命令面板、微交互、毛玻璃
- **Raycast** — 命令面板流畅度
- **Arc Browser** — 侧边栏设计、空间管理
- **Apple Vision OS** — 玻璃材质深度层级
- **Notion AI** — AI 写作辅助的动效
- **Cursor** — AI 编码助手的 inline 交互
- **Spotify** — 动态背景色提取
- **vs code** — 命令面板 + 语法高亮
- **Game UI** (Destiny, Cyberpunk 2077) — HUD 元素、扫描线、科幻感

---

## Changelog

- 2026-06-24：初始创意池，6 Phase 16 个方向
- 2026-06-24：Phase 1 第 1 条（Aurora 背景）实现。Canvas 2D + simplex noise，方案 A+B 混合，通过 `ui.style = "aurora"` 配置开关。
- 2026-06-24：Phase 2 第 4 条（Agent 心跳/呼吸/涟漪）实现。IdleRing 新增 5 个动效（breathing/ripple continuous + pulse/shake/wakeup one-shot），纯 CSS 实现，不遮盖双环。
- 2026-06-25：Phase 2 第 5 条（页面转场动画）实现。`App.svelte` 用 `{#key currentPage}` + `fly` transition，方向感知（前进右滑、后退左滑），250ms/200ms，尊重 `prefers-reduced-motion`。
- 2026-06-25：Phase 1 第 2 条（粒子系统）实现。`ParticleField.svelte`，30-50 粒子，柔光漂浮，activity 驱动密度/速度/色温，带 attractor API 供消息汇聚效果。
- 2026-06-25：Phase 5 第 14 条（Agent 角色卡片升级）实现。3D tilt（±10° 透视旋转 + 弹性回弹），光照 gloss 叠加层（radial-gradient 跟随鼠标），姿态动画（agentPose 关键帧 6s 循环渗透 IdleRing 中心内容）。跳过状态指示（卡片已有 status dot）。尊重 `prefers-reduced-motion`。
- 2026-06-25：Phase 5 第 13 条（Agent 脑图 / Cognitive State Map）方案讨论定稿。确定两级设计：Level 1 迷你认知指示器（IdleRing 替换为 CognitiveRing 单环），Level 2 完整脑图（Chat 侧面板 split view，SVG 纵向流图）。数据管道推荐 Gateway 端 `CognitiveStateTracker` + `agent:cognitive_state` SSE 事件。第 6 条"思考空间"与本条 Level 2 统一考虑。修正 roadmap 中 Phase 4/5 编号。
- 2026-06-25：Phase 5 第 13 条 **Level 1 实现**。新增 `CognitiveRing.svelte`（SVG 单环 4 段分色 ReAct 相位指示 + 步骤文字），`cognitive-state.ts`（相位状态机 + 步骤文本推导）。`Home.svelte` agent 卡片 .state-visual 替换为 CognitiveRing，`ActivityStateWidget.svelte` 按 isActive 切换 IdleRing/CognitiveRing。纯 desktop 端事件推断，无 gateway 改动。Level 2 暂缓。
