#!/usr/bin/env python3
"""Ikigai Alignment Check — four-circle Venn analysis of founder's work."""
import os, json
from typing import Any
from llm import LlmClient
_PROMPT_DIR = os.path.join(os.path.dirname(os.path.dirname(__file__)), "prompts")
def _load(n): return open(os.path.join(_PROMPT_DIR, n)).read()

def check_ikigai(ideas: list[dict], scores_history: dict[str, list] | None = None,
                  llm: LlmClient | None = None) -> dict:
    client = llm or LlmClient()
    ctx = [f"Founder's evaluated ideas ({len(ideas)}):\n" + json.dumps([
        {"slug": i.get("slug",""), "description": i.get("description",""),
         "verdict": i.get("verdict",""), "final_score": i.get("final_score",0)}
        for i in ideas], default=str)[:5000]]
    if scores_history:
        ctx.append(f"\nScore history:\n{json.dumps(scores_history, default=str)[:3000]}")
    return client.chat_json(_load("ikigai.md"), "\n\n".join(ctx), temperature=0.4)
