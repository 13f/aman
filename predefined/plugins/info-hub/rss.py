"""
RSS/Atom feed fetcher for info-hub — purely in-memory, no database.

Fetches RSS/Atom feeds directly, then runs the full AI pipeline:
  fetch → tag → score → summarize → highlights

Usage:
  python3 rss.py --standalone [--hours 48] [--top-n 10] [--lang zh]
"""

import argparse
import json
import os
import re
import ssl
import sys
import time
from concurrent.futures import ThreadPoolExecutor, as_completed
from datetime import datetime, timezone
from pathlib import Path
from typing import Optional
from urllib.error import URLError
from urllib.request import Request, urlopen

# Ensure info-hub plugin dir is on sys.path
_plugin_dir = str(Path.home() / ".aman" / "plugins" / "info-hub")
if _plugin_dir not in sys.path:
    sys.path.insert(0, _plugin_dir)

from ai import generate_highlights, score_and_summarize, tag_articles  # noqa: E402
from common import extract_main_content, strip_html, truncate_description  # noqa: E402

# ── RSS Feed Sources ────────────────────────────────────────────────────
# 90 RSS feeds from Hacker News Popularity Contest 2025 (curated by Karpathy)

RSS_FEEDS: list[dict[str, str]] = [
    {"name": "simonwillison.net", "xmlUrl": "https://simonwillison.net/atom/everything/", "htmlUrl": "https://simonwillison.net"},
    {"name": "jeffgeerling.com", "xmlUrl": "https://www.jeffgeerling.com/blog.xml", "htmlUrl": "https://jeffgeerling.com"},
    {"name": "seangoedecke.com", "xmlUrl": "https://www.seangoedecke.com/rss.xml", "htmlUrl": "https://seangoedecke.com"},
    {"name": "krebsonsecurity.com", "xmlUrl": "https://krebsonsecurity.com/feed/", "htmlUrl": "https://krebsonsecurity.com"},
    {"name": "daringfireball.net", "xmlUrl": "https://daringfireball.net/feeds/main", "htmlUrl": "https://daringfireball.net"},
    {"name": "ericmigi.com", "xmlUrl": "https://ericmigi.com/rss.xml", "htmlUrl": "https://ericmigi.com"},
    {"name": "antirez.com", "xmlUrl": "http://antirez.com/rss", "htmlUrl": "http://antirez.com"},
    {"name": "idiallo.com", "xmlUrl": "https://idiallo.com/feed.rss", "htmlUrl": "https://idiallo.com"},
    {"name": "maurycyz.com", "xmlUrl": "https://maurycyz.com/index.xml", "htmlUrl": "https://maurycyz.com"},
    {"name": "pluralistic.net", "xmlUrl": "https://pluralistic.net/feed/", "htmlUrl": "https://pluralistic.net"},
    {"name": "shkspr.mobi", "xmlUrl": "https://shkspr.mobi/blog/feed/", "htmlUrl": "https://shkspr.mobi"},
    {"name": "lcamtuf.substack.com", "xmlUrl": "https://lcamtuf.substack.com/feed", "htmlUrl": "https://lcamtuf.substack.com"},
    {"name": "mitchellh.com", "xmlUrl": "https://mitchellh.com/feed.xml", "htmlUrl": "https://mitchellh.com"},
    {"name": "dynomight.net", "xmlUrl": "https://dynomight.net/feed.xml", "htmlUrl": "https://dynomight.net"},
    {"name": "utcc.utoronto.ca/~cks", "xmlUrl": "https://utcc.utoronto.ca/~cks/space/blog/?atom", "htmlUrl": "https://utcc.utoronto.ca/~cks"},
    {"name": "xeiaso.net", "xmlUrl": "https://xeiaso.net/blog.rss", "htmlUrl": "https://xeiaso.net"},
    {"name": "devblogs.microsoft.com/oldnewthing", "xmlUrl": "https://devblogs.microsoft.com/oldnewthing/feed", "htmlUrl": "https://devblogs.microsoft.com/oldnewthing"},
    {"name": "righto.com", "xmlUrl": "https://www.righto.com/feeds/posts/default", "htmlUrl": "https://righto.com"},
    {"name": "lucumr.pocoo.org", "xmlUrl": "https://lucumr.pocoo.org/feed.atom", "htmlUrl": "https://lucumr.pocoo.org"},
    {"name": "skyfall.dev", "xmlUrl": "https://skyfall.dev/rss.xml", "htmlUrl": "https://skyfall.dev"},
    {"name": "garymarcus.substack.com", "xmlUrl": "https://garymarcus.substack.com/feed", "htmlUrl": "https://garymarcus.substack.com"},
    {"name": "rachelbythebay.com", "xmlUrl": "https://rachelbythebay.com/w/atom.xml", "htmlUrl": "https://rachelbythebay.com"},
    {"name": "overreacted.io", "xmlUrl": "https://overreacted.io/rss.xml", "htmlUrl": "https://overreacted.io"},
    {"name": "timsh.org", "xmlUrl": "https://timsh.org/rss/", "htmlUrl": "https://timsh.org"},
    {"name": "johndcook.com", "xmlUrl": "https://www.johndcook.com/blog/feed/", "htmlUrl": "https://johndcook.com"},
    {"name": "gilesthomas.com", "xmlUrl": "https://gilesthomas.com/feed/rss.xml", "htmlUrl": "https://gilesthomas.com"},
    {"name": "matklad.github.io", "xmlUrl": "https://matklad.github.io/feed.xml", "htmlUrl": "https://matklad.github.io"},
    {"name": "derekthompson.org", "xmlUrl": "https://www.theatlantic.com/feed/author/derek-thompson/", "htmlUrl": "https://derekthompson.org"},
    {"name": "evanhahn.com", "xmlUrl": "https://evanhahn.com/feed.xml", "htmlUrl": "https://evanhahn.com"},
    {"name": "terriblesoftware.org", "xmlUrl": "https://terriblesoftware.org/feed/", "htmlUrl": "https://terriblesoftware.org"},
    {"name": "rakhim.exotext.com", "xmlUrl": "https://rakhim.exotext.com/rss.xml", "htmlUrl": "https://rakhim.exotext.com"},
    {"name": "joanwestenberg.com", "xmlUrl": "https://joanwestenberg.com/rss", "htmlUrl": "https://joanwestenberg.com"},
    {"name": "xania.org", "xmlUrl": "https://xania.org/feed", "htmlUrl": "https://xania.org"},
    {"name": "micahflee.com", "xmlUrl": "https://micahflee.com/feed/", "htmlUrl": "https://micahflee.com"},
    {"name": "nesbitt.io", "xmlUrl": "https://nesbitt.io/feed.xml", "htmlUrl": "https://nesbitt.io"},
    {"name": "construction-physics.com", "xmlUrl": "https://www.construction-physics.com/feed", "htmlUrl": "https://construction-physics.com"},
    {"name": "tedium.co", "xmlUrl": "https://feed.tedium.co/", "htmlUrl": "https://tedium.co"},
    {"name": "susam.net", "xmlUrl": "https://susam.net/feed.xml", "htmlUrl": "https://susam.net"},
    {"name": "entropicthoughts.com", "xmlUrl": "https://entropicthoughts.com/feed.xml", "htmlUrl": "https://entropicthoughts.com"},
    {"name": "buttondown.com/hillelwayne", "xmlUrl": "https://buttondown.com/hillelwayne/rss", "htmlUrl": "https://buttondown.com/hillelwayne"},
    {"name": "dwarkesh.com", "xmlUrl": "https://www.dwarkeshpatel.com/feed", "htmlUrl": "https://dwarkesh.com"},
    {"name": "borretti.me", "xmlUrl": "https://borretti.me/feed.xml", "htmlUrl": "https://borretti.me"},
    {"name": "wheresyoured.at", "xmlUrl": "https://www.wheresyoured.at/rss/", "htmlUrl": "https://wheresyoured.at"},
    {"name": "jayd.ml", "xmlUrl": "https://jayd.ml/feed.xml", "htmlUrl": "https://jayd.ml"},
    {"name": "minimaxir.com", "xmlUrl": "https://minimaxir.com/index.xml", "htmlUrl": "https://minimaxir.com"},
    {"name": "geohot.github.io", "xmlUrl": "https://geohot.github.io/blog/feed.xml", "htmlUrl": "https://geohot.github.io"},
    {"name": "paulgraham.com", "xmlUrl": "http://www.aaronsw.com/2002/feeds/pgessays.rss", "htmlUrl": "https://paulgraham.com"},
    {"name": "filfre.net", "xmlUrl": "https://www.filfre.net/feed/", "htmlUrl": "https://filfre.net"},
    {"name": "blog.jim-nielsen.com", "xmlUrl": "https://blog.jim-nielsen.com/feed.xml", "htmlUrl": "https://blog.jim-nielsen.com"},
    {"name": "dfarq.homeip.net", "xmlUrl": "https://dfarq.homeip.net/feed/", "htmlUrl": "https://dfarq.homeip.net"},
    {"name": "jyn.dev", "xmlUrl": "https://jyn.dev/atom.xml", "htmlUrl": "https://jyn.dev"},
    {"name": "geoffreylitt.com", "xmlUrl": "https://www.geoffreylitt.com/feed.xml", "htmlUrl": "https://geoffreylitt.com"},
    {"name": "downtowndougbrown.com", "xmlUrl": "https://www.downtowndougbrown.com/feed/", "htmlUrl": "https://downtowndougbrown.com"},
    {"name": "brutecat.com", "xmlUrl": "https://brutecat.com/rss.xml", "htmlUrl": "https://brutecat.com"},
    {"name": "eli.thegreenplace.net", "xmlUrl": "https://eli.thegreenplace.net/feeds/all.atom.xml", "htmlUrl": "https://eli.thegreenplace.net"},
    {"name": "abortretry.fail", "xmlUrl": "https://www.abortretry.fail/feed", "htmlUrl": "https://abortretry.fail"},
    {"name": "fabiensanglard.net", "xmlUrl": "https://fabiensanglard.net/rss.xml", "htmlUrl": "https://fabiensanglard.net"},
    {"name": "oldvcr.blogspot.com", "xmlUrl": "https://oldvcr.blogspot.com/feeds/posts/default", "htmlUrl": "https://oldvcr.blogspot.com"},
    {"name": "bogdanthegeek.github.io", "xmlUrl": "https://bogdanthegeek.github.io/blog/index.xml", "htmlUrl": "https://bogdanthegeek.github.io"},
    {"name": "hugotunius.se", "xmlUrl": "https://hugotunius.se/feed.xml", "htmlUrl": "https://hugotunius.se"},
    {"name": "gwern.net", "xmlUrl": "https://gwern.substack.com/feed", "htmlUrl": "https://gwern.net"},
    {"name": "berthub.eu", "xmlUrl": "https://berthub.eu/articles/index.xml", "htmlUrl": "https://berthub.eu"},
    {"name": "chadnauseam.com", "xmlUrl": "https://chadnauseam.com/rss.xml", "htmlUrl": "https://chadnauseam.com"},
    {"name": "simone.org", "xmlUrl": "https://simone.org/feed/", "htmlUrl": "https://simone.org"},
    {"name": "it-notes.dragas.net", "xmlUrl": "https://it-notes.dragas.net/feed/", "htmlUrl": "https://it-notes.dragas.net"},
    {"name": "beej.us", "xmlUrl": "https://beej.us/blog/rss.xml", "htmlUrl": "https://beej.us"},
    {"name": "hey.paris", "xmlUrl": "https://hey.paris/index.xml", "htmlUrl": "https://hey.paris"},
    {"name": "danielwirtz.com", "xmlUrl": "https://danielwirtz.com/rss.xml", "htmlUrl": "https://danielwirtz.com"},
    {"name": "matduggan.com", "xmlUrl": "https://matduggan.com/rss/", "htmlUrl": "https://matduggan.com"},
    {"name": "refactoringenglish.com", "xmlUrl": "https://refactoringenglish.com/index.xml", "htmlUrl": "https://refactoringenglish.com"},
    {"name": "worksonmymachine.substack.com", "xmlUrl": "https://worksonmymachine.substack.com/feed", "htmlUrl": "https://worksonmymachine.substack.com"},
    {"name": "philiplaine.com", "xmlUrl": "https://philiplaine.com/index.xml", "htmlUrl": "https://philiplaine.com"},
    {"name": "steveblank.com", "xmlUrl": "https://steveblank.com/feed/", "htmlUrl": "https://steveblank.com"},
    {"name": "bernsteinbear.com", "xmlUrl": "https://bernsteinbear.com/feed.xml", "htmlUrl": "https://bernsteinbear.com"},
    {"name": "danieldelaney.net", "xmlUrl": "https://danieldelaney.net/feed", "htmlUrl": "https://danieldelaney.net"},
    {"name": "troyhunt.com", "xmlUrl": "https://www.troyhunt.com/rss/", "htmlUrl": "https://troyhunt.com"},
    {"name": "herman.bearblog.dev", "xmlUrl": "https://herman.bearblog.dev/feed/", "htmlUrl": "https://herman.bearblog.dev"},
    {"name": "tomrenner.com", "xmlUrl": "https://tomrenner.com/index.xml", "htmlUrl": "https://tomrenner.com"},
    {"name": "blog.pixelmelt.dev", "xmlUrl": "https://blog.pixelmelt.dev/rss/", "htmlUrl": "https://blog.pixelmelt.dev"},
    {"name": "martinalderson.com", "xmlUrl": "https://martinalderson.com/feed.xml", "htmlUrl": "https://martinalderson.com"},
    {"name": "danielchasehooper.com", "xmlUrl": "https://danielchasehooper.com/feed.xml", "htmlUrl": "https://danielchasehooper.com"},
    {"name": "chiark.greenend.org.uk/~sgtatham", "xmlUrl": "https://www.chiark.greenend.org.uk/~sgtatham/quasiblog/feed.xml", "htmlUrl": "https://chiark.greenend.org.uk/~sgtatham"},
    {"name": "grantslatton.com", "xmlUrl": "https://grantslatton.com/rss.xml", "htmlUrl": "https://grantslatton.com"},
    {"name": "experimental-history.com", "xmlUrl": "https://www.experimental-history.com/feed", "htmlUrl": "https://experimental-history.com"},
    {"name": "anildash.com", "xmlUrl": "https://anildash.com/feed.xml", "htmlUrl": "https://anildash.com"},
    {"name": "aresluna.org", "xmlUrl": "https://aresluna.org/main.rss", "htmlUrl": "https://aresluna.org"},
    {"name": "michael.stapelberg.ch", "xmlUrl": "https://michael.stapelberg.ch/feed.xml", "htmlUrl": "https://michael.stapelberg.ch"},
    {"name": "miguelgrinberg.com", "xmlUrl": "https://blog.miguelgrinberg.com/feed", "htmlUrl": "https://miguelgrinberg.com"},
    {"name": "keygen.sh", "xmlUrl": "https://keygen.sh/blog/feed.xml", "htmlUrl": "https://keygen.sh"},
    {"name": "mjg59.dreamwidth.org", "xmlUrl": "https://mjg59.dreamwidth.org/data/rss", "htmlUrl": "https://mjg59.dreamwidth.org"},
    {"name": "computer.rip", "xmlUrl": "https://computer.rip/rss.xml", "htmlUrl": "https://computer.rip"},
    {"name": "tedunangst.com", "xmlUrl": "https://www.tedunangst.com/flak/rss", "htmlUrl": "https://tedunangst.com"},
]

