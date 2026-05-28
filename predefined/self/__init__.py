"""
Self — agent self-iteration, update, and evolution modules.

These Python modules live in the user data directory and can be modified
by the agent at runtime. They replace/extend Rust-side prompt building,
decision logic, memory extraction, and provide new self-evolution
infrastructure (A/B testing, self-audit).

Packages:
- prompts/    — system prompt, skills prompt, tool prompt, reflection templates
- decisions/  — skill routing, command parsing, complexity assessment
- memory/     — extraction strategies, memory organization
- evolution/  — prompt mutation, variant tracking, self-audit
"""

from . import prompts
from . import decisions
from . import memory
from . import evolution

__all__ = ["prompts", "decisions", "memory", "evolution"]
