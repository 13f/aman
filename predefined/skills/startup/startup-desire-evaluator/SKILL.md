---
name: startup-desire-evaluator
category: startup
description: Evaluate how strongly an app idea connects to fundamental human desires (survival, status, belonging, control, curiosity). Returns scored dimensions with virality potential.
version: 1.0.0
metadata:
  tags: [startup, validation, desire, consumer-psychology]
---

# Desire Evaluator

Evaluate how strongly an app idea connects to fundamental human desires.

## The Five Desire Dimensions

Rate each dimension from 1 (weak/no connection) to 5 (very strong/direct connection):

1. **Survival** — Health, safety, financial security. Does using this app make the user feel safer, healthier, or more secure?
2. **Status** — Looking good, achieving, winning. Does the app help users signal status, achievement, or superiority?
3. **Belonging** — Community, connection, not being alone. Does the app connect users to others who share their identity or interests?
4. **Control** — Mastery, autonomy, reducing chaos. Does the app help users feel in control of their lives or environment?
5. **Curiosity** — Learning, discovery, novelty. Does the app satisfy the urge to explore, learn, or experience something new?

## Scoring Rules

- 5 = DIRECTLY satisfies this desire as primary function
- 3 = INDIRECTLY connects to this desire
- 1 = no meaningful connection
- If NO dimension ≥ 3, flag as weak desire connection (high churn risk)

## Virality Potential

- `high`: taps into desires people actively talk about (status, belonging)
- `medium`: has shareable moments but not inherently viral
- `low`: purely utilitarian, unlikely to be shared

## Output

Return ONLY valid JSON (no markdown fences):
```json
{
  "desire_scores": {"survival": 1-5, "status": 1-5, "belonging": 1-5, "control": 1-5, "curiosity": 1-5},
  "desire_label": "strong|moderate|weak",
  "desire_strength": 0.0-5.0,
  "primary_driver": "survival|status|belonging|control|curiosity",
  "virality_potential": "high|medium|low"
}
```
