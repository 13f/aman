"""
Skills system prompt builder — replaces crates/skill/src/formatting.rs

Builds the "Skills" section with an <available_skills> XML-style block
grouping skills by category. Skill matching is 100% LLM-driven — the model
reads each description and decides which skills to load via skill_view(name).

Self-evolution hooks:
- The prompt instruction text can be rewritten by the agent.
- Skill grouping and ordering heuristics.
"""

from __future__ import annotations

from dataclasses import dataclass
from typing import Optional


@dataclass
class SkillInfo:
    """Lightweight skill metadata (mirrors Rust SkillInfo)."""
    name: str
    description: str
    category: str = "general"
    triggers: list[str] = None
    platforms: list[str] = None
    environments: list[str] = None

    def __post_init__(self):
        if self.triggers is None:
            self.triggers = []
        if self.platforms is None:
            self.platforms = []
        if self.environments is None:
            self.environments = []


# ── Skill view reinforcement ─────────────────────────────────────────

SKILL_VIEW_REINFORCEMENT = """[The skill "{name}" has been loaded and is now active. \
Its instructions in the tool result above are authoritative \
for this task. You MUST follow its prescribed methodology, \
analysis framework, and output format completely. Do not skip \
or abbreviate any prescribed stage — execute each step in order.]"""


FORMAT_REMINDER_PREFIX = """[FORMAT INSTRUCTION] Data collection is complete. Now produce \
the final report using the skill's prescribed template. Fill ALL \
scoring sections — do not leave anything blank or marked "TBD". \
Output the report now in a single message, using the exact section \
headers and template layout from the skill."""


# ── Public builders ───────────────────────────────────────────────────

def build_skills_system_prompt(skills: list[SkillInfo]) -> str:
    """Build the full skills section for the system prompt.

    Produces a ``## Skills`` section with an ``<available_skills>`` XML-style
    block grouping skills by category. Returns empty string if no skills are
    provided.
    """
    if not skills:
        return ""

    out = "\n\n## Skills\n\n"
    out += (
        "Before replying, scan the skills below. If a skill matches or is even partially "
        "relevant to your task, you MUST load it with skill_view(name) and follow its "
        "instructions.\n\n"
    )
    out += "<available_skills>\n"

    # Group by category
    grouped: dict[str, list[SkillInfo]] = {}
    for s in skills:
        cat = s.category if s.category else "general"
        grouped.setdefault(cat, []).append(s)

    for category in sorted(grouped):
        out += f"  {category}:\n"
        for s in grouped[category]:
            out += f"    - {s.name}: {s.description}\n"

    out += "</available_skills>\n"
    return out


def build_skill_view_reinforcement(skill_name: str) -> str:
    """Message injected after skill_view to mark it as authoritative."""
    return SKILL_VIEW_REINFORCEMENT.format(name=skill_name)


def build_format_reminder(skill_body: Optional[str] = None) -> str:
    """Message injected after data collection to enforce output format."""
    msg = FORMAT_REMINDER_PREFIX
    if skill_body:
        msg += "\n\n---\n## Skill Methodology (re-injected, FULL)\n\n"
        msg += skill_body
        msg += "\n\n---\n"
        msg += (
            "Follow ALL sections, scoring dimensions, weights, sub-dimensions, "
            "traps, and template exactly as shown above. Fill every section completely."
        )
    return msg


def strip_frontmatter(raw: str) -> str:
    """Strip YAML frontmatter (---...---) from SKILL.md content."""
    s = raw.lstrip()
    if s.startswith("---"):
        # Find closing ---
        if "\n---" in s:
            end = s.index("\n---")
            return s[end + 4:].lstrip()
    return s
