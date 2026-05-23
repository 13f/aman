#![forbid(unsafe_code)]
// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0


use config::{ConfigLoader, AgentConfig};
use gateway::runtime::{serve, AgentRuntimeBuilder, HttpServerConfig};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

#[tokio::main(flavor = "multi_thread")]
async fn main() {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    let Some(cmd) = args.first().map(String::as_str) else {
        print_usage();
        std::process::exit(2);
    };

    match cmd {
        "run" => {
            if let Err(code) = run_cmd(&args[1..]).await {
                std::process::exit(code);
            }
        }
        "health" => {
            if let Err(code) = health_cmd(&args[1..]).await {
                std::process::exit(code);
            }
        }
        "agent" => {
            if let Err(code) = agent_cmd(&args[1..]).await {
                std::process::exit(code);
            }
        }
        "metrics" => {
            if let Err(code) = metrics_cmd(&args[1..]).await {
                std::process::exit(code);
            }
        }
        "audit-log" => {
            if let Err(code) = audit_log_cmd(&args[1..]).await {
                std::process::exit(code);
            }
        }
        "event" => {
            if let Err(code) = event_cmd(&args[1..]).await {
                std::process::exit(code);
            }
        }
        "dlq" => {
            if let Err(code) = dlq_cmd(&args[1..]).await {
                std::process::exit(code);
            }
        }
        "source" => {
            if let Err(code) = source_cmd(&args[1..]).await {
                std::process::exit(code);
            }
        }
        "plugin" => {
            if let Err(code) = plugin_cmd(&args[1..]).await {
                std::process::exit(code);
            }
        }
        "skill" => {
            if let Err(code) = skill_cmd(&args[1..]).await {
                std::process::exit(code);
            }
        }
        "workflow" => {
            if let Err(code) = workflow_cmd(&args[1..]).await {
                std::process::exit(code);
            }
        }
        "cron" => {
            if let Err(code) = cron_cmd(&args[1..]).await {
                std::process::exit(code);
            }
        }
        "config" => {
            if let Err(code) = config_cmd(&args[1..]).await {
                std::process::exit(code);
            }
        }
        _ => {
            print_usage();
            std::process::exit(2);
        }
    }
}

async fn run_cmd(args: &[String]) -> Result<(), i32> {
    let mut config_path: Option<PathBuf> = None;
    let mut bind: SocketAddr = "127.0.0.1:8080".parse().expect("default bind");
    let mut api_token: Option<String> = std::env::var("AMAN_API_TOKEN").ok();
    let mut soul_path: Option<PathBuf> = None;
    let mut _daemon: bool = false;
    let mut _log_level: Option<String> = None;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--config" => {
                let path = args.get(i + 1).ok_or(2)?;
                config_path = Some(PathBuf::from(path));
                i += 2;
            }
            "--soul" => {
                let path = args.get(i + 1).ok_or(2)?;
                soul_path = Some(PathBuf::from(path));
                i += 2;
            }
            "--daemon" => {
                _daemon = true;
                i += 1;
            }
            "--log-level" => {
                let raw = args.get(i + 1).ok_or(2)?;
                _log_level = Some(raw.to_owned());
                i += 2;
            }
            "--bind" => {
                let raw = args.get(i + 1).ok_or(2)?;
                bind = raw.parse::<SocketAddr>().map_err(|_| 2)?;
                i += 2;
            }
            "--token" => {
                let raw = args.get(i + 1).ok_or(2)?;
                api_token = Some(raw.to_owned());
                i += 2;
            }
            _ => return Err(2),
        }
    }

    let config = load_config(config_path.as_ref()).map_err(|_| 1)?;
    let mut builder = AgentRuntimeBuilder::new(config)
        .with_bind_addr(bind)
        .with_api_token(api_token);
    if let Some(path) = soul_path {
        builder = builder.with_soul(path);
    }
    let runtime = builder.build().map_err(|_| 1)?;

    let server = serve(
        runtime.clone(),
        HttpServerConfig {
            bind: runtime.bind_addr(),
        },
    )
    .await
    .map_err(|_| 1)?;

    let addr = server.local_addr();
    println!("{addr}");

    #[cfg(unix)]
    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .map_err(|_| 1)?;

    #[cfg(unix)]
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        _ = sigterm.recv() => {}
    }

    #[cfg(not(unix))]
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
    }

    let _ = runtime.shutdown().await;
    server.shutdown();
    Ok(())
}

async fn health_cmd(args: &[String]) -> Result<(), i32> {
    let Some(sub) = args.first().map(String::as_str) else {
        return Err(2);
    };

    match sub {
        "ready" => {
            let (opts, _rest) = parse_api_opts(&args[1..])?;
            let client = build_client()?;
            let res = opts
                .apply_headers(client.get(opts.url("/health/ready")))
                .send()
                .await
                .map_err(|_| 1)?;
            if res.status() == reqwest::StatusCode::OK {
                Ok(())
            } else {
                Err(1)
            }
        }
        _ => Err(2),
    }
}

#[derive(Debug, Clone)]
struct ApiOpts {
    addr: SocketAddr,
    token: Option<String>,
    operator: Option<String>,
    confirm: bool,
}

impl ApiOpts {
    fn url(&self, path: &str) -> String {
        format!("http://{}{}", self.addr, path)
    }

