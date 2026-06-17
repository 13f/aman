"""
Prompt builders — agent-modifiable templates that assemble system prompts,
skill indexes, tool descriptions, and reflection extraction prompts.

Each module replaces a piece of the Rust prompt pipeline, but in Python
so the agent can inspect and rewrite its own templates at runtime.
"""

import os


def load_skill_prompt(prompts_dir: str, skill_key: str) -> str:
    """Load a SKILL.md-style prompt file, stripping YAML frontmatter.

    Args:
      prompts_dir: Directory containing .md prompt files.
      skill_key: Internal skill key (e.g. 'desire', 'competitors').

    Returns the prompt body text (methodology/instructions) without frontmatter.
    """
    filename = f"{skill_key}.md"
    filepath = os.path.join(prompts_dir, filename)
    with open(filepath, "r") as f:
        content = f.read()
    # Strip YAML frontmatter delimited by ---
    if content.startswith("---"):
        parts = content.split("---", 2)
        if len(parts) >= 3:
            content = parts[2].strip()
    return content


from .soul_builder import Soul, parse_soul, soul_to_system_prompt
from .skills_builder import build_skills_system_prompt, build_skill_view_reinforcement
from .tools_builder import build_full_system_prompt, current_date_string
from .reflection import extraction_prompt, format_conversation

__all__ = [
    "load_skill_prompt",
    "Soul",
    "parse_soul",
    "soul_to_system_prompt",
    "build_skills_system_prompt",
    "build_skill_view_reinforcement",
    "build_full_system_prompt",
    "current_date_string",
    "extraction_prompt",
    "format_conversation",
]
