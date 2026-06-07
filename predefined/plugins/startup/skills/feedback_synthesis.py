#!/usr/bin/env python3
"""User Feedback Synthesizer — unstructured feedback → structured insights."""
import os
from typing import Any
from llm import LlmClient
_PROMPT_DIR = os.path.join(os.path.dirname(os.path.dirname(__file__)), "prompts")
def _load(n): return open(os.path.join(_PROMPT_DIR, n)).read()

def synthesize_feedback(feedback_text: str, competitor_analysis: dict | None = None,
                         llm: LlmClient | None = None) -> dict:
    client = llm or LlmClient()
    ctx = [f"User feedback to analyze:\n\n{feedback_text[:8000]}"]
    if competitor_analysis:
        gaps = competitor_analysis.get("positioning_gaps", [])
        if gaps:
            ctx.append(f"\nCompare against these known competitor gaps:\n"
                       + "\n".join(f"- [{g.get('gap_type','')}] {g.get('description','')}" for g in gaps))
    return client.chat_json(_load("feedback_synthesis.md"), "\n\n".join(ctx), temperature=0.3, max_tokens=4000)