    fn apply_headers(&self, mut req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        if let Some(token) = &self.token {
            req = req.bearer_auth(token);
        }
        if let Some(operator) = &self.operator {
            req = req.header("x-aman-operator", operator);
        }
        if self.confirm {
            req = req.header("x-aman-confirm", "yes");
        }
        req
    }
}

fn build_client() -> Result<reqwest::Client, i32> {
    reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(3))
        .build()
        .map_err(|_| 1)
}

fn parse_api_opts(args: &[String]) -> Result<(ApiOpts, Vec<String>), i32> {
    let mut addr: SocketAddr = "127.0.0.1:8080".parse().expect("default addr");
    let mut token: Option<String> = std::env::var("AMAN_API_TOKEN").ok();
    let mut operator: Option<String> = None;
    let mut confirm = false;

    let mut rest = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--addr" => {
                let raw = args.get(i + 1).ok_or(2)?;
                addr = raw.parse::<SocketAddr>().map_err(|_| 2)?;
                i += 2;
            }
            "--token" => {
                let raw = args.get(i + 1).ok_or(2)?;
                token = Some(raw.to_owned());
                i += 2;
            }
            "--operator" => {
                let raw = args.get(i + 1).ok_or(2)?;
                operator = Some(raw.to_owned());
                i += 2;
            }
            "--confirm" => {
                confirm = true;
                i += 1;
            }
            _ => {
                rest.push(args[i].clone());
                i += 1;
            }
        }
    }

    Ok((
        ApiOpts {
            addr,
            token,
            operator,
            confirm,
        },
        rest,
    ))
}

async fn agent_cmd(args: &[String]) -> Result<(), i32> {
    let Some(sub) = args.first().map(String::as_str) else {
        return Err(2);
    };
    let (opts, _rest) = parse_api_opts(&args[1..])?;
    let client = build_client()?;
    let res = match sub {
        "start" => opts
            .apply_headers(client.post(opts.url("/agent/start")))
            .send()
            .await
            .map_err(|_| 1)?,
        "shutdown" => opts
            .apply_headers(client.post(opts.url("/agent/shutdown")))
            .send()
            .await
            .map_err(|_| 1)?,
        _ => return Err(2),
    };
    if res.status().is_success() {
        Ok(())
    } else if res.status() == reqwest::StatusCode::CONFLICT {
        Err(3)
    } else {
        Err(1)
    }
}

async fn metrics_cmd(args: &[String]) -> Result<(), i32> {
    let (opts, _rest) = parse_api_opts(args)?;
    let client = build_client()?;
    let res = opts
        .apply_headers(client.get(opts.url("/metrics")))
        .send()
        .await
        .map_err(|_| 1)?;
    let status = res.status();
    let body = res.text().await.map_err(|_| 1)?;
    if status.is_success() {
        print!("{body}");
        Ok(())
    } else {
        eprint!("{body}");
        Err(1)
    }
}

async fn audit_log_cmd(args: &[String]) -> Result<(), i32> {
    let (opts, rest) = parse_api_opts(args)?;
    let mut action: Option<String> = None;
    let mut operator: Option<String> = None;
    let mut since_ms: Option<i64> = None;
    let mut until_ms: Option<i64> = None;
    let mut limit: Option<usize> = None;
    let mut offset: Option<usize> = None;

    let mut i = 0;
    while i < rest.len() {
        match rest[i].as_str() {
            "--action" => {
                action = Some(rest.get(i + 1).ok_or(2)?.to_owned());
                i += 2;
            }
            "--operator" => {
                operator = Some(rest.get(i + 1).ok_or(2)?.to_owned());
                i += 2;
            }
            "--since-ms" => {
                since_ms = Some(rest.get(i + 1).ok_or(2)?.parse::<i64>().map_err(|_| 2)?);
                i += 2;
            }
            "--until-ms" => {
                until_ms = Some(rest.get(i + 1).ok_or(2)?.parse::<i64>().map_err(|_| 2)?);
                i += 2;
            }
            "--limit" => {
                limit = Some(rest.get(i + 1).ok_or(2)?.parse::<usize>().map_err(|_| 2)?);
                i += 2;
            }
            "--offset" => {
                offset = Some(rest.get(i + 1).ok_or(2)?.parse::<usize>().map_err(|_| 2)?);
                i += 2;
            }
            _ => return Err(2),
        }
    }

    let mut url = reqwest::Url::parse(&opts.url("/audit-log")).map_err(|_| 1)?;
    {
        let mut pairs = url.query_pairs_mut();
        if let Some(value) = action.as_ref() {
            pairs.append_pair("action", value);
        }
        if let Some(value) = operator.as_ref() {
            pairs.append_pair("operator", value);
        }
        if let Some(value) = since_ms {
            pairs.append_pair("since_ms", &value.to_string());
        }
        if let Some(value) = until_ms {
            pairs.append_pair("until_ms", &value.to_string());
        }
        if let Some(value) = limit {
            pairs.append_pair("limit", &value.to_string());
        }
        if let Some(value) = offset {
            pairs.append_pair("offset", &value.to_string());
        }
    }

    let client = build_client()?;
    let res = opts
        .apply_headers(client.get(url))
        .send()
        .await
        .map_err(|_| 1)?;
    let status = res.status();
    let body = res.text().await.map_err(|_| 1)?;
    if status.is_success() {
        print!("{body}");
        Ok(())
    } else {
        eprint!("{body}");
        Err(1)
    }
}

