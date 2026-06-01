#!/usr/bin/env python3
"""
Query arxiv API by tag → subject category mapping.

Usage:
  python3 arxiv_search.py [<tag> [tag2 tag3 ...]] [--max N] [--sort relevance|lastUpdatedDate|submittedDate]

When tags are provided, maps them to arxiv subject categories and queries.
When no tags are given, queries all subjects (browse latest papers).

Examples:
  python3 arxiv_search.py "AI" --max 5
  python3 arxiv_search.py "机器学习" "量子计算" --max 10 --sort submittedDate
  python3 arxiv_search.py "crypto" "security"
  python3 arxiv_search.py --max 20              # browse all subjects, top 20

Output: JSON array of paper objects to stdout.
  Empty array [] if tag→subject mapping fails or no results found.
"""

import sys
import json
import time
import urllib.request
import urllib.parse
import xml.etree.ElementTree as ET
from typing import Optional

# ── Tag → Arxiv Subject Category Mapping ──────────────────────────────────
# Each tag maps to one or more arxiv category codes.
# English tags, Chinese tags, common abbreviations, and aliases are all supported.

TAG_MAP: dict[str, list[str]] = {
    # ── Computer Science ───────────────────────────────────────────────
    "ai": ["cs.AI"],
    "artificial intelligence": ["cs.AI"],
    "artificial-intelligence": ["cs.AI"],
    "人工智能": ["cs.AI"],

    "ml": ["cs.LG", "stat.ML"],
    "machine learning": ["cs.LG", "stat.ML"],
    "machine-learning": ["cs.LG", "stat.ML"],
    "机器学习": ["cs.LG", "stat.ML"],

    "dl": ["cs.LG", "cs.CV"],
    "deep learning": ["cs.LG", "cs.CV"],
    "deep-learning": ["cs.LG", "cs.CV"],
    "深度学习": ["cs.LG", "cs.CV"],

    "nlp": ["cs.CL"],
    "natural language processing": ["cs.CL"],
    "natural-language-processing": ["cs.CL"],
    "自然语言处理": ["cs.CL"],
    "语言模型": ["cs.CL"],

    "llm": ["cs.CL", "cs.AI"],
    "large language model": ["cs.CL", "cs.AI"],
    "large-language-model": ["cs.CL", "cs.AI"],
    "大模型": ["cs.CL", "cs.AI"],
    "大语言模型": ["cs.CL", "cs.AI"],
    "gpt": ["cs.CL", "cs.AI"],

    "cv": ["cs.CV"],
    "computer vision": ["cs.CV"],
    "computer-vision": ["cs.CV"],
    "计算机视觉": ["cs.CV"],
    "视觉": ["cs.CV"],

    "robotics": ["cs.RO"],
    "机器人": ["cs.RO"],

    "crypto": ["cs.CR"],
    "cryptography": ["cs.CR"],
    "密码学": ["cs.CR"],

    "security": ["cs.CR"],
    "安全": ["cs.CR"],

    "networking": ["cs.NI"],
    "network": ["cs.NI"],
    "网络": ["cs.NI"],

    "os": ["cs.OS"],
    "operating system": ["cs.OS"],
    "operating-system": ["cs.OS"],
    "操作系统": ["cs.OS"],

    "database": ["cs.DB"],
    "db": ["cs.DB"],
    "数据库": ["cs.DB"],

    "distributed": ["cs.DC"],
    "distributed system": ["cs.DC"],
    "distributed-systems": ["cs.DC"],
    "分布式": ["cs.DC"],
    "分布式系统": ["cs.DC"],

    "software engineering": ["cs.SE"],
    "software-engineering": ["cs.SE"],
    "软件工程": ["cs.SE"],

    "programming language": ["cs.PL"],
    "programming-languages": ["cs.PL"],
    "编程语言": ["cs.PL"],

    "compiler": ["cs.PL"],
    "编译器": ["cs.PL"],

    "hci": ["cs.HC"],
    "human computer interaction": ["cs.HC"],
    "human-computer-interaction": ["cs.HC"],
    "人机交互": ["cs.HC"],

    "ir": ["cs.IR"],
    "information retrieval": ["cs.IR"],
    "information-retrieval": ["cs.IR"],
    "信息检索": ["cs.IR"],

    "graphics": ["cs.GR"],
    "计算机图形学": ["cs.GR"],

    "data structure": ["cs.DS"],
    "data-structures": ["cs.DS"],
    "algorithm": ["cs.DS"],
    "algorithms": ["cs.DS"],
    "算法": ["cs.DS"],

    "complexity": ["cs.CC"],
    "计算复杂性": ["cs.CC"],

    "game theory": ["cs.GT"],
    "game-theory": ["cs.GT"],
    "博弈论": ["cs.GT"],

    "multiagent": ["cs.MA"],
    "multi-agent": ["cs.MA"],
    "agent": ["cs.MA", "cs.AI"],
    "多智能体": ["cs.MA"],

    "neural network": ["cs.NE", "cs.LG"],
    "neural-networks": ["cs.NE", "cs.LG"],
    "神经网络": ["cs.NE", "cs.LG"],

    "reinforcement learning": ["cs.LG", "cs.AI"],
    "reinforcement-learning": ["cs.LG", "cs.AI"],
    "rl": ["cs.LG", "cs.AI"],
    "强化学习": ["cs.LG", "cs.AI"],

    "data mining": ["cs.DB", "cs.LG"],
    "data-mining": ["cs.DB", "cs.LG"],
    "数据挖掘": ["cs.DB", "cs.LG"],

    "speech": ["cs.SD", "eess.AS"],
    "语音": ["cs.SD", "eess.AS"],
    "audio": ["cs.SD", "eess.AS"],
    "音频": ["cs.SD", "eess.AS"],

    "quantum computing": ["quant-ph", "cs.ET"],
    "quantum-computing": ["quant-ph", "cs.ET"],
    "量子计算": ["quant-ph", "cs.ET"],

    "blockchain": ["cs.CR", "cs.DC"],
    "区块链": ["cs.CR", "cs.DC"],

    "iot": ["cs.NI", "eess.SY"],
    "internet of things": ["cs.NI", "eess.SY"],
    "物联网": ["cs.NI", "eess.SY"],

    "edge computing": ["cs.DC", "cs.NI"],
    "edge-computing": ["cs.DC", "cs.NI"],
    "边缘计算": ["cs.DC", "cs.NI"],

    "cloud": ["cs.DC"],
    "cloud computing": ["cs.DC"],
    "cloud-computing": ["cs.DC"],
    "云计算": ["cs.DC"],

    "web": ["cs.IR", "cs.NI"],
    "web3": ["cs.CR", "cs.DC"],

    # ── Mathematics ──────────────────────────────────────────────────────
    "math": ["math.GM"],
    "mathematics": ["math.GM"],
    "数学": ["math.GM"],

    "algebra": ["math.RA", "math.AC"],
    "代数": ["math.RA", "math.AC"],

    "geometry": ["math.AG", "math.DG"],
    "几何": ["math.AG", "math.DG"],

    "topology": ["math.AT", "math.GT"],
    "拓扑": ["math.AT", "math.GT"],

    "number theory": ["math.NT"],
    "number-theory": ["math.NT"],
    "数论": ["math.NT"],

    "probability": ["math.PR"],
    "概率": ["math.PR"],

    "statistics": ["stat.TH", "stat.ME", "stat.AP"],
    "stat": ["stat.TH", "stat.ME", "stat.AP"],
    "统计": ["stat.TH", "stat.ME", "stat.AP"],

    "optimization": ["math.OC", "cs.LG"],
    "优化": ["math.OC", "cs.LG"],

    "combinatorics": ["math.CO"],
    "组合": ["math.CO"],

    "numerical": ["math.NA"],
    "数值计算": ["math.NA"],

    "pde": ["math.AP"],
    "偏微分方程": ["math.AP"],

    "dynamical systems": ["math.DS"],
    "dynamical-systems": ["math.DS"],
    "动力系统": ["math.DS"],

    "graph theory": ["math.CO", "cs.DM"],
    "graph-theory": ["math.CO", "cs.DM"],
    "图论": ["math.CO", "cs.DM"],

    "information theory": ["cs.IT", "math.IT"],
    "information-theory": ["cs.IT", "math.IT"],
    "信息论": ["cs.IT", "math.IT"],

    "category theory": ["math.CT"],
    "category-theory": ["math.CT"],
    "范畴论": ["math.CT"],

    # ── Physics ───────────────────────────────────────────────────────────
    "physics": ["physics.gen-ph"],
    "物理": ["physics.gen-ph"],

    "quantum": ["quant-ph"],
    "quantum physics": ["quant-ph"],
    "quantum-physics": ["quant-ph"],
    "量子": ["quant-ph"],
    "量子物理": ["quant-ph"],

    "quantum mechanics": ["quant-ph"],
    "quantum-mechanics": ["quant-ph"],
    "量子力学": ["quant-ph"],

    "astrophysics": ["astro-ph.CO", "astro-ph.GA", "astro-ph.HE"],
    "天体物理": ["astro-ph.CO", "astro-ph.GA", "astro-ph.HE"],

    "cosmology": ["astro-ph.CO"],
    "宇宙学": ["astro-ph.CO"],

    "particle physics": ["hep-ph", "hep-ex"],
    "particle-physics": ["hep-ph", "hep-ex"],
    "粒子物理": ["hep-ph", "hep-ex"],

    "condensed matter": ["cond-mat.mtrl-sci", "cond-mat.str-el"],
    "condensed-matter": ["cond-mat.mtrl-sci", "cond-mat.str-el"],
    "凝聚态": ["cond-mat.mtrl-sci", "cond-mat.str-el"],

    "relativity": ["gr-qc"],
    "general relativity": ["gr-qc"],
    "广义相对论": ["gr-qc"],

    "string theory": ["hep-th"],
    "string-theory": ["hep-th"],
    "弦论": ["hep-th"],

    "optics": ["physics.optics"],
    "光学": ["physics.optics"],

    "plasma": ["physics.plasm-ph"],
    "等离子体": ["physics.plasm-ph"],

    "fluid dynamics": ["physics.flu-dyn"],
    "fluid-dynamics": ["physics.flu-dyn"],
    "流体力学": ["physics.flu-dyn"],

    "nuclear": ["nucl-th", "nucl-ex"],
    "核物理": ["nucl-th", "nucl-ex"],

    # ── Quantitative Biology ─────────────────────────────────────────────
    "biology": ["q-bio.QM", "q-bio.GN"],
    "生物": ["q-bio.QM", "q-bio.GN"],

    "genomics": ["q-bio.GN"],
    "基因组": ["q-bio.GN"],

    "genome": ["q-bio.GN"],
    "bioinformatics": ["q-bio.QM", "q-bio.GN"],
    "生物信息": ["q-bio.QM", "q-bio.GN"],

    "neuroscience": ["q-bio.NC"],
    "neuron": ["q-bio.NC"],
    "neural": ["q-bio.NC"],
    "神经科学": ["q-bio.NC"],

    "evolution": ["q-bio.PE"],
    "进化": ["q-bio.PE"],

    # ── Quantitative Finance ─────────────────────────────────────────────
    "finance": ["q-fin.GN", "q-fin.MF", "q-fin.ST"],
    "金融": ["q-fin.GN", "q-fin.MF", "q-fin.ST"],

    "quantitative finance": ["q-fin.MF", "q-fin.CP"],
    "quantitative-finance": ["q-fin.MF", "q-fin.CP"],
    "量化金融": ["q-fin.MF", "q-fin.CP"],

    "trading": ["q-fin.TR"],
    "交易": ["q-fin.TR"],

    "risk": ["q-fin.RM"],
    "风险管理": ["q-fin.RM"],

    "portfolio": ["q-fin.PM"],
    "投资组合": ["q-fin.PM"],

    "pricing": ["q-fin.PR"],
    "定价": ["q-fin.PR"],

    # ── Economics ────────────────────────────────────────────────────────
    "economics": ["econ.GN", "econ.TH"],
    "econ": ["econ.GN", "econ.TH"],
    "经济": ["econ.GN", "econ.TH"],

    "econometrics": ["econ.EM"],
    "计量经济学": ["econ.EM"],

    # ── Electrical Engineering ───────────────────────────────────────────
    "signal processing": ["eess.SP"],
    "signal-processing": ["eess.SP"],
    "信号处理": ["eess.SP"],

    "image processing": ["eess.IV", "cs.CV"],
    "image-processing": ["eess.IV", "cs.CV"],
    "图像处理": ["eess.IV", "cs.CV"],

    "control": ["eess.SY"],
    "control systems": ["eess.SY"],
    "control-systems": ["eess.SY"],
    "控制": ["eess.SY"],

    # ── Cross-disciplinary ───────────────────────────────────────────────
    "data science": ["cs.LG", "stat.ML"],
    "data-science": ["cs.LG", "stat.ML"],
    "数据科学": ["cs.LG", "stat.ML"],

    "gan": ["cs.LG", "cs.CV"],
    "generative": ["cs.LG", "cs.AI"],
    "generative ai": ["cs.LG", "cs.AI"],
    "generative-ai": ["cs.LG", "cs.AI"],
    "生成式": ["cs.LG", "cs.AI"],

    "diffusion": ["cs.LG", "cs.CV"],
    "diffusion model": ["cs.LG", "cs.CV"],
    "diffusion-models": ["cs.LG", "cs.CV"],
    "扩散模型": ["cs.LG", "cs.CV"],

    "transformer": ["cs.LG", "cs.CL"],
    "transformers": ["cs.LG", "cs.CL"],
    "attention": ["cs.LG", "cs.CL"],

    "rag": ["cs.IR", "cs.CL"],
    "retrieval augmented": ["cs.IR", "cs.CL"],

    "federated learning": ["cs.LG", "cs.DC"],
    "federated-learning": ["cs.LG", "cs.DC"],
    "联邦学习": ["cs.LG", "cs.DC"],

    "graph neural": ["cs.LG"],
    "gnn": ["cs.LG"],
    "图神经网络": ["cs.LG"],

    "interpretability": ["cs.LG", "cs.AI"],
    "explainability": ["cs.LG", "cs.AI"],
    "xai": ["cs.LG", "cs.AI"],
    "可解释性": ["cs.LG", "cs.AI"],

    "alignment": ["cs.AI", "cs.CL"],
    "safety": ["cs.AI", "cs.CY"],
    "ai safety": ["cs.AI", "cs.CY"],
    "ai-safety": ["cs.AI", "cs.CY"],
    "ai对齐": ["cs.AI", "cs.CL"],
    "ai安全": ["cs.AI", "cs.CY"],

    "ethics": ["cs.CY"],
    "伦理": ["cs.CY"],
    "fairness": ["cs.CY", "cs.LG"],
    "公平性": ["cs.CY", "cs.LG"],

    "recommender": ["cs.IR", "cs.LG"],
    "recommendation": ["cs.IR", "cs.LG"],
    "推荐系统": ["cs.IR", "cs.LG"],

    "knowledge graph": ["cs.AI", "cs.DB"],
    "knowledge-graph": ["cs.AI", "cs.DB"],
    "知识图谱": ["cs.AI", "cs.DB"],

    "time series": ["stat.ML", "cs.LG"],
    "timeseries": ["stat.ML", "cs.LG"],
    "时序": ["stat.ML", "cs.LG"],
    "时间序列": ["stat.ML", "cs.LG"],

    "anomaly detection": ["cs.LG", "stat.ML"],
    "anomaly-detection": ["cs.LG", "stat.ML"],
    "异常检测": ["cs.LG", "stat.ML"],

    "continual learning": ["cs.LG", "cs.AI"],
    "lifelong learning": ["cs.LG", "cs.AI"],
    "持续学习": ["cs.LG", "cs.AI"],

    "meta learning": ["cs.LG"],
    "meta-learning": ["cs.LG"],
    "元学习": ["cs.LG"],

    "multi-modal": ["cs.CV", "cs.CL"],
    "multimodal": ["cs.CV", "cs.CL"],
    "多模态": ["cs.CV", "cs.CL"],

    "embodied": ["cs.RO", "cs.AI"],
    "具身智能": ["cs.RO", "cs.AI"],
}

