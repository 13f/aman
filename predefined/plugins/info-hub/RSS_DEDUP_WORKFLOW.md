# RSS Fusion 去重 — Agent 工作流

## 文件位置

每个 agent 维护自己的去重状态：
```
~/.aman/agents/{agent_id}/memory/rss_fusion.json
```

## 文件格式 (JSON)

```json
{
  "source": "fusion-local",
  "last_fetch": "2026-06-28T14:30:00Z",
  "first_fetch": "2026-06-20T08:00:00Z",
  "fetch_count": 47,
  "total_articles_seen": 312,
  "notes": "Agent-maintained RSS dedup state."
}
```

## Agent 工作流

### 1. 冲浪前 — 读取状态

```
read_file ~/.aman/agents/{agent_id}/memory/rss_fusion.json
```

如果文件不存在或 `last_fetch` 为 null → 首次冲浪，`since` 不传（拿全部）。
如果存在 → 提取 `last_fetch` 作为 `since` 参数。

### 2. 搜索 — 带 since 参数

```
info_search(query="", limit=20, sources=["fusion-local"], since="2026-06-28T14:30:00Z")
```

`since` 会被透传到 fusion.py → SQL `WHERE pub_date > ?`，只返回增量文章。

### 3. 冲浪后 — 更新状态

处理完文章后，更新 rss_fusion.json：

```json
{
  "source": "fusion-local",
  "last_fetch": "<当前UTC时间 ISO 8601>",
  "first_fetch": "<首次fetch时间，如果之前为null则填入当前时间>",
  "fetch_count": <+1>,
  "total_articles_seen": <+本次新增篇数>,
  "notes": "Last fetch: N new articles from fusion-local"
}
```

### 注意事项

- `since` 是 ISO 8601 格式，如 `2026-06-28T14:30:00Z`
- 不要传未来的时间戳 — 会导致空结果
- 首次冲浪（文件不存在）时不要传 `since`，获取完整数据建立基线
- 每 24 小时可以做一次全量刷新（不传 since），捕获可能遗漏的文章（如 pub_date 异常的数据）
