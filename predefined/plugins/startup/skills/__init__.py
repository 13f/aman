#!/usr/bin/env python3
"""Startup evaluation skills — Phase 1 + Phase 2.

Each skill takes an idea description and optionally prior analysis results,
returning structured JSON data. Skills are called by the validation workflows
in main.py.

Evaluation (Phase 1):
  analyze_competitors  — competitive landscape + gap analysis
  evaluate_desire      — 5-dimension desire scoring
  analyze_pricing      — Van Westendorp pricing model
  generate_decision_memo — human-readable decision brief

Evaluation (Phase 2):
  estimate_market_size — TAM/SAM/SOM triangulated estimation
  model_cac            — CAC by channel
  analyze_distribution — viral loops + ASO assessment
  predict_retention    — retention/churn prediction
  assess_complexity    — build difficulty assessment
  detect_weaknesses    — root cause classification
  analyze_trends       — multi-platform trend scanning
  generate_pivots      — pivot options for low-scoring ideas
"""

from skills.competitor_mapper import analyze_competitors
from skills.desire_evaluator import evaluate_desire
from skills.pricing import analyze_pricing
from skills.decision_memo import generate_decision_memo
from skills.tam_sam_som import estimate_market_size
from skills.cac_modeler import model_cac
from skills.distribution_analysis import analyze_distribution
from skills.retention_predictor import predict_retention
from skills.complexity_assessment import assess_complexity
from skills.weakness_detection import detect_weaknesses
from skills.trend_analysis import analyze_trends
from skills.pivot_engine import generate_pivots
from skills.landing_page import build_landing_page
from skills.gtm_narrative import build_gtm_narrative
from skills.pricing_page import optimize_pricing_page
from skills.cold_outreach import design_outreach
from skills.mvp_scope import negotiate_mvp
from skills.feedback_synthesis import synthesize_feedback
from skills.decision_journal import audit_decisions
from skills.ikigai import check_ikigai
from skills.what_if import simulate_what_if

__all__ = [
    # Phase 1
    "analyze_competitors",
    "evaluate_desire",
    "analyze_pricing",
    "generate_decision_memo",
    # Phase 2
    "estimate_market_size",
    "model_cac",
    "analyze_distribution",
    "predict_retention",
    "assess_complexity",
    "detect_weaknesses",
    "analyze_trends",
    "generate_pivots",
    # Strategy
    "build_landing_page",
    "build_gtm_narrative",
    "optimize_pricing_page",
    "design_outreach",
    # Execution
    "negotiate_mvp",
    "synthesize_feedback",
    # Reflection
    "audit_decisions",
    "check_ikigai",
    # AI-Native
    "simulate_what_if",
]
