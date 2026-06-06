#!/usr/bin/env python3
"""Founder Decision Journal — cognitive bias detection from decision history."""
import os, json
from typing import Any
from llm import LlmClient
_PROMPT_DIR = os.path.join(os.path.dirname(os.path.dirname(__file__)), "prompts")
def _load(n): return open(os.path.join(_PROMPT_DIR, n)).read()

def audit_decisions(decisions: list[dict], llm: LlmClient | None = None) -> dict:
    client = llm or LlmClient()
    ctx = [f"Founder decision history ({len(decisions)} decisions):\n\n"
           + json.dumps(decisions, default=str)[:6000]]
    return client.chat_json(_load("decision_journal.txt"), "\n".join(ctx), temperature=0.3)