# Arxiv API base URL
API_BASE = "https://export.arxiv.org/api/query"

# Atom XML namespaces
ATOM_NS = "http://www.w3.org/2005/Atom"
OPENSEARCH_NS = "http://a9.com/-/spec/opensearch/1.1/"
ARXIV_NS = "http://arxiv.org/schemas/atom"


def resolve_tags(tags: list[str]) -> list[str]:
    """Convert user-supplied tags to arxiv subject category codes.

    Returns a sorted, deduplicated list of category codes.
    Returns an empty list if none of the tags could be resolved.
    """
    categories: set[str] = set()
    for tag in tags:
        key = tag.strip().lower()
        if key in TAG_MAP:
            categories.update(TAG_MAP[key])
        else:
            # Try case-insensitive partial match as fallback
            matched = False
            for map_key, cats in TAG_MAP.items():
                if key in map_key or map_key in key:
                    categories.update(cats)
                    matched = True
            if not matched:
                # If the tag looks like a raw category code (e.g. "cs.AI"), accept it
                if "." in key and len(key) < 20:
                    # Could be a direct category code — accept it (defense in depth)
                    categories.add(key)
    return sorted(categories)


def build_query(categories: list[str]) -> str:
    """Build an arxiv API search query from category codes.

    Uses OR to combine categories: cat:cs.AI+OR+cat:stat.ML
    When categories is empty, returns 'all:*' to match all papers.
    """
    if not categories:
        return "all:*"
    parts = [f"cat:{cat}" for cat in categories]
    return "+OR+".join(parts)


