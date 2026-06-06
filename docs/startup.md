# Startup Analysis — 集成设计文档

> **状态：** 设计阶段
> **参考：** [idea-validation-agents](https://github.com/MaxKmet/idea-validation-agents) 的 15 个评估维度 + 4 个 workflow 编排模式
>
> **核心命题：** Startup 分析是财务追求、工作实践、精神追求的综合体现——不只是一次性评估，而是持续的知识积累和自主机会发现。

---

## 目录

1. [设计目标](#1-设计目标)
2. [从 idea-validation-agents 引入什么](#2-从-idea-validation-agents-引入什么)
3. [组件定位：为什么是独立组件而非 Kernel 能力](#3-组件定位)
4. [架构总览](#4-架构总览)
5. [Skill 映射](#5-skill-映射)
    - [5.1 评估层（15 个 Skill）](#51-评估层15-个-skill)
    - [5.2 策略层（4 个 Skill）](#52-策略层4-个-skill)
    - [5.3 执行层（1 个 Skill）](#53-执行层1-个-skill)
    - [5.4 反思层（3 个 Skill）](#54-反思层3-个-skill)
    - [5.5 AI-Native 层（1 个 Skill）](#55-ai-native-层1-个-skill)
    - [5.6 Skill 依赖全图](#56-skill-依赖全图)
6. [数据流：一次完整评估](#6-数据流一次完整评估)
7. [Aman 独有能力：自主层](#7-aman-独有能力自主层)
8. [集成点](#8-集成点)
9. [与 Team 组件的并行设计](#9-与-team-组件的并行设计)
10. [UI 设计](#10-ui-设计)
11. [实现路线](#11-实现路线)

---

## 1. 设计目标

### 1.1 核心目标

| 目标 | 说明 |
|---|---|
| **一次性评估** | 用户带着 idea 来，走完 9 步分析 → decision memo（等价于 idea-validation 的 Validation workflow） |
| **持续积累** | 每次分析的竞品、市场、定价数据**不丢弃**，形成可查询的知识库 |
| **自主发现** | Agent 在 idle/sleep 期间主动扫描趋势、关联历史分析、发现新机会 |
| **个人化** | 分析结果与 user_profile 绑定，recommendation 随 founder tier 调整 |

### 1.2 超越 idea-validation-agents 的能力

| idea-validation-agents | Aman Startup |
|---|---|
| 用户触发 → 4 个 workflow | 用户触发 + **自主触发**（Cron/Webhook/Idle） |
| 单次分析，文件持久化 | 单次分析 + **知识图谱积累** + 跨 idea 关联 |
| LLM 模拟研究（训练数据） | **真实工具调用**（info-hub 搜索 App Store/Reddit/TikTok） |
| 无行动闭环 | 分析结果 → **work item 创建** → team Kanban → agent 执行 |
| B2C App 专用 | B2C App + B2B SaaS + marketplace + 可扩展维度 |
| 单用户 | 多 agent 协作分析（并行评估不同维度） |

---

## 2. 从 idea-validation-agents 引入什么

### 2.1 完整引入：15 个评估维度的概念模型

这是 idea-validation-agents 最有价值的部分——不是代码，是经过验证的**领域知识结构**：

```
用户层 ─────────────────────────────────────────────────
  user-segmentation-profiler      ICP 分层（理想客户画像 tier）
  user-background-interviewer     深度背景采集 → user_profile

信号层 ─────────────────────────────────────────────────
  trend-analysis                  多平台趋势扫描（TikTok/Reddit/AppStore/Google）
  trend-to-product-mapper         趋势 → 产品机会映射

分析层（每个 idea 独立）─────────────────────────────────
  desire-evaluator                心理欲望评分（5 维度：survival/status/belonging/control/curiosity）
  competitor-mapper               竞品四象限（direct/indirect/substitute/emerging）+ 评论挖掘
  pricing-and-wtp                 Van Westendorp 价格敏感度 + 欲望溢价系数
  cac-modeler                    按渠道获客成本建模
  tam-sam-som-builder            三角验证自底向上市场估算（搜索量/社区代理/竞品收入）
  distribution-analysis           6 种病毒循环 k-factor + ASO 5-factor + 创作者经济适配
  retention-predictor             留存与流失预测
  complexity-assessment           构建难度评估

决策层 ─────────────────────────────────────────────────
  weakness-detection              弱点检测 + 根因分类（structural/situational/knowledge-gap/addressable）
  idea-scoring                    乘法地板算法（一个灾难性弱点 → 整体归零）+ RAT 设计
  pivot-engine                    只变 1-2 变量的 pivot 方案 + 重新评分
  decision-memo                   裁决 pursue/test/pivot/drop + pre-mortem + kill criteria
```

### 2.2 引入但不照搬

| 引入的 | 不照搬的原因 |
|---|---|
| 评估维度分类法 | — |
| 乘法地板算法 `floor_penalty *= (d_i / 25)` | — |
| RAT（Riskiest Assumption Test）设计模板 | — |
| Pre-mortem（Klein, 2007）方法论 | — |
| Van Westendorp 定价 + 欲望溢价系数 | — |
| 竞品四象限分类（含 substitute/emerging） | 这是该领域最被低估的洞察 |
| 5-factor 市场饱和度评分 | — |
| 根因分类法（structural/situational/knowledge-gap/addressable） | — |
| 双格式输出（JSON 机器 + Markdown 人类） | — |

| 不引入的 | 替代方案 |
|---|---|
| SKILL.md 纯 prompt 模板 | Aman Skill 系统（YAML 定义 + 可执行代码 + Tantivy 索引） |
| Orchestrator 中介文件传递 | EventBus 事件驱动 + 直接查询 |
| 文件系统作为唯一数据库 | 独立存储组件（SurrealDB 嵌入式 + YAML config） |
| `.claude/` `.codex/` `.cursor/` 三层适配 | Aman 是 runtime，不需要平台适配层 |
| LLM 模拟研究 | info-hub 真实 API 调用 |

---

## 3. 组件定位

### 3.1 为什么是独立组件而非 Kernel 能力

```
Kernel 层（kernel/core, kernel/event-bus, kernel/workflow, ...）
  └── 通用基础设施，所有 agent 共享

Cognitive 层（cognitive/engine, cognitive/llm）
  └── 认知引擎抽象，不绑定业务领域

Plugin 层（plugins/info-hub, plugins/memory-store, ...）
  └── 可插拔功能模块

Startup 组件 ← 定位在这里
  └── 特定领域（创业分析）的完整子系统
      有自己的技能、存储、workflow、触发规则
      独立部署、独立配置、独立演进
```

**依据：** Team 组件已经证明了这个模式——`predefined/plugins/team/` 是一个独立运行的 Python 进程，有自己的 SQLite 数据库、自己的 Kanban 逻辑、自己的 HTTP 路由。Startup 组件遵循相同的架构哲学：

| 维度 | Team 组件 | Startup 组件（设计） |
|---|---|---|
| **隔离模式** | subprocess (Python 3.11+) | subprocess (Python 3.11+) |
| **独立存储** | `~/.aman/team/` (SQLite + YAML + JSONL) | `~/.aman/startup/` (SurrealDB 嵌入式 + YAML) |
| **数据库** | Python `sqlite3`（内置） | [`surrealdb.py`](https://github.com/surrealdb/surrealdb.py) (`surrealkv://`) |
| **通信协议** | JSON-RPC 2.0 over stdin/stdout | JSON-RPC 2.0 over stdin/stdout |
| **配置入口** | `plugin.yaml` | `plugin.yaml` |
| **HTTP 路由** | `/team/*` | `/startup/*` |
| **事件命名空间** | `team:*` | `startup:*` |
| **Skill 导出** | 无（纯调度） | 15 个评估 skill |

### 3.2 选择 Subprocess (Python) 而非 In-Process (Rust)

两轮权衡的结果：

**最初倾向 Rust in-process**，原因：LLM 调用密集、工具调用密集、KG 操作频繁，希望低延迟和类型安全。

**最终选择 Python subprocess**，决定因素是 **SurrealDB**：

- **SurrealDB Python SDK 嵌入式模式开箱即用** — `Surreal("surrealkv://data/startup")`，文档 + 图查询原生支持，不需要 40 张规范化表
- **如果选 Rust in-process + SurrealDB**，Rust 嵌入式后端（`SurrealKV`）的 API 稳定性待验证，且会给 gateway 二进制引入重量级依赖；如果用 Rust in-process + SQLite，则要面对 JSON 列或 40 张表的规范化痛苦
- **Python subprocess 模式已被 Team 插件充分验证** — JSON-RPC bridge、HTTP 路由注册、事件订阅、capability 安全模型全部现成可复用
- **Startup 组件的计算特征适合 subprocess** — 主要工作是编排 LLM 调用（通过 JSON-RPC 调 gateway 的 CognitiveEngine）和文档存储（本地 SurrealDB），不涉及高频实时操作；一次 JSON-RPC 序列化往返相对于 LLM 推理的秒级延迟可以忽略
- **Schema-less 的实用价值** — 评估维度会持续迭代（可能增加 B2B SaaS 维度、监管风险维度），SurrealDB 无 schema + Python 无编译 = 随时加字段，不需要 migration

**trade-off 总结：**

| 维度 | Rust in-process + SQLite | Python subprocess + SurrealDB（选择） |
|---|---|---|
| LLM 调用延迟 | 直接函数调用 | JSON-RPC 桥接（~1ms，可忽略 vs LLM 秒级延迟） |
| 文档存储 | 需规范化 40+ 表或 JSON 列 | 原生文档，直接映射评估 JSON schema |
| 图查询 | 手动边表 JOIN | SurrealQL `→` `←` 原生语法 |
| 依赖管理 | Cargo（重编译 gateway） | `pip install surrealdb` |
| Schema 演进 | Rust struct + SQL migration | 无 schema，随时加字段 |
| 开发速度 | 编译-测试循环 | 改 Python 即生效 |
| 部署 | 编译进 gateway 二进制 | 独立 Python 进程，可热重载 |

---

## 4. 架构总览

```
                        ┌──────────────────────────────────────┐
                        │            Aman Gateway              │
                        │                                      │
   User Message ──────▶ │  Intent Detection                    │
                        │  ("evaluate" / "write landing page"   │
                        │   / "check my burnout" / "what if")   │
   Cron Source ──────▶ │  ("weekly trend + competitor scan")   │
                        │                                      │
                        │  ┌────────────────────────────────┐  │
                        │  │       Startup Component         │  │
                        │  │     (Python subprocess plugin)  │  │
                        │  │                                  │  │
                        │  │  ┌────────────────────────────┐  │  │
                        │  │  │     Workflow Orchestrator   │  │  │
                        │  │  │     (Gateway PipelineEngine) │  │  │
                        │  │  └────────────────────────────┘  │  │
                        │  │                                  │  │
                        │  │  ┌─ 评估层 ───────────────────┐  │  │
                        │  │  │ Signal → Analyze (parallel) │  │  │
                        │  │  │ → Decide → [Pivot]         │  │  │
                        │  │  │ 15 skills, 4 workflows     │  │  │
                        │  │  └────────────────────────────┘  │  │
                        │  │           │                      │  │
                        │  │           │ verdict = pursue/test │  │
                        │  │           ▼                      │  │
                        │  │  ┌─ 策略层 ───────────────────┐  │  │
                        │  │  │ landing-page-builder        │  │  │
                        │  │  │ gtm-narrative               │  │  │
                        │  │  │ pricing-page-optimizer      │  │  │
                        │  │  │ cold-outreach-designer      │  │  │
                        │  │  │ 4 skills                    │  │  │
                        │  │  └────────────────────────────┘  │  │
                        │  │           │                      │  │
                        │  │           │ 发布后在执行中反馈     │  │
                        │  │           ▼                      │  │
                        │  │  ┌─ 执行层 ───────────────────┐  │  │
                        │  │  │ mvp-scope-negotiator        │  │  │
                        │  │  │ competitive-radar           │  │  │
                        │  │  │ user-feedback-synthesizer   │  │  │
                        │  │  │ 3 skills                    │  │  │
                        │  │  └────────────────────────────┘  │  │
                        │  │           │                      │  │
                        │  │           │ 长期运行，数据积累     │  │
                        │  │           ▼                      │  │
                        │  │  ┌─ 反思层 ───────────────────┐  │  │
                        │  │  │ founder-decision-journal    │  │  │
                        │  │  │ ikigai-alignment-check      │  │  │
                        │  │  │ burnout-early-warning       │  │  │
                        │  │  │ 3 skills                    │  │  │
                        │  │  └────────────────────────────┘  │  │
                        │  │           │                      │  │
                        │  │           │ 任意阶段可触发         │  │
                        │  │           ▼                      │  │
                        │  │  ┌─ AI-Native 层 ────────────┐  │  │
                        │  │  │ what-if-simulator          │  │  │
                        │  │  │ cofounder-compatibility    │  │  │
                        │  │  │ 2 skills                   │  │  │
                        │  │  └────────────────────────────┘  │  │
                        │  │                                  │  │
                        │  │  Output → SurrealDB + EventBus   │  │
                        │  └────────────────────────────────┘  │
                        │                                      │
                        │  ┌────────────────────────────────┐  │
                        │  │       Autonomous Layer          │  │
                        │  │                                  │  │
                        │  │  TrendWatcher (Cron: weekly)     │  │
                        │  │  IncubationBridge (Idle: cross) │  │
                        │  │  MarketMonitor (Webhook: apps)  │  │
                        │  └────────────────────────────────┘  │
                        │                                      │
                        │  ┌────────────────────────────────┐  │
                        │  │         Startup UI              │  │
                        │  │   /startup/ (HTML + Alpine.js)  │  │
                        │  │   左侧分层导航 + 右侧模块内容     │  │
                        │  └────────────────────────────────┘  │
                        └──────────────────────────────────────┘
```

---

## 5. Skill 映射

> Startup 组件共 **24 个 Skill**，按创业生命周期分为五层。评估层是基础，上层依赖下层的数据积累。

### 5.1 评估层（15 个 Skill）

继承自 [idea-validation-agents](https://github.com/MaxKmet/idea-validation-agents) 的完整评估框架。详见 §2.1。

#### Skill 定义示例

每个评估维度不再是一个 SKILL.md prompt 文件，而是一个 **Aman Skill**：

```yaml
# skills/startup/competitor-mapper/skill.yaml
name: competitor-mapper
version: "1.0"
category: startup
description: >
  Map competitive landscape across direct, indirect, substitute, and emerging
  categories. Includes review mining for positioning gaps and market saturation
  scoring.

input:
  required:
    - idea_slug: string
    - app_description: string
    - primary_keywords: [string]
  optional:
    - market_insights: [MarketInsight]

output:
  type: structured
  schema: CompetitorAnalysis
  tags: [startup, competitor, "idea:{idea_slug}"]

tools:
  - info-hub.search_app_store      # App Store 搜索
  - info-hub.search_web             # "best X apps" 文章
  - info-hub.fetch_product_hunt     # Product Hunt 搜索
  - cognitive-llm.analyze_text      # 评论挖掘（1星/3星/5星分类）

execution:
  engine: cognitive                 # 使用 CognitiveEngine 做推理
  prompt_pipeline: competitor_mapping  # 专用 prompt 模板
  max_tokens: 8000
  temperature: 0.3                   # 分析任务，低温度

depends_on:
  skills: [trend-analysis]           # 需要先有趋势数据
  files: []                          # 不依赖文件
```

### 5.1.1 评估层 Skill 列表

| # | Skill | 输入 | LLM 角色 | 工具调用 | 输出类型 |
|---|---|---|---|---|---|
| 1 | `trend-analysis` | niche + platforms | 综合多平台信号，判断趋势方向 | info-hub 搜索 | `TrendReport` |
| 2 | `trend-to-product-mapper` | trend_report + user_profile | 趋势 → 具体产品机会 | 无（纯推理） | `Vec<IdeaSeed>` |
| 3 | `user-segmentation-profiler` | user_background | ICP 分层 | 无（纯推理） | `UserProfile` |
| 4 | `user-background-interviewer` | 对话历史 | 结构化提取 | 无 | `UserProfile` |
| 5 | `desire-evaluator` | idea + user_profile | 5 维度欲望评分（1-5） | 无（纯推理） | `DesireScores` |
| 6 | `competitor-mapper` | idea + keywords | 竞品分类 + 评论挖掘 + 饱和度 | **info-hub 多源搜索** | `CompetitorAnalysis` |
| 7 | `pricing-and-wtp` | idea + competitors + desire | Van Westendorp 建模 | 竞品价格抓取 | `PricingModel` |
| 8 | `tam-sam-som-builder` | competitors + market_insights | 三角验证估算 | info-hub 搜索量查询 | `MarketSize` |
| 9 | `cac-modeler` | competitors + pricing | 按渠道 CAC 估算 | info-hub 广告成本数据 | `CacModel` |
| 10 | `distribution-analysis` | idea + competitors + user_profile | 6 种病毒循环 + ASO | 无（纯推理） | `DistributionModel` |
| 11 | `retention-predictor` | idea + desire + competitors | 留存预测 + 习惯强度 | 无（纯推理） | `RetentionModel` |
| 12 | `complexity-assessment` | idea | 构建难度评估 | 无（纯推理） | `ComplexityScore` |
| 13 | `weakness-detection` | 所有维度结果 | 根因分类（4 类） | 无（纯推理） | `Vec<Weakness>` |
| 14 | `idea-scoring` | 前 7 维度结果（至少 3） | 乘法地板算法 + RAT | 无（确定性计算*） | `ScoreResult` |
| 15 | `decision-memo` | scores + weaknesses + user_profile | 决策简报（~500 words） | 无（纯推理） | `DecisionMemo` |
| 16 | `pivot-engine` | scores + weaknesses | Pivot 方案生成 + 重新评分 | 无（纯推理） | `PivotOptions` |

> *`idea-scoring` 的乘法地板算法是确定性数学计算，不需要 LLM 推理。RAT 实验设计可以用 LLM 生成，但评分公式本身是纯函数。

### 5.1.2 评估层 Skill 依赖图

```
user-background-interviewer ──┐
                               ├── user_profile ─────────────┐
user-segmentation-profiler ───┘                              │
                                                             │
trend-analysis ──→ trend-to-product-mapper ──→ idea_seeds    │
                                                             │
        ┌────────────────────────────────────────────────────┤
        │                                                    │
        ▼                                                    │
   ┌─────────┐  ┌──────────────────┐  ┌──────────────┐     │
   │ desire  │  │ competitor-mapper │  │ tam-sam-som  │     │
   └────┬────┘  └────────┬─────────┘  └──────┬───────┘     │
        │                │                    │              │
        ▼                ▼                    ▼              │
   ┌─────────┐  ┌──────────────────┐  ┌──────────────┐     │
   │ pricing │  │ distribution     │  │ cac-modeler  │     │
   └────┬────┘  └────────┬─────────┘  └──────┬───────┘     │
        │                │                    │              │
        │    ┌───────────┴──────────┐         │              │
        │    │ retention-predictor  │         │              │
        │    └───────────┬──────────┘         │              │
        │                │                    │              │
        ▼                ▼                    ▼              │
   ┌────────────────────────────────────────────────┐      │
   │              weakness-detection                 │      │
   └────────────────────┬───────────────────────────┘      │
                        │                                   │
                        ▼                                   │
   ┌────────────────────────────────────────────────┐      │
   │              idea-scoring (barrier)             │      │
   │         需要 Phase 2 至少 3/7 维度完成          │      │
   └────────────────────┬───────────────────────────┘      │
                        │                                   │
              ┌─────────┴─────────┐                        │
              ▼                   ▼                        │
   ┌──────────────────┐  ┌────────────────┐               │
   │  decision-memo   │  │  pivot-engine  │               │
   │  (≥55 分)        │  │  (<55 分)      │               │
   └──────────────────┘  └────────────────┘               │
```

**关键设计：Phase 2 的 7 个维度全部并行执行。** 它们互相独立，不存在先后依赖（desire 不需要知道 competitor 的结果）。这与 idea-validation 的顺序执行有本质区别——Aman 的 `PipelineEngine::Parallel` 可以让 7 个分析同时跑。

---

### 5.2 策略层（4 个 Skill）

> **触发条件：** 评估层 verdict = `pursue` 或 `test`。
> **核心命题：** 从"知道这个 idea 值得做"到"知道怎么把它卖出去"。

| # | Skill | 输入 | LLM 角色 | ⭐ |
|---|---|---|---|---|
| 17 | `landing-page-builder` | idea + competitors + desire + keywords | Hero 文案（3 角度）+ 社会证明策略 + A/B 计划 + SEO 关键词映射 + 差异化一句话 | ⭐⭐⭐⭐⭐ |
| 18 | `gtm-narrative` | idea + distribution + user_profile + competitors | Product Hunt 文案 + Reddit 社区×角度矩阵 + "Building in public" 30 天内容日历 + 冷邮件模板 + 种子文章大纲 | ⭐⭐⭐⭐⭐ |
| 19 | `pricing-page-optimizer` | pricing + competitors + desire | 定价心理学理由 + 竞品对比表 + 锚定策略 + FAQ + 退款保证文案 | ⭐⭐⭐⭐ |
| 20 | `cold-outreach-designer` | idea + user_profile + icp | 具体找人策略 + 个性化邮件模板 ×3 + 跟进序列 + 异议处理脚本 | ⭐⭐⭐⭐ |

#### landing-page-builder 详细设计

```yaml
# skills/startup/landing-page-builder/skill.yaml
name: landing-page-builder
version: "1.0"
category: startup/strategy
description: >
  Generate landing page copy, structure, and A/B test plan from validated
  idea analysis. Turn desire + competitor data into conversion copy.

input:
  required:
    - idea_slug: string
    - competitors_json: CompetitorAnalysis
    - desire_scores: DesireScores
  optional:
    - keywords_json: Keywords

output:
  schema: LandingPage
  stores_to: "landing_page:{idea_slug}"

execution:
  engine: cognitive
  temperature: 0.7             # 文案需要创意，比分析高
```

```python
# startup/skills/landing_page.py — 核心生成逻辑

@dataclass
class LandingPage:
    hero_variants: list[HeroVariant]     # 3 个角度
    social_proof_strategy: str           # 没用户时怎么说
    ab_test_plan: ABTestPlan
    seo_keywords: list[str]
    differentiator_oneliner: str         # "X but without the Y"

@dataclass
class HeroVariant:
    angle: str                  # "functional" | "desire" | "identity"
    headline: str
    subheadline: str
    cta: str
    expected_conversion: str    # "high for {icp_tier} users"

def build_landing_page(idea: dict, competitors: dict, desire: dict) -> LandingPage:
    """从竞品 gap + 欲望驱动 → 落地页文案"""
    # 核心逻辑：
    # 1. 从 positioning_gaps 提取最强差异化
    # 2. 从 desire_scores 提取 primary_driver（survival/status/belonging/...）
    # 3. 竞品 top_complaints → 你的优势文案
    # 4. 社会证明策略取决于 stage（pre-launch → "由 XX 背景的创始人打造"）
    ...
```

#### gtm-narrative 详细设计

```python
@dataclass
class GtmNarrative:
    product_hunt: ProductHuntLaunch      # 标题 + 副标题 + 首条评论 + GIF 脚本
    reddit_plan: list[SubredditAngle]    # 社区名 + 发帖角度 + 禁忌
    build_in_public: list[DailyPost]     # 30 天推特/LinkedIn 内容
    cold_email: ColdEmailKit             # B2B 场景模板
    content_seeds: list[ArticleOutline]  # 5 篇种子文章

@dataclass
class SubredditAngle:
    subreddit: str           # "r/climbing"
    angle: str               # "I built a habit tracker because I kept forgetting to log"
    taboo: str               # "DON'T: self-promotion without context"
    best_time: str           # "Tuesday 10am EST"

def build_gtm_narrative(idea: dict, distribution: dict, user: dict, competitors: dict) -> GtmNarrative:
    """分发分析 → 具体社区 × 角度矩阵"""
    # 核心逻辑：
    # 1. distribution.channels → 匹配具体 subreddit / PH / newsletter
    # 2. competitors.review_mining.top_complaints → 你的故事角度
    # 3. user_profile.icp_tier → 影响内容深度和专业度
    # 4. 每个分发渠道有"禁忌列表"（PH 不能刷票、Reddit 禁止裸营销）
    ...
```

---

### 5.3 执行层（3 个 Skill）

> **触发条件：** 产品发布后，或在开发过程中持续运行。
> **核心命题：** 在"打"的过程中持续校准方向。

| # | Skill | 输入 | LLM 角色 | ⭐ |
|---|---|---|---|---|
| 21 | `mvp-scope-negotiator` | idea + distribution + user_profile + rat_results | **魔鬼代言人** — 从 14 个 feature 砍到 3 个 + 功能优先级矩阵 + v0.1 边界清单 | ⭐⭐⭐⭐⭐ |
| 22 | `user-feedback-synthesizer` | 用户反馈流（邮件/评论/访谈） | 主题聚类 + 情感趋势 + 功能请求排名 + "用户说缺 X 但行为表明缺 Y" | ⭐⭐⭐⭐⭐ |
| 23 | `competitive-radar` | 历史 competitors snapshots + 新数据 | 变化检测 + 威胁评估 + 竞品动态摘要 | ⭐⭐⭐ |

#### mvp-scope-negotiator 详细设计

这是 LLM 最被低估的能力——**扮演魔鬼代言人**。创业者第一本能是加功能，LLM 应该反着来。

```python
@dataclass
class MvpScope:
    must_have: list[Feature]        # ≤3
    nice_to_have: list[Feature]
    explicitly_excluded: list[str]  # "User profiles", "Dark mode", ...
    priority_matrix: list[FeatureWithScore]  # impact × effort × risk
    rat_for_mvp: RatExperiment      # 最小可行实验
    boundary_statement: str         # "v0.1 只做 X，不做 Y 和 Z"

def negotiate_mvp(idea: dict, distribution: dict, user: dict, rat_results: dict) -> MvpScope:
    """对抗性范围压缩：站在'你的用户不需要这个'的立场争论"""
    # 核心逻辑：
    # 1. 从 idea 的 feature list 出发
    # 2. 对每个 feature 问："你的 RAT 数据支持这个吗？"
    # 3. 对每个 feature 问："竞品有这个吗？用户因此离开竞品了吗？"
    # 4. 只保留"用户真正切换的理由"级别的 feature
    # 5. v0.1 就是能跑完一个最小实验就够
    ...
```

#### user-feedback-synthesizer 详细设计

```python
@dataclass
class FeedbackSynthesis:
    topic_clusters: list[TopicCluster]      # "12 个用户中有 7 个提到了 onboarding"
    sentiment_trends: list[SentimentTrend]  # "pricing 负面反馈 3 周内增加 3x"
    feature_requests: list[FeatureRequest]  # 按频率 × 情感强度 × 实现成本排序
    latent_needs: list[str]                 # "用户说缺 X，但行为表明真正缺的是 Y"
    competitive_gap_check: str              # "你在竞品 review 中看到的 gap，你的用户也在抱怨吗？"

class FeedbackSource(str, Enum):
    APP_STORE_REVIEW = "app_store_review"
    REDDIT = "reddit"
    CUSTOMER_EMAIL = "customer_email"
    USER_INTERVIEW = "user_interview"
    SURVEY = "survey"
```

---

### 5.4 反思层（3 个 Skill）

> **触发条件：** 定期（月度/季度）+ 关键事件后（pivot、launch、cofounder 变动）。
> **核心命题：** 创业者的精神层面——你在做什么、为什么做、你还好吗。

| # | Skill | 输入 | LLM 角色 | ⭐ |
|---|---|---|---|---|
| 24 | `founder-decision-journal` | 历史 decision_memo + scores + pivot 记录 | 认知偏差检测（系统性乐观偏差、锚定效应）+ 决策质量趋势 + "你上次这个假设还 hold 吗？" | ⭐⭐⭐⭐ |
| 25 | `ikigai-alignment-check` | user_profile + 全部 idea 评估 + 决策日志 + 情绪数据 | 四个圆的交集分析 + 矛盾检测 + "你在追的东西和你在乎的东西不一致" | ⭐⭐⭐⭐ |
| 26 | `burnout-early-warning` | work item completion + session 频率 + 决策质量趋势 | 模式识别（3 周 productivity 下降 40% → 倦怠信号）+ 干预建议 | ⭐⭐ |

#### ikigai-alignment-check 详细设计

这是你提到的"财务追求 + 工作 + 精神追求"的直译：

```python
@dataclass
class IkigaiProfile:
    what_you_love: list[str]       # 从 user_profile + 对话提取
    what_you_are_good_at: list[str]
    what_world_needs: list[str]    # 从趋势 + market_insights 提取
    what_you_can_be_paid_for: list[str]  # 从 pricing + tam 数据提取

@dataclass
class IkigaiCheck:
    alignment_score: float          # 0-100
    overlapping_quadrants: list[str]  # ["能赚钱的", "你擅长的"]
    missing_quadrant: str             # "你热爱的" — 缺失的圆
    contradiction: str  # "你 3 次 pivot 都向更赚钱的方向转，但没有一次更符合你的价值观"
    suggested_adjustment: str  # 具体的调整建议

def check_ikigai(
    profile: UserProfile,
    all_ideas: list[IdeaAnalysis],
    decisions: list[DecisionRecord],
    sentiment_data: list[SentimentPoint],
) -> IkigaiCheck:
    """四个圆交集分析 — 你的创业方向对齐了哪几个？缺失了哪个？"""
    # 核心逻辑：
    # 1. 从 user_profile 提取"你热爱/擅长"的
    # 2. 从 market_insights 提取"世界需要的"
    # 3. 从 pricing + tam 验证"能赚钱的"
    # 4. 检查所有 idea 在哪个象限 → 画交集图
    # 5. 特别关注矛盾："你说你热爱 X，但你评估的 idea 全部是 Y"
    ...
```

#### founder-decision-journal 详细设计

```python
@dataclass
class DecisionEntry:
    decision: str                   # "把定价从 $9/mo 改成 $4.99/mo"
    info_at_the_time: str           # 做决策时拥有的信息
    assumptions: list[str]          # 隐含假设
    expected_outcome: str
    actual_outcome: Optional[str]   # 3 个月后填写
    was_correct: Optional[bool]

@dataclass
class BiasDetection:
    systematic_optimism: float      # 0-1，"你过去 4 个决策有 3 个高估了增长速度"
    anchoring: str                  # "你保留了 6 个月前的 pivot 假设，但数据已变"
    confirmation_bias: str          # "你 5 次找竞品弱点，却从未更新自己 idea 的弱点"
    decision_quality_trend: str     # "up" | "stable" | "declining"

def audit_decisions(journal: list[DecisionEntry]) -> BiasDetection:
    """定期审计：你的决策模式有什么系统性偏差？"""
    ...
```

---

### 5.5 AI-Native 层（2 个 Skill）

> **触发条件：** 任意阶段，用户主动触发。
> **核心命题：** 只有 LLM 才能做的独特推理。

| # | Skill | 输入 | LLM 角色 | ⭐ |
|---|---|---|---|---|
| 27 | `what-if-simulator` | 当前 idea 全部分析数据 | 多场景推演 + 连锁效应 + 概率加权结果 + 历史对比 | ⭐⭐⭐⭐ |
| 28 | `cofounder-compatibility` | 两个 user_profile | 技能互补矩阵 + 价值观对齐 + 决策风格差异 + 盲点重叠区 + 合作建议 | ⭐⭐⭐ |

#### what-if-simulator 详细设计

```python
@dataclass
class WhatIfScenario:
    question: str                   # "如果 Apple 在 iOS 里加了这个功能？"
    affected_dimensions: list[str]  # ["competition", "distribution", "retention"]
    cascade: list[str]  # ["distinctiveness → 0", "pricing power → 0", "→ pivot or drop"]
    best_case: str
    most_likely: str
    worst_case: str
    historical_reference: str       # "上次你做类似判断时，实际结果是..."

def simulate_what_if(
    idea: IdeaAnalysis,
    question: str,
    history: list[DecisionEntry],
) -> WhatIfScenario:
    """假设推演：改变一个变量，连锁影响所有维度"""
    # 核心逻辑：
    # 1. 解析问题 → 确定受影响维度
    # 2. 对每个维度推演直接 + 间接影响
    # 3. 参考历史决策数据（"你上次类似判断的实际结果"）
    # 4. 给出概率估计（不是点估计）
    ...
```

---

### 5.6 Skill 依赖全图

```
┌────────────────────────────────────────────────────────────┐
│                       评估层（基础）                         │
│  user-profile ──→ trend ──→ idea-seeds                     │
│                      ──→ 7-dimension parallel analyze      │
│                      ──→ scoring ──→ decision              │
│                          verdict = pursue / test            │
└──────────────────────────┬─────────────────────────────────┘
                           │
        ┌──────────────────┼──────────────────┐
        ▼                  ▼                  ▼
┌───────────────┐  ┌───────────────┐  ┌───────────────┐
│   策略层       │  │   执行层       │  │   反思层       │
│               │  │               │  │               │
│ landing-page  │  │ mvp-scope     │  │ decision-     │
│ gtm-narrative │  │ user-feedback │  │ journal       │
│ pricing-page  │  │ competitive-  │  │ ikigai-check  │
│ cold-outreach │  │ radar         │  │ burnout-warn  │
│               │  │               │  │               │
│ 依赖：评估层   │  │ 依赖：评估层   │  │ 依赖：全部层   │
│ 评估 + 竞品   │  │ + 用户反馈流  │  │ 历史数据积累   │
└──────┬────────┘  └──────┬────────┘  └──────┬────────┘
       │                  │                  │
       └──────────────────┼──────────────────┘
                          │
           ┌──────────────┴──────────────┐
           │        AI-Native 层          │
           │  what-if-simulator           │
           │  cofounder-compatibility     │
           │                              │
           │  依赖：所有下层数据            │
           │  触发：用户主动               │
           └──────────────────────────────┘
```

**层间数据流：** 下层积累的数据是上层的输入。评估层的 `CompetitorAnalysis` → 策略层的 `landing-page-builder` 直接引用竞品 gap 生成文案。执行层的 `user-feedback-synthesizer` 结果 → 反思层的 `ikigai-check` 用来判断"你在做的是你想做的吗？"

---

## 6. 数据流：一次完整评估

### 6.1 触发路径

```
用户输入："帮我评估一个 habit tracker for climbers 的 idea"
     │
     ▼
Intent Detection (gateway)
     │
     ▼
CognitiveEngine::process()
     │  产生 Decision::DelegateToSkill("startup:validate")
     │
     ▼
StartupComponent::validate(idea_slug, raw_description)
     │
     ▼
WorkflowEngine::execute(ValidationWorkflow)
```

### 6.2 Workflow 定义

Workflow 由 Startup 插件在启动时通过 JSON-RPC 注册到 Gateway 的 `PipelineEngine`：

```python
# startup/workflow.py — 插件侧 workflow 注册

VALIDATION_WORKFLOW = {
    "name": "startup:validate",
    "stages": [
        # Phase 1: 信号采集（串行——trend-to-product 依赖 trend-analysis）
        {"name": "signal", "mode": "serial", "skills": [
            "trend-analysis",
            "trend-to-product-mapper",
        ]},
        # Phase 2: 多维度并行分析（7 个维度互不依赖，全并行）
        {"name": "analyze", "mode": "parallel", "skills": [
            "desire-evaluator",
            "competitor-mapper",
            "pricing-and-wtp",
            "tam-sam-som-builder",
            "cac-modeler",
            "distribution-analysis",
            "retention-predictor",
            "complexity-assessment",
        ]},
        # Phase 3: 决策合成（串行——依赖全部 Phase 2 结果）
        {"name": "decide", "mode": "serial", "skills": [
            "weakness-detection",
            "idea-scoring",
            "decision-memo",
        ]},
        # Phase 4: 条件 pivot（仅当 verdict == "pivot" 时触发）
        {"name": "pivot", "mode": "conditional",
         "condition": {"field": "verdict", "op": "eq", "value": "pivot"},
         "skills": [
            "pivot-engine",
            "idea-scoring",     # re-score
            "decision-memo",    # re-generate
        ]},
    ]
}
```

Gateway 侧 `PipelineEngine` 负责实际的编排——Serial stages 顺序执行，Parallel stages 内所有 skill 同时调用（每个 skill 是一次 JSON-RPC 调用到 Python 插件的对应处理函数）。

### 6.3 数据如何流动（与 idea-validation 的对比）

```
idea-validation-agents:
  Skill A 写 memory/ideas/<slug>/file_a.json
  Orchestrator 读 file_a.json
  Orchestrator 把内容贴到 Skill B 的 prompt 里
  Skill B 写 memory/ideas/<slug>/file_b.json
  ...（串行，人工编排）

Aman Startup:
  Skill A 完成 → 发布 startup:analyzed.desire 事件
  EventBus 通知 PipelineEngine
  Skill B 通过 WorkflowContext 直接读取 Skill A 的结构化输出
  ...（并行 + 自动编排 + EventBus 解耦）
```

**关键差异：** Aman 的 Skill 输出是 SurrealDB 中可查询的文档记录，不是扁平 JSON 文件。Skill B 通过 SurrealQL 直接查询 Skill A 的产出，不需要 orchestrator 中介传递。图关系（`idea→competitor`）是数据库的原生能力，不是额外的索引层。

---

## 7. Aman 独有能力：自主层

这是 idea-validation-agents **完全不具备**的能力，也是 startup 组件最大的差异化价值。

### 7.1 TrendWatcher — 持续趋势监控

```
CronSource ("0 9 * * 1")  ← 每周一早上 9 点
     │
     ▼
trend-analysis (niche = 用户关注的所有领域)
     │
     ▼
对比上次趋势快照 → 检测变化:
  - 新 rising-fast 趋势 → 通知用户
  - 现有趋势 velocity 下降 → 更新 market_insights
  - 新竞品出现 → 自动 competitor-mapper
     │
     ▼
startup:trend.alert 事件 → 用户通知
```

### 7.2 IncubationBridge — 跨领域孵化

复用 aman 的 `IncubationRunner`（`kernel/gateway/src/runtime/incubation_runner.rs`）：

```
IdleEvent (depth 200+)
     │
     ▼
IncubationRunner::cross_domain_sample()
     │
     ▼
从 StartupStore 加载所有历史分析
     │
     ▼
LLM 跨领域关联:
  "nutrition-coach 的 retention 模型和 habit-tracker 有相同的弱点"
  "freelance-invoice 的 distribution 策略可以迁移到 micro-saas-idea"
     │
     ▼
生成 InspirationSeed → 存储到 StartupStore
     │
     ▼
如果 inspiration_score > 阈值 → 自动生成新 idea → 通知用户
```

### 7.3 自主触发 vs 用户触发的分界

| 触发源 | 行为 | 用户参与 |
|---|---|---|
| 用户消息 | 完整评估流程 | 全程 |
| Cron (每周) | 趋势扫描 + 变化检测 | 仅通知 |
| Idle/Sleep | 思维整理 + 跨领域关联 | 无 |
| Webhook (App Store 新应用) | 竞品警报 | 仅通知 |
| Incubation (深度闲置) | 创意发现 | 通知 + 用户确认 |

**原则：** 自主层只做**分析和发现**，不做**决策**。`verdict = pursue` 永远是用户的选择。

---

## 8. 集成点

### 8.1 与现有 Aman 系统的集成

```
                    ┌──────────────────┐
                    │   Startup 组件    │
                    └────────┬─────────┘
                             │
        ┌────────────────────┼────────────────────┐
        │                    │                    │
        ▼                    ▼                    ▼
┌──────────────┐  ┌──────────────────┐  ┌──────────────────┐
│ CognitiveEngine│  │   MemoryProvider │  │    EventBus      │
│              │  │                  │  │                  │
│ • 每一步 LLM  │  │ • 评估结果存储    │  │ • startup:analyzed│
│   推理       │  │ • KG 边:         │  │ • startup:scored  │
│ • Prompt 模板 │  │   idea→competitor│  │ • startup:pivot   │
│ • 工具调用    │  │   idea→market    │  │ • startup:decided │
│   (Function)  │  │ • 语义搜索历史   │  │                  │
└──────────────┘  │   分析           │  └──────────────────┘
                  └──────────────────┘
                             │
        ┌────────────────────┼────────────────────┐
        │                    │                    │
        ▼                    ▼                    ▼
┌──────────────┐  ┌──────────────────┐  ┌──────────────────┐
│   Info-hub   │  │   Team 组件      │  │   通知系统        │
│              │  │                  │  │                  │
│ • App Store  │  │ • 评估完成 →     │  │ • RAT 实验到期   │
│   搜索       │  │   创建 work item │  │ • 趋势变化警报   │
│ • Reddit     │  │ • Kanban 追踪    │  │ • Kill criteria  │
│   抓取       │  │   RAT 实验       │  │   触发提醒       │
│ • Web 搜索   │  │ • 构建 MVP 任务  │  │                  │
└──────────────┘  └──────────────────┘  └──────────────────┘
```

### 8.2 具体集成细节

#### SurrealDB 嵌入式存储（Startup 组件的独立数据库）

```python
# startup/store.py — SurrealDB 嵌入式存储
from surrealdb import Surreal

class StartupStore:
    """Startup 组件的独立持久化层。嵌入式 SurrealDB，无需独立服务进程。"""

    def __init__(self, data_dir: str = "~/.aman/startup"):
        path = os.path.expanduser(f"{data_dir}/data")
        self.db = Surreal(f"surrealkv://{path}")
        self.db.use("startup", "ideas")

    # ── Idea CRUD ──────────────────────────────────────

    def create_idea(self, slug: str, idea: dict) -> dict:
        """创建新 idea 记录（candidate 状态）"""
        return self.db.create(f"idea:{slug}", {
            **idea,
            "status": "candidate",
            "created_at": datetime.now().isoformat(),
        })

    # ── 分析维度存储（直接映射 idea-validation 的 JSON schema）──

    def store_competitor_analysis(self, idea_slug: str, analysis: dict):
        """存储竞品分析结果。文档完整保留，不需要拆表。"""
        self.db.create(f"competitor_analysis:{idea_slug}", {
            "idea_slug": idea_slug,
            "direct_competitors": analysis["direct_competitors"],
            "indirect_competitors": analysis["indirect_competitors"],
            "substitutes": analysis["substitutes"],
            "emerging_threats": analysis["emerging_threats"],
            "positioning_gaps": analysis["positioning_gaps"],
            "saturation_score": analysis["saturation_score"],
            "market_saturation": analysis["market_saturation"],
            "reviewed_at": datetime.now().isoformat(),
        })

        # 建立图边: idea → competes_with → competitor
        for comp in analysis["direct_competitors"]:
            comp_id = self._normalize_id(comp["name"])
            # 确保 competitor 节点存在
            self.db.query(f"CREATE competitor:{comp_id} CONTENT $data", {
                "data": {"name": comp["name"], "platform": comp.get("platform")}
            })
            # 建边
            self.db.query(f"""
                RELATE idea:{idea_slug}->competes_with->competitor:{comp_id}
                SET strength = 'direct', discovered_at = $ts
            """, {"ts": datetime.now().isoformat()})

    # ── 跨 idea 图查询 ─────────────────────────────────

    def find_competing_ideas(self, competitor_name: str) -> list:
        """哪些 idea 都有同一个竞争对手？（图遍历）"""
        comp_id = self._normalize_id(competitor_name)
        return self.db.query(f"""
            SELECT *, <-competes_with<-idea.* AS ideas
            FROM competitor:{comp_id}
        """)

    def get_scored_ideas(self, verdict: str = None, min_score: float = 0) -> list:
        """跨 idea 条件查询 — SurrealQL 一行搞定"""
        where = []
        if verdict:
            where.append(f'verdict = "{verdict}"')
        if min_score:
            where.append(f'final_score >= {min_score}')
        clause = " AND ".join(where) if where else "true"
        return self.db.query(f"""
            SELECT slug, final_score, verdict, market_saturation,
                   ->competes_with->competitor.* AS competitors
            FROM idea
            WHERE {clause}
            ORDER BY final_score DESC
        """)

    # ── 评分快照（时间序列追踪）─────────────────────────

    def store_score_snapshot(self, idea_slug: str, scores: dict):
        """每次评分生成一个快照，可追溯分数变化历史"""
        self.db.create(f"score_snapshot:{idea_slug}", {
            "idea_slug": idea_slug,
            "dimension_scores": scores["dimension_scores"],
            "base_score": scores["base_score"],
            "floor_penalty": scores["floor_penalty"],
            "final_score": scores["final_score"],
            "verdict": scores["verdict"],
            "snapshot_at": datetime.now().isoformat(),
        })
        # 同步更新 idea 主记录的当前状态
        self.db.query(f"""
            UPDATE idea:{idea_slug} MERGE {{
                status: 'scored',
                final_score: {scores["final_score"]},
                verdict: '{scores["verdict"]}'
            }}
        """)

    # ── 辅助 ───────────────────────────────────────────

    def _normalize_id(self, name: str) -> str:
        """名称 → SurrealDB record ID"""
        return name.lower().replace(" ", "-").replace("'", "")
```

#### CognitiveEngine 调用（通过 JSON-RPC bridge 调 Gateway）

```python
# startup/analyzer.py — 每个评估 skill 通过 JSON-RPC 调 Gateway 的 CognitiveEngine

async def run_competitor_mapping(idea_slug: str, idea: dict, bridge: JsonRpcBridge):
    """调用 Gateway CognitiveEngine 做竞品分析"""
    # 通过 JSON-RPC 请求 LLM 推理
    result = await bridge.request("aman.cognitive.process", {
        "agent_id": "startup-agent",
        "observation": {
            "type": "user_message",
            "content": f"Analyze competitors for: {idea['description']}",
        },
        "context": {
            "evaluation_dimension": "competitor_mapping",
            "idea_slug": idea_slug,
            "prior_market_insights": idea.get("market_insights", []),
        },
        "tools": ["info-hub.search_app_store", "info-hub.search_web"],
    })
    return result["output"]  # 结构化竞品分析结果
```

#### EventBus 事件发布

```python
# 每个阶段完成发布事件（通过 JSON-RPC bridge）
await bridge.request("aman.event.publish", {
    "event": "startup:analyzed.competitor",
    "data": {
        "idea_slug": idea_slug,
        "competitor_count": len(analysis["direct_competitors"]),
        "saturation_score": analysis["saturation_score"]["total"],
        "strongest_gap": analysis.get("strongest_gap_signal"),
    }
})

# 评估完成 → Team 组件据此创建 work item
await bridge.request("aman.event.publish", {
    "event": "startup:decided",
    "data": {
        "idea_slug": idea_slug,
        "verdict": scores["verdict"],
        "final_score": scores["final_score"],
        "rat_experiment": scores.get("rat_experiment"),
    }
})
```

#### Gateway 侧集成到 YantrikDB MemoryProvider

```python
# startup/memory_bridge.py — 关键分析结果同步到 agent 长期记忆

async def sync_to_longterm_memory(idea_slug: str, store: StartupStore, bridge: JsonRpcBridge):
    """将 SurrealDB 中的分析结果同步到 Gateway 的 YantrikDB，进入 agent 长期记忆"""
    # 从 SurrealDB 加载竞品分析
    analysis = store.db.select(f"competitor_analysis:{idea_slug}")

    # 通过 JSON-RPC 存入 YantrikDB
    await bridge.request("aman.memory.store", {
        "agent_id": "startup-agent",
        "record": {
            "content": json.dumps(analysis),
            "tags": ["startup", "competitor", f"idea:{idea_slug}"],
            "domain": "startup",
            "importance": 0.8,
        }
    })

    # 同步图边到 YantrikDB KG
    for comp in analysis.get("direct_competitors", []):
        await bridge.request("aman.memory.relate", {
            "from": f"idea:{idea_slug}",
            "relation": "competes_with",
            "to": f"competitor:{comp['name']}",
        })
```

#### Team 组件集成

```
startup:decided (verdict = "test", rat_experiment = {...})
     │
     ▼
Team Plugin 监听此事件
     │
     ▼
自动创建 work item:
  project: "startup-experiments"
  stage: "design"
  title: "RAT: {rat_experiment.description}"
  context: {rat 实验细节 + pass/fail threshold}
  safety_gates: ["cost ≤ ${rat_experiment.estimated_cost}"]
     │
     ▼
Agent dispatch → 执行实验 → 结果回写 StartupStore (SurrealDB)
```

#### 通知系统

```python
# RAT 实验到期提醒（Gateway Cron 驱动，查询 Startup SurrealDB）
# Cron: "0 9 * * *" 每天检查
async def check_rat_deadlines(store: StartupStore, bridge: JsonRpcBridge):
    results = store.db.query("""
        SELECT slug, rat_experiment
        FROM score_snapshot
        WHERE verdict = 'test'
          AND rat_experiment.deadline < time::now()
    """)
    for row in results:
        await bridge.request("aman.notification.send", {
            "message": f"RAT experiment for {row['slug']} is overdue. Kill criteria met?"
        })
```

---

## 9. 与 Team 组件的并行设计

### 9.1 架构对称性

```
~/.aman/
  team/                              startup/
  ├── config.yaml                    ├── config.yaml
  ├── projects/                      ├── data/               ← SurrealDB 文件存储
  │   └── <key>/                     │   └── ...              (surrealkv://)
  │       ├── config.yaml            │
  │       ├── data.db  (SQLite)      ├── templates/           ← HTTP 页面模板
  │       └── works/                 │   ├── idea-list.html
  │           └── <id>.jsonl         │   └── idea-detail.html
  │                                  │
  └── ...                            └── static/
                                         └── startup.css
```

**关键差异：** Team 用 SQLite（每 project 一个 db + JSONL 日志），Startup 用 SurrealDB 嵌入式（全局单实例，`surrealkv://` 文件，文档 + 图双模型统一存储）。

### 9.2 组件对比

| | Team | Startup |
|---|---|---|
| **核心抽象** | Project → Stage → Work Item | Idea → Evaluation → Decision |
| **状态机** | Kanban stage transitions | `candidate → in_validation → scored → active → paused → dropped` |
| **存储引擎** | SQLite (`sqlite3`) | SurrealDB 嵌入式 (`surrealdb.py`, `surrealkv://`) |
| **数据模型** | 关系表（works, stage_history, safety_log） | 文档 + 图（idea, competitor_analysis, score_snapshot, edges） |
| **查询方式** | SQL JOIN | SurrealQL (文档查询 + 图遍历) |
| **Schema** | 有（需 CREATE TABLE + migration） | 无（文档 schema-less） |
| **事件日志** | JSONL per work item | 无需（score_snapshot 表即时间序列） |
| **安全模型** | Safety gates (rm -rf, low confidence) | Validation gates (missing data, low confidence, stale data) |
| **HTTP 路由** | `/team/{project}/kanban` | `/startup/ideas`, `/startup/ideas/{slug}` |
| **调度** | Agent dispatch for work items | Workflow execution (Serial/Parallel/Conditional) |
| **自主运行** | ❌ 纯被动调度 | ✅ TrendWatcher + IncubationBridge |

---

## 10. UI 设计

### 10.1 布局总览

```
┌──────────────────────────────────────────────────────────────┐
│  /startup/                                                   │
│  ┌─────────────┬────────────────────────────────────────────┐│
│  │ 左侧导航     │  右侧内容区                                 ││
│  │ (240px)     │                                            ││
│  │             │  ┌──────────────────────────────────────┐  ││
│  │ ▸ 评估层    │  │  当前模块的完整内容                     │  ││
│  │   idea-gen  │  │                                      │  ││
│  │   validate  │  │  - Skill 触发按钮                      │  ││
│  │   market-   │  │  - 结果展示（表格/图表/Markdown）       │  ││
│  │   deepdive  │  │  - 历史记录                            │  ││
│  │   pivot     │  │  - 操作区（下一步行动）                 │  ││
│  │             │  │                                      │  ││
│  │ ▸ 策略层    │  └──────────────────────────────────────┘  ││
│  │   landing   │                                            ││
│  │   gtm       │                                            ││
│  │   pricing-  │                                            ││
│  │   page      │                                            ││
│  │   outreach  │                                            ││
│  │             │                                            ││
│  │ ▸ 执行层    │                                            ││
│  │   mvp-scope │                                            ││
│  │   feedback  │                                            ││
│  │   radar     │                                            ││
│  │             │                                            ││
│  │ ▸ 反思层    │                                            ││
│  │   journal   │                                            ││
│  │   ikigai    │                                            ││
│  │   burnout   │                                            ││
│  │             │                                            ││
│  │ ▸ AI-Native │                                            ││
│  │   what-if   │                                            ││
│  │   cofounder │                                            ││
│  └─────────────┴────────────────────────────────────────────┘│
└──────────────────────────────────────────────────────────────┘
```

### 10.2 技术选型：Alpine.js，不是 Svelte

**结论：不需要 Svelte。用 Alpine.js + 服务端渲染 HTML 即可。**

| 考量 | Svelte | Alpine.js（选择） |
|---|---|---|
| **构建步骤** | 需要 Node.js + npm build | 无，CDN 加载（~15KB） |
| **复杂度匹配** | 组件框架，适合 SPA | 轻量响应式，适合增强型页面 |
| **与插件集成** | 需单独构建、部署构建产物 | 一个 `<script>` 标签即可 |
| **Python 模板** | 需前后端分离 | 直接嵌入 Jinja2 模板 |
| **团队已有模式** | 无 | 无（但 team 插件是纯 HTML，Alpine 是自然升级） |
| **这个页面的需求** | 过重 | **刚好** — 左侧导航折叠/展开 + 右侧内容切换 |

**选择 Alpine.js 的理由：**

1. **导航交互极其简单** — 点击一级菜单展开/折叠，点击二级菜单切换右侧内容。这不是 SPA，只是带一点 JS 的 HTML 页面。
2. **零构建步骤** — `<script src="https://cdn.jsdelivr.net/npm/alpinejs@3.x.x/dist/cdn.min.js" defer></script>` 一行搞定
3. **与 Python 模板天然共存** — Jinja2 渲染 HTML 结构，Alpine.js 负责交互，各管各的
4. **团队插件已验证无构建流程** — Team 插件用纯 HTML + 一个 `chat-input.js`。Alpine.js 只是让交互更优雅，不改变部署模式
5. **Svelte 留下升级空间** — 如果未来交互复杂度真的超过 Alpine.js 的舒适区（比如需要复杂的状态管理、图表库深度集成），迁移到 Svelte 不难。但先不支付这个成本

### 10.3 页面结构

```python
# startup/routes.py — HTTP 路由注册

# 主页面（左侧导航 + 右侧内容框架）
register_route("GET",  "/startup",              "templates/startup-index.html")

# 各模块页面（右侧内容区，通过 HTMX 或 Alpine fetch 加载）
register_route("GET",  "/startup/evaluate",     "templates/evaluate/validate.html")
register_route("GET",  "/startup/evaluate/<slug>", "templates/evaluate/idea-detail.html")
register_route("GET",  "/startup/strategy/landing-page/<slug>", "templates/strategy/landing-page.html")
register_route("GET",  "/startup/strategy/gtm/<slug>", "templates/strategy/gtm.html")
register_route("GET",  "/startup/execution/mvp-scope/<slug>", "templates/execution/mvp-scope.html")
register_route("GET",  "/startup/execution/feedback/<slug>", "templates/execution/feedback.html")
register_route("GET",  "/startup/execution/radar/<slug>", "templates/execution/radar.html")
register_route("GET",  "/startup/reflection/journal", "templates/reflection/journal.html")
register_route("GET",  "/startup/reflection/ikigai", "templates/reflection/ikigai.html")
register_route("GET",  "/startup/reflection/burnout", "templates/reflection/burnout.html")
register_route("GET",  "/startup/ai-native/what-if/<slug>", "templates/ai-native/what-if.html")
register_route("GET",  "/startup/ai-native/cofounder", "templates/ai-native/cofounder.html")

# API endpoints（返回 JSON → Alpine.js 渲染）
register_route("POST", "/api/startup/validate",       "handle_validate")
register_route("POST", "/api/startup/build-landing",  "handle_landing_page")
register_route("POST", "/api/startup/simulate-what-if","handle_what_if")
# ... 其他 API
```

### 10.4 核心模板结构

```html
<!-- templates/startup-index.html -->
<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="UTF-8">
  <title>Startup — Aman</title>
  <link rel="stylesheet" href="/static/startup.css">
  <script src="https://cdn.jsdelivr.net/npm/alpinejs@3.x.x/dist/cdn.min.js" defer></script>
</head>
<body x-data="startupNav()" class="startup-layout">
  <!-- 左侧导航 -->
  <nav class="sidebar">
    <div class="sidebar-header">
      <h2>Startup</h2>
      <span class="idea-count" x-text="activeIdeas + ' active'"></span>
    </div>

    <template x-for="layer in layers" :key="layer.name">
      <div class="nav-layer">
        <!-- 一级：层名（可折叠） -->
        <button class="nav-layer-btn"
                @click="toggle(layer.name)"
                :aria-expanded="isOpen(layer.name)">
          <span class="layer-icon" x-text="layer.icon"></span>
          <span class="layer-label" x-text="layer.label"></span>
          <span class="layer-chevron" x-show="isOpen(layer.name)">▾</span>
          <span class="layer-chevron" x-show="!isOpen(layer.name)">▸</span>
        </button>

        <!-- 二级：模块列表 -->
        <ul x-show="isOpen(layer.name)" class="nav-modules">
          <template x-for="mod in layer.modules" :key="mod.id">
            <li>
              <a :href="mod.url"
                 :class="{ active: currentModule === mod.id }"
                 @click.prevent="navigate(mod)">
                <span class="module-icon" x-text="mod.icon"></span>
                <span x-text="mod.label"></span>
                <span x-show="mod.badge" class="badge" x-text="mod.badge"></span>
              </a>
            </li>
          </template>
        </ul>
      </div>
    </template>
  </nav>

  <!-- 右侧内容区 -->
  <main class="content" x-html="currentContent">
    <!-- 通过 Alpine fetch 加载当前模块的 HTML -->
  </main>
</body>
</html>
```

```javascript
// static/startup.js — Alpine.js 数据 + 逻辑
function startupNav() {
  return {
    openLayers: ['evaluate'],  // 默认展开评估层
    currentModule: null,
    currentContent: '',

    layers: [
      {
        name: 'evaluate', label: '评估层', icon: '🔍',
        modules: [
          { id: 'validate', label: 'Idea 评估', url: '/startup/evaluate',
            icon: '📊', badge: null },
          { id: 'idea-gen', label: 'Idea 生成', url: '/startup/evaluate/generate',
            icon: '💡', badge: null },
          { id: 'market', label: '市场深潜', url: '/startup/evaluate/market',
            icon: '🌊', badge: null },
        ]
      },
      {
        name: 'strategy', label: '策略层', icon: '🎯',
        modules: [
          { id: 'landing-page', label: '落地页生成', url: '/startup/strategy/landing-page',
            icon: '📄', badge: 'new' },
          { id: 'gtm', label: '增长叙事', url: '/startup/strategy/gtm',
            icon: '📣', badge: null },
          { id: 'pricing-page', label: '定价页优化', url: '/startup/strategy/pricing-page',
            icon: '💰', badge: null },
          { id: 'outreach', label: '冷启动外展', url: '/startup/strategy/outreach',
            icon: '✉️', badge: null },
        ]
      },
      {
        name: 'execution', label: '执行层', icon: '⚡',
        modules: [
          { id: 'mvp-scope', label: 'MVP 范围', url: '/startup/execution/mvp-scope',
            icon: '✂️', badge: null },
          { id: 'feedback', label: '用户反馈', url: '/startup/execution/feedback',
            icon: '🗣️', badge: null },
          { id: 'radar', label: '竞品雷达', url: '/startup/execution/radar',
            icon: '📡', badge: null },
        ]
      },
      {
        name: 'reflection', label: '反思层', icon: '🧘',
        modules: [
          { id: 'journal', label: '决策日志', url: '/startup/reflection/journal',
            icon: '📓', badge: null },
          { id: 'ikigai', label: 'Ikigai 对齐', url: '/startup/reflection/ikigai',
            icon: '🎌', badge: 'new' },
          { id: 'burnout', label: '倦怠预警', url: '/startup/reflection/burnout',
            icon: '🫀', badge: null },
        ]
      },
      {
        name: 'ai-native', label: 'AI-Native', icon: '🤖',
        modules: [
          { id: 'what-if', label: '假设推演', url: '/startup/ai-native/what-if',
            icon: '🔄', badge: null },
          { id: 'cofounder', label: '联合创始人', url: '/startup/ai-native/cofounder',
            icon: '👥', badge: null },
        ]
      },
    ],

    isOpen(layerName) { return this.openLayers.includes(layerName); },
    toggle(layerName) {
      this.openLayers = this.openLayers.includes(layerName)
        ? this.openLayers.filter(l => l !== layerName)
        : [...this.openLayers, layerName];
    },
    async navigate(mod) {
      this.currentModule = mod.id;
      // 从服务端加载模块 HTML（Jinja2 渲染 + SurrealDB 数据）
      const resp = await fetch(mod.url);
      this.currentContent = await resp.text();
      // 更新 URL hash 以便刷新保持状态
      window.location.hash = mod.id;
    },
    get activeIdeas() {
      // 从 API 获取当前 active 的 idea 数量
      return '...';  // 初始化时从 Jinja2 模板注入
    },
  };
}
```

### 10.5 各模块页面风格指南

| 层 | 模块 | 内容类型 | 推荐展示方式 |
|---|---|---|---|
| 评估层 | validate | 表单 + 结果 | 左侧参数区 → 右侧实时流式输出 |
| 评估层 | idea-gen | 访谈式对话 | 聊天气泡 + 每轮追加结果卡片 |
| 策略层 | landing-page | 多版本对比 | 三列 A/B/C variant 并排 + copy 按钮 |
| 策略层 | gtm | 结构化日历 | 30 天格子 + 点击展开每日内容 |
| 执行层 | mvp-scope | 拖拽优先级 | feature cards → MUST/SHOULD/WONT 三列 |
| 执行层 | feedback | 仪表盘 | 主题聚类气泡图 + 情感趋势线 |
| 反思层 | ikigai | 可视化 | 四圆 Venn 图（SVG/CSS）+ 对齐分析 |
| 反思层 | journal | 时间线 | 决策条目列表 + 偏差检测面板 |
| AI-Native | what-if | 对话式 | 输入问题 → 结构化结果卡片（best/likely/worst） |

---

## 11. 实现路线

### Phase 0：SurrealDB 存储 + plugin 骨架 + Idea 状态机（1-2 周）

- [ ] 创建 `predefined/plugins/startup/plugin.yaml`
- [ ] 搭建 Python 插件骨架（JSON-RPC bridge、路由注册、事件订阅）
- [ ] 集成 `surrealdb.py` 嵌入式模式：`Surreal("surrealkv://data/startup")`
- [ ] 实现 `StartupStore` 类（CRUD + 图边 + 评分快照）
- [ ] 定义 SurrealDB 文档结构（idea, competitor_analysis, score_snapshot, market_insight）
- [ ] 实现 Idea 状态机（candidate → in_validation → scored → active → paused → dropped）
- [ ] 验证：SurrealDB 嵌入式模式在 subprocess plugin 环境中正常运行

### Phase 1：单体评估（2-3 周）

- [ ] 实现 `competitor-mapper` skill（第一个带真实工具调用的 skill，验证：JSON-RPC → CognitiveEngine → info-hub 搜索 → SurrealDB 存储）
- [ ] 实现 `desire-evaluator` skill（纯 LLM 推理，验证 CognitiveEngine 集成）
- [ ] 实现 `pricing-and-wtp` skill
- [ ] 实现 `idea-scoring` skill（乘法地板算法 + RAT 设计，纯 Python 函数 + LLM 辅助）
- [ ] 实现 `decision-memo` skill（Markdown 模板生成）
- [ ] 组装 `ValidationWorkflow`（Serial → Parallel → Serial）
- [ ] HTTP endpoint: `POST /startup/validate`（通过 plugin route 注册）
- [ ] 端到端测试：用户输入 idea → 5 维度分析 → decision memo 存入 SurrealDB

### Phase 2：完整评估（2-3 周）

- [ ] 实现剩余的 8 个 analysis skill
- [ ] 实现 `pivot-engine` skill（pivot_options → SurrealDB + pivot_report.md）
- [ ] 实现 `trend-analysis` + `trend-to-product-mapper`（Idea Generation workflow）
- [ ] 实现 Market Deep Dive workflow
- [ ] 实现 `user-segmentation-profiler` + `user-background-interviewer`
- [ ] 并行执行优化（Gateway PipelineEngine 编排 Python skill）

### Phase 3：自主层（2-3 周）

- [ ] 实现 `TrendWatcher`（Gateway CronSource → 触发 startup skill 执行趋势扫描）
- [ ] 实现 `IncubationBridge`（Gateway Idle depth 200+ → 查询 SurrealDB 历史分析 → LLM 跨领域关联）
- [ ] 实现 `MarketMonitor`（Webhook 新竞品警报）
- [ ] 与通知系统集成
- [ ] RAT 实验到期提醒（Cron 查询 SurrealDB score_snapshot 表）

### Phase 4：策略层 + 基础 UI（2-3 周）

- [ ] 实现 `landing-page-builder` skill（评估结果 → 落地页文案 + A/B 计划）
- [ ] 实现 `gtm-narrative` skill（分发分析 → PH 文案 + Reddit 矩阵 + 内容日历）
- [ ] 实现 `pricing-page-optimizer` skill（定价心理学文案 + 竞品对比表）
- [ ] 实现 `cold-outreach-designer` skill（ICP → 个性化邮件模板 + 跟进序列）
- [ ] 搭建 `/startup` 页面框架（Alpine.js + 左侧分层导航 + 右侧内容区）
- [ ] 评估层页面（idea 列表、评估结果、decision memo 展示）

### Phase 5：执行层 + 反思层（2-3 周）

- [ ] 实现 `mvp-scope-negotiator` skill（魔鬼代言人模式：砍 feature）
- [ ] 实现 `user-feedback-synthesizer` skill（非结构化反馈 → 主题聚类 + 情感趋势）
- [ ] 实现 `competitive-radar` skill（SurrealDB 时间序列对比 → 威胁评估）
- [ ] 实现 `founder-decision-journal` skill（决策日志 + 偏差检测）
- [ ] 实现 `ikigai-alignment-check` skill（四圆交集分析 + 矛盾检测）
- [ ] 实现 `burnout-early-warning` skill（基本版：work item 完成率趋势）
- [ ] 策略层 + 执行层 + 反思层的模块页面

### Phase 6：AI-Native 层 + 完善 UI（2-3 周）

- [ ] 实现 `what-if-simulator` skill（连锁推演 + 概率估计 + 历史对比）
- [ ] 实现 `cofounder-compatibility` skill（双人画像 → 互补矩阵）
- [ ] 完善全部模块页面（ikigai 四圆图、mvp-scope 拖拽、GTM 日历）
- [ ] 模块间的导航状态保持（Alpine.js persist plugin 或 URL hash）

### Phase 7：Team 集成 + 自主层（2-3 周）

- [ ] `startup:decided` → Team work item 自动创建
- [ ] 实现 `TrendWatcher`（Gateway CronSource → 触发 startup skill）
- [ ] 实现 `IncubationBridge`（Idle depth 200+ → SurrealDB 跨领域关联）
- [ ] 实现 `MarketMonitor`（Webhook 新竞品警报）
- [ ] RAT 实验到期提醒
- [ ] 分析结果同步到 YantrikDB MemoryProvider（长期记忆）
- [ ] 与通知系统集成

---

## 附录 A：乘法地板算法（参考实现）

```python
# startup/scoring.py
# 乘法地板评分算法
# 参考: idea-validation-agents skills/idea-scoring/SKILL.md

from dataclasses import dataclass, field
from enum import Enum

TOTAL_DIMENSIONS = 7

class Verdict(str, Enum):
    PURSUE = "pursue"
    TEST = "test"
    PIVOT = "pivot"
    DROP = "drop"

class Confidence(str, Enum):
    HIGH = "high"
    MEDIUM = "medium"
    LOW = "low"

@dataclass
class ScoreResult:
    base_score: float
    floor_penalty: float
    missing_discount: float
    final_score: int
    verdict: Verdict
    confidence: Confidence
    killer_dimensions: list[str] = field(default_factory=list)

def score_idea(
    dimensions: dict[str, float],   # {"demand": 80.0, "competition": 45.0, ...}
    weights: dict[str, float],      # {"demand": 0.20, "competition": 0.10, ...}
) -> ScoreResult:
    """乘法地板评分算法：加权求和 + 地板惩罚 + 缺失折扣"""

    # Step 1: 维度子分由各 skill 提供（0-100），跳过

    # Step 2: 地板惩罚 — 任何维度 < 25 触发乘法惩罚
    floor_penalty = 1.0
    killer_dimensions = []
    for dim, score in dimensions.items():
        if score < 25.0:
            floor_penalty *= score / 25.0
            killer_dimensions.append(dim)

    # Step 3: 加权基础分
    base_score = sum(
        score * weights.get(dim, 0.0)
        for dim, score in dimensions.items()
    )

    # Step 4: 应用惩罚
    missing_discount = len(dimensions) / TOTAL_DIMENSIONS
    adjusted = base_score * floor_penalty * missing_discount
    final_score = round(max(0.0, min(100.0, adjusted)))

    # Step 5: 置信度
    n = len(dimensions)
    if n >= 6:
        confidence = Confidence.HIGH
    elif n >= 4:
        confidence = Confidence.MEDIUM
    else:
        confidence = Confidence.LOW

    # Step 6: 裁决
    if final_score >= 75:
        verdict = Verdict.PURSUE
    elif final_score >= 55:
        verdict = Verdict.TEST
    elif final_score >= 35:
        verdict = Verdict.PIVOT
    else:
        verdict = Verdict.DROP

    return ScoreResult(
        base_score=base_score,
        floor_penalty=floor_penalty,
        missing_discount=missing_discount,
        final_score=final_score,
        verdict=verdict,
        confidence=confidence,
        killer_dimensions=killer_dimensions,
    )
```

## 附录 B：RAT 实验设计模板

```python
# startup/rat.py
# Riskiest Assumption Test
# 约束：≤ 2 周，≤ $100，行为信号（非宣称偏好）

from dataclasses import dataclass, field
from enum import Enum

class AssumptionCategory(str, Enum):
    DEMAND = "demand"               # "People actually have this problem"
    MONETIZATION = "monetization"   # "Users will pay $X/mo"
    DISTRIBUTION = "distribution"   # "Users will find it via {channel}"
    RETENTION = "retention"         # "Users will come back after day 7"
    TECHNICAL = "technical"         # "Can be built by solo dev in {timeframe}"
    MARKET = "market"               # "Market is large enough to sustain {goal}"

EXPERIMENT_TEMPLATES = {
    AssumptionCategory.DEMAND: {
        "type": "landing_page_waitlist",
        "description_template": "Build landing page for {idea_name}. Drive {channel} traffic.",
        "duration_days": 14,
        "estimated_cost_usd": 50,
        "pass_threshold": "≥ 10% email signup from ≥ 100 visitors",
    },
    AssumptionCategory.MONETIZATION: {
        "type": "wizard_of_oz",
        "description_template": "Manually deliver {idea_name} service to 5 paying customers.",
        "duration_days": 14,
        "estimated_cost_usd": 100,
        "pass_threshold": "≥ 3/5 customers pay and would recommend",
    },
    # ... 其余类别类似
}

@dataclass
class RatExperiment:
    assumption: str                 # "People actually have this problem"
    category: AssumptionCategory
    criticality: int                # 1-5
    uncertainty: int                # 1-5
    rat_score: int                  # criticality × uncertainty
    experiment_type: str            # "landing_page_waitlist" | "wizard_of_oz" | ...
    description: str
    duration_days: int              # ≤ 14
    estimated_cost_usd: int         # ≤ 100
    pass_threshold: str             # "≥ 10% email signup from ≥ 100 visitors"
    fail_action: str                # "Drop idea, document learning"

def design_rat(dimensions: dict, scores: dict) -> RatExperiment:
    """从各维度提取所有假设，找出 criticality × uncertainty 最大的那个"""
    assumptions = _extract_assumptions(dimensions)
    for a in assumptions:
        a["rat_score"] = a["criticality"] * a["uncertainty"]
    assumptions.sort(key=lambda a: a["rat_score"], reverse=True)
    top = assumptions[0]

    template = EXPERIMENT_TEMPLATES.get(top["category"], EXPERIMENT_TEMPLATES[AssumptionCategory.DEMAND])
    return RatExperiment(
        assumption=top["assumption"],
        category=top["category"],
        criticality=top["criticality"],
        uncertainty=top["uncertainty"],
        rat_score=top["rat_score"],
        experiment_type=template["type"],
        description=template["description_template"],
        duration_days=template["duration_days"],
        estimated_cost_usd=template["estimated_cost_usd"],
        pass_threshold=template["pass_threshold"],
        fail_action=f"Drop {dimensions.get('idea_name', 'idea')}, document learning",
    )
```

## 附录 C：与 idea-validation-agents 的差异总结

| 维度 | idea-validation-agents | Aman Startup |
|---|---|---|
| **平台依赖** | Claude Code / Codex / Cursor | Aman Gateway（自包含 runtime） |
| **Skill 形式** | SKILL.md prompt 模板 | Python 插件 + JSON-RPC → Gateway CognitiveEngine |
| **研究方式** | LLM 训练数据模拟 | info-hub 真实 API 调用 |
| **编排方式** | Orchestrator 顺序链 | Gateway WorkflowEngine Serial/Parallel/Conditional |
| **并行度** | 无（文件依赖强制串行） | Phase 2 全并行（7 个维度同时跑） |
| **数据流** | 文件系统读写 + prompt 拼接 | SurrealDB 文档 + JSON-RPC 事件 |
| **存储** | Markdown + JSON 文件树 | SurrealDB 嵌入式（文档 + 图原生） + YantrikDB 长期记忆 |
| **自主运行** | ❌ 无 | ✅ TrendWatcher + IncubationBridge |
| **行动闭环** | ❌ 评估即终点 | ✅ Team work item 创建 + agent 执行 |
| **跨 idea 分析** | ❌ grep | ✅ 结构化查询 + 图遍历 |
| **评估范围** | B2C App | B2C + B2B SaaS + marketplace + 可扩展 |
