# Plugin Development Guide

Plugins extend aman with custom Skills, Tools, Event Sources, Hooks, Memory Providers, and lifecycle hooks. They can be loaded as in-process Plugin trait implementations, WASM modules via wasmtime, or subprocesses communicating over JSON-RPC 2.0.

## Plugin Manifest (`plugin.yaml`)

```yaml
name: invoice-processor
version: "1.2.0"
isolation: in_process           # in_process | subprocess | wasm
depends_on:
  - name: common-utils
    version_range: ">=1.0 <2.0"
lifecycle:
  auto_start: true
exports:
  skills: [ocr, slack-notify]
  tools: [ocr-extract]
  event_sources: [invoice-watch]
  hooks: [audit-log]
  memory_providers: []
```

Additional optional fields (see example plugins in `crates/plugins/` and `predefined/plugins/`):

```yaml
description: "Human-readable description"
author: "your-name"
config_schema:
  type: object
  properties:
    enabled:
      type: boolean
subprocess:
  command: python3
  args: ["main.py"]
  cwd: /path/to/plugin
  timeout_ms: 30000
wasm_path: "plugin.wasm"
capabilities: ["chat", "session_management"]
ui:
  pages: ["team"]
  events: ["team:work_item.updated"]
runtime: python3              # Script runtime for subprocess plugins
min_version: ">=3.11"        # Minimum runtime version
entrypoint: "main.py"        # Entrypoint script relative to plugin dir
```

### Important field notes

- `version` must be a valid semver string (e.g. `"1.2.0"`). Quoting is recommended to avoid YAML type coercion.
- `depends_on[n].version_range` uses semver range syntax (e.g. `">=1.0 <2.0"`). The field is `version_range`, not `version`.
- `isolation` is a **top-level** field, not nested under `lifecycle`. The `PluginLifecycleConfig` only has `auto_start: bool`.
- `description`, `author`, and any other unknown YAML keys are silently accepted but have no effect at the manifest level.

## Plugin Structure

For **in_process** plugins (Rust crate with the `Plugin` trait):

```
my-plugin/
├── Cargo.toml                 # Rust crate manifest
├── plugin.yaml                # aman plugin manifest
├── src/
│   └── lib.rs                 # Plugin trait implementation
└── README.md
```

For **subprocess** plugins (script or binary, communicates via stdio JSON-RPC):

```
my-plugin/
├── plugin.yaml                # aman plugin manifest (isolation: subprocess)
├── main.py                    # Entrypoint script (or main.sh, index.js, ...)
└── README.md
```

For **WASM** plugins (compiled .wasm module):

```
my-plugin/
├── plugin.yaml                # aman plugin manifest (isolation: wasm)
├── plugin.wasm                # Compiled WASM module
└── README.md
```

## WASM Plugins

WASM plugins export specific functions that aman calls at runtime via wasmtime. All three exports must return `i32` (zero indicates success):

```wat
;; Required exports
(func (export "aman_skill_on_load") (result i32) ...)
(func (export "aman_skill_on_unload") (result i32) ...)
(func (export "aman_skill_execute") (result i32) ...)
```

The equivalent Rust source (when targeting wasm32-unknown-unknown):

```rust
// These become the exports shown above when compiled to WASM.
// Return 0 for success, non-zero for error.
#[no_mangle]
pub extern "C" fn aman_skill_on_load() -> i32 { 0 }

#[no_mangle]
pub extern "C" fn aman_skill_on_unload() -> i32 { 0 }

#[no_mangle]
pub extern "C" fn aman_skill_execute() -> i32 { 0 }
```

Note: The WASM runtime currently does not support bridging to manifest `exports` (skills/tools/event_sources). Calling `aman_skill_execute` on a WASM plugin that has manifest exports will return an error. WASM plugins are limited to the three lifecycle exports above.

## Plugin Lifecycle

