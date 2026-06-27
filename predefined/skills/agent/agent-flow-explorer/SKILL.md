---
name: agent-flow-explorer
description: >
  探索 aman agent 框架的代码架构与执行流程。回答"这个事件怎么流转？"
  "这个 trait 有哪些实现？""最近改动影响了哪些模块？"等问题。
  使用 grep/find 定位代码，read 理解逻辑，综合分析与解释。
category: agent
triggers:
  - "agent flow"
  - "代码流程"
  - "架构"
  - "event flow"
  - "事件流转"
  - "调用链"
  - "哪些实现"
  - "explain"
  - "解释代码"
metadata:
  triggers: "agent flow, 代码流程, 架构, event flow, 事件流转, 调用链, 哪些实现, explain, 解释代码"
---

# Agent Flow Explorer

探索 aman agent 框架的代码架构和执行流程。回答架构问题、追踪事件流、
寻找 trait 实现、理解模块间调用关系。

## 何时使用

- "这个事件从 Source 到 EventBus 的流程是什么？"
- "CognitiveEngine trait 有哪些实现？"
- "AgentRuntime 的 Phase 0→5 启动序列做了什么？"
- "最近改动的 X 模块会影响哪些文件？"
- "LLM 调用的完整调用链是什么？"

不适用于简单的一行问题（如"这个函数在哪"）或纯 git log 查询。

## 探索流程

### 阶段 1：定位（find + grep）

使用 find 和 grep 工具快速定位相关代码：

```bash
# 定位 trait 定义
grep -rn "pub trait CognitiveEngine" cognitive/

# 找到所有实现
grep -rn "impl.*CognitiveEngine.*for" cognitive/ kernel/

# 找到调用点
grep -rn "\.process(" kernel/gateway/src/

# 列出某个 crate 的结构
find cognitive/llm/src -name "*.rs" | sort
```

### 阶段 2：理解（read）

读取关键文件的完整内容，理解接口定义和实现逻辑。
每次至少读 30 行获取足够上下文。

### 阶段 3：综合（解释）

将发现整理为清晰的回答：
- **架构层级**：从上到下解释调用链
- **关键接口**：列出涉及的 trait 和 struct
- **数据流**：说明数据如何在不同模块间传递
- **错误处理**：标注关键的 error 路径

## 常见探索模式

### 追踪事件流

```
Source → EventBus → Dispatcher → Pipeline/Skill → Workflow
```

1. `grep -rn "impl EventSource" kernel/source/src/` — 找所有 Source 实现
2. `grep -rn "publish\|dispatch" kernel/event-bus/src/` — 追踪事件发布
3. `grep -rn "RouteRule\|DispatchTarget" kernel/dispatcher/src/` — 看路由规则

### 找 trait 实现

```
grep -rn "impl.*{TRAIT_NAME}.*for" --include="*.rs" .
```

### 理解启动流程

```
grep -rn "Phase 0\|Phase 1\|Phase 5\|startup" kernel/gateway/src/runtime/
```

## 输出格式

```markdown
## {问题}

### 调用链

{ASCII 流程图}

### 关键文件

| 文件 | 作用 |
|------|------|
| `path/to/file.rs` | 说明 |

### 详细解释

{逐层解释}
```

## 限制

- 仅读取 aman 仓库内的代码，不修改任何文件
- 如果某个函数/模块不清楚，标注 `[需要进一步确认]`
- 优先级：先 grep 定位 → 再 read 理解 → 最后综合分析
