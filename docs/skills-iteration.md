# Skills System Iteration Plan

Integrate `skm-core` + `skm-select` to replace ad-hoc SKILL.md handling,
add spec-compliant validation/export, and implement cascade-based skill selection.

Reference: [Agent Skills Specification](https://agentskills.io)
Dependency: [skm](https://github.com/tonitangpotato/skm) (skm-core, skm-select)

---

## Phase 1 — Foundation: Replace ad-hoc parsing with skm-core

**Goal**: Drop `skill::discover_llm_skills()` / `skill::load_llm_skill()` in favor
of skm-core's `SkillParser` + `SkillRegistry`. Keep the `LlmSkill` type as an
adapter layer to avoid rippling changes across the codebase.

### Steps

1. **Add dependencies**
   - `skm-core` to `kernel/skill/Cargo.toml`
   - If skm-core uses `workspace = true` internally, vendor or pin explicit versions

2. **Create adapter `SkmRegistry` in `kernel/skill/src/`**
   - Wraps `skm_core::SkillRegistry`
   - `new(root: &Path) -> Self` — initialises the registry pointing at `~/.aman/skills/`
   - `discover() -> Vec<LlmSkill>` — iterates `registry.catalog()`, maps `Skill` → `LlmSkill`
   - `load_content(name: &str) -> Option<String>` — lazy-loads full SKILL.md text
   - `refresh() -> RefreshReport` — re-scans disk for changes

3. **Update callers**
   - `kernel/gateway/src/runtime/agent_runtime.rs`:
     Replace `skill::discover_llm_skills(&skills_dir)` → `skill::SkmRegistry::new(&skills_dir).discover()`
   - `AgentRuntime::read_skill()`: use `SkmRegistry::load_content()` internally
   - `HotReloadManager`: integrate `SkmRegistry::refresh()` into the watch loop

4. **Keep existing public API**
   - `LlmSkill { name, description, path }` stays unchanged
   - No changes to `gateway` crate's `llm_skills_prompt()` or `read_skill()`

### Files touched

| File | Change |
|---|---|
| `kernel/skill/Cargo.toml` | add `skm-core` |
| `kernel/skill/src/lib.rs` | add `mod skm_adapter;` + re-export |
| `kernel/skill/src/skm_adapter.rs` | new: `SkmRegistry` adapter |
| `kernel/gateway/src/runtime/agent_runtime.rs` | use `SkmRegistry` in build |

### Exit criteria

- All existing tests pass (skill sync, frontmatter parsing, discovery)
- `cargo test -p skill` green
- No changes to `LlmSkill` struct or `gateway` public API

---

## Phase 2 — Validation layer

**Goal**: Spec-compliant validation CLI and library, built on skm-core's parsing.

### Validation rules (agentskills.io specification)

| Rule | Check |
|---|---|
| R1 | SKILL.md must have YAML frontmatter delimited by `---` |
| R2 | Frontmatter must contain `name` (string) |
| R3 | Frontmatter must contain `description` (string) |
| R4 | `name` must match `^[a-z0-9]([a-z0-9_-]*[a-z0-9])?$` |
| R5 | File name must be exactly `SKILL.md` (case-sensitive) |
| R6 | Directory name must equal `name` field in frontmatter |
| R7 | No orphan files in skill directory (only `SKILL.md` + allowed resources) |
| R8 | Trigger patterns in frontmatter must be valid regex (if present) |
| R9 | Cross-reference check: `related_skills` entries exist (if present) |
| R10 | `version` field (if present) must be semver-compatible |

### Implementation

```
kernel/skill/src/
├── mod.rs
├── skm_adapter.rs       ← Phase 1
├── validation.rs         ← New
│   pub struct SkillValidator { rules: Vec<Box<dyn ValidationRule>> }
│   pub fn validate_all(skills: &[LlmSkill]) -> Vec<ValidationFinding>
│   pub fn validate_one(path: &Path) -> Vec<ValidationFinding>
│
└── export.rs             ← Phase 3
```

### CLI command (`aman skills validate`)

```
$ aman skills validate
✓ ipo-research: all 10 rules passed
⚠ unlisted-ecosystem-analysis: R6 — directory name != frontmatter.name
✗ chaotic-reasoning: R4 — name contains uppercase characters

$ aman skills validate ./skills/my-skill/SKILL.md
✓ my-skill: all 10 rules passed
```

Implementation in `kernel/cli/src/commands/skills.rs` (new file).

### Files touched

| File | Change |
|---|---|
| `kernel/skill/Cargo.toml` | (already updated in Phase 1) |
| `kernel/skill/src/validation.rs` | new |
| `kernel/skill/src/lib.rs` | re-export `validation` module |
| `kernel/cli/src/main.rs` | add `skills validate` subcommand |
| `kernel/cli/src/commands/` | add `mod skills;` with handlers |

### Exit criteria

- `cargo test -p skill` includes validation tests
- `cargo run --bin aman -- skills validate` runs on `~/.aman/skills/`
- All 10 rules tested with valid / invalid fixtures

---

## Phase 3 — Export capability

**Goal**: Export skills to a spec-compliant directory tree consumable by Claude Code,
Cursor, Codex, or any agent that follows agentskills.io.

### Output structure

```
./out/
├── <skill-name>/
│   └── SKILL.md
├── <skill-name>/
│   └── SKILL.md
└── ...
```

### 与 `skm-core` 的依赖关系

| 依赖 | 用途 |
|---|---|
| skm-core | 解析 SKILL.md，提供结构化元数据 |
| skm-select | 三级级联选择引擎 |
| 自有代码 | 验证规则 + 导出逻辑 |

验证和导出的核心能力来自 agentskills.io 规范本身，而非 harness-rs-skills。
skm-core 提供了底层解析基础设施，上层规则我们自行实现。

### CLI command

```
$ aman skills export ./out
✓ exported 4 skills to ./out

$ ls ./out/
ipo-research/  unlisted-ecosystem-analysis/  chaotic-reasoning/  discover-facts/
```

### Implementation

`kernel/skill/src/export.rs`:

```rust
pub fn export_skills(skills: &[LlmSkill], out_dir: &Path) -> Result<ExportReport>
```

Simple file copy — read SKILL.md from source path, write to `out_dir/{name}/SKILL.md`.

### Files touched

| File | Change |
|---|---|
| `kernel/skill/src/export.rs` | new |
| `kernel/skill/src/lib.rs` | re-export `export` module |
| `kernel/cli/src/commands/skills.rs` | add `export` subcommand |

### Exit criteria

- `cargo run --bin aman -- skills export ./out` creates correct directory structure
- Exported files are byte-identical to originals
- Output can be consumed by Claude Code (`claude_code.md` step, if applicable)

---

## Phase 4 — Cascade selection (skm-select integration)

**Goal**: Replace keyword matching in `http.rs` with skm-select's
`CascadeSelector` (trigger → semantic → LLM).

### Current state (implemented)

```rust
// http.rs — simple keyword matching
skills.iter().find(|s| text.to_lowercase().contains(&s.name.to_lowercase()))
```

### Target state

```
User message
  │
  ▼
CascadeSelector
  │
  ├─ Stage 1: TriggerStrategy (~50µs)
  │    Regex / keyword / name matching
  │    └─ High confidence? → Return matched skill(s)
  │
  ├─ Stage 2: SemanticStrategy (~5ms)
  │    Embedding similarity (BGE-M3 local ONNX)
  │    └─ Above threshold? → Return ranked skills
  │
  └─ Stage 3: LlmStrategy (~1-2s)
       Few-shot LLM classification
       └─ Return best skill + confidence
```

### Steps

1. **Add `skm-select` dependency** to `kernel/gateway/Cargo.toml`

2. **Build selector at startup** in `AgentRuntime::build()`:
   ```rust
   let selector = CascadeSelector::new(vec![
       Box::new(TriggerStrategy::from_registry(&registry)),
       Box::new(SemanticStrategy::new(embedding_provider)),
       Box::new(LlmStrategy::new(llm_client)),
   ]);
   ```

3. **Store selector on `AgentRuntime`**:
   ```rust
   skill_selector: StdMutex<Option<CascadeSelector>>,
   ```

4. **Replace matching in `http.rs`**:
   ```rust
   let result = runtime.skill_selector()
       .select(&text, &registry.catalog(), &ctx);
   ```

5. **Progressive disclosure integration**:
   - Selected skills at high confidence → auto-inject full content
   - Low confidence → inject only name + description, instruct LLM to use `read_skill()`

### Caveats

- **Semantic stage**: Requires embedding model (BGE-M3 ~100MB). First launch downloads model.
  - Make this configurable: `--semantic-model none` to skip semantic stage.
  - Fallback: skip Stage 2 if model unavailable, go directly trigger → LLM.
- **LLM stage**: Uses the same provider/API key as chat. Adds latency.
  - Only fires when trigger + semantic miss.

### Performance budget (per message)

| Configuration | Max added latency |
|---|---|
| Trigger only (no semantic model) | ~120µs |
| + Semantic (BGE-M3) | ~5ms |
| + LLM fallback | ~1-2s |

### Files touched

| File | Change |
|---|---|
| `kernel/gateway/Cargo.toml` | add `skm-select` |
| `kernel/gateway/src/runtime/agent_runtime.rs` | add `skill_selector` field + init |
| `kernel/gateway/src/runtime/http.rs` | use `CascadeSelector` instead of keyword match |
| `kernel/gateway/src/config.rs` | add `skill_selector.semantic_model` config field |

### Exit criteria

- Trigger-only path: message latency unchanged (< 1ms overhead)
- Full cascade path: skill injection matches user intent better than keyword matching
- Config toggle to disable semantic model
- Graceful degradation when model unavailable

---

## Phase 5 — Multi-skill injection & token budget (future)

**Goal**: Handle multiple relevant skills, respect context window limits.

- When multiple skills match (e.g., trigger returns 3 candidates), select top-N that fit within a token budget
- Use `skm-disclose` if available, or implement simple token counting
- Priority: higher confidence skills get injected first

This phase is deferred — only implement if token pressure becomes a measurable issue
(hit context limits during long chat sessions with many installed skills).

---

## Dependency graph

```
Phase 1 ───────────────────────────────────────────────
  skm-core: parsing + registry + filesystem watching
  └─ replaces ad-hoc discover_llm_skills / load_llm_skill

Phase 2 ───────────────────────────────────────────────
  skill::validation (own code, ~300 lines)
  └─ builds on skm-core parsing

Phase 3 ───────────────────────────────────────────────
  skill::export (own code, ~50 lines)
  └─ builds on skm-core registry

Phase 4 ───────────────────────────────────────────────
  skm-select: CascadeSelector (trigger → semantic → LLM)
  └─ builds on skm-core registry + catalog
```

## Rollback strategy

Each phase is independently revertible:
- Phase 1: Restore `discover_llm_skills()` — old path still compiles
- Phase 2/3: No production impact — CLI-only features
- Phase 4: Fall back to keyword matching in `http.rs` — toggle via config flag