FEED_FETCH_TIMEOUT = 15
FEED_CONCURRENCY = 10
DESCRIPTION_MAX_LEN = 384


# ── RSS/Atom XML Parsing ────────────────────────────────────────────────

def _get_tag_content(xml: str, tag_name: str) -> str:
    """Extract text content from an XML tag (handles CDATA)."""
    # Try namespaced and non-namespaced variants
    for pat in [
        rf"<{tag_name}[^>]*>([\s\S]*?)</{tag_name}>",
        rf"<[^:>]*:{tag_name}[^>]*>([\s\S]*?)</[^:>]*:{tag_name}>",
    ]:
        m = re.search(pat, xml, re.IGNORECASE)
        if m:
            text = m.group(1)
            # Extract CDATA
            cdata = re.search(r"<!\[CDATA\[([\s\S]*?)\]\]>", text)
            if cdata:
                return cdata.group(1).strip()
            return text.strip()
    return ""


def _get_attr_value(xml: str, tag_name: str, attr_name: str) -> str:
    """Extract an attribute value from an XML tag."""
    pat = rf"<{tag_name}[^>]*\s{attr_name}=[\"']([^\"']*)[\"'][^>]*/?>"
    m = re.search(pat, xml, re.IGNORECASE)
    return m.group(1) if m else ""


