---
name: create-skill
category: authoring
description: >
  Use when creating, editing, or reviewing a SKILL.md file — covers frontmatter
  structure, description writing, naming conventions, body structure, and
  validation rules.
triggers:
  - "create a skill"
  - "new skill"
  - "edit skill"
  - "skill template"
  - "SKILL.md"
  - "write a skill"
  - "make a skill"
metadata:
  triggers: "create a skill, new skill, edit skill, skill template, SKILL.md, write a skill, make a skill"
---

# Creating Skills (SKILL.md)

## Overview

A skill is a markdown file (`SKILL.md`) in a named directory that teaches an AI agent a reusable workflow, methodology, or domain expertise. Skills are discovered automatically from the skills directory and loaded when their description matches the current task.

Each skill lives in its own directory: `<skills-root>/<name>/SKILL.md`. The directory name must match the `name` field in the frontmatter.

## When to Use

- Creating a new reusable workflow, methodology, or domain reference
- Encoding a pattern you've used successfully across multiple sessions
- Formalizing research checklists, data-gathering routines, or analysis frameworks
- User asks to "save this as a skill" or "remember this workflow"

Don't use for:
- Trivial 1-2 step operations that don't need a reusable reference
- One-shot tasks that won't recur
- Content that belongs in a single conversation, not a persistent skill

## Directory and File Structure

```
<skills-root>/
├── <category>/              # optional category grouping
│   └── <skill-name>/        # must match frontmatter `name`
│       ├── SKILL.md          # required — the skill itself
│       ├── references/       # optional — supporting docs
│       ├── scripts/          # optional — executable scripts
│       ├── resources/        # optional — data files, templates
│       └── assets/           # optional — images, diagrams
└── ...
```

Allowed files in a skill directory: `SKILL.md`, `references/`, `scripts/`, `resources/`, `assets/`, `fixtures/`, `README.md`, and dotfiles. Any other file triggers a validation warning (rule R7).

## Frontmatter Reference

SKILL.md starts with YAML frontmatter delimited by `---`. The frontmatter must begin at byte 0 — no leading blank lines, no BOM.

### Required Fields

| Field | Constraint |
|---|---|
| `name` | Lowercase + hyphens, ≤ 64 chars. Must match the directory name. |
| `description` | Free text describing when to load the skill. ≤ 1024 chars. |

### Optional aman-Specific Fields

| Field | Purpose |
|---|---|
| `category` | Grouping label (e.g., `investment`, `authoring`). Displayed in listings. |
| `triggers` | YAML array of trigger phrases. Higher priority than `description` for matching. |
| `metadata` | Arbitrary key-value data. Use `metadata.triggers` for comma-separated trigger strings (agentskills.io standard format). |

### Minimal Example

```yaml
---
name: my-skill
description: Use when <trigger condition>. <what the skill does>.
---

# My Skill

Skill body here.
```

### Full Example

```yaml
---
name: my-skill
category: research
description: >
  Use when researching <topic> — gathers data, scores quality, produces
  a structured report with actionable recommendations.
triggers:
  - "research topic"
  - "analyze data"
  - "deep dive"
metadata:
  triggers: "research topic, analyze data, deep dive"
---

# My Skill Title

## Overview
...
```

## Writing the Description

The description is the most important field — it determines whether the skill gets loaded for a given task. Rules:

1. Start with `Use when` — describes the trigger condition, not the behavior.
2. Be specific enough to match relevant tasks, generic enough to cover variants.
3. Stay under 1024 characters (hard limit).

| Bad | Good |
|---|---|
| `This skill helps with research.` | `Use when researching a company or industry — gathers financials, competitive landscape, and risks into a structured report.` |
| `Handles crypto analysis.` | `Use when analyzing BTC cycle timing, on-chain indicators, or bottom/top signals for macro investment decisions.` |

## Body Structure

A well-structured skill body follows this pattern:

