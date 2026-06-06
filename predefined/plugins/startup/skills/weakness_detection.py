#!/usr/bin/env python3
"""Weakness Detection — identifies weak dimensions and classifies root causes."""

from __future__ import annotations
import os, json
from typing import Any
from llm import LlmClient

_PROMPT_DIR = os.path.join(os.path.dirname(os.path.dirname(__file__)), "prompts")

def _load_prompt(name: str) -> str:
    with open(os.path.join(_PROMPT_DIR, name), "r") as f:
        return f.read()

def detect_weaknesses(
    dimension_scores: dict[str, float],
    desire: dict | None = None,
    competitors: dict | None = None,
    pricing: dict | None = None,
    distribution: dict | None = None,
    retention: dict | None = None,
    complexity: dict | None = None,
    llm: LlmClient | None = None,
) -> dict[str, Any]:
    client = llm or LlmClient()
    system_prompt = _load_prompt("weakness_detection.txt")

    context = [f"Dimension scores: {json.dumps(dimension_scores)}"]
    if desire:
        context.append(f"Desire: {json.dumps({k: desire.get(k) for k in ['primary_driver', 'desire_strength', 'desire_label']})}")
    if competitors:
        context.append(f"Competitors: {competitors.get('market_saturation', '?')}, "
                       f"gaps={len(competitors.get('positioning_gaps', []))}")
    if pricing:
        context.append(f"Pricing: ${pricing.get('recommended_price_monthly', 0)}/mo, "
                       f"model={pricing.get('pricing_model', '?')}")
    if distribution:
        context.append(f"Distribution k-factor: {distribution.get('composite_k_factor', 0)}")
    if retention:
        context.append(f"Retention tier: {retention.get('retention_tier', '?')}, "
                       f"D7={retention.get('predicted_retention', {}).get('day_7_pct', 0)}%")
    if complexity:
        context.append(f"Build time: {complexity.get('build_time_estimate_months', 0)} months")

    return client.chat_json(system_prompt, "\n".join(context), temperature=0.2)
