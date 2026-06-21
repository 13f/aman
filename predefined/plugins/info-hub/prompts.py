#!/usr/bin/env python3
"""
Info-hub prompt templates — all LLM prompt text lives here, not in Rust.

Usage:
  python3 prompts.py <method> '<json_args>'

Methods:
  scoring     — Build scoring prompt (relevance/quality/timeliness, 1-10)
  tagging     — Build tagging prompt (category + keywords)
  summary     — Build summarization prompt (title_zh, summary, reason)
  highlights  — Build highlights prompt (trend analysis)

Each method accepts article data as JSON and returns:
  {"system": "...", "user": "..."}
"""

from __future__ import annotations

import json
import sys
from typing import Optional

DESCRIPTION_MAX_LEN = 384


# ═══════════════════════════════════════════════════════════════════════════
# Helpers
# ═══════════════════════════════════════════════════════════════════════════

def _truncate(text: str, max_len: int = DESCRIPTION_MAX_LEN) -> str:
    if len(text) <= max_len:
        return text
    return text[: max_len - 3] + "..."


# ═══════════════════════════════════════════════════════════════════════════
# System prompts (self-evolution hooks — agent can rewrite these)
# ═══════════════════════════════════════════════════════════════════════════

SYSTEM_SCORING = "你是一个技术内容策展人，正在为一份面向技术爱好者的每日精选摘要筛选文章。"

SYSTEM_TAGGING = "你是一个技术内容分类专家，负责快速识别文章所属的行业/领域。"

SYSTEM_SUMMARY = "你是一个技术内容摘要专家。"

SYSTEM_HIGHLIGHTS = "你是一个技术趋势分析专家。"


# ═══════════════════════════════════════════════════════════════════════════
# Scoring prompt
# ═══════════════════════════════════════════════════════════════════════════

def build_scoring(articles: list[dict]) -> dict:
    """Build scoring prompt: relevance, quality, timeliness (1-10)."""
    articles_list = _format_articles_scoring(articles)
    user = f"""请对以下文章进行三个维度的评分（1-10 整数，10 分最高）。

每篇文章前的 [category | keywords] 标签已预先标注，评分时请结合这些标签判断文章在所属领域内的价值。

## 评分维度

### 1. 相关性 (relevance) - 对技术/编程/AI/互联网从业者的价值
- 10: 所有技术人都应该知道的重大事件/突破
- 7-9: 对大部分技术从业者有价值
- 4-6: 对特定技术领域有价值
- 1-3: 与技术行业关联不大

### 2. 质量 (quality) - 文章本身的深度和写作质量
- 10: 深度分析，原创洞见，引用丰富
- 7-9: 有深度，观点独到
- 4-6: 信息准确，表达清晰
- 1-3: 浅尝辄止或纯转述

### 3. 时效性 (timeliness) - 当前是否值得阅读
- 10: 正在发生的重大事件/刚发布的重要工具
- 7-9: 近期热点相关
- 4-6: 常青内容，不过时
- 1-3: 过时或无时效价值

## 待评分文章

{articles_list}

请严格按 JSON 格式返回，不要包含 markdown 代码块或其他文字：
{{
  "results": [
    {{
      "index": 0,
      "relevance": 8,
      "quality": 7,
      "timeliness": 9
    }}
  ]
}}"""
    return {"system": SYSTEM_SCORING, "user": user}


def _format_articles_scoring(articles: list[dict]) -> str:
    parts = []
    for a in articles:
        tag_parts = []
        cat = a.get("category", "")
        kws = a.get("keywords", [])
        if cat and kws:
            tag_parts.append(f"[{cat} | {', '.join(kws)}] ")
        elif cat:
            tag_parts.append(f"[{cat}] ")
        elif kws:
            tag_parts.append(f"[{', '.join(kws)}] ")
        tag_line = "".join(tag_parts)
        parts.append(
            f"Index {a['index']}: {tag_line}[{a.get('source_name', '')}] {a['title']}\n"
            f"{_truncate(a.get('description', ''))}"
        )
    return "\n\n---\n\n".join(parts)


# ═══════════════════════════════════════════════════════════════════════════
# Tagging prompt
# ═══════════════════════════════════════════════════════════════════════════

def build_tagging(articles: list[dict]) -> dict:
    """Build tagging prompt: category assignment + keyword extraction."""
    articles_list = _format_articles_simple(articles)
    user = f"""请为每篇文章分配一个分类标签，并提取 1-3 个关键词。

## 分类标签
根据文章内容自由选择一个最合适的分类标签（用英文，简短，如 "ai-ml", "security", "engineering", "tools", "opinion", "linux", "rust", "database", "frontend", "career" 等，也可以自创更精确的分类）。

## 关键词提取
提取 1-3 个最能代表文章主题的关键词（用英文，简短，如 "Rust", "LLM", "database", "performance"）。

## 待分类文章

{articles_list}

请严格按 JSON 格式返回，不要包含 markdown 代码块或其他文字：
{{
  "results": [
    {{
      "index": 0,
      "category": "engineering",
      "keywords": ["Rust", "compiler"]
    }}
  ]
}}"""
    return {"system": SYSTEM_TAGGING, "user": user}


