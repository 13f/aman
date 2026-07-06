---
name: compound
category: agent
description: >
  Compound Loop — extract learnings after execution. Review what worked,
  what didn't, and what patterns emerged. Persist findings to EXP.md as
  durable experience. Use as the final phase of any complex pipeline to make
  the system smarter over time.
version: 1.0.0
triggers:
  - "compound"
  - "learn from this"
  - "总结经验"
  - "回炉"
  - "what did we learn"
  - "extract learnings"
tags:
  - reflection
  - experience-extraction
  - compound-loop
  - learning
metadata:
  hermes:
    tags: [reflection, experience-extraction, compound-loop, learning]
    related_skills: [plan, brainstorm, review]
---

# Compound Loop

## Core Rule

**Look back. Extract. Persist. Make the next run smarter.**

Execution without reflection is consumption. Reflection without persistence
is wasted. Your job: turn what just happened into durable system knowledge.

## When to Use

**Use compound when:**
- A complex task or pipeline just finished (especially the Guarded Flow phase)
- The user asks "what did we learn?"
- Something unexpected happened (success or failure)
- As the final step of any Plan pipeline
- Experience=Apprehensive triggered (analyze what went wrong)

**Skip compound when:**
- The task was trivial with nothing new to learn
- The task is mid-execution (compound is for post-mortems)
- You're about to run the exact same task again immediately

## Methodology

### Step 1: Reconstruct

Re-examine what just happened:
- What was the original goal?
- What actually happened?
- Where did reality diverge from the plan?

### Step 2: Pattern Extract

For each significant event, classify:

| Pattern Type | Question | Example |
|---|---|---|
| **Gotcha** | What surprised us? | "kind doesn't need port-forward" |
| **Effective Strategy** | What worked well that we should repeat? | "gh CLI more stable than raw API" |
| **Anti-Pattern** | What should we avoid next time? | "raw API calls timed out after 3 retries" |
| **Template** | What's reusable? | "k8s deploy script pattern" |
| **Insight** | What do we believe now that we didn't before? | "Deploy order matters: secrets → config → pods" |

### Step 3: Persist to EXP.md

Use the `experience` system (EXP.md) to save learnings:

For each pattern found:
1. Tag it with the task type (e.g., `[deploy]`, `[pr]`, `[k8s]`)
2. Write a one-line gotcha/strategy
3. Set initial confidence based on evidence:
   - Single observation → 0.5 (tentative)
   - 2-3 consistent observations → 0.7 (emerging pattern)
   - 4+ observations → 0.9 (reliable)

The EXP.md format:
```markdown
## Gotchas
### [task_tag] Short description
- **Gotcha**: What happened and what to do instead
- **confidence**: 0.0-1.0
- **uses**: N
- **successes**: N

## Tool Strategies
### [task_tag] Short description
- **Strategy**: What works well
- **confidence**: 0.0-1.0
```

### Step 4: Feedback Loop

After persisting, summarize for the user:
- What new knowledge was captured
- How it will change future behavior (e.g., "next time EXP=Confident, skip this check")
- What remains uncertain (needs more evidence)

## Output Format

```markdown
## Compound: <task name>

### Patterns Extracted

| # | Type | Tag | Finding | Confidence |
|---|------|-----|---------|------------|
| 1 | Gotcha | deploy | kind doesn't need port-forward | 0.5 |
| 2 | Strategy | github | gh CLI > raw API for PR ops | 0.5 |
| 3 | Insight | k8s | Deploy order: secrets → config → pods | 0.5 |

### EXP.md Updated
- New entries: 2
- Updated entries: 1 (gh CLI confidence 0.7 → 0.8)

### Next-Time Effect
- Tag `deploy` will trigger Apprehensive-aware tool selection
- Tag `github` will skip scout phase next time (Confidence > 0.7)

### Open Questions
- Does the deploy-order insight hold for Helm charts?
```

## Integration Points

- **Input**: Guarded Flow execution log (task list + outcomes)
- **Output**: Updated EXP.md + structured learning summary
- **Trigger**: Plan pipeline's final step, or post-execution reflection
- **Experience Link**: This is the human-readable complement to the automated
  Experience Extractor (workflow::compound event handler)

## Anti-patterns

- ❌ Vague learnings ("be careful next time") — be specific
- ❌ Over-confidence from single observation — use 0.5 unless repeated
- ❌ Tagging everything with "misc" — tags must be actionable for future matching
- ❌ Capturing what's already in EXP.md — update confidence instead of duplicating
- ❌ Persisting opinions as facts — distinguish "we tried this" from "this is true"

## Key Principle

> The value of compound is not in the reflection itself — it's in making the
**next** execution skip the mistakes of this one. If EXP.md isn't updated,
the compound loop is incomplete.
