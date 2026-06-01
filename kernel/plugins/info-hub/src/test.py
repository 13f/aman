"""
Integration test: read articles from fusion DB, enrich content, tag, score.

Flow:
  1. Read top N from fusion DB
  2. Enrich: fetch URL content if description too short
  3. Tag + Score via aman gateway (uses ai.tag_and_score)

Usage:
  python3 test.py [--db-path /path/to/fusion.db] [--top-n 10] [--port 9999]
"""

import argparse
import sys
from pathlib import Path

# Ensure info-hub plugin dir is on sys.path
_plugin_dir = str(Path.home() / ".aman" / "plugins" / "info-hub")
if _plugin_dir not in sys.path:
    sys.path.insert(0, _plugin_dir)

from common import enrich_articles
from fusion import search_articles

# Import after path setup — triggers gateway health check
from ai import tag_and_score

MIN_CONTENT_LEN = 100


def main():
    parser = argparse.ArgumentParser(description="Test info-hub pipeline")
    parser.add_argument("--db-path", default="/Users/jerin/apps/fusion/fusion.db")
    parser.add_argument("--top-n", type=int, default=10)
    args = parser.parse_args()

    # ── Step 1: Read top N from fusion DB ──────────────────────────────
    print(f"[1/3] Reading top {args.top_n} articles from {args.db_path}...")
    articles = search_articles(args.db_path, query="", limit=args.top_n, offset=0)
    print(f"       Found {len(articles)} articles")

    if not articles:
        print("ERROR: No articles found")
        sys.exit(1)

    for i, a in enumerate(articles):
        content_len = len(a.get("raw", {}).get("content", ""))
        print(f"  [{i}] {a['title'][:60]} | {a['source']} | content={content_len}c")

    # ── Step 2: Enrich content ─────────────────────────────────────────
    print(f"\n[2/3] Enriching content (fetch URLs if < {MIN_CONTENT_LEN} chars)...")
    enriched = enrich_articles(articles, min_content_len=MIN_CONTENT_LEN)
    print(f"       Enriched {enriched}/{len(articles)} articles")

    # ── Step 3: Tag + Score via gateway ────────────────────────────────
    print(f"\n[3/3] Tagging and scoring via gateway...")
    articles = tag_and_score(articles)

    # ── Summary ─────────────────────────────────────────────────────────
    print(f"\n{'='*70}")
    print(f"RESULTS (sorted by score)")
    print(f"{'='*70}")
    for i, a in enumerate(articles):
        raw = a.get("raw", {})
        rel = raw.get("relevance", 0)
        qual = raw.get("quality", 0)
        ts = raw.get("timeliness", 0)
        total = a.get("score", 0)
        cat = raw.get("category", "?")
        kws = ", ".join(raw.get("keywords", []))
        print(f"\n  #{i+1}  [{cat}] {a['title']}")
        print(f"       Score: R{rel}/Q{qual}/T{ts} = {total}/30")
        print(f"       Keywords: {kws}")
        print(f"       {a['url']}")

    print(f"\nDone.")


if __name__ == "__main__":
    main()
