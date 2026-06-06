#!/usr/bin/env python3
"""LLM client for the Startup plugin.

Calls the aman gateway's existing tool-execution endpoint:
    POST /api/v1/tools/llm_chat/execute

This reuses the gateway's `llm_chat` tool (registered at startup with
access to every agent's LLM provider). No new API keys, no new HTTP
routes — just the existing tool system.
"""

from __future__ import annotations

import json
import os
import sys
from typing import Any, Optional

import urllib.request
import urllib.error

# ---------------------------------------------------------------------------
# Config
# ---------------------------------------------------------------------------

STARTUP_DIR = os.path.expanduser("~/.aman/startup")
CONFIG_PATH = os.path.join(STARTUP_DIR, "config.yaml")


def _log(msg: str) -> None:
    print(f"[startup-llm] {msg}", file=sys.stderr, flush=True)


def _get_gateway_url() -> str:
    """Resolve the gateway base URL from config or defaults."""
    config = {}
    if os.path.isfile(CONFIG_PATH):
        try:
            import yaml
            with open(CONFIG_PATH, "r") as f:
                config = yaml.safe_load(f) or {}
        except Exception:
            pass
    return config.get("gateway_url", "http://localhost:9999").rstrip("/")


def _get_agent_id() -> str:
    """Get the agent ID to use for LLM requests.

    Priority:
      1. STARTUP_AGENT_ID env var
      2. ~/.aman/startup/config.yaml → agent_id
      3. Query the gateway for the first available agent
    """
    env_id = os.environ.get("STARTUP_AGENT_ID", "").strip()
    if env_id:
        return env_id

    config = {}
    if os.path.isfile(CONFIG_PATH):
        try:
            import yaml
            with open(CONFIG_PATH, "r") as f:
                config = yaml.safe_load(f) or {}
        except Exception:
            pass
    config_id = config.get("agent_id", "").strip()
    if config_id:
        return config_id

    try:
        url = f"{_get_gateway_url()}/api/v1/agents"
        req = urllib.request.Request(url, method="GET")
        req.add_header("Accept", "application/json")
        with urllib.request.urlopen(req, timeout=5) as resp:
            agents = json.loads(resp.read().decode("utf-8"))
            if agents and len(agents) > 0:
                agent_id = agents[0].get("id", "")
                _log(f"Auto-selected agent: {agent_id}")
                return agent_id
    except Exception as e:
        _log(f"Failed to query gateway for agents: {e}")

    return "default"


# ---------------------------------------------------------------------------
# LLM Client (via gateway tool system)
# ---------------------------------------------------------------------------


class LlmClient:
    """LLM client that calls the gateway's llm_chat tool."""

    def __init__(self, agent_id: Optional[str] = None):
        self.agent_id = agent_id or _get_agent_id()
        self._url = f"{_get_gateway_url()}/api/v1/tools/llm_chat/execute"

    def chat(
        self,
        system_prompt: str,
        user_prompt: str,
        *,
        temperature: float = 0.3,
        max_tokens: int = 4000,
    ) -> str:
        """Call the llm_chat tool via the gateway.

        Returns the model's text response.
        """
        body = {
            "agent_id": self.agent_id,
            "system_prompt": system_prompt,
            "user_prompt": user_prompt,
            "temperature": temperature,
            "max_tokens": max_tokens,
        }
        return self._call(body)

    def chat_json(
        self,
        system_prompt: str,
        user_prompt: str,
        *,
        temperature: float = 0.2,
        max_tokens: int = 4000,
    ) -> dict:
        """Call chat() and parse the response as JSON."""
        text = self.chat(
            system_prompt=system_prompt,
            user_prompt=user_prompt,
            temperature=temperature,
            max_tokens=max_tokens,
        )
        text = text.strip()
        if text.startswith("```"):
            lines = text.split("\n")
            text = "\n".join(lines[1:-1]) if len(lines) > 2 else text
        return json.loads(text)

    def _call(self, body: dict, retries: int = 2) -> str:
        data = json.dumps(body).encode("utf-8")
        headers = {"Content-Type": "application/json"}

        last_error = None
        for attempt in range(retries + 1):
            try:
                req = urllib.request.Request(
                    self._url, data=data, headers=headers, method="POST"
                )
                with urllib.request.urlopen(req, timeout=120) as resp:
                    result = json.loads(resp.read().decode("utf-8"))
                    # Tool response: {"output": {"content": "...", "finish_reason": "..."}}
                    # or error: {"error": "..."}
                    if "error" in result:
                        last_error = result.get("error", str(result))
                        _log(f"Tool error (attempt {attempt+1}): {last_error}")
                        if attempt < retries:
                            import time
                            time.sleep(2 ** attempt)
                        continue
                    output = result.get("output", result)
                    content = output.get("content", "")
                    if not content:
                        _log(f"Empty response from llm_chat (attempt {attempt+1})")
                        continue
                    return content
            except urllib.error.HTTPError as e:
                last_error = f"HTTP {e.code}: {e.read().decode()[:500]}"
                _log(f"llm_chat call failed (attempt {attempt+1}): {last_error}")
                if attempt < retries:
                    import time
                    time.sleep(2 ** attempt)
            except Exception as e:
                last_error = str(e)
                _log(f"llm_chat call failed (attempt {attempt+1}): {last_error}")
                if attempt < retries:
                    import time
                    time.sleep(2 ** attempt)

        raise RuntimeError(f"llm_chat tool failed after {retries+1} attempts: {last_error}")
