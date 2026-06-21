#!/usr/bin/env python3
"""Burnout Early Warning — multi-signal founder burnout risk detection.

Architecture (v1 — 3-phase, data-driven):
  1. Data extraction  — pull structured signals from store for 5 categories
  2. Algorithmic risk scoring — rule-based aggregation into 0-100 risk score
  3. LLM synthesis    — context-aware interpretation + founder narrative

Signal categories:
  - productivity       — 3-week score decline >= 40%
  - decision_quality   — incorrect decision ratio, pivot frequency
  - ikigai             — contradiction severity, alignment score
  - pursuit            — active ideas stuck without progress
  - health             — sleep, mood, energy anomalies (graceful no-op)

Trigger rules (from maslow-hierarchy.md):
  - 3-week productivity decline >= 40% → burnout signal
  - High-severity ikigai contradictions → auto-trigger after ikigai check
  - Weekly cron (Sunday evening) → proactive scan
"""

from __future__ import annotations

import json
import os
from collections import Counter
from dataclasses import dataclass, field
from datetime import datetime, timezone, timedelta
from typing import Any, Optional

from llm import LlmClient

_PROMPT_DIR = os.path.join(os.path.dirname(os.path.dirname(__file__)), "prompts")


def _load(name: str) -> str:
    return open(os.path.join(_PROMPT_DIR, name)).read()


# ---------------------------------------------------------------------------
# Data types
# ---------------------------------------------------------------------------


@dataclass
class BurnoutSignal:
    """A single detected burnout risk signal with evidence."""

    category: str  # productivity | decision_quality | ikigai | pursuit | health
    signal: str
    severity: str  # high | medium | low
    magnitude: float = 0.0  # 0.0-1.0
    evidence: list[str] = field(default_factory=list)

    def to_dict(self) -> dict:
        return {
            "category": self.category,
            "signal": self.signal,
            "severity": self.severity,
            "magnitude": self.magnitude,
            "evidence": self.evidence,
        }


@dataclass
class BurnoutRisk:
    """Algorithmic burnout risk assessment (pre-LLM)."""

    risk_score: float = 0.0
    risk_level: str = "low"  # low | moderate | high | critical
    signals: list = field(default_factory=list)
    productivity_decline_pct: float | None = None
    decision_quality_trend: str | None = None
    ikigai_contradiction_count: int = 0
    pursuit_stuck_ideas: list[str] = field(default_factory=list)
    health_anomalies: list[str] = field(default_factory=list)
    recommendations: list[str] = field(default_factory=list)

    def to_dict(self) -> dict:
        return {
            "risk_score": self.risk_score,
            "risk_level": self.risk_level,
            "signals": [s.to_dict() if isinstance(s, BurnoutSignal) else s for s in self.signals],
            "productivity_decline_pct": self.productivity_decline_pct,
            "decision_quality_trend": self.decision_quality_trend,
            "ikigai_contradiction_count": self.ikigai_contradiction_count,
            "pursuit_stuck_ideas": self.pursuit_stuck_ideas,
            "health_anomalies": self.health_anomalies,
            "recommendations": self.recommendations,
        }


# ---------------------------------------------------------------------------
# Phase 1: Data extraction
# ---------------------------------------------------------------------------


