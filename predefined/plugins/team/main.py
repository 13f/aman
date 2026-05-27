#!/usr/bin/env python3
"""Team Plugin — Kanban scheduler using aman's Work system.

Protocol: Bidirectional JSON-RPC 2.0 over stdin/stdout (newline-delimited).
All logging goes to stderr to avoid corrupting the JSON-RPC stream on stdout.
"""

import json
import sys
import os
import traceback
from typing import Any, Callable, Dict, Optional

# ---------------------------------------------------------------------------
# JSON-RPC Bridge
# ---------------------------------------------------------------------------

_PENDING: Dict[int, "PendingRequest"] = {}


class _PendingRequest:
    """Tracks a plugin→server JSON-RPC request awaiting a response."""

    __slots__ = ("method", "resolve")

    def __init__(self, method: str, resolve: Callable[[Any], None]):
        self.method = method
        self.resolve = resolve


_next_id = 1


def _make_id() -> int:
    global _next_id
    rid = _next_id
    _next_id += 1
    return rid


def _log(msg: str) -> None:
    print(f"[team-plugin] {msg}", file=sys.stderr, flush=True)


# ── Sending (Plugin → Server) ──────────────────────────────────────────


def send_response(req_id: int, result: Any) -> None:
    """Send a JSON-RPC success response."""
    payload = json.dumps({"jsonrpc": "2.0", "id": req_id, "result": result})
    _write_line(payload)


def send_error(req_id: int, code: int, message: str) -> None:
    """Send a JSON-RPC error response."""
    payload = json.dumps(
        {"jsonrpc": "2.0", "id": req_id, "error": {"code": code, "message": message}}
    )
    _write_line(payload)


def send_request(method: str, params: Any) -> Any:
    """Send a JSON-RPC request to the server and block waiting for response."""
    rid = _make_id()
    payload = json.dumps({"jsonrpc": "2.0", "id": rid, "method": method, "params": params})
    result_holder: list = []

    def resolve(val: Any) -> None:
        result_holder.append(val)

    _PENDING[rid] = _PendingRequest(method, resolve)
    _write_line(payload)

    # Wait for response (blocking — we're single-threaded)
    _process_until_response(rid)

    if not result_holder:
        raise RuntimeError(f"No response for {method}")
    return result_holder[0]


def send_notification(method: str, params: Any) -> None:
    """Send a JSON-RPC notification (no response expected)."""
    payload = json.dumps({"jsonrpc": "2.0", "method": method, "params": params})
    _write_line(payload)


def _write_line(data: str) -> None:
    """Write a single line to stdout and flush."""
    try:
        sys.stdout.write(data + "\n")
        sys.stdout.flush()
    except BrokenPipeError:
        # Server closed the pipe (shutting down), stop writing
        pass


# ── Receiving (Server → Plugin handling) ───────────────────────────────


def _process_until_response(rid: int) -> None:
    """Read stdin lines until the response for `rid` arrives."""
    while rid in _PENDING:
        line = sys.stdin.readline()
        if not line:
            break
        _dispatch(line.rstrip("\n"))


def _dispatch(line: str) -> None:
    """Parse and dispatch a single JSON-RPC line."""
    if not line.strip():
        return
    try:
        msg = json.loads(line)
    except json.JSONDecodeError as e:
        _log(f"Invalid JSON from server: {e}")
        return

    msg_id = msg.get("id")

    if msg_id is not None and "method" not in msg:
        # This is a *response* to one of our requests
        rid = int(msg_id)
        pending = _PENDING.pop(rid, None)
        if pending:
            result = msg.get("result")
            error = msg.get("error")
            if error:
                _log(f"Server error for {pending.method}: {error}")
                pending.resolve({"__error__": error})
            else:
                pending.resolve(result)
        return

    method = msg.get("method")
    if method is None:
        return

    params = msg.get("params")
    req_id = int(msg_id) if msg_id is not None else None
    _handle_incoming_request(method, params, req_id)


def _handle_incoming_request(method: str, params: Any, req_id: Optional[int]) -> None:
    """Dispatch a server→plugin request to the registered handler."""
    handler = _HANDLERS.get(method)
    if handler is None:
        _log(f"Unknown method: {method}")
        if req_id is not None:
            send_error(req_id, -32601, f"Method not found: {method}")
        return

    try:
        result = handler(params)
        if req_id is not None:
            send_response(req_id, result)
    except Exception as e:
        _log(f"Handler error for {method}: {traceback.format_exc()}")
        if req_id is not None:
            send_error(req_id, -32000, str(e))


_HANDLERS: Dict[str, Callable[[Any], Any]] = {}


def on(method: str):
    """Decorator to register a handler for a server→plugin method."""

    def decorator(fn):
        _HANDLERS[method] = fn
        return fn

    return decorator


# ---------------------------------------------------------------------------
# Team Plugin Logic
# ---------------------------------------------------------------------------
#
# Data layout (per user spec):
#   ~/.aman/team/
#     config.yaml                      — team-level config (name, creator, …)
#     projects/
#       {project_key}/
#         config.yaml                  — kanban stages, safety gates, context, …
#         data.db                      — SQLite: tasks, safety_log, context cache

import re
import sqlite3
import time
from pathlib import Path
from string import Template
from collections import defaultdict

TEAM_DIR = os.path.expanduser("~/.aman/team")
PROJECTS_DIR = os.path.join(TEAM_DIR, "projects")

# ── Global state ───────────────────────────────────────────────────────

_team_config: Optional[Dict[str, Any]] = None       # parsed config.yaml
_projects: Dict[str, Dict[str, Any]] = {}            # project_key → {"config": …, "db": …}


# ── Path helpers ───────────────────────────────────────────────────────

def _project_dir(project_key: str) -> str:
    return os.path.join(PROJECTS_DIR, project_key)


def _project_db_path(project_key: str) -> str:
    return os.path.join(_project_dir(project_key), "data.db")


def _project_config_path(project_key: str) -> str:
    return os.path.join(_project_dir(project_key), "config.yaml")


def _team_config_path() -> str:
    return os.path.join(TEAM_DIR, "config.yaml")


# ---------------------------------------------------------------------------
# Config Loading
# ---------------------------------------------------------------------------


