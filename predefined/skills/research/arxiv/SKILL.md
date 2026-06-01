---
name: arxiv
description: >
  Search arxiv.org for research papers by topic tag. Maps human-readable tags
  (English, Chinese, abbreviations) to arxiv subject categories, queries the
  arxiv API, and returns formatted paper listings. When no tags are given,
  browses all subjects and returns the latest papers. Returns empty results
  when tag-to-subject mapping fails.
version: 1.0.0
author: Aman
license: MIT
react_mode: direct
triggers:
  - "arxiv"
  - "arXiv"
  - "paper"
  - "论文"
  - "预印本"
  - "preprint"
  - "学术搜索"
  - "research paper"
  - "find paper"
  - "latest paper"
  - "最新论文"
  - "search arxiv"
tags:
  - idle_run
  - research
  - paper
  - academic
  - search
  - arxiv
idle_prompts:
  - "Browse arxiv for the latest AI papers. Pick 3 interesting ones and share what they're about."
  - "Check what's new on arxiv in machine learning. Find papers that look surprising or exciting."
  - "Scan arxiv for recent computer science breakthroughs. Highlight 3-5 papers worth reading."
  - "搜索 arxiv 上最新的量子计算论文，挑几篇有意思的分享。"
  - "看看 arxiv 上最近有什么 AI 安全/对齐相关的论文，分享你最感兴趣的。"
  - "Explore arxiv for trending papers across different fields — pick an area you're curious about and share the best finds."
  - "Go browse arxiv: find papers on topics you personally find fascinating. Summarize 3 highlights."
  - "Check arxiv for the newest papers in NLP and language models. Anything groundbreaking?"
  - "Look at what's new on arxiv in robotics and embodied AI. Find the most practical or surprising results."
  - "Browse arxiv cross-disciplinary papers — anything at the intersection of ML and another field (biology, physics, finance, etc.)."
metadata:
  hermes:
    tags: [Research, Paper, Academic, Arxiv, Search, Science]
    related_skills: [discover-facts, info-hub]
---

# Arxiv Paper Search

Search arxiv.org for the latest research papers by topic. Uses a built-in
tag→subject mapping to convert human-readable keywords into arxiv category
codes, then queries the arxiv API (Atom feed) and returns structured results.

## Core Concept

**Tag → Subject → Query**: Every user-provided tag is mapped to one or more
arxiv subject categories (e.g., "AI" → `cs.AI`, "机器学习" → `cs.LG` + `stat.ML`).
If a tag cannot be resolved to any known category, it is dropped. If ALL tags
fail to resolve, an empty result set is returned (no guessing).

## Tag → Subject Mapping

The mapping is maintained in `scripts/arxiv_search.py` (`TAG_MAP` dict).
Below is the current mapping reference:

### AI / ML / Data Science

