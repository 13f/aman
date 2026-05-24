"""
Shared utilities for info-hub Python scripts.

- Aman config loading (~/.aman/config.yaml)
- Text helpers: HTML stripping, truncation, date parsing
- DB adapter protocol: stdin JSON → stdout JSON
"""

import json
import os
import re
import sys
from datetime import datetime, timezone
from pathlib import Path
from typing import Optional
from urllib.request import Request, urlopen
from urllib.error import URLError
import ssl


# ── Config ────────────────────────────────────────────────────────────

def aman_config_path() -> Path:
    """Return path to aman config file."""
    aman_home = os.environ.get("AMAN_HOME", os.path.join(Path.home(), ".aman"))
    return Path(aman_home) / "config.yaml"


def load_aman_config() -> dict:
    """Load the full aman config from ~/.aman/config.yaml."""
    path = aman_config_path()
    if not path.exists():
        return {}
    import yaml
    with open(path) as f:
        return yaml.safe_load(f) or {}


def get_gateway_url() -> str:
    """Resolve the gateway base URL from config.

    Reads gateway.port from ~/.aman/config.yaml. Falls back to localhost:9999.
    """
    cfg = load_aman_config()
    gateway = cfg.get("gateway", {})
    port = gateway.get("port", 9999)
    return f"http://localhost:{port}"


def get_llm_config() -> Optional[dict]:
    """Resolve LLM config from memory.llm + providers section.

    DEPRECATED for AI use — ai.py now calls gateway tools instead.
    Only use this for direct LLM access if the gateway is unavailable.

    Returns dict with keys: base_url, api_key, model
    or None if no LLM is configured.
    """
    cfg = load_aman_config()
    memory = cfg.get("memory", {})
    llm = memory.get("llm")
    if not llm:
        return None

    provider_key = llm.get("provider", "")
    model_id = llm.get("model", "")

    providers = cfg.get("providers", {})
    provider = providers.get(provider_key, {})

    base_url = provider.get("base_url", "https://api.openai.com/v1")
    api_key = provider.get("api_key") or os.environ.get(
        f"AMAN_PROVIDER_{provider_key.upper()}_API_KEY"
    )

    # Resolve model: lookup in provider's models list
    api_model = model_id
    for entry in provider.get("models", []):
        if entry.get("id") == model_id:
            api_model = entry.get("model_id", model_id)
            break

    return {
        "base_url": base_url.rstrip("/"),
        "api_key": api_key,
        "model": api_model,
    }


# ── Text helpers ──────────────────────────────────────────────────────

def strip_html(text: str) -> str:
    """Remove HTML tags and decode common entities."""
    text = re.sub(r"<[^>]*>", "", text)
    text = text.replace("&amp;", "&")
    text = text.replace("&lt;", "<")
    text = text.replace("&gt;", ">")
    text = text.replace("&quot;", '"')
    text = text.replace("&#39;", "'")
    text = text.replace("&nbsp;", " ")
    text = re.sub(r"&#(\d+);", lambda m: chr(int(m.group(1))), text)
    return text.strip()


def truncate_description(text: str, max_len: int = 384) -> str:
    """Truncate text to max_len, breaking at sentence boundary."""
    if len(text) <= max_len:
        return text
    sliced = text[:max_len]
    m = re.search(r"[.!?。！？]", sliced[::-1])
    if m:
        end = max_len - m.start()
        return sliced[:end].rstrip()
    # Fallback: last space, only if not too short
    pos = sliced.rfind(" ")
    if pos > max_len * 3 // 5:
        return sliced[:pos]
    return sliced


def parse_date(date_str: str) -> Optional[str]:
    """Parse a date string to ISO 8601, or return None."""
    if not date_str:
        return None
    try:
        # Try ISO 8601 first
        dt = datetime.fromisoformat(date_str.replace("Z", "+00:00"))
        return dt.isoformat()
    except (ValueError, TypeError):
        pass
    # Try common RFC formats
    for fmt in [
        "%a, %d %b %Y %H:%M:%S %z",
        "%a, %d %b %Y %H:%M:%S %Z",
        "%Y-%m-%dT%H:%M:%S%z",
        "%Y-%m-%dT%H:%M:%SZ",
        "%Y-%m-%d %H:%M:%S",
        "%Y-%m-%d",
    ]:
        try:
            return datetime.strptime(date_str, fmt).isoformat()
        except ValueError:
            continue
    return None


