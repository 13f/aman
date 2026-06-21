#![forbid(unsafe_code)]
// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use serde_json::Value;
use tauri::Emitter;
use tokio::sync::Mutex;

use crate::gateway_client::GatewayClient;
use crate::tts::AutoReader;

/// Event type for final assistant reply — used to trigger TTS auto-read.
const EVT_AGENT_REPLY_READY: &str = "agent:reply_ready";

/// Base delay for exponential backoff on reconnect.
const BASE_DELAY: Duration = Duration::from_millis(500);
/// Maximum backoff delay.
const MAX_DELAY: Duration = Duration::from_secs(30);

/// Launch the single SSE listener, replacing all polling loops.
///
/// Connects to `{gateway_base_url}/events/stream`, parses SSE frames, and
/// dispatches each received message to the matching Tauri event name.
/// Automatically reconnects on disconnect with exponential backoff.
///
/// When `auto_reader` is `Some`, `agent:reply_ready` events also trigger
/// TTS speech synthesis and playback.
pub async fn run_sse_listener(
    app_handle: tauri::AppHandle,
    gateway_client: Arc<Mutex<Option<GatewayClient>>>,
    auto_reader: Option<Arc<AutoReader>>,
) {
    let mut delay = BASE_DELAY;

    loop {
        let base_url = {
            let guard = gateway_client.lock().await;
            guard.as_ref().map(|c| c.base_url.clone())
        };

        let Some(base_url) = base_url else {
            tokio::time::sleep(delay).await;
            delay = (delay * 2).min(MAX_DELAY);
            continue;
        };

        let stream_url = format!("{}/events/stream", base_url.trim_end_matches('/'));

        // Build a dedicated client for SSE — longer timeout since this is a
        // long-lived connection. Keep-alive pings (every 15s from server)
        // prevent idle-read timeouts from firing.
        let client = match reqwest::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(120))
            .build()
        {
            Ok(c) => c,
            Err(e) => {
                tracing::error!(error = %e, "sse_client: failed to build reqwest client");
                tokio::time::sleep(delay).await;
                delay = (delay * 2).min(MAX_DELAY);
                continue;
            }
        };

        match client.get(&stream_url).send().await {
            Ok(resp) => {
                if !resp.status().is_success() {
                    tracing::warn!(status = %resp.status(), "sse_client: non-success status, retrying");
                    tokio::time::sleep(delay).await;
                    delay = (delay * 2).min(MAX_DELAY);
                    continue;
                }
                // Connection succeeded — reset backoff.
                delay = BASE_DELAY;

                let mut stream = resp.bytes_stream();
                let mut buf: Vec<u8> = Vec::new();
                let mut event_type = String::new();
                let mut data_buf = String::new();

                while let Some(chunk_result) = stream.next().await {
                    match chunk_result {
                        Ok(chunk) => {
                            buf.extend_from_slice(&chunk);
                            // Extract complete SSE frames (terminated by \n\n).
                            while let Some(pos) = find_frame_boundary(&buf) {
                                let frame_bytes = &buf[..pos];
                                let frame =
                                    String::from_utf8_lossy(frame_bytes);

                                // Parse frame lines.
                                for line in frame.lines() {
                                    if let Some(value) = line.strip_prefix("event: ") {
                                        event_type = value.trim().to_owned();
                                    } else if let Some(value) = line.strip_prefix("data: ") {
                                        data_buf = value.trim().to_owned();
                                    }
                                    // Ignore comments (lines starting with ':') and
                                    // unknown prefixes.
                                }

                                // Frame complete — emit the event.
                                if !event_type.is_empty() && !data_buf.is_empty() {
                                    let payload: Value =
                                        serde_json::from_str(&data_buf)
                                            .unwrap_or(Value::Null);
                                    // Trigger TTS auto-read for final assistant replies.
                                    if event_type == EVT_AGENT_REPLY_READY
                                        && let Some(ref reader) = auto_reader
                                    {
                                        let reader = Arc::clone(reader);
                                        let tts_payload = payload.clone();
                                        tokio::spawn(async move {
                                            reader.on_reply_ready(tts_payload).await;
                                        });
                                    }
                                    let _ = app_handle.emit(&event_type, payload);
                                }
                                event_type.clear();
                                data_buf.clear();

                                // Remove the processed frame + separator from the
                                // buffer. Safe to drain because pos came from buf.
                                buf.drain(..pos + 2); // +2 for the \n\n separator
                            }
                        }
                        Err(e) => {
                            tracing::warn!(error = %e, "sse_client: stream error, reconnecting");
                            break;
                        }
                    }
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, "sse_client: connection failed, retrying");
            }
        }

        tracing::info!("sse_client: disconnected, reconnecting in {:?}", delay);
        tokio::time::sleep(delay).await;
        delay = (delay * 2).min(MAX_DELAY);
    }
}

/// Find the end of the first complete SSE frame in `buf`.
///
/// An SSE frame is terminated by a double newline (`\n\n`).
/// Returns the byte position just before the `\n\n` separator.
fn find_frame_boundary(buf: &[u8]) -> Option<usize> {
    buf.windows(2).position(|w| w == b"\n\n")
}