def _compute_score_trend(
    scores: list[dict],
    lookback_days: int = 21,
) -> dict:
    """Compute score decline over a lookback window.

    Splits score snapshots into two windows:
      - recent: last 7 days
      - prior:  days 8-21 before now

    Returns:
        dict with recent_avg, prior_avg, decline_pct, is_declining, data_points.
        is_declining is False when fewer than 2 data points exist (cold-start safe).
    """
    now = datetime.now(timezone.utc)
    cutoff_recent = now - timedelta(days=7)
    cutoff_prior_start = now - timedelta(days=lookback_days)

    recent_scores = []
    prior_scores = []

    for s in scores:
        snapshot_at = s.get("snapshot_at", "")
        if not snapshot_at:
            continue
        try:
            ts = datetime.fromisoformat(snapshot_at.replace("Z", "+00:00"))
        except (ValueError, TypeError):
            continue
        score = s.get("final_score", 0)
        if ts >= cutoff_recent:
            recent_scores.append(score)
        elif ts >= cutoff_prior_start:
            prior_scores.append(score)

    if len(recent_scores) + len(prior_scores) < 2:
        return {
            "recent_avg": 0.0,
            "prior_avg": 0.0,
            "decline_pct": 0.0,
            "is_declining": False,
            "data_points": len(recent_scores) + len(prior_scores),
        }

    recent_avg = sum(recent_scores) / len(recent_scores) if recent_scores else 0.0
    prior_avg = sum(prior_scores) / len(prior_scores) if prior_scores else 0.0

    if prior_avg > 0:
        decline_pct = round(((prior_avg - recent_avg) / prior_avg) * 100, 1)
    elif recent_avg > 0:
        # No prior data, but recent exists — can't compute decline
        decline_pct = 0.0
    else:
        decline_pct = 0.0

    is_declining = decline_pct >= 40.0

    return {
        "recent_avg": round(recent_avg, 1),
        "prior_avg": round(prior_avg, 1),
        "decline_pct": decline_pct,
        "is_declining": is_declining,
        "data_points": len(recent_scores) + len(prior_scores),
    }


def _extract_productivity_signals(
    ideas: list[dict],
    scores_history: dict[str, list],
) -> tuple[list[BurnoutSignal], list[str]]:
    """Detect productivity decline from score trends across ideas.

    A >=40% score decline in the last 3 weeks triggers a high-severity signal.
    Multiple declining ideas are consolidated into evidence rather than
    generating one signal per idea.
    """
    signals: list[BurnoutSignal] = []
    evidence: list[str] = []

    declining_ideas: list[dict] = []
    for idea in ideas:
        slug = idea.get("slug", "")
        history = scores_history.get(slug, [])
        if len(history) < 2:
            continue
        trend = _compute_score_trend(history)
        if trend["is_declining"]:
            declining_ideas.append({
                "slug": slug,
                "decline_pct": trend["decline_pct"],
                "recent_avg": trend["recent_avg"],
                "prior_avg": trend["prior_avg"],
            })
            evidence.append(
                f"{slug}: {trend['prior_avg']} → {trend['recent_avg']} "
                f"({trend['decline_pct']}% decline over 3 weeks)"
            )

    if declining_ideas:
        max_decline = max(d["decline_pct"] for d in declining_ideas)
        severity = "high" if max_decline >= 60 else ("medium" if max_decline >= 40 else "low")
        signals.append(BurnoutSignal(
            category="productivity",
            signal=f"Score decline detected in {len(declining_ideas)} idea(s), "
                    f"max decline {max_decline}%",
            severity=severity,
            magnitude=min(max_decline / 100.0, 1.0),
            evidence=evidence[:10],
        ))

    return signals, evidence


