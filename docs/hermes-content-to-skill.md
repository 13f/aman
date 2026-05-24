# Hermes Skill 创建判定机制

Hermes 如何判断「当前内容应该整理成一个 skill」——完整分析。

---

## 核心结论

**Hermes 没有独立的「技能检测算法」或「自动提取模块」。** Skill 创建的判定完全是 Agent 行为层的逻辑，由系统提示词（system prompt）中的规则驱动，Agent 在对话过程中自行判断是否触发。

---

## 判定流程

```
对话进行中
    │
    ├── 任务复杂度高（5+ 次工具调用）──→ 完成后 Agent 主动提议存储
    │
    ├── 遇到错误并修复成功        ──→ 将修复流程保存为 skill
    │
    ├── 用户纠正了做法            ──→ 纠正后的正确流程值得保存
    │
    ├── 发现非平凡的新工作流      ──→ 主动保存
    │
    └── 用户明说「记住这个流程」   ──→ 直接存入
```

---

## 触发条件（来自 Hermes 系统提示词）

`skill_manage` 工具定义的 Create 时机：

| # | 条件 | 说明 |
|---|------|------|
| 1 | 复杂任务成功完成（5+ 次工具调用） | 多步骤、多工具协同的任务完成后，Agent 会主动问「要不要存成 skill？」 |
| 2 | 克服了错误 / 修复了棘手 bug | 包括环境问题、依赖冲突、API 限流、调试流程等 |
| 3 | 用户纠正了做法，纠正后有效 | 用户的纠正代表正确的、值得复现的流程 |
| 4 | 发现了非平凡的工作流 | 不限于代码——配置、部署、数据处理都算 |
| 5 | 用户直接要求 | 显式指令，最高优先级 |

配套行为指令：

- *"After completing a complex task (5+ tool calls), fixing a tricky error, or discovering a non-trivial workflow, save the approach as a skill"*
- *"After difficult/iterative tasks, offer to save as a skill"*
- *"If a skill you loaded was missing steps, had wrong commands, or needed pitfalls you discovered, update it before finishing"*
- *"如果用一个 skill 时发现它过时/错误，立即 patch，不要等用户说"*

---

## 什么不是 skill

同样来自系统提示词，明确排除的情况：

- **单次工具调用**的简单操作
- **临时 TODO 状态**、任务进度、session 结果
- **会在一周内过时的事实**（PR 号、issue 号、commit SHA、「修好了 bug X」等）
- **能从项目结构轻易重新发现的东西**（如文件路径、依赖版本）
- 这些应该用 `memory`（持久事实）或 `session_search`（跨 session 回忆）

区分逻辑：

```
是稳定可复用的流程/工作流？
    ├── 是 → skill
    └── 否 → 是长期有用的事实/偏好？
                ├── 是 → memory
                └── 否 → session_search（事后回溯）
```

---

## 创建后的生命周期：Curator 系统

Skill 创建只是开始。Hermes 有一个独立的后台系统 `Curator` 自动管理 skill 的完整生命周期。

### 架构

```
Agent 创建 skill
    │
    ▼
.skill 文件写入 ~/.hermes/skills/
    │ 附带 provenance: created_by: "agent"
    │
    ▼
Curator（后台 cron）
    │  每 interval_hours 运行一次
    │  只处理 created_by: "agent" 的 skill
    │  Hub 安装的和打包的不受影响
    │
    ├── 追踪：记录 use_count、patch_count、last_activity_at
    │
    ├── 标记陈旧：超过 stale_after_days 未使用 → stale
    │        Pinned skill 豁免此阶段
    │
    └── 归档：超过 archive_after_days 的 stale skill → archive
             Pinned skill 豁免此阶段
             归档 ≠ 删除，支持 restore
```

### 配置项

`~/.hermes/config.yaml`:

```yaml
curator:
  enabled: true           # 总开关
  interval_hours: 24      # 检查间隔
  min_idle_hours: 48      # 创建后至少等多久才开始追踪
  stale_after_days: 30    # 未使用多少天后标记为陈旧
  archive_after_days: 90  # 陈旧多少天后归档
```

### CLI 操作

```bash
hermes curator status     # 查看 curator 状态和所有 skill 状态
hermes curator run        # 手动触发一次检查
hermes curator pin NAME   # 永久保护，永不自动归档
hermes curator unpin NAME # 取消保护
hermes curator backup     # 手动备份
hermes curator rollback   # 回滚到最近备份
```

### 关键设计原则

- **永不删除** — 最坏情况是 archive，可恢复。Pinned skill 连 archive 都不会。
- **Agent 创建的 skill 才会被 curator 管理** — 通过 `created_by: "agent"` 标记区分。
- **telemetry 存储**在 `~/.hermes/skills/.usage.json`，纯 JSON sidecar，不侵入 SKILL.md。

---

## Aman 的启示

对比 Hermes 的设计，aman 如果要实现类似机制，需要考虑：

| 维度 | Hermes 做法 | Aman 可参考 |
|------|------------|------------|
| 创建决策 | Agent 自主判断（提示词规则） | 同样可以让 Agent 自主判断 |
| 触发阈值 | 5+ tool calls, 纠错, 用户指令 | 可以自定义阈值和规则集 |
| 生命周期 | Curator 后台系统 | 可做轻量版：追踪 + 陈旧标记 |
| 存储格式 | SKILL.md (YAML frontmatter + Markdown) | 已有 `skill` crate，可复用 |
| 安全约束 | 不删除、不触碰非 agent 创建的 | 同理，区分系统 skill 和 agent 生成的 |
| 用户控制 | Pin 机制 + CLI | 需要类似机制 |

核心洞察：**Hermes 把这个功能完全做在 Agent 层（提示词），而不是在框架层做自动提取**。这意味着：
- 判定逻辑灵活、可随模型能力进化
- 不需要设计复杂的「重要性评分算法」
- Agent 具备上下文理解，比规则引擎更准确地判断「这个流程值不值得复用」
- 代价是依赖模型质量——差的模型可能漏判或误判
