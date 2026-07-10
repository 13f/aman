# 认知翻译层 — 从系统信号到 Agent 的「感受」

> Agent 的"感受"不是 UI 指标（那是 health/metrics 层的事），
> 而是它**自己感受到的**，并据此调整行为。
>
> 不是显示 `memory_clarity: 0.3`，而是 Agent 自己说：
> "对不起，我脑子有点空，能再说一遍吗？"

---

## 1. 核心原则

```
观测系统信号 → 翻译为认知状态 → 触发行为变化 → 发出事件。
```

### 为什么需要翻译层？

| 系统信号 | LLM 看到的（原始） | Agent 感受到的（翻译后） |
|---|---|---|
| LLM P95 延迟 8s | 用户需要等 8s | "我脑子有点转不动了"（Groggy） |
| EXP.md 无匹配条目 | 空搜索结果 | "我从没做过这类事"（Bootstrap） |
| 用户消息 < 10 token | 短文本 | "问题不清楚"（Vague → 强制澄清） |
| 工作记忆 token > 70% | 上下文过长 | "信息太多我理不清"（Overloaded → 压缩） |

**翻译层的价值必须来自硬编码的行为调制，不是更好的 prompt。**

### 行为调制必须硬编码

什么算真正的硬编码调制：
- Consciousness = Catatonic → `CognitiveEngine::process` 直接 return
- Experience = Apprehensive → 从可选工具列表里剔除触发工具
- Situation = Vague → 在 ReAct loop 之前**强制插入澄清轮**

什么不算：把信号注入 prompt 让 LLM 自己判断该怎么做。

---

## 2. 三个翻译器

```
System Events (tool:completed, memory:recalled, llm:timeout, workflow:completed)
        ↓
3 个独立翻译器（各自观测不同信号，输出离散档位）
        ↓
档位组合 → 查行为表 → 硬编码行为调制（不经过 LLM 决策）
        ↓
认知事件发布（供 UI 展示或未来扩展）
```

三个翻译器之间**没有事件订阅关系**。
跨翻译器联动通过**硬编码的组合规则**处理。

---

## 3. 翻译器 A：Consciousness（意识水平）

**观测**：LLM 后端可用性

| 档位 | 信号来源 | 硬编码行为 |
|---|---|---|
| **Lucid（清醒）** | LLM 正常响应 | 正常执行 |
| **Groggy（迷糊）** | LLM 降速（P95 > 阈值） | 1 次 retry 后跳过；max_turns 降低 50% |
| **Catatonic（木僵）** | LLM 断掉 | CognitiveEngine 直接返回；不进入 ReAct loop |
| **Coma（昏迷）** | LLM 断掉 > 15 分钟 | 同上 + 不处理新入队事件 |

> 详见 [consciousness.md](./consciousness.md)

---

## 4. 翻译器 B：Grounding（信息充足度）

**观测**：Agent 是否有足够信息来执行任务。**两个独立维度，不压缩。**

### 4.1 为什么是两个维度？

"我懂这个领域（coverage 高），但用户的问题太模糊（completeness 低）" → 应该追问澄清
"用户的问题很清晰（completeness 高），但我对这个领域一无所知（coverage 低）" → 应该坦诚不知道

加权成一个值后，这两种情况得到相同分数，但正确行为完全不同。

### 4.2 维度一：Knowledge（我有没有料）

| 档位 | 信号 | 硬编码行为 |
|---|---|---|
| **Informed** | coverage > 0.6, freshness > 0.5 | 正常执行 |
| **Uninformed** | coverage < 0.3 | max_turns 降低 30%；每步 continuation 后添加强制反思检查点 |
| **Outdated** | coverage > 0.6, freshness < 0.3 | 执行但 Decision 强制标记 `confidence: Low` |

### 4.3 维度二：Situation（问题本身清不清楚）

| 档位 | 信号 | 硬编码行为 |
|---|---|---|
| **Clear** | goal 有动词 + 约束明确 | 正常执行 |
| **Vague** | goal 无动词 / 过短（< N token） | **强制插入澄清轮**——不是让 LLM 决定要不要问 |
| **Overloaded** | context token > 预算 70% 但 goal 不明确 | **先压缩**到预算 50% 以下，再重新评估 |

### 4.4 统一行为表

| 信号组合 | 涌现模式 | 硬编码行为 |
|---|---|---|
| Situation = Vague | — | 强制澄清一轮 |
| Situation = Overloaded | — | 先压缩到 50% 以下 |
| 任何以上 + Consciousness ≠ Lucid | 叠加保守 | max_turns 进一步收缩 |
| Clear + Experience(Confident) | **简单模式** | 跳过侦查工具提示；跳过反思检查点 |
| Clear + Experience(Untouched) + Knowledge(Uninformed) | **复杂模式** | 添加强制反思检查点；max_turns 降低 30% |
| Clear + Experience(Confident) + Knowledge(Outdated) | **谨慎自信模式** | 正常执行；Decision 强制 `confidence: Low` |
| 任何 + Experience(Apprehensive) | **规避模式** | 移除触发工具；stall 立即 pivot |

### 4.5 Plan 模式：不是前置分类，是运行时涌现

> **Plan 不是前置仪式，是运行时涌现的行为模式。**

所有任务默认从中等开始——正常执行，走完整 ReAct loop。
复杂度不是输入，是**执行过程中涌现出来的属性**。

Situation 档位判断的是**问题本身清不清楚**，不是任务复杂度：
- "帮我查一下天气" → Clear（问题清楚）
- "帮我搞定这个" → Vague（问题不清楚）
- "帮我分析 A 和 B 的关系，考虑 C 因素，用 D 格式输出" → Clear 但信息量大