def _parse_date(date_str: str) -> Optional[str]:
    """Parse a date string to ISO 8601, or return None."""
    if not date_str:
        return None
    try:
        dt = datetime.fromisoformat(date_str.replace("Z", "+00:00"))
        return dt.isoformat()
    except (ValueError, TypeError):
        pass
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


def parse_rss_items(xml: str) -> list[dict]:
    """Parse RSS 2.0 or Atom XML and return a list of item dicts.

    Each item: {title, link, pub_date, description}
    """
    items: list[dict] = []

    is_atom = bool(re.search(r'<feed\s', xml, re.IGNORECASE))

    if is_atom:
        for entry in re.finditer(r"<entry[\s>]([\s\S]*?)</entry>", xml, re.IGNORECASE):
            exml = entry.group(1)

            title = strip_html(_get_tag_content(exml, "title"))

            link = _get_attr_value(exml, r'link[^>]*rel="alternate"', "href")
            if not link:
                link = _get_attr_value(exml, "link", "href")

            pub_date = _get_tag_content(exml, "published") or _get_tag_content(exml, "updated")

            desc = strip_html(
                _get_tag_content(exml, "summary") or _get_tag_content(exml, "content")
            )

            if title or link:
                items.append({
                    "title": title,
                    "link": link,
                    "pub_date": pub_date,
                    "description": truncate_description(desc, DESCRIPTION_MAX_LEN),
                })
    else:
        for item in re.finditer(r"<item[\s>]([\s\S]*?)</item>", xml, re.IGNORECASE):
            iml = item.group(1)

            title = strip_html(_get_tag_content(iml, "title"))
            link = _get_tag_content(iml, "link") or _get_tag_content(iml, "guid")
            pub_date = (
                _get_tag_content(iml, "pubDate")
                or _get_tag_content(iml, "dc:date")
                or _get_tag_content(iml, "date")
            )
            desc = strip_html(
                _get_tag_content(iml, "description")
                or _get_tag_content(iml, "content:encoded")
            )

            if title or link:
                items.append({
                    "title": title,
                    "link": link,
                    "pub_date": pub_date,
                    "description": truncate_description(desc, DESCRIPTION_MAX_LEN),
                })

    return items


