# 马斯洛需求层次理论与 Aman Agent 架构映射

> **核心命题：** Aman 的 agent 体系是马斯洛需求层次在数字空间中的工程化表达——每个需求层次对应一个或多个 agent 子系统，从生理健康到自我实现，形成完整的"数字人格"支撑。

---

## 目录

1. [马斯洛需求层次理论概述](#1-马斯洛需求层次理论概述)
2. [需求层次 → Agent 映射总览](#2-需求层次--agent-映射总览)
3. [第一层：生理需求 → 健康 Agent](#3-第一层生理需求--健康-agent)
4. [第二层：安全需求 → 财务 Agent](#4-第二层安全需求--财务-agent)
5. [第三层：归属需求 → Team](#5-第三层归属需求--team)
6. [第四层：尊重需求 → Team + Startup](#6-第四层尊重需求--team--startup)
7. [第五层：自我实现 → Startup + Ikigai](#7-第五层自我实现--startup--ikigai)
8. [Aman 的 5 维度欲望模型](#8-aman-的-5-维度欲望模型)
9. [Ikigai：财务追求与精神追求的统一](#9-ikigai财务追求与精神追求的统一)
10. [架构全景图](#10-架构全景图)

---

## 1. 马斯洛需求层次理论概述

亚伯拉罕·马斯洛（Abraham Maslow）于 1943 年提出需求层次理论，将人类动机按优先级分为五个层次：

```
                    ┌─────────────────────────┐
                    │   自我实现               │
                    │   (Self-actualization)   │
                    │   创造力、意义、成长      │
                    ├─────────────────────────┤
                    │   尊重需求               │
                    │   (Esteem)               │
                    │   成就、地位、认可        │
                    ├─────────────────────────┤
                    │   归属与爱               │
                    │   (Love/Belonging)       │
                    │   社交、团队、亲密关系     │
                    ├─────────────────────────┤
                    │   安全需求               │
                    │   (Safety)               │
                    │   财务安全、健康保障、稳定  │
                    ├─────────────────────────┤
                    │   生理需求               │
                    │   (Physiological)        │
                    │   睡眠、饮食、身体健康      │
                    └─────────────────────────┘
```

核心原则：

- **层次递进**：低层需求得到基本满足后，高层需求才会成为主要动机
- **非刚性**：不需要 100% 满足低层才能追求高层——一个人可以在健康欠佳时仍追求事业
- **动态性**：需求满足程度随生活状态波动，Agent 系统需要持续感知、适应

---

## 2. 需求层次 → Agent 映射总览

Aman 将马斯洛五层需求映射到四个 agent 子系统中：

| 马斯洛层次 | 人类需求 | Aman Agent 系统 | 关键能力 |
|---|---|---|---|
| **生理** | 睡眠、饮食、身体健康 | **Daily Life 系统**（health 模块） | 健康指标采集、异常检测、习惯追踪 |
| **安全** | 财务安全、稳定收入 | **Startup 组件**（财务分析） | 创业评估、市场分析、投资研究 |
| **归属** | 团队、社交、社区 | **Team 插件**（通信空间） | 人机混编团队、沟通协作 |
| **尊重** | 成就、地位、认可 | **Team + Startup**（策略层） | 工作成果追踪、决策质量评估 |
| **自我实现** | 意义、创造、成长 | **Startup 组件**（反思层 + Ikigai） | 意义对齐、倦怠预警、决策日志 |

```
Aman Agent 体系

  ┌──────────────────────────────────────────────────────────────────┐
  │                        自我实现                                   │
  │   Startup 反思层: Ikigai 对齐 / 倦怠预警 / 决策日志               │
  │   ┌──────────────────────────────────────────────────────────┐   │
  │   │                     尊重需求                              │   │
  │   │   Team: 工作成果看板、质量评审                             │   │
  │   │   Startup: 决策质量趋势、认知偏差检测                      │   │
  │   │   ┌─────────────────────────────────────────────────┐    │   │
  │   │   │                   归属需求                       │    │   │
  │   │   │  Team: 通信空间、@mention、对话→work item        │    │   │
  │   │   │  ┌────────────────────────────────────────┐     │    │   │
  │   │   │  │                安全需求                 │     │    │   │
  │   │   │  │  Startup: 创业评估、财务分析、投资研究   │     │    │   │
  │   │   │  │  ┌──────────────────────────────┐      │     │    │   │
  │   │   │  │  │         生理需求              │      │     │    │   │
  │   │   │  │  │  Daily Life: 健康监测、       │      │     │    │   │
  │   │   │  │  │  睡眠追踪、习惯管理            │      │     │    │   │
  │   │   │  │  └──────────────────────────────┘      │     │    │   │
  │   │   │  └────────────────────────────────────────┘     │    │   │
  │   │   └─────────────────────────────────────────────────┘    │   │
  │   └──────────────────────────────────────────────────────────┘   │
  └──────────────────────────────────────────────────────────────────┘
```

---

## 3. 第一层：生理需求 → 健康 Agent

### 3.1 理论对应

马斯洛的第一层包含最基础的生理需求：呼吸、睡眠、饮食、身体健康。这些需求若不满足，个体会将所有注意力集中在弥补这些缺失上。

### 3.2 Aman 实现：Daily Life 健康系统

Aman 通过 **Daily Life System**（`kernel/daily-life`）中的健康模块来承载这一层：

```rust
// 设计文档: docs/daily-life-design.md

// HealthDataClient trait — 健康数据读取接口
pub trait HealthDataClient: Send + Sync {
    async fn fetch_health_snapshot(&self) -> Result<HealthSnapshot>;
    // → 返回: steps, active_energy, sleep_duration, weight, mood
}
```

**健康例行（Routines）：**

| 时间窗口 | Routine | 触发 |
|---|---|---|
| 早晨 (6:00-9:00) | `check_sleep_quality` | Cron Source |
| 中午 (11:00-14:00) | `check_health` | Cron Source |
| 晚间 (19:00-22:00) | `daily_reflection` | Cron Source |
| 全天 | `health_data_sync` | HealthDataSync Event |

**异常检测：**

```yaml
health_metrics:
  sleep_duration:
    low: 6.0h    # 低于 6h → 异常告警
    high: 10.0h  # 高于 10h → 异常告警
  steps:
    low: 3000
  mood:
    low: 3       # 1-5 量表，连续 3 天 < 3 → 干预建议
```

**集成点：**
- Apple Health / Fitbit → `HealthDataSync` Event → EventBus → DailyLifeSystem
- 健康异常 → `HealthAnomaly` Event → Notification 系统 → 推送提醒
- 健康数据 → MemoryStore（保留 365 天）

### 3.3 多 Agent 配置

在 README 展示的多 agent 配置中，每个 agent 可以有独立的 provider/model/soul：

```yaml
agents:
  health:
    display_name: Health
    provider: deepseek
    model: deepseek-v4-flash
    soul: "你关注用户的身体健康，温和但坚持..."
```

---

## 4. 第二层：安全需求 → 财务 Agent

### 4.1 理论对应

安全需求涵盖人身安全、健康保障、资源所有权、**财务安全**和**稳定收入**。在数字代理的语境中，"财务安全"是最核心的可操作维度。

### 4.2 Aman 实现：Startup 财务分析能力

Aman 没有独立的"money agent"，财务安全通过 **Startup 组件**的评估层和投资技能来承载：

```
财务安全 = 创业评估层（15 个 Skill） + 投资研究 Skill + 市场自主扫描
```

**创业评估层（与财务直接相关的 Skill）：**

| Skill | 财务维度 | 说明 |
|---|---|---|
| `startup-pricing` | 定价与支付意愿 | Van Westendorp 价格敏感度 + 欲望溢价系数 |
| `startup-cac-model` | 获客成本 | 按渠道获客成本建模 |
| `startup-tam-sam-som` | 市场规模 | 三角验证自底向上市场估算 |
| `startup-distribution` | 分发效率 | 6 种病毒循环 k-factor |
| `startup-retention` | 留存与收入预测 | 流失预测 + LTV 估算 |

**投资研究 Skill：**

| Skill | 说明 |
|---|---|
| `investment/ipo-research` | IPO 深度研究 |
| `investment/unlisted-ecosystem-analysis` | 非上市公司生态分析 |

**自主财务安全层：**

```python
# 设计文档: docs/startup.md 第 7 节

# TrendWatcher — 每周扫描新兴市场机会
# MarketMonitor — 竞品定价变动 → webhook 通知
# IncubationBridge — idle 期间跨领域关联历史分析
```

### 4.3 财务 Agent 的设计原则

- **不替用户做决策**：提供分析和置信度，最终决策权在人类
- **持续积累**：每次分析的数据（竞品、市场、定价）不丢弃，形成知识库
- **主动扫描**：在 idle 期间自主发现机会，不等用户触发

---

## 5. 第三层：归属需求 → Team

### 5.1 理论对应

归属与爱的需求包括：友谊、家庭、**团队归属感**、社会连接。人类是社会性动物，在孤立状态下心理和生理都会恶化。

### 5.2 Aman 实现：Team 插件

Team 是 Aman 中承载归属需求的核心组件——一个人 + Agent 的混编协作空间：

```yaml
# 设计文档: docs/team.md

team:
  name: "Aman Core Team"
  description: "Aman agent framework development"

members:
  - id: "jerin"
    type: human
    name: "Jerin"
    roles: [owner]

  - id: "coder"
    type: agent
    name: "Coder"
    profile: "coder"
    capabilities: [code, refactor, fix]
    autonomy: autonomous
```

**归属感的三个来源：**

| 来源 | 实现 |
|---|---|
| **通信空间** | 人类和 Agent 在同一聊天室中 @mention、讨论、决策 |
| **共享上下文** | Team 级别的架构文档、编码规范、决策记录——Agent 执行时自动加载 |
| **共同目标** | work item 从对话中浮现，人类定义目标，Agent 执行，成果共享 |

**归属 ≠ 社交娱乐**：Team 的归属感来自**共同建造**——不是闲聊机器人，而是一起产出有价值的工作成果。

### 5.3 归属需求中的"精神追求"维度

Team 不只是"干活的地方"，它承载了精神层面的需求：

- **被理解**：Agent 加载 user_profile 和 SOUL.md，理解人类的偏好和价值观
- **被回应**：@mention Agent → 即时响应，不是异步工单系统
- **共同成长**：决策日志和学习记录在 Team 中共享，团队一起进化

---

## 6. 第四层：尊重需求 → Team + Startup

### 6.1 理论对应

尊重需求分为两类：
- **外部尊重**：地位、认可、名声、他人的尊重
- **内部尊重**：自尊、自信、成就感、掌控感

### 6.2 Aman 实现

#### 外部尊重（来自 Team）

| 机制 | 说明 |
|---|---|
| **工作成果看板** | 完成的 work item 在 Kanban 中可见，积累可量化的"成就清单" |
| **质量评审** | 人类和其他 Agent 可以对产出打分、评论 |
| **能力标签** | Agent 的 capabilities 是公开的——"擅长什么"是一种身份 |

#### 内部尊重（来自 Startup）

| 机制 | 说明 |
|---|---|
| **决策质量趋势** | `founder-decision-journal` — "你的决策质量在上升还是下降？" |
| **认知偏差检测** | 系统性乐观偏差、锚定效应、确认偏差——认识自己才能尊重自己 |
| **能力边界认知** | 15 个评估维度暴露自己的知识盲区，诚实面对"不知道" |

### 6.3 尊重需求的"精神追求"维度

尊重需求是"财务追求"和"精神追求"的交汇点：

```
财务追求 → 创业成功、市场认可 →  外部尊重（地位、收入）
精神追求 → 认识自己、诚实决策 →  内部尊重（自尊、掌控感）
```

---

## 7. 第五层：自我实现 → Startup + Ikigai

### 7.1 理论对应

自我实现是马斯洛金字塔的顶端：**成为你能成为的人**。包括创造力、自发性、解决问题、道德感和意义追求。

### 7.2 Aman 实现：Startup 反思层

Startup 的反思层（Reflection Layer）直接承载自我实现需求：

| Skill | 自我实现维度 | 说明 |
|---|---|---|
| `startup-ikigai` | 意义对齐 | 四圆交集分析——你现在追的东西和你在乎的东西一致吗？ |
| `founder-decision-journal` | 认知进化 | 追踪决策质量、暴露隐含假设、检测系统性偏差 |
| `burnout-early-warning` | 可持续性 | 3 周生产力下降 40% → 倦怠信号 → 干预建议 |
| `startup-what-if` | 创造力 | "如果你有无限资源，你会做什么？"——不设限的探索 |

### 7.3 Ikigai：财务追求与精神追求的统一

Ikigai（生き甲斐）是日语概念，意为"存在的理由"。Aman 将其建模为四个圆的交集：

```
         你热爱的                你擅长的
      ┌───────────┐         ┌───────────┐
      │  Passion  │         │ Profession│
      │           │         │           │
      └─────┬─────┘         └─────┬─────┘
            │   ┌───────────────┐ │
            │   │    Ikigai     │ │
            │   │   (存在的理由)  │ │
            │   └───────────────┘ │
      ┌─────┴─────┐         ┌─────┴─────┐
      │  Mission  │         │ Vocation  │
      │           │         │           │
      └───────────┘         └───────────┘
       世界需要的              你能被付钱的
```

Ikigai 是"财务追求（你能被付钱的）+ 工作实践（你擅长的）+ 精神追求（你热爱的 × 世界需要的）"的完整交集。

```python
@dataclass
class IkigaiCheck:
    alignment_score: float              # 0-100
    overlapping_quadrants: list[str]     # 当前对齐了哪几个圆
    missing_quadrant: str                # 缺失的圆
    contradiction: str                   # "你 3 次 pivot 都更赚钱，但没有一次更符合你的价值观"
    suggested_adjustment: str            # 具体的调整建议
```

---

## 8. Aman 的 5 维度欲望模型

Aman 没有直接使用马斯洛的五层，而是从创业评估场景出发，发展了一个**修改版 5 维度欲望模型**：

| 欲望维度 | 马斯洛对应 | 定义 | 与产品的关系 |
|---|---|---|---|
| **Survival** | 生理 + 安全 | 健康、安全、财务安全 | 使用此产品是否让用户感到更安全/健康？ |
| **Status** | 尊重需求 | 外表、成就、获胜 | 是否帮用户展现地位、成就、优越感？ |
| **Belonging** | 归属与爱 | 社区、连接、不孤独 | 是否将用户与共享身份/兴趣的人连接？ |
| **Control** | 无直接对应 | 掌控、自主、减少混乱 | 是否帮用户感到对生活或环境的掌控？ |
| **Curiosity** | 自我实现 | 学习、发现、新奇 | 是否满足探索、学习的冲动？ |

**评分规则（1-5）：**

- 5 = 直接满足该欲望（核心功能）
- 3 = 间接关联该欲望
- 1 = 无意义关联
- **弱欲望连接标记**：无任何维度 ≥ 3 → 高流失风险

**差异化：为什么 Control 不在马斯洛中？**

Control（掌控感）更接近 Deci & Ryan 的**自我决定理论（SDT）**——自主性（Autonomy）是人类三大基本心理需求之一。Aman 将 SDT 与马斯洛融合，因为"减少混乱、增强掌控"是 B2B 和生产力产品最强的欲望驱动力之一。

---

## 9. Ikigai：财务追求与精神追求的统一

### 9.1 为什么需要 Ikigai？

马斯洛需求层次给出了"需求是什么"，而 Ikigai 给出了"需求之间如何平衡"——特别是**财务追求和精神追求不是对立的**：

```
           错误的二分法                      正确的交集

   财务 ────────────── 精神              财务
    │                   │                 │
    │     ← 鸿沟 →     │          ┌──────┴──────┐
    │                   │          │   Ikigai   │
   赚钱                意义         │  有意义    │
   务实                浪漫         │  的赚钱     │
                                    └──────┬──────┘
                                           │
                                          精神
```

### 9.2 Ikigai 在 Aman 中的触发时机

| 触发场景 | 说明 |
|---|---|
| **每次创业评估完成** | 新 idea 评估后，自动运行 ikigai 对齐检查 |
| **Pivot 决策前** | 转向新方向前，检查是否偏离核心价值观 |
| **季度反思** | 即使没有明确的 idea 要评估，定期检查生活对齐度 |
| **倦怠信号后** | 检测到倦怠信号时，优先检查是否存在"追错方向"问题 |

### 9.3 矛盾检测示例

```
用户说："我热爱教育科技"
数据说：你 3 次 pivot 都转向了金融科技（更赚钱但从未涉及教育）

Ikigai 输出：
  contradiction: "你在追的东西（fintech）和你在乎的东西（edtech）不一致"
  suggested_adjustment: "考虑 edtech × fintech 交叉领域：金融素养教育"
```

---

## 10. 架构全景图

### 10.1 需求层次与 Aman 事件流

```
外部世界                          Aman Agent 体系
────────                    ────────────────────────

健康数据源                    DailyLifeSystem
(Apple Health/Fitbit)  ──→   health routines
                              │
用户输入/对话          ──→  EventBus  ──→  CognitiveEngine
                              │                │
市场事件                      │            Decision
(Trend/Competitor)     ──→    │                │
                              ↓                ↓
                          Dispatcher      Notification
                              │                │
                    ┌─────────┼─────────┐      │
                    ↓         ↓         ↓      ↓
                Team       Startup   Study   Push
                Plugin     Plugin    System  /Email
                    │         │
              ┌─────┴──┐  ┌──┴──────────┐
              │Kanban  │  │Evaluation    │
              │Chat    │  │Strategy      │
              │Scheduler│ │Execution     │
              │Safety  │  │Reflection    │
              └────────┘  │(Ikigai/Burnout)│
                          └──────────────┘
```

### 10.2 需求层次总结表

| 马斯洛层次 | Aman 组件 | 财务追求 | 精神追求 | 具体产出 |
|---|---|---|---|---|
| 生理 | Daily Life | — | — | 健康快照、异常告警、习惯完成率 |
| 安全 | Startup 评估层 | ✅ 定价/市场规模/成本 | — | 竞争分析、TAM/SAM/SOM、决策备忘 |
| 归属 | Team 通信空间 | — | ✅ 团队归属 | 对话线程、work item、共享上下文 |
| 尊重 | Team + Startup 策略层 | ✅ 外部认可 | ✅ 内部尊重 | 工作成果看板、决策质量趋势、偏差检测 |
| 自我实现 | Startup 反思层 | ✅ 有意义的价值创造 | ✅ 意义对齐 | Ikigai 分析、倦怠预警、决策日志 |

### 10.3 关键设计原则

1. **层次不是孤立的** — 健康异常会触发 Notification，影响 Team 中的工作状态；Ikigai 矛盾会触发反思，影响下一次创业评估的方向
2. **Agent 不是在"帮助"人类 — Agent 是"承载"人类的某些需求**。健康 Agent 不是在提醒你睡觉——它是在承载你对"身体健康"的关注，让你可以把认知资源释放出来
3. **财务和精神不是二选一** — Ikigai 框架的核心洞察是：最有意义的事业恰好位于两者的交集。Aman 的 job 不是让你在这两者之间选择，而是帮你找到交集
4. **自主性是隐含的第六需求** — 5 维度欲望模型中的 Control 维度、Team 的 Agent 自主级别、Startup 的自主发现层，都指向同一个方向：人类需要感到自己（或自己的代理）在掌控生活

---

> **参考文档：**
> - [Startup 集成设计](startup.md) — 24 个 Skill 的完整设计
> - [Team 业务逻辑设计](team.md) — 人机混编团队设计
> - [Daily Life 系统设计](daily-life-design.md) — 健康与日常例行
> - [Team 架构设计](team-architect.md) — Rust 实现架构
> - [Ikigai Skill](../../predefined/skills/startup/startup-ikigai/SKILL.md)
> - [Desire Evaluator Skill](../../predefined/skills/startup/startup-desire-evaluator/SKILL.md)
