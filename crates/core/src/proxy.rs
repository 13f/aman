#![forbid(unsafe_code)]
// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! Proxy URL detection and normalization for outbound HTTP clients.
//!
//! Detects proxy settings from the standard environment variables
//! (`ALL_PROXY`, `HTTPS_PROXY`, `https_proxy`) and converts `socks5://`
//! to `socks5h://` so DNS is resolved by the proxy rather than locally.
//! This is required for external services (e.g. `api.telegram.org`,
//! Tavily, Brave Search) when behind the GFW.

/// Read the proxy URL from environment variables and convert `socks5://`
/// to `socks5h://`.
///
/// Checks `ALL_PROXY` first, then `HTTPS_PROXY`, then `https_proxy`.
/// Returns `None` if no proxy is configured.
///
/// ## Why `socks5h://`?
///
/// | scheme | DNS resolution | GFW-safe? |
/// |--------|---------------|-----------|
/// | `socks5://` | local machine | ❌ — DNS leaks, may resolve poisoned IPs |
/// | `socks5h://` | proxy server | ✅ — DNS goes through the tunnel |
///
/// HTTP proxies (`http://…`) are returned unchanged — they inherently resolve
/// DNS on the proxy side.
#[must_use]
pub fn detect_proxy_url() -> Option<String> {
    std::env::var("ALL_PROXY")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| std::env::var("HTTPS_PROXY").ok().filter(|s| !s.is_empty()))
        .or_else(|| std::env::var("https_proxy").ok().filter(|s| !s.is_empty()))
        .map(ensure_socks5h)
}

/// If `url` is a `socks5://` (but not `socks5h://`) URL, replace the scheme
/// with `socks5h://`. All other URLs (HTTP proxies, plain `socks5h://`) are
/// returned unchanged.
#[must_use]
pub fn ensure_socks5h(url: String) -> String {
    if url.starts_with("socks5://") && !url.starts_with("socks5h://") {
        url.replacen("socks5://", "socks5h://", 1)
    } else {
        url
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ensure_socks5h_converts() {
        assert_eq!(
            ensure_socks5h("socks5://127.0.0.1:10808".into()),
            "socks5h://127.0.0.1:10808"
        );
    }

    #[test]
    fn test_ensure_socks5h_already_h() {
        assert_eq!(
            ensure_socks5h("socks5h://127.0.0.1:10808".into()),
            "socks5h://127.0.0.1:10808"
        );
    }

    #[test]
    fn test_ensure_socks5h_http_unchanged() {
        assert_eq!(
            ensure_socks5h("http://127.0.0.1:10808".into()),
            "http://127.0.0.1:10808"
        );
    }

    #[test]
    fn test_ensure_socks5h_https_unchanged() {
        assert_eq!(
            ensure_socks5h("https://127.0.0.1:10808".into()),
            "https://127.0.0.1:10808"
        );
    }
}
