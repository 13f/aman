#![forbid(unsafe_code)]
// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0


mod grpc_client;

use config::{ConfigLoader, AgentConfig};
use gateway::runtime::{serve, serve_stdio, AgentRuntimeBuilder, Agenverse, HttpServerConfig};
use std::sync::Arc;
use gateway::ai_signal::AmanSignalV1;
use grpc_client::GrpcClient;
use kernel::{safe_eprintln, safe_println};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

use i18n::{Locale, Translator};

const DEFAULT_BIND_ADDR: &str = "127.0.0.1:8080";

static _AI_SIGNAL: () = {
    let _ = std::any::TypeId::of::<AmanSignalV1>();
};


// ── CLI helpers (P2-15) ────────────────────────────────────────────

/// Read the value following a flag at position `i` in `args`.
/// Replaces the `arg(args, i)?` boilerplate
/// that was repeated 52 times across the 16 `*_cmd` functions.
#[inline]
fn arg(args: &[String], i: usize) -> Result<String, i32> {
    args.get(i + 1)
        .map(String::as_str)
        .map(str::to_owned)
        .ok_or(2)
}

/// Create a translator for CLI output.
/// Reads `AMAN_LOCALE` env var (e.g. `en`, `zhs`); defaults to English.
fn cli_translator() -> Translator {
    let locale = std::env::var("AMAN_LOCALE")
        .ok()
        .and_then(|s| Locale::from_code(&s))
        .unwrap_or(Locale::En);
    Translator::new(locale)
}

/// Translate a CLI message without placeholders.
fn cli_t(key: &'static str) -> String {
    cli_translator().translate(key).to_owned()
}

/// Translate a CLI message with placeholder pairs.
fn cli_t_with(key: &'static str, pairs: &[(&str, &str)]) -> String {
    let map: HashMap<&str, &str> = pairs.iter().copied().collect();
    cli_translator().translate_with(key, &map)
}

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
        "analyze" => {
            if let Err(code) = analyze_cmd(&args[1..]).await {
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
        "serve" => {
            if let Err(code) = serve_cmd(&args[1..]).await {
                std::process::exit(code);
            }
        }
        "config" => {
            if let Err(code) = config_cmd(&args[1..]).await {
                std::process::exit(code);
            }
        }
        "--version" | "-V" => {
            safe_println!("{}", cli_t_with(i18n::key::CLI_VERSION, &[("version", env!("CARGO_PKG_VERSION"))]));
        }
        "--help" | "-h" => {
            print_usage();
            std::process::exit(0);
        }
        _ => {
            print_usage();
            std::process::exit(2);
        }
    }
}

async fn run_cmd(args: &[String]) -> Result<(), i32> {
    let mut config_path: Option<PathBuf> = None;
    let mut bind: SocketAddr = DEFAULT_BIND_ADDR.parse().expect("default bind");
    let mut api_token: Option<String> = std::env::var("AMAN_API_TOKEN").ok();
    let mut soul_path: Option<PathBuf> = None;

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
    let agenverse = Arc::new(Agenverse::new(Duration::from_millis(0), Duration::from_secs(720)));
    let runtime = builder.build(Arc::clone(&agenverse)).map_err(|_| 1)?;
    agenverse.set_runtime(Arc::clone(&runtime));

    let server = serve(
        runtime.clone(),
        HttpServerConfig {
            bind: runtime.bind_addr(),
        },
    )
    .await
    .map_err(|_| 1)?;

    let addr = server.local_addr();
    safe_println!("{addr}");

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

    let cancel = CancellationToken::new();
    let _ = runtime.shutdown(&cancel).await;
    server.shutdown();
    Ok(())
}

async fn serve_cmd(args: &[String]) -> Result<(), i32> {
    let mut config_path: Option<PathBuf> = None;
    let mut soul_path: Option<PathBuf> = None;

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
            _ => return Err(2),
        }
    }

    let config = load_config(config_path.as_ref()).map_err(|_| 1)?;
    let mut builder = AgentRuntimeBuilder::new(config);
    if let Some(path) = soul_path {
        builder = builder.with_soul(path);
    }
    let agenverse = Arc::new(Agenverse::new(Duration::from_millis(0), Duration::from_secs(720)));
    let runtime = builder.build(Arc::clone(&agenverse)).map_err(|_| 1)?;
    agenverse.set_runtime(Arc::clone(&runtime));

    serve_stdio(runtime)
        .await
        .map_err(|e| {
            let e_str = e.to_string();
            safe_eprintln!("{}", cli_t_with(i18n::key::CLI_STDIO_SERVER_ERROR, &[("e", &e_str)]));
            1
        })
}

