#!/usr/bin/env python3
"""Retention Predictor — predicts user retention and churn risk."""

from __future__ import annotations
import os
from typing import Any
from llm import LlmClient

_PROMPT_DIR = os.path.join(os.path.dirname(os.path.dirname(__file__)), "prompts")

def _load_prompt(name: str) -> str:
    with open(os.path.join(_PROMPT_DIR, name), "r") as f:
        return f.read()

def predict_retention(
    idea_description: str,
    desire: dict | None = None,
    llm: LlmClient | None = None,
) -> dict[str, Any]:
    client = llm or LlmClient()
    system_prompt = _load_prompt("retention.txt")
    user_parts = [f"Predict retention for: {idea_description}"]
    if desire:
        user_parts.append(f"Desire strength: {desire.get('desire_strength', 0)}, "
                         f"Primary driver: {desire.get('primary_driver', 'unknown')}")
    return client.chat_json(system_prompt, "\n".join(user_parts), temperature=0.2)
