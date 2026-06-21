"""
Unified system prompt builder — single entry point for all system prompt assembly.

Consolidates what was previously spread across:
  - prompts/soul_builder.py   (SOUL.md parsing, template rendering)
  - prompts/skills_builder.py (skills index <available_skills> block)
  - prompts/tools_builder.py  (final assembly: soul + date + tools + memories)

All three modules still exist and can be imported directly for backward
compatibility, but bridge.py now calls this module as the primary path.

Self-evolution hooks:
  - DEFAULT_SOUL_TEMPLATE: how the soul renders itself
  - TOOL_LIST_HEADER, FILE_OPS_DOCS, TOOL_CALL_FORMAT, WEB_SEARCH_REMINDER,
    WEB_FETCH_REMINDER, MEMORY_HEADER: tool/memory formatting
  - SKILL_VIEW_REINFORCEMENT, FORMAT_REMINDER_PREFIX: skill augmentation
  - The agent can rewrite these module-level constants at runtime.
"""

from __future__ import annotations

import re
from dataclasses import dataclass, field
from datetime import datetime, timezone
from pathlib import Path
from typing import Optional


# ═══════════════════════════════════════════════════════════════════════════
# Data classes
# ═══════════════════════════════════════════════════════════════════════════

@dataclass
class Soul:
    """Parsed SOUL.md content, ready for prompt generation."""

    name: str = "aman"
    identity: str = ""
    core: str = ""
    expertise: list[str] = field(default_factory=list)
    boundaries: list[str] = field(default_factory=list)
    vibe: str = ""
    preferences: list[str] = field(default_factory=list)
    raw: str = ""
    template: str = ""  # set from DEFAULT_SOUL_TEMPLATE below
    extra_sections: dict[str, str] = field(default_factory=dict)

    def to_system_prompt(self) -> str:
        """Render the soul portion of the system prompt."""
        expertise = ", ".join(self.expertise) if self.expertise else ""
        preferences = "; ".join(self.preferences) if self.preferences else ""

        if self.boundaries:
            boundaries = "\n".join(f"- {b}" for b in self.boundaries)
        else:
            boundaries = ""

        extra = ""
        if self.extra_sections:
            extra = "\n" + "\n".join(
                f"## {k}\n{v}" for k, v in self.extra_sections.items()
            )

        tpl = self.template or DEFAULT_SOUL_TEMPLATE
        return tpl.format(
            name=self.name,
            identity=self.identity.strip(),
            core=self.core.strip(),
            expertise=expertise,
            vibe=self.vibe.strip(),
            preferences=preferences,
            boundaries=boundaries,
            extra_sections=extra,
        )

    def check_boundary(self, text: str) -> tuple[bool, Optional[str]]:
        """Check if text violates any boundary. Returns (blocked, boundary_text)."""
        text_lower = text.strip().lower()
        for boundary in self.boundaries:
            trimmed = boundary.strip()
            if not trimmed:
                continue
            boundary_lower = trimmed.lower()

            derived = None
            for prefix in ("do not ", "don't ", "never "):
                if boundary_lower.startswith(prefix):
                    derived = boundary_lower[len(prefix):].strip()
                    break

            if boundary_lower in text_lower:
                return True, trimmed
            if derived and derived in text_lower:
                return True, trimmed

        return False, None


@dataclass
class SkillInfo:
    """Lightweight skill metadata (mirrors Rust SkillInfo)."""
    name: str
    description: str
    category: str = "general"
    triggers: list[str] = field(default_factory=list)
    platforms: list[str] = field(default_factory=list)
    environments: list[str] = field(default_factory=list)


@dataclass
class ToolDescriptor:
    """Mirrors kernel::react::ToolDescriptor."""
    name: str
    description: str
    parameters: str = ""


# ═══════════════════════════════════════════════════════════════════════════
# Overridable template fragments (self-evolution hooks)
# ═══════════════════════════════════════════════════════════════════════════

