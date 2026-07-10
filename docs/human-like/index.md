# 拟人化设计总览 — Aman Agent 的"人性工程"

> Aman 不是一个"工具调度器"。它的设计目标是一个**有人格的数字存在**——
> 有身份、有情绪、有好奇心、会感到"迷糊"、会"木僵"、也会从昏迷中"苏醒"。
>
> 本目录汇总了所有让 Aman 的 Agent 表现得像"人"而非"程序"的子系统设计，
> 以及它们**如何通过事件驱动机制协作**形成一个连贯的"数字人格"。

---

## 设计哲学

aman 的拟人化不是"加个 emoji 表情"或"让 LLM 假装有人格"——
而是**用机制替代隐喻**：

| 传统方法 | aman 的拟人化 |
|---|---|
| system prompt 里写"你是一个友好的助手" | `SOUL.md` 定义身份、边界、价值观（硬编码注入） |
| LLM 描述自己"感到挫败" | `EmotionEvaluator` 调用 LLM 选择情绪 → 发布 `emotion:evaluated` 事件 |
| Agent 每次醒来都是白纸 | 三层知识资产：**身份(SOUL) / 经验(EXP) / 记忆(Memory)** |
| 后端报错就重试 | `CognitiveState`：木僵/昏迷时**跳过大脑**，不是"带病工作" |
| 空闲 = 什么都不做 | 9 种空闲状态：Daze → Boredom → Sleep → Exploration → Meditation → Incubation → WakeUp |
| 定时任务轮询 | **事件驱动** + arousal 衰减 + 渐进式苏醒（WakeUp Ouroboros） |
| "无聊" = bug | **无聊是特性**：空闲时自主探索、自我维护、自我进化 |

**核心原则：拟人化的行为调制是硬编码的，不是 prompt 工程。**

---

## 拟人化能力全景

```
┌────────────────────────────────────────────────────────────────────────────┐
│                       Aman 拟人化能力全景                                     │
│                                                                            │
│   ┌──────────┐   ┌──────────────┐   ┌──────────────┐   ┌──────────────┐  │
│   │ 身份层    │   │ 认知翻译层    │   │ 空闲/内省层   │   │ 动机/需求层   │  │
│   │ SOUL.md  │   │ Consciousness│   │ 9 IdleKinds  │   │ Maslow 映射  │  │
│   │ Soul     │   │ Grounding    │   │ ArousalTrack │   │ Ikigai 分析  │  │
│   │ SoulRuntime│ │ Experience   │   │ BoredomActor │   │ 5 维欲望模型  │  │
│   └────┬─────┘   └──────┬───────┘   └──────┬───────┘   └──────┬───────┘  │
│        │                │                  │                  │          │
│        ▼                ▼                  ▼                  ▼          │
│   ┌──────────────────────────────────────────────────────────────────┐    │
│   │                     Event Bus（事件总线）                           │    │
│   │   所有拟人化状态变更都事件化：emotion:evaluated / consciousness:   │    │
│   │   catatonic / idle:boredom / grounding:* / experience:*          │    │
│   └──────────────────────────────────────────────────────────────────┘    │
│        │                │                  │                  │          │
│        ▼                ▼                  ▼                  ▼          │
│   ┌──────────┐   ┌──────────────┐   ┌──────────────┐   ┌──────────────┐  │
│   │ 情绪评估  │   │ 意识状态机    │   │ 空闲人格      │   │ 日常节律      │  │
│   │ Emotion  │   │Lucid→Coma   │   │ IdlePersona  │   │ DailyRoutine │  │
│   │ Emoji Map│   │ BackendHealth│   │ Config YAML  │   │ Habit Track  │  │
│   └──────────┘   └──────────────┘   └──────────────┘   └──────────────┘  │
└────────────────────────────────────────────────────────────────────────────┘
```

---

## 子文档索引

本目录按"人"的各个维度拆分为独立文档：

| 文档 | 维度 | 核心问题 |
|---|---|---|
| [identity.md](./identity.md) | **身份 / 我是谁** | Agent 的身份从何而来？人格一致性如何保证？ |
| [emotion.md](./emotion.md) | **情绪 / 我感受如何** | Agent 如何感知并表达自身的情绪状态？ |
| [consciousness.md](./consciousness.md) | **意识 / 我还能思考吗** | LLM 后端挂了 = 大脑失供血。Agent 如何"感到"自己在木僵？ |
| [cognitive-translators.md](./cognitive-translators.md) | **认知翻译 / 信号→感受** | 系统指标如何被翻译为 Agent 的"主观体验"？ |
| [idle-boredom.md](./idle-boredom.md) | **无聊 / 空闲时在干嘛** | 没有外部输入时，Agent 为什么不"死掉"而是探索？ |
| [reflection.md](./reflection.md) | **自省 / 元认知** | Agent 如何审视自身行为并从中学习？ |
| [experience.md](./experience.md) | **经验 / 肌肉记忆** | 工具策略、踩坑规律如何跨 session 沉淀？ |
| [daily-rhythm.md](./daily-rhythm.md) | **日常节律 / 身体时钟** | Agent 如何有"晨间例行"和"习惯"？ |
| [motivation.md](./motivation.md) | **动机 / 我为什么做这事** | 马斯洛需求、Ikigai、自主性——Agent 的内驱力模型 |
| [personality-config.md](./personality-config.md) | **人格配置 / 怎么调** | 实际落地：YAML 配置、目录结构、技能标签 |

