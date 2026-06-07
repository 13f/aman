#!/usr/bin/env python3
"""Autonomous scheduler for the Startup plugin.

Runs background tasks on a schedule using Python's threading.Timer.
Calls gateway APIs via the JSON-RPC bridge and HTTP.

Tasks:
  - trend_watcher: weekly trend scan for tracked niches
  - rat_reminder: daily check for overdue RAT experiments
  - market_monitor: weekly competitor landscape refresh
"""

from __future__ import annotations

import json
import sys
import threading
import time
from typing import Any, Callable, Optional

import urllib.request
import urllib.error


def _log(msg: str) -> None:
    print(f"[startup-scheduler] {msg}", file=sys.stderr, flush=True)


# ---------------------------------------------------------------------------
# Task definitions
# ---------------------------------------------------------------------------


class ScheduledTask:
    """A recurring task with interval and action."""

    def __init__(self, name: str, interval_seconds: int, action: Callable[[], None]):
        self.name = name
        self.interval = interval_seconds
        self.action = action
        self._timer: Optional[threading.Timer] = None
        self._running = False

    def start(self) -> None:
        """Start the periodic timer."""
        if self._running:
            return
        self._running = True
        self._schedule_next()

    def stop(self) -> None:
        """Stop the timer."""
        self._running = False
        if self._timer:
            self._timer.cancel()
            self._timer = None

    def _schedule_next(self) -> None:
        if not self._running:
            return
        self._timer = threading.Timer(self.interval, self._run_and_reschedule)
        self._timer.daemon = True
        self._timer.start()

    def _run_and_reschedule(self) -> None:
        try:
            self.action()
        except Exception as e:
            _log(f"[{self.name}] Task failed: {e}")
        finally:
            self._schedule_next()


# ---------------------------------------------------------------------------
# Task actions
# ---------------------------------------------------------------------------


def _get_gateway_url() -> str:
    """Resolve gateway URL."""
    import os
    config_path = os.path.expanduser("~/.aman/startup/config.yaml")
    if os.path.isfile(config_path):
        try:
            import yaml
            with open(config_path) as f:
                cfg = yaml.safe_load(f) or {}
            return cfg.get("gateway_url", "http://localhost:9999").rstrip("/")
        except Exception:
            pass
    return "http://localhost:9999"


def send_notification_http(
    title: str,
    message: str,
    severity: str = "info",
    category: str = "plugin",
) -> dict:
    """Send a notification to the gateway via HTTP (thread-safe)."""
    url = f"{_get_gateway_url()}/api/v1/notifications/send"
    data = json.dumps({
        "title": title, "message": message,
        "severity": severity, "category": category,
    }).encode("utf-8")
    req = urllib.request.Request(url, data=data,
        headers={"Content-Type": "application/json"}, method="POST")
    try:
        with urllib.request.urlopen(req, timeout=10) as resp:
            return {"ok": True}
    except Exception as e:
        print(f"[startup-scheduler] Notification failed: {e}", file=sys.stderr, flush=True)
        return {"error": str(e)}


def _http_post(path: str, body: dict) -> dict:
    """Make an HTTP POST request to the gateway."""
    url = f"{_get_gateway_url()}{path}"
    data = json.dumps(body).encode("utf-8")
    req = urllib.request.Request(url, data=data,
        headers={"Content-Type": "application/json"}, method="POST")
    try:
        with urllib.request.urlopen(req, timeout=30) as resp:
            return json.loads(resp.read().decode("utf-8"))
    except Exception as e:
        _log(f"HTTP POST {path} failed: {e}")
        return {"error": str(e)}


def make_trend_watcher(
    store: Any,
    send_notification_fn: Callable,
    agent_id: str = "default",
) -> Callable[[], None]:
    """Create a trend watcher that scans tracked niches for changes.

    Runs weekly (configured via cron expression in production).
    """
    from skills import analyze_trends

    def run() -> None:
        if store is None:
            return
        _log("TrendWatcher: scanning...")
        # Get all niches from stored ideas
        try:
            ideas = store.list_ideas()
            niches = list(set(i.get("niche", "") for i in ideas if i.get("niche")))
            if not niches:
                _log("TrendWatcher: no niches to track")
                return
        except Exception as e:
            _log(f"TrendWatcher: failed to list ideas: {e}")
            return

        for niche in niches[:3]:  # Limit to 3 niches per scan
            try:
                trends = _http_post("/tools/llm_chat/execute", {
                    "agent_id": agent_id,
                    "system_prompt": "You are a market trend analyst. Analyze current trends.",
                    "user_prompt": f"Analyze trends for niche: {niche}\nPlatforms: TikTok, Reddit, App Store, Google Trends\nReturn JSON with trend_velocity and top_signals.",
                })
                output = trends.get("output", {})
                content = output.get("content", "")
                if content:
                    trend_data = json.loads(content) if isinstance(content, str) else content
                    store.store_market_insight(
                        niche, "combined",
                        time.strftime("%Y-%m"),
                        {"trend_velocity": trend_data.get("trend_velocity", "stable"),
                         "top_signals": trend_data.get("top_signals", []),
                         "narrative": json.dumps(trend_data) if isinstance(trend_data, dict) else str(trend_data)}
                    )
                    _log(f"TrendWatcher: stored insight for {niche}")

                    # Alert if rising
                    velocity = trend_data.get("trend_velocity", "stable")
                    if velocity in ("rising", "rising-fast"):
                        send_notification_fn(
                            title=f"Trend Alert: {niche}",
                            message=f"Trend velocity is {velocity} for {niche}. "
                                    f"Top signal: {trend_data.get('top_signals', ['N/A'])[0]}",
                            severity="info",
                            category="plugin",
                        )
            except Exception as e:
                _log(f"TrendWatcher: failed for niche '{niche}': {e}")

    return run


