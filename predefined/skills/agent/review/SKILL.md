---
name: review
category: agent
description: >
  Multi-dimensional structured review — evaluate a plan, code change, document,
  or decision from multiple perspectives. Each perspective asks specific
  questions and produces a verdict. Use after brainstorming to compare options,
  after implementing to validate quality, or standalone on any deliverable.
version: 1.0.0
triggers:
  - "review"
  - "critique"
  - "evaluate"
  - "look at this"
  - "审查"
  - "评审"
  - "检查一下"
  - "看看有没有问题"
  - "review this plan"
  - "多视角"
tags:
  - review
  - critique
  - validation
  - multi-perspective
  - quality
metadata:
  hermes:
    tags: [review, critique, validation, multi-perspective, quality]
    related_skills: [brainstorm, plan, subagent-driven-development]
---

# Review

## Core Rule

**From each perspective, ask that perspective's questions. Don't mix lenses.**

Your job: evaluate a deliverable through distinct lenses, one at a time.
Each lens cares about different things. Switching lenses mid-thought produces
shallow review.

## When to Use

**Use review when:**
- Brainstorm produced options and you need to compare them before committing
- A plan is written and needs validation before execution
- Code is written and needs spec/quality checks
- A document, architecture decision, or design rationale needs scrutiny
- After a task completes, as a Compound Loop gate ("did we actually do what we intended?")
- As Plan's Multi-lens Lock phase

**Skip review when:**
- The deliverable is trivial (one-line typo fix, single config change)
- The user explicitly says "just do it"
- You're in the middle of a time-sensitive iteration (review after, not during)
- The "review" is just reading a file (that's reading, not reviewing)

## Perspectives (Lenses)

Pick **at least 3** that apply to the deliverable. Each perspective has
a focus and specific questions.

### Always Available

| Perspective | Focus | Key Questions |
|---|---|---|
| **Correctness** | Does it work? | Edge cases? Error paths? Off-by-one? Null inputs? |
| **Safety** | Can it hurt us? | Injection? Privilege escalation? Data leak? Rollback path? |
| **Performance** | Does it scale? | N+1 queries? Memory leaks? Bottlenecks under load? |
| **Cost** | What does we pay? | Token cost? Compute? Maintenance burden? Lock-in? |
| **Maintainability** | Can the next human understand it? | Naming? Coupling? Documentation? Test coverage? |
| **User Experience** | Does the user succeed? | Error messages? Defaults? Edge case UX? Accessibility? |

### Domain-Addable

| Perspective | Focus | Key Questions |
|---|---|---|
| **Consistency** | Does it match existing patterns? | Convention adherence? Shared infra? Same style? |
| **Security** | (deeper than Safety) | AuthZ? Audit trail? Secret rotation? Encryption at rest? |
| **Observability** | Can we debug it in production? | Logs? Metrics? Traces? Alert thresholds? |
| **Evolvability** | Can we change it later? | Extension points? Migration path? Coupling? |

### Per-Project Custom

Projects can add project-specific lenses in the call. Example:
- "Also review from a **data migration** lens" (can we roll back?)
- "Review from a **team onboarding** lens" (can a new member understand this?)

> If the caller specifies perspectives, use those. If not, pick from Always
> Available based on what the deliverable is (code → Correctness/Safety/Performance;
> plan → Consistency/Evolvability/Cost).

## Methodology

### Step 1: Frame

Clarify what's being reviewed:
- What is the deliverable? (plan, code diff, document, decision)
- What are the acceptance criteria? (what does "good" look like?)
- Which perspectives apply? (select 3+)

### Step 2: Per-Lens Review

For EACH selected perspective:
1. State the lens name
2. Ask that lens's specific questions against the deliverable
3. Produce a verdict: ✅ Pass / ⚠️ Concern / ❌ Fail
4. If Concern or Fail: cite the exact issue and suggest fix

Do NOT mix lenses. One at a time, top to bottom.

### Step 3: Cross-Lens Synthesis

After all lenses have reviewed:
- Flag contradictions (e.g., Performance says "cache aggressively", Cost says "cache costs memory")
- Surface trade-offs the caller needs to know
- If reviewing multiple options (from Brainstorm): produce a comparison matrix

## Output Format

```markdown
## Review: <deliverable name>

### Perspectives Applied
- Correctness ✅
- Safety ⚠️
- Performance ✅
- Maintainability ❌

### Per-Lens Findings

#### Correctness ✅
- Edge case `foo=null` → handled by check on line 42
- Error path `bar timeout` → retry with backoff, good

#### Safety ⚠️
- **Concern**: `user_input` passed to `exec()` without sanitization
- **Suggest**: use `arg!()` macro to pass as argument, not shell interpolation

#### Performance ✅
- Single DB query, indexed lookup, no N+1
- Memory: O(1) additional after fix

#### Maintainability ❌
- **Fail**: 200-line function with 4 levels of nesting
- **Suggest**: extract `validate()`, `transform()`, `persist()` as separate functions

### Cross-Lens Notes
- Safety concern (exec) and Maintainability concern (nesting) compound:
  the nested code IS the unsafe code. Fix nesting first, safety becomes obvious.

### Verdict
⚠️ **Conditional pass** — resolve Safety `exec` issue and Maintainability
nesting before merge. Correctness and Performance are solid.
```

## Anti-patterns

- ❌ One generic "looks good" without per-lens analysis
- ❌ Mixing concerns (safety issue mentioned in performance section)
- ❌ Review without stating which lenses were used
- ❌ Critiquing things outside the selected lenses' scope
- ❌ Producing a verdict before all lenses have reported
- ❌ Treating "I like it" as a review (taste ≠ validation)
- ❌ Revealing the review structure meta — just do the review

## Integration Points

- **Input from Review**: Raw directions from Brainstorm can be reviewed to
  compare options head-to-head across the same lenses
- **Output to Plan**: Review becomes Plan's Multi-lens Lock phase —
  Once all lenses pass, the plan is "locked" and ready for Guarded Flow execution
- **Post-execution gate**: After subagent-driven-development implements a task,
  Review can validate the output matches the plan

## Key Principle

> A review without explicit lenses is just an opinion. The lenses force you
> to ask questions you'd otherwise skip.
