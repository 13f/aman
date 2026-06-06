#!/usr/bin/env python3
"""Bridge between Startup plugin and Team plugin + YantrikDB memory.

Handles:
  1. Auto-creating Team work items from startup:decided events
  2. Syncing analysis results to agent long-term memory (YantrikDB)
  3. Incubation bridge — exposing cross-domain data for Gateway IncubationRunner
"""

from __future__ import annotations

import json
import sys
import time
from typing import Any, Callable, Optional


def _log(msg: str) -> None:
    print(f"[startup-bridge] {msg}", file=sys.stderr, flush=True)


# ---------------------------------------------------------------------------
# Team Integration
# ---------------------------------------------------------------------------


def create_team_work_item(
    send_request: Callable,
    idea_slug: str,
    verdict: str,
    final_score: int,
    rat_experiment: Optional[dict] = None,
    description: str = "",
) -> Optional[dict]:
    """Create a Team kanban work item from a startup decision.

    Called when startup:decided fires with verdict=test or pursue.
    """
    if verdict not in ("test", "pursue"):
        return None

    # Build title and context from the decision
    if verdict == "test" and rat_experiment:
        title = f"RAT: {rat_experiment.get('assumption', idea_slug)[:80]}"
        desc = (
            f"Riskiest Assumption Test for startup idea '{idea_slug}' (score: {final_score}/100).\n\n"
            f"**Experiment**: {rat_experiment.get('description', 'N/A')}\n"
            f"**Duration**: {rat_experiment.get('duration_days', 14)} days\n"
            f"**Budget**: ${rat_experiment.get('estimated_cost_usd', 100)}\n"
            f"**Pass threshold**: {rat_experiment.get('pass_threshold', 'N/A')}\n"
            f"**Fail action**: {rat_experiment.get('fail_action', 'Drop idea, document learning')}\n\n"
            f"Full analysis: /startup/ideas/{idea_slug}"
        )
    elif verdict == "pursue":
        title = f"Build MVP: {idea_slug}"
        desc = (
            f"Startup idea '{idea_slug}' scored {final_score}/100 — verdict: PURSUE.\n\n"
            f"**Idea**: {description[:200]}\n"
            f"**Next**: Scope MVP, define v0.1 boundary, start building.\n\n"
            f"Full analysis: /startup/ideas/{idea_slug}"
        )
    else:
        return None

    try:
        result = send_request("aman.push_work_item", {
            "agent_id": "",  # Let Team dispatcher pick best agent
            "title": title,
            "description": desc,
            "priority": "high",
            "context": {
                "project_key": "startup-experiments",
                "source": "startup-plugin",
                "idea_slug": idea_slug,
                "verdict": verdict,
                "final_score": final_score,
            },
        })
        _log(f"Team work item created for {idea_slug}: {title}")
        return result
    except Exception as e:
        _log(f"Failed to create Team work item for {idea_slug}: {e}")
        return None


# ---------------------------------------------------------------------------
# YantrikDB Memory Sync
# ---------------------------------------------------------------------------


def sync_to_longterm_memory(
    send_request: Callable,
    idea_slug: str,
    agent_id: str,
    analysis_data: dict,
) -> None:
    """Sync key analysis results to the agent's YantrikDB long-term memory.

    This makes analysis insights discoverable by the agent's semantic search
    and available for cross-domain incubation.
    """
    if not agent_id:
        return

    # Store competitor analysis as a memory record
    if analysis_data.get("competitors"):
        comp = analysis_data["competitors"]
        try:
            send_request("aman.emit_event", {
                "event_type": "startup:memory.sync",
                "payload": {
                    "agent_id": agent_id,
                    "idea_slug": idea_slug,
                    "record_type": "competitor_analysis",
                    "content": json.dumps({
                        "idea": idea_slug,
                        "market_saturation": comp.get("market_saturation"),
                        "direct_count": comp.get("direct_count", 0),
                        "saturation_total": comp.get("saturation_score", {}).get("total", 0),
                    }),
                    "tags": ["startup", "competitor", f"idea:{idea_slug}"],
                },
            })
        except Exception as e:
            _log(f"Memory sync (competitor) failed for {idea_slug}: {e}")

    # Store scoring result
    if analysis_data.get("scores"):
        scores = analysis_data["scores"]
        try:
            send_request("aman.emit_event", {
                "event_type": "startup:memory.sync",
                "payload": {
                    "agent_id": agent_id,
                    "idea_slug": idea_slug,
                    "record_type": "score_snapshot",
                    "content": json.dumps({
                        "idea": idea_slug,
                        "final_score": scores.get("final_score"),
                        "verdict": scores.get("verdict"),
                        "confidence": scores.get("confidence"),
                        "killer_dimensions": scores.get("killer_dimensions", []),
                    }),
                    "tags": ["startup", "score", f"idea:{idea_slug}"],
                },
            })
        except Exception as e:
            _log(f"Memory sync (scores) failed for {idea_slug}: {e}")

    _log(f"Synced {idea_slug} analysis to long-term memory (agent={agent_id})")


# ---------------------------------------------------------------------------
# Incubation Bridge
# ---------------------------------------------------------------------------


def build_incubation_context(store: Any) -> dict:
    """Build a cross-domain analysis context for the Gateway's IncubationRunner.

    Queries SurrealDB for patterns across all evaluated ideas that could
    spark creative connections during deep idle incubation.
    """
    if store is None:
        return {"ideas_analyzed": 0, "patterns": []}

    try:
        ideas = store.list_ideas()
        scored = store.get_scored_ideas()

        # Build domain summaries
        domains = {}
        for idea in ideas:
            niche = idea.get("niche", "unknown")
            if niche not in domains:
                domains[niche] = []
            domains[niche].append({
                "slug": idea.get("slug"),
                "verdict": idea.get("verdict"),
                "score": idea.get("final_score"),
            })

        # Find cross-domain patterns
        high_scorers = [i for i in scored if i.get("final_score", 0) >= 55]
        patterns = []
        for idea in high_scorers[:5]:
            comp = store.get_competitor_analysis(idea.get("slug", ""))
            if comp:
                patterns.append({
                    "idea": idea.get("slug"),
                    "score": idea.get("final_score"),
                    "market_saturation": comp.get("market_saturation"),
                    "gap_count": len(comp.get("positioning_gaps", [])),
                })

        return {
            "ideas_analyzed": len(ideas),
            "scored_count": len(scored),
            "domains": domains,
            "high_scoring_patterns": patterns,
            "generated_at": time.time(),
        }
    except Exception as e:
        _log(f"Incubation context build failed: {e}")
        return {"ideas_analyzed": 0, "error": str(e)}
