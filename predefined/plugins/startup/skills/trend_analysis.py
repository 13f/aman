#!/usr/bin/env python3
"""Trend Analysis — analyzes market trends across platforms."""

from __future__ import annotations
import os
from typing import Any
from llm import LlmClient

_PROMPT_DIR = os.path.join(os.path.dirname(os.path.dirname(__file__)), "prompts")

def _load_prompt(name: str) -> str:
    with open(os.path.join(_PROMPT_DIR, name), "r") as f:
        return f.read()

def analyze_trends(
    niche: str,
    platforms: list[str] | None = None,
    llm: LlmClient | None = None,
) -> dict[str, Any]:
    client = llm or LlmClient()
    system_prompt = _load_prompt("trend_analysis.md")
    platform_str = ", ".join(platforms) if platforms else "TikTok, Reddit, App Store, Google Trends"
    return client.chat_json(system_prompt, f"Analyze trends for niche: {niche}\nPlatforms: {platform_str}", temperature=0.3)
