# Plugin Development Guide

Plugins extend Aman with custom Skills, Tools, Event Sources, and lifecycle hooks. They can be loaded as shared libraries, WASM modules, or subprocesses.

## Plugin Manifest (`plugin.yaml`)

```yaml
name: invoice-processor
version: 1.2.0
description: Invoice processing plugin with OCR and Slack integration
author: ops
depends_on:
  - name: common-utils
    version: ">=1.0 <2.0"
lifecycle:
  isolation: in_process     # in_process | subprocess | wasm
  load_timeout_sec: 30
exports:
  skills: [ocr, slack-notify]
  tools: [ocr-extract]
  event_sources: [invoice-watch]
```

## Plugin Structure

```
my-plugin/
├── plugin.yaml
├── src/
│   ├── lib.rs              # Rust implementation (in_process)
│   └── ...
└── README.md
```

## WASM Plugins

WASM plugins export specific functions that Aman calls at runtime:

```rust
// Required exports for WASM plugin
#[no_mangle]
pub extern "C" fn aman_skill_execute(input: *const u8, len: usize) -> u64 {
    // ... implementation
}

#[no_mangle]
pub extern "C" fn aman_skill_on_load() -> u32 {
    // ... initialization
}

#[no_mangle]
pub extern "C" fn aman_skill_on_unload() -> u32 {
    // ... cleanup
}
```

## Plugin Lifecycle

1. **Discovery**: Aman scans `plugins.dir` from config
2. **Dependency resolution**: Topological sort + cycle detection
3. **Load**: `on_load()` called for each plugin (topological order)
4. **Enable**: Plugin registers its exports
5. **Unload**: Reverse topological order with `on_unload()`
6. **Dependency notification**: `on_dependency_unloading()` before unloading a dependency

## Installation

```bash
# Install from tar.gz archive
curl -X POST http://localhost:9090/plugin/install \
  -F "file=@my-plugin.tar.gz"

# Via CLI
aman plugin install my-plugin.tar.gz

# Enable/disable
aman plugin enable invoice-processor
aman plugin disable invoice-processor

# List installed plugins
aman plugin list
```

## Isolation Modes

| Mode | Description | Use Case |
|---|---|---|
| `in_process` | Arc-based interface isolation | Trusted, performance-sensitive plugins |
| `subprocess` | JSON-RPC over stdin/stdout | Untrusted plugins, fault isolation |
| `wasm` | wasmtime runtime sandbox | Sandboxed execution, portable plugins |

## SOUL Integration

Plugins receive SOUL context during `on_load()` to enforce identity boundaries. The `Soul` object is injected into `PluginContext` and can be used for permission checks:

```rust
fn on_load(context: &PluginContext) -> Result<()> {
    if context.soul.boundaries.contains(&"no-network".into()) {
        // restrict network access
    }
    Ok(())
}
```
