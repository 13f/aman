#!/usr/bin/env python3
"""Reusable bidirectional JSON-RPC 2.0 bridge for subprocess plugins.

Drop-in replacement for the duplicated bridge code in every plugin.
Plugins import the bridge, register handlers via ``@bridge.on("method")``,
and call ``bridge.run_forever()`` in their main.

Usage:
    import sys
    from self.jsonrpc import Bridge

    bridge = Bridge("my-plugin")

    @bridge.on("my_method")
    def handle_my_method(params):
        return {"ok": True}

    if __name__ == "__main__":
        bridge.main()
"""

from __future__ import annotations

import json
import sys
import traceback
from typing import Any, Callable, Dict, Optional


class Bridge:
    """Bidirectional JSON-RPC 2.0 bridge over stdin/stdout.

    Each plugin creates ONE Bridge instance. The bridge:
    - Reads JSON-RPC requests from stdin
    - Dispatches to registered handlers
    - Sends responses back to stdout
    - Supports sending requests to the server and awaiting responses
    """

    def __init__(self, plugin_name: str):
        self.plugin_name = plugin_name
        self._pending: Dict[int, "_PendingRequest"] = {}
        self._next_id = 1
        self._handlers: Dict[str, Callable[[Any], Any]] = {}

    # ── Logging ──────────────────────────────────────────────────────

    def log(self, msg: str) -> None:
        print(f"[{self.plugin_name}] {msg}", file=sys.stderr, flush=True)

    # ── Sending (Plugin → Server) ────────────────────────────────────

    def send_response(self, req_id: int, result: Any) -> None:
        payload = json.dumps({"jsonrpc": "2.0", "id": req_id, "result": result})
        self._write_line(payload)

    def send_error(self, req_id: int, code: int, message: str) -> None:
        payload = json.dumps(
            {"jsonrpc": "2.0", "id": req_id, "error": {"code": code, "message": message}}
        )
        self._write_line(payload)

    def send_request(self, method: str, params: Any) -> Any:
        """Send a JSON-RPC request to the server and block waiting for response."""
        rid = self._make_id()
        payload = json.dumps({"jsonrpc": "2.0", "id": rid, "method": method, "params": params})
        result_holder: list = []

        def resolve(val: Any) -> None:
            result_holder.append(val)

        self._pending[rid] = _PendingRequest(method, resolve)
        self._write_line(payload)
        self._process_until_response(rid)

        if not result_holder:
            raise RuntimeError(f"No response for {method}")
        return result_holder[0]

    def send_notification(self, method: str, params: Any) -> None:
        """Send a JSON-RPC notification (no response expected)."""
        payload = json.dumps({"jsonrpc": "2.0", "method": method, "params": params})
        self._write_line(payload)

    # ── Receiving (Server → Plugin handling) ─────────────────────────

    def _process_until_response(self, rid: int) -> None:
        while rid in self._pending:
            line = sys.stdin.readline()
            if not line:
                break
            self._dispatch(line.rstrip("\n"))

    def _dispatch(self, line: str) -> None:
        if not line.strip():
            return
        try:
            msg = json.loads(line)
        except json.JSONDecodeError as e:
            self.log(f"Invalid JSON from server: {e}")
            return

        msg_id = msg.get("id")

        if msg_id is not None and "method" not in msg:
            rid = int(msg_id)
            pending = self._pending.pop(rid, None)
            if pending:
                result = msg.get("result")
                error = msg.get("error")
                if error:
                    self.log(f"Server error for {pending.method}: {error}")
                    pending.resolve({"__error__": error})
                else:
                    pending.resolve(result)
            return

        method = msg.get("method")
        if method is None:
            return

        params = msg.get("params")
        req_id = int(msg_id) if msg_id is not None else None
        self._handle_incoming_request(method, params, req_id)

    def _handle_incoming_request(self, method: str, params: Any, req_id: Optional[int]) -> None:
        handler = self._handlers.get(method)
        if handler is None:
            self.log(f"Unknown method: {method}")
            if req_id is not None:
                self.send_error(req_id, -32601, f"Method not found: {method}")
            return

        try:
            result = handler(params)
            if req_id is not None:
                self.send_response(req_id, result)
        except Exception as e:
            self.log(f"Handler error for {method}: {traceback.format_exc()}")
            if req_id is not None:
                self.send_error(req_id, -32000, str(e))

    def on(self, method: str):
        """Decorator to register a handler for a server→plugin method."""
        def decorator(fn):
            self._handlers[method] = fn
            return fn
        return decorator

    # ── Internal helpers ─────────────────────────────────────────────

    def _make_id(self) -> int:
        rid = self._next_id
        self._next_id += 1
        return rid

    @staticmethod
    def _write_line(data: str) -> None:
        try:
            sys.stdout.write(data + "\n")
            sys.stdout.flush()
        except BrokenPipeError:
            pass

    # ── Main loop ────────────────────────────────────────────────────

    def main(self) -> None:
        """Read JSON-RPC lines from stdin forever."""
        self.log(f"Started, waiting for JSON-RPC...")
        try:
            for line in sys.stdin:
                self._dispatch(line.rstrip("\n"))
        except KeyboardInterrupt:
            pass
        except BrokenPipeError:
            pass
        self.log(f"Stopped")


class _PendingRequest:
    __slots__ = ("method", "resolve")

    def __init__(self, method: str, resolve: Callable[[Any], None]):
        self.method = method
        self.resolve = resolve