def load_team_config() -> Optional[Dict[str, Any]]:
    """Load and validate ~/.aman/team/config.yaml."""
    path = _team_config_path()
    if not os.path.isfile(path):
        _log(f"team config not found at {path}")
        return None

    try:
        import yaml
        with open(path, "r") as f:
            raw = yaml.safe_load(f) or {}
    except Exception as e:
        _log(f"failed to parse team config: {e}")
        return None

    return _validate_team_config(raw)


def _validate_team_config(raw: dict) -> Optional[Dict[str, Any]]:
    """Validate and normalize the team-level config."""
    meta = raw.get("team", {})
    if not meta:
        _log("team config: missing 'team' key")
        return None

    return {
        "team_name": meta.get("name", "Team"),
        "description": meta.get("description", ""),
        "creator": meta.get("creator", ""),
    }


def load_project_config(project_key: str) -> Optional[Dict[str, Any]]:
    """Load and validate a project's config.yaml."""
    path = _project_config_path(project_key)
    if not os.path.isfile(path):
        return None

    try:
        import yaml
        with open(path, "r") as f:
            raw = yaml.safe_load(f) or {}
    except Exception:
        return None

    return _validate_project_config(project_key, raw)


def _validate_project_config(project_key: str, raw: dict) -> Optional[Dict[str, Any]]:
    """Validate and normalize a project config dict."""
    meta = raw.get("project", {})
    stages = raw.get("stages", [])
    safety = raw.get("safety_gates", {})
    initial_stage = raw.get("initial_stage", "")
    context_files = raw.get("context_files", [])
    work_dir = raw.get("work_dir", os.getcwd())

    stage_ids = {s["id"] for s in stages if isinstance(s, dict) and "id" in s}

    # Validate stage allowed_next references
    for s in stages:
        for n in s.get("allowed_next", []):
            if n not in stage_ids:
                _log(f"{project_key}: stage {s.get('id', '?')} references unknown next '{n}'")

    if initial_stage and initial_stage not in stage_ids:
        _log(f"{project_key}: initial_stage '{initial_stage}' not found in stages")
        initial_stage = ""

    return {
        "project_key": project_key,
        "project_name": meta.get("name", project_key),
        "description": meta.get("description", ""),
        "stages": stages,
        "safety_gates": {
            "dangerous_actions": safety.get("dangerous_actions", []),
            "min_confidence": safety.get("min_confidence", 0.7),
            "max_autonomous_actions_without_human": safety.get(
                "max_autonomous_actions_without_human", 20
            ),
        },
        "initial_stage": initial_stage,
        "context_files": context_files,
        "work_dir": work_dir,
    }


def discover_projects() -> Dict[str, Dict[str, Any]]:
    """Scan ~/.aman/team/projects/ for project configs and load them."""
    projects = {}
    if not os.path.isdir(PROJECTS_DIR):
        return projects

    for entry in sorted(os.listdir(PROJECTS_DIR)):
        proj_dir = os.path.join(PROJECTS_DIR, entry)
        if not os.path.isdir(proj_dir):
            continue
        if not os.path.isfile(os.path.join(proj_dir, "config.yaml")):
            continue
        config = load_project_config(entry)
        if config:
            projects[entry] = config

    return projects


# ---------------------------------------------------------------------------
# SQLite Store (per project)
# ---------------------------------------------------------------------------


def init_db(project_key: str) -> sqlite3.Connection:
    """Initialize the project database, creating tables if needed."""
    os.makedirs(_project_dir(project_key), exist_ok=True)
    db_path = _project_db_path(project_key)
    db = sqlite3.connect(db_path)
    db.row_factory = sqlite3.Row
    db.execute("PRAGMA journal_mode=WAL")
    db.execute("PRAGMA foreign_keys=ON")

    db.execute(
        """CREATE TABLE IF NOT EXISTS safety_log (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            work_item_id TEXT NOT NULL,
            agent_id TEXT NOT NULL,
            action TEXT NOT NULL DEFAULT '',
            reason TEXT NOT NULL DEFAULT 'dangerous_action',
            human_decision TEXT,
            decided_by TEXT,
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            resolved_at TEXT
        )"""
    )

    db.execute(
        """CREATE TABLE IF NOT EXISTS context (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            title TEXT NOT NULL DEFAULT '',
            file_path TEXT NOT NULL UNIQUE,
            content TEXT NOT NULL DEFAULT '',
            category TEXT NOT NULL DEFAULT 'general',
            updated_at TEXT NOT NULL DEFAULT (datetime('now')),
            indexed_at TEXT NOT NULL DEFAULT (datetime('now'))
        )"""
    )

    db.execute(
        """CREATE TABLE IF NOT EXISTS tasks (
            id TEXT PRIMARY KEY,
            title TEXT NOT NULL DEFAULT '',
            description TEXT NOT NULL DEFAULT '',
            source_type TEXT NOT NULL DEFAULT 'manual',
            source_ref TEXT NOT NULL DEFAULT '',
            creator TEXT NOT NULL DEFAULT '',
            current_stage TEXT NOT NULL DEFAULT '',
            priority TEXT NOT NULL DEFAULT 'normal',
            tags TEXT NOT NULL DEFAULT '[]',
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            updated_at TEXT NOT NULL DEFAULT (datetime('now'))
        )"""
    )

    db.execute(
        """CREATE TABLE IF NOT EXISTS stage_history (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            task_id TEXT NOT NULL,
            stage TEXT NOT NULL,
            entered_at TEXT NOT NULL DEFAULT (datetime('now')),
            assignee TEXT NOT NULL DEFAULT '',
            completed_at TEXT,
            confidence REAL,
            FOREIGN KEY (task_id) REFERENCES tasks(id)
        )"""
    )

    db.commit()
    return db


def get_db(project_key: str) -> sqlite3.Connection:
    """Get or create the project database."""
    proj = _projects.get(project_key)
    if proj and proj.get("db"):
        return proj["db"]
    db = init_db(project_key)
    if proj:
        proj["db"] = db
    return db


# ── Safety Log ──────────────────────────────────────────────────────────

def insert_safety_log(project_key: str, work_item_id: str, agent_id: str,
                      action: str, reason: str) -> int:
    db = get_db(project_key)
    cur = db.execute(
        "INSERT INTO safety_log (work_item_id, agent_id, action, reason) VALUES (?, ?, ?, ?)",
        (work_item_id, agent_id, action, reason),
    )
    db.commit()
    return cur.lastrowid


