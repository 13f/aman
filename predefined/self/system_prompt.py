"""
Unified system prompt builder — single entry point for all system prompt assembly.

Three-layer structure (Hermes-inspired, stable → context → volatile):
  STABLE:   soul identity, task discipline, tool guidance, platform hint, skills
  CONTEXT:  CLAUDE.md / project context files
  VOLATILE: USER.md snapshot, MEMORY.md snapshot, timestamp

Built once at session start, cached, and reused — protects LLM prefix cache.

Consolidates what was previously spread across:
  - prompts/soul_builder.py   (SOUL.md parsing, template rendering)
  - prompts/skills_builder.py (skills index <available_skills> block)
  - prompts/tools_builder.py  (final assembly: soul + date + tools + memories)

Self-evolution hooks: module-level template constants the agent can rewrite.
"""

from __future__ import annotations

import os
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
# STABLE layer — built once, never changes within a session
# ═══════════════════════════════════════════════════════════════════════════

# -- Soul template --

DEFAULT_SOUL_TEMPLATE = """You are {name}.
Identity: {identity}
Core: {core}
Expertise: {expertise}
Vibe: {vibe}
Preferences: {preferences}
Boundaries:
{boundaries}{extra_sections}"""

# -- Task completion discipline (Hermes §3) --

TASK_COMPLETION_DISCIPLINE = """## Task Completion Discipline

You MUST fully complete tasks, not stop at stubs, plans, or code outlines.
When asked to build or implement something:
- Write complete, working code — no placeholder comments like "// TODO" or "// add logic here"
- Run the code to verify it works, then fix any errors before reporting success
- If a multi-step plan is needed, execute ALL steps — don't stop after planning
- When you encounter an error, debug it yourself rather than asking the user to fix it"""

# -- Parallel tool call guidance (Hermes §4) --

PARALLEL_TOOL_GUIDANCE = """## Parallel Tool Calls

When you need to call multiple tools and their results don't depend on each other,
batch them into a single response. This reduces round-trips and is faster.
- Independent reads/searches → batch together
- Tool A's output is needed as Tool B's input → sequential (don't batch)"""

# -- Tool-use enforcement (Hermes §9) --

TOOL_USE_ENFORCEMENT = """## Tool Use

You MUST actually call tools to perform actions — never just describe what you
would do or what tool you would call. If a task requires reading a file, call
the read tool. If it requires running a command, call the exec tool. Always
execute, never speculate."""

# -- Platform hints (Hermes §17) --

PLATFORM_HINT_CLI = """## Platform

You are a CLI AI Agent running in a terminal. Prefer concise output. Avoid
rich markdown (tables, bold, headers) when a simple text format would suffice.
Code blocks and JSON are fine."""

PLATFORM_HINT_DESKTOP = """## Platform

You are a Desktop AI Agent. You can use rich markdown formatting including
tables, headers, and formatting when it improves clarity."""

# -- Skills section --

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
# CONTEXT layer — session-stable, depends on working directory
# ═══════════════════════════════════════════════════════════════════════════

# -- CLAUDE.md / project context (Hermes §19) --

CLAUDE_MD_HEADER = "\n## Project Context (CLAUDE.md)\n"


# ═══════════════════════════════════════════════════════════════════════════
# VOLATILE layer — snapshot at session start, frozen for the session
# ═══════════════════════════════════════════════════════════════════════════

# -- USER.md (Hermes §21) --

USER_MD_HEADER = "\n## User Profile\n"
USER_MD_MAX_CHARS = 2000

# -- MEMORY.md (Hermes §20) --

MEMORY_MD_HEADER = "\n## Agent Memory\n"
MEMORY_MD_MAX_CHARS = 2200


# ═══════════════════════════════════════════════════════════════════════════
# TOOLS section (appended after VOLATILE, only when tools are available)
# ═══════════════════════════════════════════════════════════════════════════

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


# ═══════════════════════════════════════════════════════════════════════════
# Special-purpose prompts (entity extraction, reflection, etc.)
# ═══════════════════════════════════════════════════════════════════════════

ENTITY_EXTRACTION_PROMPT = """\
You are an entity extraction system. Extract named entities \
(people, places, organizations, concepts, technical terms, product names, \
project names, tool names) from each content block below. \
Return a JSON object with an "entities" field containing an array of arrays, \
where each inner array corresponds to one content block in order. \
Example: {"entities": [["Neural Networks", "Yann LeCun"], ["Rust", "Actix-Web"]]} \
If no entities are found in a block, return an empty array for that position."""


