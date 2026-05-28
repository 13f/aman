"""
Evolution package — self-improvement infrastructure.

Modules for prompt mutation, A/B testing, and self-audit.
"""

from .mutator import (
    MutationStrategy,
    VariantRecord,
    BUILTIN_MUTATORS,
    generate_variants,
    track_variant,
    best_variant,
    list_variants,
)
from .auditor import (
    AuditFinding,
    AuditReport,
    check_recurring_errors,
    check_missed_skills,
    check_boundary_near_misses,
    run_self_audit,
)

__all__ = [
    "MutationStrategy",
    "VariantRecord",
    "BUILTIN_MUTATORS",
    "generate_variants",
    "track_variant",
    "best_variant",
    "list_variants",
    "AuditFinding",
    "AuditReport",
    "check_recurring_errors",
    "check_missed_skills",
    "check_boundary_near_misses",
    "run_self_audit",
]
