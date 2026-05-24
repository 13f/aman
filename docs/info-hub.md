# info-hub Plugin Design

信息中心插件。aman 不维护 RSS 或数据源，只做**统一检索入口 + 结果归一化**。用户通过自己常用的工具/app/云维护数据，info-hub 负责查询。

## Architecture

```
┌──────────────────────────────────────────────────┐
│ Skill (SKILL.md)                                  │
│   "use info_search + info_score_articles..."      │
└─────────────────────┬────────────────────────────┘
                      │ LLM decides to call tools
                      ▼
┌──────────────────────────────────────────────────┐
│ ToolRegistry                                      │
│   info_search, info_score_articles,               │
│   info_summarize_articles, info_generate_highlights│
└─────────────────────┬────────────────────────────┘
                      │
                      ▼
┌──────────────────────────────────────────────────┐
│ info-hub Plugin (InProcess)                       │
│                                                    │
│  ┌──────────┐  ┌──────────┐  ┌──────────┐        │
│  │ api.rs   │  │ cli.rs   │  │ db.rs    │        │
│  │ (reqwest)│  │ (Command)│  │ (Command)│        │
│  └────┬─────┘  └────┬─────┘  └────┬─────┘        │
│       │             │             │               │
│       ▼             ▼             ▼               │
│  HTTP GET      spawn CLI     spawn script         │
│  parse JSON    parse stdout  stdin→stdout JSON     │
│       │             │             │               │
│       └─────────┬───┴─────────────┘               │
│                 ▼                                  │
│         merge + dedup (by url)                     │
│                 │                                  │
│  ┌──────────────┼──────────────────────┐          │
│  │ ai.rs        │                      │          │
│  │ score_articles│ summarise_articles  │          │
│  │ generate_highlights                 │          │
│  │              │                      │          │
│  │     memory.llm config               │          │
│  │     OpenAI-compatible API           │          │
│  └──────────────┴──────────────────────┘          │
│                                                    │
│         → Vec<InfoItem> + AI results              │
└──────────────────────────────────────────────────┘
```

## Data Flow

```
User query: "golang generics"
     │
     ▼
info_search(query: "golang generics", limit: 20)
     │
     ├──[parallel]──▶ api adapter   → GET https://rsshub.app/feed/golang%20generics
     ├──[parallel]──▶ cli adapter   → spawn blogwatcher search "golang generics" --json
     └──[parallel]──▶ db adapter    → echo '{"query":"golang generics"}' | python3 search.py
     │
     ▼
merge results, dedup by url, sort by published desc
     │
     ▼
return Vec<InfoItem>
```

## Core Types

```rust
/// Registered as `info_search` tool in ToolRegistry.
/// Input: natural language query + optional filters.
struct InfoSearchInput {
    query: String,
    limit: Option<usize>,      // default 20
    offset: Option<usize>,     // default 0
    sources: Option<Vec<String>>, // filter by source name, empty = all
}

/// Normalized result from any data source.
struct InfoItem {
    title: String,
    url: String,
    summary: String,
    published: Option<DateTime<Utc>>,
    source: String,            // configured source name, e.g. "rsshub-tech"
    raw: serde_json::Value,     // original item for downstream use
}

/// Plugin config loaded from user config file.
struct InfoHubConfig {
    sources: Vec<SourceConfig>,
    timeout_ms: u64,           // per-source timeout, default 10000
    max_retries: u32,          // default 0 (no retry)
}

enum SourceConfig {
    Api {
        name: String,
        api_url: String,       // "{query}" placeholder for substitution
        api_key: Option<String>, // env var expansion: ${VAR_NAME}
        headers: HashMap<String, String>,
    },
    Cli {
        name: String,
        command: String,
        args: Vec<String>,     // "{query}", "{limit}" placeholders
    },
    Db {
        name: String,
        runtime: String,       // "python3", "node", "deno", etc.
        script: String,        // path to script, ~ expanded
        db_path: Option<String>, // informational, passed as env
    },
}
```

## Three Adapters

### 1. API Adapter

```
config → reqwest GET → parse JSON → Vec<InfoItem>
```

