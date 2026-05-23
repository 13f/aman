// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

use secret::{EnvSecretBackend, SecretResolver};

#[test]
fn env_backend_resolves_process_env_placeholder() {
    let expected = std::env::var("PATH").expect("PATH should exist in test environment");
    let mut resolver = SecretResolver::new().with_backend(Box::new(EnvSecretBackend));
    let mut payload = serde_json::json!({
        "env_path": "${PATH}"
    });

    let resolved_keys = resolver
        .resolve_all(&mut payload)
        .expect("env backend should resolve PATH");
    assert_eq!(resolved_keys, vec!["PATH".to_string()]);
    assert_eq!(payload["env_path"], expected);
    assert!(
        resolver.audit_log().iter().any(|record| {
            record.affected_keys.iter().any(|key| key == "PATH")
                && record.backend_hits.iter().any(|backend| backend == "env")
        }),
        "resolution audit should include PATH and env backend"
    );
}
