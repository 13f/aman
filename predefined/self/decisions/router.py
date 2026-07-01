"""
Skill routing — replaces crates/skill/src/execution.rs

Parses slash commands, matches skill names/prefixes, and resolves skills
to their full SKILL.md bodies.

Self-evolution hooks:
- match_skill_prefix: agent can upgrade from substring match to semantic
  match (embedding similarity, keyword weighting).
- parse_skill_command: supports new invocation patterns.
"""

from __future__ import annotations

from dataclasses import dataclass
from pathlib import Path
from typing import Optional


@dataclass
class SkillInfo:
    """Lightweight skill metadata."""
    name: str
    description: str
    category: str = "General"
    path: str = ""


@dataclass
class SkillExecution:
    """Resolved skill ready for LLM injection."""
    skill_name: str
    skill_body: str
    user_input: str
    augmented_message: str


# ── Command parsing ───────────────────────────────────────────────────

def parse_skill_command(input_text: str) -> Optional[tuple[str, str]]:
    """Parse a slash-command into (skill_name, user_input).

    Supports two forms:
    - `/skill skillName args...` — explicit command prefix
    - `/skillName args...` — direct skill invocation

    Returns None if input doesn't start with '/' or no skill name is found.
    """
    trimmed = input_text.strip()
    if not trimmed.startswith("/"):
        return None

    inner = trimmed[1:]  # strip leading '/'
    parts = inner.split(None, 2)  # split on whitespace, max 3 parts
    if not parts:
        return None

    first = parts[0]
    if not first:
        return None

    # "/skill skillName args..."
    if first == "skill" and len(parts) >= 2:
        skill_name = parts[1].strip()
        user_input = parts[2].strip() if len(parts) > 2 else ""
        return skill_name, user_input

    # "/skillName args..." (direct invocation)
    skill_name = first.strip()
    user_input = parts[1].strip() if len(parts) > 1 else ""
    return skill_name, user_input


# ── Skill matching ────────────────────────────────────────────────────

def match_skill_prefix(prefix: str, skills: list[SkillInfo]) -> list[SkillInfo]:
    """Filter skills by prefix matching name or description.

    Currently uses substring matching (mirrors Rust match_skill_prefix).
    Agent can upgrade this to semantic matching.
    """
    prefix = prefix.lstrip("/").lower()
    if not prefix:
        return list(skills)

    return [
        s for s in skills
        if prefix in s.name.lower() or prefix in s.description.lower()
    ]


# ── Skill resolution ──────────────────────────────────────────────────

def strip_frontmatter(raw: str) -> str:
    """Strip YAML frontmatter (---...---) from SKILL.md content."""
    s = raw.lstrip()
    if s.startswith("---"):
        if "\n---" in s:
            end = s.index("\n---")
            return s[end + 4:].lstrip()
    return s


# Directory names skipped when collecting supporting files — they hold
# reference material (examples, tests, cached output) that should not be
# injected into the LLM prompt.
_EXCLUDED_SKILL_DIRS = frozenset({
    "examples", "example", "testcase", "testcases", "test", "tests",
    "output", "outputs", "__pycache__", "node_modules", "fixtures",
})


def discover_supporting_files(skill_dir: str) -> list[tuple[str, str]]:
    """Walk skill_dir, return (relative_path, absolute_path) for every file
    except SKILL.md. Returns empty list if directory cannot be read.

    Hidden files (e.g. .DS_Store) and excluded directories
    (examples, tests, output, …) are skipped."""
    from pathlib import Path as P

    files = []
    base = P(skill_dir).resolve()
    if not base.is_dir():
        return files
    for entry in base.rglob("*"):
        if not entry.is_file():
            continue
        if entry.name == "SKILL.md":
            continue
        # Skip hidden files (.DS_Store, .gitignore, …).
        if entry.name.startswith("."):
            continue
        # Skip excluded directories (examples, tests, output, …).
        try:
            parts = entry.relative_to(base).parts
        except ValueError:
            continue
        if parts and parts[0] in _EXCLUDED_SKILL_DIRS:
            continue
        try:
            rel = str(entry.relative_to(base))
            files.append((rel, str(entry)))
        except ValueError:
            pass
    files.sort(key=lambda x: x[0])
    return files


def build_supporting_files_footer(skill_dir: str) -> str:
    """Build the ``[This skill has supporting files:]`` footer block."""
    files = discover_supporting_files(skill_dir)
    if not files:
        return ""
    lines = ["\n\n[This skill has supporting files:]"]
    for rel, abs_path in files:
        lines.append(f"- {rel}  ->  {abs_path}")
    return "\n".join(lines) + "\n"


def resolve_skill(
    skill_name: str,
    user_input: str,
    skills: list[SkillInfo],
) -> Optional[SkillExecution]:
    """Resolve a skill by name, read its SKILL.md, build augmented message.

    Returns None if skill not found or file unreadable.
    """
    info = None
    for s in skills:
        if s.name == skill_name:
            info = s
            break
    if info is None:
        return None

    try:
        raw = Path(info.path).read_text(encoding="utf-8")
    except (OSError, UnicodeDecodeError):
        return None

    body = strip_frontmatter(raw).strip()

    skill_dir = str(Path(info.path).parent)
    dir_header = f"[Skill directory: {skill_dir}]\n"
    supporting_footer = build_supporting_files_footer(skill_dir)

    if not user_input:
        augmented = (
            f"{dir_header}\n"
            f'[SKILL MODE] The user invoked skill "{skill_name}".\n\n'
            f"--- SKILL METHODOLOGY ---\n"
            f"{body}\n"
            f"--- END SKILL ---\n\n"
            f"Follow the skill's methodology, analysis framework, and output "
            f"template exactly. Execute each step in order. Do not skip or "
            f"abbreviate any prescribed stage."
            f"{supporting_footer}"
        )
    else:
        augmented = (
            f"{dir_header}\n"
            f'[SKILL MODE] The user invoked skill "{skill_name}" with the '
            f"following input:\n\n"
            f"> {user_input}\n\n"
            f"--- SKILL METHODOLOGY ---\n"
            f"{body}\n"
            f"--- END SKILL ---\n\n"
            f"Process the user's input above using the skill's methodology, "
            f"analysis framework, and output template. Execute each step in "
            f"order. Do not skip or abbreviate any prescribed stage."
            f"{supporting_footer}"
        )

    return SkillExecution(
        skill_name=skill_name,
        skill_body=body,
        user_input=user_input,
        augmented_message=augmented,
    )
