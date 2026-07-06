# 认知翻译层：从系统信号到 Agent 的「感受」

> 状态：设计方案（等待实现）
> 创建日期：2026-07-06
> 最后更新：2026-07-06
> 触发背景：LLM 可用性被翻译为认知状态（清醒/迷糊/木僵/昏迷）的设计启发——
> 所有系统信号都可以翻译为 Agent 的"主观感受"，进而调制行为。

---

## 核心原则

> **不是加字段、加模块，而是：观测系统信号 → 翻译为认知状态 → 触发行为变化 → 发出事件。**

就像 LLM 可用性不是显示"延迟 300ms"，而是 Agent 感到"迷糊"。

认知状态不是给 UI 看的系统指标（那是 health/metrics 层的事），而是 Agent **自己感受到的**，并据此调整行为。UI 可以选择性展示——不是显示 `memory clarity: 0.3`，而是 Agent 自己说："对不起，我脑子有点空，能再说一遍吗？"

### 行为调制必须硬编码

**翻译层的价值必须来自硬编码的行为调制，而不是更好的 prompt。**

如果翻译出来的信号最后是注入 prompt 让 LLM 自己判断该怎么做——那和直接把原始信号喂给 LLM 没有区别。翻译层就变成了修辞，不是机制。

什么算真正的硬编码调制：
- Consciousness = Catatonic → `CognitiveEngine::process` 直接 return，不经过 LLM
- Experience = Apprehensive → 从可选工具列表里剔除触发工具，LLM 看不到
- Situation = Vague → 在 ReAct loop 之前**强制插入澄清轮**，不是让 LLM 决定要不要问

---

## 三层知识资产

在认知翻译层之下，aman 的知识资产分三层，互不替代：

| 层 | 存储 | 性质 | 生命周期 | 衰减 |
|---|---|---|---|---|
| **身份** | `SOUL.md` | 我是谁、我的边界、我的品味 | 几乎不变，人工维护 | 无 |
| **经验** | `EXP.md` | 工具策略、踩坑规律、有效模式 | 渐进增长，事件驱动更新 | 不降权，但可标"需验证" |
| **知识** | `yantrikdb` | 用户是谁、业务事件、历史事实 | 持续写入 | 30 天半衰期，自带遗忘 |

> **SOUL 是骨，EXP 是肌肉，Memory 是血液。**

### 为什么 EXP.md 独立于 memory？

Memory 是事件驱动的、有衰减的、语义检索的。但经验的特点：

- **不是"事实"，而是"规律"**：`("gh" CLI 比 raw API 对 PR 任务成功率更高)` 不是事件，是模式
- **跨 session 长期有效**：30 天半衰期不适合——好的工具策略可能半年有效
- **结构不同**：经验有"场景"、"策略"、"结果"、"置信度"，不是自由文本
- **更新机制不同**：不是追加新条目，而是**升级旧经验的置信度**或**标注失效**

---

## 统一架构：认知翻译层

```
System Events (tool:completed, memory:recalled, llm:timeout, workflow:completed)
        ↓
3 个独立翻译器（各自观测不同信号，输出离散档位）
        ↓
档位组合 → 查行为表 → 硬编码行为调制（不经过 LLM 决策）
        ↓
认知事件发布（供 UI 展示或未来扩展，但不参与行为调制）
```

三个翻译器之间**没有事件订阅关系**。跨翻译器联动通过**硬编码的组合规则**处理，不依赖 LLM 综合判断。

---

## 翻译器 A：Consciousness（意识水平）

观测 LLM 后端可用性。对应已有的 `cognitive-state-model.md` 设计。

### 档位与硬编码行为

| 档位 | 信号来源 | 硬编码行为 |
|---|---|---|
| **Lucid（清醒）** | LLM 正常响应 | 正常执行 |
| **Groggy（迷糊）** | LLM 降速（P95 > 阈值） | CognitiveEngine: 1 次 retry 后跳过；max_turns 降低 50% |
| **Catatonic（木僵）** | LLM 断掉 | CognitiveEngine 直接返回；不进入 ReAct loop |
| **Coma（昏迷）** | LLM 断掉 > 15 分钟 | 同上 + 不处理新入队事件 |

### 关于 idle 系统的说明

> ⚠️ **不修改 idle 状态。** Catatonic/Coma 时 idle 系统保持原样——idle 的状态转换是独立的
> 内省调度问题，不应被 LLM 可用性强制扭转。CognitiveEngine 直接返回已经足够阻止
> 无效推理，不需要额外把 idle 推进某个状态。

