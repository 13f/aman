#!/usr/bin/env python3
"""MVP Scope Negotiator — devil's advocate that cuts features, not adds them."""

import os, json
from typing import Any
from llm import LlmClient

_PROMPT_DIR = os.path.join(os.path.dirname(os.path.dirname(__file__)), "prompts")
def _load(n): return open(os.path.join(_PROMPT_DIR, n)).read()

def negotiate_mvp(idea_description: str, competitors: dict | None = None,
                  complexity: dict | None = None, llm: LlmClient | None = None) -> dict:
    client = llm or LlmClient()
    ctx = [f"Idea: {idea_description}"]
    if competitors:
        ctx.append(f"Competitor count: {len(competitors.get('direct_competitors', []))}")
        gaps = competitors.get("positioning_gaps", [])
        if gaps: ctx.append(f"Key differentiators: {json.dumps([g.get('description','') for g in gaps[:3]])}")
    if complexity:
        ctx.append(f"Build estimate: {complexity.get('build_time_estimate_months', 0)} months, "
                   f"riskiest: {complexity.get('riskiest_technical_unknown', 'N/A')}")
    return client.chat_json(_load("mvp_scope.txt"), "\n".join(ctx), temperature=0.4)