def build_entity_extraction_prompt() -> str:
    """Return the entity extraction system prompt for incubation memory indexing."""
    return ENTITY_EXTRACTION_PROMPT


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
# Environment hints (Hermes §13) — OS, home, cwd, shell
# ═══════════════════════════════════════════════════════════════════════════

ENVIRONMENT_HINTS_HEADER = "## Environment"

# Remote backends where host info should be suppressed.
_REMOTE_BACKENDS = frozenset({"docker", "singularity", "modal", "daytona", "ssh"})


def _is_wsl() -> bool:
    """Check if running under Windows Subsystem for Linux."""
    try:
        return "microsoft" in Path("/proc/version").read_text().lower()
    except OSError:
        return False


def build_environment_hints(cwd: str | None = None) -> str:
    """Return a concise environment block: OS, home, cwd, shell.

    Suppresses host info when running inside a remote/sandbox backend
    (detected via TERMINAL_ENV env var).
    """
    import platform
    import sys
    import shutil

    backend = (os.getenv("TERMINAL_ENV") or "local").strip().lower()
    is_remote = backend in _REMOTE_BACKENDS

    lines: list[str] = []

    if not is_remote:
        # Host info
        if _is_wsl():
            lines.append("Host: WSL (Windows Subsystem for Linux)")
        elif sys.platform == "darwin":
            mac_ver = platform.mac_ver()[0]
            lines.append(f"Host: macOS ({mac_ver or platform.release()})")
        elif sys.platform == "win32":
            lines.append(f"Host: Windows ({platform.release()})")
        else:
            lines.append(f"Host: {platform.system()} ({platform.release()})")

        lines.append(f"Home: {os.path.expanduser('~')}")

        try:
            resolved_cwd = cwd or os.getcwd()
            lines.append(f"CWD: {resolved_cwd}")
        except OSError:
            pass

        shell = os.getenv("SHELL") or os.getenv("COMSPEC") or ""
        if shell:
            lines.append(f"Shell: {shell}")
    else:
        lines.append(
            f"Terminal backend: {backend}. Tool execution happens inside "
            f"this {backend} environment, not on the host machine."
        )

    return "\n".join(lines)


# ═══════════════════════════════════════════════════════════════════════════
# Python toolchain probe (Hermes §15) — only emits when non-clean
# ═══════════════════════════════════════════════════════════════════════════

def build_python_probe_line() -> str:
    """Probe python3/pip/uv state. Returns "" when environment is clean.

    Emits a single compact line only when something is non-default:
    missing pip module, PEP 668 without uv, or version mismatch.
    This saves the model from discovering by failure.
    """
    import shutil
    import subprocess

    def _python_version(binary: str) -> str | None:
        if not shutil.which(binary):
            return None
        try:
            r = subprocess.run([binary, "--version"], capture_output=True, text=True, timeout=5)
            return r.stdout.strip().split()[1] if r.returncode == 0 else None
        except (OSError, subprocess.SubprocessError):
            return None

    def _pip_bound_version() -> str | None:
        if not shutil.which("pip"):
            return None
        try:
            r = subprocess.run(["pip", "--version"], capture_output=True, text=True, timeout=5)
            if r.returncode == 0 and "(python " in r.stdout and r.stdout.endswith(")"):
                tail = r.stdout.rsplit("(python ", 1)[1]
                return tail[:-1].strip()
        except (OSError, subprocess.SubprocessError):
            pass
        return None

    # Skip for remote backends — host Python is irrelevant.
    backend = (os.getenv("TERMINAL_ENV") or "local").strip().lower()
    if backend in _REMOTE_BACKENDS:
        return ""

    py3_ver = _python_version("python3")
    pip_bound = _pip_bound_version()
    has_uv = shutil.which("uv") is not None

    # Silent when environment is clean: python3 exists, pip matches, no PEP 668 issues
    mismatch = bool(pip_bound and py3_ver and not py3_ver.startswith(pip_bound))
    if py3_ver and not mismatch:
        # Check PEP 668 only when clean otherwise — avoid extra subprocess when already noisy
        try:
            r = subprocess.run(
                ["python3", "-c",
                 "import sys,os;m=os.path.join(os.path.dirname(os.__file__),'EXTERNALLY-MANAGED');"
                 "print('yes' if os.path.exists(m) else 'no')"],
                capture_output=True, text=True, timeout=5,
            )
            pep668 = r.returncode == 0 and r.stdout.strip() == "yes"
        except (OSError, subprocess.SubprocessError):
            pep668 = False

        if not pep668 or has_uv:
            return ""  # clean — nothing to report

    # Build compact one-liner
    bits: list[str] = []
    if py3_ver:
        py3_bit = f"python3={py3_ver}"
        try:
            r = subprocess.run(
                ["python3", "-c", "import pip; print(pip.__version__)"],
                capture_output=True, text=True, timeout=5,
            )
            if r.returncode == 0 and r.stdout.strip():
                py3_bit += f", pip={r.stdout.strip()}"
        except (OSError, subprocess.SubprocessError):
            pass
        bits.append(py3_bit)
    else:
        bits.append("python3: NOT FOUND")

    if pip_bound:
        bits.append(f"pip → python{pip_bound}")
    if has_uv:
        bits.append("uv: available")
    if mismatch:
        bits.append("(pip/python version mismatch)")

    return f"Python: {', '.join(bits)}"


