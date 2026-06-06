#!/usr/bin/env python3
"""Landing Page Builder — generates conversion copy from validated analysis."""

from __future__ import annotations
import os, json
from typing import Any
from llm import LlmClient

_PROMPT_DIR = os.path.join(os.path.dirname(os.path.dirname(__file__)), "prompts")

def _load_prompt(name: str) -> str:
    with open(os.path.join(_PROMPT_DIR, name), "r") as f:
        return f.read()

def build_landing_page(
    idea_description: str,
    desire: dict | None = None,
    competitors: dict | None = None,
    keywords: list[str] | None = None,
    llm: LlmClient | None = None,
) -> dict[str, Any]:
    client = llm or LlmClient()
    system_prompt = _load_prompt("landing_page.txt")
    context = [f"Idea: {idea_description}"]
    if desire:
        context.append(f"Primary desire: {desire.get('primary_driver', 'unknown')}, strength: {desire.get('desire_strength', 0)}")
    if competitors:
        gaps = competitors.get("positioning_gaps", [])
        if gaps:
            context.append(f"Key gaps: {json.dumps(gaps[:3])}")
        strongest = competitors.get("review_mining_summary", {}).get("strongest_gap_signal", "")
        if strongest:
            context.append(f"Strongest competitor gap: {strongest}")
    if keywords:
        context.append(f"Keywords: {', '.join(keywords)}")
    return client.chat_json(system_prompt, "\n".join(context), temperature=0.7, max_tokens=3000)
