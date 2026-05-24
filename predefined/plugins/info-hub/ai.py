"""
AI processing for info-hub scripts.

Calls aman gateway tool execution endpoint (POST /tools/{name}/execute).
Gateway URL is read from ~/.aman/config.yaml (gateway.port).
The Rust server resolves LLM config (memory.llm + providers) — scripts
never see API keys.

Usage:
    from ai import tag_articles, score_articles, summarize_articles, generate_highlights

    tags = tag_articles([{"index": 0, "title": "...", "description": "..."}])
    scores = score_articles([{"index": 0, "title": "...", "description": "..."}])
"""

from typing import Optional

import requests

from common import article_to_api_input, get_gateway_url

_REQUEST_TIMEOUT = 120  # seconds (LLM calls can be slow)


# ── Gateway client ────────────────────────────────────────────────────

def _gateway_session() -> requests.Session:
    """Create a requests session that bypasses proxy for localhost gateway."""
    session = requests.Session()
    session.trust_env = False
    return session


def _call_tool(tool_name: str, params: dict) -> dict:
    """Call an info-hub tool via the aman gateway HTTP API.

    Args:
        tool_name: e.g. "info_tag_articles", "info_score_articles"
        params: Tool parameters dict

    Returns:
        Parsed output from the tool

    Raises:
        RuntimeError: on HTTP error, tool not found, or tool execution failure
    """
    headers = {"Content-Type": "application/json"}
    url = f"{get_gateway_url()}/tools/{tool_name}/execute"
    resp = _gateway_session().post(url, headers=headers, json=params, timeout=_REQUEST_TIMEOUT)

    if resp.status_code >= 400:
        detail = ""
        try:
            detail = resp.json().get("error", resp.text[:500])
        except Exception:
            detail = resp.text[:500]
        raise RuntimeError(f"Tool {tool_name}: HTTP {resp.status_code} — {detail}")

    data = resp.json()
    if "error" in data:
        raise RuntimeError(f"Tool {tool_name}: {data['error']}")

    return data.get("output", {})


# ── Tagging ───────────────────────────────────────────────────────────

def tag_articles(articles: list) -> list:
    """Tag articles with category/domain label and extract keywords.

    Calls info_tag_articles tool on the aman gateway.

    Args:
        articles: List of dicts with {index, title, description, source_name?, link?}

    Returns:
        List of dicts with {index, category, keywords}
    """
    if not articles:
        return []

    output = _call_tool("info_tag_articles", {"articles": articles})
    return output.get("results", [])


# ── Scoring ───────────────────────────────────────────────────────────

def score_articles(articles: list) -> list:
    """Score articles on relevance, quality, and timeliness (1-10).

    Calls info_score_articles tool on the aman gateway.

    Args:
        articles: List of dicts with {index, title, description, source_name?, link?}

    Returns:
        List of dicts with {index, relevance, quality, timeliness}
    """
    if not articles:
        return []

    output = _call_tool("info_score_articles", {"articles": articles})
    return output.get("results", [])


# ── Summarization ─────────────────────────────────────────────────────

def summarize_articles(articles: list, lang: str = "zh", min_score: int = 0) -> list:
    """Summarize articles with title translation, summary, and reason.

    Calls info_summarize_articles tool on the aman gateway.

    Args:
        articles: List of dicts with {index, title, description, source_name?, link?,
                  relevance?, quality?, timeliness?}
        lang: Output language (zh or en)
        min_score: Minimum total score (relevance+quality+timeliness, 3-30).
                   Articles below this get a fallback entry instead of consuming LLM tokens.
                   Default 0 disables filtering.

    Returns:
        List of dicts with {index, title_zh, summary, reason}
    """
    if not articles:
        return []

    output = _call_tool("info_summarize_articles", {
        "articles": articles,
        "lang": lang,
        "min_score": min_score,
    })
    return output.get("results", [])