def get_pending_safety_logs(project_key: str) -> list:
    db = get_db(project_key)
    rows = db.execute(
        "SELECT * FROM safety_log WHERE human_decision IS NULL ORDER BY created_at DESC"
    ).fetchall()
    return [dict(r) for r in rows]


def resolve_safety_log(project_key: str, log_id: int, decision: str, decided_by: str) -> bool:
    db = get_db(project_key)
    db.execute(
        "UPDATE safety_log SET human_decision=?, decided_by=?, resolved_at=datetime('now') WHERE id=?",
        (decision, decided_by, log_id),
    )
    db.commit()
    return True


# ── Tasks ───────────────────────────────────────────────────────────────

def insert_task(project_key: str, task: dict) -> dict:
    db = get_db(project_key)
    db.execute(
        """INSERT OR REPLACE INTO tasks (id, title, description, source_type, source_ref,
           creator, current_stage, priority, tags, updated_at)
           VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, datetime('now'))""",
        (task["id"], task["title"], task.get("description", ""),
         task.get("source_type", "manual"), task.get("source_ref", ""),
         task.get("creator", ""), task.get("current_stage", ""),
         task.get("priority", "normal"), task.get("tags", "[]")),
    )
    # Record initial stage
    db.execute(
        "INSERT INTO stage_history (task_id, stage) VALUES (?, ?)",
        (task["id"], task.get("current_stage", "")),
    )
    db.commit()
    row = db.execute("SELECT * FROM tasks WHERE id=?", (task["id"],)).fetchone()
    return dict(row) if row else task


def get_task(project_key: str, task_id: str) -> Optional[dict]:
    db = get_db(project_key)
    row = db.execute("SELECT * FROM tasks WHERE id=?", (task_id,)).fetchone()
    return dict(row) if row else None


def list_tasks(project_key: str, stage: Optional[str] = None) -> list:
    db = get_db(project_key)
    query = """SELECT t.*, COALESCE(
                   (SELECT sh.assignee FROM stage_history sh
                    WHERE sh.task_id = t.id AND sh.completed_at IS NULL
                    ORDER BY sh.id DESC LIMIT 1), '') as assignee
               FROM tasks t"""
    if stage:
        rows = db.execute(
            query + " WHERE t.current_stage=? ORDER BY t.created_at DESC", (stage,)
        ).fetchall()
    else:
        rows = db.execute(query + " ORDER BY t.created_at DESC").fetchall()
    return [dict(r) for r in rows]


def update_task_stage(project_key: str, task_id: str, stage: str, assignee: str = "") -> None:
    db = get_db(project_key)
    db.execute(
        "UPDATE tasks SET current_stage=?, updated_at=datetime('now') WHERE id=?",
        (stage, task_id),
    )
    db.execute(
        "INSERT INTO stage_history (task_id, stage, assignee) VALUES (?, ?, ?)",
        (task_id, stage, assignee),
    )
    db.commit()


def complete_task_stage(project_key: str, task_id: str, confidence: float) -> None:
    db = get_db(project_key)
    db.execute(
        """UPDATE stage_history SET completed_at=datetime('now'), confidence=?
            WHERE task_id=? AND completed_at IS NULL
            ORDER BY id DESC LIMIT 1""",
        (confidence, task_id),
    )
    db.commit()


# ── Context ─────────────────────────────────────────────────────────────

def get_contexts(project_key: str) -> list:
    db = get_db(project_key)
    rows = db.execute("SELECT * FROM context ORDER BY title").fetchall()
    return [dict(r) for r in rows]


def get_context(project_key: str, ctx_id: int) -> Optional[dict]:
    db = get_db(project_key)
    row = db.execute("SELECT * FROM context WHERE id=?", (ctx_id,)).fetchone()
    return dict(row) if row else None


def index_context_file(project_key: str, file_path: str, work_dir: str) -> Optional[dict]:
    full_path = os.path.join(work_dir, file_path)
    if not os.path.isfile(full_path):
        return None
    try:
        mtime = os.path.getmtime(full_path)
        with open(full_path, "r") as f:
            content = f.read()
    except Exception:
        return None

    title = os.path.splitext(os.path.basename(file_path))[0]
    ext = os.path.splitext(file_path)[1]
    if ext in (".md",):
        category = "documentation"
    elif ext in (".rs", ".py", ".js", ".ts", ".go"):
        category = "code"
    else:
        category = "general"

    db = get_db(project_key)
    db.execute(
        """INSERT OR REPLACE INTO context (title, file_path, content, category, updated_at, indexed_at)
           VALUES (?, ?, ?, ?, datetime(?, 'unixepoch'), datetime('now'))""",
        (title, file_path, content, category, int(mtime)),
    )
    db.commit()
    row = db.execute("SELECT * FROM context WHERE file_path=?", (file_path,)).fetchone()
    return dict(row) if row else None


# ---------------------------------------------------------------------------
# Safety Gate
# ---------------------------------------------------------------------------

_AUTONOMOUS_COUNTER: Dict[str, int] = defaultdict(int)


def _check_dangerous_action(project_config: dict, action: str) -> Optional[dict]:
    dangerous = project_config.get("safety_gates", {}).get("dangerous_actions", [])
    for entry in dangerous:
        pattern = entry.get("pattern", "") if isinstance(entry, dict) else str(entry)
        if pattern and re.search(pattern, action, re.IGNORECASE):
            return {"reason": "dangerous_action", "pattern": pattern, "action": action}
    return None


def _check_confidence(project_config: dict, confidence: float) -> Optional[dict]:
    min_conf = project_config.get("safety_gates", {}).get("min_confidence", 0.7)
    if confidence < min_conf:
        return {"reason": "low_confidence", "confidence": confidence, "min_required": min_conf}
    return None


def _check_autonomous_limit(project_config: dict, agent_id: str) -> Optional[dict]:
    max_actions = project_config.get("safety_gates", {}).get("max_autonomous_actions_without_human", 20)
    count = _AUTONOMOUS_COUNTER[agent_id]
    if count >= max_actions:
        return {"reason": "autonomous_limit", "count": count, "max": max_actions}
    return None


