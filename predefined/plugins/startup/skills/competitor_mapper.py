#!/usr/bin/env python3
"""Competitor Mapper skill — maps the competitive landscape across
four categories (direct, indirect, substitute, emerging) with gap analysis.
"""

from __future__ import annotations

import os
from typing import Any

from llm import LlmClient


_PROMPT_DIR = os.path.join(os.path.dirname(os.path.dirname(__file__)), "prompts")


def _load_prompt(name: str) -> str:
    with open(os.path.join(_PROMPT_DIR, name), "r") as f:
        return f.read()


def analyze_competitors(
    idea_description: str,
    keywords: list[str] | None = None,
    niche: str = "",
    market_insights: str = "",
    llm: LlmClient | None = None,
) -> dict[str, Any]:
    """Map the competitive landscape for an idea.

    Args:
        idea_description: The app idea description.
        keywords: Search keywords for the idea.
        niche: Market niche/category.
        market_insights: Optional pre-existing market research text.
        llm: LLM client (uses default config if None).

    Returns:
        Dict with direct_competitors, indirect_competitors, substitutes,
        emerging_threats, positioning_gaps, saturation_score, etc.
    """
    client = llm or LlmClient()
    system_prompt = _load_prompt("competitor_mapping.md")

    user_parts = [f"Analyze the competitive landscape for this app idea:\n\n{idea_description}"]
    if keywords:
        user_parts.append(f"\n\nSearch keywords: {', '.join(keywords)}")
    if niche:
        user_parts.append(f"\n\nMarket niche/category: {niche}")
    if market_insights:
        user_parts.append(f"\n\nExisting market research:\n{market_insights}")

    user_prompt = "\n".join(user_parts)

    result = client.chat_json(system_prompt, user_prompt, temperature=0.3, max_tokens=6000)

    # Validate and normalize
    for field in ["direct_competitors", "indirect_competitors", "substitutes",
                  "emerging_threats", "positioning_gaps"]:
        if field not in result:
            result[field] = []

    if "saturation_score" not in result:
        result["saturation_score"] = {"total": 0}
    if "market_saturation" not in result:
        result["market_saturation"] = "unknown"
    if "differentiation_opportunities" not in result:
        result["differentiation_opportunities"] = []
    if "market_insights_sources_used" not in result:
        result["market_insights_sources_used"] = []
    if "review_mining_summary" not in result:
        result["review_mining_summary"] = {
            "most_common_complaint_across_competitors": "",
            "strongest_gap_signal": "",
            "competitors_mined": 0,
        }

    return result