DEFAULT_SOUL_TEMPLATE = """You are {name}.
Identity: {identity}
Core: {core}
Expertise: {expertise}
Vibe: {vibe}
Preferences: {preferences}
Boundaries:
{boundaries}{extra_sections}"""

TOOL_LIST_HEADER = "\n## Available Tools\nYou can use these tools when responding:\n"

TOOL_ITEM_TEMPLATE = "- {name}: {description} (parameters: {parameters})"

FILE_OPS_DOCS = """## File Operations (safe, no shell)
 - read(path): read file contents
 - write(path, content): write file (auto-creates parent dirs)
 - edit(file_path, old_string, new_string): replace exact matching text in file
 - list(path): list directory entries
 - find(pattern, base): search files by name (recursive, case-insensitive)
 - grep(pattern, path, glob?): search file contents via ripgrep (multi-threaded)"""

TOOL_CALL_FORMAT = """When you need to use a tool, respond with a JSON tool call in the format:\
```tool_call
{"name": "tool_name", "arguments": {...}}
```"""

WEB_SEARCH_REMINDER = """Important: If the user asks about current events, recent data, prices, dates, \
or any time-sensitive information, use the web_search tool first rather than \
relying on your training data. For example, search for "recent" or "today" \
queries instead of answering from memory."""

WEB_FETCH_REMINDER = """To read the full content of a web page, fetch a specific URL, or download raw \
data from an API endpoint, use the web_fetch tool. Typical flow: find URLs \
via web_search, then read them via web_fetch."""

MEMORY_HEADER = "\n## Retrieved Memories\n"

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


# ═══════════════════════════════════════════════════════════════════════════
# SOUL parsing
# ═══════════════════════════════════════════════════════════════════════════

def parse_soul(content: str) -> Soul:
    """Parse a SOUL.md string into a Soul object.

    Sections are delimited by ## headings. The top-level # heading sets
    the agent name. List sections (expertise, boundaries, preferences)
    use ``- item`` or ``* item`` bullet format.
    """
    title: Optional[str] = None
    sections: dict[str, str] = {}
    current_section: Optional[str] = None
    current_lines: list[str] = []

    def flush() -> None:
        nonlocal current_section, current_lines
        if current_section is not None:
            sections[current_section] = "\n".join(current_lines).strip()
            current_lines.clear()
            current_section = None

    for line in content.splitlines():
        trimmed = line.strip()

        if trimmed.startswith("# ") and not trimmed.startswith("## "):
            if title is None:
                title = trimmed[2:].strip()
            continue

        if trimmed.startswith("## "):
            flush()
            current_section = trimmed[3:].strip().lower()
            continue

        if current_section is not None:
            current_lines.append(line)

    flush()

    name = title or sections.get("name") or "aman"

    return Soul(
        name=name,
        identity=sections.get("identity", ""),
        core="\n".join(_section_lines(sections, "core truths", "core")),
        expertise=_parse_list(sections, "expertise"),
        boundaries=_parse_list(sections, "boundaries"),
        vibe=sections.get("vibe", ""),
        preferences=_parse_list(sections, "preferences"),
        raw=content,
    )


def parse_soul_file(path: str | Path) -> Soul:
    """Load and parse a SOUL.md file."""
    return parse_soul(Path(path).read_text(encoding="utf-8"))


def soul_to_system_prompt(soul: Soul) -> str:
    """Convenience: render a Soul to its system prompt string."""
    return soul.to_system_prompt()


# ═══════════════════════════════════════════════════════════════════════════
# Skills section builder
# ═══════════════════════════════════════════════════════════════════════════

def build_skills_section(skills: list[SkillInfo]) -> str:
    """Build the ``## Skills`` section with ``<available_skills>`` XML block.

    Returns empty string if no skills are provided.
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
        if "\n---" in s:
            end = s.index("\n---")
            return s[end + 4:].lstrip()
    return s


# ═══════════════════════════════════════════════════════════════════════════
# Tool list builder
# ═══════════════════════════════════════════════════════════════════════════

def current_date_string() -> str:
    """Return today's date as YYYY-MM-DD."""
    return datetime.now(timezone.utc).strftime("%Y-%m-%d")