async fn event_cmd(args: &[String]) -> Result<(), i32> {
    let Some(sub) = args.first().map(String::as_str) else {
        return Err(2);
    };
    let (opts, rest) = parse_api_opts(&args[1..])?;
    let client = build_client()?;

    match sub {
        "inject" => {
            let mut source: Option<String> = None;
            let mut event_type: Option<String> = None;
            let mut payload: Option<serde_json::Value> = None;
            let mut i = 0;
            while i < rest.len() {
                match rest[i].as_str() {
                    "--source" => {
                        source = Some(rest.get(i + 1).ok_or(2)?.to_owned());
                        i += 2;
                    }
                    "--type" => {
                        event_type = Some(rest.get(i + 1).ok_or(2)?.to_owned());
                        i += 2;
                    }
                    "--payload" => {
                        let raw = rest.get(i + 1).ok_or(2)?;
                        payload =
                            Some(serde_json::from_str::<serde_json::Value>(raw).map_err(|_| 2)?);
                        i += 2;
                    }
                    _ => return Err(2),
                }
            }
            let body = serde_json::json!({
                "source": source.ok_or(2)?,
                "event_type": event_type.ok_or(2)?,
                "payload": payload.ok_or(2)?,
            });
            let res = opts
                .apply_headers(client.post(opts.url("/inject-event")).json(&body))
                .send()
                .await
                .map_err(|_| 1)?;
            let status = res.status();
            let text = res.text().await.map_err(|_| 1)?;
            if status.is_success() {
                print!("{text}");
                Ok(())
            } else if status == reqwest::StatusCode::CONFLICT {
                Err(3)
            } else {
                eprint!("{text}");
                Err(1)
            }
        }
        "push" => {
            let mut source: Option<String> = None;
            let mut event_type: Option<String> = None;
            let mut payload: Option<serde_json::Value> = None;
            let mut agent_id: Option<String> = None;
            let mut priority: Option<String> = None;
            let mut delivery: Option<String> = None;
            let mut ttl_ms: Option<u64> = None;
            let mut payload_stdin: bool = false;
            let mut i = 0;
            while i < rest.len() {
                match rest[i].as_str() {
                    "--source" => {
                        source = Some(rest.get(i + 1).ok_or(2)?.to_owned());
                        i += 2;
                    }
                    "--type" => {
                        event_type = Some(rest.get(i + 1).ok_or(2)?.to_owned());
                        i += 2;
                    }
                    "--payload" => {
                        let raw = rest.get(i + 1).ok_or(2)?;
                        payload = Some(
                            serde_json::from_str::<serde_json::Value>(raw).map_err(|_| 2)?,
                        );
                        i += 2;
                    }
                    "--agent" => {
                        agent_id = Some(rest.get(i + 1).ok_or(2)?.to_owned());
                        i += 2;
                    }
                    "--priority" => {
                        priority = Some(rest.get(i + 1).ok_or(2)?.to_owned());
                        i += 2;
                    }
                    "--delivery" => {
                        delivery = Some(rest.get(i + 1).ok_or(2)?.to_owned());
                        i += 2;
                    }
                    "--ttl-ms" => {
                        ttl_ms = Some(
                            rest.get(i + 1)
                                .ok_or(2)?
                                .parse::<u64>()
                                .map_err(|_| 2)?,
                        );
                        i += 2;
                    }
                    "--payload-stdin" => {
                        payload_stdin = true;
                        i += 1;
                    }
                    _ => return Err(2),
                }
            }
            if payload_stdin {
                use std::io::Read;
                let mut buf = String::new();
                std::io::stdin()
                    .read_to_string(&mut buf)
                    .map_err(|_| 1)?;
                payload =
                    Some(serde_json::from_str::<serde_json::Value>(&buf).map_err(|_| 1)?);
            }
            let mut body = serde_json::json!({
                "source": source.ok_or(2)?,
                "event_type": event_type.ok_or(2)?,
                "payload": payload.ok_or(2)?,
            });
            if let Some(ref id) = agent_id {
                body["agent_id"] = serde_json::json!(id);
            }
            if let Some(ref p) = priority {
                body["priority"] = serde_json::json!(p);
            }
            if let Some(ref d) = delivery {
                body["delivery"] = serde_json::json!(d);
            }
            if let Some(t) = ttl_ms {
                body["ttl_ms"] = serde_json::json!(t);
            }
            let res = opts
                .apply_headers(client.post(opts.url("/events/push")).json(&body))
                .send()
                .await
                .map_err(|_| 1)?;
            let status = res.status();
            let text = res.text().await.map_err(|_| 1)?;
            if status.is_success() {
                print!("{text}");
                Ok(())
            } else {
                eprint!("{text}");
                Err(1)
            }
        }
        "types" => {
            let res = opts
                .apply_headers(client.get(opts.url("/events/types")))
                .send()
                .await
                .map_err(|_| 1)?;
            let status = res.status();
            let text = res.text().await.map_err(|_| 1)?;
            if status.is_success() {
                print!("{text}");
                Ok(())
            } else {
                eprint!("{text}");
                Err(1)
            }
        }
        "dump" => {
            let mut id: Option<String> = None;
            let mut i = 0;
            while i < rest.len() {
                match rest[i].as_str() {
                    "--id" => {
                        id = Some(rest.get(i + 1).ok_or(2)?.to_owned());
                        i += 2;
                    }
                    _ => return Err(2),
                }
            }
            let id = id.ok_or(2)?;
            let res = opts
                .apply_headers(client.get(opts.url(&format!("/events/dump/{id}"))))
                .send()
                .await
                .map_err(|_| 1)?;
            let status = res.status();
            let text = res.text().await.map_err(|_| 1)?;
            if status.is_success() {
                print!("{text}");
                Ok(())
            } else {
                eprint!("{text}");
                Err(1)
            }
        }
        "trace" => {
            let mut trace_id: Option<String> = None;
            let mut i = 0;
            while i < rest.len() {
                match rest[i].as_str() {
                    "--trace-id" => {
                        trace_id = Some(rest.get(i + 1).ok_or(2)?.to_owned());
                        i += 2;
                    }
                    _ => return Err(2),
                }
            }
            let trace_id = trace_id.ok_or(2)?;
            let res = opts
                .apply_headers(client.get(opts.url(&format!("/events/trace/{trace_id}"))))
                .send()
                .await
                .map_err(|_| 1)?;
            let status = res.status();
            let text = res.text().await.map_err(|_| 1)?;
            if status.is_success() {
                print!("{text}");
                Ok(())
            } else {
                eprint!("{text}");
                Err(1)
            }
        }
        _ => Err(2),
    }
}