def run_safety_checks(project_key: str, action: str, agent_id: str,
                      work_item_id: str, confidence: float) -> dict:
    """Run all safety checks. Returns {"allowed": true} or {"blocked": ..., "requires_human": true}."""
    project_config = _projects.get(project_key, {}).get("config", {})

    danger = _check_dangerous_action(project_config, action)
    if danger:
        insert_safety_log(project_key, work_item_id, agent_id, action, danger["reason"])
        send_notification("aman.emit_event", {
            "event_type": "team:safety.gate_triggered",
            "payload": {
                "project_key": project_key,
                "work_item_id": work_item_id,
                "agent_id": agent_id,
                "action": action,
                "reason": danger["reason"],
            },
        })
        return {"allowed": False, "blocked": True, "requires_human": True,
                "reason": f"Dangerous action: {action}"}

    conf_check = _check_confidence(project_config, confidence)
    if conf_check:
        insert_safety_log(project_key, work_item_id, agent_id, action, conf_check["reason"])
        return {"allowed": False, "blocked": True, "requires_human": True,
                "reason": f"Low confidence: {confidence} < {conf_check['min_required']}"}

    auto_check = _check_autonomous_limit(project_config, agent_id)
    if auto_check:
        return {"allowed": False, "blocked": True, "requires_human": True,
                "reason": f"Autonomous action limit exceeded: {auto_check['count']}/{auto_check['max']}"}

    _AUTONOMOUS_COUNTER[agent_id] += 1
    return {"allowed": True}


def reset_autonomous_counter(agent_id: str) -> None:
    _AUTONOMOUS_COUNTER[agent_id] = 0


# ---------------------------------------------------------------------------
# Scheduler
# ---------------------------------------------------------------------------


def _capability_match(agent_caps: list, required_caps: list) -> int:
    agent_set = set(agent_caps)
    return sum(1 for c in required_caps if c in agent_set)


def _agent_id(agent: dict) -> str:
    return agent.get("agent_id", agent.get("id", ""))


def _agent_caps(agent: dict) -> list:
    return agent.get("capabilities", [])


def dispatch(project_key: str, task: dict, stage_id: str) -> Optional[Dict[str, Any]]:
    """Find the best agent from the registry and push the work item."""
    proj = _projects.get(project_key, {}).get("config", {})
    stages = {s["id"]: s for s in proj.get("stages", [])}
    stage = stages.get(stage_id)
    if not stage:
        return None

    policy = stage.get("assignment_policy")
    if not policy or not policy.get("auto_assign"):
        return None

    required_caps = policy.get("required_capabilities", [])
    dispatch_strategy = policy.get("dispatch_strategy", "best_match")
    default_queue_max = 5

    agents_result = send_request("aman.get_agents", {})
    all_agents = _unwrap_result(agents_result) or []

    candidates = []
    for agent in all_agents:
        agent_caps = _agent_caps(agent)
        # Skip agents that don't match required capabilities
        if required_caps and not any(c in required_caps for c in agent_caps):
            continue

        aid = _agent_id(agent)
        queue_len = _get_agent_queue_length(aid)
        if queue_len >= default_queue_max:
            continue

        candidates.append({
            "agent": agent,
            "caps": agent_caps,
            "match_score": _capability_match(agent_caps, required_caps) if required_caps else 0,
            "queue_len": queue_len,
        })

    if not candidates:
        return None

    if dispatch_strategy == "best_match":
        candidates.sort(key=lambda c: c["match_score"], reverse=True)
        target = candidates[0]
    elif dispatch_strategy == "least_loaded":
        candidates.sort(key=lambda c: c["queue_len"])
        target = candidates[0]
    elif dispatch_strategy == "random_idle":
        idle = [c for c in candidates if c["queue_len"] == 0]
        if idle:
            import random
            target = random.choice(idle)
        else:
            return None
    else:
        target = candidates[0]

    target_id = _agent_id(target["agent"])

    push_result = send_request("aman.push_work_item", {
        "agent_id": target_id,
        "title": task.get("title", ""),
        "description": task.get("description", ""),
        "priority": task.get("priority", "normal"),
        "context": task.get("context", {}),
    })

    if _unwrap_result(push_result):
        update_task_stage(project_key, task["id"], stage_id, target_id)
        send_notification("aman.emit_event", {
            "event_type": "team:work_item.assigned",
            "payload": {
                "project_key": project_key,
                "work_item_id": task.get("id", ""),
                "stage_id": stage_id,
                "agent_id": target_id,
            },
        })
        return target

    return None


def _get_agent_queue_length(agent_id: str) -> int:
    return 0


def _unwrap_result(response: Any) -> Any:
    if isinstance(response, dict) and "__error__" in response:
        return None
    return response


# ---------------------------------------------------------------------------
# Workflow Compiler (per project)
# ---------------------------------------------------------------------------


def compile_workflow(project_key: str) -> Optional[Dict[str, Any]]:
    """Compile project stages into a WorkflowDef and register with the engine."""
    proj = _projects.get(project_key, {}).get("config", {})
    if not proj:
        return None

    stages = proj.get("stages", [])
    states = []
    transitions = []

    for stage in stages:
        state_def = {
            "name": stage["id"],
            "display": stage.get("name", stage["id"]),
        }
        policy = stage.get("assignment_policy")
        if policy and policy.get("execution_timeout_minutes"):
            state_def["timeout"] = {
                "duration_secs": policy["execution_timeout_minutes"] * 60,
                "on_timeout": "team:work_item.failed",
            }
        states.append(state_def)

        for next_id in stage.get("allowed_next", []):
            transitions.append({
                "from": stage["id"],
                "event": f"team:stage.{stage['id']}.{next_id}",
                "to": next_id,
            })

    final_states = [s["id"] for s in stages if not s.get("allowed_next")]
    team_name = (_team_config or {}).get("team_name", "team")
    workflow_def = {
        "name": f"team-{team_name}-{project_key}",
        "states": states,
        "initial_state": proj.get("initial_stage", stages[0]["id"] if stages else ""),
        "final_states": final_states,
        "transitions": transitions,
    }

    result = send_request("aman.register_workflow", workflow_def)
    return _unwrap_result(result)