# ── Highlights ────────────────────────────────────────────────────────

def generate_highlights(articles_json: str, lang: str = "zh") -> str:
    """Generate a 3-5 sentence trend overview from a list of articles.

    Calls info_generate_highlights tool on the aman gateway.

    Args:
        articles_json: JSON string of the article list
        lang: Output language (zh or en)

    Returns:
        Plain text overview
    """
    output = _call_tool("info_generate_highlights", {
        "articles_json": articles_json,
        "lang": lang,
    })
    if isinstance(output, str):
        return output.strip()
    return str(output).strip()


# ── Orchestration ──────────────────────────────────────────────────────

def score_and_summarize(articles: list, lang: str = "zh", min_score: int = 18):
    """Score articles, then summarize those meeting the score threshold.

    1. Calls ``info_score_articles`` to get relevance/quality/timeliness (1-10 each).
    2. Attaches scores to each article dict.
    3. Calls ``info_summarize_articles``, which filters by *min_score* and only
       spends LLM tokens on articles worth summarizing.

    Args:
        articles: List of dicts with {index, title, description, source_name?, link?}
        lang: Output language (zh or en)
        min_score: Minimum total score (3-30, default 18 = avg 6 per dimension)

    Returns:
        (scores, summaries) tuple:
        - scores: [{index, relevance, quality, timeliness}]
        - summaries: [{index, title_zh, summary, reason}]
    """
    if not articles:
        return [], []

    scores = score_articles(articles)

    # Attach scores so the summarizer can filter
    score_map = {r["index"]: r for r in scores}
    scored_articles = []
    for a in articles:
        s = score_map.get(a["index"], {})
        scored_articles.append({
            **a,
            "relevance": s.get("relevance", 0),
            "quality": s.get("quality", 0),
            "timeliness": s.get("timeliness", 0),
        })

    summaries = summarize_articles(scored_articles, lang=lang, min_score=min_score)
    return scores, summaries


def tag_and_score(articles: list) -> list:
    """Tag and score a list of articles — out-of-the-box pipeline.

    1. Tag articles via ``info_tag_articles`` (category + keywords, max 3).
    2. Score articles via ``info_score_articles`` with tags as context
       (relevance, quality, timeliness, 1-10 each).
    3. Merge results into each article's ``score`` and ``raw`` fields.
    4. Sort by score descending.

    Each article should have: ``title``, ``url``, ``source``, ``summary``,
    and ``raw.content``. Use :func:`common.article_to_api_input` to
    normalise the format.

    Returns the same list, mutated and sorted.
    """
    if not articles:
        return articles

    # Tag
    tag_input = [article_to_api_input(a, i) for i, a in enumerate(articles)]
    tag_output = _call_tool("info_tag_articles", {"articles": tag_input})
    for tr in tag_output.get("results", []):
        idx = tr.get("index", -1)
        if 0 <= idx < len(articles):
            articles[idx].setdefault("raw", {})["category"] = tr.get("category", "other")
            articles[idx].setdefault("raw", {})["keywords"] = tr.get("keywords", [])[:3]

    # Score with tags
    score_input = []
    for i, a in enumerate(articles):
        inp = article_to_api_input(a, i)
        raw = a.get("raw", {})
        inp["category"] = raw.get("category", "")
        inp["keywords"] = raw.get("keywords", [])
        score_input.append(inp)

    score_output = _call_tool("info_score_articles", {"articles": score_input})
    for sr in score_output.get("results", []):
        idx = sr.get("index", -1)
        if 0 <= idx < len(articles):
            r, q, t = sr.get("relevance", 0), sr.get("quality", 0), sr.get("timeliness", 0)
            articles[idx]["score"] = r + q + t
            raw = articles[idx].setdefault("raw", {})
            raw["relevance"] = r
            raw["quality"] = q
            raw["timeliness"] = t

    articles.sort(key=lambda a: a.get("score", 0), reverse=True)
    return articles