async fn dlq_cmd(args: &[String]) -> Result<(), i32> {
    let Some(sub) = args.first().map(String::as_str) else {
        return Err(2);
    };
    let (opts, rest) = parse_api_opts(&args[1..])?;
    let client = build_client()?;

    match sub {
        "list" => {
            let mut url = reqwest::Url::parse(&opts.url("/dlq")).map_err(|_| 1)?;
            {
                let mut i = 0;
                let mut pairs = url.query_pairs_mut();
                while i < rest.len() {
                    match rest[i].as_str() {
                        "--reason" => {
                            pairs.append_pair("reason", rest.get(i + 1).ok_or(2)?);
                            i += 2;
                        }
                        "--source" => {
                            pairs.append_pair("source", rest.get(i + 1).ok_or(2)?);
                            i += 2;
                        }
                        "--event-type" => {
                            pairs.append_pair("event_type", rest.get(i + 1).ok_or(2)?);
                            i += 2;
                        }
                        "--limit" => {
                            pairs.append_pair("limit", rest.get(i + 1).ok_or(2)?);
                            i += 2;
                        }
                        "--offset" => {
                            pairs.append_pair("offset", rest.get(i + 1).ok_or(2)?);
                            i += 2;
                        }
                        _ => return Err(2),
                    }
                }
            }
            let res = opts
                .apply_headers(client.get(url))
                .send()
                .await
                .map_err(|_| 1)?;
            let status = res.status();
            let text = res.text().await.map_err(|_| 1)?;
            if status.is_success() {
                print!("{text}");
                Ok(())
            } else {
                eprint!("{text}");
                Err(1)
            }
        }
        "retry" => {
            let mut id: Option<String> = None;
            let mut reason: Option<String> = None;
            let mut i = 0;
            while i < rest.len() {
                match rest[i].as_str() {
                    "--id" => {
                        id = Some(rest.get(i + 1).ok_or(2)?.to_owned());
                        i += 2;
                    }
                    "--reason" => {
                        reason = Some(rest.get(i + 1).ok_or(2)?.to_owned());
                        i += 2;
                    }
                    _ => return Err(2),
                }
            }
            let id = id.ok_or(2)?;
            let body = serde_json::json!({ "reason": reason });
            let res = opts
                .apply_headers(client.post(opts.url(&format!("/dlq/{id}/retry"))).json(&body))
                .send()
                .await
                .map_err(|_| 1)?;
            if res.status().is_success() {
                Ok(())
            } else if res.status() == reqwest::StatusCode::CONFLICT {
                Err(3)
            } else {
                Err(1)
            }
        }
        "discard" => {
            let mut id: Option<String> = None;
            let mut i = 0;
            while i < rest.len() {
                match rest[i].as_str() {
                    "--id" => {
                        id = Some(rest.get(i + 1).ok_or(2)?.to_owned());
                        i += 2;
                    }
                    _ => return Err(2),
                }
            }
            let id = id.ok_or(2)?;
            let res = opts
                .apply_headers(client.post(opts.url(&format!("/dlq/{id}/discard"))))
                .send()
                .await
                .map_err(|_| 1)?;
            if res.status().is_success() {
                Ok(())
            } else {
                Err(1)
            }
        }
        _ => Err(2),
    }
}

