#!/usr/bin/env python3
"""Pricing Page Optimizer — designs high-converting pricing pages."""

from __future__ import annotations
import os, json
from typing import Any
from llm import LlmClient

_PROMPT_DIR = os.path.join(os.path.dirname(os.path.dirname(__file__)), "prompts")

def _load_prompt(name: str) -> str:
    with open(os.path.join(_PROMPT_DIR, name), "r") as f:
        return f.read()

def optimize_pricing_page(
    idea_description: str,
    pricing: dict | None = None,
    competitors: dict | None = None,
    desire: dict | None = None,
    llm: LlmClient | None = None,
) -> dict[str, Any]:
    client = llm or LlmClient()
    system_prompt = _load_prompt("pricing_page.txt")
    context = [f"Idea: {idea_description}"]
    if pricing:
        context.append(f"Recommended price: ${pricing.get('recommended_price_monthly', 0)}/mo, "
                       f"model: {pricing.get('pricing_model', 'freemium')}, "
                       f"premium: {pricing.get('desire_premium_applied', 1.0)}x")
    if competitors:
        comp_names = [c.get("name", "") for c in competitors.get("direct_competitors", [])[:5]]
        if comp_names:
            context.append(f"Competitors: {', '.join(comp_names)}")
    if desire:
        context.append(f"Primary desire: {desire.get('primary_driver', 'unknown')}")
    return client.chat_json(system_prompt, "\n".join(context), temperature=0.5)