### 事件

- `consciousness:lucid` / `consciousness:groggy` / `consciousness:catatonic` / `consciousness:coma`
- `consciousness:recovered`（从任何非 Lucid 状态恢复）

---

## 翻译器 B：Grounding（信息充足度）

观测 Agent 是否有足够信息来执行任务。**两个独立维度，不压缩。**

### 为什么是两个维度？

"我懂这个领域（coverage 高），但用户的问题太模糊（completeness 低）" → 应该追问澄清
"用户的问题很清晰（completeness 高），但我对这个领域一无所知（coverage 低）" → 应该坦诚不知道

加权成一个值后，这两种情况得到相同分数，但正确行为完全不同。不该压缩。

### 维度一：Knowledge（我有没有料）

| 档位 | 信号 | 硬编码行为 |
|---|---|---|
| **Informed** | coverage > 0.6, freshness > 0.5 | 正常执行 |
| **Uninformed** | coverage < 0.3 | max_turns 降低 30%；每步 continuation 后添加强制反思检查点 |
| **Outdated** | coverage > 0.6, freshness < 0.3 | 执行但 Decision 强制标记 `confidence: Low`（结构化字段，非 prompt 注入） |

### 维度二：Situation（问题本身清不清楚）

| 档位 | 信号 | 硬编码行为 |
|---|---|---|
| **Clear** | goal 有动词 + 约束明确 | 正常执行 |
| **Vague** | goal 无动词 / 过短（< N token） | **在 ReAct loop 之前强制插入澄清轮**——不是让 LLM 决定要不要问，是强制至少问一次 |
| **Overloaded** | context token > 预算 70% 但 goal 不明确 | **先压缩**到预算 50% 以下，再重新评估 Situation 档位；压缩后如果 goal 仍不明确，走 Vague 的强制澄清 |

### Plan 模式：不是前置分类，是运行时涌现

> **Plan 不是前置仪式，是运行时涌现的行为模式。**

参考彭超的 Plan 五步闭环（Context Scout → Co-spark → Multi-lens Lock → Guarded
Flow → Compound Loop），但**不是每条消息都跑完整 Plan**。类比人类：收到一条
消息，你不会先分类再决定怎么回——你直接开始处理，处理中发现"这比我想象的复杂"
于是调整策略。

**核心区别：复杂度不是输入，是输出。** 它不是一个前置判断，而是执行过程中
涌现出来的属性。不需要复杂度分类器。

#### 默认值：中等复杂度

所有任务默认从中等开始——正常执行，走完整 ReAct loop。不需要任何前置判断。

#### Situation 是前置判断，但判断的不是复杂度

Situation 档位判断的是**问题本身清不清楚**（GCC 的 G 和 C），不是任务复杂度：

- "帮我查一下天气" → Clear（问题清楚）
- "帮我搞定这个" → Vague（问题不清楚）
- "帮我分析 A 和 B 的关系，考虑 C 因素，用 D 格式输出" → Clear 但信息量大

**关键区分：**
- **Situation = Vague/Overloaded** → 不属于"Plan 决策"，是"能否进入
  ReAct loop 的前置门控"。必须先澄清或压缩，再让任务进入执行。
- **Situation = Clear** → 任务可以进入执行，行为模式由翻译器信号自然涌现。

### 统一行为表

以下为三个翻译器信号组合后的**涌现行为模式**。这是输入（信号）到输出（行为）
的完整映射——不需要额外的"Plan 强度"分类步骤：

| 信号组合 | 涌现模式 | 硬编码行为 |
|---|---|---|
| **Situation = Vague** | — | 强制澄清一轮，澄清完重新评估 |
| **Situation = Overloaded** | — | 先压缩到 50% 以下，再重新评估 |
| **任何以上 + Consciousness ≠ Lucid** | 叠加保守 | max_turns 进一步收缩（见联动函数） |
| **Situation = Clear + Experience(Informed) = Confident** | **简单模式** | CognitiveEngine 读取 `skip_scout: true`，跳过侦查工具提示；continuation 跳过反思检查点 |
| **Situation = Clear + Experience(Untouched) + Knowledge(Uninformed)** | **复杂模式** | 每步 continuation 后添加强制反思检查点；max_turns 降低 30% |
| **Situation = Clear + Experience(Confident) + Knowledge(Outdated)** | **谨慎自信模式** | 正常执行；Decision 强制标记 `confidence: Low`（结构化字段） |
| **任何 + Experience = Apprehensive** | **规避模式** | 从可用工具列表中移除触发工具；continuation 遇到 stall 立即 pivot |
| **任何 + Experience = Bootstrap** | — | 事件订阅器在 workflow::completed 时触发经验萃取 |

