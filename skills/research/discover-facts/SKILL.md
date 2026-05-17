---
name: discover-facts
description: 从 URL 或文本内容中提取结构化事实三元组（主体-谓词-对象），涵盖事件、行为、动作、声明、发布信息等。输入可以是网页 URL 或原始文本，输出为机器可读的事实列表。
version: 1.0.0
author: Hermes Agent
license: MIT
metadata:
  hermes:
    tags: [Fact-Extraction, NER, Relation-Extraction, Intelligence, Research]
    related_skills: [web, arxiv, blogwatcher, chaotic-reasoning]
---

# Discover Facts

从非结构化文本或网页中提取结构化事实三元组（Subject-Predicate-Object），涵盖：事件、行为、动作、声明、发布/发帖行为等。

## 核心概念

**事实三元组**: `(主体, 谓词, 对象)` — 每一个可独立验证的信息单元。

**适用场景**:
- 从新闻页面提取「谁在何时做了什么事」
- 从社交媒体帖子提取「某人发表了什么声明/在哪个平台发布了什么内容」
- 从文档提取「某实体采取了什么行动」
- 从混合内容中提取所有可归纳的事实

## 工作流

### 步骤 1：获取内容

**输入是 URL**:
```
web_extract(urls=["https://example.com/article"])
```

**输入是原始文本**: 直接进入步骤 2。

### 步骤 2：事实提取

直接在当前对话中让 LLM 按指定格式提取 facts（无需额外 API key，使用当前 provider）：

```
从以下文本中提取所有事实三元组（subject, predicate, object），输出JSON格式。

要求：
- type 可选：event（事件）、action（行为）、statement（声明）、claim（未经证实的断言）、publication（发布/发帖）
- 每条事实附上 source_quote（原文对应片段）和 confidence（0.0-1.0）
- 只提取文本中明确存在的信息，不要推断
- 按 type 分组统计
- 覆盖所有能提取的事实，不要遗漏

输出格式：
{
  "facts": [
    {"subject": "...", "predicate": "...", "object": "...", "type": "...", "source_quote": "...", "confidence": 0.x}
  ],
  "metadata": {"total_facts": N, "by_type": {"event": N, "action": N, ...}}
}

文本内容：
[粘贴提取到的文本内容]
```

### 步骤 3：验证与过滤（可选）

提取后，检查：
- **confidence < 0.5** → 标记为低置信度，手动复核
- **duplicate** → 去重（相同 subject + predicate + object）
- **unverified claim** → 区分 `type: claim` 和 `type: event`

### 步骤 4：整理为可读表格

将 JSON 输出整理为三列表格：

| 事实内容 | 类型 | 置信度 |
|---------|------|--------|
| (subject, predicate, object 合并为一句完整中文) | type | confidence |

示例：

| 事实内容 | 类型 | 置信度 |
|---------|------|--------|
| 特朗普家族从 WLFI 相关业务中已获得至少 8.9 亿美元收入 | event | 0.9 |
| 孙宇晨向 WLFI 投资 7,500 万美元 | event | 0.95 |
| WLFI 将孙宇晨 5.95 亿枚代币（时值约 1.07 亿美元）钱包列入黑名单 | action | 0.9 |
| 赵长鹏获总统特赦后，SEC 撤销了对币安的诉讼 | event | 0.95 |
| Corey Caplan 同时担任 Dolomite 联合创始人和 WLFI 首席技术官 | action | 0.9 |
| Trump 总统在 Truth Social 发文"现在是买入的好时机"，数小时后宣布关税暂停 90 天，纳斯达克指数随即上涨约 12% | publication | 0.95 |

## 事实类型分类

| Type | Description | Example |
|------|-------------|---------|
| `event` | 发生的事情（有时间点） | "Apple released iPhone 16" |
| `action` | 实体执行的行为 | "Google blocked the account" |
| `statement` | 明确引用/声明 | "Biden said: 'We will not negotiate'" |
| `claim` | 未经证实的断言 | "Sources say the company is bankrupt" |
| `publication` | 在互联网上发布/发帖 | "User @john posted on Reddit: ..." |

## 工具选择

| 输入类型 | 推荐工具 |
|---------|---------|
| 网页 URL | `web_extract` → LLM 解析 |
| 多个 URL | `web_extract` (批量，最多 5) |
| 本地文本文件 | `read_file` → 粘贴到步骤 2 |
| 社交媒体帖子截图 | `browser_navigate` → `browser_vision` |
| PDF 文档 | `web_extract` (PDF URL) 或 `ocr-and-documents` skill |

## 注意事项

- **不要编造事实** — 只提取文本中明确存在的信息
- **区分事件和声明** — 「说」不等于「做了」
- **记录来源** — 每条事实需附上 `source_quote`
- **处理匿名源** — "Anonymous source said..." 应标记为 `type: claim`, confidence: 0.4
- **长文本分段** — 超过 8000 token 的内容需分段处理
- **去重** — 同一事实可能在文中多次出现，只保留一条

## 质量标准

1. **可验证性** — 每条事实都能在原文找到对应表述
2. **独立性** — 每条事实独立成立，不依赖其他事实
3. **完整性** — 三元组（主体, 谓词, 对象）无缺失
4. **无推断** — 不添加原文未明确说明的信息