# ── DB adapter protocol ───────────────────────────────────────────────

def read_stdin_query() -> dict:
    """Read JSON query from stdin (DB adapter protocol)."""
    raw = sys.stdin.read()
    if not raw.strip():
        return {}
    return json.loads(raw)


def write_stdout_result(items: list) -> None:
    """Write JSON array result to stdout."""
    json.dump(items, sys.stdout, ensure_ascii=False, indent=2)


def expand_tilde(path: str) -> str:
    """Expand ~ to user home directory."""
    if path.startswith("~"):
        return os.path.expanduser(path)
    return path


# ── Web fetch ──────────────────────────────────────────────────────────

def fetch_article_content(url: str, timeout: int = 8) -> str:
    """Fetch a URL and extract the main text content.

    Returns extracted plain text, or empty string on any error.
    """
    if not url or not url.startswith("http"):
        return ""

    try:
        ctx = ssl.create_default_context()
        req = Request(url, headers={
            "User-Agent": "Mozilla/5.0 (compatible; RSS Reader)",
            "Accept": "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8",
        })
        with urlopen(req, timeout=timeout, context=ctx) as resp:
            if resp.status >= 400:
                return ""
            # Read up to 500KB
            html = resp.read(500_000).decode("utf-8", errors="replace")
        return extract_main_content(html)
    except (URLError, OSError, ValueError, UnicodeDecodeError):
        return ""


def extract_main_content(html: str) -> str:
    """Extract readable text from HTML, stripping navigation and boilerplate."""
    # Remove non-content elements
    for tag in ("script", "style", "nav", "header", "footer", "aside"):
        html = re.sub(rf"<{tag}[^>]*>[\s\S]*?</{tag}>", " ", html, flags=re.IGNORECASE)

    # Strip all remaining HTML tags
    text = re.sub(r"<[^>]+>", " ", html)

    # Collapse whitespace
    text = re.sub(r"\s+", " ", text).strip()

    # Limit to 1500 chars for AI context
    return text[:1500]


# ── Article enrichment ────────────────────────────────────────────────

def enrich_articles(
    articles: list,
    min_content_len: int = 100,
    timeout: int = 8,
) -> int:
    """Ensure each article has sufficient content for AI processing.

    For articles whose ``raw.content`` is shorter than *min_content_len*,
    fetch the article's URL and extract the main text. The fetched content
    is written back into ``raw.content`` and ``summary``.

    Returns the number of articles that were enriched.
    """
    enriched = 0
    for a in articles:
        content = a.get("raw", {}).get("content", "")
        if len(content) >= min_content_len:
            continue
        url = a.get("url", "")
        if not url:
            continue
        fetched = fetch_article_content(url, timeout=timeout)
        if fetched:
            a.setdefault("raw", {})["content"] = fetched
            a["summary"] = fetched
            enriched += 1
    return enriched


def article_to_api_input(article: dict, index: int) -> dict:
    """Convert a fusion-style article dict to the standard gateway API format.

    Returns ``{index, title, description, source_name, link}`` suitable for
    ``info_tag_articles``, ``info_score_articles``, etc.
    """
    raw = article.get("raw", {})
    return {
        "index": index,
        "title": article.get("title", ""),
        "description": raw.get("content", "") or article.get("summary", ""),
        "source_name": article.get("source", ""),
        "link": article.get("url", ""),
    }


# ── Score filtering ────────────────────────────────────────────────────

def filter_by_score(articles: list, min_score: int = 18) -> tuple[list, list]:
    """Filter articles by total score (relevance + quality + timeliness).

    Each article dict must have ``relevance``, ``quality``, ``timeliness``
    integer fields (1-10 each, from ``info_score_articles``).

    Returns ``(passing, skipped)`` — two lists of the same dicts.
    """
    passing = []
    skipped = []
    for a in articles:
        total = a.get("relevance", 0) + a.get("quality", 0) + a.get("timeliness", 0)
        if total >= min_score:
            passing.append(a)
        else:
            skipped.append(a)
    return passing, skipped
