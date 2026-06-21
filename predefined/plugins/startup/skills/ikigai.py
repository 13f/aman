#!/usr/bin/env python3
"""Ikigai Alignment Check — four-circle Venn analysis of founder's work.

Architecture (v2 — data-driven):
  1. Data extraction  — pull structured signals from store for each circle
  2. Pre-analysis     — algorithmic contradiction detection, trend analysis
  3. LLM synthesis    — nuanced interpretation + actionable recommendation

The four circles:
  - what_you_love         ← idea descriptions, decision patterns, pursuit choices
  - what_you_are_good_at  ← founder_fit scores, complexity, improvement trends
  - what_world_needs      ← positioning gaps, trend velocity, demand signals
  - what_you_can_be_paid_for ← monetization scores, market validation, verdicts

Data sources (all from StartupStore):
  - idea records           — slug, description, verdict, status
  - score_snapshot records — dimension_scores, final_score, verdict over time
  - competitor_analysis    — positioning_gaps, market_saturation
  - market_insight         — trend_velocity, top_signals, monetization_evidence
  - decision_entry         — decisions, assumptions, outcomes
  - previous ikigai_check  — trend comparison
"""

from __future__ import annotations

import json
import os
from collections import Counter
from dataclasses import dataclass, field
from typing import Any, Optional

from llm import LlmClient

_PROMPT_DIR = os.path.join(os.path.dirname(os.path.dirname(__file__)), "prompts")


def _load(name: str) -> str:
    return open(os.path.join(_PROMPT_DIR, name)).read()


# ---------------------------------------------------------------------------
# Data types
# ---------------------------------------------------------------------------


@dataclass
class IkigaiProfile:
    """Signals extracted from real data for each Ikigai circle.

    These are NOT LLM-generated — they are computed from store data.
    The LLM receives this profile as structured context for synthesis.
    """

    what_you_love: list[str] = field(default_factory=list)
    what_you_are_good_at: list[str] = field(default_factory=list)
    what_world_needs: list[str] = field(default_factory=list)
    what_you_can_be_paid_for: list[str] = field(default_factory=list)

    # Per-signal evidence (so the LLM can trace back to source data)
    love_evidence: list[str] = field(default_factory=list)
    skill_evidence: list[str] = field(default_factory=list)
    need_evidence: list[str] = field(default_factory=list)
    paid_evidence: list[str] = field(default_factory=list)

    # Domain clustering: {domain: [slug, ...]}
    domain_clusters: dict[str, list[str]] = field(default_factory=dict)


@dataclass
class DetectedContradiction:
    stated: str
    revealed: str
    severity: str  # high | medium | low
    evidence: list[str] = field(default_factory=list)


@dataclass
class IkigaiCheck:
    """Full ikigai analysis result — profile + LLM synthesis."""

    alignment_score: float
    quadrant_scores: dict[str, float]
    dominant_quadrant: str
    missing_quadrant: str
    overlapping_quadrants: list[str]
    contradictions: list[dict]
    trend: str
    recommendation: str
    ikigai_summary: str
    # v2 additions
    profile: IkigaiProfile | None = None
    detected_contradictions: list[dict] | None = None
    ideas_analyzed: int = 0


# ---------------------------------------------------------------------------
# Phase 1: Data extraction — compute signals from store data
# ---------------------------------------------------------------------------