def parse_atom(xml_text: str) -> list[dict]:
    """Parse arxiv Atom XML response into a list of paper dicts."""
    root = ET.fromstring(xml_text)

    papers = []
    for entry in root.findall(f"{{{ATOM_NS}}}entry"):
        paper = {}

        # ID (arxiv URL)
        id_el = entry.find(f"{{{ATOM_NS}}}id")
        if id_el is not None and id_el.text:
            # Extract arxiv ID from URL like http://arxiv.org/abs/2301.12345v1
            paper["id"] = id_el.text.strip().split("/abs/")[-1]

        # Title
        title_el = entry.find(f"{{{ATOM_NS}}}title")
        if title_el is not None and title_el.text:
            paper["title"] = title_el.text.strip().replace("\n", " ").replace("  ", " ")

        # Summary / abstract
        summary_el = entry.find(f"{{{ATOM_NS}}}summary")
        if summary_el is not None and summary_el.text:
            paper["summary"] = summary_el.text.strip().replace("\n", " ").replace("  ", " ")

        # Published date
        published_el = entry.find(f"{{{ATOM_NS}}}published")
        if published_el is not None and published_el.text:
            paper["published"] = published_el.text.strip()

        # Updated date
        updated_el = entry.find(f"{{{ATOM_NS}}}updated")
        if updated_el is not None and updated_el.text:
            paper["updated"] = updated_el.text.strip()

        # Authors
        authors = []
        for author_el in entry.findall(f"{{{ATOM_NS}}}author"):
            name_el = author_el.find(f"{{{ATOM_NS}}}name")
            if name_el is not None and name_el.text:
                authors.append(name_el.text.strip())
        paper["authors"] = authors

        # Primary category
        primary_el = entry.find(f"{{{ARXIV_NS}}}primary_category")
        if primary_el is not None:
            paper["primary_category"] = primary_el.get("term", "")

        # All categories
        categories = []
        for cat_el in entry.findall(f"{{{ARXIV_NS}}}category"):
            categories.append(cat_el.get("term", ""))
        paper["categories"] = categories

        # Links
        links = []
        for link_el in entry.findall(f"{{{ATOM_NS}}}link"):
            links.append({
                "href": link_el.get("href", ""),
                "rel": link_el.get("rel", ""),
                "type": link_el.get("type", ""),
            })
        paper["links"] = links

        # Arxiv URL (absolute link)
        abs_url = f"https://arxiv.org/abs/{paper.get('id', '')}"
        paper["url"] = abs_url

        # PDF URL
        pdf_url = f"https://arxiv.org/pdf/{paper.get('id', '')}"
        paper["pdf_url"] = pdf_url

        papers.append(paper)

    return papers