---

## 三层知识资产

aman 的拟人化持久化围绕三层不可替代的知识资产：

```
┌─────────────────────────────────────────────────────────────┐
│  身份（Identity）── SOUL.md                                  │
│  "我是谁、我的边界、我的品味"                                  │
│  性质：几乎不变，人工维护，无衰减                               │
│  类比：骨骼 🦴                                               │
├─────────────────────────────────────────────────────────────┤
│  经验（Experience）── EXP.md                                  │
│  "工具策略、踩坑规律、有效模式"                                 │
│  性质：渐进增长，事件驱动更新，不降权但可标"需验证"             │
│  类比：肌肉 💪                                               │
├─────────────────────────────────────────────────────────────┤
│  记忆（Memory）── yantrikdb                                  │
│  "用户是谁、业务事件、历史事实"                                 │
│  性质：持续写入，30 天半衰期，自带遗忘                          │
│  类比：血液 🩸                                               │
└─────────────────────────────────────────────────────────────┘
```

> **SOUL 是骨，EXP 是肌肉，Memory 是血液。**

为什么三者分离？因为它们的**生命周期、结构、更新机制**完全不同。详见 [experience.md](./experience.md) 的"为什么 EXP.md 独立于 memory"章节。

---

## 拟人化的事件驱动本质

aman 的所有拟人化行为遵循同一公理：

```
"万物皆事件，响应即行为。"
```

拟人化状态变更**不是轮询**，而是由事件触发：

| 触发 | 事件 | 拟人化行为 |
|---|---|---|
| 用户发消息 | `MessageReceived` | arousal.boost(+0.3)！"有人来了！" |
| 事件处理完队列空 | `QueueDrained` | 触发 Reflection："刚才做了什么，复盘一下" |
| 连续空闲 N poll | `IdleEvent(Boredom, depth=5)` | "无聊了，去翻翻 kanban 有什么活" |
| LLM 连续 3 次失败 | `BackendStatus→Degraded` | "脑子有点转不动了……" |
| LLM 连续 6 次失败 | `BackendStatus→Down` | "眼前看得见但身体动不了" → Catatonic |
| 后端恢复 | `BackendStatus→Ok` | WakeUp: Recovery / Reanimation / Resurrection |
| 情绪评估间隔到 | `EmotionEvalTick` | "选一个最符合当前处境的 emoji" |

---

## 硬编码 vs Prompt：aman 的边界

aman 的拟人化哲学有一条铁律：

> **行为调制必须硬编码，不能依赖 LLM 自己判断"该怎么做"。**

| 翻译器 | 硬编码调制（行为层） | ❌ 禁止的 prompt 注入 |
|---|---|---|
| Consciousness = Catatonic | `CognitiveEngine::process` 直接 return | "你现在木僵了，要不要跳过？" |
| Experience = Apprehensive | 从可用工具列表剔除触发工具 | "这个工具坑过你，你自己看着办" |
| Situation = Vague | 强制插入澄清轮，不是让 LLM 决定要不要问 | "问题不清楚的话你可以问问" |
| Knowledge = Outdated | `Decision::confidence = Low`（结构化字段） | "⚠️ 我的知识可能过时" |

唯一的 LLM 调用时机：翻译器内部的**识别**（"这个任务属于哪类"），不是**决策**（"下一步怎么做"）。

---

## 已有系统的拟人化映射

