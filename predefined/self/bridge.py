#!/usr/bin/env python3
"""
One-shot bridge between Rust gateway and self/ Python modules.

Usage:
  python3 bridge.py <method> [json-args]

Methods:
  system-prompt     Build complete system prompt (soul + skills + tools + memory)
  soul-prompt       Parse SOUL.md content → system prompt string (legacy)
  skills-prompt     Build skills section from SkillInfo JSON list (legacy)
  extraction-prompt Return the extraction prompt template
  entity-extraction-prompt  Return the entity extraction prompt
  parse-command     Parse a slash-command string → {skill_name, user_input}
  match-prefix      Match skill prefix → [matching skill names]

All methods read additional data from stdin as JSON, and write JSON to stdout.
Exit code 0 on success, 1 on error (with error message on stderr).
"""

from __future__ import annotations

import json
import sys
import os

# Ensure the predefined/ parent is on sys.path so "from self.xxx" works
_parent = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
if _parent not in sys.path:
    sys.path.insert(0, _parent)


def cmd_soul_prompt(args: dict) -> str:
    """Parse SOUL.md content and return the system prompt string."""
    from self.prompts.soul_builder import parse_soul

    content = args.get("content", "")
    if not content:
        filepath = args.get("file", "")
        if filepath:
            content = open(filepath).read()
    if not content:
        raise ValueError("soul-prompt requires 'content' or 'file' in args")

    soul = parse_soul(content)
    return soul.to_system_prompt()


def cmd_skills_prompt(args: dict) -> str:
    """Build skills system prompt from a list of SkillInfo dicts."""
    from self.prompts.skills_builder import build_skills_system_prompt, SkillInfo

    skills_data = args.get("skills", [])
    skills = [
        SkillInfo(
            name=s["name"],
            description=s.get("description", ""),
            category=s.get("category", "general"),
            platforms=s.get("platforms", []),
            environments=s.get("environments", []),
        )
        for s in skills_data
    ]
    return build_skills_system_prompt(skills)


def cmd_system_prompt(args: dict) -> str:
    """Build the complete system prompt (soul + skills + tools + memory).

    This is the primary entry point — replaces the old two-step
    soul-prompt + skills-prompt + full-prompt dance.
    """
    from self.system_prompt import build_system_prompt, SkillInfo, ToolDescriptor

    soul_content = args.get("soul_content", "")
    if not soul_content:
        filepath = args.get("file", "")
        if filepath:
            soul_content = open(filepath).read()
    if not soul_content:
        raise ValueError("system-prompt requires 'soul_content' or 'file' in args")

    skills_data = args.get("skills", [])
    skills = [
        SkillInfo(
            name=s["name"],
            description=s.get("description", ""),
            category=s.get("category", "general"),
            platforms=s.get("platforms", []),
            environments=s.get("environments", []),
        )
        for s in skills_data
    ]

    tools_data = args.get("tools", [])
    tools = [
        ToolDescriptor(
            name=t["name"],
            description=t.get("description", ""),
            parameters=t.get("parameters", ""),
        )
        for t in tools_data
    ]

    memory = args.get("memory", None)
    return build_system_prompt(soul_content, skills, tools, memory)


def cmd_extraction_prompt(args: dict) -> str:
    """Return the session extraction prompt template."""
    from self.prompts.reflection import extraction_prompt
    return extraction_prompt()


def cmd_entity_extraction_prompt(args: dict) -> str:
    """Return the entity extraction prompt for incubation memory indexing."""
    from self.system_prompt import build_entity_extraction_prompt
    return build_entity_extraction_prompt()


def cmd_parse_command(args: dict) -> dict:
    """Parse a slash-command string → {skill_name, user_input} or null."""
    from self.decisions.router import parse_skill_command

    text = args.get("text", "")
    result = parse_skill_command(text)
    if result is None:
        return None
    return {"skill_name": result[0], "user_input": result[1]}


def cmd_match_prefix(args: dict) -> list:
    """Match skill prefix → list of matching skill names."""
    from self.decisions.router import match_skill_prefix, SkillInfo

    prefix = args.get("prefix", "")
    skills_data = args.get("skills", [])
    skills = [
        SkillInfo(
            name=s["name"],
            description=s.get("description", ""),
            category=s.get("category", ""),
            path=s.get("path", ""),
        )
        for s in skills_data
    ]
    matches = match_skill_prefix(prefix, skills)
    return [s.name for s in matches]


# ── Method registry ──────────────────────────────────────────────────

METHODS = {
    "system-prompt": cmd_system_prompt,
    "soul-prompt": cmd_soul_prompt,
    "skills-prompt": cmd_skills_prompt,
    "extraction-prompt": cmd_extraction_prompt,
    "entity-extraction-prompt": cmd_entity_extraction_prompt,
    "parse-command": cmd_parse_command,
    "match-prefix": cmd_match_prefix,
}


def main() -> None:
    if len(sys.argv) < 2:
        print(json.dumps({"error": "missing method name"}), file=sys.stderr)
        sys.exit(1)

    method = sys.argv[1]

    # Read args from argv[2] if provided, otherwise from stdin
    if len(sys.argv) >= 3:
        args_raw = sys.argv[2]
    else:
        args_raw = sys.stdin.read().strip()

    try:
        args = json.loads(args_raw) if args_raw else {}
    except json.JSONDecodeError as e:
        print(json.dumps({"error": f"invalid JSON args: {e}"}), file=sys.stderr)
        sys.exit(1)

    func = METHODS.get(method)
    if func is None:
        print(json.dumps({"error": f"unknown method: {method}"}), file=sys.stderr)
        sys.exit(1)

    try:
        result = func(args)
        if isinstance(result, str):
            # String results: print directly (not as JSON string)
            print(result)
        else:
            print(json.dumps(result, ensure_ascii=False))
    except Exception as e:
        print(json.dumps({"error": str(e)}), file=sys.stderr)
        sys.exit(1)


if __name__ == "__main__":
    main()
