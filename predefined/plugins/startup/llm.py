#!/usr/bin/env python3
"""LLM client for the Startup plugin — re-exports from self.llm."""

# Ensure ~/.aman is on sys.path so we can import self.llm
import sys, os
_aman_dir = os.path.expanduser("~/.aman")
if _aman_dir not in sys.path:
    sys.path.insert(0, _aman_dir)

from self.llm import LlmClient, get_agent_id, get_gateway_url

__all__ = ["LlmClient", "get_agent_id", "get_gateway_url"]
