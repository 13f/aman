#!/usr/bin/env python3
"""Idea scoring — multiplicative-floor algorithm + RAT design.

Reference: idea-validation-agents skills/idea-scoring/SKILL.md
"""

from dataclasses import dataclass, field
from enum import Enum

TOTAL_DIMENSIONS = 7

DEFAULT_WEIGHTS = {
    "demand":        0.20,
    "competition":   0.10,
    "monetization":  0.20,
    "distribution":  0.20,
    "retention":     0.15,
    "founder_fit":   0.15,
}


class Verdict(str, Enum):
    PURSUE = "pursue"
    TEST = "test"
    PIVOT = "pivot"
    DROP = "drop"


class Confidence(str, Enum):
    HIGH = "high"
    MEDIUM = "medium"
    LOW = "low"


class AssumptionCategory(str, Enum):
    DEMAND = "demand"
    MONETIZATION = "monetization"
    DISTRIBUTION = "distribution"
    RETENTION = "retention"
    TECHNICAL = "technical"
    MARKET = "market"


@dataclass
class ScoreResult:
    base_score: float
    floor_penalty: float
    missing_discount: float
    final_score: int
    verdict: Verdict
    confidence: Confidence
    killer_dimensions: list[str] = field(default_factory=list)
    dimension_scores: dict[str, float] = field(default_factory=dict)
    weights_applied: dict[str, float] = field(default_factory=dict)


@dataclass
class RatExperiment:
    assumption: str
    category: AssumptionCategory
    criticality: int          # 1-5
    uncertainty: int          # 1-5
    rat_score: int            # criticality × uncertainty
    experiment_type: str
    description: str
    duration_days: int        # ≤ 14
    estimated_cost_usd: int   # ≤ 100
    pass_threshold: str
    fail_action: str


# ---------------------------------------------------------------------------
# Multiplicative-floor scoring
# ---------------------------------------------------------------------------


def score_idea(
    dimensions: dict[str, float],
    weights: dict[str, float] | None = None,
) -> ScoreResult:
    """Score an idea using the multiplicative-floor algorithm.

    Args:
        dimensions: {"demand": 80.0, "competition": 45.0, ...}
        weights:   {"demand": 0.20, "competition": 0.10, ...}

    Returns:
        ScoreResult with final_score (0-100) and verdict.
    """
    w = weights or DEFAULT_WEIGHTS

    # Step 1: dimension sub-scores are provided by each analysis skill

    # Step 2: Floor penalty — any dimension < 25 triggers multiplicative penalty
    floor_penalty = 1.0
    killer_dimensions = []
    for dim, score in dimensions.items():
        if score < 25.0:
            floor_penalty *= score / 25.0
            killer_dimensions.append(dim)

    # Step 3: Weighted base score
    base_score = sum(
        score * w.get(dim, 0.0)
        for dim, score in dimensions.items()
    )

    # Step 4: Apply penalties
    missing_discount = len(dimensions) / TOTAL_DIMENSIONS
    adjusted = base_score * floor_penalty * missing_discount
    final_score = round(max(0.0, min(100.0, adjusted)))

    # Step 5: Confidence
    n = len(dimensions)
    if n >= 6:
        confidence = Confidence.HIGH
    elif n >= 4:
        confidence = Confidence.MEDIUM
    else:
        confidence = Confidence.LOW

    # Step 6: Verdict
    if final_score >= 75:
        verdict = Verdict.PURSUE
    elif final_score >= 55:
        verdict = Verdict.TEST
    elif final_score >= 35:
        verdict = Verdict.PIVOT
    else:
        verdict = Verdict.DROP

    return ScoreResult(
        base_score=base_score,
        floor_penalty=floor_penalty,
        missing_discount=missing_discount,
        final_score=final_score,
        verdict=verdict,
        confidence=confidence,
        killer_dimensions=killer_dimensions,
        dimension_scores=dimensions,
        weights_applied=w,
    )


# ---------------------------------------------------------------------------
# RAT (Riskiest Assumption Test) design
# ---------------------------------------------------------------------------

