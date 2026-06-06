#!/usr/bin/env python3
"""Cold Outreach Designer — generates targeted outbound campaigns."""

from __future__ import annotations
import os, json
from typing import Any
from llm import LlmClient

_PROMPT_DIR = os.path.join(os.path.dirname(os.path.dirname(__file__)), "prompts")

def _load_prompt(name: str) -> str:
    with open(os.path.join(_PROMPT_DIR, name), "r") as f:
        return f.read()

def design_outreach(
    idea_description: str,
    user_profile: str = "",
    competitors: dict | None = None,
    llm: LlmClient | None = None,
) -> dict[str, Any]:
    client = llm or LlmClient()
    system_prompt = _load_prompt("cold_outreach.txt")
    context = [f"Idea: {idea_description}"]
    if user_profile:
        context.append(f"Founder context: {user_profile}")
    if competitors:
        comp_names = [c.get("name", "") for c in competitors.get("direct_competitors", [])[:3]]
        if comp_names:
            context.append(f"Competitors to differentiate from: {', '.join(comp_names)}")
    return client.chat_json(system_prompt, "\n".join(context), temperature=0.6, max_tokens=3000)