| Tag | Arxiv Categories |
|-----|-----------------|
| `ai`, `artificial intelligence`, `人工智能` | `cs.AI` |
| `ml`, `machine learning`, `机器学习` | `cs.LG`, `stat.ML` |
| `dl`, `deep learning`, `深度学习` | `cs.LG`, `cs.CV` |
| `nlp`, `natural language processing`, `自然语言处理` | `cs.CL` |
| `llm`, `large language model`, `大模型`, `大语言模型`, `gpt` | `cs.CL`, `cs.AI` |
| `cv`, `computer vision`, `计算机视觉`, `视觉` | `cs.CV` |
| `rl`, `reinforcement learning`, `强化学习` | `cs.LG`, `cs.AI` |
| `generative ai`, `生成式` | `cs.LG`, `cs.AI` |
| `diffusion`, `diffusion model`, `扩散模型` | `cs.LG`, `cs.CV` |
| `transformer`, `transformers`, `attention` | `cs.LG`, `cs.CL` |
| `gan` | `cs.LG`, `cs.CV` |
| `neural network`, `神经网络` | `cs.NE`, `cs.LG` |
| `gnn`, `graph neural`, `图神经网络` | `cs.LG` |
| `federated learning`, `联邦学习` | `cs.LG`, `cs.DC` |
| `data science`, `数据科学` | `cs.LG`, `stat.ML` |
| `data mining`, `数据挖掘` | `cs.DB`, `cs.LG` |
| `recommender`, `recommendation`, `推荐系统` | `cs.IR`, `cs.LG` |
| `time series`, `时序`, `时间序列` | `stat.ML`, `cs.LG` |
| `anomaly detection`, `异常检测` | `cs.LG`, `stat.ML` |
| `interpretability`, `explainability`, `xai`, `可解释性` | `cs.LG`, `cs.AI` |
| `alignment` | `cs.AI`, `cs.CL` |
| `ai safety`, `ai安全` | `cs.AI`, `cs.CY` |
| `fairness`, `ethics`, `公平性`, `伦理` | `cs.CY`, `cs.LG` |
| `rag`, `retrieval augmented` | `cs.IR`, `cs.CL` |
| `knowledge graph`, `知识图谱` | `cs.AI`, `cs.DB` |
| `multi-modal`, `multimodal`, `多模态` | `cs.CV`, `cs.CL` |
| `continual learning`, `lifelong learning`, `持续学习` | `cs.LG`, `cs.AI` |
| `meta learning`, `元学习` | `cs.LG` |
| `embodied`, `具身智能` | `cs.RO`, `cs.AI` |

### Computer Science (other)

| Tag | Arxiv Categories |
|-----|-----------------|
| `robotics`, `机器人` | `cs.RO` |
| `crypto`, `cryptography`, `密码学` | `cs.CR` |
| `security`, `安全` | `cs.CR` |
| `networking`, `网络` | `cs.NI` |
| `os`, `operating system`, `操作系统` | `cs.OS` |
| `database`, `db`, `数据库` | `cs.DB` |
| `distributed`, `distributed system`, `分布式` | `cs.DC` |
| `cloud`, `cloud computing`, `云计算` | `cs.DC` |
| `edge computing`, `边缘计算` | `cs.DC`, `cs.NI` |
| `software engineering`, `软件工程` | `cs.SE` |
| `programming language`, `compiler`, `编程语言`, `编译器` | `cs.PL` |
| `hci`, `human computer interaction`, `人机交互` | `cs.HC` |
| `ir`, `information retrieval`, `信息检索` | `cs.IR` |
| `graphics`, `计算机图形学` | `cs.GR` |
| `algorithm`, `algorithms`, `data structures`, `算法` | `cs.DS` |
| `complexity`, `计算复杂性` | `cs.CC` |
| `game theory`, `博弈论` | `cs.GT` |
| `multiagent`, `agent`, `多智能体` | `cs.MA`, `cs.AI` |
| `speech`, `audio`, `语音`, `音频` | `cs.SD`, `eess.AS` |
| `quantum computing`, `量子计算` | `quant-ph`, `cs.ET` |
| `blockchain`, `区块链` | `cs.CR`, `cs.DC` |
| `iot`, `internet of things`, `物联网` | `cs.NI`, `eess.SY` |
| `web` | `cs.IR`, `cs.NI` |
| `web3` | `cs.CR`, `cs.DC` |

### Mathematics

| Tag | Arxiv Categories |
|-----|-----------------|
| `math`, `mathematics`, `数学` | `math.GM` |
| `algebra`, `代数` | `math.RA`, `math.AC` |
| `geometry`, `几何` | `math.AG`, `math.DG` |
| `topology`, `拓扑` | `math.AT`, `math.GT` |
| `number theory`, `数论` | `math.NT` |
| `probability`, `概率` | `math.PR` |
| `statistics`, `stat`, `统计` | `stat.TH`, `stat.ME`, `stat.AP` |
| `optimization`, `优化` | `math.OC`, `cs.LG` |
| `combinatorics`, `组合` | `math.CO` |
| `numerical`, `数值计算` | `math.NA` |
| `pde`, `偏微分方程` | `math.AP` |
| `dynamical systems`, `动力系统` | `math.DS` |
| `graph theory`, `图论` | `math.CO`, `cs.DM` |
| `information theory`, `信息论` | `cs.IT`, `math.IT` |
| `category theory`, `范畴论` | `math.CT` |