# ── Feed Fetching ───────────────────────────────────────────────────────

def fetch_feed(feed: dict) -> list[dict]:
    """Fetch a single RSS/Atom feed and return parsed articles.

    Each article: {title, url, published, source, source_url, summary, raw}
    """
    try:
        ctx = ssl.create_default_context()
        req = Request(feed["xmlUrl"], headers={
            "User-Agent": "AI-Daily-Digest/1.0 (RSS Reader)",
            "Accept": "application/rss+xml, application/atom+xml, application/xml, text/xml, */*",
        })
        with urlopen(req, timeout=FEED_FETCH_TIMEOUT, context=ctx) as resp:
            if resp.status >= 400:
                print(f"  [rss] ✗ {feed['name']}: HTTP {resp.status}")
                return []
            xml = resp.read(2_000_000).decode("utf-8", errors="replace")

        items = parse_rss_items(xml)
        articles = []
        for item in items:
            pub_date = _parse_date(item.get("pub_date", ""))
            articles.append({
                "title": item["title"],
                "url": item["link"],
                "published": pub_date or "",
                "source": feed["name"],
                "source_url": feed.get("htmlUrl", ""),
                "summary": item.get("description", ""),
                "raw": {
                    "content": item.get("description", ""),
                    "category": "",
                    "keywords": [],
                    "relevance": 0,
                    "quality": 0,
                    "timeliness": 0,
                },
            })
        return articles

    except URLError as e:
        reason = str(e.reason) if hasattr(e, "reason") else str(e)
        print(f"  [rss] ✗ {feed['name']}: {reason[:80]}")
        return []
    except (OSError, ValueError, UnicodeDecodeError) as e:
        print(f"  [rss] ✗ {feed['name']}: {e}")
        return []


