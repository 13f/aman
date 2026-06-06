#!/usr/bin/env python3
"""Desire Evaluator skill — scores how strongly an idea connects to
fundamental human motivations (survival, status, belonging, control, curiosity).
"""

from __future__ import annotations

import json
import os
from typing import Any

from llm import LlmClient


_PROMPT_DIR = os.path.join(os.path.dirname(os.path.dirname(__file__)), "prompts")


def _load_prompt(name: str) -> str:
    with open(os.path.join(_PROMPT_DIR, name), "r") as f:
        return f.read()


def evaluate_desire(
    idea_description: str,
    llm: LlmClient | None = None,
) -> dict[str, Any]:
    """Evaluate desire motivation for an idea.

    Args:
        idea_description: The app idea description.
        llm: LLM client (uses default config if None).

    Returns:
        Dict with desire_scores, primary_driver, desire_strength, etc.
    """
    client = llm or LlmClient()
    system_prompt = _load_prompt("desire_evaluation.txt")
    user_prompt = f"Evaluate the desire motivations for this app idea:\n\n{idea_description}"

    result = client.chat_json(system_prompt, user_prompt, temperature=0.2)

    # Validate and normalize
    required_fields = ["desire_scores", "primary_driver", "desire_strength", "desire_label", "virality_potential", "notes"]
    for field in required_fields:
        if field not in result:
            result[field] = "" if field != "desire_scores" else {}

    # Ensure scores are numeric
    scores = result.get("desire_scores", {})
    for dim in ["survival", "status", "belonging", "control", "curiosity"]:
        if dim not in scores:
            scores[dim] = 1
        scores[dim] = int(scores[dim])

    result["desire_strength"] = float(result.get("desire_strength", 2.0))
    return result
