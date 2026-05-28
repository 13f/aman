"""
Prompt mutator — generates and tracks prompt variations for self-optimization.

This is a completely new capability with no Rust equivalent. It lets the
agent run A/B tests on its own prompt templates and keep the winners.

Self-evolution hooks:
- MutationStrategy: defines how to generate variants of a prompt template.
- track_variant: record performance of a variant.
- best_variant: return the winning variant.
"""

from __future__ import annotations

import hashlib
import json
import time
from dataclasses import dataclass, field
from typing import Any, Callable, Optional


# ── Mutation strategies ───────────────────────────────────────────────

@dataclass
class MutationStrategy:
    """How to generate variants of a prompt template."""
    name: str
    description: str
    # Function that takes a template string and returns N variants
    mutate: Callable[[str, int], list[str]]


def _rewrite_shorter(template: str, n: int = 2) -> list[str]:
    """Generate shorter, more concise variants."""
    variants = []
    for _ in range(n):
        v = template.replace("You MUST", "Must")
        v = v.replace("do not skip this step", "required")
        variants.append(v)
    return variants


def _rewrite_detailed(template: str, n: int = 2) -> list[str]:
    """Generate more detailed, explicit variants."""
    variants = []
    for _ in range(n):
        v = template.replace(".", ". Be explicit about each step.")
        variants.append(v)
    return variants


def _rewrite_tone(template: str, n: int = 2) -> list[str]:
    """Generate tone variations — direct vs collaborative."""
    variants = []
    tones = [
        ("you", "we"),
        ("must", "should"),
        ("execute", "work through"),
    ]
    for i in range(min(n, len(tones))):
        old, new = tones[i]
        variants.append(template.replace(old, new))
    return variants


BUILTIN_MUTATORS: dict[str, MutationStrategy] = {
    "shorter": MutationStrategy("shorter", "More concise variants", _rewrite_shorter),
    "detailed": MutationStrategy("detailed", "More explicit variants", _rewrite_detailed),
    "tone": MutationStrategy("tone", "Tone variations", _rewrite_tone),
}


# ── Variant tracking ──────────────────────────────────────────────────

@dataclass
class VariantRecord:
    template_name: str
    variant_hash: str
    variant_text: str
    # Performance metrics (set after use)
    uses: int = 0
    successes: int = 0
    user_corrections: int = 0
    avg_turns: float = 0.0
    created_at: float = field(default_factory=time.time)
    last_used_at: float = 0.0


# In-memory store (agent can persist to disk)
_variant_store: dict[str, list[VariantRecord]] = {}


def _hash(text: str) -> str:
    return hashlib.sha256(text.encode()).hexdigest()[:12]


def generate_variants(
    template_name: str,
    base_template: str,
    mutator_names: list[str] | None = None,
    n_per_mutator: int = 2,
) -> list[str]:
    """Generate variant strings for a template using named mutators."""
    if mutator_names is None:
        mutator_names = ["shorter", "detailed", "tone"]

    variants: list[str] = []
    for name in mutator_names:
        mutator = BUILTIN_MUTATORS.get(name)
        if mutator:
            variants.extend(mutator.mutate(base_template, n_per_mutator))

    # Deduplicate
    seen: set[str] = {base_template}
    unique = []
    for v in variants:
        if v not in seen and v.strip():
            seen.add(v)
            unique.append(v)
            rec = VariantRecord(
                template_name=template_name,
                variant_hash=_hash(v),
                variant_text=v,
            )
            _variant_store.setdefault(template_name, []).append(rec)

    return unique


def track_variant(
    template_name: str,
    variant_text: str,
    success: bool = True,
    user_corrected: bool = False,
    turns: int = 1,
) -> None:
    """Record performance data for a variant."""
    h = _hash(variant_text)
    for rec in _variant_store.get(template_name, []):
        if rec.variant_hash == h:
            rec.uses += 1
            if success:
                rec.successes += 1
            if user_corrected:
                rec.user_corrections += 1
            # Exponential moving average for turns
            rec.avg_turns = rec.avg_turns * 0.8 + turns * 0.2
            rec.last_used_at = time.time()
            return


def best_variant(template_name: str) -> Optional[str]:
    """Return the best-performing variant for a template, or None."""
    records = _variant_store.get(template_name, [])
    if not records:
        return None

    # Score: success rate weighted by usage, penalized by corrections
    def score(r: VariantRecord) -> float:
        if r.uses == 0:
            return 0.0
        success_rate = r.successes / r.uses
        correction_penalty = r.user_corrections / max(r.uses, 1)
        return success_rate * (1.0 - correction_penalty) * min(r.uses, 10)

    best = max(records, key=score)
    if score(best) > 0:
        return best.variant_text
    return None


def list_variants(template_name: str) -> list[dict[str, Any]]:
    """List all tracked variants for a template with their stats."""
    result = []
    for rec in _variant_store.get(template_name, []):
        result.append({
            "hash": rec.variant_hash,
            "text_preview": rec.variant_text[:120],
            "uses": rec.uses,
            "successes": rec.successes,
            "corrections": rec.user_corrections,
            "avg_turns": round(rec.avg_turns, 1),
        })
    return sorted(result, key=lambda r: r["successes"] / max(r["uses"], 1), reverse=True)
