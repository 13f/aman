#!/usr/bin/env python3
"""Pivot Engine — generates pivot options for ideas that scored low."""

from __future__ import annotations
import os, json
from typing import Any
from llm import LlmClient

_PROMPT_DIR = os.path.join(os.path.dirname(os.path.dirname(__file__)), "prompts")

def _load_prompt(name: str) -> str:
    with open(os.path.join(_PROMPT_DIR, name), "r") as f:
        return f.read()

def generate_pivots(
    idea_description: str,
    scores: dict[str, Any],
    weaknesses: list[dict] | None = None,
    competitors: dict | None = None,
    llm: LlmClient | None = None,
) -> dict[str, Any]:
    client = llm or LlmClient()
    system_prompt = _load_prompt("pivot.md")

    context = [
        f"Idea: {idea_description}",
        f"Score: {scores.get('final_score', 0)}/100, Verdict: {scores.get('verdict', 'drop')}",
        f"Dimension scores: {json.dumps(scores.get('dimension_scores', {}))}",
    ]
    if weaknesses:
        context.append(f"Weaknesses: {json.dumps(weaknesses[:5])}")
    if competitors:
        gaps = competitors.get("positioning_gaps", [])
        if gaps:
            context.append(f"Competitor gaps: {json.dumps(gaps[:3])}")

    return client.chat_json(system_prompt, "\n".join(context), temperature=0.4, max_tokens=3000)