# ---------------------------------------------------------------------------
# Context Loader (per project)
# ---------------------------------------------------------------------------


def load_context_files(project_key: str) -> list:
    proj = _projects.get(project_key, {}).get("config", {})
    work_dir = proj.get("work_dir", os.getcwd())
    context_files = proj.get("context_files", [])

    results = []
    for file_path in context_files:
        ctx = index_context_file(project_key, file_path, work_dir)
        if ctx:
            results.append(ctx)
    return results


# ---------------------------------------------------------------------------
# API Handlers (called via aman.handle_route)
# ---------------------------------------------------------------------------


def handle_api(project_key: str, method: str, path: str, query: Optional[str],
               headers: dict, body: Optional[str]) -> dict:
    """Route HTTP requests to the appropriate project API handler."""
    prefix = f"/team/projects/{project_key}/"
    rel_path = path[len(prefix):] if path.startswith(prefix) else path

    try:
        body_json = json.loads(body) if body else {}
    except (json.JSONDecodeError, TypeError):
        body_json = {}

    # ── Tasks ──────────────────────────────────────────────────────
    if method == "GET" and rel_path == "tasks":
        return _handle_list_tasks(project_key, query)

    if method == "POST" and rel_path == "tasks/create":
        return _handle_create_task(project_key, body_json)

    if method == "GET" and rel_path.startswith("tasks/") and len(rel_path.split("/")) == 2:
        task_id = rel_path.split("/")[1]
        return _handle_get_task(project_key, task_id)

    if method == "POST" and rel_path.endswith("/assign"):
        parts = rel_path.split("/")
        if len(parts) == 3 and parts[0] == "tasks":
            return _handle_assign_task(project_key, parts[1], body_json)

    if method == "POST" and rel_path.endswith("/complete"):
        parts = rel_path.split("/")
        if len(parts) == 3 and parts[0] == "tasks":
            return _handle_complete_task(project_key, parts[1], body_json)

    # ── Safety ─────────────────────────────────────────────────────
    if method == "GET" and rel_path == "safety/pending":
        return _handle_pending_safety(project_key)

    if method == "POST" and rel_path.startswith("safety/") and rel_path.endswith("/resolve"):
        parts = rel_path.split("/")
        if len(parts) == 3:
            return _handle_resolve_safety(project_key, int(parts[1]), body_json)

    # ── Context ────────────────────────────────────────────────────
    if method == "GET" and rel_path == "context":
        return _handle_list_context(project_key)

    if method == "GET" and rel_path.startswith("context/") and len(rel_path.split("/")) == 2:
        ctx_id = int(rel_path.split("/")[1])
        return _handle_get_context(project_key, ctx_id)

    # ── Agents ─────────────────────────────────────────────────────
    if method == "GET" and rel_path == "agents":
        return _handle_list_project_agents(project_key)

    # ── Project info ───────────────────────────────────────────────
    if method == "GET" and rel_path == "":
        return _handle_get_project(project_key)

    return {"status": 404, "body": json.dumps({"error": "not found"})}


def _handle_get_project(project_key: str) -> dict:
    proj = _projects.get(project_key, {}).get("config", {})
    return {
        "status": 200,
        "headers": {"content-type": "application/json"},
        "body": json.dumps({
            "project_key": project_key,
            "project_name": proj.get("project_name", project_key),
            "description": proj.get("description", ""),
            "stages": proj.get("stages", []),
        }),
    }


def _handle_list_tasks(project_key: str, query: Optional[str]) -> dict:
    proj = _projects.get(project_key, {}).get("config", {})
    tasks = list_tasks(project_key)
    return {
        "status": 200,
        "headers": {"content-type": "application/json"},
        "body": json.dumps({
            "project_key": project_key,
            "project_name": proj.get("project_name", project_key),
            "stages": proj.get("stages", []),
            "tasks": tasks,
        }),
    }


def _handle_create_task(project_key: str, body: dict) -> dict:
    title = body.get("title", "")
    description = body.get("description", "")
    priority = body.get("priority", "normal")

    proj = _projects.get(project_key, {}).get("config", {})
    initial_stage = proj.get("initial_stage", "")

    task = {
        "id": f"task-{int(time.time() * 1000)}",
        "title": title,
        "description": description,
        "priority": priority,
        "current_stage": initial_stage,
        "source_type": "manual",
        "creator": body.get("creator", ""),
    }

    # Persist to the project database
    try:
        insert_task(project_key, task)
    except Exception as e:
        _log(f"Failed to insert task: {e}")

    send_notification("aman.emit_event", {
        "event_type": "team:work_item.created",
        "payload": {
            "project_key": project_key,
            "work_item_id": task["id"],
            "title": title,
            "description": description,
            "priority": priority,
            "stage_id": initial_stage,
        },
    })

    # Auto-dispatch if the initial stage has auto_assign
    stages = {s["id"]: s for s in proj.get("stages", [])}
    stage = stages.get(initial_stage, {})
    if stage.get("assignment_policy", {}).get("auto_assign"):
        dispatch(project_key, task, initial_stage)

    return {
        "status": 201,
        "headers": {"content-type": "application/json"},
        "body": json.dumps(task),
    }


def _handle_get_task(project_key: str, task_id: str) -> dict:
    task = get_task(project_key, task_id)
    if task:
        return {
            "status": 200,
            "headers": {"content-type": "application/json"},
            "body": json.dumps(task),
        }
    return {"status": 404, "body": json.dumps({"error": "task not found"})}


def _handle_assign_task(project_key: str, task_id: str, body: dict) -> dict:
    agent_id = body.get("agent_id", "")
    stage_id = body.get("stage_id", "")

    result = send_request("aman.push_work_item", {
        "agent_id": agent_id,
        "title": f"Manual assign: {task_id}",
        "description": body.get("reason", "Manual assignment"),
    })

    if _unwrap_result(result):
        update_task_stage(project_key, task_id, stage_id, agent_id)

    return {
        "status": 200,
        "headers": {"content-type": "application/json"},
        "body": json.dumps({"assigned": bool(_unwrap_result(result)), "agent_id": agent_id}),
    }