def _domain_from_slug(slug: str, description: str = "") -> str:
    """Infer a domain label from the idea slug/description.

    Uses keyword matching to cluster ideas into domains.
    """
    text = (slug + " " + description).lower()
    domains = {
        "health-fitness": ["health", "fitness", "wellness", "sleep", "diet", "exercise",
                           "mental health", "meditation", "yoga", "workout"],
        "finance": ["finance", "fintech", "invest", "bank", "payment", "crypto",
                    "trading", "wealth", "budget", "expense", "tax"],
        "education": ["education", "learning", "edtech", "course", "teach", "study",
                      "skill", "tutor", "curriculum", "knowledge"],
        "productivity": ["productivity", "task", "todo", "calendar", "note", "focus",
                         "time management", "organize", "plan", "habit"],
        "social-community": ["social", "community", "chat", "network", "dating", "friend",
                             "group", "collaborate", "share", "connect"],
        "creator-media": ["creator", "content", "media", "podcast", "video", "write",
                          "blog", "newsletter", "stream", "publish"],
        "dev-tools": ["dev", "developer", "api", "code", "programming", "tool", "cli",
                      "sdk", "open source", "infra", "cloud", "deploy"],
        "marketplace": ["marketplace", "market", "buy", "sell", "commerce", "shop",
                        "store", "vendor", "subscription", "saas"],
        "ai-automation": ["ai", "ml", "automation", "gpt", "llm", "model", "agent",
                          "chatbot", "generate", "assistant"],
    }
    for domain, keywords in domains.items():
        if any(kw in text for kw in keywords):
            return domain
    return "other"


def _extract_love_signals(
    ideas: list[dict],
    decisions: list[dict] | None,
) -> tuple[list[str], list[str]]:
    """Extract what the founder LOVES from idea descriptions and decisions.

    Signals:
      - Recurring themes in idea descriptions
      - Ideas the founder chose to pursue (verdict=pursue/test) vs drop
      - Decision entries — what they chose to invest in
      - High-desire but low-monetization ideas (passion over profit)
    """
    signals: list[str] = []
    evidence: list[str] = []

    # ── Theme clustering from idea descriptions ───────────────────
    domain_counter: Counter = Counter()
    for idea in ideas:
        slug = idea.get("slug", "")
        desc = idea.get("description", "")
        domain = _domain_from_slug(slug, desc)
        domain_counter[domain] += 1

    top_domains = domain_counter.most_common(3)
    for domain, count in top_domains:
        signals.append(domain.replace("-", " ").title())
        evidence.append(f"{count} ideas in {domain}")

    # ── Revealed preference: pursue vs drop ───────────────────────
    pursued_slugs = []
    dropped_slugs = []
    for idea in ideas:
        verdict = idea.get("verdict", "")
        status = idea.get("status", "")
        slug = idea.get("slug", "")
        if verdict in ("pursue", "test") or status in ("active", "scored"):
            pursued_slugs.append(slug)
        elif verdict == "drop" or status == "dropped":
            dropped_slugs.append(slug)

    if pursued_slugs:
        pursued_domains = Counter(
            _domain_from_slug(s) for s in pursued_slugs
        )
        top_pursued = pursued_domains.most_common(2)
        for domain, count in top_pursued:
            label = domain.replace("-", " ").title()
            if label not in signals:
                signals.append(label)
            evidence.append(f"Chose to pursue {count} ideas in {domain}")

    if dropped_slugs:
        dropped_domains = Counter(
            _domain_from_slug(s) for s in dropped_slugs
        )
        top_dropped = dropped_domains.most_common(2)
        for domain, count in top_dropped:
            evidence.append(f"Dropped {count} ideas in {domain}")

    # ── Decision patterns ─────────────────────────────────────────
    if decisions:
        decision_themes = []
        for d in decisions:
            decision_text = d.get("decision", "")
            if decision_text:
                decision_themes.append(decision_text.lower())
        if decision_themes:
            evidence.append(f"{len(decision_themes)} decisions logged")

    # ── Passion-over-profit ideas ─────────────────────────────────
    passion_ideas = []
    for idea in ideas:
        slug = idea.get("slug", "")
        dims = idea.get("_dimension_scores", {})
        demand = dims.get("demand", 0)
        monetization = dims.get("monetization", 0)
        if demand >= 60 and monetization < 40:
            passion_ideas.append(slug)
    if passion_ideas:
        evidence.append(
            f"{len(passion_ideas)} high-demand/low-monetization ideas "
            f"(passion over profit): {', '.join(passion_ideas[:3])}"
        )

    return signals, evidence