async fn source_cmd(args: &[String]) -> Result<(), i32> {
    let Some(sub) = args.first().map(String::as_str) else {
        return Err(2);
    };
    let (opts, rest) = parse_api_opts(&args[1..])?;
    let client = build_client()?;
    match sub {
        "pause" | "resume" => {
            let mut id: Option<String> = None;
            let mut i = 0;
            while i < rest.len() {
                match rest[i].as_str() {
                    "--id" => {
                        id = Some(rest.get(i + 1).ok_or(2)?.to_owned());
                        i += 2;
                    }
                    _ => return Err(2),
                }
            }
            let id = id.ok_or(2)?;
            let path = if sub == "pause" {
                format!("/source/{id}/pause")
            } else {
                format!("/source/{id}/resume")
            };
            let res = opts
                .apply_headers(client.post(opts.url(&path)))
                .send()
                .await
                .map_err(|_| 1)?;
            if res.status().is_success() {
                Ok(())
            } else {
                Err(1)
            }
        }
        "config" => {
            let mut id: Option<String> = None;
            let mut patch: Option<serde_json::Value> = None;
            let mut i = 0;
            while i < rest.len() {
                match rest[i].as_str() {
                    "--id" => {
                        id = Some(rest.get(i + 1).ok_or(2)?.to_owned());
                        i += 2;
                    }
                    "--json" => {
                        patch = Some(
                            serde_json::from_str::<serde_json::Value>(rest.get(i + 1).ok_or(2)?)
                                .map_err(|_| 2)?,
                        );
                        i += 2;
                    }
                    _ => return Err(2),
                }
            }
            let id = id.ok_or(2)?;
            let patch = patch.ok_or(2)?;
            let res = opts
                .apply_headers(
                    client
                        .put(opts.url(&format!("/source/{id}/config")))
                        .json(&patch),
                )
                .send()
                .await
                .map_err(|_| 1)?;
            if res.status().is_success() {
                Ok(())
            } else {
                Err(1)
            }
        }
        _ => Err(2),
    }
}

async fn plugin_cmd(args: &[String]) -> Result<(), i32> {
    let Some(sub) = args.first().map(String::as_str) else {
        return Err(2);
    };
    let (opts, rest) = parse_api_opts(&args[1..])?;
    let client = build_client()?;
    match sub {
        "list" => {
            let res = opts
                .apply_headers(client.get(opts.url("/plugins")))
                .send()
                .await
                .map_err(|_| 1)?;
            let status = res.status();
            let text = res.text().await.map_err(|_| 1)?;
            if status.is_success() {
                print!("{text}");
                Ok(())
            } else {
                eprint!("{text}");
                Err(1)
            }
        }
        "enable" | "disable" | "uninstall" => {
            let mut name: Option<String> = None;
            let mut i = 0;
            while i < rest.len() {
                match rest[i].as_str() {
                    "--name" => {
                        name = Some(rest.get(i + 1).ok_or(2)?.to_owned());
                        i += 2;
                    }
                    _ => return Err(2),
                }
            }
            let name = name.ok_or(2)?;
            let path = match sub {
                "enable" => format!("/plugin/{name}/enable"),
                "disable" => format!("/plugin/{name}/disable"),
                _ => format!("/plugin/{name}/uninstall"),
            };
            let res = opts
                .apply_headers(client.post(opts.url(&path)))
                .send()
                .await
                .map_err(|_| 1)?;
            if res.status().is_success() {
                Ok(())
            } else if res.status() == reqwest::StatusCode::CONFLICT {
                Err(3)
            } else {
                Err(1)
            }
        }
        "install" => {
            let mut file: Option<PathBuf> = None;
            let mut i = 0;
            while i < rest.len() {
                match rest[i].as_str() {
                    "--file" => {
                        file = Some(PathBuf::from(rest.get(i + 1).ok_or(2)?));
                        i += 2;
                    }
                    _ => return Err(2),
                }
            }
            let file = file.ok_or(2)?;
            let bytes = std::fs::read(&file).map_err(|_| 1)?;
            let part = reqwest::multipart::Part::bytes(bytes).file_name(
                file.file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("plugin.tar.gz")
                    .to_owned(),
            );
            let form = reqwest::multipart::Form::new().part("plugin", part);
            let res = opts
                .apply_headers(client.post(opts.url("/plugin/install")).multipart(form))
                .send()
                .await
                .map_err(|_| 1)?;
            let status = res.status();
            let text = res.text().await.map_err(|_| 1)?;
            if status.is_success() {
                print!("{text}");
                Ok(())
            } else {
                eprint!("{text}");
                Err(1)
            }
        }
        _ => Err(2),
    }
}

