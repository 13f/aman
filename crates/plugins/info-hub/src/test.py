"""
Integration test: read articles from fusion DB, enrich content, tag, score.

Flow:
  1. Read top 10 from fusion DB
  2. Enrich: fetch URL content if description too short
  3. Tag via aman gateway (info_tag_articles) — max 3 keywords each
  4. Score via aman gateway (info_score_articles) with tags as context

Usage:
  python3 test.py [--db-path /path/to/fusion.db] [--top-n 10] [--port 9999]
"""

import argparse
import json
import sys
import time
from pathlib import Path

import requests

# ── Ensure info-hub plugin dir is on sys.path ──────────────────────────
_plugin_dir = str(Path.home() / ".aman" / "plugins" / "info-hub")
if _plugin_dir not in sys.path:
    sys.path.insert(0, _plugin_dir)

from common import fetch_article_content, extract_main_content
from fusion import search_articles, open_db, _is_fusion_db

MIN_CONTENT_LEN = 100
REQUEST_TIMEOUT = 180


# ── Gateway client ─────────────────────────────────────────────────────


def _gateway_session() -> requests.Session:
    """Create a requests session that bypasses proxy for localhost."""
    session = requests.Session()
    session.trust_env = False  # bypass system proxy for local gateway
    return session


def call_tool(port: int, tool_name: str, params: dict) -> dict:
    """Call an aman gateway tool via HTTP and return parsed output."""
    url = f"http://localhost:{port}/tools/{tool_name}/execute"
    resp = _gateway_session().post(
        url,
        headers={"Content-Type": "application/json"},
        json=params,
        timeout=REQUEST_TIMEOUT,
    )
    resp.raise_for_status()
    data = resp.json()
    if "error" in data:
        raise RuntimeError(f"Tool {tool_name}: {data['error']}")
    return data.get("output", {})


# ── Main flow ──────────────────────────────────────────────────────────


def main():
    parser = argparse.ArgumentParser(description="Test info-hub pipeline")
    parser.add_argument("--db-path", default="/Users/jerin/apps/fusion/fusion.db")
    parser.add_argument("--top-n", type=int, default=10)
    parser.add_argument("--port", type=int, default=9999)
    args = parser.parse_args()

    port = args.port
    top_n = args.top_n

    # ── Step 1: Read top N from fusion DB ──────────────────────────────
    print(f"[1/4] Reading top {top_n} articles from {args.db_path}...")
    articles = search_articles(args.db_path, query="", limit=top_n, offset=0)
    print(f"       Found {len(articles)} articles")

    if not articles:
        print("ERROR: No articles found")
        sys.exit(1)

    for i, a in enumerate(articles):
        content_len = len(a["raw"].get("content", ""))
        print(f"  [{i}] {a['title'][:60]} | {a['source']} | content={content_len}c")

    # ── Step 2: Enrich content ─────────────────────────────────────────
    print(f"\n[2/4] Enriching content (fetch URLs if < {MIN_CONTENT_LEN} chars)...")
    enriched = 0
    for i, a in enumerate(articles):
        content = a["raw"].get("content", "")
        if len(content) >= MIN_CONTENT_LEN:
            continue
        url = a.get("url", "")
        if not url:
            continue
        print(f"       Fetching [{i}] {url[:80]}...")
        fetched = fetch_article_content(url)
        if fetched:
            a["raw"]["content"] = fetched
            a["summary"] = fetched
            enriched += 1
            print(f"       -> {len(fetched)} chars extracted")
        else:
            print(f"       -> fetch failed, keeping original ({len(content)} chars)")

    print(f"       Enriched {enriched}/{len(articles)} articles")

    # ── Step 3: Tag via aman gateway ────────────────────────────────────
    print(f"\n[3/4] Tagging articles via gateway (port {port})...")
    tag_input = []
    for a in articles:
        tag_input.append({
            "index": a["raw"].get("item_key", a["url"]),
            "title": a["title"],
            "description": a["raw"].get("content", "") or a.get("summary", ""),
            "source_name": a["source"],
            "link": a["url"],
        })

    # Re-index for the tag call
    tag_index_map = []
    for i, a in enumerate(articles):
        tag_input[i]["index"] = i
        tag_index_map.append(a["raw"].get("item_key", a["url"]))

    t0 = time.time()
    tag_output = call_tool(port, "info_tag_articles", {"articles": tag_input})
    tag_elapsed = time.time() - t0

    tag_results = tag_output.get("results", [])
    print(f"       Got {len(tag_results)} tag results in {tag_elapsed:.1f}s")

    # Merge tags back into articles
    tag_by_index = {}
    for tr in tag_results:
        idx = tr.get("index", -1)
        if 0 <= idx < len(articles):
            articles[idx]["raw"]["category"] = tr.get("category", "other")
            articles[idx]["raw"]["keywords"] = tr.get("keywords", [])[:3]

    for i, a in enumerate(articles):
        cat = a["raw"].get("category", "?")
        kws = a["raw"].get("keywords", [])
        print(f"  [{i}] {cat:20s} | {', '.join(kws)}")

    # ── Step 4: Score via aman gateway (with tags) ──────────────────────
    print(f"\n[4/4] Scoring articles via gateway (port {port})...")
    score_input = []
    for i, a in enumerate(articles):
        score_input.append({
            "index": i,
            "title": a["title"],
            "description": a["raw"].get("content", "") or a.get("summary", ""),
            "source_name": a["source"],
            "link": a["url"],
            "category": a["raw"].get("category", ""),
            "keywords": a["raw"].get("keywords", []),
        })

    t0 = time.time()
    score_output = call_tool(port, "info_score_articles", {"articles": score_input})
    score_elapsed = time.time() - t0

    score_results = score_output.get("results", [])
    print(f"       Got {len(score_results)} score results in {score_elapsed:.1f}s")

    # Merge scores back
    for sr in score_results:
        idx = sr.get("index", -1)
        if 0 <= idx < len(articles):
            articles[idx]["score"] = (
                sr.get("relevance", 0) + sr.get("quality", 0) + sr.get("timeliness", 0)
            )
            articles[idx]["raw"]["relevance"] = sr.get("relevance", 0)
            articles[idx]["raw"]["quality"] = sr.get("quality", 0)
            articles[idx]["raw"]["timeliness"] = sr.get("timeliness", 0)

    # Sort by score desc
    articles.sort(key=lambda a: a.get("score", 0), reverse=True)

    # ── Summary ─────────────────────────────────────────────────────────
    print(f"\n{'='*70}")
    print(f"RESULTS (sorted by score)")
    print(f"{'='*70}")
    for i, a in enumerate(articles):
        rel = a["raw"].get("relevance", 0)
        qual = a["raw"].get("quality", 0)
        time_s = a["raw"].get("timeliness", 0)
        total = a.get("score", 0)
        cat = a["raw"].get("category", "?")
        kws = ", ".join(a["raw"].get("keywords", []))
        print(f"\n  #{i+1}  [{cat}] {a['title']}")
        print(f"       Score: R{rel}/Q{qual}/T{time_s} = {total}/30")
        print(f"       Keywords: {kws}")
        print(f"       {a['url']}")

    print(f"\nDone. Tag: {tag_elapsed:.1f}s | Score: {score_elapsed:.1f}s")


if __name__ == "__main__":
    main()