def _extract_skill_signals(
    ideas: list[dict],
    scores_history: dict[str, list],
    competitor_data: dict[str, dict] | None,
) -> tuple[list[str], list[str]]:
    """Extract what the founder is GOOD AT from scores and analysis depth.

    Signals:
      - High founder_fit scores (the scoring dimension most aligned to skill)
      - Low complexity assessments
      - Score improvement trends (learning over time)
      - Thorough competitor analysis (depth = domain knowledge)
    """
    signals: list[str] = []
    evidence: list[str] = []

    # ── High founder_fit ideas ────────────────────────────────────
    high_fit_domains: Counter = Counter()
    for idea in ideas:
        slug = idea.get("slug", "")
        dims = idea.get("_dimension_scores", {})
        founder_fit = dims.get("founder_fit", 0)
        if founder_fit >= 60:
            domain = _domain_from_slug(slug, idea.get("description", ""))
            high_fit_domains[domain] += 1

    for domain, count in high_fit_domains.most_common(3):
        label = domain.replace("-", " ").title()
        signals.append(label)
        evidence.append(f"High founder_fit (≥60) in {count} {domain} ideas")

    # ── Low complexity ideas ──────────────────────────────────────
    low_complexity_domains: Counter = Counter()
    for idea in ideas:
        slug = idea.get("slug", "")
        dims = idea.get("_dimension_scores", {})
        # Lower competition + higher founder_fit suggests domain skill
        competition = dims.get("competition", 50)
        founder_fit = dims.get("founder_fit", 50)
        if founder_fit >= 60 and competition <= 50:
            domain = _domain_from_slug(slug, idea.get("description", ""))
            low_complexity_domains[domain] += 1

    # ── Score improvement trends ──────────────────────────────────
    improving_domains = []
    for slug, history in (scores_history or {}).items():
        if len(history) < 2:
            continue
        scores = [s.get("final_score", 0) for s in history]
        if scores[-1] > scores[0] + 10:  # improved by 10+ points
            domain = _domain_from_slug(slug)
            improving_domains.append(domain)

    if improving_domains:
        improved = Counter(improving_domains).most_common(2)
        for domain, count in improved:
            evidence.append(
                f"Score improved ≥10pts across {count} re-validations in {domain}"
            )

    # ── Competitor analysis depth ─────────────────────────────────
    if competitor_data:
        for slug, comp in competitor_data.items():
            direct_count = len(comp.get("direct_competitors", []))
            gap_count = len(comp.get("positioning_gaps", []))
            if direct_count >= 4 and gap_count >= 2:
                domain = _domain_from_slug(slug)
                evidence.append(
                    f"Deep competitor analysis in {domain} "
                    f"({direct_count} competitors, {gap_count} gaps found)"
                )

    return signals, evidence


def _extract_need_signals(
    ideas: list[dict],
    competitor_data: dict[str, dict] | None,
    market_insights: list[dict] | None,
) -> tuple[list[str], list[str]]:
    """Extract what the WORLD NEEDS from market data.

    Signals:
      - Positioning gaps from competitor analysis (unmet needs)
      - Market insights with rising trend velocity
      - High demand scores across ideas
      - Monetization evidence in market insights
    """
    signals: list[str] = []
    evidence: list[str] = []

    # ── Positioning gaps ──────────────────────────────────────────
    if competitor_data:
        all_gaps: Counter = Counter()
        for slug, comp in competitor_data.items():
            for gap in comp.get("positioning_gaps", []):
                if isinstance(gap, str):
                    all_gaps[gap.lower()] += 1
                elif isinstance(gap, dict):
                    gap_text = gap.get("gap", gap.get("description", str(gap)))
                    all_gaps[gap_text.lower()] += 1

        for gap_text, count in all_gaps.most_common(5):
            signals.append(gap_text[:80])
            evidence.append(f"Gap found in {count} analyses: {gap_text[:100]}")

    # ── Market trend velocity ─────────────────────────────────────
    if market_insights:
        rising_insights = [
            m for m in market_insights
            if m.get("trend_velocity") in ("rising", "surging")
        ]
        if rising_insights:
            niches = Counter(m.get("niche", "unknown") for m in rising_insights)
            for niche, count in niches.most_common(3):
                signals.append(f"Growing market: {niche}")
                evidence.append(
                    f"Trend velocity 'rising/surging' in {niche} "
                    f"({count} insights)"
                )

        # Monetization evidence
        monetizable = [
            m for m in market_insights
            if m.get("monetization_evidence") is True
        ]
        if monetizable:
            evidence.append(
                f"Monetization evidence found in "
                f"{len(monetizable)} market insights"
            )

    # ── High demand scores ────────────────────────────────────────
    high_demand_ideas = []
    for idea in ideas:
        slug = idea.get("slug", "")
        dims = idea.get("_dimension_scores", {})
        if dims.get("demand", 0) >= 70:
            high_demand_ideas.append(slug)

    if high_demand_ideas:
        evidence.append(
            f"{len(high_demand_ideas)} ideas with high demand scores (≥70): "
            f"{', '.join(high_demand_ideas[:3])}"
        )

    return signals, evidence