async fn skill_cmd(args: &[String]) -> Result<(), i32> {
    let Some(sub) = args.first().map(String::as_str) else {
        return Err(2);
    };

    // Local commands (no running gateway needed)
    match sub {
        "validate" => return skill_validate_cmd(&args[1..]),
        "export" => return skill_export_cmd(&args[1..]),
        _ => {}
    }

    // Remote commands (require running gateway)
    let (opts, rest) = parse_api_opts(&args[1..])?;
    let client = build_client()?;
    match sub {
        "list" => {
            let res = opts
                .apply_headers(client.get(opts.url("/skills")))
                .send()
                .await
                .map_err(|_| 1)?;
            let status = res.status();
            let text = res.text().await.map_err(|_| 1)?;
            if status.is_success() {
                print!("{text}");
                Ok(())
            } else {
                eprint!("{text}");
                Err(1)
            }
        }
        "search" => {
            let mut q: Option<String> = None;
            let mut limit: Option<usize> = None;
            let mut i = 0;
            while i < rest.len() {
                match rest[i].as_str() {
                    "--q" => {
                        q = Some(rest.get(i + 1).ok_or(2)?.to_owned());
                        i += 2;
                    }
                    "--limit" => {
                        let raw = rest.get(i + 1).ok_or(2)?;
                        limit = Some(raw.parse::<usize>().map_err(|_| 2)?);
                        i += 2;
                    }
                    _ => return Err(2),
                }
            }
            let q = q.unwrap_or_default();
            let limit_text = limit.unwrap_or(10).to_string();
            let res = opts
                .apply_headers(
                    client
                        .get(opts.url("/skills/search"))
                        .query(&[("q", q.as_str()), ("limit", limit_text.as_str())]),
                )
                .send()
                .await
                .map_err(|_| 1)?;
            let status = res.status();
            let text = res.text().await.map_err(|_| 1)?;
            if status.is_success() {
                print!("{text}");
                Ok(())
            } else {
                eprint!("{text}");
                Err(1)
            }
        }
        "info" | "enable" | "disable" | "version" | "rollback" => {
            let mut name: Option<String> = None;
            let mut version: Option<String> = None;
            let mut i = 0;
            while i < rest.len() {
                match rest[i].as_str() {
                    "--name" => {
                        name = Some(rest.get(i + 1).ok_or(2)?.to_owned());
                        i += 2;
                    }
                    "--version" => {
                        version = Some(rest.get(i + 1).ok_or(2)?.to_owned());
                        i += 2;
                    }
                    _ => return Err(2),
                }
            }
            let name = name.ok_or(2)?;
            let path = match sub {
                "info" => format!("/skill/{name}"),
                "enable" => format!("/skill/{name}/enable"),
                "disable" => format!("/skill/{name}/disable"),
                "version" => format!("/skill/{name}/versions"),
                _ => format!("/skill/{name}/rollback"),
            };

            let req = match sub {
                "enable" | "disable" | "rollback" => client.post(opts.url(&path)),
                _ => client.get(opts.url(&path)),
            };
            let req = if sub == "rollback" {
                let v = version.ok_or(2)?;
                opts.apply_headers(req.json(&serde_json::json!({ "version": v })))
            } else {
                opts.apply_headers(req)
            };

            let res = req.send().await.map_err(|_| 1)?;
            let status = res.status();
            let text = res.text().await.map_err(|_| 1)?;
            if status.is_success() {
                if sub == "info" || sub == "version" {
                    print!("{text}");
                }
                Ok(())
            } else if status == reqwest::StatusCode::CONFLICT {
                Err(3)
            } else {
                eprint!("{text}");
                Err(1)
            }
        }
        _ => Err(2),
    }
}

/// `aman skills validate [path]` — validate SKILL.md files against the spec.
fn skill_validate_cmd(args: &[String]) -> Result<(), i32> {
    let root = if args.is_empty() {
        // Default to ~/.aman/skills/
        let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_owned());
        std::path::PathBuf::from(home).join(".aman/skills")
    } else {
        std::path::PathBuf::from(&args[0])
    };

    if !root.exists() {
        eprintln!("skill directory not found: {}", root.display());
        return Err(1);
    }

    let report = if root.is_dir() {
        if root.join("SKILL.md").exists() {
            // Single-skill directory
            skill::validate_one(&root)
        } else {
            // Skills root directory
            skill::validate_all(&root)
        }
    } else {
        // Direct path to SKILL.md or other file
        skill::validate_one(&root)
    };

    if report.findings.is_empty() {
        println!("✓ all skills passed validation");
        Ok(())
    } else {
        for f in &report.findings {
            let icon = match f.severity {
                skill::Severity::Error => "✗",
                skill::Severity::Warning => "⚠",
            };
            println!("{icon} {} {}: {} — {}", f.rule, f.skill_name.as_deref().unwrap_or("?"), f.path.display(), f.message);
        }
        let errors = report.error_count();
        let warnings = report.warning_count();
        if errors > 0 {
            eprintln!("{errors} error(s), {warnings} warning(s)");
            Err(1)
        } else {
            println!("{warnings} warning(s), 0 errors");
            Ok(())
        }
    }
}

/// `aman skill export <out_dir>` — export skills to spec-compliant directory tree.
fn skill_export_cmd(args: &[String]) -> Result<(), i32> {
    let out_dir = match args.first() {
        Some(p) => std::path::PathBuf::from(p),
        None => {
            eprintln!("usage: aman skill export <out_dir>");
            return Err(2);
        }
    };

    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_owned());
    let skills_root = std::path::PathBuf::from(home).join(".aman/skills");

    if !skills_root.exists() {
        eprintln!("skill directory not found: {}", skills_root.display());
        return Err(1);
    }

    let report = skill::export_all(&skills_root, &out_dir);

    for name in &report.exported {
        println!("✓ {name}");
    }
    for (name, msg) in &report.errors {
        eprintln!("✗ {name}: {msg}");
    }
    for (name, msg) in &report.skipped {
        println!("⚠ {name}: {msg}");
    }

    println!(
        "exported {} skill(s) to {}",
        report.exported.len(),
        out_dir.display()
    );

    if report.is_ok() {
        Ok(())
    } else {
        Err(1)
    }
}