async fn health_cmd(args: &[String]) -> Result<(), i32> {
    let Some(sub) = args.first().map(String::as_str) else {
        return Err(2);
    };

    match sub {
        "ready" => {
            let (opts, _rest) = parse_api_opts(&args[1..])?;
            if opts.use_grpc {
                let mut client = connect_grpc(opts.addr).await?;
                client.health_ready().await.map_err(|e| {
                    safe_eprintln!("{}", cli_t_with(i18n::key::CLI_GRPC_ERROR, &[("e", &e.to_string())]));
                    1
                })?;
                Ok(())
            } else {
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
    /// Use gRPC transport instead of HTTP REST.
    use_grpc: bool,
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

async fn connect_grpc(addr: SocketAddr) -> Result<GrpcClient, i32> {
    GrpcClient::connect(addr).await.map_err(|e| {
        let e_str = e.to_string();
        safe_eprintln!("{}", cli_t_with(i18n::key::CLI_GRPC_CONNECT_ERROR, &[("e", &e_str)]));
        1
    })
}

fn parse_api_opts(args: &[String]) -> Result<(ApiOpts, Vec<String>), i32> {
    let mut addr: SocketAddr = DEFAULT_BIND_ADDR.parse().expect("default addr");
    let mut token: Option<String> = std::env::var("AMAN_API_TOKEN").ok();
    let mut operator: Option<String> = None;
    let mut confirm = false;
    let mut use_grpc = false;

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
            "--grpc" => {
                use_grpc = true;
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
            use_grpc,
        },
        rest,
    ))
}

async fn agent_cmd(args: &[String]) -> Result<(), i32> {
    let Some(sub) = args.first().map(String::as_str) else {
        return Err(2);
    };
    let (opts, _rest) = parse_api_opts(&args[1..])?;

    if opts.use_grpc {
        let mut client = connect_grpc(opts.addr).await?;
        match sub {
            "start" => client.agent_start().await,
            "shutdown" => client.agent_shutdown().await,
            _ => return Err(2),
        }
        .map_err(|e| {
            safe_eprintln!("{}", cli_t_with(i18n::key::CLI_GRPC_ERROR, &[("e", &e.to_string())]));
            1
        })?;
        return Ok(());
    }

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

// ---------------------------------------------------------------------------
// analyze
// ---------------------------------------------------------------------------

async fn analyze_cmd(args: &[String]) -> Result<(), i32> {
    let sub = args.first().map(String::as_str).unwrap_or("");
    match sub {
        "trends" | "anomalies" => analyze_run(sub, &args[1..]).await,
        _ => {
            safe_eprintln!("{}", cli_t(i18n::key::CLI_USAGE_ANALYZE));
            Err(2)
        }
    }
}

async fn analyze_run(sub: &str, args: &[String]) -> Result<(), i32> {
    let (opts, rest) = parse_api_opts(args)?;

    let mut from_str = String::from("today");
    let mut to_str = String::from("now");
    let mut agent: Option<String> = None;
    let mut i = 0;
    while i < rest.len() {
        match rest[i].as_str() {
            "--from" => {
                from_str = rest.get(i + 1).ok_or(2)?.clone();
                i += 2;
            }
            "--to" => {
                to_str = rest.get(i + 1).ok_or(2)?.clone();
                i += 2;
            }
            "--agent" => {
                agent = Some(rest.get(i + 1).ok_or(2)?.clone());
                i += 2;
            }
            _ => {
                safe_eprintln!("{}", cli_t_with(i18n::key::CLI_UNKNOWN_FLAG, &[("flag", &rest[i])]));
                return Err(2);
            }
        }
    }

    // Build the request body
    // Parse time shortcuts
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);

    let start_ms = parse_time_shortcut(&from_str, now_ms)?;
    let end_ms = parse_time_shortcut(&to_str, now_ms)?;

    let analyses = match sub {
        "trends" => vec!["trends"],
        "anomalies" => vec!["anomalies"],
        _ => vec!["trends", "anomalies"],
    };

    let mut body = serde_json::json!({
        "time_range": {
            "start_ms": start_ms,
            "end_ms": end_ms
        },
        "analyses": analyses,
    });
    if let Some(ref a) = agent {
        body["agent_filter"] = serde_json::json!([a]);
    }

    if opts.use_grpc {
        // gRPC: send via REST for now (analytics doesn't have gRPC endpoint yet)
        let client = build_client()?;
        let res = opts
            .apply_headers(client.post(opts.url("/analytics/analyze")).json(&body))
            .send()
            .await
            .map_err(|_| 1)?;
        let status = res.status();
        let text = res.text().await.map_err(|_| 1)?;
        if status.is_success() {
            safe_println!("{text}");
            Ok(())
        } else {
            safe_eprintln!("{text}");
            Err(1)
        }
    } else {
        let client = build_client()?;
        let res = opts
            .apply_headers(client.post(opts.url("/analytics/analyze")).json(&body))
            .send()
            .await
            .map_err(|_| 1)?;
        let status = res.status();
        let text = res.text().await.map_err(|_| 1)?;
        if status.is_success() {
            safe_println!("{text}");
            Ok(())
        } else {
            safe_eprintln!("{text}");
            Err(1)
        }
    }
}

fn parse_time_shortcut(input: &str, now_ms: i64) -> Result<i64, i32> {
    match input {
        "now" => Ok(now_ms),
        "today" => {
            // Start of current UTC day
            let day_ms = 86_400_000i64;
            Ok((now_ms / day_ms) * day_ms)
        }
        "yesterday" => {
            let day_ms = 86_400_000i64;
            Ok((now_ms / day_ms - 1) * day_ms)
        }
        other => {
            // Try ISO 8601: "2026-06-21T00:00:00" or "2026-06-21"
            let s = other.trim();
            // Very simple ISO parser: YYYY-MM-DD or YYYY-MM-DDTHH:MM:SS
            if s.len() >= 10 {
                let year: i64 = s[0..4].parse().map_err(|_| 2)?;
                let month: i64 = s[5..7].parse().map_err(|_| 2)?;
                let day: i64 = s[8..10].parse().map_err(|_| 2)?;
                let (hour, min, sec) = if s.len() >= 19 && s.as_bytes().get(10) == Some(&b'T') {
                    (
                        s[11..13].parse::<i64>().map_err(|_| 2)?,
                        s[14..16].parse::<i64>().map_err(|_| 2)?,
                        s[17..19].parse::<i64>().map_err(|_| 2)?,
                    )
                } else {
                    (0i64, 0i64, 0i64)
                };
                // Approximate days since epoch (civil calendar)
                let days = days_from_civil(year, month as u32, day as u32);
                let ms = (days * 86_400 + hour * 3_600 + min * 60 + sec) * 1000;
                Ok(ms)
            } else {
                safe_eprintln!("{}", cli_t_with(i18n::key::CLI_INVALID_TIME, &[("other", other)]));
                Err(2)
            }
        }
    }
}

/// Days from UNIX epoch to (y, m, d). Approximation using civil calendar.
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y / 400 } else { (y - 399) / 400 };
    let yoe = (y - era * 400) as u32;
    let doy = if m <= 2 {
        153 * (m + 9) / 5
    } else {
        153 * (m - 3) / 5
    } + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    (era * 146_097 + doe as i64) - 719_468
}

