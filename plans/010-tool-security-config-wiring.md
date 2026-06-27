# Plan 010: Wire ToolSecurityConfig into agent harness

> **Executor instructions**: Follow this plan step by step. Run every verification command and confirm the expected result before moving to the next step. If anything in the "STOP conditions" section occurs, stop and report — do not improvise. When done, update the status row for this plan in `plans/README.md` — unless a reviewer dispatched you and told you they maintain the index.
>
> **Drift check (run first)**: `git diff --stat ec2cddf..HEAD -- kernel/gateway/src/runtime/agent_harness.rs kernel/tool/src/lib.rs kernel/gateway/src/runtime/agent_runtime.rs kernel/config/src/lib.rs`
> If any in-scope file changed since the plan was written, compare the "Current state" excerpts against the live code before proceeding; on a mismatch, treat it as a STOP condition.

## Status

- **Priority**: P1
- **Effort**: L
- **Risk**: MED
- **Depends on**: none
- **Category**: security
- **Planned at**: commit `ec2cddf`, 2026-06-16

## Why this matters

The `ToolSecurityConfig` system provides runtime enforcement for tool execution: path allowlists (restrict which directories `ReadTool`/`WriteTool` can access), network access control (block `HttpTool` from making outbound requests), and command allowlists (restrict which binaries `ExecTool` can run, plus hardline blocks for `rm -rf /`, fork bombs, `dd` against block devices). The security checks are fully implemented in `kernel/tool/src/lib.rs:331-401` — `check_tool_security()`, `check_allowed_path()`, `check_hardline_block()`.

However, the `AgentHarness` builder's `with_security_config()` method at `agent_harness.rs:235` is annotated `#[allow(dead_code)]` — it is never called. The `security_config` field is always `None`, so `check_tool_security` is only partially invoked (hardline blocks run, but path/network/command allowlists never apply). This means `ExecTool` can run any command, `ReadTool`/`WriteTool` can access any path, and `HttpTool` can make any outbound request — the security infrastructure exists but is never activated.

## Current state

- `kernel/gateway/src/runtime/agent_harness.rs:232-238` — dead `with_security_config`:
```rust
/// Set a security config for path/network/command allowlist checks.
#[must_use]
#[allow(dead_code)]
pub fn with_security_config(mut self, config: ToolSecurityConfig) -> Self {
    self.security_config = Some(config);
    self
}
```

- `kernel/gateway/src/runtime/agent_harness.rs:344-351` — security check (only hardline runs):
```rust
let hardline_blocked: Option<&str> =
    security::check_hardline_block(&tool_name, &call.args);

let config_blocked: Option<String> = self.security_config.as_ref().and_then(|config| {
    tool::check_tool_security(config, &call.args)
        .err()
        .map(|e| e.to_string())
});
```

- `kernel/tool/src/lib.rs:331-401` — `check_tool_security`, `check_allowed_path`, `check_hardline_block` — all fully implemented and functional.

- `kernel/config/src/lib.rs` — `SecurityConfig` has `sandbox_enabled`, `auto_approve_plugins`, etc. Check if there's already a `ToolSecurityConfig` equivalent in the config model. If not, one needs to be added or derived from the existing security config.

- `kernel/gateway/src/runtime/agent_runtime.rs:186-215` — `AgentRuntime::build()` constructs the `AgentHarness`. This is where `with_security_config()` should be called.

## Commands you will need

| Purpose | Command | Expected on success |
|---------|---------|---------------------|
| Build | `cargo build -p gateway` | exit 0 |
| Test | `cargo test -p gateway` | all tests pass |
| Lint | `cargo clippy -p gateway -- -D warnings` | exit 0 |
| Workspace check | `cargo build --workspace` | exit 0 |

## Scope

**In scope** (the only files you should modify):
- `kernel/gateway/src/runtime/agent_harness.rs` — remove `#[allow(dead_code)]`, possibly make `security_config` always set
- `kernel/gateway/src/runtime/agent_runtime.rs` — populate `ToolSecurityConfig` from config and pass to harness builder
- `kernel/config/src/lib.rs` — add `ToolSecurityConfig` fields if not present
- `kernel/tool/src/lib.rs` — possibly add a `ToolSecurityConfig::permissive()` default for backward compatibility

**Out of scope** (do NOT touch):
- The security check logic itself (`check_tool_security`, `check_hardline_block`, `check_allowed_path`) — it works.
- Plugin sandboxing — different layer.
- The capability approval system — different layer.

## Git workflow

- Branch: `advisor/010-tool-security-config-wiring`
- Commit messages (one per logical step):
  - `feat(config): add tool_security section to SecurityConfig`
  - `feat(gateway): wire ToolSecurityConfig from config into AgentHarness`
  - `refactor(gateway): remove dead_code annotation from with_security_config`
