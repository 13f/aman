#!/usr/bin/env python3
"""
Extract visible text from HTML piped via stdin or from a URL argument.
Usage:
  curl -sL URL | python3 extract_visible_text.py              # from stdin
  python3 extract_visible_text.py URL                          # from URL
  curl -sL URL | python3 extract_visible_text.py --filter 营收,利润,亏损,基石
"""
import sys, re, html, urllib.request

def extract(text: str, min_len: int = 15) -> list[str]:
    """Strip HTML tags and return visible text lines >= min_len chars."""
    text = re.sub(r'<script[^>]*>.*?</script>', '', text, flags=re.DOTALL)
    text = re.sub(r'<style[^>]*>.*?</style>', '', text, flags=re.DOTALL)
    text = re.sub(r'<[^>]+>', '\n', text)
    text = html.unescape(text)
    return [l.strip() for l in text.split('\n') if len(l.strip()) >= min_len]


if __name__ == '__main__':
    filters = []
    args = sys.argv[1:]

    if '--filter' in args:
        idx = args.index('--filter')
        filters = args[idx + 1].split(',')
        args = args[:idx] + args[idx + 2:]

    if args and args[0].startswith('http'):
        req = urllib.request.Request(args[0], headers={'User-Agent': 'Mozilla/5.0'})
        with urllib.request.urlopen(req) as resp:
            raw = resp.read()
            # Try utf-8, then gb2312
            try:
                text = raw.decode('utf-8')
            except UnicodeDecodeError:
                text = raw.decode('gb2312', errors='replace')
    else:
        text = sys.stdin.read()

    lines = extract(text)

    if filters:
        lines = [l for l in lines if any(kw in l for kw in filters)]

    for l in lines:
        print(l)
