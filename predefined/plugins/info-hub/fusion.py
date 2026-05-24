"""
Fusion DB adapter for info-hub.

DB adapter protocol:
  stdin:  {query, limit, offset, db_path}
  stdout: JSON array of InfoItems sorted by published desc

Standalone mode (--standalone):
  Full pipeline: query DB → score → summarize → generate highlights → output report.
  Reads LLM config from ~/.aman/config.yaml.
"""

import argparse
import json
import os
import sqlite3
import sys
from datetime import datetime, timezone
from pathlib import Path
from typing import Optional

from common import (
    expand_tilde,
    parse_date,
    read_stdin_query,
    strip_html,
    truncate_description,
    write_stdout_result,
)
from ai import generate_highlights, score_articles, summarize_articles, tag_articles

DEFAULT_TOP_N = 20


def open_db(db_path: str) -> sqlite3.Connection:
    """Open SQLite database, creating tables if they don't exist."""
    path = expand_tilde(db_path)
    os.makedirs(os.path.dirname(path) or ".", exist_ok=True)
    conn = sqlite3.connect(path)
    conn.row_factory = sqlite3.Row
    _ensure_tables(conn)
    return conn


def _ensure_tables(conn: sqlite3.Connection) -> None:
    conn.execute(
        """
        CREATE TABLE IF NOT EXISTS articles (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            title TEXT NOT NULL,
            link TEXT NOT NULL UNIQUE,
            pub_date TEXT,
            description TEXT,
            source_name TEXT DEFAULT '',
            source_url TEXT DEFAULT '',
            created_at TEXT DEFAULT (datetime('now')),
            score REAL DEFAULT 0,
            relevance INTEGER DEFAULT 0,
            quality INTEGER DEFAULT 0,
            timeliness INTEGER DEFAULT 0,
            category TEXT DEFAULT 'other',
            keywords TEXT DEFAULT '[]',
            title_zh TEXT DEFAULT '',
            summary TEXT DEFAULT '',
            reason TEXT DEFAULT ''
        )
    """
    )
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_articles_pub_date ON articles(pub_date DESC)"
    )
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_articles_score ON articles(score DESC)"
    )
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_articles_link ON articles(link)"
    )
    conn.commit()


# ── DB Adapter Protocol ───────────────────────────────────────────────

def search_articles(
    db_path: str,
    query: str = "",
    limit: int = 20,
    offset: int = 0,
) -> list[dict]:
    """Search articles in SQLite DB and return as InfoItem list."""
    conn = open_db(db_path)
    try:
        if query.strip():
            sql = """
                SELECT * FROM articles
                WHERE title LIKE ? OR description LIKE ? OR summary LIKE ?
                ORDER BY pub_date DESC
                LIMIT ? OFFSET ?
            """
            pattern = f"%{query}%"
            rows = conn.execute(sql, (pattern, pattern, pattern, limit, offset)).fetchall()
        else:
            sql = """
                SELECT * FROM articles
                ORDER BY score DESC, pub_date DESC
                LIMIT ? OFFSET ?
            """
            rows = conn.execute(sql, (limit, offset)).fetchall()
    finally:
        conn.close()

    results = []
    for row in rows:
        item = {
            "title": row["title"],
            "url": row["link"],
            "summary": row["summary"] or row["description"] or "",
            "published": row["pub_date"],
            "source": row["source_name"] or "fusion",
            "raw": {
                "score": row["score"],
                "relevance": row["relevance"],
                "quality": row["quality"],
                "timeliness": row["timeliness"],
                "category": row["category"],
                "keywords": json.loads(row["keywords"] or "[]"),
                "title_zh": row["title_zh"],
                "reason": row["reason"],
            },
        }
        results.append(item)

    return results


def upsert_articles(db_path: str, articles: list[dict]) -> int:
    """Insert or update articles in SQLite DB. Returns count of new articles."""
    conn = open_db(db_path)
    new_count = 0
    try:
        for a in articles:
            exists = conn.execute(
                "SELECT id FROM articles WHERE link = ?", (a.get("link", ""),)
            ).fetchone()
            if not exists:
                conn.execute(
                    """INSERT INTO articles
                       (title, link, pub_date, description, source_name, source_url)
                       VALUES (?, ?, ?, ?, ?, ?)""",
                    (
                        a.get("title", ""),
                        a.get("link", ""),
                        a.get("pubDate", ""),
                        strip_html(a.get("description", "")),
                        a.get("sourceName", ""),
                        a.get("sourceUrl", ""),
                    ),
                )
                new_count += 1
        conn.commit()
    finally:
        conn.close()
    return new_count


# ── Standalone pipeline ────────────────────────────────────────────────

