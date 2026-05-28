"""
Self-auditor — periodic self-review to detect pattern failures and
suggest improvements. Complements the passive ReflectionRunner with
active, agent-initiated self-examination.

This is a new capability with no Rust equivalent. The agent periodically
reviews its own conversation logs and identifies:
- Recurring mistakes (same error type appearing frequently)
- Missed skill matches (user asked for something a skill covers, but agent didn't load it)
- Boundary violations (near-misses or actual violations)
- Prompt improvement opportunities
"""

from __future__ import annotations

from dataclasses import dataclass, field
from typing import Any


@dataclass
class AuditFinding:
    """A single issue found during self-audit."""
    severity: str  # "low", "medium", "high", "critical"
    category: str   # "recurring_error", "missed_skill", "boundary_near_miss", "prompt_issue"
    description: str
    evidence: list[str] = field(default_factory=list)
    suggested_fix: str = ""
    auto_fixable: bool = False


@dataclass
class AuditReport:
    """Result of a self-audit cycle."""
    findings: list[AuditFinding] = field(default_factory=list)
    sessions_reviewed: int = 0
    errors_analyzed: int = 0
    prompt_suggestions: list[str] = field(default_factory=list)

    @property
    def critical_count(self) -> int:
        return sum(1 for f in self.findings if f.severity == "critical")

    @property
    def auto_fixable_count(self) -> int:
        return sum(1 for f in self.findings if f.auto_fixable)

    def summary(self) -> str:
        if not self.findings:
            return "No issues found."
        lines = [
            f"Audit: {len(self.findings)} findings "
            f"({self.critical_count} critical, {self.auto_fixable_count} auto-fixable) "
            f"across {self.sessions_reviewed} sessions."
        ]
        for f in self.findings:
            lines.append(f"  [{f.severity.upper()}] {f.category}: {f.description}")
        return "\n".join(lines)


# ── Audit checks ──────────────────────────────────────────────────────

def check_recurring_errors(
    error_logs: list[dict[str, Any]],
    threshold: int = 3,
) -> list[AuditFinding]:
    """Detect error types that appear repeatedly."""
    from collections import Counter
    type_counts = Counter(
        e.get("error_type", "unknown") for e in error_logs
    )
    findings = []
    for error_type, count in type_counts.items():
        if count >= threshold:
            findings.append(AuditFinding(
                severity="high" if count >= 5 else "medium",
                category="recurring_error",
                description=f"Error type '{error_type}' occurred {count} times",
                evidence=[f"{count} occurrences of {error_type}"],
                suggested_fix=f"Review tool or workflow that produces '{error_type}' errors",
                auto_fixable=False,
            ))
    return findings


def check_missed_skills(
    sessions: list[dict[str, Any]],
    available_skills: list[str],
) -> list[AuditFinding]:
    """Detect sessions where a skill could have helped but wasn't loaded."""
    findings = []
    for session in sessions:
        intent = session.get("intent", "").lower()
        loaded = session.get("skills_loaded", [])
        for skill in available_skills:
            skill_lower = skill.lower().replace("-", " ")
            if skill_lower in intent and skill not in loaded:
                findings.append(AuditFinding(
                    severity="low",
                    category="missed_skill",
                    description=f"Skill '{skill}' might have helped for intent: {session.get('intent', '')}",
                    evidence=[f"Intent: {session.get('intent')}", f"Skills loaded: {loaded}"],
                    suggested_fix=f"Improve skill description or trigger words for '{skill}'",
                    auto_fixable=True,
                ))
    return findings


def check_boundary_near_misses(
    agent_outputs: list[str],
    boundaries: list[str],
) -> list[AuditFinding]:
    """Detect outputs that come close to violating boundaries."""
    findings = []
    for i, output in enumerate(agent_outputs):
        for boundary in boundaries:
            # Simple check: boundary keywords appearing in output
            boundary_words = set(boundary.lower().split())
            output_words = set(output.lower().split())
            overlap = boundary_words & output_words
            if len(overlap) >= len(boundary_words) * 0.5:
                findings.append(AuditFinding(
                    severity="medium",
                    category="boundary_near_miss",
                    description=f"Output #{i} may be near boundary '{boundary}'",
                    evidence=[f"Overlapping terms: {overlap}"],
                    suggested_fix=f"Strengthen boundary enforcement for '{boundary}'",
                    auto_fixable=False,
                ))
    return findings


def run_self_audit(
    error_logs: list[dict[str, Any]] | None = None,
    sessions: list[dict[str, Any]] | None = None,
    agent_outputs: list[str] | None = None,
    boundaries: list[str] | None = None,
    available_skills: list[str] | None = None,
) -> AuditReport:
    """Run all audit checks and return a consolidated report."""
    report = AuditReport()

    if error_logs:
        report.errors_analyzed = len(error_logs)
        report.findings.extend(check_recurring_errors(error_logs))

    if sessions and available_skills:
        report.sessions_reviewed = len(sessions)
        report.findings.extend(check_missed_skills(sessions, available_skills))

    if agent_outputs and boundaries:
        report.findings.extend(check_boundary_near_misses(agent_outputs, boundaries))

    return report
