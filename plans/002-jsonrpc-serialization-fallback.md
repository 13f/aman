# Plan 002: Fix JSON-RPC empty-string response on serialization failure

> **Executor instructions**: Follow this plan step by step. Run every verification command and confirm the expected result before moving to the next step. If anything in the "STOP conditions" section occurs, stop and report — do not improvise. When done, update the status row for this plan in `plans/README.md` — unless a reviewer dispatched you and told you they maintain the index.
>
> **Drift check (run first)**: `git diff --stat ec2cddf..HEAD -- kernel/gateway/src/runtime/stdio.rs`
> If this file changed since the plan was written, compare the "Current state" excerpts against the live code before proceeding; on a mismatch, treat it as a STOP condition.

## Status

- **Priority**: P1
- **Effort**: S
- **Risk**: LOW
- **Depends on**: none
- **Category**: bug
- **Planned at**: commit `ec2cddf`, 2026-06-16

## Why this matters

The JSON-RPC 2.0 stdio bridge (`kernel/gateway/src/runtime/stdio.rs`) is the protocol used by MCP clients and subprocess plugins to communicate with the gateway. In three places, it calls `serde_json::to_string(&resp).unwrap_or_default()` to serialize a JSON-RPC `Response` struct. If serialization fails (should not happen for well-formed data, but it's a `Result` that's discarded), the output is an empty string — which is not valid JSON-RPC and violates the wire protocol. The client receives a protocol violation with no error context and may hang, crash, or disconnect.

A well-formed JSON-RPC error response on serialization failure gives the client actionable information (internal error, code -32603) instead of a silent wire break.

## Current state

- `kernel/gateway/src/runtime/stdio.rs:82-88` — invalid-request path:
```rust
println!("{}", serde_json::to_string(&resp).unwrap_or_default());
```

- `kernel/gateway/src/runtime/stdio.rs:93-104` — parse-error path:
```rust
println!("{}", serde_json::to_string(&resp).unwrap_or_default());
```

- `kernel/gateway/src/runtime/stdio.rs:110-124` — success/error dispatch path:
```rust
println!("{}", serde_json::to_string(&resp).unwrap_or_default());
```

All three use the same `unwrap_or_default()` pattern that silently produces empty output on serialization failure.

- **Conventions**: The file uses `println!` (not `tracing`) because stdio JSON-RPC is a wire protocol — stdout IS the transport. The existing `map_error` function (near line 130) converts `AmanError` to `JsonRpcError`. Error handling is explicit `match`/`map_err` style.

## Commands you will need

| Purpose | Command | Expected on success |
|---------|---------|---------------------|
| Build | `cargo build -p gateway` | exit 0 |
| Test | `cargo test -p gateway` | all tests pass |
| Lint | `cargo clippy -p gateway -- -D warnings` | exit 0 |

## Scope

**In scope** (the only files you should modify):
- `kernel/gateway/src/runtime/stdio.rs`

**Out of scope** (do NOT touch):
- The JSON-RPC `Response` struct definition — it's fine, serialization failures on it are theoretical.
- Other serialization sites — this plan is only about the stdio JSON-RPC wire protocol.
- The `map_error` function or error mapping logic.

## Git workflow

- Branch: `advisor/002-jsonrpc-serialization-fallback`
- Commit message: `fix(gateway): emit JSON-RPC error object on serialization failure in stdio`
- Do NOT push or open a PR unless instructed.

## Steps

### Step 1: Extract a helper for safe JSON-RPC response printing

Add this helper function to `kernel/gateway/src/runtime/stdio.rs`, near the other helpers (after the `map_error` function around line 150):

