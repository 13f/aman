#!/usr/bin/env python3
"""SurrealDB-backed persistent store for the Startup plugin.

Data layout:
    ~/.aman/startup/
        data/               ← SurrealDB files (surrealkv://)
        config.yaml         ← plugin-level config

SurrealDB document types (all in namespace "startup", database "ideas"):

    idea:{slug}                     — core idea record (status, verdict, score)
    competitor_analysis:{slug}      — full competitor landscape
    score_snapshot:{slug}           — time-series scoring history
    market_insight:{id}             — per-niche trend data
    landing_page:{slug}             — generated landing page copy
    gtm_narrative:{slug}            — go-to-market plan
    feedback_synthesis:{slug}       — user feedback analysis
    decision_entry:{id}             — founder decision journal
    ikigai_check:{id}               — ikigai alignment results

Graph edges (RELATE):
    idea:{slug} -> competes_with -> competitor:{name}
    idea:{slug} -> targets_market -> market:{category}
    idea:{slug} -> pivoted_from -> idea:{original_slug}
"""

from __future__ import annotations

import os
from datetime import datetime, timezone
from typing import Any, Optional

from surrealdb import Surreal

# ---------------------------------------------------------------------------
# Paths
# ---------------------------------------------------------------------------

STARTUP_DIR = os.path.expanduser("~/.aman/startup")
DATA_DIR = os.path.join(STARTUP_DIR, "data")


# ---------------------------------------------------------------------------
# Idea state machine
# ---------------------------------------------------------------------------

IDEA_STATES = [
    "candidate",        # generated but not yet validated
    "in_validation",    # currently being analyzed
    "scored",           # validation complete, has decision_memo
    "active",           # user is building this
    "paused",           # on hold
    "dropped",          # decided not to pursue
]

VALID_TRANSITIONS = {
    "candidate":      ["in_validation", "dropped"],
    "in_validation":  ["scored", "dropped"],
    "scored":         ["active", "paused", "dropped"],
    "active":         ["paused", "dropped"],
    "paused":         ["active", "dropped"],
    "dropped":        [],  # terminal
}


# ---------------------------------------------------------------------------
# StartupStore
# ---------------------------------------------------------------------------