def _extract_paid_signals(
    ideas: list[dict],
    scores_history: dict[str, list],
    market_insights: list[dict] | None,
) -> tuple[list[str], list[str]]:
    """Extract what the founder CAN BE PAID FOR from monetization data.

    Signals:
      - High monetization dimension scores
      - Ideas with "pursue" verdict (validated business models)
      - Market insights with monetization evidence
      - High distribution + retention scores (sustainable revenue)
    """
    signals: list[str] = []
    evidence: list[str] = []

    # ── High monetization scores ──────────────────────────────────
    high_mon_domains: Counter = Counter()
    for idea in ideas:
        slug = idea.get("slug", "")
        dims = idea.get("_dimension_scores", {})
        monetization = dims.get("monetization", 0)
        distribution = dims.get("distribution", 0)
        retention = dims.get("retention", 0)
        # Monetization + distribution + retention = revenue sustainability
        revenue_score = (monetization + distribution + retention) / 3
        if revenue_score >= 55:
            domain = _domain_from_slug(slug, idea.get("description", ""))
            high_mon_domains[domain] += 1

    for domain, count in high_mon_domains.most_common(3):
        label = domain.replace("-", " ").title()
        signals.append(label)
        evidence.append(
            f"Sustainable revenue potential (mon+dist+ret ≥55) in "
            f"{count} {domain} ideas"
        )

    # ── Validated business models (pursue verdict) ────────────────
    pursue_ideas = []
    for idea in ideas:
        verdict = idea.get("verdict", "")
        if verdict == "pursue":
            pursue_ideas.append(idea.get("slug", ""))

    if pursue_ideas:
        pursue_domains = Counter(
            _domain_from_slug(s) for s in pursue_ideas
        )
        for domain, count in pursue_domains.most_common(2):
            evidence.append(
                f"Validated business model in {domain}: "
                f"{count} ideas with 'pursue' verdict"
            )

    # ── Monetization evidence from market ─────────────────────────
    if market_insights:
        paid_niches = [
            m.get("niche", "") for m in market_insights
            if m.get("monetization_evidence") is True
        ]
        if paid_niches:
            for niche in Counter(paid_niches).most_common(2):
                signals.append(f"Paying market: {niche[0]}")
                evidence.append(
                    f"Market shows willingness to pay in {niche[0]}"
                )

    # ── High scoring ideas ────────────────────────────────────────
    high_scorers = []
    for idea in ideas:
        score = idea.get("final_score") or 0
        if score >= 65:
            high_scorers.append(idea.get("slug", ""))

    if high_scorers:
        evidence.append(
            f"{len(high_scorers)} ideas scored ≥65/100 overall"
        )

    return signals, evidence


# ---------------------------------------------------------------------------
# Phase 2: Algorithmic contradiction detection
# ---------------------------------------------------------------------------