def fetch_all_feeds(
    feeds: list[dict] | None = None,
    hours: int = 48,
    concurrency: int = FEED_CONCURRENCY,
) -> list[dict]:
    """Fetch all RSS feeds concurrently, returning articles from the time window.

    Args:
        feeds: List of feed dicts. Defaults to RSS_FEEDS.
        hours: Only return articles published within this many hours.
        concurrency: Max concurrent fetch workers.

    Returns:
        List of article dicts sorted by published desc.
    """
    if feeds is None:
        feeds = RSS_FEEDS

    cutoff = datetime.now(timezone.utc).timestamp() - hours * 3600
    all_articles: list[dict] = []
    success = 0
    fail = 0
    total = len(feeds)

    print(f"[rss] Fetching {total} feeds (concurrency={concurrency}, window={hours}h)...")

    with ThreadPoolExecutor(max_workers=concurrency) as pool:
        futures = {pool.submit(fetch_feed, f): f for f in feeds}
        for i, future in enumerate(as_completed(futures)):
            feed = futures[future]
            try:
                articles = future.result()
            except Exception as e:
                print(f"  [rss] ✗ {feed['name']}: {e}")
                fail += 1
                continue

            if articles:
                success += 1
            else:
                fail += 1

            for a in articles:
                try:
                    if a.get("published"):
                        pub_ts = datetime.fromisoformat(a["published"]).timestamp()
                        if pub_ts >= cutoff:
                            all_articles.append(a)
                except (ValueError, TypeError):
                    all_articles.append(a)

            if (success + fail) % 10 == 0 or success + fail == total:
                print(f"  [rss] Progress: {success + fail}/{total} "
                      f"({success} ok, {fail} fail, {len(all_articles)} articles)")

    all_articles.sort(key=lambda a: a.get("published", ""), reverse=True)
    print(f"[rss] Done: {len(all_articles)} articles within {hours}h window")
    return all_articles


