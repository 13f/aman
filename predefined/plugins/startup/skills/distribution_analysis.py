#!/usr/bin/env python3
"""Distribution Analysis — evaluates viral loops, ASO opportunity, and channel fit."""

from __future__ import annotations
import os
from typing import Any
from llm import LlmClient

_PROMPT_DIR = os.path.join(os.path.dirname(os.path.dirname(__file__)), "prompts")

def _load_prompt(name: str) -> str:
    with open(os.path.join(_PROMPT_DIR, name), "r") as f:
        return f.read()

def analyze_distribution(
    idea_description: str,
    competitors: dict | None = None,
    llm: LlmClient | None = None,
) -> dict[str, Any]:
    client = llm or LlmClient()
    system_prompt = _load_prompt("distribution.txt")
    user_parts = [f"Analyze distribution for: {idea_description}"]
    if competitors:
        comp_count = len(competitors.get("direct_competitors", []))
        user_parts.append(f"Category has {comp_count} direct competitors")
    return client.chat_json(system_prompt, "\n".join(user_parts), temperature=0.2)