### Decision 结构扩展

Knowledge = Outdated 时，需要在行为上体现"可信度低"。这**不是 prompt 注入**
（那将违反硬编码原则），而是 Decision 结构增加一个结构化字段：

```rust
pub struct Decision {
    pub action: Action,
    pub confidence: ConfidenceLevel,  // 新增字段
    // ...
}

pub enum ConfidenceLevel {
    Normal,
    Low,    // Knowledge = Outdated 时强制标记
}
```

UI 可以选择性展示（回复前加"⚠️ 我的知识可能过时，请核实"），但行为调制
是硬编码的——Decision::confidence 是一个可被下游系统读取的结构化信号，不是
prompt 模板变量。

### 落地位置

`ContextManager::refresh_memories` 之后追加 `evaluate_grounding()`，产出 Knowledge
和 Situation 两个维度的档位。Situation 用于前置门控（澄清/压缩），Knowledge
作为执行过程中的调制信号。Knowledge = Outdated 时，通过 Decision::confidence
字段传递，不经过 prompt 注入。

### 事件

- `grounding:knowledge_informed` / `grounding:knowledge_uninformed` / `grounding:knowledge_outdated`
- `grounding:situation_clear` / `grounding:situation_vague` / `grounding:situation_overloaded`

---

## 翻译器 C：Experience（经验模式）

观测 EXP.md 中是否有匹配当前任务的策略。**三档离散，不是连续值。**

### 为什么是三档而非连续 confidence？

"从没做过"和"做过但全失败"是**质的不同**，不是量的不同：
- 高 pattern_score → 经验直接驱动行为，Agent 自然进入简单模式
- 低 pattern_score → 不是"慢一点"，是"**绕路走**"

Apprehension 不是低 confidence——它是**负面积经验**，行为调制也不同：低 confidence 是"多确认"，apprehensive 是"换条路"。

### 档位与硬编码行为

| 档位 | 信号 | 翻译器输出 | 硬编码行为 |
|---|---|---|---|
| **Confident** | pattern_score > 0.7, evidence >= 3 | `skip_scout: true` | continuation 跳过反思检查点（不追加反思提示） |
| **Bootstrap** | EXP.md 为空或尚未创建 | `trigger_extraction: true` | 正常执行；由事件订阅器在 workflow::completed 时触发经验萃取 |
| **Untouched** | EXP.md 有内容但无匹配条目 | — | 正常执行 |
| **Apprehensive** | pattern_score < 0.3, evidence >= 2 | — | 从可用工具列表中移除触发工具；continuation 遇到 stall 立即 pivot |

**翻译器只输出信号，CognitiveEngine 读取并执行**——`skip_scout` 标志由
CognitiveEngine 读取，跳过侦查工具提示；`trigger_extraction` 由事件订阅器读取，
触发经验萃取。翻译器不直接干预 CognitiveEngine 内部逻辑。

### task_tag 的来源

workflow 启动时，Experience 翻译器调用 LLM 做**识别**（不是决策）：输入 workflow
的 goal 描述 + 当前 EXP.md 已有的 task_tag 列表，输出最匹配的 tag 或 `untouched`。
这是翻译器的内部实现细节——LLM 只负责"这个任务属于哪类"，不负责"下一步怎
么做"，不违反"行为调制硬编码"原则。

### 落地位置

workflow 启动时查询 EXP.md，按 task_tag 匹配。结果缓存于 workflow 生命周期内。

### 事件

- `experience:confident` / `experience:bootstrap` / `experience:untouched` / `experience:apprehensive`

---

## 跨翻译器联动（硬编码）

### 调制顺序

三个翻译器**并行观测**（读不同信号，无依赖），但行为调用的**应用有顺序**：

```
1. Consciousness 先应用
       ↓
2. effective_experience() / effective_knowledge() → 调制其他翻译器输出
       ↓
3. Situation → 前置门控（澄清/压缩）
       ↓
4. 调制后的所有档位 → 三轴合一的 max_turns 计算 → 最终行为决策
```

### 联动函数