# ═══════════════════════════════════════════════════════════════════════════
# Coding posture (Hermes §14) — cwd + git snapshot
# ═══════════════════════════════════════════════════════════════════════════

CODING_POSTURE_HEADER = "\n## Workspace"

_GIT_TIMEOUT = 3  # seconds


def build_coding_posture(cwd: str | None = None) -> str:
    """Return a concise workspace snapshot: cwd + git branch/status.

    Only emits when the working directory is inside a git repository.
    Returns "" outside a git repo.
    """
    import subprocess

    try:
        resolved = cwd or os.getcwd()
    except OSError:
        return ""

    def _git(*args: str) -> str:
        try:
            r = subprocess.run(
                ["git", "-C", resolved, *args],
                capture_output=True, text=True, timeout=_GIT_TIMEOUT,
            )
            return r.stdout.strip() if r.returncode == 0 else ""
        except (OSError, subprocess.SubprocessError):
            return ""

    # Is this a git repo?
    if not _git("rev-parse", "--is-inside-work-tree") == "true":
        return ""

    lines = ["Workspace (snapshot at session start — re-check with `git` before acting on it):"]
    lines.append(f"- Root: {resolved}")

    # Branch
    branch = _git("branch", "--show-current")
    if branch:
        lines.append(f"- Branch: {branch}")
    elif _git("rev-parse", "--abbrev-ref", "HEAD") == "HEAD":
        lines.append("- Branch: (detached HEAD)")

    # Status summary
    porcelain = _git("status", "--porcelain")
    if porcelain:
        staged = sum(1 for l in porcelain.splitlines() if l[0] != " " and l[0] != "?")
        modified = sum(1 for l in porcelain.splitlines() if l[1] != " " and l[0] != "?")
        untracked = sum(1 for l in porcelain.splitlines() if l.startswith("?"))
        dirty = [s for s in (
            f"{staged} staged" if staged else "",
            f"{modified} modified" if modified else "",
            f"{untracked} untracked" if untracked else "",
        ) if s]
        lines.append(f"- Status: {', '.join(dirty) if dirty else 'clean'}")
    else:
        lines.append("- Status: clean")

    # Recent commits
    recent = _git("log", "-3", "--pretty=%h %s")
    if recent:
        lines.append("- Recent commits:")
        for c in recent.splitlines():
            lines.append(f"    {c}")

    return "\n".join(lines)


# ═══════════════════════════════════════════════════════════════════════════
# Snapshot helpers (USER.md, MEMORY.md)
# ═══════════════════════════════════════════════════════════════════════════

def _read_snapshot(path: str | Path, max_chars: int) -> str | None:
    """Read a file, truncate to max_chars, return None if file doesn't exist."""
    p = Path(path).expanduser()
    if not p.exists():
        return None
    try:
        content = p.read_text(encoding="utf-8").strip()
        if not content:
            return None
        if len(content) > max_chars:
            # Truncate at nearest paragraph/section break
            cut = content[:max_chars]
            last_break = max(cut.rfind("\n\n"), cut.rfind("\n## "), cut.rfind("\n# "))
            if last_break > max_chars // 2:
                content = content[:last_break].strip()
            else:
                content = cut
        return content
    except (OSError, UnicodeDecodeError):
        return None


def _resolve_user_md_path(user_md_path: str | None) -> str:
    """Resolve USER.md path. Defaults to ~/.aman/USER.md."""
    if user_md_path:
        return user_md_path
    return os.path.expanduser("~/.aman/USER.md")


def _resolve_memory_md_path(memory_md_path: str | None) -> str:
    """Resolve MEMORY.md path. Defaults to ~/.aman/memory/MEMORY.md."""
    if memory_md_path:
        return memory_md_path
    return os.path.expanduser("~/.aman/memory/MEMORY.md")


# ═══════════════════════════════════════════════════════════════════════════
# Timestamp helpers
# ═══════════════════════════════════════════════════════════════════════════

