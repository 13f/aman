#!/usr/bin/env python3
"""Complexity Assessment — estimates build difficulty for solo/indie developer."""

from __future__ import annotations
import os
from typing import Any
from llm import LlmClient

_PROMPT_DIR = os.path.join(os.path.dirname(os.path.dirname(__file__)), "prompts")

def _load_prompt(name: str) -> str:
    with open(os.path.join(_PROMPT_DIR, name), "r") as f:
        return f.read()

def assess_complexity(
    idea_description: str,
    llm: LlmClient | None = None,
) -> dict[str, Any]:
    client = llm or LlmClient()
    system_prompt = _load_prompt("complexity.md")
    return client.chat_json(system_prompt, idea_description, temperature=0.2)