- URL 中 `{query}` 替换为 URL-encoded 的用户输入
- `api_key` 支持 `${ENV_VAR}` 展开
- 只发 GET，不写
- 响应格式通过 content-type 判断，优先 JSON，fallback XML/RSS

### 2. CLI Adapter

```
config → std::process::Command → stdout JSON → Vec<InfoItem>
```

- 使用 `Command::arg()` 逐参数传递，**不走 shell**，防止命令注入
- args 中 `{query}` 替换为用户输入原文
- stdout 约定：JSON 数组 `[{...}, ...]`
- 非零 exit code → 记录 error log，返回空结果

### 3. DB Adapter

```
config → spawn runtime + script → stdin JSON → stdout JSON → Vec<InfoItem>
```

**脚本契约（stdin/stdout）：**

```
stdin:  {"query": "...", "limit": 20, "offset": 0, "db_path": "/path/to/db"}
stdout: [{"title": "...", "url": "...", "summary": "...", "published": "..."}, ...]
stderr: free-form, logged at debug level
exit:   0 = success, non-zero = error (logged, empty result returned)
```

- info-hub 不引入任何 DB driver
- 用户用自己熟悉的语言写查询脚本，连 SQLite / LanceDB / 任何存储
- 脚本崩了不影响 aman 主进程（subprocess 隔离）

**最小示例脚本（Python）：**

```python
import sys, json, sqlite3

input_data = json.loads(sys.stdin.read())
db = sqlite3.connect(f"file:{input_data['db_path']}?mode=ro", uri=True)
query = input_data['query']
limit = input_data.get('limit', 20)

rows = db.execute(
    "SELECT title, url, summary, published FROM entries WHERE title LIKE ? OR content LIKE ? LIMIT ?",
    (f"%{query}%", f"%{query}%", limit)
).fetchall()

results = [
    {"title": r[0], "url": r[1], "summary": r[2], "published": r[3]}
    for r in rows
]
json.dump(results, sys.stdout)
```

## Config Example

```yaml
# In user config
info_hub:
  timeout_ms: 10000

  sources:
    - name: rsshub-tech
      type: api
      api_url: "https://rsshub.app/feed/{query}"
      api_key: "Bearer ${RSSHUB_TOKEN}"

    - name: blogwatcher
      type: cli
      command: blogwatcher
      args: ["search", "{query}", "--json", "--limit", "20"]

    - name: fusion-local
      type: db
      runtime: python3
      script: ~/.aman/scripts/fusion_search.py
      db_path: ~/.fusion/data.db

    - name: my-lancedb
      type: db
      runtime: deno
      script: ~/.aman/scripts/lancedb_search.ts
      db_path: ~/data/lancedb
```

## Execution Model

```
info_search(query, limit, sources_filter)
│
├─ Filter sources by sources_filter (if specified)
├─ For each source, spawn async task (tokio::spawn)
│   ├─ timeout_ms per source
│   └─ on timeout → log warning, skip source
│
├─ Collect all results
├─ Dedup by url (first wins)
├─ Sort by published desc (items without date go last)
├─ Truncate to limit
│
└─ Return Vec<InfoItem>
```

## Security

| 关注点 | 措施 |
|---|---|
| CLI 命令注入 | `Command::arg()` 逐参数传递，不走 `/bin/sh -c` |
| API key 泄漏 | 配置中只写 `${ENV_VAR}` 引用，运行时展开，不落盘 |
| DB 脚本注入 | stdin JSON 协议，不拼接 shell；用户对自己的脚本负责 |
| DB 写操作 | 脚本建议以只读方式打开数据库，但由用户自行保证 |
| SSRF | API URL 由用户配置，接受风险；可后续加 allowlist |
| 超时 | 每个 source 独立 timeout，避免某个远端拖死整个查询 |

## AI Processing

info-hub 提供三个 AI 工具，使用 `memory.llm` 配置的 LLM 进行文章处理：

| Tool | Description |
|---|---|
| `info_score_articles` | 多维度评分（相关性/质量/时效性 1-10）+ 分类标签 + 关键词提取 |
| `info_summarize_articles` | 中文标题翻译 + 结构化摘要（4-6 句）+ 推荐理由 |
| `info_generate_highlights` | 今日看点总结（3-5 句宏观趋势归纳） |

### LLM Config Resolution