def _build_timestamp(date_str: str, model: str | None, provider: str | None) -> str:
    """Build a byte-stable session timestamp line (Hermes §23).

    Format: "Conversation started on <date>.  Model: <model>.  Provider: <provider>."
    Omits model/provider if not provided.
    """
    parts = [f"Conversation started on {date_str}."]
    if model:
        parts.append(f"  Model: {model}.")
    if provider:
        parts.append(f"  Provider: {provider}.")
    return "".join(parts)


# ═══════════════════════════════════════════════════════════════════════════
# Main entry point — three-layer system prompt assembly
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
    # ── New Hermes-style params ──
    user_md_path: str | None = None,
    memory_md_path: str | None = None,
    claude_md_content: str | None = None,
    cwd: str | None = None,
    platform: str = "cli",
    model: str | None = None,
    provider: str | None = None,
) -> str:
    """Build the complete system prompt — three-layer Hermes-style assembly.

    Layers (stable → context → volatile):
      STABLE:   soul, task discipline, tool guidance, platform, skills
      CONTEXT:  CLAUDE.md / project files
      VOLATILE: USER.md, MEMORY.md, timestamp
      TOOLS:    available tools, file ops, format, web reminders
      MEMORY:   per-turn retrieved memories

    This is the single function bridge.py calls — no more multi-step
    assembly on the Rust side.
    """
    if date_str is None:
        date_str = current_date_string()

    parts: list[str] = []

    # ── STABLE layer ─────────────────────────────────────────────────
    # 1. Soul identity
    soul = parse_soul(soul_content)
    parts.append(soul.to_system_prompt())

    # 2. Task completion discipline (only when tools are available)
    tools = tools or []
    if tools:
        parts.append(TASK_COMPLETION_DISCIPLINE)

    # 3. Parallel tool call guidance (only when tools are available)
    if tools:
        parts.append(PARALLEL_TOOL_GUIDANCE)

    # 4. Tool-use enforcement (only when tools are available)
    if tools:
        parts.append(TOOL_USE_ENFORCEMENT)

    # 5. Platform hint
    platform = platform.lower()
    if platform in ("desktop", "tauri", "gui"):
        parts.append(PLATFORM_HINT_DESKTOP)
    else:
        parts.append(PLATFORM_HINT_CLI)

    # 6. Environment hints (OS, home, cwd, shell)
    try:
        env_hints = build_environment_hints(cwd)
        if env_hints:
            parts.append(f"{ENVIRONMENT_HINTS_HEADER}\n{env_hints}")
    except Exception:
        pass  # never let environment probe block prompt build

    # 7. Python toolchain probe (only emits when non-clean)
    try:
        py_probe = build_python_probe_line()
        if py_probe:
            parts.append(py_probe)
    except Exception:
        pass

    # 8. Skills index
    skills = skills or []
    skills_section = build_skills_section(skills)
    if skills_section:
        parts.append(skills_section)

    # ── CONTEXT layer ────────────────────────────────────────────────
    # 9. CLAUDE.md / project context
    if claude_md_content and claude_md_content.strip():
        parts.append(f"{CLAUDE_MD_HEADER}{claude_md_content.strip()}")

    # 10. Coding posture (cwd + git snapshot, only in a git repo)
    try:
        posture = build_coding_posture(cwd)
        if posture:
            parts.append(f"{CODING_POSTURE_HEADER}\n{posture}")
    except Exception:
        pass

    # ── VOLATILE layer ───────────────────────────────────────────────
    # 8. USER.md snapshot (frozen at session start, ~/.aman/USER.md)
    user_md = _read_snapshot(_resolve_user_md_path(user_md_path), USER_MD_MAX_CHARS)
    if user_md:
        parts.append(f"{USER_MD_HEADER}{user_md}")

    # 9. MEMORY.md snapshot (frozen at session start, ~/.aman/memory/MEMORY.md)
    mem_md = _read_snapshot(_resolve_memory_md_path(memory_md_path), MEMORY_MD_MAX_CHARS)
    if mem_md:
        parts.append(f"{MEMORY_MD_HEADER}{mem_md}")

    # 10. Timestamp (byte-stable for the session)
    parts.append(_build_timestamp(date_str, model, provider))

    # ── TOOLS section ────────────────────────────────────────────────
    if tools:
        parts.append(build_tool_list(tools))
        if include_file_ops:
            parts.append(FILE_OPS_DOCS)
        parts.append(TOOL_CALL_FORMAT)
        if include_web_reminder:
            parts.append(WEB_SEARCH_REMINDER)
        if include_web_fetch_reminder:
            parts.append(WEB_FETCH_REMINDER)

    # ── Retrieved memories (per-turn, appended by Rust) ──────────────
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
