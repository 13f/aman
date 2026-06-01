// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

use kernel::retry::RetryBackoff;
use secret::{SecretBackend, SecretResolver, SecretResolverConfig};

struct StaticBackend;

impl SecretBackend for StaticBackend {
    fn get(&self, key: &str) -> kernel::AmanResult<Option<String>> {
        if key == "ROTATE_KEY" {
            Ok(Some("value-v2".to_string()))
        } else {
            Ok(None)
        }
    }

    fn priority(&self) -> u32 {
        10
    }

    fn name(&self) -> &'static str {
        "static"
    }
}

#[test]
fn rotate_with_grace_period_updates_cache_and_audit() {
    let mut resolver = SecretResolver::new()
        .with_backend(Box::new(StaticBackend))
        .with_config(SecretResolverConfig {
            retry_count: 3,
            retry_backoff: RetryBackoff::Immediate,
            cache_ttl_ms: 300_000,
            cache_fallback: None,
        });

    resolver
        .rotate(&["ROTATE_KEY".to_string()], "integration", 60)
        .expect("rotate should succeed");
    let last = resolver
        .audit_log()
        .last()
        .expect("rotate should audit");
    assert_eq!(last.trigger_source, "integration");
    assert!(last.fingerprint_created_at_ms >= last.resolved_at_ms + 60_000);

    let mut payload = serde_json::json!({ "k": "${ROTATE_KEY}" });
    resolver.resolve_all(&mut payload).expect("resolve should work");
    assert_eq!(payload["k"], "value-v2");
}
