---
name: opencode
description: 委托编码任务给 OpenCode 开源 CLI 工具。适合代码库探索、安全审计和大规模 grep 分析。用于理解和分析不熟悉的代码。
version: 1.0.0
author: Aman
license: MIT
metadata:
  hermes:
    tags: [code, opencode, exploration, security-audit, analysis, open-source]
    related_skills: [claude-code, codex-cli, gemini-cli]
---

# OpenCode

通过 ``opencode`` 工具调用 OpenCode CLI (`opencode`)。

## 何时使用

- **代码库探索** — 理解不熟悉的代码库结构和逻辑
- **安全审计** — 查找安全漏洞和代码异味
- **Grep 分析** — 大规模代码搜索和模式匹配
- **代码理解** — 追踪调用链、理解数据流

不适用于需要直接生成或修改代码的任务（优先使用 claude-code 或 codex）。

## 如何使用

调用工具 ``opencode``，参数：

```json
{
  "prompt": "探索/分析任务描述",
  "cwd": "可选的工作目录"
}
```

OpenCode 的 prompt 直接作为位置参数传递（无额外标志）。

## 返回结果

- `stdout` — OpenCode 的标准输出
- `stderr` — 错误输出（如有）
- `exit_code` — 进程退出码（0 = 成功）

## 限制

- 超时时间：5 分钟
- 需要本地安装 `opencode` CLI
- 在沙箱模式下运行（Sandbox）