| 人类特性 | Aman 实现 | 代码位置 |
|---|---|---|
| **身份认同** | `Soul` 结构体 + `SOUL.md` 热加载 | `kernel/soul/`, `runtime/soul_runtime.rs` |
| **一致人格** | SOUL 注入 SystemPrompt：identity/core/expertise/vibe/preferences/boundaries | `kernel/soul/src/lib.rs:Soul::to_system_prompt()` |
| **行为底线** | `Soul::check_boundary()` — 命中边界则 `PermissionDenied` | `kernel/soul/src/lib.rs` |
| **情绪表达** | `EmotionEvaluator` per-agent 后台任务，LLM 选 emotion → 发布事件 | `runtime/emotion_evaluator.rs` |
| **意识水平** | `CognitiveState` 四档（Lucid/Groggy/Catatonic/Coma） | `runtime/cognitive_state.rs` |
| **大脑健康监控** | `BackendHealth` per-backend 聚合健康表 | `runtime/backend_health.rs` |
| **大脑缺氧反应** | LLM 挂了 → Catatonic（闭锁综合征） → Coma（麻醉） | `runtime/cognitive_state.rs` |
| **苏醒/复活** | `WakeUpReason`：Recovery / Reanimation / Resurrection | `kernel/idle/` |
| **无聊** | `IdleKind::Boredom` + `BoredomActor` 加权随机挑技能 | `kernel/idle/` |
| **探索欲** | `IdleKind::Exploration`：跨领域关联、知识缺口挖掘 | `kernel/idle/` |
| **沉思** | `IdleKind::Meditation`：深度记忆整理 | `kernel/idle/` |
| **孵化** | `IdleKind::Incubation`：潜意识处理复杂问题 | `kernel/idle/` |
| **睡眠** | `IdleKind::Sleep`：记忆巩固、context 压缩 | `kernel/idle/` |
| **内省力** | `ArousalTracker` 指数衰减：刚忙完 arousal 高（该复盘），无聊时 arousal 低 | `kernel/idle/src/coordination.rs` |
| **反思** | `QueueDrained` → Reflection Pipeline，可被 select! 抢先 | `Dispatcher` + `ReflectionBreaker` |
| **事后复盘** | `workflow::completed` → 经验萃取器 → EXP.md 更新 | `kernel/experience/` |
| **经验积累** | EXP.md 三层：Tool Strategies / Anti-Patterns / Gotchas | `kernel/experience/` |
| **工具效能感** | Experience 翻译器：Confident→跳过侦查 / Apprehensive→绕路走 | `cognitive/engine/` |
| **日常节律** | TimeWindow：Morning/Midday/Evening/Night 各一套 Routine | `kernel/daily-life/` |
| **习惯追踪** | `CheckHabits` + escalation: Gentle→Friendly→Firm→Concerned | `kernel/daily-life/` |
| **动机/需求** | Maslow 5 层映射：Daily Life / Team / Startup | `docs/maslow-hierarchy.md` |
| **Ikigai 对齐** | 4 圆交集：热爱 × 擅长 × 世界需要 × 能赚钱 | `predefined/skills/startup/startup-ikigai/` |
| **信息饥饿** | Grounding 翻译器：Knowledge/ Situation 双维度 | `cognitive/engine/` |
| **不确定性表达** | `Decision::confidence` + Cons说的清楚 → 追问澄清 | `cognitive/engine/` |
| **渐进苏醒** | WakeUp Ouroboros：60s 静默 → 线性插值 depth→0 + arousal→1.0 | `kernel/idle/` |
| **工作倦怠预警** | `burnout-early-warning` skill：3 周生产力降 40% | `predefined/skills/startup/` |
| **决策质量追踪** | `founder-decision-journal` skill：认知偏差检测 | `predefined/skills/startup/` |
| **多 Agent 归属** | Team 插件：人机混编空间，@mention 触发归属感 | `kernel/plugins/` |

---

## 与 Hermes / CrewAI 的拟人化对比

| 维度 | Hermes | CrewAI | Aman |
|---|---|---|---|
| **身份** | SOUL.md 热加载 | 无（每 task 重置） | SOUL.md + SoulRuntime + 边界检查 |
| **情绪** | emoji 状态映射 | 无 | LLM 驱动 EmotionEvaluator + 事件发布 |
| **意识** | 无 | 无 | 4 档 CognitiveState + 恢复体验 |
| **空闲** | cron 轮询 | 无 | 9 种 IdleKind + arousal 衰减 + Ouroboros |
| **经验** | MEMORY.md 线性追加 | 无 | EXP.md 三层（策略/反模式/坑）置信度升降级 |
| **自省** | 无 | 无 | QueueDrained 强制 Reflection + 经验萃取 |
| **日常节律** | 无 | 无 | TimeWindow + 习惯 escal ation + 反思引导 |
| **需求/动机** | 无 | 无 | 马斯洛 5 层映射 + Ikigai + 5 维欲望 |
| **异步人格** | 无 | 无 | Work/Study/Daily/Idle 4 套并行 LifecycleEngine |

---

## 落地清单

如果你要为 aman 增加一个新 Agent 的拟人化能力，需要：

1. **定义身份**：`~/.aman/agents/{id}/SOUL.md` 填 identity / core / vibe / boundaries
2. **配置空闲人格**：`idle.personality` YAML 设定 enabled_kinds + depth_schedule + boredom
3. **启用情绪**：`~/.aman/agents/{id}/emotions/` 放 data.json + emoji 图片
4. **配置 Daily 节律**：`daily_life.routines` 各 TimeWindow 的例行事项
5. **启用 EXP.md**：`experience.extraction = true` 自动积累工具经验
6. **配置认知翻译**：Consciousness / Grounding / Experience 翻译器默认自动启用
7. **设置 Motivation**：如有创业场景，配置 `startup-ikigai` + `desire-evaluator`

详见 [personality-config.md](./personality-config.md)。

---

> **参考文档：**
> - [认知翻译层设计](../cognitive-memory.md)
> - [Maslow 需求层次映射](../maslow-hierarchy.md)
> - [Idle 系统设计](../idle-design.md)
> - [Idle → Boredom 流程](../idle-boredom-flow.md)
> - [拟人化与事件驱动](../agent-boredom-narrative-event-driven.md)
> - [认知状态模型](../ideas/cognitive-state-model.md)
> - [Daily Life 设计](../daily-life-design.md)
> - [Startup 集成设计](../startup.md)
> - [Team 设计](../team.md)