def detect_contradictions(
    profile: IkigaiProfile,
    ideas: list[dict],
    decisions: list[dict] | None,
) -> list[DetectedContradiction]:
    """Detect contradictions between stated values and revealed preferences.

    These are algorithmic signals — the LLM may find additional nuances.
    """
    contradictions: list[DetectedContradiction] = []

    love_set = set(s.lower() for s in profile.what_you_love)
    paid_set = set(s.lower() for s in profile.what_you_can_be_paid_for)
    good_set = set(s.lower() for s in profile.what_you_are_good_at)

    # ── Contradiction 1: Pursuing what pays but not what you love ──
    if paid_set and love_set and not (paid_set & love_set):
        contradictions.append(DetectedContradiction(
            stated=f"Values {', '.join(list(love_set)[:3])}",
            revealed=f"Only pursues ideas that pay in {', '.join(list(paid_set)[:3])}",
            severity="high",
            evidence=[
                f"What you love ({', '.join(list(love_set)[:3])}) has zero overlap "
                f"with what pays ({', '.join(list(paid_set)[:3])})"
            ],
        ))

    # ── Contradiction 2: Good at something but not pursuing it ─────
    if good_set and paid_set:
        unmonetized_skills = good_set - paid_set
        if unmonetized_skills:
            contradictions.append(DetectedContradiction(
                stated=f"Skilled in {', '.join(list(good_set)[:3])}",
                revealed=f"Not monetizing skills in {', '.join(list(unmonetized_skills)[:3])}",
                severity="medium",
                evidence=[
                    f"Founder is good at {', '.join(list(unmonetized_skills)[:3])} "
                    f"but has no validated revenue model there"
                ],
            ))

    # ── Contradiction 3: Pivot direction vs stated values ──────────
    if decisions:
        pivot_decisions = [
            d for d in decisions
            if "pivot" in d.get("decision", "").lower()
        ]
        if pivot_decisions and love_set:
            # Check if pivots moved toward or away from love
            contradictions.append(DetectedContradiction(
                stated=f"Values {', '.join(list(love_set)[:2])}",
                revealed=f"{len(pivot_decisions)} pivot decisions — direction unclear",
                severity="low",
                evidence=[
                    "Pivot direction should be checked against stated values",
                    f"{len(pivot_decisions)} pivots logged",
                ],
            ))

    # ── Contradiction 4: Drop pattern ──────────────────────────────
    dropped_slugs = [
        idea.get("slug", "") for idea in ideas
        if idea.get("verdict") == "drop" or idea.get("status") == "dropped"
    ]
    pursued_slugs = [
        idea.get("slug", "") for idea in ideas
        if idea.get("verdict") in ("pursue", "test")
    ]

    if dropped_slugs and pursued_slugs:
        dropped_domains = set(
            _domain_from_slug(s) for s in dropped_slugs
        )
        pursued_domains = set(
            _domain_from_slug(s) for s in pursued_slugs
        )
        # If you drop what you love and pursue what pays
        dropped_love = dropped_domains & love_set
        pursued_paid = pursued_domains & paid_set
        if dropped_love and pursued_paid and not (dropped_domains & pursued_domains):
            contradictions.append(DetectedContradiction(
                stated=f"Interested in {', '.join(list(love_set)[:2])}",
                revealed=(
                    f"Dropped ideas in {', '.join(list(dropped_love)[:2])} "
                    f"while pursuing {', '.join(list(pursued_paid)[:2])}"
                ),
                severity="high",
                evidence=[
                    f"Dropped: {', '.join(dropped_slugs[:3])}",
                    f"Pursued: {', '.join(pursued_slugs[:3])}",
                ],
            ))

    return contradictions


# ---------------------------------------------------------------------------
# Phase 3: Build enriched LLM context
# ---------------------------------------------------------------------------

# Maximum context tokens (rough estimate: 1 token ≈ 4 chars)
_MAX_CONTEXT_CHARS = 6000


