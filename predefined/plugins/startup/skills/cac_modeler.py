#!/usr/bin/env python3
"""CAC Modeler — estimates customer acquisition cost by channel."""

from __future__ import annotations
import os
from typing import Any
from llm import LlmClient

_PROMPT_DIR = os.path.join(os.path.dirname(os.path.dirname(__file__)), "prompts")

def _load_prompt(name: str) -> str:
    with open(os.path.join(_PROMPT_DIR, name), "r") as f:
        return f.read()

def model_cac(
    idea_description: str,
    pricing: dict | None = None,
    llm: LlmClient | None = None,
) -> dict[str, Any]:
    client = llm or LlmClient()
    system_prompt = _load_prompt("cac_model.txt")
    user_parts = [f"Estimate CAC for: {idea_description}"]
    if pricing:
        price = pricing.get("recommended_price_monthly", 0)
        user_parts.append(f"Target price: ${price}/mo")
    return client.chat_json(system_prompt, "\n".join(user_parts), temperature=0.2)
