// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! Shared ureq agent helpers for skipping system proxies on local/LAN hosts.
//!
//! System proxies can intercept local traffic even when `no_proxy` is set
//! inconsistently across shells. The helpers here detect local and private-
//! range hosts and build a proxy-bypassing agent for them.

/// Build a ureq Agent, skipping the proxy only for local/LAN hosts.
///
/// System proxies can intercept local traffic even when `no_proxy` is set
/// inconsistently across shells. We only bypass for hosts where it's safe:
/// localhost and private-range IPs (192.168.x.x, 10.x.x.x, 172.16-31.x.x).
pub(crate) fn agent_for(base_url: &str) -> ureq::Agent {
    let host = url_host(base_url);
    if is_local_or_private(&host) {
        ureq::AgentBuilder::new()
            .try_proxy_from_env(false)
            .build()
    } else {
        ureq::Agent::new()
    }
}

/// Extract the host portion from a URL string.
pub(crate) fn url_host(url: &str) -> String {
    url.trim_start_matches("http://")
        .trim_start_matches("https://")
        .split('/')
        .next()
        .unwrap_or("localhost")
        .split(':')
        .next()
        .unwrap_or("localhost")
        .to_owned()
}

/// True for localhost and RFC 1918 private addresses.
pub(crate) fn is_local_or_private(host: &str) -> bool {
    if host == "localhost" || host == "127.0.0.1" || host == "[::1]" || host == "::1" {
        return true;
    }
    if let Some(tail) = host.strip_prefix("192.168.") {
        return tail.split('.').all(|s| s.parse::<u8>().is_ok());
    }
    if let Some(tail) = host.strip_prefix("10.") {
        return tail.split('.').all(|s| s.parse::<u8>().is_ok());
    }
    if host.starts_with("172.")
        && let Some(second) = host.split('.').nth(1)
        && let Ok(n) = second.parse::<u8>()
    {
        return (16..=31).contains(&n);
    }
    false
}
