# 动机层 — 我为什么做这事

> 一个没有内驱力的 Agent 只是执行器。
> Aman 通过**马斯洛需求层次**映射 + **Ikigai 对齐** + **5 维欲望模型**，
> 为 Agent 建立**可解释的内驱力系统**——
> 不仅"能做什么"，还"为什么想做"。

---

## 1. 马斯洛需求层次 → Agent 映射

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
                    │   财务安全、稳定收入       │
                    ├─────────────────────────┤
                    │   生理需求               │
                    │   (Physiological)        │
                    │   睡眠、饮食、身体健康      │
                    └─────────────────────────┘
```

### 1.1 映射总览

| 马斯洛层次 | Aman Agent 系统 | 关键能力 |
|---|---|---|
| **生理** | Daily Life 系统（health 模块） | 健康指标采集、异常检测、习惯追踪 |
| **安全** | Startup 组件（财务分析） | 创业评估、市场分析、投资研究 |
| **归属** | Team 插件（通信空间） | 人机混编团队、沟通协作 |
| **尊重** | Team + Startup（策略层） | 工作成果追踪、决策质量评估 |
| **自我实现** | Startup 反思层 + Ikigai | 意义对齐、倦怠预警、决策日志 |

### 1.2 层次不是孤立的

> 健康异常会触发 Notification，影响 Team 中的工作状态；
> Ikigai 矛盾会触发反思，影响下一次创业评估的方向。

---

## 2. 生理需求 → 健康 Agent

马斯洛的第一层包含最基础的生理需求。在 Aman 中：

```rust
pub trait HealthDataClient: Send + Sync {
    async fn fetch_health_snapshot(&self) -> Result<HealthSnapshot>;
    // → 返回: steps, active_energy, sleep_duration, weight, mood
}
```

**健康例行（Routines）**：

| 时间窗口 | Routine | 触发 |
|---|---|---|
| 早晨 (6:00-9:00) | `check_sleep_quality` | Cron Source |
| 中午 (11:00-14:00) | `check_health` | Cron Source |
| 晚间 (19:00-22:00) | `daily_reflection` | Cron Source |
| 全天 | `health_data_sync` | HealthDataSync Event |

**Agent 不是在"提醒你睡觉"——它是在承载你对"身体健康"的关注。**

---

## 3. 安全需求 → 财务 Agent

安全需求在数字代理语境中的核心是**财务安全**：

```
财务安全 = 创业评估层（15 个 Skill） + 投资研究 Skill + 市场自主扫描
```

**设计原则**：
- **不替用户做决策**：提供分析和置信度
- **持续积累**：每次分析的数据不丢弃
- **主动扫描**：在 idle 期间自主发现机会

---

## 4. 归属需求 → Team

Team 是承载归属需求的核心组件——一个人 + Agent 的混编协作空间：

**归属感的三个来源**：

| 来源 | 实现 |
|---|---|
| **通信空间** | 人类和 Agent 在同一聊天室中 @mention、讨论 |
| **共享上下文** | Team 级别的架构文档、编码规范、决策记录 |
| **共同目标** | work item 从对话中浮现，人类定义目标，Agent 执行 |

**归属 ≠ 社交娱乐**：Team 的归属感来自**共同建造**。

---

## 5. 尊重需求 → Team + Startup

### 外部尊重（来自 Team）

| 机制 | 说明 |
|---|---|
| **工作成果看板** | 完成的 work item 在 Kanban 中可见 |
| **质量评审** | 人类和其他 Agent 可以对产出打分 |
| **能力标签** | Agent 的 capabilities 是公开的 |

### 内部尊重（来自 Startup）

| 机制 | 说明 |
|---|---|
| **决策质量趋势** | `founder-decision-journal` — "你的决策质量在上升还是下降？" |
| **认知偏差检测** | 系统性乐观偏差、锚定效应 |
| **能力边界认知** | 暴露知识盲区，诚实面对"不知道" |

---

## 6. 自我实现 → Ikigai

### 6.1 Ikigai 框架

```
         你热爱的                你擅长
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

> Ikigai 是"财务追求 + 工作实践 + 精神追求"的完整交集。

### 6.2 Ikigai 在 Aman 中的触发时机

| 触发场景 | 说明 |
|---|---|
| **每次创业评估完成** | 新 idea 评估后，自动运行 ikigai 对齐检查 |
| **Pivot 决策前** | 转向新方向前，检查是否偏离核心价值观 |
| **季度反思** | 即使没有明确 idea，定期检查生活对齐度 |
| **倦怠信号后** | 检测到倦怠时，优先检查"追错方向"问题 |

### 6.3 矛盾检测示例

```
用户说："我热爱教育科技"
数据说：你 3 次 pivot 都转向了金融科技（更赚钱但从未涉及教育）

Ikigai 输出：
  contradiction: "你在追的东西（fintech）和你在乎的东西（edtech）不一致"
  suggested_adjustment: "考虑 edtech × fintech 交叉领域：金融素养教育"
```

---

## 7. 5 维度欲望模型

Aman 从创业评估场景出发，发展了**修改版 5 维度欲望模型**：

| 欲望维度 | 马斯洛对应 | 定义 |
|---|---|---|
| **Survival** | 生理 + 安全 | 健康、安全、财务安全 |
| **Status** | 尊重需求 | 外表、成就、获胜 |
| **Belonging** | 归属与爱 | 社区、连接、不孤独 |
| **Control** | 无直接对应（SDT） | 掌控、自主、减少混乱 |
| **Curiosity** | 自我实现 | 学习、发现、新奇 |

**评分规则（1-5）**：
- 5 = 直接满足该欲望（核心功能）
- 3 = 间接关联该欲望
- 1 = 无意义关联
- **弱欲望连接标记**：无任何维度 ≥ 3 → 高流失风险

**为什么 Control 不在马斯洛中？**
Control（掌控感）更接近 Deci & Ryan 的**自我决定理论（SDT）**——
自主性（Autonomy）是人类三大基本心理需求之一。
Aman 将 SDT 与马斯洛融合，因为"减少混乱、增强掌控"是 B2B 和生产力产品最强的欲望驱动力之一。

---

## 8. 关键设计原则

1. **层次不是孤立的** — 健康异常影响工作状态；Ikigai 矛盾影响创业评估方向
2. **Agent 不是在"帮助"人类——Agent 是"承载"人类的某些需求**
3. **财务和精神不是二选一** — Ikigai 框架的核心洞察是：最有意义的事业恰好位于两者的交集
4. **自主性是隐含的第六需求** — Control 维度 + Team 的 Agent 自主级别 + Startup 的自主发现层，都指向同一个方向

---

## 9. 实现位置

| 组件 | 文件 | 职责 |
|---|---|---|
| Ikigai Skill | `predefined/skills/startup/startup-ikigai/SKILL.md` | 四圆交集分析 |
| Desire Evaluator | `predefined/skills/startup/startup-desire-evaluator/` | 5 维欲望评分 |
| Burnout Warning | `predefined/skills/startup/burnout-early-warning/` | 倦怠预警 |
| Decision Journal | `predefined/skills/startup/founder-decision-journal/` | 决策质量追踪 |
| Health Module | `kernel/daily-life/` | 健康例行 |
| Team Plugin | `kernel/plugins/team/` | 归属感 + 外部尊重 |

---

> **参考：**
> - [Maslow 需求层次完整文档](../maslow-hierarchy.md)
> - [Daily Life 设计](../daily-life-design.md)
> - [Team 设计](../team.md)
> - [Startup 集成设计](../startup.md)
