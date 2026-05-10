# EverOS 内化讨论记录

日期: 2026-04-25
来源: 对 [EverMind-AI/EverOS](https://github.com/EverMind-AI/EverOS) 的架构讨论

---

## 背景

EverOS 是一个长期记忆方法、基准测试和用例的集合，包含两个核心记忆方法：

- **EverCore (evermemos/)** — 一个完整的记忆操作系统，以 Docker 微服务运行。依赖 MongoDB、Elasticsearch、Milvus、Redis。提供 6 层结构化记忆（episodic_memory, atomic_fact, user_profile, agent_case, agent_skill, foresight_record），混合检索（BM25 + 向量 + 重排序），93% LoCoMo 基准。
- **HyperMem** — 超图记忆架构，三层次（主题→情节→事实），ACL 2026 论文。需要 8 张 GPU 运行嵌入/重排序模型，研究级。

## 核心对比：OpenClaw/Hermes 式 Markdown 记忆 vs EverOS

| 维度 | OpenClaw/Hermes 方式 | EverOS (EverCore) 方式 |
|------|---------------------|----------------------|
| 基础设施 | 零依赖，纯文件 | Docker + 4 个数据库 |
| 记忆存储 | 人类可读的 .md 文件 | MongoDB/ES/Milvus 二进制存储 |
| 抽取方式 | Agent 自主决定 ("记住这个") | 服务端自动 LLM 抽取每条消息 |
| 检索方式 | 关键词搜索 (session_search) | Hybrid: BM25 + 向量 + 重排序 |
| 记忆层次 | MEMORY.md + daily logs 两层 | 6 种结构化记忆类型 |
| 运维成本 | cat file.md | docker compose up + 监控 |
| 可移植性 | Git 可直接追踪 | 需要导出工具 |
| 适用场景 | 个人 Agent，单用户 | 服务化、多租户、百万级对话 |

## 讨论结论

**不直接用 EverOS 替换，而是内化其结构化思路到 Hermes 现有体系。**

理由：
1. EverOS 的基础设施负担过重（Docker + 4 DB）不适合个人 Agent 场景
2. 二进制存储丧失了 .md 文件的人类透明度和 Git 可追踪性
3. 增加了系统依赖链（任何数据库崩溃都会导致 Agent 失忆）
4. OpenClaw 的 markdown 方式在个人 Agent 场景下更优雅

## 三层结构化内化路线（构思）

### 第一层 — Episode + Fact 抽取（最高回报）

周期性 cron 任务扫描 `thoughts/` 和 `thinking-queue.json`，用 LLM 做两件事：
- 提取 compact episode 摘要 → `episodes/` 目录
- 提取 atomic fact → `facts/` 目录（每个事实一个独立文件，带结构化元数据）

### 第二层 — Topic 聚类 + Profile 累积

周期性对 episodes 做主题聚类：
- 建立 topic-index.yaml（episode → topics 映射）
- 自动生长 user profile，可注入 system prompt

### 第三层 — 轻量级混合检索（可选）

用本地 sentence-transformers 给 facts/episodes 加 embedding，做 RRF 融合（BM25 + cosine），无需 Milvus/ES。

### 核心优势

| 方面 | 直接用 EverOS | 内化方案 |
|------|--------------|---------|
| 部署 | docker compose up + 4 DB | 零额外进程 |
| 记忆透明度 | 二进制 → 只能 API 查 | 仍是 .md 文件，git 可追踪 |
| 增量收益 | 全或无 | 每层独立受益 |
| 检索质量 | 高 (hybrid + rerank) | 中高 (keyword + 可选 embedding) |
| 运维负担 | 高 | 零 |

### 后续可能

考虑将内化后的结构化记忆工具集整理为 `~/.hermes/hermes-agent/tools/` 下的独立工具，作为 Hermes 的原生扩展而非外部服务。

---

## 参考链接

- EverOS 仓库: https://github.com/EverMind-AI/EverOS
- EverCore Docs: https://docs.evermind.ai
- OpenClaw Markdown 记忆架构: https://huizhou92.com/p/why-your-ai-agent-keeps-forgetting-a-practical-memory-architecture-for-individual-users/

---

pickfire 注释 - 2026-04-25：
> 如果实现了梦境/睡眠/休息机制，可以在其中整理这些信息。——对应的，人类会在睡眠中清理大脑中的垃圾。