### Physics

| Tag | Arxiv Categories |
|-----|-----------------|
| `physics`, `物理` | `physics.gen-ph` |
| `quantum`, `quantum physics`, `量子`, `量子物理` | `quant-ph` |
| `quantum mechanics`, `量子力学` | `quant-ph` |
| `astrophysics`, `天体物理` | `astro-ph.CO`, `astro-ph.GA`, `astro-ph.HE` |
| `cosmology`, `宇宙学` | `astro-ph.CO` |
| `particle physics`, `粒子物理` | `hep-ph`, `hep-ex` |
| `condensed matter`, `凝聚态` | `cond-mat.mtrl-sci`, `cond-mat.str-el` |
| `relativity`, `general relativity`, `广义相对论` | `gr-qc` |
| `string theory`, `弦论` | `hep-th` |
| `optics`, `光学` | `physics.optics` |
| `plasma`, `等离子体` | `physics.plasm-ph` |
| `fluid dynamics`, `流体力学` | `physics.flu-dyn` |
| `nuclear`, `核物理` | `nucl-th`, `nucl-ex` |

### Biology

| Tag | Arxiv Categories |
|-----|-----------------|
| `biology`, `生物` | `q-bio.QM`, `q-bio.GN` |
| `genomics`, `genome`, `基因组` | `q-bio.GN` |
| `bioinformatics`, `生物信息` | `q-bio.QM`, `q-bio.GN` |
| `neuroscience`, `neuron`, `neural`, `神经科学` | `q-bio.NC` |
| `evolution`, `进化` | `q-bio.PE` |

### Finance / Economics

| Tag | Arxiv Categories |
|-----|-----------------|
| `finance`, `金融` | `q-fin.GN`, `q-fin.MF`, `q-fin.ST` |
| `quantitative finance`, `量化金融` | `q-fin.MF`, `q-fin.CP` |
| `trading`, `交易` | `q-fin.TR` |
| `risk`, `风险管理` | `q-fin.RM` |
| `portfolio`, `投资组合` | `q-fin.PM` |
| `pricing`, `定价` | `q-fin.PR` |
| `economics`, `econ`, `经济` | `econ.GN`, `econ.TH` |
| `econometrics`, `计量经济学` | `econ.EM` |

### Engineering

| Tag | Arxiv Categories |
|-----|-----------------|
| `signal processing`, `信号处理` | `eess.SP` |
| `image processing`, `图像处理` | `eess.IV`, `cs.CV` |
| `control`, `control systems`, `控制` | `eess.SY` |

## Workflow

### Step 1: Identify Tags (Optional)

The user may provide topic tags or simply ask to browse. Example requests:
- "arxiv AI safety" → tags: AI, safety
- "搜索论文 量子计算" → tags: 量子计算
- "find latest papers on reinforcement learning and robotics" → tags: RL, robotics
- "arxiv" (no tags) → browse all subjects
- "what's new on arxiv?" → browse all subjects

**When no tags are provided**: browse all subjects (latest papers across all categories).

### Step 2: Run the Query Script

```
python3 predefined/skills/research/arxiv/scripts/arxiv_search.py [<tag1> <tag2> ...] [--max N] [--sort submittedDate|relevance|lastUpdatedDate] [--json]
```

**Parameters:**

| Parameter | Required | Default | Description |
|-----------|----------|---------|-------------|
| `tags` (positional) | no | — | Topic tags to search for. Omit to browse all subjects. |
| `--max` | no | `20` (no tags) / `10` (with tags) | Maximum number of results |
| `--sort` | no | `submittedDate` | Sort order: `submittedDate`, `relevance`, or `lastUpdatedDate` |
| `--json` | no | `false` | Output raw JSON instead of human-readable format |

**Examples:**