```rust
/// LLM 不正常时，经验不可靠——Confident 降级为 Untouched
fn effective_experience(exp: Experience, consc: Consciousness) -> Experience {
    match (exp, consc) {
        (Experience::Confident, Consciousness::Groggy) => Experience::Untouched,
        (Experience::Confident, Consciousness::Catatonic) => Experience::Untouched,
        (Experience::Confident, Consciousness::Coma) => Experience::Untouched,
        (e, Consciousness::Lucid) => e,
        (e, _) => e,
    }
}

/// LLM 断了，Grounding 的 Knowledge 维度直接归零
fn effective_knowledge(know: Knowledge, consc: Consciousness) -> Knowledge {
    match consc {
        Consciousness::Catatonic | Consciousness::Coma => Knowledge::Uninformed,
        _ => know,
    }
}

/// 三轴合一：Consciousness × Knowledge × Experience → max_turns
fn effective_max_turns(base: u32, consc: Consciousness, know: Knowledge, exp: Experience) -> u32 {
    match (consc, know, exp) {
        // 最危险：LLM迷糊 + 没知识 + 踩过坑
        (Consciousness::Groggy, Knowledge::Uninformed, Experience::Apprehensive) => base * 30 / 100,
        // 两两组合（取最严）
        (Consciousness::Groggy, _, Experience::Apprehensive)
        | (Consciousness::Groggy, Knowledge::Uninformed, _)
        | (_, Knowledge::Uninformed, Experience::Apprehensive) => base * 50 / 100,
        // 单一维度危险
        (Consciousness::Groggy, _, _)
        | (_, Knowledge::Uninformed, _)
        | (_, _, Experience::Apprehensive) => base * 70 / 100,
        // 正常
        _ => base,
    }
}

/// 认知状态 × progress level → continuation 决策
fn should_continue(level: ProgressLevel, consc: Consciousness) -> bool {
    match (level, consc) {
        // LLM 迷糊时：只有明确 Advancing 才继续，否则暂停
        (ProgressLevel::Advancing, Consciousness::Groggy) => true,
        (ProgressLevel::Creeping, Consciousness::Groggy) => false,
        (ProgressLevel::Circling, Consciousness::Groggy) => false,
        // LLM 清醒时：按原有逻辑
        (_, Consciousness::Lucid) => level != ProgressLevel::Stuck,
        // Catatonic/Coma：不 continuation（由 CognitiveEngine 直接返回处理）
        (_, Consciousness::Catatonic) => false,
        (_, Consciousness::Coma) => false,
    }
}
```

联动规则**可枚举、可单测、不依赖 LLM 判断**。

### 翻译器 fail-safe

翻译器自身异常时（`evaluate_grounding()` panic、EXP.md 文件损坏等），档位回退到
**最保守的默认值**：

| 翻译器 | 异常时默认档位 | 理由 |
|---|---|---|
| Consciousness | 保持原样 | 由独立健康检查模块管理，不在此处假设 |
| Knowledge | `Uninformed` | 没有信息时假设最差 |
| Situation | `Vague` | 问题不清楚时先澄清 |
| Experience | `Untouched` | 没有经验时走完整流程 |

Agent 能继续运行，只是行为变保守。

---

## 经验系统：EXP.md

### 文件结构

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

### 经验萃取（事件驱动）

```
workflow::completed 事件
  → 经验萃取器订阅
  → 提取：用了什么工具组合、结果如何、是否匹配已有经验
  → 匹配到 → 升级 confidence，追加 evidence
  → 没匹配 → 新条目，confidence = 0.5
  → 发布 experience:extracted 事件
```

**不经过 LLM**——纯结构化的统计更新。只有当经验需要"文字总结"时才调 LLM（低频，可在 idle 时做）。

### Experience Think（与 yantrik think() 平行）

拟人隐喻：

> **yantrik think()** = 睡眠时巩固记忆（把短期记忆变长期，遗忘不重要的）
> **experience.consolidate()** = 睡醒后复盘手感（哪些功夫没生疏、哪些招式该练了）

