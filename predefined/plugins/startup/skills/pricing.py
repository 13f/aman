#!/usr/bin/env python3
"""Pricing & WTP skill — models pricing using Van Westendorp analysis
with desire-premium multipliers.
"""

from __future__ import annotations

import os
from typing import Any

from llm import LlmClient


_PROMPT_DIR = os.path.join(os.path.dirname(os.path.dirname(__file__)), "prompts")


def _load_prompt(name: str) -> str:
    with open(os.path.join(_PROMPT_DIR, name), "r") as f:
        return f.read()


def analyze_pricing(
    idea_description: str,
    desire_scores: dict | None = None,
    competitor_pricing: str = "",
    llm: LlmClient | None = None,
) -> dict[str, Any]:
    """Model pricing and willingness-to-pay for an idea.

    Args:
        idea_description: The app idea description.
        desire_scores: Optional desire evaluation results (for premium multiplier).
        competitor_pricing: Optional competitor pricing context.
        llm: LLM client (uses default config if None).

    Returns:
        Dict with van_westendorp analysis, recommended pricing, etc.
    """
    client = llm or LlmClient()
    system_prompt = _load_prompt("pricing.md")

    user_parts = [f"Model the pricing for this app idea:\n\n{idea_description}"]

    if desire_scores:
        user_parts.append(f"\n\nDesire analysis (use for premium multiplier):\n"
                          f"Primary driver: {desire_scores.get('primary_driver', 'unknown')}\n"
                          f"Desire strength: {desire_scores.get('desire_strength', 2.0)}\n"
                          f"Desire label: {desire_scores.get('desire_label', 'moderate')}")

    if competitor_pricing:
        user_parts.append(f"\n\nCompetitor pricing context:\n{competitor_pricing}")

    user_prompt = "\n".join(user_parts)

    result = client.chat_json(system_prompt, user_prompt, temperature=0.2)

    # Validate and normalize
    if "van_westendorp" not in result:
        result["van_westendorp"] = {}
    if "recommended_price_monthly" not in result:
        result["recommended_price_monthly"] = 0
    if "pricing_model" not in result:
        result["pricing_model"] = "freemium"
    if "desire_premium_applied" not in result:
        result["desire_premium_applied"] = 1.0

    return result