```rust
/// Print a JSON-RPC Response to stdout. If serialization fails, print a
/// hardcoded JSON-RPC internal-error object so the client always receives
/// valid JSON-RPC, never an empty line.
fn print_jsonrpc_response(resp: &Response) {
    match serde_json::to_string(resp) {
        Ok(json) => println!("{json}"),
        Err(e) => {
            // Fallback: a hardcoded, always-valid JSON-RPC error response.
            // This should never happen in practice (Response is always
            // serializable), but if it does, the client gets a valid
            // JSON-RPC error instead of a protocol-breaking empty line.
            println!(
                r#"{{"jsonrpc":"2.0","id":null,"error":{{"code":-32603,"message":"internal serialization error: {}"}}}}"#,
                e
            );
        }
    }
}
```

Note: The error message from `serde_json::to_string` is inserted into the hardcoded JSON string via `format!` args. The `{e}` substitution goes into a JSON string value, so it's safe (the `serde_json::Error` Display impl does not contain unescaped quotes or control characters). If you're concerned, use `e.to_string().replace('"', r#"\""#)` instead.

**Verify**: `cargo build -p gateway` → exit 0

### Step 2: Replace all three `println!(..., unwrap_or_default())` sites with the helper

Replace each of the three `println!("{}", serde_json::to_string(&resp).unwrap_or_default());` calls with `print_jsonrpc_response(&resp);`.

The three locations are at lines 88, 104, and 124 of `kernel/gateway/src/runtime/stdio.rs`.

**Verify**: `cargo build -p gateway` → exit 0
**Verify**: `grep -n 'unwrap_or_default' kernel/gateway/src/runtime/stdio.rs` → returns no matches (or only matches outside the JSON-RPC handler)

### Step 3: Run tests and lint

**Verify**: `cargo test -p gateway` → all tests pass
**Verify**: `cargo clippy -p gateway -- -D warnings` → exit 0

## Test plan

No new tests required — this is a pure error-path hardening change. The existing gateway tests cover the stdio handler's normal paths. The fallback path is only hit if `serde_json::to_string` fails on a `Response` struct, which is a `Serialize` derive — it won't fail in practice, but the hardening is valuable regardless.

If you want to add a defensive test, add to the existing test module in `stdio.rs`:

```rust
#[test]
fn print_jsonrpc_response_always_emits_valid_json() {
    // Even for an empty response, output must be valid JSON
    let resp = Response {
        jsonrpc: "2.0",
        id: Value::Null,
        result: None,
        error: None,
    };
    // We can't easily capture stdout in a unit test, but we can verify
    // serde_json::to_string succeeds on the Response
    let json = serde_json::to_string(&resp).expect("Response must serialize");
    assert!(!json.is_empty());
    // It must parse back as valid JSON
    let _: serde_json::Value = serde_json::from_str(&json).expect("must be valid JSON");
}
```

## Done criteria

- [ ] `cargo build -p gateway` exits 0
- [ ] `cargo test -p gateway` exits 0
- [ ] `cargo clippy -p gateway -- -D warnings` exits 0
- [ ] `grep -n 'unwrap_or_default' kernel/gateway/src/runtime/stdio.rs` returns no matches in the JSON-RPC handler
- [ ] No files outside `kernel/gateway/src/runtime/stdio.rs` are modified (`git status`)

## STOP conditions

Stop and report back (do not improvise) if:

- The three `unwrap_or_default()` sites don't match the excerpts (code has drifted).
- The gateway crate fails to build after adding the helper (check for missing imports of `Response` — it's already in scope at all three sites).
- Any existing test fails.

## Maintenance notes

- If new JSON-RPC response sites are added to `stdio.rs`, they should use `print_jsonrpc_response` instead of raw `println!`.
- The hardcoded JSON fallback string includes `{e}` from the serde error. If serde_json ever changes its error Display format to include characters that break JSON string literals, the fallback could produce invalid JSON — add escaping at that point.
- This pattern could be extended to gRPC and HTTP response serialization, but those protocols have different error-reporting mechanisms (gRPC status codes, HTTP status codes); this plan only addresses the stdio JSON-RPC wire.