def _extract_decision_quality_signals(
    decisions: list[dict] | None,
) -> tuple[list[BurnoutSignal], list[str]]:
    """Detect decision quality erosion from decision journal entries.

    - >30% incorrect decisions → high-severity signal
    - >=3 pivot decisions → medium-severity signal (decision instability)
    - High ratio of pending outcomes → medium-severity signal
    """
    signals: list[BurnoutSignal] = []
    evidence: list[str] = []

    if not decisions:
        return signals, evidence

    total = len(decisions)
    incorrect = [d for d in decisions if d.get("was_correct") is False]
    pending = [d for d in decisions if d.get("actual_outcome", "").strip() == ""
                and d.get("was_correct") is None]
    pivots = [d for d in decisions if "pivot" in d.get("decision", "").lower()]

    incorrect_ratio = len(incorrect) / total if total > 0 else 0
    pending_ratio = len(pending) / total if total > 0 else 0

    evidence.append(f"{total} decisions logged, {len(incorrect)} incorrect "
                    f"({round(incorrect_ratio * 100)}%), "
                    f"{len(pending)} pending, {len(pivots)} pivots")

    if incorrect_ratio > 0.3:
        signals.append(BurnoutSignal(
            category="decision_quality",
            signal=f"{round(incorrect_ratio * 100)}% of decisions were incorrect "
                    f"({len(incorrect)}/{total})",
            severity="high",
            magnitude=min(incorrect_ratio, 1.0),
            evidence=[
                f"{len(incorrect)} incorrect out of {total} decisions",
                *[f"- {d.get('decision', '')[:100]}" for d in incorrect[:3]],
            ],
        ))
    elif incorrect_ratio > 0.15:
        signals.append(BurnoutSignal(
            category="decision_quality",
            signal=f"{round(incorrect_ratio * 100)}% incorrect decisions "
                    f"({len(incorrect)}/{total})",
            severity="medium",
            magnitude=incorrect_ratio,
            evidence=[f"{len(incorrect)} incorrect out of {total} decisions"],
        ))

    if len(pivots) >= 3:
        signals.append(BurnoutSignal(
            category="decision_quality",
            signal=f"{len(pivots)} pivot decisions — possible direction instability",
            severity="medium",
            magnitude=min(len(pivots) / 10.0, 1.0),
            evidence=[
                f"{len(pivots)} pivot decisions logged",
                "Frequent pivoting may indicate loss of conviction",
            ],
        ))

    if pending_ratio > 0.5 and total >= 3:
        signals.append(BurnoutSignal(
            category="decision_quality",
            signal=f"{round(pending_ratio * 100)}% of decisions have pending outcomes "
                    f"({len(pending)}/{total})",
            severity="low",
            magnitude=pending_ratio * 0.5,
            evidence=["High pending ratio may indicate decision avoidance"],
        ))

    return signals, evidence


def _extract_ikigai_signals(
    latest_ikigai: dict | None,
) -> tuple[list[BurnoutSignal], list[str]]:
    """Detect ikigai misalignment signals that correlate with burnout risk.

    - High-severity contradictions → check for burnout-related keywords
    - alignment_score < 40 → missing sense of purpose
    - Contradictions stating love/passion vs revealing profit-chasing
    """
    signals: list[BurnoutSignal] = []
    evidence: list[str] = []

    if not latest_ikigai:
        return signals, evidence

    # ── Alignment score ────────────────────────────────────────────
    alignment = latest_ikigai.get("alignment_score", 50)
    evidence.append(f"Ikigai alignment score: {alignment}/100")

    if alignment < 30:
        signals.append(BurnoutSignal(
            category="ikigai",
            signal=f"Critical ikigai misalignment: alignment score is {alignment}/100",
            severity="high",
            magnitude=(100 - alignment) / 100.0,
            evidence=[
                f"Alignment score: {alignment}/100",
                f"Missing quadrant: {latest_ikigai.get('missing_quadrant', 'unknown')}",
            ],
        ))
    elif alignment < 40:
        signals.append(BurnoutSignal(
            category="ikigai",
            signal=f"Low ikigai alignment: {alignment}/100",
            severity="medium",
            magnitude=(100 - alignment) / 100.0,
            evidence=[f"Alignment score: {alignment}/100"],
        ))

    # ── Contradictions ──────────────────────────────────────────────
    contradictions = latest_ikigai.get("contradictions", [])
    detected = latest_ikigai.get("detected_contradictions", [])
    all_contradictions = contradictions + detected

    burnout_keywords = [
        "love", "passion", "energy", "motivat", "burnout", "tired",
        "pursu", "profit", "money", "pay", "value", "meaning",
    ]

    high_sev_contradictions = []
    for c in all_contradictions:
        if not isinstance(c, dict):
            continue
        sev = c.get("severity", "low")
        if sev == "high":
            high_sev_contradictions.append(c)

    # Check if high-severity contradictions mention burnout-relevant keywords
    burnout_contradictions = []
    for c in high_sev_contradictions:
        stated = c.get("stated", "").lower()
        revealed = c.get("revealed", "").lower()
        combined = stated + " " + revealed
        if any(kw in combined for kw in burnout_keywords):
            burnout_contradictions.append(c)

    evidence.append(
        f"{len(high_sev_contradictions)} high-severity contradictions, "
        f"{len(burnout_contradictions)} burnout-relevant"
    )

    if burnout_contradictions:
        signals.append(BurnoutSignal(
            category="ikigai",
            signal=f"{len(burnout_contradictions)} high-severity contradiction(s) "
                    f"indicate value-action misalignment",
            severity="high",
            magnitude=min(len(burnout_contradictions) * 0.3, 1.0),
            evidence=[
                f"Stated: {c.get('stated', '')} | Revealed: {c.get('revealed', '')}"
                for c in burnout_contradictions[:3]
            ],
        ))
    elif high_sev_contradictions:
        signals.append(BurnoutSignal(
            category="ikigai",
            signal=f"{len(high_sev_contradictions)} high-severity contradiction(s) detected",
            severity="medium",
            magnitude=min(len(high_sev_contradictions) * 0.15, 1.0),
            evidence=[
                f"- {c.get('stated', '')[:100]}"
                for c in high_sev_contradictions[:3]
            ],
        ))

    return signals, evidence


