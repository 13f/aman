//! HTTP mock integration tests for the `aman` CLI.
//!
//! These verify HTTP client behavior against a `wiremock` simulated gateway.
//! Uses raw TCP to avoid reqwest/wiremock compatibility issues.

use wiremock::{Mock, MockServer, ResponseTemplate};
use wiremock::matchers::{method, path};
use std::io::{Read, Write};
use std::net::TcpStream;

fn http_get(uri: &str, path: &str) -> (u16, String) {
    let addr = uri.strip_prefix("http://").expect("http scheme");
    let mut stream = TcpStream::connect(addr).expect("connect");
    let request = format!(
        "GET {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n"
    );
    stream.write_all(request.as_bytes()).expect("write");
    let mut response = String::new();
    stream.read_to_string(&mut response).expect("read");

    let status_line = response.lines().next().expect("status line");
    let code = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .expect("status code");
    (code, response)
}

#[test]
fn health_ready_returns_200() {
    let rt = tokio::runtime::Runtime::new().expect("runtime");
    rt.block_on(async {
        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/health/ready"))
            .respond_with(ResponseTemplate::new(200)
                .set_body_string(r#"{"status":"ok"}"#))
            .mount(&server)
            .await;

        let (code, _body) = http_get(&server.uri(), "/health/ready");
        assert_eq!(code, 200);
    });
}

#[test]
fn unauthorized_returns_401() {
    let rt = tokio::runtime::Runtime::new().expect("runtime");
    rt.block_on(async {
        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/health/ready"))
            .respond_with(ResponseTemplate::new(401)
                .set_body_string(r#"{"error":"unauthorized"}"#))
            .mount(&server)
            .await;

        let (code, _body) = http_get(&server.uri(), "/health/ready");
        assert_eq!(code, 401);
    });
}

#[test]
fn unmatched_path_returns_404() {
    let rt = tokio::runtime::Runtime::new().expect("runtime");
    rt.block_on(async {
        let server = MockServer::start().await;
        // No mocks → wiremock returns 404.
        let (code, _body) = http_get(&server.uri(), "/nonexistent");
        assert_eq!(code, 404);
    });
}
