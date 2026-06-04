#!/usr/bin/env python3
"""JSONL-based per-work-item context persistence.

Each work item's execution history (thoughts, tool calls, responses,
human directions, step outputs) is stored as an append-only JSONL file:

    ~/.aman/team/projects/{project_key}/works/{work_id}.jsonl

JSONL is a natural fit: Agent execution is an event stream, and every
write is an append. The file can be tailed directly into an LLM system
prompt — no SQL queries, no serialization overhead.

Design decisions (per user feedback):
- No separate .meta.json file — metadata lives in the project DB.
- No size management — work items have natural boundaries (like a chat
  session), they don't grow unbounded.
- No separate team database — the existing data.db covers structured data.
"""

import json
import os
from datetime import datetime, timezone
from pathlib import Path
from typing import Any, Dict, List, Optional


# ---------------------------------------------------------------------------
# Path helpers
# ---------------------------------------------------------------------------

TEAM_DIR = os.path.expanduser("~/.aman/team")
PROJECTS_DIR = os.path.join(TEAM_DIR, "projects")


def _project_dir(project_key: str) -> str:
    return os.path.join(PROJECTS_DIR, project_key)


def _works_dir(project_key: str) -> str:
    return os.path.join(_project_dir(project_key), "works")


def _context_path(project_key: str, work_id: str) -> str:
    return os.path.join(_works_dir(project_key), f"{work_id}.jsonl")


# ---------------------------------------------------------------------------
# Event type builders
# ---------------------------------------------------------------------------

def _now_ts() -> str:
    return datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%S.%f")[:-3] + "Z"


def make_event(event_type: str, **kwargs) -> Dict[str, Any]:
    """Build a context event dict with a timestamp."""
    event = {"type": event_type, "ts": _now_ts()}
    event.update(kwargs)
    return event


# ---------------------------------------------------------------------------
# Core operations
# ---------------------------------------------------------------------------


def append_event(project_key: str, work_id: str, event: Dict[str, Any]) -> None:
    """Append a single event to a work item's JSONL file.

    Creates the works directory and file if they don't exist.
    Each event is written as one newline-delimited JSON record.
    """
    path = _context_path(project_key, work_id)
    os.makedirs(os.path.dirname(path), exist_ok=True)

    line = json.dumps(event, ensure_ascii=False)
    with open(path, "a") as f:
        f.write(line + "\n")


def read_context(
    project_key: str, work_id: str, max_lines: int = 200
) -> List[Dict[str, Any]]:
    """Read the tail of a work item's context file.

    Returns up to *max_lines* most recent events as deserialized dicts.
    Returns an empty list if the file doesn't exist yet.
    """
    path = _context_path(project_key, work_id)
    if not os.path.isfile(path):
        return []

    lines = _tail_file(path, max_lines)
    events = []
    for line in lines:
        line = line.strip()
        if not line:
            continue
        try:
            events.append(json.loads(line))
        except json.JSONDecodeError:
            continue
    return events


def read_context_raw(
    project_key: str, work_id: str, max_lines: int = 200
) -> str:
    """Read the context file as raw text (for direct LLM prompt injection).

    Returns up to *max_lines* lines from the tail of the file.
    """
    path = _context_path(project_key, work_id)
    if not os.path.isfile(path):
        return ""

    lines = _tail_file(path, max_lines)
    return "\n".join(lines)


def context_len(project_key: str, work_id: str) -> int:
    """Count the number of events in a work item's context file."""
    path = _context_path(project_key, work_id)
    if not os.path.isfile(path):
        return 0
    with open(path, "r") as f:
        return sum(1 for _ in f)


def delete_context(project_key: str, work_id: str) -> None:
    """Delete a work item's context file (cleanup after archival/deletion)."""
    path = _context_path(project_key, work_id)
    if os.path.isfile(path):
        os.remove(path)


def context_exists(project_key: str, work_id: str) -> bool:
    """Check if a context file exists for a work item."""
    return os.path.isfile(_context_path(project_key, work_id))


def context_path(project_key: str, work_id: str) -> str:
    """Get the full path to a work item context file."""
    return _context_path(project_key, work_id)


def list_context_files(project_key: str) -> List[str]:
    """List all work item IDs that have context files."""
    d = _works_dir(project_key)
    if not os.path.isdir(d):
        return []
    ids = []
    for name in sorted(os.listdir(d)):
        if name.endswith(".jsonl"):
            ids.append(name[:-6])  # strip .jsonl
    return ids


# ---------------------------------------------------------------------------
# Context building for agent prompts
# ---------------------------------------------------------------------------


