#!/usr/bin/env python3
"""What-If Simulator — simulates cascading effects of hypothetical changes."""

import os, json
from typing import Any
from llm import LlmClient

_PROMPT_DIR = os.path.join(os.path.dirname(os.path.dirname(__file__)), "prompts")
def _load(n): return open(os.path.join(_PROMPT_DIR, n)).read()

def simulate_what_if(
    idea_description: str,
    question: str,
    scores: dict[str, Any] | None = None,
    competitors: dict | None = None,
    pricing: dict | None = None,
    desire: dict | None = None,
    distribution: dict | None = None,
    history: list[dict] | None = None,
    llm: LlmClient | None = None,
) -> dict:
    client = llm or LlmClient()
    ctx = [
        f"## Current Idea\n{idea_description}",
        f"## Question\n{question}",
    ]
    if scores:
        ctx.append(f"## Scores\n{json.dumps(scores, default=str)[:2000]}")
    if competitors:
        ctx.append(f"## Competitors\n{json.dumps({'market_saturation': competitors.get('market_saturation'), 'direct_count': len(competitors.get('direct_competitors', [])), 'gaps': competitors.get('positioning_gaps', [])[:3]}, default=str)[:1500]}")
    if pricing:
        ctx.append(f"## Pricing\n{json.dumps({k: pricing.get(k) for k in ['recommended_price_monthly', 'pricing_model', 'desire_premium_applied']}, default=str)}")
    if desire:
        ctx.append(f"## Desire\n{json.dumps({k: desire.get(k) for k in ['primary_driver', 'desire_strength', 'virality_potential']}, default=str)}")
    if distribution:
        ctx.append(f"## Distribution\nk-factor: {distribution.get('composite_k_factor', 0)}")
    if history:
        ctx.append(f"## Past Decisions\n{json.dumps(history[-5:] if len(history) > 5 else history, default=str)[:1500]}")

    return client.chat_json(_load("what_if.txt"), "\n\n".join(ctx), temperature=0.4, max_tokens=3000)
