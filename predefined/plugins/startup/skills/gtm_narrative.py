#!/usr/bin/env python3
"""GTM Narrative — generates complete go-to-market plan."""

from __future__ import annotations
import os, json
from typing import Any
from llm import LlmClient

_PROMPT_DIR = os.path.join(os.path.dirname(os.path.dirname(__file__)), "prompts")

def _load_prompt(name: str) -> str:
    with open(os.path.join(_PROMPT_DIR, name), "r") as f:
        return f.read()

def build_gtm_narrative(
    idea_description: str,
    competitors: dict | None = None,
    distribution: dict | None = None,
    desire: dict | None = None,
    llm: LlmClient | None = None,
) -> dict[str, Any]:
    client = llm or LlmClient()
    system_prompt = _load_prompt("gtm_narrative.txt")
    context = [f"Idea: {idea_description}"]
    if competitors:
        context.append(f"Competitor count: {len(competitors.get('direct_competitors', []))}")
    if distribution:
        context.append(f"Primary channel: {distribution.get('primary_channel', 'unknown')}, "
                       f"k-factor: {distribution.get('composite_k_factor', 0)}")
    if desire:
        context.append(f"Desire: {desire.get('primary_driver', 'unknown')}, "
                       f"virality: {desire.get('virality_potential', 'medium')}")
    return client.chat_json(system_prompt, "\n".join(context), temperature=0.7, max_tokens=4000)
