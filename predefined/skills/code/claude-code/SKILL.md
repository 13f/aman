---
name: claude-code
description: 委托编码任务给 Claude Code (Anthropic 的 CLI 工具)。适合复杂重构、架构变更、多文件编辑和需要深度代码推理的场景。
version: 1.0.0
author: Aman
license: MIT
metadata:
  hermes:
    tags: [code, claude, refactor, architecture, multi-file]
    related_skills: [codex-cli, opencode, gemini-cli]
---

# Claude Code

通过 ``claude`` 工具调用 Anthropic 的 Claude Code CLI (`claude`)。

## 何时使用

- **复杂重构** — 跨多个文件的架构级变更
- **深度推理** — 需要理解复杂代码结构的任务
- **多文件编辑** — 涉及 3+ 个文件的协同修改
- **代码审查** — 需要仔细分析的代码检查

不适用于简单的单文件修改、单行 bug 修复或纯文本搜索。

## 如何使用

调用工具 ``claude``，参数：

```json
{
  "prompt": "完整的编码任务描述，包括具体文件路径和期望输出",
  "cwd": "可选的工作目录，默认使用agent运行时目录"
}
```

**编写 prompt 的最佳实践：**
- 明确指定要修改的文件路径
- 说明期望的变更内容
- 如有约束条件（如不改变公共 API），请明确说明
- 引用相关代码片段或行号

## 返回结果

工具返回：
- `stdout` — Claude Code 的标准输出
- `stderr` — 错误输出（如有）
- `exit_code` — 进程退出码（0 = 成功）

## 限制

- 超时时间：5 分钟
- 需要本地安装 `claude` CLI
- 在沙箱模式下运行（Sandbox）