1. **Discovery**: aman scans `plugins.dir` from config for subprocess plugins (`plugin.yaml` with `isolation: subprocess`). In-process plugins are hard-coded (e.g., `memory-store`, `info-hub`) or passed programmatically.
2. **Dependency resolution**: Topological sort + cycle detection against `depends_on`.
3. **Load**: `on_load()` called for each plugin in topological order. For in-process plugins, exports are registered immediately after a successful `on_load()`.
4. **Enable → Running**: After loading, the plugin transitions through `Loaded` → `Enabled` → `Running` automatically.
5. **Pause / Resume**: A running plugin can be paused (`Paused`) and resumed back to `Running`.
6. **Disable / Enable**: A running plugin can be disabled (`Disabled`) and re-enabled (back to `Running`).
7. **Dependency notification**: When unloading a plugin, `on_dependency_unloading()` is called on each dependent before the dependency is unloaded.
8. **Unload**: Reverse topological order with `on_unload()` and a configurable timeout (default 30s). After 3 consecutive timeouts the plugin is marked `unstable`.
9. **Shutdown**: After successful unload, the plugin's state is `Shutdown`.

Lifecycle states in the code: `Loaded` → `Enabled` → `Running` → `Paused` / `Disabled` / `Shutdown`.

## Installation

Available CLI subcommands (see `aman plugin`):

```
aman plugin list
aman plugin enable --name <name>
aman plugin disable --name <name>
aman plugin uninstall --name <name>
aman plugin install --file <path.tar.gz>
```

### Install via HTTP API

```bash
# Install from tar.gz archive (multipart field name is "plugin")
curl -X POST http://localhost:9090/api/v1/plugin/install \
  -F "plugin=@my-plugin.tar.gz"
```

The HTTP API path is mounted by the gateway runtime. The exact prefix depends on the gateway configuration.

## Isolation Modes

| Mode | Description | Use Case |
|---|---|---|
| `in_process` | `Box<dyn Plugin>` trait implementation in the same process | Trusted, performance-sensitive plugins |
| `subprocess` | Long-lived subprocess with JSON-RPC 2.0 over stdin/stdout (via `SubprocessPluginBridge`) | Untrusted plugins, fault isolation, script-based plugins |
| `wasm` | wasmtime runtime sandbox | Sandboxed execution, portable plugins |

The subprocess bridge supports bidirectional communication. Plugins can make calls back to the host (register routes, subscribe to events, emit events, push work items, query agents, register workflows) via JSON-RPC methods like `aman.register_routes`, `aman.subscribe_events`, `aman.emit_event`, `aman.push_work_item`, `aman.get_agents`, `aman.register_workflow`.

## Plugin Trait (Rust)

In-process plugins implement the `Plugin` trait from `kernel::plugin`:

```rust
#[async_trait]
pub trait Plugin: Send + Sync {
    fn name(&self) -> &str;
    fn version(&self) -> &Version;
    fn dependencies(&self) -> &[PluginDependency];

    async fn on_load(&mut self, ctx: PluginContext) -> AmanResult<()>;
    async fn on_unload(&mut self) -> AmanResult<()>;
    async fn on_dependency_unloading(&self, dep_name: &str) -> AmanResult<()>;

    fn event_sources(&self) -> Vec<Arc<dyn EventSource>>;
    fn skills(&self) -> Vec<Arc<dyn Skill>>;
    fn tools(&self) -> Vec<Arc<dyn Tool>>;
    fn hooks(&self) -> Vec<Arc<dyn Hook>>;              // default: vec![]
    fn memory_providers(&self) -> Vec<Arc<dyn MemoryProvider>>; // default: vec![]
    fn routes(&self) -> Option<axum::Router<()>> { None } // default: None
}
```

### `PluginContext`

The `PluginContext` passed to `on_load()` provides resource tracking capabilities:

```rust
pub struct PluginContext {
    pub base: BaseContext,           // trace_id, timeout_ms, labels, extensions
    pub plugin_name: Option<String>,
    pub resource_tracker: Arc<Mutex<PluginResourceTracker>>, // serialization-skipped
}
```

Use `ctx.track_fd(fd)`, `ctx.track_db(url)`, `ctx.track_path(path)` to register resources that will be cleaned up if `on_load()` fails.