async fn metrics_cmd(args: &[String]) -> Result<(), i32> {
    let (opts, rest) = parse_api_opts(args)?;

    // --format: only "json" is supported
    let mut i = 0;
    while i < rest.len() {
        match rest[i].as_str() {
            "--format" => {
                let raw = rest.get(i + 1).ok_or(2)?;
                if raw != "json" {
                    safe_eprintln!("{}", cli_t_with(i18n::key::CLI_UNSUPPORTED_FORMAT, &[("raw", raw)]));
                    return Err(2);
                }
                i += 2;
            }
            _ => return Err(2),
        }
    }

    if opts.use_grpc {
        let mut client = connect_grpc(opts.addr).await?;
        let json = client.get_metrics_json().await.map_err(|e| {
            safe_eprintln!("{}", cli_t_with(i18n::key::CLI_GRPC_ERROR, &[("e", &e.to_string())]));
            1
        })?;
        safe_println!("{json}");
        return Ok(());
    }

    let client = build_client()?;
    let res = opts
        .apply_headers(client.get(opts.url("/metrics")))
        .send()
        .await
        .map_err(|_| 1)?;
    let status = res.status();
    let body = res.text().await.map_err(|_| 1)?;
    if status.is_success() {
        safe_println!("{body}");
        Ok(())
    } else {
        safe_eprintln!("{body}");
        Err(1)
    }
}

async fn audit_log_cmd(args: &[String]) -> Result<(), i32> {
    let (opts, rest) = parse_api_opts(args)?;
    let mut action: Option<String> = None;
    let mut operator: Option<String> = None;
    let mut since_ms: Option<i64> = None;
    let mut until_ms: Option<i64> = None;
    let mut limit: Option<u32> = None;
    let mut offset: Option<u32> = None;

    let mut i = 0;
    while i < rest.len() {
        match rest[i].as_str() {
            "--action" => {
                action = Some(arg(&rest, i)?);
                i += 2;
            }
            "--operator" => {
                operator = Some(arg(&rest, i)?);
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
                limit = Some(rest.get(i + 1).ok_or(2)?.parse::<u32>().map_err(|_| 2)?);
                i += 2;
            }
            "--offset" => {
                offset = Some(rest.get(i + 1).ok_or(2)?.parse::<u32>().map_err(|_| 2)?);
                i += 2;
            }
            _ => return Err(2),
        }
    }

    if opts.use_grpc {
        let mut client = connect_grpc(opts.addr).await?;
        let json = client
            .audit_log_json(action, operator, since_ms, until_ms, limit, offset)
            .await
            .map_err(|e| {
                safe_eprintln!("{}", cli_t_with(i18n::key::CLI_GRPC_ERROR, &[("e", &e.to_string())]));
                1
            })?;
        safe_println!("{json}");
        return Ok(());
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
        safe_println!("{body}");
        Ok(())
    } else {
        safe_eprintln!("{body}");
        Err(1)
    }
}

