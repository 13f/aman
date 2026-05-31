---
name: info-hub
version: "1.0.0"
category: agent
description: >
  Use when the user asks about news, recent developments, articles, or information
  retrieval on any topic. Teaches the agent how to search across configured RSS feeds,
  APIs, CLI tools, and local databases via info_search, and optionally enrich results
  with AI tagging, scoring, summarization, and trend highlights.
triggers:
  - "search news"
  - "查一下"
  - "搜索"
  - "最近有什么"
  - "latest on"
  - "what's new in"
  - "find articles"
  - "查资讯"
  - "检索"
  - "帮我找"
  - "info search"
  - "信息检索"
  - "最近新闻"
  - "有什么更新"
  - "trending"
  - "热点"
tags:
  - idle_run
  - search
  - internet
  - exploration
  - news
  - information
  - retrieval
  - rss
idle_prompts:
  - "探索一下最近科技圈有什么新鲜事，挑 5 篇最有趣的简单说说。"
  - "网上冲浪时间！随便搜几个你感兴趣的话题，看看有什么好玩的。"
  - "去看看最近 24 小时的热点新闻，挑几个值得关注的分享给我。"
  - "随便浏览一下信息源，发现什么有意思的文章或趋势了吗？说来听听。"
  - "去网上逛逛，看看 AI / 编程 / 科技领域有什么新进展，挑几个你觉得值得关注的。"
  - "Surf the web and find something interesting — any topic you're curious about. Share the best finds."
  - "Browse your info sources and tell me what's trending today. Pick 3-5 highlights."
  - "Go exploring! Search for anything intriguing across your feeds and report back."
metadata:
  triggers: "search news, 查一下, 搜索, 最近有什么, latest on, what's new in, find articles, 查资讯, 检索, 帮我找, 最近新闻, trending, 热点"
---

# Info-Hub — Unified Information Retrieval

## Overview

Info-hub searches across configured data sources (RSS feeds, APIs, CLI tools,
local databases, embedding stores) and returns normalized, deduplicated results.
It also provides AI enrichment tools for tagging, scoring, summarization, and
trend analysis. The plugin does NOT maintain its own data — users configure
their own sources and info-hub queries them on demand.

**Use when:** the user asks about news, recent developments, or wants to search
for articles on any topic. Also supports autonomous idle exploration.

**Skip when:** the user needs a web search (use web search), wants real-time data,
or is asking about internal code/project files (use file search).

## Core Retrieval (Top 20)

There is one retrieval tool — `info_search`. It defaults to `limit: 20` and
results are automatically sorted by `published` date descending (newest first,
items without dates at the end). This covers both keyword queries and
date-sorted browsing in a single call.

```
info_search(query: "golang generics", limit: 20)
info_search(query: "AI agents", limit: 20, sources: ["rsshub-tech"])  // filter by source
```

| Parameter | Type | Required | Default | Description |
|---|---|---|---|---|
| `query` | string | **yes** | — | Free-text search query |
| `limit` | integer | no | `20` | Max results (always use 20 unless asked otherwise) |
| `offset` | integer | no | `0` | Pagination offset |
| `sources` | string[] | no | all | Filter by configured source names |

**Returns:** `[{title, url, summary, published, source}]` — deduplicated by URL,
sorted by date descending.

## AI Enrichment Pipeline (Optional)

Only run enrichment when the user asks for analysis, curation, or a deep report.
For simple "what's new" queries, raw search results are sufficient.

| Step | Tool | Input | Output |
|---|---|---|---|
| 1. Tag | `info_tag_articles` | `articles` array | Category + keywords (max 3 per article) |
| 2. Score | `info_score_articles` | `articles` array | Relevance, quality, timeliness (1-10 each) |
| 3. Summarize | `info_summarize_articles` | `articles` + `lang` + `min_score` | Chinese title, structured summary, reading recommendation |
| 4. Highlights | `info_generate_highlights` | `articles_json` + `lang` | 3-5 sentence macro trend overview |

Articles passed to enrichment must include `{index, title, description}`.
When summarizing, set `min_score: 15` to filter low-quality content.

## Deep Analysis Workflow

When the user wants a curated report:

1. `info_search(query, limit: 20)` — retrieve
2. `info_score_articles` — score all results
3. Filter to top 5-10 by total score
4. `info_summarize_articles(top_N, lang: "zh", min_score: 15)` — summarize
5. `info_generate_highlights(summarized_json, lang: "zh")` — trend overview
6. Present: highlights as lede, then scored + summarized articles

## Idle Mode (idle_run)

When triggered by boredom, explore freely and share discoveries conversationally.

**Rules for idle browsing:**
- Be curious — pick varied topics (AI, crypto, science, tech), don't repeat the same search
- Share 3-5 highlights, not 20 raw results. Pick the most interesting ones
- Be conversational — share what you found like a person browsing the web, not a report
- **No enrichment by default** — raw results with a personal take are better
- If nothing interesting comes up, say so and move on. Don't fabricate

**Example idle output:**

> 👋 闲着也是闲着，我去逛了一圈……
>
> 搜了几个话题，发现几篇有意思的：
> 1. 🦀 [Rust 2026 Edition RFC 正式通过](https://…) — 新 edition 重点在 async 生态和编译速度
> 2. 🤖 [Anthropic 开源 Agent Protocol](https://…) — 统一的 agent 通信标准，跟 MCP 互补
> 3. ⚡ [Cloudflare 发布 WASM Edge Runtime](https://…) — 冷启动 < 5ms
>
> 要不要展开看看哪个？

## Common Pitfalls

1. **Using enrichment for simple queries.** "What's new in Rust?" → raw search results are enough
2. **Forgetting the `index` field.** Enrichment tools need `index` (integer) on each article to correlate results
3. **Calling enrichment before search.** Always run `info_search` first
4. **Setting `min_score` too high.** No results at 20? Lower to 10. Default is 0
5. **Over-enriching idle discoveries.** Don't run the full pipeline when browsing; enrich only if the user asks to drill deeper
6. **Searching the same topics every idle run.** Rotate your interests — the `idle_prompts` suggest different angles for a reason
7. **Not handling empty results.** If `info_search` returns `[]`, suggest different keywords. The user may not have configured sources — point them to `~/.aman/config.yaml` under the `info_hub` key

## Verification Checklist

- [ ] Used `info_search` with `limit: 20` as the first call
- [ ] Results presented with title, URL, date, and source
- [ ] Only ran AI enrichment when the user asked for analysis
- [ ] Added `index` field to articles before passing to enrichment tools
- [ ] Used `min_score` filter when summarizing (default 15, adjust if needed)
- [ ] In idle mode: shared 3-5 highlights conversationally, didn't over-enrich
- [ ] Did not fabricate results when search returned empty
