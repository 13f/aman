"""
Prompt builders — agent-modifiable templates that assemble system prompts,
skill indexes, tool descriptions, and reflection extraction prompts.

Each module replaces a piece of the Rust prompt pipeline, but in Python
so the agent can inspect and rewrite its own templates at runtime.
"""

from .soul_builder import Soul, parse_soul, soul_to_system_prompt
from .skills_builder import build_skills_system_prompt, build_skill_activation_message
from .tools_builder import build_full_system_prompt, current_date_string
from .reflection import extraction_prompt, format_conversation

__all__ = [
    "Soul",
    "parse_soul",
    "soul_to_system_prompt",
    "build_skills_system_prompt",
    "build_skill_activation_message",
    "build_full_system_prompt",
    "current_date_string",
    "extraction_prompt",
    "format_conversation",
]