async fn event_cmd(args: &[String]) -> Result<(), i32> {
    let Some(sub) = args.first().map(String::as_str) else {
        return Err(2);
    };
    let (opts, rest) = parse_api_opts(&args[1..])?;

    if opts.use_grpc {
        return event_cmd_grpc(sub, opts, rest).await;
    }

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
                        source = Some(arg(&rest, i)?);
                        i += 2;
                    }
                    "--type" => {
                        event_type = Some(arg(&rest, i)?);
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
                safe_println!("{text}");
                Ok(())
            } else if status == reqwest::StatusCode::CONFLICT {
                Err(3)
            } else {
                safe_eprintln!("{text}");
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
                        source = Some(arg(&rest, i)?);
                        i += 2;
                    }
                    "--type" => {
                        event_type = Some(arg(&rest, i)?);
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
                        agent_id = Some(arg(&rest, i)?);
                        i += 2;
                    }
                    "--priority" => {
                        priority = Some(arg(&rest, i)?);
                        i += 2;
                    }
                    "--delivery" => {
                        delivery = Some(arg(&rest, i)?);
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
                safe_println!("{text}");
                Ok(())
            } else {
                safe_eprintln!("{text}");
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
                safe_println!("{text}");
                Ok(())
            } else {
                safe_eprintln!("{text}");
                Err(1)
            }
        }
        "dump" => {
            let mut id: Option<String> = None;
            let mut i = 0;
            while i < rest.len() {
                match rest[i].as_str() {
                    "--id" => {
                        id = Some(arg(&rest, i)?);
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
                safe_println!("{text}");
                Ok(())
            } else {
                safe_eprintln!("{text}");
                Err(1)
            }
        }
        "trace" => {
            let mut trace_id: Option<String> = None;
            let mut i = 0;
            while i < rest.len() {
                match rest[i].as_str() {
                    "--trace-id" => {
                        trace_id = Some(arg(&rest, i)?);
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
                safe_println!("{text}");
                Ok(())
            } else {
                safe_eprintln!("{text}");
                Err(1)
            }
        }
        _ => Err(2),
    }
}

async fn event_cmd_grpc(sub: &str, opts: ApiOpts, rest: Vec<String>) -> Result<(), i32> {
    let mut client = connect_grpc(opts.addr).await?;
    match sub {
        "inject" => {
            let mut source: Option<String> = None;
            let mut event_type: Option<String> = None;
            let mut payload: Option<serde_json::Value> = None;
            let mut i = 0;
            while i < rest.len() {
                match rest[i].as_str() {
                    "--source" => {
                        source = Some(arg(&rest, i)?);
                        i += 2;
                    }
                    "--type" => {
                        event_type = Some(arg(&rest, i)?);
                        i += 2;
                    }
                    "--payload" => {
                        let raw = rest.get(i + 1).ok_or(2)?;
                        payload = Some(serde_json::from_str::<serde_json::Value>(raw).map_err(|_| 2)?);
                        i += 2;
                    }
                    _ => return Err(2),
                }
            }
            let payload_bytes = serde_json::to_vec(&payload.ok_or(2)?).map_err(|_| 1)?;
            let json = client
                .inject_event_json(source.ok_or(2)?, event_type.ok_or(2)?, payload_bytes)
                .await
                .map_err(|e| { safe_eprintln!("{}", cli_t_with(i18n::key::CLI_GRPC_ERROR, &[("e", &e.to_string())])); 1 })?;
            safe_println!("{json}");
            Ok(())
        }
        "push" => {
            let mut source: Option<String> = None;
            let mut event_type: Option<String> = None;
            let mut payload: Option<serde_json::Value> = None;
            let mut agent_id: Option<String> = None;
            let mut payload_stdin: bool = false;
            let mut i = 0;
            while i < rest.len() {
                match rest[i].as_str() {
                    "--source" => {
                        source = Some(arg(&rest, i)?);
                        i += 2;
                    }
                    "--type" => {
                        event_type = Some(arg(&rest, i)?);
                        i += 2;
                    }
                    "--payload" => {
                        let raw = rest.get(i + 1).ok_or(2)?;
                        payload = Some(serde_json::from_str::<serde_json::Value>(raw).map_err(|_| 2)?);
                        i += 2;
                    }
                    "--agent" => {
                        agent_id = Some(arg(&rest, i)?);
                        i += 2;
                    }
                    "--priority" | "--delivery" | "--ttl-ms" => {
                        // These are not yet in the gRPC proto; skip
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
                std::io::stdin().read_to_string(&mut buf).map_err(|_| 1)?;
                payload = Some(serde_json::from_str::<serde_json::Value>(&buf).map_err(|_| 1)?);
            }
            let payload_bytes = serde_json::to_vec(&payload.ok_or(2)?).map_err(|_| 1)?;
            let json = client
                .push_event_json(source.ok_or(2)?, event_type.ok_or(2)?, payload_bytes, agent_id)
                .await
                .map_err(|e| { safe_eprintln!("{}", cli_t_with(i18n::key::CLI_GRPC_ERROR, &[("e", &e.to_string())])); 1 })?;
            safe_println!("{json}");
            Ok(())
        }
        "types" => {
            let types = client.event_types().await.map_err(|e| { safe_eprintln!("{}", cli_t_with(i18n::key::CLI_GRPC_ERROR, &[("e", &e.to_string())])); 1 })?;
            let json = serde_json::to_string(&types).unwrap_or_default();
            safe_println!("{json}");
            Ok(())
        }
        "dump" => {
            let mut id: Option<String> = None;
            let mut i = 0;
            while i < rest.len() {
                match rest[i].as_str() {
                    "--id" => {
                        id = Some(arg(&rest, i)?);
                        i += 2;
                    }
                    _ => return Err(2),
                }
            }
            let json = client
                .dump_event_json(id.ok_or(2)?)
                .await
                .map_err(|e| { safe_eprintln!("{}", cli_t_with(i18n::key::CLI_GRPC_ERROR, &[("e", &e.to_string())])); 1 })?;
            safe_println!("{json}");
            Ok(())
        }
        "trace" => {
            let mut trace_id: Option<String> = None;
            let mut i = 0;
            while i < rest.len() {
                match rest[i].as_str() {
                    "--trace-id" => {
                        trace_id = Some(arg(&rest, i)?);
                        i += 2;
                    }
                    _ => return Err(2),
                }
            }
            let json = client
                .event_trace_json(trace_id.ok_or(2)?)
                .await
                .map_err(|e| { safe_eprintln!("{}", cli_t_with(i18n::key::CLI_GRPC_ERROR, &[("e", &e.to_string())])); 1 })?;
            safe_println!("{json}");
            Ok(())
        }
        _ => Err(2),
    }
}

async fn dlq_cmd(args: &[String]) -> Result<(), i32> {
    let Some(sub) = args.first().map(String::as_str) else {
        return Err(2);
    };
    let (opts, rest) = parse_api_opts(&args[1..])?;

    if opts.use_grpc {
        return dlq_cmd_grpc(sub, opts, rest).await;
    }

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
                safe_println!("{text}");
                Ok(())
            } else {
                safe_eprintln!("{text}");
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
                        id = Some(arg(&rest, i)?);
                        i += 2;
                    }
                    "--reason" => {
                        reason = Some(arg(&rest, i)?);
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
                        id = Some(arg(&rest, i)?);
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

async fn dlq_cmd_grpc(sub: &str, opts: ApiOpts, rest: Vec<String>) -> Result<(), i32> {
    let mut client = connect_grpc(opts.addr).await?;
    match sub {
        "list" => {
            let mut reason: Option<String> = None;
            let mut source: Option<String> = None;
            let mut event_type: Option<String> = None;
            let mut limit: Option<u32> = None;
            let mut offset: Option<u32> = None;
            let mut i = 0;
            while i < rest.len() {
                match rest[i].as_str() {
                    "--reason" => {
                        reason = Some(arg(&rest, i)?);
                        i += 2;
                    }
                    "--source" => {
                        source = Some(arg(&rest, i)?);
                        i += 2;
                    }
                    "--event-type" => {
                        event_type = Some(arg(&rest, i)?);
                        i += 2;
                    }
                    "--limit" => {
                        limit = Some(rest.get(i + 1).ok_or(2)?.parse::<u32>().map_err(|_| 2)?);
                        i += 2;
                    }
                    "--offset" => {
                        offset = Some(rest.get(i + 1).ok_or(2)?.parse::<u32>().map_err(|_| 2)?);
                        i += 2;
                    }
                    _ => return Err(2),
                }
            }
            let json = client
                .dlq_list_json(reason, source, event_type, limit, offset)
                .await
                .map_err(|e| { safe_eprintln!("{}", cli_t_with(i18n::key::CLI_GRPC_ERROR, &[("e", &e.to_string())])); 1 })?;
            safe_println!("{json}");
            Ok(())
        }
        "retry" => {
            let mut id: Option<String> = None;
            let mut reason: Option<String> = None;
            let mut i = 0;
            while i < rest.len() {
                match rest[i].as_str() {
                    "--id" => {
                        id = Some(arg(&rest, i)?);
                        i += 2;
                    }
                    "--reason" => {
                        reason = Some(arg(&rest, i)?);
                        i += 2;
                    }
                    _ => return Err(2),
                }
            }
            client
                .dlq_retry(id.ok_or(2)?, reason)
                .await
                .map_err(|e| { safe_eprintln!("{}", cli_t_with(i18n::key::CLI_GRPC_ERROR, &[("e", &e.to_string())])); 1 })
        }
        "discard" => {
            let mut id: Option<String> = None;
            let mut i = 0;
            while i < rest.len() {
                match rest[i].as_str() {
                    "--id" => {
                        id = Some(arg(&rest, i)?);
                        i += 2;
                    }
                    _ => return Err(2),
                }
            }
            client
                .dlq_discard(id.ok_or(2)?)
                .await
                .map_err(|e| { safe_eprintln!("{}", cli_t_with(i18n::key::CLI_GRPC_ERROR, &[("e", &e.to_string())])); 1 })
        }
        _ => Err(2),
    }
}

async fn source_cmd(args: &[String]) -> Result<(), i32> {
    let Some(sub) = args.first().map(String::as_str) else {
        return Err(2);
    };
    let (opts, rest) = parse_api_opts(&args[1..])?;

    if opts.use_grpc {
        return source_cmd_grpc(sub, opts, rest).await;
    }

    let client = build_client()?;
    match sub {
        "pause" | "resume" => {
            let mut id: Option<String> = None;
            let mut i = 0;
            while i < rest.len() {
                match rest[i].as_str() {
                    "--id" => {
                        id = Some(arg(&rest, i)?);
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
                        id = Some(arg(&rest, i)?);
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

async fn source_cmd_grpc(sub: &str, opts: ApiOpts, rest: Vec<String>) -> Result<(), i32> {
    let mut client = connect_grpc(opts.addr).await?;
    match sub {
        "pause" => {
            let mut id: Option<String> = None;
            let mut i = 0;
            while i < rest.len() {
                match rest[i].as_str() {
                    "--id" => {
                        id = Some(arg(&rest, i)?);
                        i += 2;
                    }
                    _ => return Err(2),
                }
            }
            client
                .pause_source(id.ok_or(2)?)
                .await
                .map_err(|e| { safe_eprintln!("{}", cli_t_with(i18n::key::CLI_GRPC_ERROR, &[("e", &e.to_string())])); 1 })
        }
        "resume" => {
            let mut id: Option<String> = None;
            let mut i = 0;
            while i < rest.len() {
                match rest[i].as_str() {
                    "--id" => {
                        id = Some(arg(&rest, i)?);
                        i += 2;
                    }
                    _ => return Err(2),
                }
            }
            client
                .resume_source(id.ok_or(2)?)
                .await
                .map_err(|e| { safe_eprintln!("{}", cli_t_with(i18n::key::CLI_GRPC_ERROR, &[("e", &e.to_string())])); 1 })
        }
        "config" => {
            let mut id: Option<String> = None;
            let mut patch: Option<serde_json::Value> = None;
            let mut i = 0;
            while i < rest.len() {
                match rest[i].as_str() {
                    "--id" => {
                        id = Some(arg(&rest, i)?);
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
            let config_bytes = serde_json::to_vec(&patch.ok_or(2)?).map_err(|_| 1)?;
            client
                .source_config(id.ok_or(2)?, config_bytes)
                .await
                .map_err(|e| { safe_eprintln!("{}", cli_t_with(i18n::key::CLI_GRPC_ERROR, &[("e", &e.to_string())])); 1 })
        }
        _ => Err(2),
    }
}

async fn plugin_cmd(args: &[String]) -> Result<(), i32> {
    let Some(sub) = args.first().map(String::as_str) else {
        return Err(2);
    };
    let (opts, rest) = parse_api_opts(&args[1..])?;

    if opts.use_grpc {
        return plugin_cmd_grpc(sub, opts, rest).await;
    }

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
                safe_println!("{text}");
                Ok(())
            } else {
                safe_eprintln!("{text}");
                Err(1)
            }
        }
        "enable" | "disable" | "uninstall" => {
            let mut name: Option<String> = None;
            let mut i = 0;
            while i < rest.len() {
                match rest[i].as_str() {
                    "--name" => {
                        name = Some(arg(&rest, i)?);
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
        "approve" | "deny" => {
            let mut name: Option<String> = None;
            let mut i = 0;
            while i < rest.len() {
                match rest[i].as_str() {
                    "--name" => {
                        name = Some(arg(&rest, i)?);
                        i += 2;
                    }
                    _ => return Err(2),
                }
            }
            let name = name.ok_or(2)?;
            let approved = sub == "approve";
            let body = serde_json::json!({
                "plugin_name": name,
                "approved": approved,
            });
            let res = opts
                .apply_headers(client.post(opts.url("/plugin-auth/respond")).json(&body))
                .send()
                .await
                .map_err(|_| 1)?;
            let status = res.status();
            let text = res.text().await.map_err(|_| 1)?;
            if status.is_success() {
                let key = if approved { i18n::key::CLI_PLUGIN_APPROVED } else { i18n::key::CLI_PLUGIN_DENIED };
                safe_println!("{}", cli_t_with(key, &[("name", &name)]));
                Ok(())
            } else {
                safe_eprintln!("{text}");
                Err(1)
            }
        }
        "pending" => {
            let res = opts
                .apply_headers(client.get(opts.url("/plugin-auth/pending")))
                .send()
                .await
                .map_err(|_| 1)?;
            let status = res.status();
            let text = res.text().await.map_err(|_| 1)?;
            if status.is_success() {
                if text == "[]" || text == "null" {
                    safe_println!("{}", cli_t(i18n::key::CLI_NO_PENDING_APPROVALS));
                } else {
                    safe_println!("{text}");
                }
                Ok(())
            } else {
                safe_eprintln!("{text}");
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
                safe_println!("{text}");
                Ok(())
            } else {
                safe_eprintln!("{text}");
                Err(1)
            }
        }
        _ => Err(2),
    }
}

async fn plugin_cmd_grpc(sub: &str, opts: ApiOpts, rest: Vec<String>) -> Result<(), i32> {
    let mut client = connect_grpc(opts.addr).await?;
    match sub {
        "list" => {
            let json = client
                .list_plugins_json()
                .await
                .map_err(|e| { safe_eprintln!("{}", cli_t_with(i18n::key::CLI_GRPC_ERROR, &[("e", &e.to_string())])); 1 })?;
            safe_println!("{json}");
            Ok(())
        }
        "enable" | "disable" | "uninstall" => {
            let mut name: Option<String> = None;
            let mut i = 0;
            while i < rest.len() {
                match rest[i].as_str() {
                    "--name" => {
                        name = Some(arg(&rest, i)?);
                        i += 2;
                    }
                    _ => return Err(2),
                }
            }
            let name = name.ok_or(2)?;
            match sub {
                "enable" => client.enable_plugin(name).await,
                "disable" => client.disable_plugin(name).await,
                _ => client.uninstall_plugin(name).await,
            }
            .map_err(|e| { safe_eprintln!("{}", cli_t_with(i18n::key::CLI_GRPC_ERROR, &[("e", &e.to_string())])); 1 })
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
            let data = std::fs::read(file.ok_or(2)?).map_err(|_| 1)?;
            let json = client
                .install_plugin_json(data)
                .await
                .map_err(|e| { safe_eprintln!("{}", cli_t_with(i18n::key::CLI_GRPC_ERROR, &[("e", &e.to_string())])); 1 })?;
            safe_println!("{json}");
            Ok(())
        }
        "approve" | "deny" | "pending" => {
            safe_eprintln!("{}", cli_t(i18n::key::CLI_PLUGIN_GRPC_UNAVAILABLE));
            Err(2)
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

    if opts.use_grpc {
        return skill_cmd_grpc(sub, opts, rest).await;
    }

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
                safe_println!("{text}");
                Ok(())
            } else {
                safe_eprintln!("{text}");
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
                        q = Some(arg(&rest, i)?);
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
                safe_println!("{text}");
                Ok(())
            } else {
                safe_eprintln!("{text}");
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
                        name = Some(arg(&rest, i)?);
                        i += 2;
                    }
                    "--version" => {
                        version = Some(arg(&rest, i)?);
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
                    safe_println!("{text}");
                }
                Ok(())
            } else if status == reqwest::StatusCode::CONFLICT {
                Err(3)
            } else {
                safe_eprintln!("{text}");
                Err(1)
            }
        }
        _ => Err(2),
    }
}

async fn skill_cmd_grpc(sub: &str, opts: ApiOpts, rest: Vec<String>) -> Result<(), i32> {
    let mut client = connect_grpc(opts.addr).await?;
    match sub {
        "list" => {
            let json = client
                .list_skills_json()
                .await
                .map_err(|e| { safe_eprintln!("{}", cli_t_with(i18n::key::CLI_GRPC_ERROR, &[("e", &e.to_string())])); 1 })?;
            safe_println!("{json}");
            Ok(())
        }
        "search" => {
            let mut q: Option<String> = None;
            let mut limit: Option<u32> = None;
            let mut i = 0;
            while i < rest.len() {
                match rest[i].as_str() {
                    "--q" => {
                        q = Some(arg(&rest, i)?);
                        i += 2;
                    }
                    "--limit" => {
                        let raw = rest.get(i + 1).ok_or(2)?;
                        limit = Some(raw.parse::<u32>().map_err(|_| 2)?);
                        i += 2;
                    }
                    _ => return Err(2),
                }
            }
            let json = client
                .search_skills_json(q.unwrap_or_default(), limit.unwrap_or(10))
                .await
                .map_err(|e| { safe_eprintln!("{}", cli_t_with(i18n::key::CLI_GRPC_ERROR, &[("e", &e.to_string())])); 1 })?;
            safe_println!("{json}");
            Ok(())
        }
        "info" | "enable" | "disable" | "version" | "rollback" => {
            let mut name: Option<String> = None;
            let mut version: Option<String> = None;
            let mut i = 0;
            while i < rest.len() {
                match rest[i].as_str() {
                    "--name" => {
                        name = Some(arg(&rest, i)?);
                        i += 2;
                    }
                    "--version" => {
                        version = Some(arg(&rest, i)?);
                        i += 2;
                    }
                    _ => return Err(2),
                }
            }
            let name = name.ok_or(2)?;
            match sub {
                "info" => {
                    let json = client
                        .get_skill_json(name)
                        .await
                        .map_err(|e| { safe_eprintln!("{}", cli_t_with(i18n::key::CLI_GRPC_ERROR, &[("e", &e.to_string())])); 1 })?;
                    safe_println!("{json}");
                    Ok(())
                }
                "enable" => client.enable_skill(name).await.map_err(|e| { safe_eprintln!("{}", cli_t_with(i18n::key::CLI_GRPC_ERROR, &[("e", &e.to_string())])); 1 }),
                "disable" => client.disable_skill(name).await.map_err(|e| { safe_eprintln!("{}", cli_t_with(i18n::key::CLI_GRPC_ERROR, &[("e", &e.to_string())])); 1 }),
                "rollback" => client.rollback_skill(name, version.ok_or(2)?).await.map_err(|e| { safe_eprintln!("{}", cli_t_with(i18n::key::CLI_GRPC_ERROR, &[("e", &e.to_string())])); 1 }),
                "version" => {
                    // gRPC proto doesn't have a list_skill_versions endpoint yet
                    safe_eprintln!("{}", cli_t(i18n::key::CLI_SKILL_VERSION_GRPC));
                    Err(1)
                }
                _ => unreachable!(),
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
        safe_eprintln!("{}", cli_t_with(i18n::key::CLI_SKILL_DIR_NOT_FOUND, &[("path", &root.display().to_string())]));
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
        safe_println!("{}", cli_t(i18n::key::CLI_SKILL_ALL_PASSED));
        Ok(())
    } else {
        for f in &report.findings {
            let icon = match f.severity {
                skill::Severity::Error => "✗",
                skill::Severity::Warning => "⚠",
            };
            safe_println!("{icon} {} {}: {} — {}", f.rule, f.skill_name.as_deref().unwrap_or("?"), f.path.display(), f.message);
        }
        let errors = report.error_count();
        let warnings = report.warning_count();
        if errors > 0 {
            safe_eprintln!("{}", cli_t_with(i18n::key::CLI_SKILL_ERROR_SUMMARY, &[("errors", &errors.to_string()), ("warnings", &warnings.to_string())]));
            Err(1)
        } else {
            safe_println!("{}", cli_t_with(i18n::key::CLI_SKILL_WARNINGS_ONLY, &[("warnings", &warnings.to_string())]));
            Ok(())
        }
    }
}

/// `aman skill export <out_dir>` — export skills to spec-compliant directory tree.
fn skill_export_cmd(args: &[String]) -> Result<(), i32> {
    let out_dir = match args.first() {
        Some(p) => std::path::PathBuf::from(p),
        None => {
            safe_eprintln!("{}", cli_t(i18n::key::CLI_SKILL_EXPORT_USAGE));
            return Err(2);
        }
    };

    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_owned());
    let skills_root = std::path::PathBuf::from(home).join(".aman/skills");

    if !skills_root.exists() {
        safe_eprintln!("{}", cli_t_with(i18n::key::CLI_SKILL_DIR_NOT_FOUND, &[("path", &skills_root.display().to_string())]));
        return Err(1);
    }

    let report = skill::export_all(&skills_root, &out_dir);

    for name in &report.exported {
        safe_println!("✓ {name}");
    }
    for (name, msg) in &report.errors {
        safe_eprintln!("✗ {name}: {msg}");
    }
    for (name, msg) in &report.skipped {
        safe_println!("⚠ {name}: {msg}");
    }

    safe_println!(
        "{}",
        cli_t_with(i18n::key::CLI_EXPORT_RESULT, &[
            ("count", &report.exported.len().to_string()),
            ("dir", &out_dir.display().to_string())
        ])
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

    if opts.use_grpc {
        return workflow_cmd_grpc(sub, opts, rest).await;
    }

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
                safe_println!("{text}");
                Ok(())
            } else {
                safe_eprintln!("{text}");
                Err(1)
            }
        }
        "show" | "retry" | "cancel" => {
            let mut id: Option<String> = None;
            let mut i = 0;
            while i < rest.len() {
                match rest[i].as_str() {
                    "--id" => {
                        id = Some(arg(&rest, i)?);
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
                    safe_println!("{text}");
                }
                Ok(())
            } else if status == reqwest::StatusCode::CONFLICT {
                Err(3)
            } else {
                safe_eprintln!("{text}");
                Err(1)
            }
        }
        _ => Err(2),
    }
}

async fn workflow_cmd_grpc(sub: &str, opts: ApiOpts, rest: Vec<String>) -> Result<(), i32> {
    let mut client = connect_grpc(opts.addr).await?;
    match sub {
        "list" => {
            let json = client
                .list_workflow_instances_json()
                .await
                .map_err(|e| { safe_eprintln!("{}", cli_t_with(i18n::key::CLI_GRPC_ERROR, &[("e", &e.to_string())])); 1 })?;
            safe_println!("{json}");
            Ok(())
        }
        "show" | "retry" | "cancel" => {
            let mut id: Option<String> = None;
            let mut i = 0;
            while i < rest.len() {
                match rest[i].as_str() {
                    "--id" => {
                        id = Some(arg(&rest, i)?);
                        i += 2;
                    }
                    _ => return Err(2),
                }
            }
            let id = id.ok_or(2)?;
            match sub {
                "show" => {
                    let json = client
                        .get_workflow_instance_json(id)
                        .await
                        .map_err(|e| { safe_eprintln!("{}", cli_t_with(i18n::key::CLI_GRPC_ERROR, &[("e", &e.to_string())])); 1 })?;
                    safe_println!("{json}");
                    Ok(())
                }
                "retry" => client.retry_workflow(id).await.map_err(|e| { safe_eprintln!("{}", cli_t_with(i18n::key::CLI_GRPC_ERROR, &[("e", &e.to_string())])); 1 }),
                _ => client.cancel_workflow(id).await.map_err(|e| { safe_eprintln!("{}", cli_t_with(i18n::key::CLI_GRPC_ERROR, &[("e", &e.to_string())])); 1 }),
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

    if opts.use_grpc {
        return cron_cmd_grpc(sub, opts, rest).await;
    }

    let client = build_client()?;
    match sub {
        "add" => {
            let mut id: Option<String> = None;
            let mut expression: Option<String> = None;
            let mut agent_key: Option<String> = None;
            let mut i = 0;
            while i < rest.len() {
                match rest[i].as_str() {
                    "--id" => {
                        id = Some(arg(&rest, i)?);
                        i += 2;
                    }
                    "--expression" => {
                        expression = Some(arg(&rest, i)?);
                        i += 2;
                    }
                    "--agent-key" => {
                        agent_key = Some(arg(&rest, i)?);
                        i += 2;
                    }
                    _ => return Err(2),
                }
            }
            let body = serde_json::json!({
                "id": id.ok_or(2)?,
                "expression": expression.ok_or(2)?,
                "agent_key": agent_key.unwrap_or_default(),
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
            let mut agent_key: Option<String> = None;
            let mut i = 0;
            while i < rest.len() {
                match rest[i].as_str() {
                    "--id" => {
                        id = Some(arg(&rest, i)?);
                        i += 2;
                    }
                    "--json" => {
                        patch = Some(
                            serde_json::from_str::<serde_json::Value>(rest.get(i + 1).ok_or(2)?)
                                .map_err(|_| 2)?,
                        );
                        i += 2;
                    }
                    "--agent-key" => {
                        agent_key = Some(arg(&rest, i)?);
                        i += 2;
                    }
                    _ => return Err(2),
                }
            }
            let id = id.ok_or(2)?;
            let mut patch = patch.ok_or(2)?;
            if let Some(obj) = patch.as_object_mut()
                && let Some(ak) = agent_key
            {
                obj.insert("agent_key".to_owned(), serde_json::Value::String(ak));
            }
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
            let mut agent_key: Option<String> = None;
            let mut i = 0;
            while i < rest.len() {
                match rest[i].as_str() {
                    "--id" => {
                        id = Some(arg(&rest, i)?);
                        i += 2;
                    }
                    "--agent-key" => {
                        agent_key = Some(arg(&rest, i)?);
                        i += 2;
                    }
                    _ => return Err(2),
                }
            }
            let id = id.ok_or(2)?;
            let body = serde_json::json!({
                "agent_key": agent_key.unwrap_or_default(),
            });
            let res = opts
                .apply_headers(client.post(opts.url(&format!("/cron/{id}/remove"))).json(&body))
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

async fn cron_cmd_grpc(sub: &str, opts: ApiOpts, rest: Vec<String>) -> Result<(), i32> {
    let mut client = connect_grpc(opts.addr).await?;
    match sub {
        "add" => {
            let mut id: Option<String> = None;
            let mut expression: Option<String> = None;
            let mut agent_key: Option<String> = None;
            let mut i = 0;
            while i < rest.len() {
                match rest[i].as_str() {
                    "--id" => {
                        id = Some(arg(&rest, i)?);
                        i += 2;
                    }
                    "--expression" => {
                        expression = Some(arg(&rest, i)?);
                        i += 2;
                    }
                    "--agent-key" => {
                        agent_key = Some(arg(&rest, i)?);
                        i += 2;
                    }
                    _ => return Err(2),
                }
            }
            client
                .add_cron(id.ok_or(2)?, expression.ok_or(2)?, agent_key.unwrap_or_default())
                .await
                .map_err(|e| { safe_eprintln!("{}", cli_t_with(i18n::key::CLI_GRPC_ERROR, &[("e", &e.to_string())])); 1 })
        }
        "update" => {
            let mut id: Option<String> = None;
            let mut patch: Option<serde_json::Value> = None;
            let mut agent_key: Option<String> = None;
            let mut i = 0;
            while i < rest.len() {
                match rest[i].as_str() {
                    "--id" => {
                        id = Some(arg(&rest, i)?);
                        i += 2;
                    }
                    "--json" => {
                        patch = Some(
                            serde_json::from_str::<serde_json::Value>(rest.get(i + 1).ok_or(2)?)
                                .map_err(|_| 2)?,
                        );
                        i += 2;
                    }
                    "--agent-key" => {
                        agent_key = Some(arg(&rest, i)?);
                        i += 2;
                    }
                    _ => return Err(2),
                }
            }
            let patch_bytes = serde_json::to_vec(&patch.ok_or(2)?).map_err(|_| 1)?;
            client
                .update_cron(id.ok_or(2)?, patch_bytes, agent_key.unwrap_or_default())
                .await
                .map_err(|e| { safe_eprintln!("{}", cli_t_with(i18n::key::CLI_GRPC_ERROR, &[("e", &e.to_string())])); 1 })
        }
        "remove" => {
            let mut id: Option<String> = None;
            let mut agent_key: Option<String> = None;
            let mut i = 0;
            while i < rest.len() {
                match rest[i].as_str() {
                    "--id" => {
                        id = Some(arg(&rest, i)?);
                        i += 2;
                    }
                    "--agent-key" => {
                        agent_key = Some(arg(&rest, i)?);
                        i += 2;
                    }
                    _ => return Err(2),
                }
            }
            client
                .remove_cron(id.ok_or(2)?, agent_key.unwrap_or_default())
                .await
                .map_err(|e| { safe_eprintln!("{}", cli_t_with(i18n::key::CLI_GRPC_ERROR, &[("e", &e.to_string())])); 1 })
        }
        _ => Err(2),
    }
}

async fn config_cmd(args: &[String]) -> Result<(), i32> {
    // Iterator-based parsing: the first entry is the inner subcommand
    // (`show` | `validate` | `set`); the rest are the flag/value pairs.
    // Using `split_first` + a `rest_iter` removes the `let mut i = 1;`
    // index magic that the code review flagged as asymmetric vs
    // `run_cmd` (which uses `i = 0`). The asymmetry was harmless — both
    // functions receive `&args[1..]` from the dispatcher, but in
    // `config_cmd` the first slot is the subcommand while in `run_cmd`
    // the first slot is already a flag — but the magic-numbered offset
    // is a bug surface waiting to happen.
    let (sub, rest) = args.split_first().ok_or(2)?;
    let sub = sub.as_str();
    let mut config_path: Option<PathBuf> = None;
    let mut runtime_override: Option<PathBuf> = None;
    let mut patch_json: Option<String> = None;

    let mut rest_iter = rest.iter();
    while let Some(arg) = rest_iter.next() {
        match arg.as_str() {
            "--config" => {
                config_path = Some(PathBuf::from(rest_iter.next().ok_or(2)?));
            }
            "--override" => {
                runtime_override = Some(PathBuf::from(rest_iter.next().ok_or(2)?));
            }
            "--json" => {
                patch_json = Some(rest_iter.next().ok_or(2)?.to_owned());
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
            safe_println!("{body}");
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
                    safe_eprintln!("{}", cli_t_with(i18n::key::CLI_WARNING, &[("message", &w)]));
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
    safe_eprintln!("{}", cli_t(i18n::key::CLI_USAGE));
}

#[cfg(test)]
mod tests {
    use super::{parse_api_opts, DEFAULT_BIND_ADDR};

    #[test]
    fn parse_api_opts_defaults() {
        let (opts, rest) = parse_api_opts(&[]).expect("default opts");
        assert_eq!(opts.addr.to_string(), DEFAULT_BIND_ADDR);
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
