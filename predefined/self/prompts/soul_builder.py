"""
SOUL.md parser and system prompt builder.

Replaces: crates/soul/src/lib.rs — Soul::parse() and Soul::to_system_prompt()

The SOUL.md format is markdown with ## sections. This module parses it
into a structured Soul object and renders it into the system prompt prefix.

Self-evolution hooks:
- Soul.template: the Jinja2-style template string. Agent can rewrite it.
- Soul.extra_sections: dynamic sections injected at runtime (e.g. "Current
  focus", "Recent lessons learned"). Not parsed from SOUL.md.
"""

from __future__ import annotations

import re
from dataclasses import dataclass, field
from pathlib import Path
from typing import Optional


# ── Default system prompt template ────────────────────────────────────
# The agent can rewrite this to change how it presents itself to the LLM.
# Variables: {name}, {identity}, {core}, {expertise}, {vibe}, {preferences},
#            {boundaries}, {extra_sections}
DEFAULT_SOUL_TEMPLATE = """You are {name}.
Identity: {identity}
Core: {core}
Expertise: {expertise}
Vibe: {vibe}
Preferences: {preferences}
Boundaries:
{boundaries}{extra_sections}"""


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
    template: str = DEFAULT_SOUL_TEMPLATE
    extra_sections: dict[str, str] = field(default_factory=dict)

    def to_system_prompt(self) -> str:
        """Render the system prompt from this soul."""
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

        return self.template.format(
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

            # Check derived forms (strip "do not", "don't", "never" prefixes)
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


# ── Parsing ───────────────────────────────────────────────────────────

def parse_soul(content: str) -> Soul:
    """Parse a SOUL.md string into a Soul object.

    Sections are delimited by ## headings. The top-level # heading sets
    the agent name. List sections (expertise, boundaries, preferences)
    use `- item` or `* item` bullet format.
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

        # Top-level heading → agent name
        if trimmed.startswith("# ") and not trimmed.startswith("## "):
            if title is None:
                title = trimmed[2:].strip()
            continue

        # Second-level heading → new section
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
    """Convenience: render a Soul to system prompt string."""
    return soul.to_system_prompt()


# ── Internal helpers ──────────────────────────────────────────────────

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