```
现有 think() 调用链：
  idle 触发 → yantrik.think() → 合并/冲突/模式

修改后：
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

## 与现有系统的集成

| 现有系统 | 集成方式 | 入侵程度 |
|---|---|---|
| yantrik think() | idle 触发链追加 `experience.consolidate()` 调用 | 仅追加调用 |
| cognitive state（清醒/迷糊/木僵/昏迷） | 已完成设计（`cognitive-state-model.md`），直接实现 | 新模块 |
| ContextManager | `refresh_memories` 后追加 `evaluate_grounding()` | 追加一个函数 |
| PromptPipeline | 注入 expertise_level + cognitive state 描述 | 增加参数 |
| continuation 决策 | `evaluate_progress_level` 结果经 `should_continue()` 过滤 | 追加一个过滤函数 |
| event bus | 通过 `EventType::Custom` 发布认知事件 | 零改动（Custom 即扩展点） |
| SOUL.md | 保持不变（身份层不动） | 零改动 |
| evolution（auditor/mutator） | 复用审计基础设施做经验冲突检测 | 复用 |
| idle 系统 | **不修改**。Catatonic/Coma 不强制改变 idle 状态 | 零改动 |

---

## ⚠️ 硬约束

1. **不碰 `kernel/idle/` 内部状态机**。idle 的职责是"agent 空闲时做什么内省工作"，认知翻译是不同维度的问题。Catatonic/Coma 不强制 idle 进入任何特定状态。

2. **不修改 `MemoryProvider` trait 签名**。EXP.md 是独立层，不侵入 yantrikdb 的存储模型。

3. **不阻塞主推理路径**。所有认知翻译都是异步评估 + 事件发布，不在同步调用链上。

4. **不替代 SOUL 的身份层**。SOUL.md 是"我是谁"，EXP.md 是"我会什么"，两者职责清晰。

5. **行为调制全部硬编码**。不依赖 LLM 综合判断翻译器输出。跨翻译器联动也是硬编码的组合规则。"标记可信度"等语义信号使用结构化字段（如 `Decision::confidence`），不通过 prompt 注入。

6. **翻译器只输出信号，不直接干预执行**。Experience = Confident 输出 `skip_scout: bool`，CognitiveEngine 读取并决定是否跳过侦查；Bootstrap 输出 `trigger_extraction: bool`，事件订阅器读取并触发萃取。翻译器不知道执行细节。

7. **翻译器异常时保守降级**。任何翻译器异常，档位回退到最保守默认值，Agent 继续运行但行为变保守。

---

## 实现优先级

| 优先级 | 方向 | 理由 |
|---|---|---|
| **P0** | Consciousness（清醒/迷糊/木僵/昏迷） | 设计已存在（`cognitive-state-model.md`），直接实现，奠定认知层的存在证明 |
| **P0** | EXP.md + 经验萃取 | 独立于 memory 的新层，开启"工具策略"的复利积累 |
| **P1** | Grounding（Knowledge + Situation） | 代码入侵最小（`refresh_memories` 后追加一个函数）；Situation 做前置门控，Knowledge 做执行调制 |
| **P1** | continuation 策略联动 | `should_continue()` 一行过滤，防止 Groggy 时 continuation 空转 |
| **P1** | Decision::confidence 字段 | Knowledge = Outdated 时硬编码标记 Low，结构化字段非 prompt |
| **P2** | Experience 四档调制 + skip_scout/trigger_extraction 标志 | 依赖 EXP.md 先有数据积累 |
| **P2** | task_tag LLM 识别 | Experience 翻译器的内部实现，不影响行为调制原则 |
| **P2** | 三轴合一 max_turns + 调制顺序 | 依赖三个翻译器都有输出后才能组合 |
| **P2** | Plan 模式涌现 | 不独立实现——当 Experience + Grounding + Consciousness 三个翻译器都上线后，Plan 模式自然涌现 |

---

## 参考资料

- LLM 可用性认知状态设计：`docs/ideas/cognitive-state-model.md`
- 现有 memory 系统：`kernel/memory/` + `yantrikdb` crate
- 现有 SOUL 系统：`kernel/soul/`
- 现有 evolution 系统：`predefined/self/evolution/`
- 现有 event bus：`kernel/event-bus/`（`EventType::Custom` 扩展点）
- 现有 idle 系统：`kernel/idle/`（不可修改内部状态机）
- 现有 context manager：`kernel/context-manager/`
- 现有 prompt pipeline：`self/prompts/`
- [彭超《Agentic 之道》](https://mp.weixin.qq.com/s/btMZaBifixsnlogTDAZJhg)：Context 资产化 + GCC 框架 + Plan 生产系统 + 商业大脑 + OPC
- 本文档的同行评审：`docs/ideas/cognitive-memory-review.md`