async fn workflow_cmd(args: &[String]) -> Result<(), i32> {
    let Some(sub) = args.first().map(String::as_str) else {
        return Err(2);
    };
    let (opts, rest) = parse_api_opts(&args[1..])?;
    let client = build_client()?;
    match sub {
        "list" => {
            let res = opts
                .apply_headers(client.get(opts.url("/workflow-instances")))
                .send()
                .await
                .map_err(|_| 1)?;
            let status = res.status();
            let text = res.text().await.map_err(|_| 1)?;
            if status.is_success() {
                print!("{text}");
                Ok(())
            } else {
                eprint!("{text}");
                Err(1)
            }
        }
        "show" | "retry" | "cancel" => {
            let mut id: Option<String> = None;
            let mut i = 0;
            while i < rest.len() {
                match rest[i].as_str() {
                    "--id" => {
                        id = Some(rest.get(i + 1).ok_or(2)?.to_owned());
                        i += 2;
                    }
                    _ => return Err(2),
                }
            }
            let id = id.ok_or(2)?;
            let path = match sub {
                "show" => format!("/workflow-instance/{id}"),
                "retry" => format!("/workflow-instance/{id}/retry"),
                _ => format!("/workflow-instance/{id}/cancel"),
            };
            let req = match sub {
                "retry" | "cancel" => client.post(opts.url(&path)),
                _ => client.get(opts.url(&path)),
            };
            let res = opts
                .apply_headers(req)
                .send()
                .await
                .map_err(|_| 1)?;
            let status = res.status();
            let text = res.text().await.map_err(|_| 1)?;
            if status.is_success() {
                if sub == "show" {
                    print!("{text}");
                }
                Ok(())
            } else if status == reqwest::StatusCode::CONFLICT {
                Err(3)
            } else {
                eprint!("{text}");
                Err(1)
            }
        }
        _ => Err(2),
    }
}

async fn cron_cmd(args: &[String]) -> Result<(), i32> {
    let Some(sub) = args.first().map(String::as_str) else {
        return Err(2);
    };
    let (opts, rest) = parse_api_opts(&args[1..])?;
    let client = build_client()?;
    match sub {
        "add" => {
            let mut id: Option<String> = None;
            let mut expression: Option<String> = None;
            let mut i = 0;
            while i < rest.len() {
                match rest[i].as_str() {
                    "--id" => {
                        id = Some(rest.get(i + 1).ok_or(2)?.to_owned());
                        i += 2;
                    }
                    "--expression" => {
                        expression = Some(rest.get(i + 1).ok_or(2)?.to_owned());
                        i += 2;
                    }
                    _ => return Err(2),
                }
            }
            let body = serde_json::json!({
                "id": id.ok_or(2)?,
                "expression": expression.ok_or(2)?,
            });
            let res = opts
                .apply_headers(client.post(opts.url("/cron/add")).json(&body))
                .send()
                .await
                .map_err(|_| 1)?;
            if res.status().is_success() {
                Ok(())
            } else {
                Err(1)
            }
        }
        "update" => {
            let mut id: Option<String> = None;
            let mut patch: Option<serde_json::Value> = None;
            let mut i = 0;
            while i < rest.len() {
                match rest[i].as_str() {
                    "--id" => {
                        id = Some(rest.get(i + 1).ok_or(2)?.to_owned());
                        i += 2;
                    }
                    "--json" => {
                        patch = Some(
                            serde_json::from_str::<serde_json::Value>(rest.get(i + 1).ok_or(2)?)
                                .map_err(|_| 2)?,
                        );
                        i += 2;
                    }
                    _ => return Err(2),
                }
            }
            let id = id.ok_or(2)?;
            let patch = patch.ok_or(2)?;
            let res = opts
                .apply_headers(client.post(opts.url(&format!("/cron/{id}/update"))).json(&patch))
                .send()
                .await
                .map_err(|_| 1)?;
            if res.status().is_success() {
                Ok(())
            } else {
                Err(1)
            }
        }
        "remove" => {
            let mut id: Option<String> = None;
            let mut i = 0;
            while i < rest.len() {
                match rest[i].as_str() {
                    "--id" => {
                        id = Some(rest.get(i + 1).ok_or(2)?.to_owned());
                        i += 2;
                    }
                    _ => return Err(2),
                }
            }
            let id = id.ok_or(2)?;
            let res = opts
                .apply_headers(client.post(opts.url(&format!("/cron/{id}/remove"))))
                .send()
                .await
                .map_err(|_| 1)?;
            if res.status().is_success() {
                Ok(())
            } else {
                Err(1)
            }
        }
        _ => Err(2),
    }
}