EXPERIMENT_TEMPLATES = {
    AssumptionCategory.DEMAND: {
        "type": "landing_page_waitlist",
        "description_template": "Build a landing page for {idea_name}. Drive traffic via {channel}.",
        "duration_days": 14,
        "estimated_cost_usd": 50,
        "pass_threshold": "≥ 10% email signup from ≥ 100 visitors",
    },
    AssumptionCategory.MONETIZATION: {
        "type": "wizard_of_oz",
        "description_template": "Manually deliver {idea_name} service to 5 paying customers.",
        "duration_days": 14,
        "estimated_cost_usd": 100,
        "pass_threshold": "≥ 3/5 customers pay and say they would recommend",
    },
    AssumptionCategory.DISTRIBUTION: {
        "type": "channel_test",
        "description_template": "Post about {idea_name} on {channel}. Measure response rate.",
        "duration_days": 7,
        "estimated_cost_usd": 30,
        "pass_threshold": "≥ 50 signups or ≥ 5% CTR from channel",
    },
    AssumptionCategory.RETENTION: {
        "type": "concierge_mvp",
        "description_template": "Onboard 10 users manually. Track day-7 return rate.",
        "duration_days": 14,
        "estimated_cost_usd": 80,
        "pass_threshold": "≥ 40% day-7 retention",
    },
    AssumptionCategory.TECHNICAL: {
        "type": "prototype_test",
        "description_template": "Build a minimal prototype of {idea_name}'s core feature.",
        "duration_days": 14,
        "estimated_cost_usd": 100,
        "pass_threshold": "Prototype works end-to-end for 1 happy-path scenario",
    },
    AssumptionCategory.MARKET: {
        "type": "market_sizing_validation",
        "description_template": "Validate market size with 3 independent bottom-up estimates.",
        "duration_days": 7,
        "estimated_cost_usd": 20,
        "pass_threshold": "All 3 estimates agree within 3× range AND SOM year-1 > $10K",
    },
}


def design_rat(
    idea_name: str,
    dimensions: dict[str, float],
    scores: dict,
) -> RatExperiment:
    """Design the Riskiest Assumption Test for an idea.

    Extracts assumptions from each dimension, scores them by
    criticality × uncertainty, and returns the riskiest one
    with a concrete experiment design.
    """
    assumptions = _extract_assumptions(idea_name, dimensions)
    for a in assumptions:
        a["rat_score"] = a["criticality"] * a["uncertainty"]
    assumptions.sort(key=lambda a: a["rat_score"], reverse=True)

    if not assumptions:
        # Fallback: demand assumption
        top = {
            "assumption": f"People actually have the problem {idea_name} solves",
            "category": AssumptionCategory.DEMAND,
            "criticality": 5,
            "uncertainty": 5,
            "rat_score": 25,
        }
    else:
        top = assumptions[0]

    template = EXPERIMENT_TEMPLATES.get(
        top["category"], EXPERIMENT_TEMPLATES[AssumptionCategory.DEMAND]
    )

    return RatExperiment(
        assumption=top["assumption"],
        category=top["category"],
        criticality=top["criticality"],
        uncertainty=top["uncertainty"],
        rat_score=top["rat_score"],
        experiment_type=template["type"],
        description=template["description_template"].format(idea_name=idea_name),
        duration_days=template["duration_days"],
        estimated_cost_usd=template["estimated_cost_usd"],
        pass_threshold=template["pass_threshold"],
        fail_action=f"Drop {idea_name}, document learning",
    )


def _extract_assumptions(idea_name: str, dimensions: dict[str, float]) -> list[dict]:
    """Extract the key assumption behind each dimension's score."""
    assumptions = []

    if "demand" in dimensions:
        assumptions.append({
            "assumption": f"People actually have the problem {idea_name} solves",
            "category": AssumptionCategory.DEMAND,
            "criticality": 5,
            "uncertainty": _inverse_score_to_uncertainty(dimensions.get("demand", 50)),
        })

    if "monetization" in dimensions:
        assumptions.append({
            "assumption": f"Users will pay for {idea_name} at the modelled price point",
            "category": AssumptionCategory.MONETIZATION,
            "criticality": 5,
            "uncertainty": _inverse_score_to_uncertainty(dimensions.get("monetization", 50)),
        })

    if "distribution" in dimensions:
        assumptions.append({
            "assumption": f"Users will find {idea_name} via the modelled channels",
            "category": AssumptionCategory.DISTRIBUTION,
            "criticality": 4,
            "uncertainty": _inverse_score_to_uncertainty(dimensions.get("distribution", 50)),
        })

    if "retention" in dimensions:
        assumptions.append({
            "assumption": f"Users will return to {idea_name} after day 7",
            "category": AssumptionCategory.RETENTION,
            "criticality": 4,
            "uncertainty": _inverse_score_to_uncertainty(dimensions.get("retention", 50)),
        })

    if "competition" in dimensions and dimensions.get("competition", 50) > 70:
        # High competition score = low saturation → less uncertainty about market
        pass
    else:
        assumptions.append({
            "assumption": f"The market is large enough to sustain {idea_name}",
            "category": AssumptionCategory.MARKET,
            "criticality": 4,
            "uncertainty": 4,
        })

    return assumptions


def _inverse_score_to_uncertainty(score: float) -> int:
    """Convert a confidence score (0-100) to uncertainty (1-5).
    Higher score → lower uncertainty.
    """
    if score >= 80:
        return 2
    elif score >= 60:
        return 3
    elif score >= 40:
        return 4
    else:
        return 5