def _build_enriched_context(
    ideas: list[dict],
    scores_history: dict[str, list],
    profile: IkigaiProfile,
    contradictions: list[DetectedContradiction],
    market_insights: list[dict] | None,
    decisions: list[dict] | None,
) -> str:
    """Build a structured, data-rich context for the LLM.

    Instead of dumping raw JSON and asking the LLM to figure everything out,
    we provide pre-extracted signals + the raw data for verification.
    """
    parts: list[str] = []

    # ── Section 1: Executive Summary (pre-extracted signals) ───────
    parts.append("## Pre-Extracted Ikigai Profile\n")

    parts.append("### What You LOVE (from idea themes + pursuit choices)")
    for s in profile.what_you_love:
        parts.append(f"- {s}")
    for e in profile.love_evidence:
        parts.append(f"  *Evidence*: {e}")
    parts.append("")

    parts.append("### What You're GOOD AT (from founder_fit + score trends)")
    for s in profile.what_you_are_good_at:
        parts.append(f"- {s}")
    for e in profile.skill_evidence:
        parts.append(f"  *Evidence*: {e}")
    parts.append("")

    parts.append("### What the World NEEDS (from positioning gaps + trends)")
    for s in profile.what_world_needs[:8]:
        parts.append(f"- {s}")
    for e in profile.need_evidence[:6]:
        parts.append(f"  *Evidence*: {e}")
    parts.append("")

    parts.append("### What You Can Be PAID FOR (from monetization + verdicts)")
    for s in profile.what_you_can_be_paid_for:
        parts.append(f"- {s}")
    for e in profile.paid_evidence:
        parts.append(f"  *Evidence*: {e}")
    parts.append("")

    # ── Section 2: Detected Contradictions ─────────────────────────
    if contradictions:
        parts.append("## Algorithmically Detected Contradictions\n")
        for i, c in enumerate(contradictions, 1):
            parts.append(
                f"{i}. **Stated**: {c.stated}\n"
                f"   **Revealed**: {c.revealed}\n"
                f"   **Severity**: {c.severity}\n"
            )
            for ev in c.evidence:
                parts.append(f"   - {ev}")
            parts.append("")

    # ── Section 3: Raw Data (for LLM verification) ─────────────────
    parts.append("## Raw Data\n")

    # Ideas summary — compact
    parts.append(f"### Ideas Analyzed ({len(ideas)} total)\n")
    idea_summaries = []
    for idea in ideas:
        dims = idea.get("_dimension_scores", {})
        summary = (
            f"- **{idea.get('slug', '?')}**: verdict={idea.get('verdict', '?')}, "
            f"score={idea.get('final_score', '?')}, "
            f"dims: demand={dims.get('demand', '?')}, "
            f"monetization={dims.get('monetization', '?')}, "
            f"founder_fit={dims.get('founder_fit', '?')}, "
            f"competition={dims.get('competition', '?')}"
        )
        idea_summaries.append(summary)
    parts.append("\n".join(idea_summaries))
    parts.append("")

    # Score history — compact
    if scores_history:
        parts.append(f"### Score History ({len(scores_history)} ideas with time-series)\n")
        for slug, history in scores_history.items():
            if len(history) >= 2:
                first = history[0].get("final_score", 0)
                last = history[-1].get("final_score", 0)
                delta = last - first
                direction = "↑" if delta > 0 else ("↓" if delta < 0 else "→")
                parts.append(f"- {slug}: {first} → {last} ({direction}{abs(delta)})")
        parts.append("")

    # Market insights — compact
    if market_insights:
        parts.append(f"### Market Insights ({len(market_insights)} records)\n")
        for m in market_insights[:5]:
            parts.append(
                f"- {m.get('niche', '?')}/{m.get('platform', '?')}: "
                f"velocity={m.get('trend_velocity', '?')}, "
                f"monetization={m.get('monetization_evidence', False)}"
            )
        parts.append("")

    # Decisions — compact
    if decisions:
        parts.append(f"### Decision Journal ({len(decisions)} entries)\n")
        for d in decisions[-5:]:
            parts.append(
                f"- {d.get('decision', '')[:120]}\n"
                f"  outcome: {d.get('actual_outcome') or 'pending'}, "
                f"correct: {d.get('was_correct', 'unknown')}"
            )
        parts.append("")

    # Truncate to stay within token budget
    context = "\n".join(parts)
    if len(context) > _MAX_CONTEXT_CHARS:
        # Keep the pre-extracted profile, trim raw data proportionally
        profile_end = context.find("## Raw Data")
        if profile_end > 0:
            profile_section = context[:profile_end]
            raw_section = context[profile_end:]
            available = _MAX_CONTEXT_CHARS - len(profile_section) - 100
            if available > 500:
                raw_section = raw_section[:available] + "\n[... truncated for token budget]"
            context = profile_section + raw_section
        else:
            context = context[:_MAX_CONTEXT_CHARS] + "\n[... truncated]"

    return context