class StartupStore:
    """Embedded SurrealDB store for the Startup plugin.

    Usage:
        store = StartupStore()
        store.connect()
        idea = store.create_idea("habit-tracker", {"description": "..."})
        store.close()
    """

    def __init__(self, data_dir: str = DATA_DIR):
        os.makedirs(data_dir, exist_ok=True)
        self._path = data_dir
        self.db: Optional[Surreal] = None

    # ── Lifecycle ─────────────────────────────────────────────────

    def connect(self) -> Surreal:
        """Open the embedded SurrealDB. No separate server needed."""
        self.db = Surreal(f"surrealkv://{self._path}")
        self.db.use("startup", "ideas")
        return self.db

    def close(self) -> None:
        if self.db:
            self.db.close()
            self.db = None

    def is_connected(self) -> bool:
        return self.db is not None

    # ── Record ID helper ──────────────────────────────────────────

    @staticmethod
    def _rid(table: str, id_val: str) -> str:
        """Build a SurrealDB record ID with backtick quoting for special chars.

        SurrealDB requires backtick quoting for IDs containing hyphens,
        dots, or other special characters. This method always quotes
        to be safe.
        """
        return f"{table}:`{id_val}`"

    # ── Idea CRUD ─────────────────────────────────────────────────

    def create_idea(self, slug: str, idea: dict) -> dict:
        """Create a new idea in 'candidate' state."""
        now = datetime.now(timezone.utc).isoformat()
        result = self.db.create(self._rid("idea", slug), {
            **idea,
            "slug": slug,
            "status": "candidate",
            "final_score": None,
            "verdict": None,
            "created_at": now,
            "updated_at": now,
        })
        return self._to_dict(result)

    def get_idea(self, slug: str) -> Optional[dict]:
        """Get an idea by slug."""
        result = self.db.select(self._rid("idea", slug))
        return self._first(result)

    def list_ideas(self, status: Optional[str] = None) -> list:
        """List all ideas, optionally filtered by status."""
        if status:
            result = self.db.query(
                f"SELECT * FROM idea WHERE status = '{status}' ORDER BY updated_at DESC"
            )
        else:
            result = self.db.query(
                "SELECT * FROM idea ORDER BY updated_at DESC"
            )
        return self._unwrap(result)

    def update_idea_status(self, slug: str, new_status: str) -> dict:
        """Transition an idea to a new state. Validates transitions."""
        current = self.get_idea(slug)
        if not current:
            raise ValueError(f"Idea '{slug}' not found")

        old_status = current.get("status", "candidate")
        allowed = VALID_TRANSITIONS.get(old_status, [])
        if new_status not in allowed:
            raise ValueError(
                f"Invalid transition: {old_status} -> {new_status}. "
                f"Allowed: {allowed}"
            )

        now = datetime.now(timezone.utc).isoformat()
        rid = self._rid("idea", slug)
        return self.db.query(f"""
            UPDATE {rid} MERGE {{
                status: '{new_status}',
                updated_at: '{now}'
            }}
        """)

    def delete_idea(self, slug: str) -> None:
        """Soft-delete: set status to 'dropped'. Hard delete via query if needed."""
        self.update_idea_status(slug, "dropped")

    # ── Competitor Analysis ────────────────────────────────────────

    def store_competitor_analysis(self, idea_slug: str, analysis: dict) -> dict:
        """Store full competitor analysis as a single document (no normalization)."""
        now = datetime.now(timezone.utc).isoformat()
        result = self.db.create(self._rid("competitor_analysis", idea_slug), {
            "idea_slug": idea_slug,
            "direct_competitors": analysis.get("direct_competitors", []),
            "indirect_competitors": analysis.get("indirect_competitors", []),
            "substitutes": analysis.get("substitutes", []),
            "emerging_threats": analysis.get("emerging_threats", []),
            "positioning_gaps": analysis.get("positioning_gaps", []),
            "saturation_score": analysis.get("saturation_score", {}),
            "market_saturation": analysis.get("market_saturation", ""),
            "reviewed_at": now,
        })

        # Build graph edges: idea -> competes_with -> competitor
        idea_rid = self._rid("idea", idea_slug)
        for comp in analysis.get("direct_competitors", []):
            comp_id = self._normalize_id(comp.get("name", ""))
            if not comp_id:
                continue
            comp_rid = self._rid("competitor", comp_id)
            # Ensure competitor node exists
            self.db.query(f"""
                CREATE {comp_rid} CONTENT $data
            """, {"data": {
                "name": comp.get("name", ""),
                "platform": comp.get("platform", ""),
            }})
            # Create edge
            self.db.query(f"""
                RELATE {idea_rid}->competes_with->{comp_rid}
                SET strength = 'direct', discovered_at = $ts
            """, {"ts": now})

        return result

    def get_competitor_analysis(self, idea_slug: str) -> Optional[dict]:
        return self._first(self.db.select(self._rid("competitor_analysis", idea_slug)))

    def find_competing_ideas(self, competitor_name: str) -> list:
        """Find all ideas that compete with a given competitor (graph traversal)."""
        comp_id = self._normalize_id(competitor_name)
        comp_rid = self._rid("competitor", comp_id)
        return self._unwrap(self.db.query(f"""
            SELECT *, <-competes_with<-idea.* AS ideas
            FROM {comp_rid}
        """))

    # ── Score Snapshots (time-series) ──────────────────────────────

    def store_score_snapshot(self, idea_slug: str, scores: dict) -> dict:
        """Store a scoring snapshot and update the idea's current state."""
        now = datetime.now(timezone.utc).isoformat()

        # Store the snapshot
        result = self.db.create(self._rid("score_snapshot", idea_slug), {
            "idea_slug": idea_slug,
            "dimension_scores": scores.get("dimension_scores", {}),
            "weights_applied": scores.get("weights_applied", {}),
            "base_score": scores.get("base_score", 0),
            "floor_penalty": scores.get("floor_penalty", 1.0),
            "missing_discount": scores.get("missing_discount", 1.0),
            "final_score": scores.get("final_score", 0),
            "verdict": scores.get("verdict", ""),
            "confidence": scores.get("confidence", "low"),
            "killer_dimensions": scores.get("killer_dimensions", []),
            "snapshot_at": now,
        })

        # Update the idea record
        idea_rid = self._rid("idea", idea_slug)
        self.db.query(f"""
            UPDATE {idea_rid} MERGE {{
                status: 'scored',
                final_score: {scores.get("final_score", 0)},
                verdict: '{scores.get("verdict", "drop")}',
                updated_at: '{now}'
            }}
        """)

        return result

    def get_score_history(self, idea_slug: str) -> list:
        """Get scoring history for an idea (time-series)."""
        return self._unwrap(self.db.query(f"""
            SELECT * FROM score_snapshot
            WHERE idea_slug = '{idea_slug}'
            ORDER BY snapshot_at ASC
        """))

    # ── Market Insights ────────────────────────────────────────────

    def store_market_insight(self, niche: str, platform: str, period: str,
                              insight: dict) -> dict:
        """Store a market insight (append-only pattern)."""
        now = datetime.now(timezone.utc).isoformat()
        insight_id = f"{niche}-{platform}-{period}"
        return self.db.create(self._rid("market_insight", insight_id), {
            "niche": niche,
            "platform": platform,
            "period": period,
            "trend_velocity": insight.get("trend_velocity", "stable"),
            "top_signals": insight.get("top_signals", []),
            "monetization_evidence": insight.get("monetization_evidence", False),
            "narrative": insight.get("narrative", ""),
            "recorded_at": now,
        })

    def get_market_insights(self, niche: Optional[str] = None) -> list:
        """Get market insights, optionally filtered by niche."""
        if niche:
            return self._unwrap(self.db.query(f"""
                SELECT * FROM market_insight
                WHERE niche = '{niche}'
                ORDER BY recorded_at DESC
            """))
        return self._unwrap(self.db.query(
            "SELECT * FROM market_insight ORDER BY recorded_at DESC"
        ))

    # ── Strategy Layer ─────────────────────────────────────────────

    def store_landing_page(self, idea_slug: str, landing_page: dict) -> dict:
        """Store generated landing page copy."""
        now = datetime.now(timezone.utc).isoformat()
        return self.db.create(self._rid("landing_page", idea_slug), {
            "idea_slug": idea_slug,
            "hero_variants": landing_page.get("hero_variants", []),
            "social_proof_strategy": landing_page.get("social_proof_strategy", ""),
            "ab_test_plan": landing_page.get("ab_test_plan", {}),
            "seo_keywords": landing_page.get("seo_keywords", []),
            "differentiator_oneliner": landing_page.get("differentiator_oneliner", ""),
            "generated_at": now,
        })

    def store_gtm_narrative(self, idea_slug: str, gtm: dict) -> dict:
        """Store go-to-market narrative."""
        now = datetime.now(timezone.utc).isoformat()
        return self.db.create(self._rid("gtm_narrative", idea_slug), {
            "idea_slug": idea_slug,
            "product_hunt": gtm.get("product_hunt", {}),
            "reddit_plan": gtm.get("reddit_plan", []),
            "build_in_public": gtm.get("build_in_public", []),
            "cold_email": gtm.get("cold_email", {}),
            "content_seeds": gtm.get("content_seeds", []),
            "generated_at": now,
        })

    # ── Execution Layer ────────────────────────────────────────────

    def store_feedback_synthesis(self, idea_slug: str, synthesis: dict) -> dict:
        """Store user feedback synthesis."""
        now = datetime.now(timezone.utc).isoformat()
        return self.db.create(self._rid("feedback_synthesis", idea_slug), {
            "idea_slug": idea_slug,
            "topic_clusters": synthesis.get("topic_clusters", []),
            "sentiment_trends": synthesis.get("sentiment_trends", []),
            "feature_requests": synthesis.get("feature_requests", []),
            "latent_needs": synthesis.get("latent_needs", []),
            "competitive_gap_check": synthesis.get("competitive_gap_check", ""),
            "synthesized_at": now,
        })

    # ── Reflection Layer ───────────────────────────────────────────

    def store_decision_entry(self, entry: dict) -> dict:
        """Store a founder decision journal entry."""
        now = datetime.now(timezone.utc).isoformat()
        entry_id = entry.get("id") or f"decision-{int(datetime.now().timestamp())}"
        return self.db.create(self._rid("decision_entry", entry_id), {
            "decision": entry.get("decision", ""),
            "info_at_the_time": entry.get("info_at_the_time", ""),
            "assumptions": entry.get("assumptions", []),
            "expected_outcome": entry.get("expected_outcome", ""),
            "actual_outcome": entry.get("actual_outcome"),
            "was_correct": entry.get("was_correct"),
            "idea_slug": entry.get("idea_slug", ""),
            "recorded_at": now,
        })

    def list_decision_entries(self, idea_slug: Optional[str] = None) -> list:
        """List decision entries, optionally filtered by idea."""
        if idea_slug:
            return self._unwrap(self.db.query(f"""
                SELECT * FROM decision_entry
                WHERE idea_slug = '{idea_slug}'
                ORDER BY recorded_at DESC
            """))
        return self._unwrap(self.db.query(
            "SELECT * FROM decision_entry ORDER BY recorded_at DESC"
        ))

    def store_ikigai_check(self, check: dict) -> dict:
        """Store an ikigai alignment check result."""
        now = datetime.now(timezone.utc).isoformat()
        check_id = f"ikigai-{int(datetime.now().timestamp())}"
        return self.db.create(self._rid("ikigai_check", check_id), {
            "alignment_score": check.get("alignment_score", 0),
            "overlapping_quadrants": check.get("overlapping_quadrants", []),
            "missing_quadrant": check.get("missing_quadrant", ""),
            "contradiction": check.get("contradiction", ""),
            "suggested_adjustment": check.get("suggested_adjustment", ""),
            "checked_at": now,
        })

    # ── Cross-idea Queries ─────────────────────────────────────────

    def get_scored_ideas(self, verdict: Optional[str] = None,
                         min_score: float = 0) -> list:
        """Cross-idea query with optional filters."""
        conditions = []
        if verdict:
            conditions.append(f'verdict = "{verdict}"')
        if min_score:
            conditions.append(f'final_score >= {min_score}')
        clause = " AND ".join(conditions) if conditions else "true"
        return self._unwrap(self.db.query(f"""
            SELECT slug, final_score, verdict, status,
                   ->competes_with->competitor.* AS competitors
            FROM idea
            WHERE {clause}
            ORDER BY final_score DESC
        """))

    # ── Helpers ────────────────────────────────────────────────────

    def _normalize_id(self, name: str) -> str:
        """Convert a name to a SurrealDB-safe record ID (underscore-only)."""
        import re
        name = name.lower().strip()
        name = re.sub(r"[^a-z0-9]+", "_", name)
        name = name.strip("_")
        return name[:80] if name else ""

    @staticmethod
    def _first(result) -> Optional[dict]:
        """Get the first record from a SurrealDB select/query result."""
        if result is None:
            return None
        if isinstance(result, list):
            item = result[0] if result else None
        elif isinstance(result, dict):
            item = result
        else:
            return None
        if item is None:
            return None
        return StartupStore._to_dict_static(item)

    @staticmethod
    def _to_dict_static(record) -> dict:
        """Convert a SurrealDB record to a plain dict, removing RecordID objects."""
        if record is None:
            return {}
        if isinstance(record, list):
            return [StartupStore._to_dict_static(r) for r in record]  # type: ignore
        if hasattr(record, "__dict__") and not isinstance(record, dict):
            record = vars(record)
        if not isinstance(record, dict):
            return {"value": str(record)}
        result = {}
        for k, v in record.items():
            if k == "id":
                # Convert RecordID to string
                result[k] = str(v)
            elif hasattr(v, "__dict__") and not isinstance(v, (dict, list, str, int, float, bool, type(None))):
                result[k] = str(v)
            elif isinstance(v, list):
                result[k] = [str(x) if hasattr(x, "__dict__") and not isinstance(x, (dict, str)) else x for x in v]
            else:
                result[k] = v
        return result

    def _to_dict(self, record) -> dict:
        """Convert a SurrealDB record to a plain dict (instance wrapper)."""
        return self._to_dict_static(record)

    def _unwrap(self, result) -> list:
        """Unwrap SurrealDB query results to a flat list of dicts."""
        if result is None:
            return []
        if isinstance(result, list):
            return [self._to_dict(r) for r in result]
        if isinstance(result, dict):
            return [self._to_dict(result)]
        return list(result) if hasattr(result, "__iter__") else [result]
