---
name: kimi
description: 委托编码任务给 Moonshot AI Kimi。适合中英文双语代码库、中文需求理解和代码生成场景。
version: 1.0.0
author: Aman
license: MIT
metadata:
  hermes:
    tags: [code, kimi, moonshot, bilingual, chinese, mandarin]
    related_skills: [claude-code, codex-cli, opencode, gemini-cli]
---

# Kimi (Code)

通过 ``kimi`` 工具调用 Moonshot AI Kimi 编码 CLI (`kimi`)。

## 何时使用

- **中英文双语代码库** — 包含大量中文注释、文档或需求文档的项目
- **中文需求理解** — 需要从中文技术规格或 PRD 理解需求的编码任务
- **国内生态集成** — 与中国云服务和平台相关的代码任务
- **快速脚本和原型** — 简洁的编码任务和快速迭代

不适用于需要深度推理代码结构的复杂重构（使用 claude-code）。

## 如何使用

调用工具 ``kimi``，参数：

```json
{
  "prompt": "编码任务描述",
  "cwd": "可选的工作目录"
}
```

Kimi CLI 的 prompt 直接作为位置参数传递（无额外标志）。

## 返回结果

- `stdout` — Kimi CLI 的标准输出
- `stderr` — 错误输出（如有）
- `exit_code` — 进程退出码（0 = 成功）

## 限制

- 超时时间：5 分钟
- 需要本地安装 `kimi` CLI
- 在沙箱模式下运行（Sandbox）
