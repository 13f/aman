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

from common import get_gateway_url

_REQUEST_TIMEOUT = 120  # seconds (LLM calls can be slow)


# ── Gateway client ────────────────────────────────────────────────────

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
    resp = requests.post(url, headers=headers, json=params, timeout=_REQUEST_TIMEOUT)

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

def summarize_articles(articles: list, lang: str = "zh") -> list:
    """Summarize articles with title translation, summary, and reason.

    Calls info_summarize_articles tool on the aman gateway.

    Args:
        articles: List of dicts with {index, title, description, source_name?, link?}
        lang: Output language (zh or en)

    Returns:
        List of dicts with {index, title_zh, summary, reason}
    """
    if not articles:
        return []

    output = _call_tool("info_summarize_articles", {
        "articles": articles,
        "lang": lang,
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