# ---------------------------------------------------------------------------
# Main entry point
# ---------------------------------------------------------------------------


def check_ikigai(
    ideas: list[dict],
    scores_history: dict[str, list] | None = None,
    llm: LlmClient | None = None,
    *,
    # v2: rich data sources
    competitor_data: dict[str, dict] | None = None,
    market_insights: list[dict] | None = None,
    decisions: list[dict] | None = None,
) -> dict:
    """Run a data-driven Ikigai alignment check.

    Args:
        ideas: List of idea records (must include _dimension_scores if available).
        scores_history: {slug: [score_snapshots]} time-series.
        llm: LLM client (uses default if None).
        competitor_data: {slug: competitor_analysis} for positioning gaps.
        market_insights: Market trend records from store.
        decisions: Decision journal entries from store.

    Returns:
        IkigaiCheck-compatible dict with profile and detected contradictions.
    """
    client = llm or LlmClient()
    scores_history = scores_history or {}

    # ── Step 1: Enrich ideas with dimension_scores from history ────
    for idea in ideas:
        slug = idea.get("slug", "")
        if "_dimension_scores" not in idea and slug in scores_history:
            history = scores_history[slug]
            if history:
                idea["_dimension_scores"] = history[-1].get(
                    "dimension_scores", {}
                )
        if "_dimension_scores" not in idea:
            idea["_dimension_scores"] = {}

    # ── Step 2: Extract structured signals for each circle ─────────
    love_signals, love_evidence = _extract_love_signals(ideas, decisions)
    skill_signals, skill_evidence = _extract_skill_signals(
        ideas, scores_history, competitor_data
    )
    need_signals, need_evidence = _extract_need_signals(
        ideas, competitor_data, market_insights
    )
    paid_signals, paid_evidence = _extract_paid_signals(
        ideas, scores_history, market_insights
    )

    # Build domain clusters
    domain_clusters: dict[str, list[str]] = {}
    for idea in ideas:
        slug = idea.get("slug", "")
        domain = _domain_from_slug(slug, idea.get("description", ""))
        domain_clusters.setdefault(domain, []).append(slug)

    profile = IkigaiProfile(
        what_you_love=love_signals,
        what_you_are_good_at=skill_signals,
        what_world_needs=need_signals,
        what_you_can_be_paid_for=paid_signals,
        love_evidence=love_evidence,
        skill_evidence=skill_evidence,
        need_evidence=need_evidence,
        paid_evidence=paid_evidence,
        domain_clusters=domain_clusters,
    )

    # ── Step 3: Algorithmic contradiction detection ────────────────
    detected = detect_contradictions(profile, ideas, decisions)
    detected_dicts = [
        {
            "stated": c.stated,
            "revealed": c.revealed,
            "severity": c.severity,
            "evidence": c.evidence,
        }
        for c in detected
    ]

    # ── Step 4: Build enriched context ─────────────────────────────
    context = _build_enriched_context(
        ideas, scores_history, profile, detected,
        market_insights, decisions,
    )

    # ── Step 5: LLM synthesis ──────────────────────────────────────
    system_prompt = _load("ikigai.md")
    result = client.chat_json(system_prompt, context, temperature=0.4)

    # ── Step 6: Merge algorithmic findings with LLM synthesis ──────
    result.setdefault("ideas_analyzed", len(ideas))

    # Merge algorithmic contradictions with LLM-found ones
    llm_contradictions = result.get("contradictions", [])
    if detected_dicts:
        # Prefix algorithmic contradictions so they appear first
        result["contradictions"] = detected_dicts + llm_contradictions

    # Attach profile for storage / debugging
    result["profile"] = {
        "what_you_love": profile.what_you_love,
        "what_you_are_good_at": profile.what_you_are_good_at,
        "what_world_needs": profile.what_world_needs[:8],
        "what_you_can_be_paid_for": profile.what_you_can_be_paid_for,
        "domain_clusters": {
            k: v for k, v in profile.domain_clusters.items()
        },
    }
    result["detected_contradictions"] = detected_dicts

    return result
