# Aman Architecture Index

> 文档导航中心 — 按主题组织的所有设计文档、架构图和实施记录。
> 最后更新：2026-07-25（idle system 更新为 UI 焦点事件驱动）

---

## 快速导航

| 想要... | 去哪 |
|---------|------|
| 了解整体架构 | [架构概览](#1-架构概览) |
| 理解空闲系统 | [空闲系统](#2-空闲系统-idle-system) |
| 理解事件系统 | [事件系统](#3-事件系统) |
| 理解 Agent 设计 | [Agent 设计](#4-agent-设计) |
| 理解拟人化机制 | [拟人化](#5-拟人化-human-like) |
| 理解 LLM 聊天 | [LLM 聊天](#6-llm-聊天) |
| 理解认知引擎 | [认知引擎](#7-认知引擎) |
| 查找实施计划 | [实施路线](#8-实施路线) |
| 理解安全 | [安全](#9-安全) |
| 其他专题 | [其他专题](#10-其他专题) |

---

## 1. 架构概览

### 总览文档

| 文档 | 内容 |
|------|------|
| **[events.md](./events.md)** | 事件系统的完整参考 — EventBus、Dispatcher、Pipeline、路由、背压 |
| **[event-responsive-agent.md](./event-responsive-agent.md)** | 事件响应式 Agent 的设计哲学 |
| **[dev-guide.md](./dev-guide.md)** | 开发者指南 — 工具、配置、扩展点 |
| **[harness.md](./harness.md)** | Agent Harness 架构 — 消息处理、工具执行、安全层 |
| **[startup.md](./startup.md)** | 启动流程 — Phase 0→5 生命周期 |
| **[loop-strategy.md](./loop-strategy.md)** | 主循环策略 |

### 架构图

| 文件 | 格式 | 内容 |
|------|------|------|
| `aman-intro.mmd` | Mermaid | 系统总览图 |
| `aman-intro.png` | PNG | 系统总览图（渲染） |
| `aman.mmd` | Mermaid | 核心架构图 |
| `events.mmd` | Mermaid | 事件系统流程图 |
| `events-viewer.html` | HTML | 交互式事件图查看器 |
| `aman-viewer.html` | HTML | 交互式架构图查看器 |

### 重构与演进

| 文档 | 内容 |
|------|------|
| **[chat-refactor.md](./chat-refactor.md)** | 聊天系统重构记录 |
| **[multi-agents-refactor.md](./multi-agents-refactor.md)** | 多 Agent 重构 |
| **[react-migration-checklist.md](./react-migration-checklist.md)** | ReAct 引擎迁移清单（LlmReActEngine → LlmCognitiveEngine） |
| **[refactor-audit-20260628.md](./refactor-audit-20260628.md)** | 2026-06-28 重构审计 |
| **[self-migration-plan.md](./self-migration-plan.md)** | Self 模块迁移计划 |

---

## 2. 空闲系统 (Idle System)

> **核心变更 (2026-07-25)**：idle system 从自动运行改为 **UI 焦点事件驱动**。
> 不再由 cron 定时器或启动时自动启动，而是由 Tauri 窗体 blur/focus 事件触发 start/stop。
> 参见 [idle-design.md §15](./idle-design.md#15-ui-焦点事件驱动--startstop)。

### 核心设计

| 文档 | 内容 |
|------|------|
| **[idle-design.md](./idle-design.md)** | **主架构设计** — 九种空闲状态、双轴模型、WakeUp Ouroboros、Per-Agent 架构、UI 焦点驱动 start/stop |
| **[idle-patch.md](./idle-patch.md)** | Idle State Execution — 每个子状态的实现细节、阶段、依赖矩阵 |
| **[idle-milestones.md](./idle-milestones.md)** | M1–M8 开发里程碑与任务拆分（33/33 完成 ✅） |
| **[idle-boredom-flow.md](./idle-boredom-flow.md)** | Idle → Boredom 完整流程图 + WakeUp + work_pressure 配置 |

### 架构演进日志

| 目录 | 内容 |
|------|------|
| `idle-design-logs/` | 13 个版本草稿（v1–v7, r1–r6） |

### 关键机制速读

| 机制 | 在哪 | 一句话 |
|------|------|--------|
| UI 焦点 → idle start/stop | `idle-design.md` §15 | 窗体 blur + 12s 延时 → `start_agent_idle`；focus → `stop_agent_idle` |
| 主窗体 blur → 全部启动 | `idle-design.md` §15.1 | 主窗体 blur + 24s 延时 → `start_all_agent_idle` |
| Idle loop 仅 Idle 状态运行 | `idle-design.md` §15.2 | `AgentSystemState != Idle` 时 idle loop 暂停 |
| 双轴模型 (depth × arousal) | `idle-design.md` §3.4 | depth 解锁范围 + arousal 精调选择 |
| WakeUp Ouroboros | `idle-design.md` §14.10 | 深层状态完成后渐进苏醒，防止无限 Sleep |
| 恢复计时器 | `idle-design.md` §15.3 | stop() 后 60s 渐进恢复 depth→0、arousal→initial |
| 9 种空闲状态 | `idle-design.md` §14 | Reflection/Daze/Boredom/Sleep/Exploration/Meditation/Waiting/Incubation/WakeUp |
| Per-Agent 隔离 | `idle-design.md` §5.1 | 每个 Agent 独立的 AgentIdleManager + Local EventBus |

---

## 3. 事件系统

| 文档 | 内容 |
|------|------|
| **[events.md](./events.md)** | EventBus 完整参考 — InMemoryBus、背压 6 级、overflow-to-disk、dedup |
| **[events-comparison.md](./events-comparison.md)** | 事件系统方案对比 |
| **[events-iteration.md](./events-iteration.md)** | 事件系统迭代记录 |
| **[events-milestones.md](./events-milestones.md)** | 事件系统里程碑 |
| **[event-responsive-agent.md](./event-responsive-agent.md)** | 事件响应式 Agent 设计哲学 |

---

## 4. Agent 设计

| 文档 | 内容 |
|------|------|
| **[agent-design.md](./agent-design.md)** | Agent 架构设计 — Dispatcher、Reflection、生命周期 |
| **[architect-design.md](./architect-design.md)** | 架构师视角的完整设计 |
| **[agent-boredom-narrative-event-driven.md](./agent-boredom-narrative-event-driven.md)** | 拟人化特性与事件驱动架构（无聊、自省、叙事） |
| **[maslow-hierarchy.md](./maslow-hierarchy.md)** | 马斯洛需求层次在 Agent 中的应用 |

### Agent 设计演进日志

| 目录 | 内容 |
|------|------|
| `agent-design-logs/` | 38 个版本草稿 |

---

## 5. 拟人化 (Human-Like)

> 目录：`human-like/`

| 文档 | 内容 |
|------|------|
| **[human-like/index.md](./human-like/index.md)** | 拟人化系统索引 |
| **[human-like/idle-boredom.md](./human-like/idle-boredom.md)** | 无聊与空闲的行为设计 |
| **[human-like/reflection.md](./human-like/reflection.md)** | 自省机制 |
| **[human-like/emotion.md](./human-like/emotion.md)** | 情绪感知与表达 |
| **[human-like/consciousness.md](./human-like/consciousness.md)** | 意识状态（Lucid/Groggy/Catatonic/Coma） |
| **[human-like/identity.md](./human-like/identity.md)** | 身份锚点 |
| **[human-like/motivation.md](./human-like/motivation.md)** | 动机系统 |
| **[human-like/experience.md](./human-like/experience.md)** | 经验系统 |
| **[human-like/personality-config.md](./human-like/personality-config.md)** | 人格配置 |
| **[human-like/daily-rhythm.md](./human-like/daily-rhythm.md)** | 日常节律 |
| **[human-like/cognitive-translators.md](./human-like/cognitive-translators.md)** | 认知翻译层 |
| **[human-like/aman-human-like-mechanism.md](./human-like/aman-human-like-mechanism.md)** | 拟人化机制总览 |

---

## 6. LLM 聊天

| 文档 | 内容 |
|------|------|
| **[llm-chat-design.md](./llm-chat-design.md)** | LLM 聊天系统设计 |
| **[llm-chat-architect.md](./llm-chat-architect.md)** | 架构师视角 |
| **[llm-chat-milestones.md](./llm-chat-milestones.md)** | 里程碑 |

### 聊天设计演进日志

| 目录 | 内容 |
|------|------|
| `llm-chat-design-logs/` | 12 个版本草稿 |

---

## 7. 认知引擎

| 文档 | 内容 |
|------|------|
| **[cognitive-memory.md](./cognitive-memory.md)** | 认知记忆设计（基于彭超的 Agentic 之道） |
| **[rig-vs-aman-cognitive-engine.md](./rig-vs-aman-cognitive-engine.md)** | Rig vs aman 认知引擎对比 |

---

## 8. 实施路线

| 文档 | 内容 |
|------|------|
| **[milestone.md](./milestone.md)** | 总体里程碑 |
| **[team.md](./team.md)** | 团队分工 |
| **[team-architect.md](./team-architect.md)** | 架构师团队 |
| **[team-human-agent-research.md](./team-human-agent-research.md)** | 人类-Agent 研究 |
| **[skills-iteration.md](./skills-iteration.md)** | 技能系统迭代 |
| **[code-review-20260614.md](./code-review-20260614.md)** | 代码审查记录 |

---

## 9. 安全

| 文档 | 内容 |
|------|------|
| **[security-harness.md](./security-harness.md)** | 安全架构 — OutputValidator、ContentFilter、InputSanitizer、InjectionDetector、沙箱 |

---

## 10. 其他专题

| 文档 | 内容 |
|------|------|
| **[study-design.md](./study-design.md)** | 学习系统设计 |
| **[work-design.md](./work-design.md)** | 工作系统设计 |
| **[daily-life-design.md](./daily-life-design.md)** | 日常生活设计 |
| **[info-hub.md](./info-hub.md)** | 信息中心插件 |
| **[notification.md](./notification.md)** | 通知系统 |
| **[prompt.md](./prompt.md)** | 提示词设计 |
| **[hermes-content-to-skill.md](./hermes-content-to-skill.md)** | Hermes 内容→技能转换 |
| **[hermes_system_prompt.md](./hermes_system_prompt.md)** | Hermes 系统提示词 |

### 外部研究

| 文档 | 内容 |
|------|------|
| `20260619-deli-autoresearch-aman.md` | Deli 自动研究对比 |
| `game/Poseidon1.md` | Poseidon1 游戏设计 |

### 想法与规则

| 目录 | 内容 |
|------|------|
| `ideas/` | 4 个想法文档 + cool.md |
| `rules/` | 1 个规范文档（中文） |

---

## 文档关系图

```
                    ┌─────────────────────────┐
                    │   architecture-index.md  │ ← 你在这里
                    └────────────┬────────────┘
                                 │
          ┌──────────────────────┼──────────────────────┐
          │                      │                      │
          ▼                      ▼                      ▼
   ┌─────────────┐      ┌──────────────┐       ┌──────────────┐
   │ 架构概览     │      │ 空闲系统      │       │ 事件系统      │
   │ events.md   │◄────►│ idle-design  │◄─────►│ events.md    │
   │ dev-guide   │      │ idle-patch   │       │ events-*     │
   │ harness     │      │ idle-milest  │       └──────────────┘
   │ startup     │      │ idle-flow    │
   └──────┬──────┘      └──────┬───────┘
          │                    │
          │            ┌───────┴───────┐
          │            │               │
          ▼            ▼               ▼
   ┌─────────────┐  ┌────────┐  ┌──────────┐
   │ Agent 设计   │  │ 拟人化  │  │ LLM 聊天  │
   │ agent-design│  │human-  │  │llm-chat  │
   │ architect   │  │like/   │  │design    │
   └─────────────┘  └────────┘  └──────────┘
          │
          ▼
   ┌─────────────┐
   │ 认知引擎     │
   │ cognitive-  │
   │ memory.md   │
   └─────────────┘
```

---

## 按读者角色推荐

| 角色 | 推荐阅读顺序 |
|------|-------------|
| **新开发者** | `events.md` → `idle-design.md` → `dev-guide.md` → `harness.md` |
| **架构师** | `architect-design.md` → `idle-design.md` → `agent-design.md` → `cognitive-memory.md` |
| **前端开发者** | `idle-design.md` §15 (UI 焦点驱动) → `idle-boredom-flow.md` → `events.md` |
| **AI/ML 工程师** | `cognitive-memory.md` → `human-like/` → `llm-chat-design.md` |
| **安全工程师** | `security-harness.md` → `harness.md` → `dev-guide.md` |
