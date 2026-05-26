---
name: grok
description: 委托编码任务给 xAI Grok。适合实时推理、X 平台集成和需要独特问题解决角度的编码场景。
version: 1.0.0
author: Aman
license: MIT
metadata:
  hermes:
    tags: [code, grok, xai, realtime, reasoning]
    related_skills: [claude-code, codex-cli, opencode, gemini-cli, kimi]
---

# Grok (Code)

通过 ``grok`` 工具调用 xAI Grok 编码 CLI (`grok`)。

## 何时使用

- **实时推理** — 需要结合最新信息的编码决策和调试
- **X 平台集成** — 与 X/Twitter API 和社交数据相关的开发任务
- **独特分析角度** — 需要不同推理路径和非常规思路的复杂问题
- **快速迭代** — 原型开发和快速编码任务

不适用于需要深度代码结构理解的复杂架构重构（使用 claude-code）。

## 如何使用

调用工具 ``grok``，参数：

```json
{
  "prompt": "编码任务描述",
  "cwd": "可选的工作目录"
}
```

Grok CLI 的 prompt 直接作为位置参数传递。

## 返回结果

- `stdout` — Grok CLI 的标准输出
- `stderr` — 错误输出（如有）
- `exit_code` — 进程退出码（0 = 成功）

## 限制

- 超时时间：5 分钟
- 需要本地安装 `grok` CLI
- 在沙箱模式下运行（Sandbox）