def search_arxiv(
    tags: list[str],
    max_results: int = 10,
    sort_by: str = "submittedDate",
    sort_order: str = "descending",
) -> dict:
    """Search arxiv for papers matching the given tags.

    When tags is empty, queries all subjects (no category filter).
    Returns a dict with keys: papers, categories_used, tags_requested, total_results.
    If tag resolution fails, returns empty papers list.
    """
    categories = resolve_tags(tags)

    result: dict = {
        "tags_requested": tags,
        "categories_used": categories,
        "papers": [],
        "total_results": 0,
        "error": None,
    }

    # If tags were provided but none resolved → error
    if tags and not categories:
        result["error"] = f"No arxiv subject categories found for tags: {tags}"
        return result

    query = build_query(categories)
    params = {
        "search_query": query,
        "start": 0,
        "max_results": max_results,
        "sortBy": sort_by,
        "sortOrder": sort_order,
    }
    url = f"{API_BASE}?{urllib.parse.urlencode(params)}"

    xml_text = None
    last_error = None
    for attempt in range(3):
        try:
            req = urllib.request.Request(url, headers={"User-Agent": "Aman-Arxiv-Skill/1.0"})
            with urllib.request.urlopen(req, timeout=30) as resp:
                xml_text = resp.read().decode("utf-8")
                break
        except urllib.error.HTTPError as e:
            if e.code == 429 or e.code == 503:
                # Rate-limited or server busy — backoff and retry
                last_error = e
                wait = (attempt + 1) * 5  # 5s, 10s, 15s
                time.sleep(wait)
                continue
            result["error"] = f"Arxiv API request failed: {e}"
            return result
        except Exception as e:
            last_error = e
            time.sleep(3)
            continue

    if xml_text is None:
        result["error"] = f"Arxiv API request failed after 3 retries: {last_error}"
        return result

    papers = parse_atom(xml_text)

    # Extract total results from opensearch if available
    root = ET.fromstring(xml_text)
    total_el = root.find(f"{{{OPENSEARCH_NS}}}totalResults")
    if total_el is not None and total_el.text:
        result["total_results"] = int(total_el.text)

    result["papers"] = papers
    return result