def _handle_complete_task(project_key: str, task_id: str, body: dict) -> dict:
    agent_id = body.get("agent_id", "")
    confidence = float(body.get("confidence", 1.0))
    action = body.get("action", "")

    safety = run_safety_checks(project_key, action, agent_id, task_id, confidence)
    if not safety.get("allowed"):
        return {
            "status": 403,
            "headers": {"content-type": "application/json"},
            "body": json.dumps(safety),
        }

    next_stage = body.get("next_stage", "")
    complete_task_stage(project_key, task_id, confidence)

    # Move to next stage if specified
    if next_stage:
        update_task_stage(project_key, task_id, next_stage, agent_id)

    send_notification("aman.emit_event", {
        "event_type": "team:work_item.completed",
        "payload": {
            "project_key": project_key,
            "work_item_id": task_id,
            "agent_id": agent_id,
            "confidence": confidence,
            "next_stage": next_stage,
        },
    })

    return {
        "status": 200,
        "headers": {"content-type": "application/json"},
        "body": json.dumps({"ok": True, "task_id": task_id}),
    }


def _handle_pending_safety(project_key: str) -> dict:
    logs = get_pending_safety_logs(project_key)
    return {
        "status": 200,
        "headers": {"content-type": "application/json"},
        "body": json.dumps(logs),
    }


def _handle_resolve_safety(project_key: str, log_id: int, body: dict) -> dict:
    decision = body.get("decision", "denied")
    decided_by = body.get("decided_by", "human")
    resolve_safety_log(project_key, log_id, decision, decided_by)

    send_notification("aman.emit_event", {
        "event_type": "team:safety.gate_resolved",
        "payload": {
            "project_key": project_key,
            "log_id": log_id,
            "decision": decision,
            "decided_by": decided_by,
        },
    })

    return {
        "status": 200,
        "headers": {"content-type": "application/json"},
        "body": json.dumps({"ok": True}),
    }


def _handle_list_context(project_key: str) -> dict:
    contexts = get_contexts(project_key)
    return {
        "status": 200,
        "headers": {"content-type": "application/json"},
        "body": json.dumps(contexts),
    }


def _handle_get_context(project_key: str, ctx_id: int) -> dict:
    ctx = get_context(project_key, ctx_id)
    if ctx:
        return {
            "status": 200,
            "headers": {"content-type": "application/json"},
            "body": json.dumps(ctx),
        }
    return {"status": 404, "body": json.dumps({"error": "context not found"})}


def _handle_list_project_agents(project_key: str) -> dict:
    result = send_request("aman.get_agents", {})
    all_agents = _unwrap_result(result) or []

    return {
        "status": 200,
        "headers": {"content-type": "application/json"},
        "body": json.dumps(all_agents),
    }


# ── Setup / Config API ─────────────────────────────────────────────────


def _handle_team_setup(body: dict) -> dict:
    """Save team config to ~/.aman/team/config.yaml."""
    global _team_config
    team_name = body.get("team_name", "").strip()
    if not team_name:
        return _json_response({"error": "team_name is required"}, 400)

    config = {
        "team": {
            "name": team_name,
            "description": body.get("description", "").strip(),
            "creator": body.get("creator", "").strip(),
        }
    }

    import yaml
    os.makedirs(TEAM_DIR, exist_ok=True)
    with open(_team_config_path(), "w") as f:
        yaml.safe_dump(config, f, default_flow_style=False, allow_unicode=True, sort_keys=False)

    _team_config = _validate_team_config(config)
    _log(f"Team config saved: {team_name}")
    return _json_response({"ok": True, "team_name": team_name})


def _handle_project_create(body: dict) -> dict:
    """Create a new project: write config.yaml, init DB, register routes, compile workflow."""
    project_key = body.get("project_key", "").strip()
    project_name = body.get("project_name", "").strip()
    if not project_key or not project_name:
        return _json_response({"error": "project_key and project_name are required"}, 400)

    # Validate project key format
    if not re.match(r"^[a-z0-9]([a-z0-9-]*[a-z0-9])?$", project_key):
        return _json_response({"error": "project_key must be lowercase alphanumeric with hyphens"}, 400)

    if project_key in _projects:
        return _json_response({"error": f"project '{project_key}' already exists"}, 409)

    stages = body.get("stages", [])
    initial_stage = body.get("initial_stage", stages[0]["id"] if stages else "")

    config = {
        "project": {
            "name": project_name,
            "description": body.get("description", "").strip(),
        },
        "stages": stages,
        "initial_stage": initial_stage,
        "safety_gates": body.get("safety_gates", {
            "dangerous_actions": [
                {"pattern": "rm\\s+-rf", "require_human": True},
                {"pattern": "DROP\\s+TABLE", "require_human": True},
                {"pattern": "DELETE\\s+FROM", "require_human": True},
            ],
            "min_confidence": 0.7,
            "max_autonomous_actions_without_human": 20,
        }),
        "context_files": body.get("context_files", []),
        "work_dir": body.get("work_dir", os.getcwd()),
    }

    import yaml
    proj_dir = _project_dir(project_key)
    os.makedirs(proj_dir, exist_ok=True)
    with open(_project_config_path(project_key), "w") as f:
        yaml.safe_dump(config, f, default_flow_style=False, allow_unicode=True, sort_keys=False)

    validated = _validate_project_config(project_key, config)
    if not validated:
        return _json_response({"error": "Project config validation failed"}, 400)

    _projects[project_key] = {"config": validated}

    try:
        init_db(project_key)
    except Exception as e:
        _log(f"DB init failed for {project_key}: {e}")

    try:
        compile_workflow(project_key)
    except Exception as e:
        _log(f"Workflow registration failed for {project_key}: {e}")

    # Register routes for the new project
    api_prefix = f"/team/projects/{project_key}"
    new_routes = [
        {"method": "GET", "path": api_prefix},
        {"method": "GET", "path": f"{api_prefix}/config"},
        {"method": "GET", "path": f"{api_prefix}/tasks"},
        {"method": "POST", "path": f"{api_prefix}/tasks/create"},
        {"method": "GET", "path": f"{api_prefix}/tasks/{{task_id}}"},
        {"method": "POST", "path": f"{api_prefix}/tasks/{{task_id}}/assign"},
        {"method": "POST", "path": f"{api_prefix}/tasks/{{task_id}}/complete"},
        {"method": "GET", "path": f"{api_prefix}/safety/pending"},
        {"method": "POST", "path": f"{api_prefix}/safety/{{log_id}}/resolve"},
        {"method": "GET", "path": f"{api_prefix}/context"},
        {"method": "GET", "path": f"{api_prefix}/context/{{id}}"},
        {"method": "GET", "path": f"{api_prefix}/agents"},
    ]
    try:
        result = send_request("aman.register_routes", new_routes)
        _log(f"Registered {len(new_routes)} route(s) for {project_key}")
    except Exception as e:
        _log(f"Route registration failed: {e}")

    _log(f"Project created: {project_key} ({project_name})")
    return _json_response({"ok": True, "project_key": project_key, "project_name": project_name}, 201)


