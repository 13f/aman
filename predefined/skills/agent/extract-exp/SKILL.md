---
name: extract-exp
category: agent
description: >
  经验提取/回炉 — 执行后复盘，提取经验教训写入 EXP.md。回顾什么
  成了、什么出了问题、发现了什么规律。作为 Plan 流程的最后一步，
  让系统越用越聪明。
version: 1.0.0
triggers:
  - "extract-exp"
  - "extract experience"
  - "经验提取"
  - "回炉"
  - "总结经验"
  - "复盘一下"
  - "学到了什么"
  - "what did we learn"
tags:
  - reflection
  - experience-extraction
  - learning
metadata:
  hermes:
    tags: [reflection, experience-extraction, learning]
    related_skills: [plan, brainstorm, review]
---

# 经验提取 (extract-exp)

## Rule

**回头看看。提取规律。写入 EXP.md。让下次更聪明。**

执行不反思等于白做。反思不写入等于白反思。把刚发生过的事变成系统持久知识。

## When to Use

**用 extract-exp：**
- 复杂任务/流程刚结束（尤其是 Guarded Flow 执行后）
- 用户问"我们学到了什么？"
- 出了意外（成功或失败都行）
- 作为 Plan 流程的最后一步
- Experience=Apprehensive 触发时（分析翻车原因）

**跳过 extract-exp：**
- 任务太简单没新东西可学
- 还在执行中（extract-exp 是事后复盘）
- 紧接着又要跑完全一样的任务

## Methodology

### 1. 重建 (Reconstruct)

复盘发生了什么：
- 原始目标是什么？
- 实际发生了什么？
- 哪里偏离了计划？

### 2. 提取规律 (Pattern Extract)

对每个重大事件分类：

| 类型 | 问题 | 示例 |
|---|---|---|
| **坑 (Gotcha)** | 什么让我们惊讶？ | "kind 不需要 port-forward" |
| **有效策略** | 什么做法值得重复？ | "gh CLI 比 raw API 稳定" |
| **反模式** | 下次该避免什么？ | "raw API 重试 3 次后超时" |
| **可复用模板** | 什么可以直接拿来用？ | "k8s 部署脚本模式" |
| **新认知** | 我们现在相信什么以前不知道的？ | "部署顺序: secrets → config → pods" |

### 3. 写入 EXP.md (Persist)

每个规律：
1. 打任务标签（`[deploy]`, `[pr]`, `[k8s]`）
2. 写一句坑/策略描述
3. 根据证据设初始置信度：
   - 单次观察 → 0.5（试探性）
   - 2-3 次一致 → 0.7（涌现规律）
   - 4+ 次 → 0.9（可靠）

EXP.md 格式：
```markdown
## Gotchas
### [task_tag] 简短描述
- **Gotcha**: 遇到了什么、该怎么绕开
- **confidence**: 0.0-1.0
- **uses**: N
- **successes**: N

## Tool Strategies
### [task_tag] 简短描述
- **Strategy**: 什么做法有效
- **confidence**: 0.0-1.0
```

### 4. 反馈 (Feedback Loop)

写入后向用户汇报：
- 捕获了什么新知识
- 怎么改变未来行为（比如"下次 EXP=Confident，跳过这个检查"）
- 什么还不确定（需要更多证据）

## Output Format

```markdown
## 提取经验: <任务名>

| # | 类型 | 标签 | 发现 | 置信度 |
|---|------|------|------|--------|
| 1 | Gotcha | deploy | kind 不需要 port-forward | 0.5 |
| 2 | Strategy | github | gh CLI > raw API for PR ops | 0.5 |
| 3 | Insight | k8s | 部署顺序: secrets → config → pods | 0.5 |

### EXP.md 更新
- 新增: 2 条
- 更新: 1 条 (gh CLI 置信度 0.7 → 0.8)

### 下次效果
- `deploy` 标签将触发 Apprehensive-aware 工具选择
- `github` 标签下次跳过侦查阶段（置信度 > 0.7）

### 待验证
- deploy-order 认知对 Helm chart 成立吗？
```

## Anti-patterns

- ❌ 模糊总结（"下次小心点"）— 要具体
- ❌ 单次观察就置信度拉满 — 用 0.5 起步
- ❌ 所有东西打 "misc" 标签 — 标签必须能指导未来匹配
- ❌ 重复写入 EXP.md 已有的 — 升级置信度而不是复制
- ❌ 把观点当事实 — 区分"我们试过"和"这是真的"

## 关键原则

> extract-exp 的价值不在复盘本身 —— 在下一次执行跳过这次的坑。
> 如果 EXP.md 没更新，回炉就不完整。