def _format_articles_simple(articles: list[dict]) -> str:
    parts = []
    for a in articles:
        parts.append(
            f"Index {a['index']}: [{a.get('source_name', '')}] {a['title']}\n"
            f"{_truncate(a.get('description', ''))}"
        )
    return "\n\n---\n\n".join(parts)


# ═══════════════════════════════════════════════════════════════════════════
# Summary prompt
# ═══════════════════════════════════════════════════════════════════════════

def build_summary(articles: list[dict], lang: str = "zh") -> dict:
    """Build summarization prompt: title_zh, summary, reason."""
    articles_list = _format_articles_summary(articles)
    lang_instruction = (
        "请用中文撰写摘要和推荐理由。如果原文是英文，请翻译为中文。标题翻译也用中文。"
        if lang == "zh"
        else "Write summaries, reasons, and title translations in English."
    )
    user = f"""请为以下文章完成三件事：

1. **中文标题** (title_zh): 将英文标题翻译成自然的中文。如果原标题已经是中文则保持不变。
2. **摘要** (summary): 4-6 句话的结构化摘要，让读者不点进原文也能了解核心内容。包含：
   - 文章讨论的核心问题或主题（1 句）
   - 关键论点、技术方案或发现（2-3 句）
   - 结论或作者的核心观点（1 句）
3. **推荐理由** (reason): 1 句话说明"为什么值得读"，区别于摘要（摘要说"是什么"，推荐理由说"为什么"）。

{lang_instruction}

摘要要求：
- 直接说重点，不要用"本文讨论了..."、"这篇文章介绍了..."这种开头
- 包含具体的技术名词、数据、方案名称或观点
- 保留关键数字和指标（如性能提升百分比、用户数、版本号等）
- 如果文章涉及对比或选型，要点出比较对象和结论
- 目标：读者花 30 秒读完摘要，就能决定是否值得花 10 分钟读原文

## 待摘要文章

{articles_list}

请严格按 JSON 格式返回：
{{
  "results": [
    {{
      "index": 0,
      "title_zh": "中文翻译的标题",
      "summary": "摘要内容...",
      "reason": "推荐理由..."
    }}
  ]
}}"""
    return {"system": SYSTEM_SUMMARY, "user": user}


def _format_articles_summary(articles: list[dict]) -> str:
    parts = []
    for a in articles:
        parts.append(
            f"Index {a['index']}: [{a.get('source_name', '')}] {a['title']}\n"
            f"{_truncate(a.get('description', ''), 600)}"
        )
    return "\n\n---\n\n".join(parts)


# ═══════════════════════════════════════════════════════════════════════════
# Highlights prompt
# ═══════════════════════════════════════════════════════════════════════════

def build_highlights(articles_json: str, lang: str = "zh") -> dict:
    """Build highlights prompt: trend analysis summary."""
    lang_note = "用中文回答。" if lang == "zh" else "Write in English."
    user = f"""根据以下今日精选技术文章列表，写一段 3-5 句话的"今日看点"总结。
要求：
- 提炼出今天技术圈的 2-3 个主要趋势或话题
- 不要逐篇列举，要做宏观归纳
- 风格简洁有力，像新闻导语
{lang_note}

文章列表：
{articles_json}

直接返回纯文本总结，不要 JSON，不要 markdown 格式。"""
    return {"system": SYSTEM_HIGHLIGHTS, "user": user}


# ═══════════════════════════════════════════════════════════════════════════
# CLI dispatch
# ═══════════════════════════════════════════════════════════════════════════

METHODS = {
    "scoring": build_scoring,
    "tagging": build_tagging,
    "summary": build_summary,
    "highlights": build_highlights,
}


def main() -> None:
    if len(sys.argv) < 2:
        print(json.dumps({"error": "missing method name"}), file=sys.stderr)
        sys.exit(1)

    method = sys.argv[1]
    args_raw = sys.argv[2] if len(sys.argv) >= 3 else "{}"

    try:
        args = json.loads(args_raw)
    except json.JSONDecodeError as e:
        print(json.dumps({"error": f"invalid JSON args: {e}"}), file=sys.stderr)
        sys.exit(1)

    func = METHODS.get(method)
    if func is None:
        print(json.dumps({"error": f"unknown method: {method}"}), file=sys.stderr)
        sys.exit(1)

    try:
        if method == "highlights":
            result = func(
                args.get("articles_json", "[]"),
                args.get("lang", "zh"),
            )
        elif method == "summary":
            result = func(
                args.get("articles", []),
                args.get("lang", "zh"),
            )
        else:
            result = func(args.get("articles", []))
        print(json.dumps(result, ensure_ascii=False))
    except Exception as e:
        print(json.dumps({"error": str(e)}), file=sys.stderr)
        sys.exit(1)


if __name__ == "__main__":
    main()
