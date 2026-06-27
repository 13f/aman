# Plan 001: Fix SecretCache Default producing all-zero encryption key

> **Executor instructions**: Follow this plan step by step. Run every verification command and confirm the expected result before moving to the next step. If anything in the "STOP conditions" section occurs, stop and report — do not improvise. When done, update the status row for this plan in `plans/README.md` — unless a reviewer dispatched you and told you they maintain the index.
>
> **Drift check (run first)**: `git diff --stat ec2cddf..HEAD -- kernel/secret/src/lib.rs`
> If this file changed since the plan was written, compare the "Current state" excerpts against the live code before proceeding; on a mismatch, treat it as a STOP condition.

## Status

- **Priority**: P0
- **Effort**: S
- **Risk**: LOW
- **Depends on**: none
- **Category**: security
- **Planned at**: commit `ec2cddf`, 2026-06-16

## Why this matters

`SecretCache` provides in-memory secret caching with AES-256-GCM encryption. Its `new()` constructor correctly generates a random 32-byte encryption key from `OsRng`. However, the struct also derives `Default`, which Rust expands to `key: [0u8; 32]` — an all-zero encryption key. Any code path that constructs `SecretCache` via `Default::default()` instead of `SecretCache::new()` produces a cache that encrypts with a zero key, making stored secrets trivially reversible. This is a silent security degradation: the code compiles, runs, and appears to work, but provides zero cryptographic protection.

## Current state

- `kernel/secret/src/lib.rs:382-387` — struct derives `Default`:
```rust
#[derive(Debug, Default)]
pub struct SecretCache {
    key: [u8; 32],
    ttl_ms: u64,
    entries: HashMap<String, (EncryptedMemory<String>, u128)>,
}
```

- `kernel/secret/src/lib.rs:389-401` — constructor correctly generates random key:
```rust
impl SecretCache {
    pub fn new(ttl_ms: u64) -> Self {
        let mut key = [0u8; 32];
        let mut rng = OsRng;
        if rng.try_fill_bytes(&mut key).is_err() {
            key = *blake3::hash(&now_ms().to_le_bytes()).as_bytes();
        }
        Self {
            key,
            ttl_ms,
            entries: HashMap::new(),
        }
    }
```

- **Conventions**: The crate follows standard Rust idioms. Tests live in the same file under `#[cfg(test)] mod tests`. Error handling uses `AmanResult<T>`. The `SecretCache` is used by `kernel/secret/src/lib.rs`'s `SecretResolver` to cache decrypted secrets.

## Commands you will need

| Purpose | Command | Expected on success |
|---------|---------|---------------------|
| Build | `cargo build -p secret` | exit 0 |
| Test | `cargo test -p secret` | all tests pass |
| Lint | `cargo clippy -p secret -- -D warnings` | exit 0 |

## Scope

**In scope** (the only files you should modify):
- `kernel/secret/src/lib.rs`

**Out of scope** (do NOT touch):
- Any other crate — this is a single-file fix.
- The `SecretCache::new()` constructor — it works correctly and needs no changes.
- The `EncryptedMemory` type or the encryption/decryption logic.

## Git workflow

- Branch: `advisor/001-secretcache-default-key`
- Commit message: `fix(secret): replace derive(Default) with manual impl that generates random key`
- Do NOT push or open a PR unless instructed.

## Steps

### Step 1: Remove `Default` from the derive list, add manual impl

Change `kernel/secret/src/lib.rs:382` from:
```rust
#[derive(Debug, Default)]
pub struct SecretCache {
```
to:
```rust
#[derive(Debug)]
pub struct SecretCache {
```

Then add a manual `Default` impl immediately before `impl SecretCache` (before line 389):
```rust
impl Default for SecretCache {
    fn default() -> Self {
        Self::new(300_000) // 5-minute default TTL, matching typical cache usage
    }
}
```

Use `300_000` (5 minutes) as the default TTL because:
- The `SecretResolver` in the same file creates `SecretCache::new(300_000)` at line ~140
- This matches the existing convention in the crate
- A 5-minute TTL is a reasonable default for secret caching

**Verify**: `cargo build -p secret` → exit 0 (no compile errors)

### Step 2: Add a test that verifies the default key is randomized

Add to the existing test module in `kernel/secret/src/lib.rs`:

```rust
#[test]
fn secret_cache_default_produces_random_key() {
    let cache1 = SecretCache::default();
    let cache2 = SecretCache::default();
    // Two default-constructed caches must have different keys
    assert_ne!(cache1.key, cache2.key);
    // The key must not be all zeros
    assert_ne!(cache1.key, [0u8; 32]);
}
```

**Verify**: `cargo test -p secret -- secret_cache_default_produces_random_key` → test passes

### Step 3: Run full test suite and lint

**Verify**: `cargo test -p secret` → all tests pass
**Verify**: `cargo clippy -p secret -- -D warnings` → exit 0

## Test plan

- **New test**: `secret_cache_default_produces_random_key` — verifies that two `Default::default()` calls produce different, non-zero keys.
- **Existing tests**: All existing `secret` crate tests must continue to pass. No existing tests should need modification since they use `SecretCache::new()`, not `Default::default()`.

## Done criteria

- [ ] `cargo build -p secret` exits 0
- [ ] `cargo test -p secret` exits 0; `secret_cache_default_produces_random_key` passes
- [ ] `cargo clippy -p secret -- -D warnings` exits 0
- [ ] `grep -n 'derive.*Default' kernel/secret/src/lib.rs` shows `Default` is no longer derived on `SecretCache`
- [ ] `grep -n 'impl Default for SecretCache' kernel/secret/src/lib.rs` finds the manual impl
- [ ] No files outside `kernel/secret/src/lib.rs` are modified (`git status`)

## STOP conditions

Stop and report back (do not improvise) if:

- The code at line 382 of `kernel/secret/src/lib.rs` doesn't match the `#[derive(Debug, Default)]` excerpt (the codebase has drifted).
- Any existing test in the `secret` crate fails after the change.
- `SecretCache` is used via `Default::default()` somewhere else in the crate (grep for it) — that caller may depend on the zero-key behavior (unlikely, but check).

## Maintenance notes

- The `300_000` default TTL is chosen to match the existing `SecretResolver` usage. If the secret resolver changes its TTL, the default here should stay consistent or be configurable.
- If `mlock` or `zeroize` behavior is added to `SecretCache` in the future, ensure the `Default` impl triggers those protections too (currently `new()` handles all initialization, so it's safe).
- A reviewer should verify that no other `#[derive(Default)]` in the codebase hides a similar issue (structs with security-sensitive fields that need explicit initialization).