- Do NOT push or open a PR unless instructed.

## Steps

### Step 1: Investigate the config model

Read `kernel/config/src/lib.rs` and determine:
- Is there already a `tool_security` section in `SecurityConfig` or `RuntimeConfig`?
- What fields does `ToolSecurityConfig` require? (Look at `kernel/tool/src/lib.rs` for the struct definition)
- Map config fields to `ToolSecurityConfig` fields: `allowed_paths`, `network_allowed`, `command_allowlist`, `allowlist_enabled`.

### Step 2: Add permissive default

In `kernel/tool/src/lib.rs`, add a method to `ToolSecurityConfig`:
```rust
impl ToolSecurityConfig {
    /// A permissive config that allows everything (backward-compatible default).
    pub fn permissive() -> Self {
        Self {
            allowed_paths: vec![],
            network_allowed: true,
            command_allowlist: vec![],
            allowlist_enabled: false,
        }
    }
}
```

When `allowlist_enabled` is `false`, `check_tool_security` should pass everything (add an early return at the top of `check_tool_security`).

### Step 3: Add tool_security config section (if needed)

If config doesn't have tool security fields, add to `SecurityConfig`:
```rust
pub struct ToolSecurityConfig {
    /// When false, all security checks pass (backward compatible).
    pub enabled: bool,
    /// Directories tools are allowed to read/write.
    pub allowed_paths: Vec<PathBuf>,
    /// Whether HTTP tools can make network requests.
    pub network_allowed: bool,
    /// Commands that ExecTool is allowed to run (empty = all allowed when enabled=false).
    pub command_allowlist: Vec<String>,
}
```

**Verify**: `cargo build -p config` → exit 0

### Step 4: Wire into AgentRuntime::build()

In `agent_runtime.rs:build()`, after constructing the harness builder, add:
```rust
let tool_security = ToolSecurityConfig {
    allowed_paths: config.security.allowed_paths.clone(),
    network_allowed: config.security.network_allowed,
    command_allowlist: config.security.command_allowlist.clone(),
    allowlist_enabled: config.security.tool_security_enabled,
};
let harness = harness.with_security_config(tool_security);
```

Remove `#[allow(dead_code)]` from `with_security_config()`.

**Verify**: `cargo build -p gateway` → exit 0

### Step 5: Add an early-return for disabled security

In `kernel/tool/src/lib.rs`, at the top of `check_tool_security`:
```rust
pub fn check_tool_security(config: &ToolSecurityConfig, params: &Value) -> AmanResult<()> {
    if !config.allowlist_enabled {
        return Ok(()); // Backward-compatible: security checks are opt-in
    }
    // ... existing checks ...
}
```

### Step 6: Run tests and lint

**Verify**: `cargo test -p gateway` → all tests pass
**Verify**: `cargo test -p tool` → all tests pass
**Verify**: `cargo clippy -p gateway -- -D warnings` → exit 0
**Verify**: `cargo clippy -p tool -- -D warnings` → exit 0

## Test plan

- Add a test in `kernel/tool/src/lib.rs` tests: verify that `check_tool_security` with `allowlist_enabled: false` passes all calls, and with `allowlist_enabled: true` blocks disallowed paths/commands.
- Add a test in `kernel/gateway/src/runtime/agent_harness.rs` (if test infrastructure exists): verify that a harness built with `with_security_config()` passes the config through to `execute()`.

## Done criteria

- [ ] `cargo build --workspace` exits 0
- [ ] `cargo test -p gateway` exits 0; `cargo test -p tool` exits 0
- [ ] `cargo clippy -p gateway -- -D warnings` exits 0
- [ ] `#[allow(dead_code)]` is removed from `with_security_config`
- [ ] `AgentRuntime::build()` passes a `ToolSecurityConfig` to the harness builder
- [ ] `check_tool_security` has an early-return when `allowlist_enabled` is false (backward compatible)
- [ ] New config fields are documented with doc comments

## STOP conditions

Stop and report back (do not improvise) if:

- The config model has been significantly refactored and `SecurityConfig` no longer exists or has moved.
- `ToolSecurityConfig` struct definition differs from what's described here — adapt the plan to the actual struct.
- Enabling tool security by default breaks existing integrations (add a config migration note instead of forcing it).
- Any existing test fails after the change.

## Maintenance notes

- This plan makes tool security **opt-in** (`allowlist_enabled: false` by default) for backward compatibility. A future plan can flip the default to `true` after a deprecation period.
- The hardline blocks (`rm -rf /`, fork bombs, `dd`) always run regardless of `allowlist_enabled` — these are non-configurable safety measures.
- If an agent's SOUL or skill requires specific paths/commands, those should be declared in the agent config's `tool_security` section.