# ── Pipeline ────────────────────────────────────────────────────────────

def run_pipeline(
    feeds: list[dict] | None = None,
    hours: int = 48,
    top_n: int = 10,
    lang: str = "zh",
) -> dict:
    """Full in-memory pipeline: fetch → tag → score → summarize → highlights.

    Args:
        feeds: Feed list (defaults to RSS_FEEDS).
        hours: Time window in hours.
        top_n: Number of top articles to include in output.
        lang: Output language (zh or en).

    Returns:
        Dict with {date, total_found, top_n, articles, highlights}.
    """
    t0 = time.time()

    # Step 1: Fetch all feeds
    print("\n[1/3] Fetching feeds...")
    articles = fetch_all_feeds(feeds=feeds, hours=hours)
    if not articles:
        return {
            "date": datetime.now(timezone.utc).strftime("%Y-%m-%d"),
            "total_found": 0,
            "articles": [],
            "highlights": "",
        }

    # Build AI input
    ai_input = []
    for i, a in enumerate(articles):
        ai_input.append({
            "index": i,
            "title": a["title"],
            "description": a.get("summary", ""),
            "source_name": a["source"],
            "link": a["url"],
        })

    # Step 2: Tag (category + keywords)
    print(f"\n[2/3] Tagging {len(articles)} articles...")
    tag_results = tag_articles(ai_input)
    tag_map = {r["index"]: r for r in tag_results}
    for i, a in enumerate(articles):
        if i in tag_map:
            a["raw"]["category"] = tag_map[i]["category"]
            a["raw"]["keywords"] = tag_map[i]["keywords"]

    # Step 3: Score + Summarize (both use LLM, delegated to ai.py)
    print(f"\n[3/3] Scoring + summarizing {len(articles)} articles (min_score=18)...")
    score_results, summary_results = score_and_summarize(ai_input, lang=lang, min_score=18)
    score_map = {r["index"]: r for r in score_results}
    summary_map = {r["index"]: r for r in summary_results}

    # Merge scores and summaries before sorting (index still matches ai_input)
    for i, a in enumerate(articles):
        if i in score_map:
            s = score_map[i]
            a["score"] = s["relevance"] + s["quality"] + s["timeliness"]
            a["raw"]["relevance"] = s["relevance"]
            a["raw"]["quality"] = s["quality"]
            a["raw"]["timeliness"] = s["timeliness"]
        if i in summary_map:
            s = summary_map[i]
            a["raw"]["title_zh"] = s.get("title_zh", a["title"])
            a["raw"]["summary"] = s.get("summary", "")
            a["raw"]["reason"] = s.get("reason", "")
            a["summary"] = s.get("summary", "")

    # Sort by score, take top N
    articles.sort(key=lambda a: a.get("score", 0), reverse=True)
    top_articles = articles[:top_n]

    # Highlights
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

    elapsed = time.time() - t0
    print(f"\n[rss] Pipeline complete in {elapsed:.1f}s")

    return {
        "date": datetime.now(timezone.utc).strftime("%Y-%m-%d"),
        "total_found": len(articles),
        "top_n": len(top_articles),
        "articles": top_articles,
        "highlights": highlights,
    }


