# Aman Desktop — "更酷" 设计方向

> 状态：创意池，按优先级逐步实现
> 讨论日期：2026-06-24
> 当前 Desktop 技术栈：Tauri v2 + Svelte 5 + Plain CSS (frosted glass design system)

---

## 路线图总览

```
Phase 1 ─── 氛围感升级 ─── 打开 app 第一眼就不同
  ├── 1. 动态环境光 / Aurora 背景     🥇
  ├── 2. 粒子系统 (sub particle field)
  └── 3. 深度感 / 视差玻璃层

Phase 2 ─── 动效语言 ─── 让 agent 感觉 "活着"
  ├── 4. Agent 心跳 / 呼吸 / 涟漪     🥈
  ├── 5. 页面转场动画
  └── 6. Agent "思考空间" 中间态

Phase 3 ─── 交互升级 ─── 实用 + 酷
  ├── 12. 命令面板 ⌘K                  🥉
  ├── 13. 拖拽式 Workflow Builder
  └── 14. 多 Agent 圆桌视图

Phase 4 ─── 排版与视觉深度
  ├── 7. 变量字体 + 排版层级
  ├── 8. 自定义光标
  └── 9. 代码块展示升级 (语法高亮 + diff)

Phase 5 ─── Agent 可视化 ─── 最具区分度
  ├── 10. Agent 脑图 / Cognitive State Map
  └── 11. Agent 角色卡片 (3D tilt + 姿态动画)

Phase 6 ─── 感官扩展
  ├── 15. 微妙的音效系统
  └── 16. 多主题系统 (Terminal / Paper / Midnight)
```

---

## Phase 1：氛围感升级（Atmospheric）

### 1. 动态环境光 / Aurora 背景 🥇

**效果**：缓慢流动的极光渐变充满整个窗口背景，色调随 agent 认知状态变化。

| Agent 状态 | 色调 | 感觉 |
|---|---|---|
| idle | 深蓝紫 | 安静、等待 |
| thinking | 金色脉冲 | 活跃思考 |
| acting | 翠绿流动 | 正在执行 |
| error | 暗红波纹 | 需要关注 |
| reflecting | 柔和的粉紫 | 内省 |

