use config::AgentConfig;
use runtime::{serve, AgentRuntimeBuilder, HttpServerConfig};
use std::time::Duration;

fn url(addr: std::net::SocketAddr, path: &str) -> String {
    format!("http://{}{}", addr, path)
}

#[tokio::test]
async fn health_and_control_endpoints_work_with_token() {
    let config = AgentConfig::default();
    let runtime = AgentRuntimeBuilder::new(config)
        .with_bind_addr("127.0.0.1:0".parse().expect("addr"))
        .with_api_token(Some("test-token".to_owned()))
        .build()
        .expect("build runtime");

    let server = serve(
        runtime.clone(),
        HttpServerConfig {
            bind: runtime.bind_addr(),
        },
    )
    .await
    .expect("serve");
    let addr = server.local_addr();

    let client = reqwest::Client::builder()
        .no_proxy()
        .build()
        .expect("client");
    let ready_before = client
        .get(url(addr, "/health/ready"))
        .send()
        .await
        .expect("ready request");
    assert_eq!(ready_before.status(), reqwest::StatusCode::SERVICE_UNAVAILABLE);

    let start_unauth = client
        .post(url(addr, "/agent/start"))
        .send()
        .await
        .expect("start request");
    assert_eq!(start_unauth.status(), reqwest::StatusCode::UNAUTHORIZED);

    let start = client
        .post(url(addr, "/agent/start"))
        .bearer_auth("test-token")
        .send()
        .await
        .expect("start request");
    assert_eq!(start.status(), reqwest::StatusCode::OK);

    let ready_after = client
        .get(url(addr, "/health/ready"))
        .send()
        .await
        .expect("ready request");
    assert_eq!(ready_after.status(), reqwest::StatusCode::OK);

    let start_again = client
        .post(url(addr, "/agent/start"))
        .bearer_auth("test-token")
        .send()
        .await
        .expect("start request");
    assert_eq!(start_again.status(), reqwest::StatusCode::OK);

    let shutdown = client
        .post(url(addr, "/agent/shutdown"))
        .bearer_auth("test-token")
        .header("x-aman-confirm", "yes")
        .send()
        .await
        .expect("shutdown request");
    assert_eq!(shutdown.status(), reqwest::StatusCode::OK);

    let ready_after_shutdown = client
        .get(url(addr, "/health/ready"))
        .send()
        .await
        .expect("ready request");
    assert_eq!(
        ready_after_shutdown.status(),
        reqwest::StatusCode::SERVICE_UNAVAILABLE
    );

    server.shutdown();
}

#[tokio::test]
async fn shutdown_interrupts_startup_and_start_returns_conflict() {
    let config = AgentConfig::default();
    let runtime = AgentRuntimeBuilder::new(config)
        .with_bind_addr("127.0.0.1:0".parse().expect("addr"))
        .with_api_token(Some("test-token".to_owned()))
        .with_startup_pause(Duration::from_millis(50))
        .build()
        .expect("build runtime");

    let server = serve(
        runtime.clone(),
        HttpServerConfig {
            bind: runtime.bind_addr(),
        },
    )
    .await
    .expect("serve");
    let addr = server.local_addr();

    let client = reqwest::Client::builder()
        .no_proxy()
        .build()
        .expect("client");
    let start_fut = tokio::spawn({
        let client = client.clone();
        async move {
            client
                .post(url(addr, "/agent/start"))
                .bearer_auth("test-token")
                .send()
                .await
                .expect("start request")
                .status()
        }
    });

    tokio::time::sleep(Duration::from_millis(10)).await;

    let shutdown_status = client
        .post(url(addr, "/agent/shutdown"))
        .bearer_auth("test-token")
        .header("x-aman-confirm", "yes")
        .send()
        .await
        .expect("shutdown request")
        .status();

    let start_status = start_fut.await.expect("start join");

    assert_eq!(shutdown_status, reqwest::StatusCode::OK);
    assert_eq!(start_status, reqwest::StatusCode::CONFLICT);

    server.shutdown();
}
