#!/usr/bin/env python3
"""TAM/SAM/SOM Builder — estimates market size using triangulated bottom-up methodology."""

from __future__ import annotations
import os
from typing import Any
from llm import LlmClient

_PROMPT_DIR = os.path.join(os.path.dirname(os.path.dirname(__file__)), "prompts")

def _load_prompt(name: str) -> str:
    with open(os.path.join(_PROMPT_DIR, name), "r") as f:
        return f.read()

def estimate_market_size(
    idea_description: str,
    competitors: dict | None = None,
    trend_velocity: str = "stable",
    llm: LlmClient | None = None,
) -> dict[str, Any]:
    client = llm or LlmClient()
    system_prompt = _load_prompt("tam_sam_som.txt")
    user_parts = [f"Estimate market size for: {idea_description}"]
    if competitors:
        comp_count = len(competitors.get("direct_competitors", []))
        user_parts.append(f"Direct competitors found: {comp_count}")
    user_parts.append(f"Trend velocity: {trend_velocity}")
    return client.chat_json(system_prompt, "\n".join(user_parts), temperature=0.2)