def _extract_pursuit_signals(
    ideas: list[dict],
    scores_history: dict[str, list],
) -> tuple[list[BurnoutSignal], list[str], list[str]]:
    """Detect 'pursuit without progress' — active ideas that are stuck.

    An idea is 'stuck' if:
      - Status is 'active' or 'scored'
      - Has >=2 score snapshots over >=30 days
      - Latest score <= first score (no improvement)
    """
    signals: list[BurnoutSignal] = []
    evidence: list[str] = []
    stuck_ideas: list[str] = []

    now = datetime.now(timezone.utc)
    for idea in ideas:
        slug = idea.get("slug", "")
        status = idea.get("status", "")
        if status not in ("active", "scored"):
            continue

        history = scores_history.get(slug, [])
        if len(history) < 2:
            continue

        # Check if idea has been active for >=30 days
        first_snapshot = history[0].get("snapshot_at", "")
        if first_snapshot:
            try:
                first_ts = datetime.fromisoformat(
                    first_snapshot.replace("Z", "+00:00")
                )
                days_active = (now - first_ts).days
                if days_active < 30:
                    continue
            except (ValueError, TypeError):
                continue
        else:
            continue

        first_score = history[0].get("final_score", 0)
        last_score = history[-1].get("final_score", 0)

        if last_score <= first_score + 5:  # Allow 5-point tolerance
            stuck_ideas.append(slug)
            evidence.append(
                f"{slug}: {first_score} → {last_score} over {days_active} days — no progress"
            )

    if stuck_ideas:
        signals.append(BurnoutSignal(
            category="pursuit",
            signal=f"{len(stuck_ideas)} active idea(s) with no score improvement "
                    f"in 30+ days",
            severity="medium",
            magnitude=min(len(stuck_ideas) * 0.2, 1.0),
            evidence=evidence[:8],
        ))

    return signals, evidence, stuck_ideas


def _extract_health_signals(
    store: Any | None,
) -> tuple[list[BurnoutSignal], list[str], list[str]]:
    """Extract health anomaly signals from DailyLifeSystem bridge.

    Currently reads from ~/.aman/lifelight/health_signals.json.
    Returns empty results if the bridge file does not exist (graceful no-op).
    """
    signals: list[BurnoutSignal] = []
    evidence: list[str] = []
    anomalies: list[str] = []

    if store is None:
        return signals, evidence, anomalies

    try:
        health_signals = store.get_health_signals()
    except Exception:
        return signals, evidence, anomalies

    if not health_signals:
        return signals, evidence, anomalies

    # Process health signal entries
    for entry in health_signals:
        if not isinstance(entry, dict):
            continue

        metric = entry.get("metric", "")
        value = entry.get("value", 0)
        threshold = entry.get("threshold", {})

        if metric == "sleep_duration":
            low = threshold.get("low", 6.0)
            if value < low:
                anomalies.append(f"Sleep: {value}h (below {low}h threshold)")
                evidence.append(f"Low sleep duration: {value}h")

        elif metric == "mood":
            low = threshold.get("low", 3)
            if value < low:
                anomalies.append(f"Mood: {value}/5 (below {low} threshold)")
                evidence.append(f"Low mood score: {value}/5")

        elif metric == "energy_level":
            low = threshold.get("low", 3)
            if value < low:
                anomalies.append(f"Energy: {value}/5 (below {low} threshold)")
                evidence.append(f"Low energy level: {value}/5")

        elif metric == "steps":
            low = threshold.get("low", 3000)
            if value < low:
                anomalies.append(f"Steps: {value} (below {low} threshold)")

    if anomalies:
        signals.append(BurnoutSignal(
            category="health",
            signal=f"{len(anomalies)} health metric(s) below threshold",
            severity="medium",
            magnitude=min(len(anomalies) * 0.25, 1.0),
            evidence=evidence[:6],
        ))

    return signals, evidence, anomalies


