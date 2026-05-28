"""
Full system prompt assembler — replaces crates/core/src/prompt.rs

Assembles the complete system prompt from: soul prompt → current date →
available tools (with formatting instructions) → retrieved memories.

Self-evolution hooks:
- TOOL_FORMAT_INSTRUCTIONS: how tools are presented. Agent can improve
  descriptions to reduce tool selection errors.
- FILE_OPS_DOCS: the file operation documentation block.
- WEB_SEARCH_REMINDER: the web search hint.
- The assembly order and separators can be changed.
"""

from __future__ import annotations

from dataclasses import dataclass
from datetime import datetime, timezone
from typing import Optional


@dataclass
class ToolDescriptor:
    """Mirrors kernel::react::ToolDescriptor."""
    name: str
    description: str
    parameters: str = ""


# ── Overridable template fragments ────────────────────────────────────

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

MEMORY_HEADER = "\n## Retrieved Memories\n"


# ── Assembly ──────────────────────────────────────────────────────────

def current_date_string() -> str:
    """Return today's date as YYYY-MM-DD (no chrono dependency needed)."""
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


def build_full_system_prompt(
    soul_prompt: str,
    tools: list[ToolDescriptor] | None = None,
    memory: str | None = None,
    *,
    date_str: str | None = None,
    include_file_ops: bool = True,
    include_web_reminder: bool = True,
) -> str:
    """Assemble the complete system prompt sent to the LLM.

    Order: soul prompt → date → tools → file ops → tool format → web reminder → memories
    """
    if date_str is None:
        date_str = current_date_string()

    parts: list[str] = [soul_prompt, f"Current date: {date_str}"]

    tools = tools or []
    if tools:
        parts.append(build_tool_list(tools))
        if include_file_ops:
            parts.append(FILE_OPS_DOCS)
        parts.append(TOOL_CALL_FORMAT)
        if include_web_reminder:
            parts.append(WEB_SEARCH_REMINDER)

    if memory and memory.strip():
        parts.append(f"{MEMORY_HEADER}{memory.strip()}")

    return "\n\n".join(parts)