def make_rat_reminder(
    store: Any,
    send_notification_fn: Callable,
) -> Callable[[], None]:
    """Check for overdue RAT experiments daily."""
    def run() -> None:
        if store is None:
            return
        _log("RatReminder: checking...")
        try:
            scored = store.get_scored_ideas(verdict="test", min_score=0)
            now = time.time()
            for idea in scored:
                slug = idea.get("slug", "")
                history = store.get_score_history(slug)
                if not history:
                    continue
                latest = history[-1]
                snapshot_at = latest.get("snapshot_at", "")
                if not snapshot_at:
                    continue
                # Parse ISO timestamp
                try:
                    from datetime import datetime, timezone, timedelta
                    ts = datetime.fromisoformat(snapshot_at.replace("Z", "+00:00"))
                    age_days = (datetime.now(timezone.utc) - ts).days
                    # Alert at 10 days (RAT experiments are ≤14 days)
                    if 10 <= age_days <= 14:
                        send_notification_fn(
                            title=f"RAT Deadline: {slug}",
                            message=f"RAT experiment for '{slug}' is {age_days} days old. "
                                    f"Verdict was 'test' with score {latest.get('final_score', 0)}. "
                                    f"Time to check kill criteria?",
                            severity="warning",
                            category="plugin",
                        )
                except Exception:
                    pass
        except Exception as e:
            _log(f"RatReminder: failed: {e}")

    return run


def make_market_monitor(
    store: Any,
    send_notification_fn: Callable,
    agent_id: str = "default",
) -> Callable[[], None]:
    """Re-analyze competitor landscape for active ideas weekly."""
    def run() -> None:
        if store is None:
            return
        _log("MarketMonitor: scanning...")
        try:
            active = store.list_ideas(status="active")
            if not active:
                return
            for idea in active[:3]:
                slug = idea.get("slug", "")
                desc = idea.get("description", "")
                if not slug or not desc:
                    continue
                result = _http_post("/api/v1/startup/api/validate", {
                    "idea_slug": f"{slug}-refresh-{int(time.time())}",
                    "description": f"Re-analysis: {desc}",
                })
                if result.get("ok"):
                    _log(f"MarketMonitor: refreshed {slug}")
                    changes = result.get("competitors", {})
                    old = store.get_competitor_analysis(slug)
                    old_count = len(old.get("direct_competitors", [])) if old else 0
                    new_count = changes.get("direct_count", 0)
                    if new_count > old_count:
                        send_notification_fn(
                            title=f"New Competitors: {slug}",
                            message=f"Competitor count changed from {old_count} to {new_count}. "
                                    f"New competitors may have entered the market.",
                            severity="info",
                            category="plugin",
                        )
        except Exception as e:
            _log(f"MarketMonitor: failed: {e}")

    return run


# ---------------------------------------------------------------------------
# Scheduler lifecycle
# ---------------------------------------------------------------------------


class StartupScheduler:
    """Manages background autonomous tasks for the startup plugin."""

    WEEKLY = 7 * 24 * 3600
    DAILY = 24 * 3600

    def __init__(self, store: Any, agent_id: str = "default"):
        self.tasks: list[ScheduledTask] = []

        self.tasks.append(ScheduledTask(
            "trend_watcher", self.WEEKLY,
            make_trend_watcher(store, send_notification_http, agent_id),
        ))
        self.tasks.append(ScheduledTask(
            "rat_reminder", self.DAILY,
            make_rat_reminder(store, send_notification_http),
        ))
        self.tasks.append(ScheduledTask(
            "market_monitor", self.WEEKLY,
            make_market_monitor(store, send_notification_http, agent_id),
        ))

    def start(self) -> None:
        """Start all background tasks."""
        _log(f"Starting {len(self.tasks)} autonomous tasks")
        for task in self.tasks:
            task.start()

    def stop(self) -> None:
        """Stop all background tasks."""
        _log("Stopping autonomous tasks")
        for task in self.tasks:
            task.stop()