def _handle_get_project_config(project_key: str) -> dict:
    """Return the full project config as JSON (for import)."""
    proj = _projects.get(project_key, {}).get("config", {})
    if not proj:
        return _json_response({"error": f"project '{project_key}' not found"}, 404)
    return _json_response(proj)


# ---------------------------------------------------------------------------
# HTML Template Helpers
# ---------------------------------------------------------------------------

_TEMPLATE_DIR = os.path.join(os.path.dirname(os.path.abspath(__file__)), "templates")


def _load_template(name: str) -> Template:
    with open(os.path.join(_TEMPLATE_DIR, name), "r") as f:
        return Template(f.read())


def _esc(s: str) -> str:
    return (s or "").replace("&", "&amp;").replace("<", "&lt;").replace(">", "&gt;").replace('"', "&quot;")


def _build_project_card(key: str, proj: dict) -> str:
    config = proj.get("config", {})
    name = _esc(config.get("project_name", key))
    desc = _esc(config.get("description", ""))
    return (f'<a href="/api/v1/team/projects/{_esc(key)}" class="project-card">'
            f'<h2>{name}</h2><p>{desc}</p>'
            f'<span class="project-key">{_esc(key)}</span></a>')


def _build_kanban_columns(proj: dict) -> str:
    parts = []
    for stage in proj.get("stages", []):
        sid = _esc(stage["id"])
        sname = _esc(stage.get("name", sid))
        parts.append(f'<div class="column" data-stage="{sid}">'
                     f'<div class="column-head"><span class="col-title">{sname}</span>'
                     f'<span class="col-count" id="count-{sid}">0</span></div>'
                     f'<div class="card-list" id="list-{sid}"></div>'
                     f'<button class="add-btn" onclick="openCreate(\'{sid}\')">+ Add</button>'
                     f'</div>')
    return "\n".join(parts)


def _html_response(html: str) -> dict:
    return {"status": 200, "headers": {"content-type": "text/html; charset=utf-8"}, "body": html}


def _json_response(data: Any, status: int = 200) -> dict:
    return {"status": status, "headers": {"content-type": "application/json"}, "body": json.dumps(data)}


# ---------------------------------------------------------------------------
# HTML Page Renderers
# ---------------------------------------------------------------------------


def _render_team_setup() -> dict:
    """Render the setup wizard page."""
    team = _team_config or {}
    existing = {}
    for key, proj in _projects.items():
        config = proj.get("config", {})
        existing[key] = {
            "project_name": config.get("project_name", key),
            "project_key": key,
            "stages": config.get("stages", []),
        }
    tmpl = _load_template("team-setup.html")
    html = tmpl.substitute(
        team_name=_esc(team.get("team_name", "")),
        team_description=_esc(team.get("description", "")),
        team_creator=_esc(team.get("creator", "")),
        existing_projects_json=json.dumps(existing).replace("$", "$$"),
    )
    return _html_response(html)


def _render_team_index() -> dict:
    cards = "\n".join(_build_project_card(k, p) for k, p in sorted(_projects.items()))
    if cards:
        project_items = f'<div class="grid">\n{cards}\n</div>'
    else:
        project_items = '<div class="empty">No projects found. <a href="/api/v1/team/setup" style="color:#6366f1;">Run setup wizard</a> to create one.</div>'

    team = _team_config or {}
    tmpl = _load_template("team-index.html")
    html = tmpl.substitute(
        team_name=_esc(team.get("team_name", "Team")),
        team_description=_esc(team.get("description", "")),
        project_items=project_items,
    )
    return _html_response(html)


def _render_project_kanban(project_key: str) -> dict:
    proj = _projects.get(project_key, {}).get("config", {})
    if not proj:
        return _html_response("<h1>Project not found</h1>")

    columns_html = _build_kanban_columns(proj)
    tmpl = _load_template("project-kanban.html")
    html = tmpl.substitute(
        project_key=_esc(project_key),
        project_name=_esc(proj.get("project_name", project_key)),
        columns_html=columns_html,
    )
    return _html_response(html)


# ---------------------------------------------------------------------------
# Lifecycle Handlers (Server → Plugin requests)
# ---------------------------------------------------------------------------


