# 经验层 — 肌肉记忆与工具策略

> 经验不是"事实"，是"规律"。
> `("gh" CLI 比 raw API 对 PR 任务成功率更高)` 不是事件，是模式。
>
> Aman 通过 `EXP.md` 文件 + 经验萃取器 + Experience 翻译器，
> 实现**跨 session 长期有效**的工具策略积累——
> Agent 的"肌肉记忆"。

---

## 1. 为什么 EXP.md 独立于 memory？

Memory 是事件驱动的、有衰减的、语义检索的。但经验的特点：

| 维度 | Memory | EXP.md |
|---|---|---|
| **性质** | 事实（"用户昨天说 deadline 是周五"） | 规律（"gh CLI 比 raw API 成功率高"） |
| **生命周期** | 30 天半衰期 | 长期有效，不降权 |
| **结构** | 自由文本 | 场景/策略/结果/置信度 |
| **更新机制** | 追加新条目 | **升级旧经验**的置信度或标注失效 |
| **遗忘** | 有（时间衰减） | 无（但可标"需验证"） |

**经验有"场景"、"策略"、"结果"、"置信度"，不是自由文本。**

---

## 2. EXP.md 文件结构

```markdown
# Experience

## Tool Strategies
### [task_tag] 短描述
- **Strategy**: 具体做法
- **confidence**: 0.0~1.0
- **uses**: N, **successes**: N
- **last_verified**: ISO date
- **learned_from**: [session_refs]

## Judgment Patterns
### [task_tag] 短描述
- **Pattern**: 判断规律
- **confidence**: 0.0~1.0
- **boundary_ref**: SOUL.md#Boundaries 第 N 条

## Anti-Patterns
### [task_tag] 短描述
- **Anti-pattern**: 不该做的事
- **learned_from**: session_ref（通常来自踩坑）
- **deprecated**: ISO date（可选）

## Gotchas
### [tool_name] 短描述
- **Gotcha**: 坑的描述
- **workaround**: 绕过方式
- **last_hit**: ISO date
```

---

## 3. Experience 翻译器

### 3.1 三档离散

| 档位 | 信号 | 硬编码行为 |
|---|---|---|
| **Confident** | pattern_score > 0.7, evidence >= 3 | `skip_scout: true`；跳过反思检查点 |
| **Bootstrap** | EXP.md 为空或尚未创建 | `trigger_extraction: true`；完成后触发萃取 |
| **Untouched** | EXP.md 有内容但无匹配条目 | 正常执行 |
| **Apprehensive** | pattern_score < 0.3, evidence >= 2 | 移除触发工具；stall 立即 pivot |

### 3.2 为什么是三档而非连续值？

"从没做过"和"做过但全失败"是**质的不同**：
- 高 pattern_score → 经验直接驱动行为，自然进入简单模式
- 低 pattern_score → 不是"慢一点"，是"**绕路走**"

Apprehension 不是低 confidence——它是**负面积经验**：
低 confidence 是"多确认"，apprehensive 是"换条路"。

### 3.3 task_tag 的来源

workflow 启动时，Experience 翻译器调用 LLM 做**识别**（不是决策）：
输入 workflow 的 goal 描述 + 当前 EXP.md 已有的 task_tag 列表，
输出最匹配的 tag 或 `untouched`。

**LLM 只负责"这个任务属于哪类"，不负责"下一步怎么做"。**

---

## 4. 经验萃取（事件驱动）

```
workflow::completed 事件
  → 经验萃取器订阅
  → 提取：用了什么工具组合、结果如何、是否匹配已有经验
  → 匹配到 → 升级 confidence，追加 evidence
  → 没匹配 → 新条目，confidence = 0.5
  → 发布 experience:extracted 事件
```

### 4.1 不经过 LLM

经验萃取是**纯结构化的统计更新**——不需要 LLM 参与。
只有当经验需要"文字总结"时才调 LLM（低频，可在 idle 时做）。

### 4.2 置信度升降规则

| 情况 | confidence 变化 |
|---|---|
| 匹配到旧经验 + 成功 | +0.05（上限 1.0） |
| 匹配到旧经验 + 失败 | -0.10（下限 0.0） |
| 新条目创建 | 初始 0.5 |
| 长期未使用（> 90 天） | 标 "需验证"（不降权） |
| 矛盾经验出现 | 标记等人工确认 |

---

## 5. Experience Think — 经验整理

拟人隐喻：

> **yantrik think()** = 睡眠时巩固记忆（把短期记忆变长期，遗忘不重要的）
> **experience.consolidate()** = 睡醒后复盘手感（哪些功夫没生疏、哪些招式该练了）

```
idle 触发 → yantrik.think() + experience.consolidate()
                            ↓                    ↓
                       memory 整理           经验整理
                                            ├── confidence 重算（基于近期证据）
                                            ├── 长期没用 → 标 "需验证"
                                            ├── 矛盾经验 → 标记等人工确认
                                            └── pattern_score 衰减（经验不会遗忘，但手感会生疏）
```

两者并行但不混合——memory 管"知道什么"，experience 管"会做什么"。

---

## 6. 与 Memory 的边界

```
┌───────────────────────────────────────────────────────────────┐
│  EXP.md (经验)                                                 │
│  "我会什么" — 渐进增长，事件驱动更新                               │
│  例："gh CLI 比 raw API 对 PR 任务成功率更高 (confidence=0.8)"   │
├───────────────────────────────────────────────────────────────┤
│  Memory (记忆)                                                 │
│  "我知道什么" — 持续写入，30 天半衰期                              │
│  例："用户昨天说他的项目 deadline 是周五"                          │
└───────────────────────────────────────────────────────────────┘
```

**不修改 `MemoryProvider` trait 签名**——EXP.md 是独立层，不侵入 yantrikdb 的存储模型。

---

## 7. 认知工具（callable from SKILL.md）

| 工具 | 功能 | 调用时机 |
|---|---|---|
| `experience-recall` | 按 task_tag 查询 EXP.md | Skill 入口：检查历史策略 |
| `experience-record` | 写入/更新 EXP.md 条目 | Skill 退出：记录新学到的 |
| `assess-grounding` | Knowledge × Situation 评估 | Skill 入口：判断信息充足度 |

---

## 8. 实现位置

| 组件 | 文件 | 职责 |
|---|---|---|
| EXP.md 解析/写入 | `kernel/experience/src/lib.rs` | 经验文件管理 |
| 经验萃取器 | `kernel/experience/src/extractor.rs` | workflow::completed → EXP.md 更新 |
| Experience 翻译器 | `cognitive/engine/src/translators/experience.rs` | EXP.md → 档位信号 |
| Experience Think | `kernel/experience/src/consolidate.rs` | idle 触发 → 经验整理 |
| 认知工具注册 | `kernel/gateway/src/runtime/cognitive_tools.rs` | 安装 experience-recall/record |

---

> **参考：**
> - [认知翻译层](../cognitive-memory.md) — Experience 翻译器在三层翻译器中的地位
> - [自省系统](./reflection.md) — 经验萃取的触发机制
> - [经验系统代码](../../kernel/experience/)
