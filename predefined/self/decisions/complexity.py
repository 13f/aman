"""
Complexity assessment — extracts the decision rules from the Decision Protocol
into executable Python so the agent can calibrate its own thresholds.

Replaces: the complexity table embedded in SOUL.md Decision Protocol Step 2.
In Rust this is inline markdown in build_skills_system_prompt().

Self-evolution hooks:
- COMPLEXITY_TABLE: the signal→level→action mapping. Agent can add new
  signals, adjust thresholds, or add confidence scores.
- assess_complexity: the classification function itself.
"""

from __future__ import annotations

from dataclasses import dataclass, field
from enum import Enum


class ComplexityLevel(str, Enum):
    SIMPLE = "simple"
    MEDIUM = "medium"
    COMPLEX = "complex"


@dataclass
class ComplexityRule:
    level: ComplexityLevel
    signals: list[str]
    examples: list[str]
    action: str


# ── Default complexity table (mirrors Decision Protocol Step 2) ───────
# Agent can rewrite this to adjust its own behavior.

COMPLEXITY_TABLE: list[ComplexityRule] = [
    ComplexityRule(
        level=ComplexityLevel.SIMPLE,
        signals=[
            "1-5 tool calls",
            "clear path",
            "no architecture decisions",
            "user says check/search/run/look at",
        ],
        examples=["check", "search", "run", "look at"],
        action="Execute directly — do not create a plan or todo",
    ),
    ComplexityRule(
        level=ComplexityLevel.MEDIUM,
        signals=[
            "3+ distinct steps",
            "2-5 files",
            "needs progress tracking",
            "user says add/fix/update",
        ],
        examples=["add", "fix", "update"],
        action="Load `todo` skill — track with task list, adjust as you go",
    ),
    ComplexityRule(
        level=ComplexityLevel.COMPLEX,
        signals=[
            "multi-stage",
            "architecture trade-offs",
            "spans subsystems",
            "destructive ops",
            "user says refactor/migrate/implement",
        ],
        examples=["refactor", "migrate", "implement"],
        action="Load `plan` skill — explore read-only, write plan, get approval before executing",
    ),
]

# When unsure between medium and complex, choose complex.
DEFAULT_AMBIGUITY_RESOLUTION = ComplexityLevel.COMPLEX


# ── Assessment ────────────────────────────────────────────────────────

def assess_complexity(
    user_input: str,
    file_count: int = 0,
    step_count: int = 0,
    is_destructive: bool = False,
    rules: list[ComplexityRule] | None = None,
) -> ComplexityLevel:
    """Classify a task into simple/medium/complex based on signals.

    This is a heuristic keyword-based classifier — the agent can replace
    this with an LLM call or learned classifier.
    """
    if rules is None:
        rules = COMPLEXITY_TABLE

    text = user_input.lower()
    scores: dict[ComplexityLevel, int] = {
        ComplexityLevel.SIMPLE: 0,
        ComplexityLevel.MEDIUM: 0,
        ComplexityLevel.COMPLEX: 0,
    }

    # Keyword matching against examples
    for rule in rules:
        for example in rule.examples:
            if example.lower() in text:
                scores[rule.level] += 1

    # Structural signals
    if file_count >= 5:
        scores[ComplexityLevel.COMPLEX] += 1
    elif file_count >= 2:
        scores[ComplexityLevel.MEDIUM] += 1

    if step_count >= 5:
        scores[ComplexityLevel.COMPLEX] += 1
    elif step_count >= 3:
        scores[ComplexityLevel.MEDIUM] += 1

    if is_destructive:
        scores[ComplexityLevel.COMPLEX] += 2

    # Determine level
    max_score = max(scores.values())
    if max_score == 0:
        return ComplexityLevel.SIMPLE

    # Tie-break: prefer higher complexity
    if scores[ComplexityLevel.COMPLEX] >= scores[ComplexityLevel.MEDIUM] and \
       scores[ComplexityLevel.COMPLEX] >= scores[ComplexityLevel.SIMPLE]:
        return ComplexityLevel.COMPLEX
    if scores[ComplexityLevel.MEDIUM] >= scores[ComplexityLevel.SIMPLE]:
        return ComplexityLevel.MEDIUM
    return ComplexityLevel.SIMPLE


def recommended_action(
    level: ComplexityLevel,
    rules: list[ComplexityRule] | None = None,
) -> str:
    """Return the recommended action for a complexity level."""
    if rules is None:
        rules = COMPLEXITY_TABLE
    for rule in rules:
        if rule.level == level:
            return rule.action
    return "Execute directly"