@on("aman.on_load")
def handle_on_load(params: Any) -> dict:
    """Initialize: load team config, discover projects, init DBs, register routes."""
    global _team_config
    plugin_name = params.get("plugin_name", "team") if isinstance(params, dict) else "team"
    _log(f"on_load: {plugin_name}")

    # Load team config
    _team_config = load_team_config()
    if _team_config:
        _log(f"Loaded team: {_team_config['team_name']}")

    # Discover projects
    discovered = discover_projects()
    _log(f"Discovered {len(discovered)} project(s): {list(discovered.keys())}")

    route_specs = []
    # Team-level page routes (always available, even unconfigured)
    route_specs.append({"method": "GET", "path": "/team"})
    route_specs.append({"method": "GET", "path": "/team/setup"})
    # Setup API routes
    route_specs.append({"method": "POST", "path": "/team/setup"})
    route_specs.append({"method": "POST", "path": "/team/projects/create"})

    for project_key, config in discovered.items():
        _projects[project_key] = {"config": config}

        try:
            init_db(project_key)
        except Exception as e:
            _log(f"DB init failed for {project_key}: {e}")

        try:
            load_context_files(project_key)
        except Exception as e:
            _log(f"Context loading failed for {project_key}: {e}")

        try:
            compile_workflow(project_key)
        except Exception as e:
            _log(f"Workflow registration failed for {project_key}: {e}")

        # Register routes for this project
        api_prefix = f"/team/projects/{project_key}"
        route_specs.extend([
            {"method": "GET", "path": api_prefix},
            {"method": "GET", "path": f"{api_prefix}/config"},
            {"method": "GET", "path": f"{api_prefix}/tasks"},
            {"method": "POST", "path": f"{api_prefix}/tasks/create"},
            {"method": "GET", "path": f"{api_prefix}/tasks/{{task_id}}"},
            {"method": "POST", "path": f"{api_prefix}/tasks/{{task_id}}/assign"},
            {"method": "POST", "path": f"{api_prefix}/tasks/{{task_id}}/complete"},
            {"method": "GET", "path": f"{api_prefix}/safety/pending"},
            {"method": "POST", "path": f"{api_prefix}/safety/{{log_id}}/resolve"},
            {"method": "GET", "path": f"{api_prefix}/context"},
            {"method": "GET", "path": f"{api_prefix}/context/{{id}}"},
            {"method": "GET", "path": f"{api_prefix}/agents"},
        ])

    # Register all routes with the server
    if route_specs:
        try:
            result = send_request("aman.register_routes", route_specs)
            _log(f"Registered {len(route_specs)} route(s): {result}")
        except Exception as e:
            _log(f"Route registration failed: {e}")

    # Subscribe to team events
    try:
        send_request("aman.subscribe_events", {
            "events": [
                "team:work_item.created",
                "team:work_item.assigned",
                "team:work_item.stage_changed",
                "team:work_item.completed",
                "team:work_item.failed",
                "team:safety.gate_triggered",
                "team:safety.gate_resolved",
            ],
        })
        _log("Subscribed to team events")
    except Exception as e:
        _log(f"Event subscription failed: {e}")

    return {"ok": True, "projects": list(discovered.keys()), "routes": len(route_specs)}


@on("aman.on_unload")
def handle_on_unload(params: Any) -> dict:
    """Shutdown: close DBs, cleanup."""
    _log("on_unload")
    for project_key, proj in _projects.items():
        db = proj.get("db")
        if db:
            db.close()
    _projects.clear()
    return {"ok": True}


@on("aman.handle_route")
def handle_route(params: Any) -> dict:
    """Handle an HTTP request forwarded from the server."""
    if not isinstance(params, dict):
        return {"status": 400, "body": json.dumps({"error": "invalid params"})}

    method = params.get("method", "GET")
    path = params.get("path", "")
    query = params.get("query")
    headers = params.get("headers", {})
    body = params.get("body")

    # Normalize path
    clean = path.removeprefix("/api/v1")

    # ── Setup wizard (HTML page) ─────────────────────────────────────
    if method == "GET" and clean in ("/team/setup", "/team/setup/"):
        return _render_team_setup()

    # ── Team index page (HTML) ───────────────────────────────────────
    if method == "GET" and clean in ("/team", "/team/"):
        if _team_config is None:
            return _render_team_setup()
        return _render_team_index()

    # ── Setup API endpoints ──────────────────────────────────────────
    if method == "POST" and clean in ("/team/setup", "/team/setup/"):
        try:
            body_json = json.loads(body) if body else {}
        except (json.JSONDecodeError, TypeError):
            body_json = {}
        return _handle_team_setup(body_json)

    if method == "POST" and clean in ("/team/projects/create", "/team/projects/create/"):
        try:
            body_json = json.loads(body) if body else {}
        except (json.JSONDecodeError, TypeError):
            body_json = {}
        return _handle_project_create(body_json)

    # ── Project config (for import) ──────────────────────────────────
    m_config = re.match(r"/team/projects/([^/]+)/config", clean)
    if m_config and method == "GET":
        return _handle_get_project_config(m_config.group(1))

    # ── Project routes ───────────────────────────────────────────────
    m = re.match(r"/team/projects/([^/]+)", clean)
    if not m:
        return {"status": 404, "body": json.dumps({"error": "project not found in path"})}

    project_key = m.group(1)
    if project_key not in _projects:
        return {"status": 404, "body": json.dumps({"error": f"project '{project_key}' not found"})}

    # Project page (HTML) — GET with no sub-path
    sub = clean[len(f"/team/projects/{project_key}"):].rstrip("/")
    if method == "GET" and sub == "":
        return _render_project_kanban(project_key)

    return handle_api(project_key, method, clean, query, headers, body)


@on("aman.on_event")
def handle_on_event(params: Any) -> None:
    """Handle an event notification from the server."""
    if not isinstance(params, dict):
        return

    event_type = params.get("event_type", "")
    payload = params.get("payload", {})

    project_key = payload.get("project_key", "") if isinstance(payload, dict) else ""

    if event_type == "team:work_item.created":
        _log(f"Event: work_item.created for {project_key}")
        proj = _projects.get(project_key, {}).get("config", {})
        if proj:
            stages = {s["id"]: s for s in proj.get("stages", [])}
            stage_id = payload.get("stage_id", proj.get("initial_stage", ""))
            stage = stages.get(stage_id, {})
            if stage.get("assignment_policy", {}).get("auto_assign"):
                task = {
                    "id": payload.get("work_item_id", ""),
                    "title": payload.get("title", ""),
                    "description": payload.get("description", ""),
                    "priority": payload.get("priority", "normal"),
                }
                dispatch(project_key, task, stage_id)

    elif event_type == "team:work_item.completed":
        _log(f"Event: work_item.completed for {project_key}")
        agent_id = payload.get("agent_id", "") if isinstance(payload, dict) else ""
        reset_autonomous_counter(agent_id)

    elif event_type == "team:safety.gate_triggered":
        _log(f"Event: safety.gate_triggered for {project_key}")

    elif event_type == "team:safety.gate_resolved":
        _log(f"Event: safety.gate_resolved for {project_key}")


# ---------------------------------------------------------------------------
# Main loop
# ---------------------------------------------------------------------------

def main() -> None:
    """Read JSON-RPC lines from stdin forever."""
    _log("Team plugin started, waiting for JSON-RPC...")
    try:
        for line in sys.stdin:
            _dispatch(line.rstrip("\n"))
    except KeyboardInterrupt:
        pass
    except BrokenPipeError:
        pass
    _log("Team plugin stopped")


if __name__ == "__main__":
    main()