**实现思路**：
- 在 `App.svelte` 最底层加一个 `<canvas>` 或 SVG filter 层
- 用 `requestAnimationFrame` 驱动 simplex noise 渐变（可以用 [simplex-noise](https://www.npmjs.com/package/simplex-noise) 包，gzip < 2KB）
- Gateway 通过 SSE 推送 agent 状态（已有 `agent:state_changed` 或类似事件？需要确认）
- 颜色之间用 `transition: background 2s ease` 做平滑切换

**技术选型**：
- 方案 A：CSS `@property` + 多个 radial gradient 叠加，由 JS 更新 custom properties → GPU 加速，最轻量
- 方案 B：`<canvas>` 2D + simplex noise → 更灵活，适合后续加粒子
- 推荐：先用方案 A 快速出效果，Phase 2 升级到方案 B 以支持粒子

**适用范围**：全局背景，所有页面共享。

---

### 2. 粒子系统（Sub Particle Field）

**效果**：极 subtle 的光点漂浮在玻璃层后面，不是花哨的 particle.js 圣诞树。

- 稀疏（~30-50 个粒子在 1200x800 视口内）
- 缓慢漂浮（每帧移动 0.2-0.5px）
- 颜色继承 aurora 背景的 accent 色调
- Agent 活跃时粒子密度/速度微微增加
- 新消息到达时，粒子短暂向消息区域汇聚再散开

**实现**：
- 在 aurora canvas 上叠加粒子层
- 粒子状态：`{ x, y, vx, vy, size, opacity, targetX?, targetY? }`
- 引力效应：每个粒子有 `targetX/targetY`，有消息时设为目标区域，到达后清除

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

### 4. Agent 心跳 / 呼吸 / 涟漪 🥈

**在现有的 IdleRing 组件上扩展**：

| 动效 | 触发条件 | 视觉表现 |
|---|---|---|
| **呼吸** | agent idle | ring 以 4-7-8 节奏缓慢脉动 (scale 1.0 → 1.03 → 1.0) |
| **涟漪** | agent 思考中 | ring 向外发出细微波纹，每 2-3 秒一圈 |
| **脉冲** | 任务完成 | 一道光沿 ring 快速旋转一圈 + 短暂放大 |
| **震动** | 错误发生 | ring 不规则抖动 200ms + 短暂变红 |
| **唤醒** | 从 idle 切换到 active | ring 快速扩大再收缩 (类似 "睁眼") |

**实现**：
- 在 `IdleRing.svelte` 里用 Svelte 的 `tweened` / `spring` store 做动画值
- SVG `stroke-dasharray` + `stroke-dashoffset` 做 "光沿线旋转" 效果
- 心跳用 CSS `animation` + `transform: scale()` + `ease-in-out`

---

### 5. 页面转场动画

**现状**：页面瞬间切换，没有过渡。

**改进**：
- 用 Svelte 的 `{#key pageKey}` + `transition:fly` 做 slide+fade 组合
- 方向感：深入导航（Home → Chat → Settings）向右滑入，返回向左滑出
- 时长：200-250ms，ease-out （太快没感觉，太慢拖沓）

**Shared Element Transition**（进阶）：
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

## Phase 4：排版与视觉深度

### 10. 变量字体 + 排版层级

**替换当前系统字体栈**：

```css
/* 当前 */
--font-ui: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, ...;
--font-mono: "SF Mono", "Fira Code", "Cascadia Code", ...;

/* 新方案 */
--font-ui: "Inter Variable", -apple-system, ...;        /* 或 Geist */
--font-mono: "JetBrains Mono Variable", "Fira Code", ...;
```

**排版改进**：
- 标题用更窄的 tracking（`letter-spacing: -0.02em`）
- 关键数字（metrics 面板的延迟、token 数）用 tabular numbers (`font-variant-numeric: tabular-nums`)
- Agent 回复的正文适当增大行高（1.7 → 更好的可读性）
- 代码块用 JetBrains Mono，连字（ligatures）开启

**字体加载**：从 Google Fonts 或自托管，`@font-face` 引入。需要确保不阻塞首屏渲染（`font-display: swap`）。

---

### 11. 自定义光标

- Agent "思考中"：光标变成小脉冲环
- 正常状态：细线光标
- 拖拽中：grab/grabbing 自定义光标

**与现有 emotions 系统联动**：Aman 已内置 emotions 系统（未设置 emotions 图片的 agent 会回退到 emoji）。光标可以跟 agent 当前 emotion 状态联动 —— 比如 agent 处于 "excited" 情绪时光标带金色微光，"reflective" 时变柔和蓝紫。emotion 数据已经通过 SSE 推送，直接订阅即可。

小众但酷。CSS `cursor: url()` 即可。

---

### 12. 代码块展示升级

**现状**：`marked` 渲染，无语法高亮。

**改进**：
- 引入 Shiki 做语法高亮（类似 VS Code 的渲染质量）
- 代码块顶部标题栏：语言标签 + 复制按钮（hover 显示）
- Diff 视图：agent 建议的代码变更用 `+/- ` 绿色/红色背景展示
- 代码块最大高度 + 内部滚动 + 底部渐变 fade-out

**技术**：Shiki 可以在 build time 生成 CSS，不需要运行时 JS。或者用 `highlight.js`（更轻量）。

---

## Phase 5：Agent 可视化（最区分度）

### 13. Agent 脑图 / Cognitive State Map ⭐

**最具区分度的功能**。实时可视化 agent 的认知过程。

```
┌──────────────────────────────────────────────────┐
│  🧠 Claude · active · ReAct loop                 │
│                                                  │
│   ┌──────────┐      ┌──────────┐                │
│   │Observation│─────→│ Thought  │                │
│   │ "user     │      │ "I need  │                │
│   │  asked..."│      │  to..."  │                │
│   └──────────┘      └────┬─────┘                │
│        ↑                 │                      │
│        │           ┌─────▼─────┐                │
│   ┌────┴─────┐    │ Decision   │                │
│   │ Result   │    │ "search"   │                │
│   │ "found   │    └─────┬─────┘                │
│   │  3 docs" │          │                      │
│   └──────────┘    ┌─────▼─────┐                │
│        ↑          │ Tool Call │                │
│        └──────────│ "search"  │                │
│                   └───────────┘                │
│                                                  │
│  Memory: episodic[342]  semantic[89]             │
│  Context: ████████░░  4.2k / 32k tokens         │
│  Current latency: P95 420ms                      │
└──────────────────────────────────────────────────┘
```

- 每个节点是发光小球，连线是流动光线
- 当前激活节点脉动，已完成节点变暗
- 连线流动方向表示数据流向
- **不是静态图 —— 是实时流动的**

**数据来源**：Gateway SSE 事件已经包含足够的信息（`tool:dispatched`, `tool:completed`, `llm_reply_ready` 等）。Chat.svelte 中已经在处理这些事件。

**实现**：SVG + CSS animation，或 `<canvas>` 2D。先用 SVG 快速出原型。

---

### 14. Agent 角色卡片升级

**Home 页 agent 卡片改成类似游戏角色选择画面**：

- **姿态动画**：每个 agent 有一个微动的 SVG 插画或 emoji 序列帧（不是静态头像）
- **3D tilt**：Hover 时卡片倾斜，跟随鼠标位置（CSS `perspective` + `rotateX/Y`）
- **光照效果**：tilt 时卡片表面的 gloss/高光跟随鼠标
- **状态指示**：卡片上有微小的活动摘要（"最后一次活跃：3 分钟前 · 完成了 2 个任务"）

**3D tilt 实现**：
```css
.card {
  transform: perspective(800px) rotateX(var(--tilt-x)) rotateY(var(--tilt-y));
  transition: transform 0.1s ease-out;
}
```
JS 监听 `mousemove` 计算 `--tilt-x` / `--tilt-y`。

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