```
memory.llm.provider → providers.<key> → base_url, api_key
memory.llm.model    → providers.<key>.models[] → API model name
```

所有 AI 工具通过 OpenAI-compatible `/chat/completions` 接口调用，不绑定特定厂商。
`api_key` 仅存在于 aman 配置文件中，插件和脚本不接触 API key。

### Gateway Endpoint

外部脚本通过 gateway HTTP API 调用 AI 工具：

```
POST /tools/{name}/execute
Authorization: Bearer <aman-api-token>
Content-Type: application/json

{"articles": [...]}
```

响应格式：

```json
{
  "tool": "info_score_articles",
  "duration_ms": 1234,
  "output": {"results": [...]}
}
```

Python 脚本直接通过 `~/.aman/config.yaml` 的 `gateway.port` 连接 gateway。

### Prompt Design

- **Scoring**: 中文系统 prompt，三维度评分细则（1-10），6 个分类标签，2-4 个英文关键词
- **Summarization**: 5 要素摘要结构（问题→论点→方案→发现→结论），支持 zh/en
- **Highlights**: 宏观趋势归纳，不逐篇列举

JSON 解析支持 markdown fence 剥离、中文智能引号替换、截断 JSON 修复。

## Python Scripts

`predefined/plugins/info-hub/` 下的脚本供 DB adapter 使用，也支持 standalone 模式：

| Script | Purpose |
|---|---|
| `common.py` | Aman 配置加载、文本工具（HTML 剥离、截断、日期解析）、DB adapter 协议 |
| `ai.py` | AI 处理：调用 gateway 的 `POST /tools/{name}/execute` 端点，API key 由 aman 服务端管理 |
| `fusion.py` | Fusion DB adapter：SQLite 查询 + standalone pipeline（搜索→评分→摘要→亮点） |
| `rss.py` | RSS DB adapter：SQLite 查询 + standalone pipeline |

### DB Adapter Protocol

```
stdin:  {"query": "...", "limit": 20, "offset": 0, "db_path": "/path/to/db"}
stdout: [{"title": "...", "url": "...", "summary": "...", "published": "...", "source": "..."}, ...]
```

### Standalone Mode

```bash
python3 fusion.py --standalone --db-path ~/.fusion/data.db --top-n 20 --lang zh
```

Standalone 模式执行完整 pipeline：DB 查询 → AI 评分 → 排序取 Top N → AI 摘要 → AI 亮点生成 → JSON 输出。

## Plugin Integration

```
info-hub
├── Cargo.toml         ← 独立 crate
├── plugin.toml        ← plugin 元数据
└── src/
    ├── lib.rs         ← Plugin trait impl, register 4 tools
    ├── config.rs      ← SourceConfig + LlmConfig 解析和校验
    ├── ai.rs          ← LLM client, prompts, JSON parsing, batch processing
    ├── adapters/
    │   ├── mod.rs     ← Adapter trait
    │   ├── api.rs     ← ApiAdapter
    │   ├── cli.rs     ← CliAdapter
    │   └── db.rs      ← DbAdapter
    ├── types.rs       ← InfoSearchInput, InfoItem
    └── merge.rs       ← 去重、排序、截断
```

`Plugin::tools()` 返回 `vec![info_search, info_score_articles, info_summarize_articles, info_generate_highlights]`。

## Skill Integration

任意 skill 的 SKILL.md 中描述即可触发：

```markdown
## Tools
- info_search: Search across configured RSS, CLI tools, and local databases.

## Method
When the user asks about recent developments in a topic:
1. Call info_search with the topic as query
2. Summarize top 5 results with url and published date
```

LLM 在 ReAct 循环中看到 tool 列表里有 `info_search`，根据 SKILL.md 指引决定调用。

## Implementation Phases

| Phase | Scope | Status |
|---|---|---|
| Phase 1 | Plugin skeleton + DB adapter + merge logic | Done |
| Phase 2 | CLI adapter | Done |
| Phase 3 | API adapter | Done |
| Phase 4 | Error handling, timeout, tests | Done |
| Phase 5 | AI scoring, summarization, highlights tools | Done |
| Phase 6 | Python scripts (common, ai, fusion, rss) | Done |
| Phase 7 | Wire into runtime as built-in plugin | Pending |
