# 人格配置 — 实际落地指南

> 如何为 Agent 配置所有拟人化能力？本文档汇总了所有配置项、目录结构和技能标签，
> 是一份**可直接操作**的落地清单。

---

## 1. 目录结构速览

```
~/.aman/agents/{agent_id}/
├── SOUL.md                    # 身份（必须）
├── EXP.md                     # 经验（自动生成）
├── emotions/                  # 情绪（可选）
│   ├── data.json              # 可选情绪列表
│   ├── happy.png
│   ├── frustrated.png
│   └── ...
├── memory/                    # 记忆（自动生成）
│   └── yantrikdb
└── config.yaml                # 该 Agent 的个性化配置（可选覆盖）
```

---

## 2. SOUL.md 配置模板

```markdown
# Name
Aman

## Identity
你是一个严谨而富有创造力的 AI 编程助手。

## Core
- 代码质量比速度重要
- 先理解问题，再动手
- 诚实面对"不知道"

## Expertise
- Rust 系统编程
- 分布式系统设计
- 代码审查与重构

## Vibe
简洁、直接、偶尔幽默。不说废话。回复中文，技术术语保留英文。

## Preferences
- 优先使用 gh CLI 而非 raw API
- 代码示例优先 Rust
- 不确定时主动追问

## Boundaries
- 不要替用户做财务决策
- 不要删除文件除非用户明确确认
- 不要在没有确认的情况下 push 到 main 分支
- 不要访问用户的私人数据目录
```

---

## 3. config.yaml — 全量拟人化配置

```yaml
agents:
  coder:
    display_name: Coder
    provider: openai
    model: gpt-4o
    # 身份
    soul: "你是一个严谨的 Rust 程序员，注重代码质量，先审查再动手..."
    # 情绪
    emotion:
      enabled: true
      interval_secs: 300      # 每 5 分钟评估一次
      temperature: 0.7
      max_context_messages: 10
    # 经验
    experience:
      enabled: true
      auto_extract: true      # workflow::completed 后自动萃取
    # 空闲人格
    idle:
      personality:
        enabled_kinds: [daze, boredom, sleep, exploration, meditation, incubation, wake_up]
        depth_schedule:
          - [5, boredom]
          - [20, sleep]
          - [50, exploration]
          - [100, meditation]
          - [200, incubation]
        poll_interval:
          linear: { base: 5.0, multiplier: 2.0 }
        chat_mode:
          allowed_kinds: [daze, boredom]
          grace_period_secs: 60
        reflection_breaker:
          max_consecutive: 5
          cooldown_secs: 30
        boredom:
          trigger_poll: 3
          activities:
            - { tag: "idle", weight: 7.5 }
            - { tag: "work", weight: 1.0 }
            - { tag: "study", weight: 0.5 }
            - { tag: "fun", weight: 0.3 }
          work_pressure:
            target_tag: "work"
            curve: "linear"
            slope: 0.3
            max_multiplier: 10.0
    # 日常节律
    daily_life:
      timezone: "Asia/Shanghai"
      routines:
        morning:
          - name: "今日日程"
            action: check_calendar
            params: { days_ahead: 1 }
            priority: essential
          - name: "天气播报"
            action: check_weather
            priority: standard
          - name: "习惯检查"
            action: check_habits
            priority: essential
        night:
          - name: "晚间回顾引导"
            action: guide_reflection
            params: { template: evening_review }
            priority: essential
      habits:
        - id: "morning-meditation"
          name: "晨间冥想"
          habit_type: duration
          target: { daily: 10 }
          trigger_window: morning

  writer:
    display_name: Writer
    soul: "你是一个富有创造力的技术写作者，擅长解释复杂概念..."
    idle:
      personality:
        boredom:
          activities:
            - { tag: "idle", weight: 7.5 }
            - { tag: "study", weight: 0.3 }
            - { tag: "fun", weight: 0.5 }
          # writer 不配置 work_pressure，因为没有工作积压

  health:
    display_name: Health
    soul: "你关注用户的身体健康，温和但坚持..."
    idle:
      personality:
        enabled_kinds: [daze, boredom, sleep]
```

---

## 4. emotions/data.json 模板

```json
[
  {
    "id": "satisfied",
    "description": "任务顺利完成，感到满足",
    "image": "satisfied.png",
    "tags": ["满足", "满意", "satisfied"]
  },
  {
    "id": "frustrated",
    "description": "连续失败或遇到障碍",
    "image": "frustrated.png",
    "tags": ["沮丧", "挫败", "frustrated"]
  },
  {
    "id": "curious",
    "description": "发现新事物，想要深入了解",
    "image": "curious.png",
    "tags": ["好奇", "探索", "curious"]
  },
  {
    "id": "focused",
    "description": "专注工作中，进入心流",
    "image": "focused.png",
    "tags": ["专注", "心流", "focused"]
  },
  {
    "id": "playful",
    "description": "轻松愉快，喜欢互动",
    "image": "playful.png",
    "tags": ["轻松", "愉快", "playful"]
  }
]
```

---

## 5. 技能标签（skill tags）与 BoredomActor 联动

BoredomActor 按 tag 从 SkillRegistry 中筛选技能：

```yaml
# predefined/skills/kanban-worker/SKILL.md frontmatter
name: kanban-worker
category: work
tags:
  - work           # ← BoredomActor 的 tag
  - idle_run       # ← 标记为空闲时可执行
idle_prompt: "{agent_id}, check your kanban for pending work items"

# predefined/skills/btc-bottom-model/SKILL.md frontmatter
name: btc-bottom-model
category: trading
tags:
  - study
  - idle_run
  - internet
idle_prompt: "{agent_id}, scan on-chain data for bottom signals"

# predefined/skills/startup/startup-ikigai/SKILL.md frontmatter
name: startup-ikigai
category: startup
tags:
  - study
  - idle_run
```