# ---------------------------------------------------------------------------
# Phase 2: Algorithmic risk scoring
# ---------------------------------------------------------------------------


def _compute_risk_score(
    all_signals: list[BurnoutSignal],
    productivity_decline_pct: float | None,
    stuck_ideas: list[str],
    health_anomalies: list[str],
) -> BurnoutRisk:
    """Aggregate signals into a 0-100 burnout risk score.

    Scoring weights:
      - +20 per high-severity productivity signal, +10 per medium
      - +15 per high-severity decision quality signal
      - +15 per high-severity ikigai signal, +10 per medium
      - +10 per medium-severity pursuit signal
      - +15 per health anomaly signal

    Risk level mapping:
      0-20   → low
      21-50  → moderate
      51-75  → high
      76-100 → critical
    """
    score = 0.0

    for s in all_signals:
        cat = s.category
        sev = s.severity

        if cat == "productivity":
            score += 20 if sev == "high" else (10 if sev == "medium" else 5)
        elif cat == "decision_quality":
            score += 15 if sev == "high" else (8 if sev == "medium" else 3)
        elif cat == "ikigai":
            score += 15 if sev == "high" else (10 if sev == "medium" else 5)
        elif cat == "pursuit":
            score += 10 if sev == "medium" else 5
        elif cat == "health":
            score += 15 if sev == "high" else (10 if sev == "medium" else 5)

    score = min(score, 100.0)

    if score <= 20:
        risk_level = "low"
    elif score <= 50:
        risk_level = "moderate"
    elif score <= 75:
        risk_level = "high"
    else:
        risk_level = "critical"

    # Generate pre-recommendations based on triggered categories
    pre_recommendations: list[str] = []
    categories_triggered = set(s.category for s in all_signals if s.severity in ("high", "medium"))

    if "productivity" in categories_triggered:
        pre_recommendations.append(
            "Consider pausing new evaluations for 1-2 weeks. Focus on executing "
            "your highest-scoring active idea instead of generating more options."
        )
    if "decision_quality" in categories_triggered:
        pre_recommendations.append(
            "Decision quality is declining. Try reducing your decision load: "
            "delegate or defer low-stakes decisions, and use written decision "
            "memos for the ones that remain."
        )
    if "ikigai" in categories_triggered:
        pre_recommendations.append(
            "Your stated values and actual pursuits are diverging. Run an Ikigai "
            "check and look for the intersection of what you love and what pays — "
            "there may be a niche you're overlooking."
        )
    if "pursuit" in categories_triggered:
        pre_recommendations.append(
            f"{len(stuck_ideas)} ideas have shown no progress in 30+ days. "
            "Apply a kill criterion to each: if no score improvement in 2 more weeks, "
            "consider dropping or pivoting."
        )
    if "health" in categories_triggered:
        pre_recommendations.append(
            "Health metrics are below threshold. Prioritize sleep and physical "
            "activity this week — burnout is physiological before it's psychological."
        )

    if not pre_recommendations:
        pre_recommendations.append(
            "No significant risk signals detected. Continue current cadence "
            "but re-check weekly."
        )

    return BurnoutRisk(
        risk_score=score,
        risk_level=risk_level,
        signals=all_signals,
        productivity_decline_pct=productivity_decline_pct,
        pursuit_stuck_ideas=stuck_ideas,
        health_anomalies=health_anomalies,
        recommendations=pre_recommendations,
    )


