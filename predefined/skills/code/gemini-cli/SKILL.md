---
name: gemini-cli
description: 委托编码任务给 Google Gemini CLI。适合长上下文分析（1M+ tokens）、Google Cloud 集成和大规模重构场景。
version: 1.0.0
author: Aman
license: MIT
metadata:
  hermes:
    tags: [code, gemini, google, long-context, large-scale, gcp]
    related_skills: [claude-code, codex-cli, opencode]
---

# Gemini CLI

通过 ``gemini`` 工具调用 Google Gemini CLI (`gemini`)。

## 何时使用

- **长上下文分析** — 需要处理海量代码（1M+ tokens）的任务
- **Google Cloud 集成** — 需要与 GCP 服务交互的代码任务
- **大规模重构** — 涉及大量文件的全局变更
- **上下文密集** — 需要理解整个代码库才能完成的任务

不适用于简单的单文件编辑或短上下文任务（使用 claude-code 更高效）。

## 如何使用

调用工具 ``gemini``，参数：

```json
{
  "prompt": "编码任务描述",
  "cwd": "可选的工作目录"
}
```

Gemini CLI 的 prompt 直接作为位置参数传递（无额外标志）。

## 返回结果

- `stdout` — Gemini CLI 的标准输出
- `stderr` — 错误输出（如有）
- `exit_code` — 进程退出码（0 = 成功）

## 限制

- 超时时间：5 分钟
- 需要本地安装 `gemini` CLI
- 在沙箱模式下运行（Sandbox）
