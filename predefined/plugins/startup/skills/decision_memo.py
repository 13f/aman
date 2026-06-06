#!/usr/bin/env python3
"""Decision Memo skill — generates the final human-readable decision brief
from completed analysis dimensions and scoring results.
"""

from __future__ import annotations

import os
from datetime import datetime, timezone
from typing import Any

from llm import LlmClient


_PROMPT_DIR = os.path.join(os.path.dirname(os.path.dirname(__file__)), "prompts")


def _load_prompt(name: str) -> str:
    with open(os.path.join(_PROMPT_DIR, name), "r") as f:
        return f.read()


VERDICT_EMOJI = {
    "pursue": "\U0001f7e2",  # green circle
    "test": "\U0001f535",     # blue circle
    "pivot": "\U0001f7e1",    # yellow circle
    "drop": "\U0001f534",     # red circle
}


def generate_decision_memo(
    idea_slug: str,
    idea_description: str,
    scores: dict[str, Any],
    desire: dict[str, Any] | None = None,
    competitors: dict[str, Any] | None = None,
    pricing: dict[str, Any] | None = None,
    weaknesses: list[dict] | None = None,
    founder_tier: str = "intermediate",
    llm: LlmClient | None = None,
) -> str:
    """Generate a human-readable decision memo.

    Args:
        idea_slug: The idea slug identifier.
        idea_description: The original idea description.
        scores: Scoring results from idea-scoring (ScoreResult dict).
        desire: Optional desire evaluation results.
        competitors: Optional competitor analysis results.
        pricing: Optional pricing analysis results.
        weaknesses: Optional detected weaknesses.
        founder_tier: Founder experience level (beginner/intermediate/experienced).
        llm: LLM client (uses default config if None).

    Returns:
        Markdown string with the complete decision memo.
    """
    client = llm or LlmClient()
    system_prompt = _load_prompt("decision_memo.txt")

    verdict = scores.get("verdict", "drop")
    final_score = scores.get("final_score", 0)
    confidence = scores.get("confidence", "low")
    dimension_scores = scores.get("dimension_scores", {})
    killer_dimensions = scores.get("killer_dimensions", [])

    # Build a rich context for the LLM
    context_parts = [
        f"## Idea\n- Slug: {idea_slug}\n- Description: {idea_description}",
        f"## Scores\n- Final Score: {final_score}/100\n- Verdict: {verdict}\n- Confidence: {confidence}",
        f"- Killer Dimensions (scored <25): {', '.join(killer_dimensions) if killer_dimensions else 'none'}",
    ]

    if dimension_scores:
        dims = "\n".join(f"  - {dim}: {score}/100" for dim, score in dimension_scores.items())
        context_parts.append(f"## Dimension Scores\n{dims}")

    if desire:
        context_parts.append(
            f"## Desire Analysis\n- Primary driver: {desire.get('primary_driver', 'unknown')}\n"
            f"- Desire strength: {desire.get('desire_strength', 0)}\n"
            f"- Virality: {desire.get('virality_potential', 'unknown')}"
        )

    if competitors:
        comp_count = len(competitors.get("direct_competitors", []))
        saturation = competitors.get("market_saturation", "unknown")
        gaps = len(competitors.get("positioning_gaps", []))
        strongest_gap = ""
        if "review_mining_summary" in competitors:
            strongest_gap = competitors["review_mining_summary"].get("strongest_gap_signal", "")
        context_parts.append(
            f"## Competition\n- Direct competitors: {comp_count}\n"
            f"- Market saturation: {saturation}\n"
            f"- Positioning gaps found: {gaps}\n"
            f"- Strongest gap signal: {strongest_gap}"
        )

    if pricing:
        context_parts.append(
            f"## Pricing\n- Recommended: ${pricing.get('recommended_price_monthly', 0)}/mo\n"
            f"- Model: {pricing.get('pricing_model', 'unknown')}"
        )

    if weaknesses:
        weak_str = "\n".join(
            f"  - {w.get('dimension', '?')}: {w.get('description', '')} "
            f"(root cause: {w.get('root_cause_type', 'unknown')})"
            for w in weaknesses[:5]
        )
        context_parts.append(f"## Weaknesses\n{weak_str}")

    context_parts.append(f"## Founder\n- Tier: {founder_tier}")

    user_prompt = (
        f"Write a decision memo for idea '{idea_slug}'.\n\n"
        + "\n\n".join(context_parts)
        + f"\n\nWrite the memo now. Verdict is {verdict.upper()}."
    )

    markdown = client.chat(
        system_prompt=system_prompt,
        user_prompt=user_prompt,
        temperature=0.4,
        max_tokens=3000,
    )

    # Add frontmatter if missing
    if not markdown.strip().startswith("---"):
        now = datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")
        emoji = VERDICT_EMOJI.get(verdict, "\U0001f7e2")
        markdown = (
            f"---\n"
            f"idea_slug: \"{idea_slug}\"\n"
            f"verdict: \"{verdict}\"\n"
            f"final_score: {final_score}\n"
            f"score_confidence: \"{confidence}\"\n"
            f"created_at: \"{now}\"\n"
            f"---\n\n"
            f"# Decision Memo: {idea_slug}\n\n"
            f"## Verdict: {emoji} {verdict.upper()}\n\n"
            f"**Score: {final_score}/100** | Confidence: {confidence}\n\n"
            + markdown
        )

    return markdown