def run_pipeline(
    db_path: str,
    top_n: int = DEFAULT_TOP_N,
    lang: str = "zh",
) -> dict:
    """Full standalone pipeline: search -> score -> summarize -> highlights."""
    articles = search_articles(db_path, query="", limit=100, offset=0)

    if not articles:
        return {
            "date": datetime.now(timezone.utc).strftime("%Y-%m-%d"),
            "total_found": 0,
            "articles": [],
            "highlights": "",
        }

    # Build input for AI processing
    ai_input = []
    for i, a in enumerate(articles):
        ai_input.append({
            "index": i,
            "title": a["title"],
            "description": a.get("summary", ""),
            "source_name": a["source"],
            "link": a["url"],
        })

    # Tag (category + keywords) — separate lighter LLM call
    tag_results = tag_articles(ai_input)
    tag_map = {r["index"]: r for r in tag_results}
    for i, a in enumerate(articles):
        if i in tag_map:
            a["raw"]["category"] = tag_map[i]["category"]
            a["raw"]["keywords"] = tag_map[i]["keywords"]

    # Score (relevance, quality, timeliness)
    score_results = score_articles(ai_input)
    score_map = {r["index"]: r for r in score_results}

    for i, a in enumerate(articles):
        if i in score_map:
            s = score_map[i]
            a["score"] = s["relevance"] + s["quality"] + s["timeliness"]
            a["raw"]["relevance"] = s["relevance"]
            a["raw"]["quality"] = s["quality"]
            a["raw"]["timeliness"] = s["timeliness"]

    # Sort by score desc, take top N
    articles.sort(key=lambda a: a.get("score", 0), reverse=True)
    top_articles = articles[:top_n]

    # Summarize top articles
    summary_input = []
    for i, a in enumerate(top_articles):
        summary_input.append({
            "index": i,
            "title": a["title"],
            "description": a.get("summary", ""),
            "source_name": a["source"],
            "link": a["url"],
        })

    summary_results = summarize_articles(summary_input, lang=lang)
    summary_map = {r["index"]: r for r in summary_results}

    for i, a in enumerate(top_articles):
        if i in summary_map:
            s = summary_map[i]
            a["raw"]["title_zh"] = s.get("title_zh", a["title"])
            a["raw"]["summary"] = s.get("summary", "")
            a["raw"]["reason"] = s.get("reason", "")
            a["summary"] = s.get("summary", "")

    # Persist scores and summaries
    conn = open_db(db_path)
    try:
        for a in top_articles:
            raw = a.get("raw", {})
            conn.execute(
                """UPDATE articles SET
                   score = ?, relevance = ?, quality = ?, timeliness = ?,
                   category = ?, keywords = ?, title_zh = ?,
                   summary = ?, reason = ?
                   WHERE link = ?""",
                (
                    a.get("score", 0),
                    raw.get("relevance", 0),
                    raw.get("quality", 0),
                    raw.get("timeliness", 0),
                    raw.get("category", "other"),
                    json.dumps(raw.get("keywords", []), ensure_ascii=False),
                    raw.get("title_zh", ""),
                    raw.get("summary", ""),
                    raw.get("reason", ""),
                    a["url"],
                ),
            )
        conn.commit()
    finally:
        conn.close()

    # Generate highlights
    highlights_input = json.dumps(
        [
            {
                "title": a.get("raw", {}).get("title_zh", a["title"]),
                "summary": a.get("raw", {}).get("summary", a["summary"]),
                "reason": a.get("raw", {}).get("reason", ""),
                "source": a["source"],
            }
            for a in top_articles
        ],
        ensure_ascii=False,
    )
    highlights = generate_highlights(highlights_input, lang=lang)

    return {
        "date": datetime.now(timezone.utc).strftime("%Y-%m-%d"),
        "total_found": len(articles),
        "top_n": len(top_articles),
        "articles": top_articles,
        "highlights": highlights,
    }


# ── CLI ────────────────────────────────────────────────────────────────

def main():
    parser = argparse.ArgumentParser(description="Fusion DB adapter for info-hub")
    parser.add_argument("--standalone", action="store_true", help="Run full pipeline")
    parser.add_argument("--db-path", type=str, help="Path to SQLite database")
    parser.add_argument("--top-n", type=int, default=DEFAULT_TOP_N)
    parser.add_argument("--lang", type=str, default="zh")
    parser.add_argument("--output", type=str, help="Output JSON file path")
    args = parser.parse_args()

    if args.standalone:
        db_path = args.db_path or os.path.join(Path.home(), ".fusion", "data.db")
        result = run_pipeline(db_path, top_n=args.top_n, lang=args.lang)

        if args.output:
            os.makedirs(os.path.dirname(args.output) or ".", exist_ok=True)
            with open(args.output, "w") as f:
                json.dump(result, f, ensure_ascii=False, indent=2)
        else:
            json.dump(result, sys.stdout, ensure_ascii=False, indent=2)
        return

    # DB adapter protocol: stdin -> stdout
    input_data = read_stdin_query()
    db_path = input_data.get("db_path", os.path.join(Path.home(), ".fusion", "data.db"))
    query = input_data.get("query", "")
    limit = input_data.get("limit", 20)
    offset = input_data.get("offset", 0)

    results = search_articles(db_path, query=query, limit=limit, offset=offset)
    write_stdout_result(results)


if __name__ == "__main__":
    main()