# ---------------------------------------------------------------------------
# Phase 3: Build enriched context and call LLM
# ---------------------------------------------------------------------------

_MAX_CONTEXT_CHARS = 5000


def _build_burnout_context(
    risk: BurnoutRisk,
    ideas: list[dict],
    decisions: list[dict] | None,
    latest_ikigai: dict | None,
) -> str:
    """Build a compact Markdown context for the LLM synthesis step."""
    parts: list[str] = []

    # ── Section 1: Algorithmic Risk Assessment ─────────────────────
    parts.append("## Algorithmic Risk Assessment\n")
    parts.append(f"- **Risk Score**: {risk.risk_score}/100 ({risk.risk_level})")
    if risk.productivity_decline_pct is not None:
        parts.append(f"- **Productivity Decline**: {risk.productivity_decline_pct}%")
    parts.append(f"- **Ikigai Contradictions**: {risk.ikigai_contradiction_count}")
    parts.append(f"- **Stuck Ideas**: {len(risk.pursuit_stuck_ideas)}")
    parts.append(f"- **Health Anomalies**: {len(risk.health_anomalies)}")
    parts.append("")

    # ── Section 2: Detected Signals ────────────────────────────────
    if risk.signals:
        parts.append("## Detected Burnout Signals\n")
        for i, s in enumerate(risk.signals, 1):
            if isinstance(s, BurnoutSignal):
                parts.append(
                    f"{i}. **[{s.severity.upper()}] [{s.category}]** {s.signal}\n"
                )
                for ev in s.evidence[:3]:
                    parts.append(f"   - {ev}\n")
            else:
                parts.append(f"{i}. {s}\n")
        parts.append("")

    # ── Section 3: Ikigai Context ──────────────────────────────────
    if latest_ikigai:
        parts.append("## Ikigai Context\n")
        parts.append(f"- Alignment: {latest_ikigai.get('alignment_score', '?')}/100")
        parts.append(f"- Dominant: {latest_ikigai.get('dominant_quadrant', '?')}")
        parts.append(f"- Missing: {latest_ikigai.get('missing_quadrant', '?')}")
        trend_text = latest_ikigai.get("trend", "")
        if trend_text:
            parts.append(f"- Trend: {trend_text}")
        parts.append("")

    # ── Section 4: Overview ────────────────────────────────────────
    active_count = sum(1 for i in ideas if i.get("status") in ("active", "scored"))
    parts.append("## Overview\n")
    parts.append(f"- Total ideas: {len(ideas)}")
    parts.append(f"- Active/scored: {active_count}")
    parts.append(f"- Decision entries: {len(decisions) if decisions else 0}")
    if risk.pursuit_stuck_ideas:
        parts.append(f"- Stuck ideas: {', '.join(risk.pursuit_stuck_ideas[:5])}")
    if risk.health_anomalies:
        parts.append(f"- Health anomalies: {', '.join(risk.health_anomalies[:5])}")
    parts.append("")

    # Truncate
    context = "\n".join(parts)
    if len(context) > _MAX_CONTEXT_CHARS:
        context = context[:_MAX_CONTEXT_CHARS] + "\n[... truncated for token budget]"

    return context


# ---------------------------------------------------------------------------
# Main entry point
# ---------------------------------------------------------------------------