```
# <Title — descriptive, not just repeating the name>

## Overview
One or two paragraphs: what this skill does and why it exists.

## When to Use / Triggers
- Bulleted trigger conditions
- "Don't use for:" counter-triggers

## <Topic / Workflow / Steps>
- Numbered steps with exact instructions
- Code blocks with concrete examples
- Quick-reference tables where useful

## Common Pitfalls
Numbered list of mistakes and their fixes. Every non-trivial skill should
have at least 2 documented pitfalls.

## Verification Checklist
- [ ] Checkbox list of post-action verifications
```

Overview + When to Use + actionable body + Pitfalls is the minimum for a skill to feel complete.

## Naming Conventions

- `lowercase-with-hyphens` (not underscores, not camelCase)
- ≤ 64 characters
- Action- or domain-oriented: `ipo-research`, `chaotic-reasoning`, `discover-facts`
- No prefix noise — don't repeat the category or project name
- Name the skill for what it does, not where it lives

## Language Rules

- **Skill body in English** — AI models follow English instructions with higher fidelity. Chinese in skill bodies can create ambiguity.
- **Chinese is fine in user-facing output examples** — if the skill produces content the user reads (e.g., a Chinese-language report), the output templates can be in Chinese, but the instructions and steps stay in English.
- **Description and triggers in English** — these are what the loader matches against.

## Validation Rules

Aman validates skills against the agentskills.io specification with these rules:

| Rule | Severity | Check |
|---|---|---|
| R1 | Error | SKILL.md frontmatter must parse as valid YAML with required `name` and `description` |
| R5 | Error | `SKILL.md` must exist in the skill directory |
| R6 | Error | Directory name must equal frontmatter `name` |
| R7 | Warning | No unexpected files in the skill directory |
| R8 | Warning | Trigger patterns must not be empty |
| R9 | Warning | `related_skills` entries must reference existing skills |

Run `aman skill validate` to check a skill before using it.

## Common Pitfalls

1. **Leading whitespace before `---`.** The frontmatter must start at column 0, byte 0. Any blank line or BOM causes a parse failure.

2. **Description too generic or too narrow.** `Use when debugging` matches nothing useful. `Use when debugging TCP socket errors on macOS 26.5 at 3am` matches nothing at all. Find the right abstraction level.

3. **Directory name mismatch.** If the directory is `my-skill/` but the frontmatter says `name: my_skill`, validation rule R6 fails. Keep them identical.

4. **Forgetting Pitfalls.** Every workflow has edge cases. If you can't think of at least 2 pitfalls, you haven't thought about the skill enough.

5. **Not verifying with the validator.** Write the skill, then run `aman skill validate <path>` to catch structural issues before relying on the skill.

6. **Duplicating an existing skill.** Before creating, review the skills already available. Prefer extending or patching an existing skill over creating a near-duplicate.

7. **Writing instructions in Chinese.** AI models parse English triggers and instructions more reliably. Keep the body in English.

8. **Over-fragmenting.** One skill per workflow, not one skill per step. If your skill is "Step 3 of the IPO pipeline," it should be a section in `ipo-research`, not its own skill.

9. **Skipping the description entirely.** A skill with an empty or placeholder description will never get loaded. Write the description first, then the body.

## Verification Checklist

- [ ] `SKILL.md` exists at `<skills-root>/<name>/SKILL.md`
- [ ] Frontmatter starts at byte 0 with `---`, closes with `\n---\n`
- [ ] `name` is present, ≤ 64 chars, lowercase + hyphens, matches directory name
- [ ] `description` is present, ≤ 1024 chars, starts with `Use when ...`
- [ ] Body after frontmatter is non-empty
- [ ] Structure: Overview → When to Use → body → Pitfalls → Verification
- [ ] At least 2 pitfalls documented
- [ ] Body in English (Chinese only in user-facing output examples)
- [ ] Run `aman skill validate <path>` — passes with no errors
- [ ] No duplicate of an existing skill
