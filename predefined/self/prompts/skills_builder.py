"""
Skills system prompt builder — replaces crates/skill/src/formatting.rs

Builds the "Available Skills" section with the mandatory Decision Protocol
(Progressive Disclosure Level 1 per agentskills.io).

Self-evolution hooks:
- DECISION_PROTOCOL_TEMPLATE: the full Step 1 / Step 2 text. Agent can
  rewrite the complexity table thresholds or add new decision paths.
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
    category: str = "General"
    triggers: list[str] = None

    def __post_init__(self):
        if self.triggers is None:
            self.triggers = []


# ── Decision Protocol template ────────────────────────────────────────
# Agent can rewrite this to change how it decides between direct execution,
# todo-based tracking, and plan-first workflows.

DECISION_PROTOCOL_TEMPLATE = """## Decision Protocol (mandatory)

### Step 1: Scan for matching skills

Scan the skills listed below. If ANY skill matches or is even partially \
relevant to the task, you MUST load it with `read_skill(skill: "...")` \
and follow its instructions. Err on the side of loading — it is always \
better to have context you don't need than to miss critical steps, pitfalls, \
or established workflows.

**If you found a matching skill → load it and follow it. You are done with \
this decision protocol.**

### Step 2: No matching skill — assess task complexity

Only reach this step if you have scanned ALL skills above and genuinely \
none are relevant. Now assess the task:

| Complexity | Signals | Action |
|------------|---------|--------|
| **Simple** | 1-5 tool calls, clear path, no architecture decisions, user says "check/search/run/look at" | Execute directly — do not create a plan or todo |
| **Medium** | 3+ distinct steps, 2-5 files, needs progress tracking, user says "add/fix/update" | Load `todo` skill — track with task list, adjust as you go |
| **Complex** | Multi-stage, architecture trade-offs, spans subsystems, destructive ops, user says "refactor/migrate/implement" | Load `plan` skill — explore read-only, write plan, get approval before executing |

**When unsure between medium and complex, choose complex (plan).** \
A 30-second plan costs far less than a wrong implementation.

Note: `plan`, `todo`, `writing-plans`, and `subagent-driven-development` \
are meta-skills for the fallback path — they guide HOW to work, not WHAT \
domain knowledge to apply. Only load them when no domain skill matches."""


SKILL_ACTIVATION_TEMPLATE = """[ACTIVATED SKILL: "{name}"]
The skill "{name}" matches your query. Call `read_skill(skill: "{name}")` \
now to load its full methodology, analysis framework, and output template.
You MUST load the skill with read_skill before proceeding — do not skip this step.
Begin your response by stating "[Skill: {name}]" to confirm activation."""


READ_SKILL_REINFORCEMENT = """[The skill "{name}" has been loaded and is now active. \
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

def build_skills_system_prompt(
    skills: list[SkillInfo],
    decision_protocol: str = DECISION_PROTOCOL_TEMPLATE,
) -> str:
    """Build the full skills section for the system prompt.

    Includes the Decision Protocol followed by the categorized skill index.
    Returns empty string if no skills are provided.
    """
    if not skills:
        return ""

    out = f"\n\n{decision_protocol}\n\n"

    out += "---\n\n### Available Skills\n\n"

    # Group by category
    grouped: dict[str, list[SkillInfo]] = {}
    for s in skills:
        cat = s.category if s.category else "General"
        grouped.setdefault(cat, []).append(s)

    for category in sorted(grouped):
        out += f"### {category}\n"
        for s in grouped[category]:
            out += f"- {s.name}: {s.description}\n"
        out += "\n"

    out += (
        "After completing a difficult or iterative task, consider offering to save "
        "the approach as a skill for future reuse by asking the user to create a new "
        "SKILL.md file.\n"
    )
    return out


def build_skill_activation_message(skill: SkillInfo) -> str:
    """Level 2 Progressive Disclosure — tell the LLM to load a skill."""
    return SKILL_ACTIVATION_TEMPLATE.format(name=skill.name)


def build_read_skill_reinforcement(skill_name: str) -> str:
    """Message injected after read_skill to mark it as authoritative."""
    return READ_SKILL_REINFORCEMENT.format(name=skill_name)


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