async fn config_cmd(args: &[String]) -> Result<(), i32> {
    let Some(sub) = args.first().map(String::as_str) else {
        return Err(2);
    };
    let mut config_path: Option<PathBuf> = None;
    let mut runtime_override: Option<PathBuf> = None;
    let mut patch_json: Option<String> = None;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--config" => {
                config_path = Some(PathBuf::from(args.get(i + 1).ok_or(2)?));
                i += 2;
            }
            "--override" => {
                runtime_override = Some(PathBuf::from(args.get(i + 1).ok_or(2)?));
                i += 2;
            }
            "--json" => {
                patch_json = Some(args.get(i + 1).ok_or(2)?.to_owned());
                i += 2;
            }
            _ => return Err(2),
        }
    }

    match sub {
        "show" => {
            let loaded = ConfigLoader::load(
                config_path.as_deref(),
                runtime_override.as_deref(),
            )
            .map_err(|_| 1)?;
            let body = serde_json::to_string_pretty(&loaded.config).map_err(|_| 1)?;
            print!("{body}");
            Ok(())
        }
        "validate" => {
            let loaded = ConfigLoader::load(
                config_path.as_deref(),
                runtime_override.as_deref(),
            )
            .map_err(|_| 1)?;
            if !loaded.warnings.is_empty() {
                for w in loaded.warnings {
                    eprintln!("warning: {w}");
                }
            }
            Ok(())
        }
        "set" => {
            let Some(override_path) = runtime_override else {
                return Err(2);
            };
            let Some(raw) = patch_json else {
                return Err(2);
            };
            let patch = serde_json::from_str::<config::PartialAgentConfig>(&raw).map_err(|_| 2)?;
            let yaml = serde_yaml::to_string(&patch).map_err(|_| 1)?;
            std::fs::write(&override_path, yaml).map_err(|_| 1)?;
            let _ = ConfigLoader::load(config_path.as_deref(), Some(&override_path)).map_err(|_| 1)?;
            Ok(())
        }
        _ => Err(2),
    }
}

fn load_config(path: Option<&PathBuf>) -> Result<AgentConfig, kernel::Error> {
    let loaded = ConfigLoader::load(path.map(|p| p.as_path()), None)?;
    Ok(loaded.config)
}

fn print_usage() {
    eprintln!(
        "usage:\n  aman run [--config <path>] [--soul <path>] [--daemon] [--log-level <level>] [--bind <ip:port>] [--token <token>]\n  aman health ready [--addr <ip:port>] [--token <token>]\n  aman agent start|shutdown [--addr <ip:port>] [--token <token>] [--operator <name>] [--confirm]\n  aman metrics [--addr <ip:port>] [--token <token>]\n  aman audit-log [--addr <ip:port>] [--token <token>] [--action <a>] [--operator <o>] [--since-ms <ms>] [--until-ms <ms>] [--limit <n>] [--offset <n>]\n  aman event inject --source <s> --type <t> --payload <json> [--addr <ip:port>] [--token <token>] [--operator <name>]\n  aman event push --source <s> --type <t> --payload <json>|--payload-stdin [--agent <id>] [--priority <p>] [--delivery <d>] [--ttl-ms <ms>] [--addr <ip:port>] [--token <token>] [--operator <name>]\n  aman event types [--addr <ip:port>] [--token <token>]\n  aman event dump --id <event_id> [--addr <ip:port>] [--token <token>]\n  aman event trace --trace-id <trace_id> [--addr <ip:port>] [--token <token>]\n  aman dlq list [--reason <r>] [--source <s>] [--event-type <t>] [--limit <n>] [--offset <n>] [--addr <ip:port>] [--token <token>]\n  aman dlq retry --id <id> [--reason <r>] [--confirm] [--addr <ip:port>] [--token <token>] [--operator <name>]\n  aman dlq discard --id <id> [--addr <ip:port>] [--token <token>] [--operator <name>]\n  aman source pause|resume --id <id> [--addr <ip:port>] [--token <token>] [--operator <name>]\n  aman source config --id <id> --json <patch> [--addr <ip:port>] [--token <token>] [--operator <name>]\n  aman plugin list [--addr <ip:port>] [--token <token>] [--operator <name>]\n  aman plugin enable|disable|uninstall --name <name> [--confirm] [--addr <ip:port>] [--token <token>] [--operator <name>]\n  aman plugin install --file <path.tar.gz> [--addr <ip:port>] [--token <token>] [--operator <name>]\n  aman cron add --id <id> --expression <expr> [--addr <ip:port>] [--token <token>] [--operator <name>]\n  aman cron update --id <id> --json <patch> [--addr <ip:port>] [--token <token>] [--operator <name>]\n  aman cron remove --id <id> [--addr <ip:port>] [--token <token>] [--operator <name>]\n  aman config show|validate [--config <path>] [--override <path>]\n  aman config set --override <path> --json <partial_agent_config_json> [--config <path>]"
    );
}

#[cfg(test)]
mod tests {
    use super::parse_api_opts;

    #[test]
    fn parse_api_opts_defaults() {
        let (opts, rest) = parse_api_opts(&[]).expect("default opts");
        assert_eq!(opts.addr.to_string(), "127.0.0.1:8080");
        assert!(rest.is_empty());
    }

    #[test]
    fn parse_api_opts_overrides() {
        let (opts, rest) = parse_api_opts(&[
            "--addr".to_owned(),
            "127.0.0.1:9999".to_owned(),
            "--operator".to_owned(),
            "alice".to_owned(),
            "--confirm".to_owned(),
        ])
        .expect("override opts");
        assert_eq!(opts.addr.to_string(), "127.0.0.1:9999");
        assert_eq!(opts.operator.as_deref(), Some("alice"));
        assert!(opts.confirm);
        assert!(rest.is_empty());
    }
}
