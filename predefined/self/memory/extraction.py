"""
Memory extraction strategies — customizable templates and logic for what
the agent remembers and how it organizes memories.

Self-evolution hooks:
- ExtractionStrategy: defines what fields to extract, how to tag, how to
  relate entities. Agent can create new strategies for different memory types.
- should_extract: gate function — agent learns when extraction is worthwhile.
"""

from __future__ import annotations

from dataclasses import dataclass, field
from typing import Any, Callable


# ── Extraction strategies ─────────────────────────────────────────────

@dataclass
class ExtractionStrategy:
    """Defines what and how to extract from a conversation into memory."""

    name: str
    description: str
    # Prompt sent to the LLM for extraction
    system_prompt: str
    # JSON schema fields the LLM should output
    output_fields: list[str]
    # Tags to attach to stored memories
    default_tags: list[str] = field(default_factory=list)
    # Entity relationship types to create
    relation_types: list[str] = field(default_factory=list)
    # Max conversation chars to send to extraction LLM
    max_chars: int = 48000
    # Max events to load for extraction
    max_events: int = 200


# ── Built-in strategies ───────────────────────────────────────────────

SESSION_EXTRACTION = ExtractionStrategy(
    name="session_extract",
    description="Extract intent, decisions, outputs, errors, tags, and entities from a session",
    system_prompt="""You are a memory extraction assistant. Given a conversation log between a user and an AI agent, extract a structured JSON summary with these fields:

- "intent": the user's primary goal in one sentence
- "decisions": array of key decisions made during the conversation
- "outputs": array of concrete results or deliverables produced
- "errors": array of errors encountered and how they were resolved
- "tags": array of topic tags for categorization
- "entities": array of named entities mentioned (people, tools, projects, etc.)

Respond with ONLY valid JSON, no markdown or explanation.""",
    output_fields=["intent", "decisions", "outputs", "errors", "tags", "entities"],
    default_tags=["session_extract"],
    relation_types=["appears_in"],
)


USER_PREFERENCE_EXTRACTION = ExtractionStrategy(
    name="user_preference",
    description="Extract user preferences, habits, and communication style from interactions",
    system_prompt="""You are a user modeling assistant. Given a conversation between a user and an AI agent, extract user preferences.

Output JSON with:
- "preferences": array of stated or implied preferences (e.g. "prefers concise answers", "uses vim keybindings")
- "anti_preferences": array of things the user dislikes or avoids
- "communication_style": short description of how the user communicates
- "domain_expertise": areas the user seems knowledgeable about
- "confidence": 1-10 rating of how confident you are in each extraction

Respond with ONLY valid JSON, no markdown or explanation.""",
    output_fields=["preferences", "anti_preferences", "communication_style", "domain_expertise", "confidence"],
    default_tags=["user_preference", "user_model"],
    relation_types=["prefers", "expert_in"],
)


ERROR_PATTERN_EXTRACTION = ExtractionStrategy(
    name="error_pattern",
    description="Identify recurring error patterns and successful recovery strategies",
    system_prompt="""You are an error analysis assistant. Given error logs from an AI agent, identify patterns.

Output JSON with:
- "patterns": array of {error_type, frequency, typical_cause, recovery_strategy}
- "most_common": the single most frequent error type
- "unrecovered": errors that were never successfully resolved
- "recommendations": concrete actions to reduce these errors

Respond with ONLY valid JSON, no markdown or explanation.""",
    output_fields=["patterns", "most_common", "unrecovered", "recommendations"],
    default_tags=["error_pattern", "self_improvement"],
    relation_types=["caused_by", "resolved_by"],
)


# ── Registry ──────────────────────────────────────────────────────────

BUILTIN_STRATEGIES: dict[str, ExtractionStrategy] = {
    s.name: s for s in [SESSION_EXTRACTION, USER_PREFERENCE_EXTRACTION, ERROR_PATTERN_EXTRACTION]
}


def get_strategy(name: str) -> ExtractionStrategy | None:
    """Look up an extraction strategy by name."""
    return BUILTIN_STRATEGIES.get(name)


def register_strategy(strategy: ExtractionStrategy) -> None:
    """Register a new or updated extraction strategy (agent self-modification)."""
    BUILTIN_STRATEGIES[strategy.name] = strategy


# ── Extraction gating ─────────────────────────────────────────────────
# Agent can replace this with a learned function.

def should_extract(
    event_count: int,
    has_errors: bool = False,
    has_decisions: bool = False,
    min_events: int = 2,
) -> bool:
    """Decide whether extraction is worthwhile for a session.

    Currently a simple heuristic — at least min_events events, or any
    errors/decisions present.
    """
    if event_count < min_events:
        return False
    if has_errors or has_decisions:
        return True
    return event_count >= 5
