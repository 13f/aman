---
name: codex-cli
description: 委托编码任务给 OpenAI Codex CLI。适合快速脚本、API 集成和 OpenAI 生态相关任务。用于快速原型设计和直接编码。
version: 1.0.0
author: Aman
license: MIT
metadata:
  hermes:
    tags: [code, codex, openai, scripting, prototyping, api-integration]
    related_skills: [claude-code, opencode, gemini-cli]
---

# Codex CLI

通过 ``codex`` 工具调用 OpenAI 的 Codex CLI (`codex`)。

## 何时使用

- **快速脚本** — 一次性脚本和实用工具
- **API 集成** — OpenAI 生态系统的 API 调用和 SDK 使用
- **快速原型** — 概念验证和快速迭代
- **直接编码** — 简单、明确的功能实现

不适用于需要深度架构理解的复杂重构或大规模代码分析。

## 如何使用

调用工具 ``codex``，参数：

```json
{
  "prompt": "编码任务描述",
  "cwd": "可选的工作目录"
}
```

Codex CLI 以 `exec` 模式运行，直接执行编码任务。

## 返回结果

- `stdout` — Codex 的标准输出
- `stderr` — 错误输出（如有）
- `exit_code` — 进程退出码（0 = 成功）

## 限制

- 超时时间：5 分钟
- 需要本地安装 `codex` CLI
- 在沙箱模式下运行（Sandbox）