# ── CLI ─────────────────────────────────────────────────────────────────

def main():
    parser = argparse.ArgumentParser(description="RSS/Atom feed fetcher for info-hub")
    parser.add_argument("--standalone", action="store_true", help="Run full pipeline")
    parser.add_argument("--hours", type=int, default=72, help="Time window in hours (default: 72)")
    parser.add_argument("--top-n", type=int, default=10, help="Top N articles after scoring (default: 10)")
    parser.add_argument("--lang", type=str, default="zh", help="Output language (zh or en)")
    parser.add_argument("--output", type=str, help="Output JSON file path")
    parser.add_argument("--list-feeds", action="store_true", help="List all default RSS feeds and exit")
    args = parser.parse_args()

    if args.list_feeds:
        for f in RSS_FEEDS:
            print(f"{f['name']}")
            print(f"  RSS:  {f['xmlUrl']}")
            print(f"  Site: {f['htmlUrl']}")
            print()
        return

    if args.standalone:
        result = run_pipeline(
            feeds=RSS_FEEDS,
            hours=args.hours,
            top_n=args.top_n,
            lang=args.lang,
        )

        if args.output:
            os.makedirs(os.path.dirname(args.output) or ".", exist_ok=True)
            with open(args.output, "w") as f:
                json.dump(result, f, ensure_ascii=False, indent=2)
            print(f"\nOutput written to {args.output}")
        else:
            # Print summary to stdout
            print(f"\n{'='*70}")
            print(f"RESULTS — {result['date']}")
            print(f"{'='*70}")
            print(f"\n{result['highlights']}\n")
            for i, a in enumerate(result["articles"]):
                raw = a.get("raw", {})
                print(f"  #{i+1}  [{raw.get('category', '?')}] {a['title']}")
                print(f"       Score: R{raw.get('relevance',0)}/Q{raw.get('quality',0)}/"
                      f"T{raw.get('timeliness',0)} = {a.get('score',0)}/30")
                print(f"       {raw.get('title_zh', '')}")
                print(f"       {a['url']}")
                print()
    else:
        print("Use --standalone to run the full pipeline, or --list-feeds to see sources.")
        print(f"Default: {len(RSS_FEEDS)} feeds available.")


if __name__ == "__main__":
    main()