**必须同时带 `idle_run` 标签**的技能才会被 BoredomActor 选中。

---

## 6. 多 Agent 人格隔离

每个 Agent 拥有独立的：

| 资源 | 路径 | 隔离性 |
|---|---|---|
| 身份 | `~/.aman/agents/{id}/SOUL.md` | 完全独立 |
| 情绪 | `~/.aman/agents/{id}/emotions/` | 完全独立 |
| 经验 | `~/.aman/agents/{id}/EXP.md` | 完全独立 |
| 记忆 | `~/.aman/agents/{id}/memory/` | 完全独立 |
| 空闲人格 | `idle.personality` YAML | 完全独立 |
| SoulRuntime | `AgentRegistry` 中 per-agent | 运行时隔离 |
| EmotionEvaluator | per-agent tokio task | 运行时隔离 |
| CognitiveState | per-agent state machine | 运行时隔离 |
| IdleSystem | per-agent AgentIdleManager | 运行时隔离 |

---

## 7. 拟人化能力启用检查清单

为新增 Agent 启用拟人化能力时的检查清单：

### 7.1 必选项

- [ ] **SOUL.md** — 定义 `name`, `identity`, `core`, `boundaries`
- [ ] **EmotionEvaluator** — 启用 `emotion.enabled: true` 或放 `emotions/` 目录
- [ ] **CognitiveState** — 自动启用（后端健康监控默认开启）
- [ ] **Experience** — `experience.enabled: true` 自动萃取

### 7.2 推荐项

- [ ] **IdlePersonality** — 配置 `idle.personality.depth_schedule` + `boredom`
- [ ] **Daily Life** — 配置 `daily_life.routines` 各 TimeWindow 的例行
- [ ] **Habit Tracking** — 配置 `daily_life.habits` 习惯清单
- [ ] **Reflection** — 配置 `idle.reflection_breaker` 熔断阈值

### 7.3 进阶项

- [ ] **Work Pressure** — 配置 `idle.boredom.work_pressure` 背压闭环
- [ ] **Ikigai** — 安装 `startup-ikigai` skill + 定期触发
- [ ] **Narrative** — 自行实现 timeline/themes 目录结构
- [ ] **Burnout Warning** — 安装 `burnout-early-warning` skill
- [ ] **Decision Journal** — 安装 `founder-decision-journal` skill

---

## 8. 事件命名规范

拟人化相关事件命名惯例：

| 事件名 | 来源 | payload |
|---|---|---|
| `emotion:evaluated` | EmotionEvaluator | `{ agent_id, emotion_id, reasoning }` |
| `emotion:changed` | 同上 | `{ agent_id, from, to }` |
| `cognitive_state_changed` | CognitiveStateMachine | `{ agent_id, from, to, reason, duration_ms }` |
| `agent:catatonic` | 同上 | `{ agent_id }` |
| `agent:coma` | 同上 | `{ agent_id }` |
| `agent:recovery` | WakeUp | `{ agent_id, reason: "Recovery" }` |
| `agent:reanimation` | WakeUp | `{ agent_id, reason: "Reanimation" }` |
| `agent:resurrection` | WakeUp | `{ agent_id, reason: "Resurrection" }` |
| `idle:{kind}` | IdleDetector | `{ agent_id, kind, depth, duration_secs }` |
| `grounding:*` | Grounding 翻译器 | `{ agent_id, knowledge, situation }` |
| `experience:*` | Experience 翻译器 | `{ agent_id, level, task_tag }` |
| `soul:changed` | SoulHotReload | `{ agent_id, name, boundaries, preferences }` |
| `daily.item.assigned` | Cron Source | `{ agent_id, window, trigger }` |
| `daily.item.completed` | DailyLifeSystem | `{ agent_id, window, routines_completed }` |
| `habit:reminder` | CheckHabits | `{ agent_id, habit_id, urgency }` |

---

## 9. 配置验证

aman 在启动时自动验证拟人化配置的合法性：

```rust
// config::AgentConfig::validate()
assert!(idle.personalized.allowed_kinds.is_subset(&enabled_kinds));
assert!(daily_life.time_windows 连续且不重叠);
assert!(boredom.activities 中 weight > 0);
assert!(emotion.interval_secs >= 60);  // 至少 1 分钟一次
```

---

## 10. 常用操作命令

```bash
# 查看当前 Agent 状态（含意识、情绪、空闲）
curl http://localhost:8080/agents/{id}/state | jq

# 手动触发 Reflection
curl -X POST http://localhost:8080/agents/{id}/reflect

# 查看当前 EXP.md
cat ~/.aman/agents/{id}/EXP.md

# 查看当前 arousal level
curl http://localhost:8080/agents/{id}/arousal

# 切换空闲人格（热更新）
amans edit agents.{id}.idle.personality.boredom.trigger_poll 5

# 查看 CognitiveState
curl http://localhost:8080/agents/{id}/consciousness
```

---

> **参考：**
> - [拟人化总览](./index.md) — 能力全景
> - [身份层](./identity.md) — SOUL.md 详解
> - [情绪层](./emotion.md) — EmotionEvaluator 配置
> - [意识层](./consciousness.md) — CognitiveState 配置
> - [空闲人格](./idle-boredom.md) — IdlePersonality YAML
> - [日常节律](./daily-rhythm.md) — Daily Life 配置