def build_work_context_for_agent(
    project_key: str, work_id: str, max_lines: int = 200
) -> str:
    """Build a context string for injecting into an agent's system prompt.

    Reads the tail of the JSONL file and formats it as a narrative block
    that the agent can understand. Returns an empty string if no context.
    """
    events = read_context(project_key, work_id, max_lines)
    if not events:
        return ""

    lines = ["[Work Item History — read-only input, NOT a completion signal]", ""]
    for ev in events:
        t = ev.get("type", "unknown")
        if t == "created":
            lines.append(
                f"  Created: {ev.get('title','')} by {ev.get('creator','')} → {ev.get('stage','')}"
            )
        elif t == "assigned":
            lines.append(
                f"  Assigned to {ev.get('agent_id','')} (stage: {ev.get('stage','')}, strategy: {ev.get('strategy','')})"
            )
        elif t == "thought":
            lines.append(f"  [Thought] {ev.get('content','')}")
        elif t == "tool_call":
            inp = json.dumps(ev.get("input", {}), ensure_ascii=False)
            lines.append(f"  [Tool: {ev.get('tool','')}] input={inp}")
            out = ev.get("output", "")
            if out:
                lines.append(f"  [Tool Output] {out[:300]}")
        elif t == "response":
            lines.append(f"  [Response] {ev.get('content','')}")
        elif t == "human_direction":
            lines.append(
                f"  [Human: {ev.get('human_id','')}] {ev.get('content','')}"
            )
        elif t == "step_complete":
            icon = "✓" if ev.get("success") else "✗"
            lines.append(
                f"  {icon} Step {ev.get('step_index',0)+1}/{ev.get('total_steps',0)}: {ev.get('summary','')}"
            )
        elif t == "stage_changed":
            lines.append(
                f"  Stage: {ev.get('from','')} → {ev.get('to','')} ({ev.get('reason','')})"
            )
        elif t == "safety_triggered":
            lines.append(
                f"  ⚠ Safety Gate: {ev.get('reason','')} — action: {ev.get('action','')}"
            )
        elif t == "safety_resolved":
            lines.append(
                f"  Safety resolved: {ev.get('decision','')} by {ev.get('decided_by','')}"
            )
        elif t == "completed":
            next_s = ev.get("next_stage", "") or "terminal"
            lines.append(
                f"  ✅ Completed (confidence: {ev.get('confidence',0):.0%}) → {next_s}"
            )
        elif t == "failed":
            lines.append(
                f"  ❌ Failed: {ev.get('error','')} (retryable: {ev.get('retryable',False)})"
            )
        elif t == "context_update":
            lines.append(f"  Context: {ev.get('key','')} = {ev.get('value','')}")
        else:
            lines.append(f"  [{t}] {json.dumps(ev, ensure_ascii=False)[:200]}")

    lines.append("")
    lines.append("[End of history — work is NOT yet complete, continue processing]")
    return "\n".join(lines)


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


def _tail_file(path: str, n: int) -> List[str]:
    """Read the last N lines from a file efficiently."""
    with open(path, "r") as f:
        # Simple read-all approach — fine for work item context files
        # which have natural boundaries.
        lines = f.readlines()
        if len(lines) <= n:
            return [line.rstrip("\n") for line in lines]
        return [line.rstrip("\n") for line in lines[-n:]]


# ---------------------------------------------------------------------------
# Tests (run with: python3 -m pytest context.py)
# ---------------------------------------------------------------------------


if __name__ == "__main__":
    import tempfile
    import sys

    # Override paths for testing
    test_dir = tempfile.mkdtemp(prefix="team-context-test-")
    TEAM_DIR = test_dir
    PROJECTS_DIR = os.path.join(TEAM_DIR, "projects")

    proj = "test-proj"
    wid = "work-001"

    # --- append + read ---
    append_event(proj, wid, make_event("created", title="Fix bug", description="OOM fix",
                                        creator="jerin", stage="backlog"))
    append_event(proj, wid, make_event("assigned", agent_id="coder", stage="wip", strategy="best_match"))
    append_event(proj, wid, make_event("thought", content="Analyzing backpressure.rs..."))
    append_event(proj, wid, make_event("tool_call", tool="read_file",
                                        input={"path": "src/event-bus/backpressure.rs"},
                                        output="pub struct Backpressure { ... }"))
    append_event(proj, wid, make_event("response", content="Found threshold issue"))
    append_event(proj, wid, make_event("completed", confidence=0.92,
                                        summary="Fixed by lowering threshold", next_stage="review"))

    assert context_len(proj, wid) == 6, f"expected 6 events, got {context_len(proj, wid)}"

    # --- read context ---
    events = read_context(proj, wid, max_lines=3)
    assert len(events) == 3, f"expected 3 tail events, got {len(events)}"
    assert events[-1]["type"] == "completed"

    # --- read raw ---
    raw = read_context_raw(proj, wid, max_lines=2)
    assert raw.count("\n") == 1  # 2 lines = 1 newline

    # --- build agent context ---
    ctx = build_work_context_for_agent(proj, wid, max_lines=10)
    assert "Fix bug" in ctx
    assert "coder" in ctx
    assert "0.92" in ctx or "92%" in ctx

    # --- list + delete ---
    assert wid in list_context_files(proj)
    assert context_exists(proj, wid)
    delete_context(proj, wid)
    assert not context_exists(proj, wid)
    assert list_context_files(proj) == []

    # cleanup
    import shutil
    shutil.rmtree(test_dir)
    print("All tests passed.", file=sys.stderr)