def build_tool_list(tools: list[ToolDescriptor]) -> str:
    """Format the tool list section."""
    items = [
        TOOL_ITEM_TEMPLATE.format(
            name=t.name, description=t.description, parameters=t.parameters
        )
        for t in tools
    ]
    return TOOL_LIST_HEADER + "\n".join(items)


# ═══════════════════════════════════════════════════════════════════════════
# Special-purpose prompts (entity extraction, reflection, etc.)
# ═══════════════════════════════════════════════════════════════════════════

ENTITY_EXTRACTION_PROMPT = """\
You are an entity extraction system. Extract named entities \
(people, places, organizations, concepts, technical terms, product names, \
project names, tool names) from each content block below. \
Return ONLY a JSON object where keys are content indices ("1", "2", etc.) \
and values are arrays of entity strings. \
Example: {"1": ["Neural Networks", "Yann LeCun"], "2": ["Rust", "Actix-Web"]} \
If no entities are found in a block, return an empty array."""


def build_entity_extraction_prompt() -> str:
    """Return the entity extraction system prompt for incubation memory indexing."""
    return ENTITY_EXTRACTION_PROMPT


# ═══════════════════════════════════════════════════════════════════════════
# Main entry point — complete system prompt assembly
# ═══════════════════════════════════════════════════════════════════════════

def build_system_prompt(
    soul_content: str,
    skills: list[SkillInfo] | None = None,
    tools: list[ToolDescriptor] | None = None,
    memory: str | None = None,
    *,
    date_str: str | None = None,
    include_file_ops: bool = True,
    include_web_reminder: bool = True,
    include_web_fetch_reminder: bool = True,
) -> str:
    """Build the complete system prompt from all components.

    Assembly order:
      1. Soul identity (parsed from SOUL.md content)
      2. Skills index (<available_skills> XML block)
      3. Current date
      4. Available tools (with formatting instructions)
      5. File operations docs
      6. Tool call format
      7. Web search reminder
      8. Web fetch reminder
      9. Retrieved memories

    This is the single function that bridge.py calls — no more multi-step
    assembly on the Rust side.
    """
    if date_str is None:
        date_str = current_date_string()

    # 1. Parse and render SOUL
    soul = parse_soul(soul_content)
    parts: list[str] = [soul.to_system_prompt()]

    # 2. Skills section
    skills = skills or []
    skills_section = build_skills_section(skills)
    if skills_section:
        parts.append(skills_section)

    # 3. Date
    parts.append(f"Current date: {date_str}")

    # 4-8. Tools
    tools = tools or []
    if tools:
        parts.append(build_tool_list(tools))
        if include_file_ops:
            parts.append(FILE_OPS_DOCS)
        parts.append(TOOL_CALL_FORMAT)
        if include_web_reminder:
            parts.append(WEB_SEARCH_REMINDER)
        if include_web_fetch_reminder:
            parts.append(WEB_FETCH_REMINDER)

    # 9. Memories
    if memory and memory.strip():
        parts.append(f"{MEMORY_HEADER}{memory.strip()}")

    return "\n\n".join(parts)


# ═══════════════════════════════════════════════════════════════════════════
# Internal helpers
# ═══════════════════════════════════════════════════════════════════════════

def _section_lines(sections: dict[str, str], *keys: str) -> list[str]:
    """Get non-empty lines from the first matching section key."""
    for key in keys:
        text = sections.get(key, "")
        if text.strip():
            return [line for line in text.splitlines() if line.strip()]
    return []


def _parse_list(sections: dict[str, str], *keys: str) -> list[str]:
    """Parse a bullet list from the first matching section."""
    lines = _section_lines(sections, *keys)
    items: list[str] = []
    for line in lines:
        stripped = line.strip()
        for prefix in ("- ", "* "):
            if stripped.startswith(prefix):
                item = stripped[len(prefix):].strip()
                if item:
                    items.append(item)
                break
    return items