def detect_burnout_risk(
    ideas: list[dict],
    scores_history: dict[str, list] | None = None,
    decisions: list[dict] | None = None,
    latest_ikigai: dict | None = None,
    store: Any | None = None,
    llm: LlmClient | None = None,
) -> dict:
    """Run a multi-signal burnout risk assessment.

    Args:
        ideas: List of idea records from the store.
        scores_history: {slug: [score_snapshots]} time-series.
        decisions: Decision journal entries from the store.
        latest_ikigai: Most recent ikigai check result.
        store: StartupStore instance (for health signal bridge).
        llm: LLM client (uses default if None).

    Returns:
        BurnoutRisk-compatible dict with algorithmic + LLM fields.
    """
    client = llm or LlmClient()
    scores_history = scores_history or {}
    decisions = decisions or []

    # ── Phase 1: Extract signals ───────────────────────────────────
    prod_signals, prod_evidence = _extract_productivity_signals(
        ideas, scores_history
    )
    dec_signals, dec_evidence = _extract_decision_quality_signals(decisions)
    iki_signals, iki_evidence = _extract_ikigai_signals(latest_ikigai)
    pursuit_signals, pursuit_evidence, stuck_ideas = _extract_pursuit_signals(
        ideas, scores_history
    )
    health_signals, health_evidence, health_anomalies = _extract_health_signals(store)

    all_signals = (
        prod_signals + dec_signals + iki_signals + pursuit_signals + health_signals
    )

    # ── Phase 2: Algorithmic risk scoring ──────────────────────────
    # Compute overall productivity decline across all ideas
    overall_decline: float | None = None
    max_decline = 0.0
    for idea in ideas:
        slug = idea.get("slug", "")
        history = scores_history.get(slug, [])
        trend = _compute_score_trend(history)
        if trend["is_declining"] and trend["decline_pct"] > max_decline:
            max_decline = trend["decline_pct"]
    if max_decline > 0:
        overall_decline = max_decline

    risk = _compute_risk_score(
        all_signals,
        overall_decline,
        stuck_ideas,
        health_anomalies,
    )

    # Update with counts from extraction
    risk.ikigai_contradiction_count = len([
        c for c in (latest_ikigai or {}).get("contradictions", [])
        if isinstance(c, dict) and c.get("severity") == "high"
    ])

    # Determine decision quality trend
    total_dec = len(decisions)
    if total_dec > 0:
        incorrect = sum(1 for d in decisions if d.get("was_correct") is False)
        ratio = incorrect / total_dec
        if ratio > 0.3:
            risk.decision_quality_trend = "declining"
        elif ratio > 0.15:
            risk.decision_quality_trend = "mixed"
        else:
            risk.decision_quality_trend = "stable"
    else:
        risk.decision_quality_trend = "insufficient_data"

    # ── Phase 3: LLM synthesis ─────────────────────────────────────
    try:
        system_prompt = _load("burnout_early_warning.md")
        context = _build_burnout_context(risk, ideas, decisions, latest_ikigai)
        llm_result = client.chat_json(
            system_prompt, context, temperature=0.3, max_tokens=4000
        )
    except Exception:
        llm_result = None

    # ── Merge algorithmic + LLM results ────────────────────────────
    result = risk.to_dict()

    if llm_result:
        result["interpretation"] = llm_result.get("interpretation", "")
        result["burnout_narrative"] = llm_result.get("burnout_narrative", "")
        result["risk_factors"] = llm_result.get("risk_factors", [])
        result["protective_factors"] = llm_result.get("protective_factors", [])
        # LLM recommendations take priority, algorithmic as fallback
        llm_recs = llm_result.get("recommendations", [])
        if llm_recs:
            result["recommendations"] = llm_recs
    else:
        result["interpretation"] = (
            f"Algorithmic assessment: {risk.risk_level.upper()} risk "
            f"(score: {risk.risk_score}/100). {len(all_signals)} signal(s) detected."
        )
        result["burnout_narrative"] = ""
        result["risk_factors"] = [
            s.signal if isinstance(s, BurnoutSignal) else str(s)
            for s in all_signals
        ]
        result["protective_factors"] = []

    # Attach profile data for storage and debugging
    result["profile"] = {
        "signals_summary": [
            {
                "category": s.category,
                "severity": s.severity,
                "signal": s.signal,
            }
            for s in all_signals
            if isinstance(s, BurnoutSignal)
        ],
        "extraction_evidence": {
            "productivity": prod_evidence,
            "decision_quality": dec_evidence,
            "ikigai": iki_evidence,
            "pursuit": pursuit_evidence,
            "health": health_evidence,
        },
    }
    result["ideas_analyzed"] = len(ideas)

    return result