def format_output(result: dict) -> str:
    """Pretty-print results for terminal display."""
    if result.get("error") and result.get("tags_requested"):
        # Only show as error if user actually requested tags that failed
        return json.dumps(result, ensure_ascii=False, indent=2)

    papers = result["papers"]
    if not papers:
        cats = result.get("categories_used", [])
        tags = result.get("tags_requested", [])
        if cats or not tags:
            # No results, but query was valid — could be browse mode or empty result
            return json.dumps(result, ensure_ascii=False, indent=2)
        return json.dumps(result, ensure_ascii=False, indent=2)

    lines = []
    for i, p in enumerate(papers, 1):
        authors = ", ".join(p.get("authors", [])[:3])
        if len(p.get("authors", [])) > 3:
            authors += " et al."
        lines.append(f"[{i}] {p.get('title', 'N/A')}")
        lines.append(f"    Authors: {authors}")
        lines.append(f"    Categories: {', '.join(p.get('categories', []))}")
        lines.append(f"    Published: {p.get('published', 'N/A')}")
        lines.append(f"    URL: {p.get('url', 'N/A')}")
        lines.append(f"    Abstract: {p.get('summary', '')[:300]}...")
        lines.append("")

    return "\n".join(lines)


def main():
    args = sys.argv[1:]
    if "--help" in args or "-h" in args:
        print(__doc__)
        sys.exit(0)

    # Parse -- flags
    tags: list[str] = []
    max_results: Optional[int] = None  # None → choose default based on mode
    sort_by = "submittedDate"
    json_output = False

    i = 0
    while i < len(args):
        arg = args[i]
        if arg == "--max" and i + 1 < len(args):
            max_results = int(args[i + 1])
            i += 2
        elif arg == "--sort" and i + 1 < len(args):
            sort_by = args[i + 1]
            i += 2
        elif arg == "--json":
            json_output = True
            i += 1
        elif arg.startswith("--"):
            i += 1
        else:
            tags.append(arg)
            i += 1

    # Default max_results: 20 for browse-all mode, 10 for targeted search
    if max_results is None:
        max_results = 20 if not tags else 10

    result = search_arxiv(tags, max_results=max_results, sort_by=sort_by)

    if json_output:
        print(json.dumps(result, ensure_ascii=False, indent=2))
    else:
        print(format_output(result))


if __name__ == "__main__":
    main()