---

## 5. 翻译器 C：Experience（经验模式）

**观测**：EXP.md 中是否有匹配当前任务的策略。**三档离散。**

### 5.1 为什么是三档而非连续值？

"从没做过"和"做过但全失败"是**质的不同**：
- 高 pattern_score → 经验直接驱动行为，自然进入简单模式
- 低 pattern_score → 不是"慢一点"，是"**绕路走**"

Apprehension 不是低 confidence——它是**负面积经验**：
低 confidence 是"多确认"，apprehensive 是"换条路"。

### 5.2 档位与硬编码行为

| 档位 | 信号 | 硬编码行为 |
|---|---|---|
| **Confident** | pattern_score > 0.7, evidence >= 3 | `skip_scout: true`；continuation 跳过反思检查点 |
| **Bootstrap** | EXP.md 为空或尚未创建 | `trigger_extraction: true`；正常执行 + 完成后触发经验萃取 |
| **Untouched** | EXP.md 有内容但无匹配条目 | 正常执行 |
| **Apprehensive** | pattern_score < 0.3, evidence >= 2 | 从可用工具列表中移除触发工具；stall 立即 pivot |

### 5.3 翻译器只输出信号，不直接干预执行

Experience = Confident → 输出 `skip_scout: bool` → CognitiveEngine 读取
Experience = Bootstrap → 输出 `trigger_extraction: bool` → 事件订阅器读取

**翻译器不知道执行细节**。

---

## 6. 跨翻译器联动（硬编码）

### 6.1 调制顺序

```
1. Consciousness 先应用
       ↓
2. effective_experience() / effective_knowledge() → 调制其他翻译器输出
       ↓
3. Situation → 前置门控（澄清/压缩）
       ↓
4. 调制后的所有档位 → 三轴合一的 max_turns 计算 → 最终行为决策
```

### 6.2 联动函数

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
        (Groggy, Uninformed, Apprehensive) => base * 30 / 100,
        (Groggy, _, Apprehensive)
        | (Groggy, Uninformed, _)
        | (_, Uninformed, Apprehensive) => base * 50 / 100,
        (Groggy, _, _) | (_, Uninformed, _) | (_, _, Apprehensive) => base * 70 / 100,
        _ => base,
    }
}

/// 认知状态 × progress → continuation 决策
fn should_continue(level: ProgressLevel, consc: Consciousness) -> bool {
    match (level, consc) {
        (Advancing, Groggy) => true,
        (Creeping, Groggy) => false,
        (Circling, Groggy) => false,
        (_, Lucid) => level != Stuck,
        (_, Catatonic) => false,
        (_, Coma) => false,
    }
}
```

联动规则**可枚举、可单测、不依赖 LLM 判断**。

---

## 7. 翻译器 fail-safe

| 翻译器 | 异常时默认档位 | 理由 |
|---|---|---|
| Consciousness | 保持原样 | 由独立健康检查模块管理 |
| Knowledge | `Uninformed` | 没有信息时假设最差 |
| Situation | `Vague` | 问题不清楚时先澄清 |
| Experience | `Untouched` | 没有经验时走完整流程 |

---

## 8. 落地位置

| 翻译器 | 落地函数 | 调用时机 |
|---|---|---|
| Consciousness | `BackendHealth` + `CognitiveStateMachine` | 每次 LLM 调用后 |
| Grounding | `evaluate_grounding()` | `ContextManager::refresh_memories` 之后 |
| Experience | workflow 启动时查询 EXP.md | 结果缓存于 workflow 生命周期内 |

---

## 9. Decision 结构扩展

Knowledge = Outdated 时，需要在行为上体现"可信度低"。
这**不是 prompt 注入**，而是 Decision 结构增加结构化字段：

```rust
pub struct Decision {
    pub action: Action,
    pub confidence: ConfidenceLevel,  // 新增字段
}

pub enum ConfidenceLevel {
    Normal,
    Low,    // Knowledge = Outdated 时强制标记
}
```

下游系统（UI/Notification/Workflow）可读取此字段，而非依赖 prompt 模板变量。

---

## 10. 事件清单

```
"consciousness:lucid" / "consciousness:groggy" / "consciousness:catatonic" / "consciousness:coma"
"consciousness:recovered"
"grounding:knowledge_informed" / "grounding:knowledge_uninformed" / "grounding:knowledge_outdated"
"grounding:situation_clear" / "grounding:situation_vague" / "grounding:situation_overloaded"
"experience:confident" / "experience:bootstrap" / "experience:untouched" / "experience:apprehensive"
```

---

## 11. 实现优先级

| 优先级 | 方向 | 理由 |
|---|---|---|
| **P0** | Consciousness | 设计已存在，直接实现 |
| **P0** | EXP.md + 经验萃取 | 独立于 memory 的新层 |
| **P1** | Grounding | 代码入侵最小 |
| **P1** | continuation 策略联动 | `should_continue()` 一行过滤 |
| **P1** | Decision::confidence 字段 | Knowledge = Outdated 时硬编码标记 Low |
| **P2** | Experience 四档调制 | 依赖 EXP.md 数据积累 |
| **P2** | task_tag LLM 识别 | Experience 翻译器内部实现 |
| **P2** | 三轴合一 max_turns | 依赖三个翻译器都有输出 |
| **P2** | Plan 模式涌现 | 三个翻译器上线后自然涌现 |

---

> **参考：**
> - [认知翻译层完整设计文档](../cognitive-memory.md)
> - [认知状态模型](../ideas/cognitive-state-model.md)
> - [CognitiveEngine trait源码](../../cognitive/engine/)