```bash
# Browse all subjects — latest 20 papers
python3 predefined/skills/research/arxiv/scripts/arxiv_search.py

# Search latest AI papers
python3 predefined/skills/research/arxiv/scripts/arxiv_search.py "ai" --max 5

# Search multiple tags with JSON output
python3 predefined/skills/research/arxiv/scripts/arxiv_search.py "nlp" "transformer" --max 10 --json

# Chinese tag search
python3 predefined/skills/research/arxiv/scripts/arxiv_search.py "量子计算" --max 5

# Browse all subjects with explicit count
python3 predefined/skills/research/arxiv/scripts/arxiv_search.py --max 20
```

### Step 3: Present Results

Present the results as a formatted list. When browsing all subjects, note that no category filter was applied:

```
🔬 Arxiv Search: "AI", "safety" → cs.AI, cs.CY
Found 10 papers (sorted by submission date, newest first):
...
```

When no tags provided:
```
🔬 Arxiv Browse: all subjects
Found 20 papers (sorted by submission date, newest first):
...
```

### Step 4: Handle Edge Cases

- **No tags provided**: Browses all subjects, returns top 20 latest papers.
- **No tags resolved**: The script returns `{"papers": [], "error": "No arxiv subject categories found for tags: [...]"}`. Tell the user the tag wasn't recognized and suggest checking the mapping table.
- **No results found**: The script returns empty papers. Tell the user no papers matched and suggest broader or different tags.
- **API error**: The script returns an error message. Tell the user the API may be rate-limited and suggest retrying in a few seconds.

## Tool Selection

| Scenario | Tool |
|----------|------|
| Browse all latest papers | `python3 predefined/skills/research/arxiv/scripts/arxiv_search.py` |
| Simple tag search | `python3 predefined/skills/research/arxiv/scripts/arxiv_search.py "tag"` |
| Multi-tag search | `python3 predefined/skills/research/arxiv/scripts/arxiv_search.py "tag1" "tag2"` |
| Need paper details | `web_fetch` the paper's `url` (https://arxiv.org/abs/XXXX.XXXXX) |
| Need full PDF | Fetch `pdf_url` (https://arxiv.org/pdf/XXXX.XXXXX) |
| Direct arxiv ID lookup | `https://arxiv.org/abs/<id>` |
| Category browsing | `python3 predefined/skills/research/arxiv/scripts/arxiv_search.py "cs.CR"` |

## Notes

- **No tags = browse all** — omitting tags queries all subjects, returning the latest papers across all categories (default 20 results)
- **Tags are case-insensitive** — "AI", "ai", "Ai" all resolve the same way
- **Direct category codes work** — `cs.AI`, `quant-ph`, etc. pass through without mapping
- **Fuzzy matching** — if an exact tag match fails, partial substring matching is attempted as fallback
- **Rate limiting** — arxiv API asks for polite usage; don't fire more than 1 request per 3 seconds
- **Sort by submittedDate** — this is the default and shows newest papers first, which is usually what users want
- **The mapping table is the source of truth** — when adding a new tag, update `TAG_MAP` in `scripts/arxiv_search.py`, then reflect the change in the reference table above

## Adding New Mappings

To add a new tag→subject mapping:

1. Edit `scripts/arxiv_search.py`
2. Add entries to the `TAG_MAP` dict in alphabetical order within the appropriate section
3. Update the reference table in this SKILL.md

Format:
```python
"new tag": ["cs.CATEGORY", "other.CATEGORY"],
"中文标签": ["cs.CATEGORY"],
```

## Verification Checklist

- [ ] User provided at least one tag
- [ ] Script executed successfully (check exit code)
- [ ] If `categories_used` is empty, informed user the tag wasn't recognized
- [ ] If `papers` is empty, informed user no results were found
- [ ] Results presented with title, authors, categories, date, URL, and abstract snippet
- [ ] Links are clickable (https://arxiv.org/abs/...)
- [ ] When user asks for more detail on a paper, fetched the paper page or abstract
