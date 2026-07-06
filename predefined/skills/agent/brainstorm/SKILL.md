---
name: brainstorm
category: agent
description: >
  Divergent exploration — given a topic, problem, or goal, generate multiple
  distinct directions, options, or approaches. Defer judgment. Prioritize
  quantity and diversity over quality in the opening round. Use when you need
  creative input, architecture alternatives, feature directions, or problem
  framings — with or without a subsequent Plan.
version: 1.0.0
triggers:
  - "brainstorm"
  - "think of some ideas"
  - "what are the options"
  - "how might we"
  - "发散"
  - "头脑风暴"
  - "想几个方案"
  - "有哪些方向"
  - "brainstorm ideas for"
  - "explore approaches"
tags:
  - creativity
  - divergent-thinking
  - ideation
  - exploration
metadata:
  hermes:
    tags: [creativity, divergent-thinking, ideation, exploration]
    related_skills: [plan, review, subagent-driven-development]
---

# Brainstorm

## Core Rule

**Generate first. Judge later. Diversity over depth — for now.**

Your job: produce a burst of distinct directions. Not the "right" answer —
*plural* answers. The user (or a downstream Review) will narrow later.

## When to Use

**Use brainstorm when:**
- Starting a feature and you don't know which direction to take
- Stuck on a problem and need alternative framings
- Comparing architecture approaches before committing
- The user explicitly asks for options or ideas
- As input to a Plan's Co-spark phase
- An Experience=Apprehensive signal means "avoid the usual path"

**Skip brainstorm when:**
- The user gave a specific, actionable instruction ("fix bug in X")
- There's clearly one obvious approach
- You're in the middle of execution (ideation phase is over)
- The task is purely mechanical (run a command, read a file)

## Methodology

### Round 1: Diverge (no judgment)

Generate **at least 3, ideally 5+** distinct directions. For each:
- One-sentence concept: "What if we..."
- Different fundamental approach (not variations of the same idea)
- Mix safe bets with wild cards

Techniques to cross-check your output:
- **Analogy**: "How would [unrelated domain] solve this?"
- **Inversion**: "What's the opposite of the obvious approach?"
- **Constraint flip**: "What if we had unlimited budget? No budget?"
- **First principles**: "What are the actual requirements vs. assumed?"

### Round 2: Light Pattern (still no commitment)

After generating raw options, add one line each:
- **Upside**: best case if this works
- **Risk**: what could make it fail

This is NOT ranking — just surfacing information.

### Round 3: Synthesize (optional)

If the user asked for a recommendation, group options into 2-3 clusters:
- Cluster A: [conservative] — options that reuse existing patterns
- Cluster B: [balanced] — options that trade some risk for leverage
- Cluster C: [aggressive] — options that bet on a new insight

Present clusters WITHOUT declaring a winner — let the user or Review decide.

## Output Format

```markdown
## Brainstorm: <topic>

### Raw Directions

1. **[Label]**: [One-line concept]
2. **[Label]**: [One-line concept]
3. **[Label]**: [One-line concept]
4. ...
   (aim for 5+)

### Pattern Snapshot (optional)

| # | Direction | Best Case | Biggest Risk |
|---|-----------|-----------|--------------|
| 1 | ... | ... | ... |
| 2 | ... | ... | ... |
| 3 | ... | ... | ... |

### Synthesis (if asked)

- **Conservative**: 1, 3 — reuse existing infrastructure
- **Balanced**: 2, 4 — moderate change with upside
- **Aggressive**: 5 — new pattern, higher leverage, higher risk
```

## Anti-patterns

- ❌ Generating only 2 options (not enough diversity)
- ❌ Variations of the same idea presented as different options
- ❌ Picking a winner during brainstorm (defer to user or Review)
- ❌ "It depends" without stating what it depends on
- ❌ Ending with "I recommend..." during the diverge round
- ❌ Judging ideas before all directions are on the table

## Integration Points

- **Input from Plan**: Plan's Co-spark phase can invoke Brainstorm as a sub-step
- **Output to Review**: Raw directions can be passed to the Review skill for
  multi-dimensional validation before converging
- **Experience signal**: If `EXP.md` shows `Apprehensive` for this task, force
  Brainstorm to avoid the failed path and explore alternatives

## Key Principle

> The value of brainstorming is not in the ideas you produce — it's in the
> ideas you *wouldn't have had* without forcing yourself to go past the first
> obvious answer.
