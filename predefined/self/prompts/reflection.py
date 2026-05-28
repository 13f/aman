"""
Reflection & memory extraction prompts — replaces crates/gateway/src/runtime/reflection.rs

Templates for session summarization, error classification, and lesson extraction.

Self-evolution hooks:
- EXTRACTION_PROMPT: the LLM prompt for session extraction. Agent can add
  new extraction fields or change the output schema.
- format_conversation: how events are serialized for the extraction LLM call.
"""

from __future__ import annotations

import json
from typing import Any


# ── Extraction prompt ─────────────────────────────────────────────────
# Agent can add fields like "emotions", "user_preferences", "follow_ups"
# to capture richer context from conversations.

EXTRACTION_PROMPT = """You are a memory extraction assistant. Given a conversation log between a user and an AI agent, extract a structured JSON summary with these fields:

- "intent": the user's primary goal in one sentence
- "decisions": array of key decisions made during the conversation
- "outputs": array of concrete results or deliverables produced
- "errors": array of errors encountered and how they were resolved
- "tags": array of topic tags for categorization
- "entities": array of named entities mentioned (people, tools, projects, etc.)

Respond with ONLY valid JSON, no markdown or explanation."""


# ── Error classification prompt ───────────────────────────────────────

ERROR_CLASSIFICATION_PROMPT = """You are an error analysis assistant. Given a list of errors from AI agent traces, classify each error into categories and identify patterns.

For each error, output:
- "error_type": a short category label (e.g. "tool_timeout", "parse_error", "permission_denied")
- "root_cause": likely cause in one sentence
- "recovery_possible": true/false
- "suggested_fix": one concrete action to prevent recurrence

Respond with ONLY valid JSON array, no markdown or explanation."""


# ── Lesson extraction prompt ──────────────────────────────────────────

LESSON_EXTRACTION_PROMPT = """You are a knowledge extraction assistant. Given successful task traces from an AI agent, extract reusable lessons.

For each trace, identify:
- "pattern": the reusable approach or decision pattern
- "when_to_apply": conditions under which this pattern is useful
- "pitfalls": what could go wrong
- "confidence": 1-10 rating of how generalizable this lesson is

Respond with ONLY valid JSON array, no markdown or explanation."""


# ── Conversation formatting ───────────────────────────────────────────

def format_conversation(
    events: list[dict[str, Any]],
    max_chars: int = 48000,
) -> str:
    """Format conversation events into compact text for LLM extraction.

    Each event is formatted as `[event_type] payload`. Payloads over
    2000 chars are truncated. Stops when max_chars is reached.
    """
    parts: list[str] = []
    used = 0

    for event in events:
        event_type = event.get("event_type", "unknown")
        payload = json.dumps(event.get("payload", {}), ensure_ascii=False)

        if len(payload) > 2000:
            payload = payload[:2000] + "…(truncated)"

        line = f"[{event_type}] {payload}\n"
        if used + len(line) > max_chars:
            break
        parts.append(line)
        used += len(line)

    return "".join(parts)


def extraction_prompt() -> str:
    """Return the session extraction system prompt."""
    return EXTRACTION_PROMPT


def error_classification_prompt() -> str:
    """Return the error classification system prompt."""
    return ERROR_CLASSIFICATION_PROMPT


def lesson_extraction_prompt() -> str:
    """Return the lesson extraction system prompt."""
    return LESSON_EXTRACTION_PROMPT
