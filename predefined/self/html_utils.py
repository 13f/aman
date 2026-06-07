#!/usr/bin/env python3
"""Shared HTML utilities for aman plugins.

Provides HTML escaping, response builders, template loading, and static
file serving — reused across team, startup, and other plugins.
"""

from __future__ import annotations

import json
import os
import re
from string import Template
from typing import Any, Optional

# ── MIME types ─────────────────────────────────────────────────────────

MIME: dict[str, str] = {
    ".html": "text/html; charset=utf-8",
    ".js": "application/javascript; charset=utf-8",
    ".css": "text/css; charset=utf-8",
    ".svg": "image/svg+xml",
    ".png": "image/png",
    ".woff2": "font/woff2",
}


# ── Escaping ───────────────────────────────────────────────────────────

def esc(s: str) -> str:
    """Escape a string for safe inclusion in HTML text or attribute values."""
    return (s or "").replace("&", "&amp;").replace("<", "&lt;").replace(">", "&gt;").replace('"', "&quot;")


def esc_js(s: str) -> str:
    """Escape a string for safe inclusion in a JS single-quoted string literal."""
    return (s or "").replace("\\", "\\\\").replace("'", "\\'").replace("\n", "\\n").replace("\r", "")


# ── Response builders ──────────────────────────────────────────────────

def html_response(html: str) -> dict:
    """Build a dict that the plugin bridge converts to an HTTP 200 HTML response."""
    return {"status": 200, "headers": {"content-type": "text/html; charset=utf-8"}, "body": html}


def json_response(data: Any, status: int = 200) -> dict:
    """Build a dict that the plugin bridge converts to an HTTP JSON response."""
    return {"status": status, "headers": {"content-type": "application/json"}, "body": json.dumps(data)}


def error_response(message: str, status: int = 400) -> dict:
    """Build a JSON error response."""
    return json_response({"error": message}, status)


# ── Request helpers ────────────────────────────────────────────────────

def parse_body(body: Any) -> dict:
    """Parse an HTTP request body, handling raw strings and pre-parsed dicts."""
    if body is None:
        return {}
    if isinstance(body, dict):
        return body
    if isinstance(body, str):
        try:
            return json.loads(body)
        except json.JSONDecodeError:
            return {}
    return {}


# ── Template loading ───────────────────────────────────────────────────

def load_template(template_dir: str, name: str) -> Template:
    """Load a Python string.Template from a file."""
    with open(os.path.join(template_dir, name), "r") as f:
        return Template(f.read())


# ── Static file serving ────────────────────────────────────────────────

def serve_static(static_dir: str, filename: str) -> Optional[dict]:
    """Serve a static file, or return None if not found / invalid path.

    Enforces basic path traversal protection.
    """
    # Reject path traversal
    if ".." in filename or "/" in filename or "\\" in filename:
        return json_response({"error": "bad filename"}, 400)

    filepath = os.path.join(static_dir, filename)
    if not os.path.isfile(filepath):
        return None

    ext = os.path.splitext(filename)[1].lower()
    mime = MIME.get(ext, "application/octet-stream")
    try:
        with open(filepath, "r") as f:
            content = f.read()
        return {"status": 200, "headers": {"content-type": mime}, "body": content}
    except Exception:
        return json_response({"error": "failed to read file"}, 500)


def serve_static_or_404(static_dir: str, filename: str) -> dict:
    """Serve a static file, returning a 404 dict if not found."""
    result = serve_static(static_dir, filename)
    if result is not None:
        return result
    return json_response({"error": "not found"}, 404)
