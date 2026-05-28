## What stays in Rust (never moves)

These are I/O, event-loop, or performance-critical — Python would add latency or
complexity without benefit:

| Module | Reason to keep in Rust |
|--------|----------------------|
| `SoulHotReloadManager` (watch + reload loop) | inotify I/O, event publishing |
| `Soul::inject_*_context()` | context wiring, called on every event |
| `Soul::check_boundary()` | runtime guard on every message — must be fast |
| `build_read_skill_reinforcement()` | triggered by tool-call results in ReAct loop |
| `build_format_reminder()` | triggered by ReAct loop state transitions |
| `prepare_skill_execution()` (file I/O part) | file reading + path resolution |
| `ReflectionRunner` (orchestration) | event handling, tokio::select, phase ordering |
| `DefaultPromptPipeline` (assembly wiring) | the trait + async — Python just provides the string fragments |

## Phase 1: Shadow mode (week 1-2)

**Goal:** Python runs alongside Rust, output compared, no user impact.

1. Add `self.enabled: "shadow"` config mode
2. Gateway startup loads Python modules, builds prompt fragments
3. For every system prompt assembly:
   - Rust builds the prompt (control)
   - Python builds the prompt (shadow)
   - Compare, log any diffs at `debug!` level
4. Run in dev/staging for 1-2 weeks, fix any divergence

**Files touched:** `gateway/src/runtime/http.rs` (prompt assembly site),
`gateway/src/runtime/agent_harness.rs` (system prompt injection)

**Risk:** Zero — Rust path unchanged, Python output is write-only.

## Phase 2: Python-first with Rust fallback (week 3-4)

**Goal:** Python becomes the default prompt builder; Rust is fallback.

1. Flip `self.enabled: true` (was `"shadow"`)
2. Gateway calls Python for these functions:
   - `soul_builder.soul_to_system_prompt()`
   - `skills_builder.build_skills_system_prompt()`
   - `tools_builder.build_full_system_prompt()`
   - `reflection.extraction_prompt()`
   - `router.parse_skill_command()`
   - `router.match_skill_prefix()`
3. If Python call fails, fall back to Rust and log a warning
4. Watch metrics: prompt build latency, error rate

**Files touched:** Same as Phase 1, plus gateway config validation.

**Risk:** Low — fallback path exists. Python errors are caught and logged.

## Phase 3: Remove Rust (week 5-6)

**Goal:** Delete the Rust code that Python now handles.

### Step 3a — Remove from `soul` crate

```rust
// REMOVE:
impl Soul {
    pub fn parse(content: &str) -> AmanResult<Self>  // → self.prompts.soul_builder.parse_soul
    pub fn to_system_prompt(&self) -> String          // → self.prompts.soul_builder.soul_to_system_prompt
}
struct SoulMarkdown { ... }                          // → Python parse_soul()
impl SoulMarkdown { fn parse(), fn list() }          // → Python parse_soul()

// KEEP:
impl Soul {
    pub fn from_file()                               // file I/O stays
    pub fn check_boundary()                          // runtime guard stays
    pub fn inject_*_context()                        // context wiring stays
}
SoulHotReloadManager                                 // watcher stays
soul_changed_event()                                 // event stays
```

### Step 3b — Remove from `skill` crate

```rust
// REMOVE from formatting.rs:
pub fn build_skills_system_prompt()                  // → self.prompts.skills_builder
pub fn build_skill_activation_message()              // → self.prompts.skills_builder
pub fn strip_frontmatter()                           // → self.prompts.skills_builder

// KEEP in formatting.rs:
pub fn build_read_skill_reinforcement()              // ReAct loop needs this
pub fn build_format_reminder()                       // ReAct loop needs this

// REMOVE from execution.rs:
pub fn parse_skill_command()                         // → self.decisions.router
pub fn match_skill_prefix()                          // → self.decisions.router
pub fn prepare_skill_execution()                     // → self.decisions.router.resolve_skill
pub fn prepare_skill_execution_from_dir()            // → self.decisions.router.resolve_skill

// KEEP: SkillExecution struct (used by callers)
```

### Step 3c — Remove from `core` crate

```rust
// REMOVE:
pub struct DefaultPromptPipeline                     // → self.prompts.tools_builder
impl PromptPipeline for DefaultPromptPipeline        // → self.prompts.tools_builder

// KEEP:
pub trait PromptPipeline                             // trait stays (other impls may exist)
pub fn current_date_string()                         // small utility, harmless to keep
```

### Step 3d — Remove from `gateway` runtime

```rust
// REMOVE:
pub fn extraction_prompt()                           // → self.prompts.reflection
pub fn format_conversation()                         // → self.prompts.reflection

// KEEP:
ReflectionRunner + all its methods                   // orchestration stays
session_extract_and_store()                          // LLM call stays
extract_entities_from_events()                       // local computation stays
```

## Phase 4: Self-evolution enabled (week 7+)

**Goal:** Agent can modify its own Python modules at runtime.

1. Add `self.auto_evolve: true` config flag
2. `evolution.mutator` generates prompt variants
3. `evolution.auditor` runs periodic self-checks
4. Agent proposes changes → writes back to `self/` files
5. Gateway hot-reloads (same mechanism as plugin hot-reload)

**Risk:** Medium — needs safeguards (diff review, rollback, max changes/day).

---

## Summary of Rust code to remove

| Crate | File | What goes | Lines ~ |
|-------|------|-----------|---------|
| `soul` | `lib.rs` | `Soul::parse()`, `Soul::to_system_prompt()`, `SoulMarkdown` | ~80 |
| `skill` | `formatting.rs` | `build_skills_system_prompt()`, `build_skill_activation_message()`, `strip_frontmatter()` | ~70 |
| `skill` | `execution.rs` | `parse_skill_command()`, `match_skill_prefix()`, `prepare_skill_execution()`, `prepare_skill_execution_from_dir()` | ~80 |
| `core` | `prompt.rs` | `DefaultPromptPipeline` impl | ~50 |
| `gateway` | `reflection.rs` | `extraction_prompt()`, `format_conversation()` | ~35 |

**Total: ~315 lines of Rust removed**, replaced by ~550 lines of self-modifiable Python.
