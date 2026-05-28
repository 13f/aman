"""
Decision modules — skill routing, command parsing, complexity assessment.

Replaces: crates/skill/src/execution.rs (parse_skill_command, match_skill_prefix)
Plus: the complexity assessment rules from the Decision Protocol in SOUL.md
"""

from .router import parse_skill_command, match_skill_prefix, resolve_skill
from .complexity import (
    ComplexityLevel,
    assess_complexity,
    recommended_action,
    COMPLEXITY_TABLE,
)

__all__ = [
    "parse_skill_command",
    "match_skill_prefix",
    "resolve_skill",
    "ComplexityLevel",
    "assess_complexity",
    "recommended_action",
    "COMPLEXITY_TABLE",
]
