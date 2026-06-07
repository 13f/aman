#!/usr/bin/env python3
"""Shared LLM client for aman plugins.

Calls the gateway's llm_chat tool endpoint (POST /tools/llm_chat/execute).
All plugins can import from self.llm instead of writing their own HTTP client.
"""

from __future__ import annotations

import json
import sys
import os
import time
import urllib.request
import urllib.error
from typing import Any, Optional


def _log(msg: str) -> None:
    print(f"[self.llm] {msg}", file=sys.stderr, flush=True)


# ── Config ──────────────────────────────────────────────────────────────

def get_gateway_url() -> str:
    """Resolve the gateway base URL from config or defaults."""
    config_path = os.path.expanduser("~/.aman/startup/config.yaml")
    config = {}
    if os.path.isfile(config_path):
        try:
            import yaml
            with open(config_path, "r") as f:
                config = yaml.safe_load(f) or {}
        except Exception:
            pass
    return config.get("gateway_url", "http://localhost:9999").rstrip("/")


def get_agent_id() -> str:
    """Auto-detect an agent ID to use for LLM requests.

    Priority:
      1. AMAN_AGENT_ID env var
      2. ~/.aman/startup/config.yaml → agent_id
      3. Query the gateway for the first available agent
    """
    env_id = os.environ.get("AMAN_AGENT_ID", "").strip()
    if env_id:
        return env_id

    config_path = os.path.expanduser("~/.aman/startup/config.yaml")
    config = {}
    if os.path.isfile(config_path):
        try:
            import yaml
            with open(config_path, "r") as f:
                config = yaml.safe_load(f) or {}
        except Exception:
            pass
    config_id = config.get("agent_id", "").strip()
    if config_id:
        return config_id

    try:
        url = f"{get_gateway_url()}/agents"
        req = urllib.request.Request(url, method="GET")
        req.add_header("Accept", "application/json")
        with urllib.request.urlopen(req, timeout=5) as resp:
            agents = json.loads(resp.read().decode("utf-8"))
            if agents and len(agents) > 0:
                for a in agents:
                    agent_id = (
                        (a.get("descriptor", {}).get("agent_id", ""))
                        or a.get("id", "")
                        or a.get("key", "")
                    )
                    if agent_id:
                        _log(f"Auto-selected agent: {agent_id}")
                        return agent_id
    except Exception as e:
        _log(f"Failed to query gateway for agents: {e}")

    _log("WARNING: No agent configured. Set AMAN_AGENT_ID env var or add 'agent_id' to config.")
    return "default"


# ── LLM Client ──────────────────────────────────────────────────────────

class LlmClient:
    """Reusable LLM client that calls the gateway's llm_chat tool.

    Usage:
        client = LlmClient(agent_id="my-agent")
        text = client.chat("You are a helpful assistant.", "Hello!")
        data = client.chat_json("You output JSON.", "Return {...}")
    """

    def __init__(self, agent_id: Optional[str] = None, gateway_url: Optional[str] = None):
        self.agent_id = agent_id or get_agent_id()
        self._url = f"{gateway_url or get_gateway_url()}/tools/llm_chat/execute"

    def chat(
        self,
        system_prompt: str,
        user_prompt: str,
        *,
        temperature: float = 0.3,
        max_tokens: int = 4000,
        timeout: int = 300,
        retries: int = 1,
        response_format: Optional[str] = None,
    ) -> str:
        """One-shot chat completion. Returns the model's text response."""
        body = {
            "agent_id": self.agent_id,
            "system_prompt": system_prompt,
            "user_prompt": user_prompt,
            "temperature": temperature,
            "max_tokens": max_tokens,
        }
        if response_format:
            body["response_format"] = response_format
        return self._call(body, timeout=timeout, retries=retries)

    def chat_json(
        self,
        system_prompt: str,
        user_prompt: str,
        *,
        temperature: float = 0.2,
        max_tokens: int = 8000,
        timeout: int = 300,
        retries: int = 1,
        response_format: str = "json_object",
    ) -> dict:
        """One-shot chat completion, parse response as JSON.

        Default max_tokens=8000 to leave headroom for reasoning models that
        consume output quota on chain-of-thought before the final JSON.

        By default sets ``response_format="json_object"`` so the model is
        constrained to output valid JSON — no markdown fences to strip.
        Set to ``None`` to disable this constraint.
        """
        body = {
            "agent_id": self.agent_id,
            "system_prompt": system_prompt,
            "user_prompt": user_prompt,
            "temperature": temperature,
            "max_tokens": max_tokens,
        }
        if response_format:
            body["response_format"] = response_format
        text = self._call(body, timeout=timeout, retries=retries)
        text = text.strip()
        # Strip markdown code fences (only needed when response_format is off)
        if text.startswith("```"):
            lines = text.split("\n")
            text = "\n".join(lines[1:-1]) if len(lines) > 2 else text
        return json.loads(text)

    def _call(self, body: dict, timeout: int = 120, retries: int = 1) -> str:
        data = json.dumps(body).encode("utf-8")
        headers = {"Content-Type": "application/json"}

        last_error = None
        for attempt in range(retries + 1):
            try:
                req = urllib.request.Request(
                    self._url, data=data, headers=headers, method="POST"
                )
                with urllib.request.urlopen(req, timeout=timeout) as resp:
                    result = json.loads(resp.read().decode("utf-8"))
                    if "error" in result:
                        last_error = result.get("error", str(result))
                        _log(f"Tool error (attempt {attempt+1}): {last_error}")
                        if attempt < retries:
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
                    time.sleep(2 ** attempt)
            except Exception as e:
                last_error = str(e)
                _log(f"llm_chat call failed (attempt {attempt+1}): {last_error}")
                if attempt < retries:
                    time.sleep(2 ** attempt)

        raise RuntimeError(f"llm_chat tool failed after {retries+1} attempts: {last_error}")
