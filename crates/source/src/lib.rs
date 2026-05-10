#![forbid(unsafe_code)]
#![doc = "Event source registry and built-in sources for the Aman framework."]

mod cron;
mod file_watch;
mod registry;
mod signal;
mod socket;
mod timer;
mod webhook;

pub use cron::{CronManager, CronSource};
pub use file_watch::FileWatchSource;
pub use registry::{
    should_pause_push_sources, SourceLifecycleState, SourceMode, SourceRegistry, SourceSnapshot,
    TrustLevel,
};
pub use signal::SignalSource;
pub use socket::SocketSource;
pub use timer::TimerSource;
pub use webhook::WebhookSource;

#[cfg(test)]
mod tests {
    use super::{
        FileWatchSource, SignalSource, SocketSource, SourceLifecycleState, SourceMode,
        SourceRegistry, TimerSource, TrustLevel, WebhookSource,
    };
    use async_trait::async_trait;
    use event_bus::{EventBus, EventHandler, InMemoryBus, SubscriptionFilter};
    use kernel::context::SourceContext;
    use kernel::event::{Event, EventType};
    use kernel::types::{BackpressureLevel, HealthStatus, SourceType};
    use kernel::AmanResult;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::{collections::HashSet, fs, path::PathBuf};
    use tokio::sync::Mutex;
    use tokio::time::{timeout, Duration};

    #[derive(Default)]
    struct RecordingHandler {
        events: Mutex<Vec<Event>>,
    }

    impl RecordingHandler {
        async fn snapshot(&self) -> Vec<Event> {
            self.events.lock().await.clone()
        }
    }

    struct SharedHandler(Arc<RecordingHandler>);

    #[async_trait]
    impl EventHandler for SharedHandler {
        async fn handle(&self, event: Event) -> AmanResult<()> {
            self.0.events.lock().await.push(event);
            Ok(())
        }
    }

    struct SigtermPipelineHandler {
        triggered: Arc<AtomicBool>,
    }

    #[async_trait]
    impl EventHandler for SigtermPipelineHandler {
        async fn handle(&self, event: Event) -> AmanResult<()> {
            if event.event_type == EventType::SystemSignal
                && event.payload.get("signal") == Some(&serde_json::json!("SIGTERM"))
            {
                self.triggered.store(true, Ordering::Release);
            }
            Ok(())
        }
    }

    struct PushProbe {
        id: String,
        paused: bool,
    }

    #[async_trait]
    impl kernel::source::EventSource for PushProbe {
        fn id(&self) -> &str {
            &self.id
        }

        fn source_type(&self) -> SourceType {
            SourceType::Webhook
        }

        async fn init(&mut self, _ctx: SourceContext) -> AmanResult<()> {
            Ok(())
        }

        async fn shutdown(&mut self) -> AmanResult<()> {
            Ok(())
        }

        async fn pause(&mut self) -> AmanResult<()> {
            self.paused = true;
            Ok(())
        }

        async fn resume(&mut self) -> AmanResult<()> {
            self.paused = false;
            Ok(())
        }

        fn health(&self) -> HealthStatus {
            if self.paused {
                HealthStatus::Degraded
            } else {
                HealthStatus::Ok
            }
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn registry_rejects_duplicate_source_ids() {
        let bus: Arc<dyn EventBus> = Arc::new(InMemoryBus::default());
        let registry = SourceRegistry::new(bus);

        registry
            .register(
                Box::new(TimerSource::new("timer:dup", 10, false)),
                SourceMode::Pull,
                TrustLevel::Untrusted,
            )
            .await
            .expect("first registration should succeed");

        let error = registry
            .register(
                Box::new(TimerSource::new("timer:dup", 10, false)),
                SourceMode::Pull,
                TrustLevel::Untrusted,
            )
            .await
            .expect_err("duplicate registration should fail");

        assert!(matches!(error, kernel::Error::AlreadyExists { .. }));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn lifecycle_flows_register_start_pause_resume_shutdown_unregister() {
        let bus: Arc<dyn EventBus> = Arc::new(InMemoryBus::default());
        let registry = SourceRegistry::new(bus);
        let id = "timer:lifecycle";

        registry
            .register(
                Box::new(TimerSource::new(id, 10, false)),
                SourceMode::Pull,
                TrustLevel::Untrusted,
            )
            .await
            .expect("register");
        assert_eq!(
            registry
                .get(id)
                .await
                .expect("source snapshot should exist")
                .state,
            SourceLifecycleState::Registered
        );

        registry.start(id).await.expect("start");
        assert_eq!(
            registry
                .get(id)
                .await
                .expect("source snapshot should exist")
                .state,
            SourceLifecycleState::Running
        );

        registry.pause(id).await.expect("pause");
        assert_eq!(
            registry
                .get(id)
                .await
                .expect("source snapshot should exist")
                .state,
            SourceLifecycleState::Paused
        );

        registry.resume(id).await.expect("resume");
        assert_eq!(
            registry
                .get(id)
                .await
                .expect("source snapshot should exist")
                .state,
            SourceLifecycleState::Running
        );

        registry.shutdown(id).await.expect("shutdown");
        assert_eq!(
            registry
                .get(id)
                .await
                .expect("source snapshot should exist")
                .state,
            SourceLifecycleState::Shutdown
        );

        registry.unregister(id).await.expect("unregister");
        assert!(
            registry.get(id).await.is_none(),
            "source should be absent after unregister"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn timer_source_publishes_into_event_bus_with_trust_level() {
        let bus = Arc::new(InMemoryBus::default());
        let registry = SourceRegistry::new(bus.clone() as Arc<dyn EventBus>);

        let handler = Arc::new(RecordingHandler::default());
        bus.subscribe(
            SubscriptionFilter::default(),
            Box::new(SharedHandler(handler.clone())),
        )
        .await
        .expect("subscribe should succeed");

        registry
            .register(
                Box::new(TimerSource::new("timer:integration", 5, true)),
                SourceMode::Pull,
                TrustLevel::Sandboxed,
            )
            .await
            .expect("register");
        registry.start("timer:integration").await.expect("start");

        timeout(Duration::from_secs(1), async {
            loop {
                if !handler.snapshot().await.is_empty() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("timer should publish at least one event");

        registry
            .shutdown("timer:integration")
            .await
            .expect("shutdown");

        let events = handler.snapshot().await;
        assert!(!events.is_empty());
        let first = &events[0];
        assert_eq!(first.event_type, EventType::Heartbeat);
        assert_eq!(
            first.payload.get("_aman_trust_level"),
            Some(&serde_json::Value::String("sandboxed".to_owned()))
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn push_sources_pause_on_l3_and_resume_on_normal() {
        let bus: Arc<dyn EventBus> = Arc::new(InMemoryBus::default());
        let registry = SourceRegistry::new(bus);

        registry
            .register(
                Box::new(PushProbe {
                    id: "webhook:push".to_owned(),
                    paused: false,
                }),
                SourceMode::Push,
                TrustLevel::Untrusted,
            )
            .await
            .expect("register");
        registry.start("webhook:push").await.expect("start");

        registry
            .apply_backpressure(BackpressureLevel::L3)
            .await
            .expect("apply l3");
        assert_eq!(
            registry
                .get("webhook:push")
                .await
                .expect("source snapshot should exist")
                .state,
            SourceLifecycleState::Paused
        );

        registry
            .apply_backpressure(BackpressureLevel::Normal)
            .await
            .expect("recover");
        assert_eq!(
            registry
                .get("webhook:push")
                .await
                .expect("source snapshot should exist")
                .state,
            SourceLifecycleState::Running
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn webhook_source_injects_events_and_returns_503_at_l3() {
        let bus = Arc::new(InMemoryBus::default());
        let registry = SourceRegistry::new(bus.clone() as Arc<dyn EventBus>);
        let recorder = Arc::new(RecordingHandler::default());
        bus.subscribe(
            SubscriptionFilter::default(),
            Box::new(SharedHandler(recorder.clone())),
        )
        .await
        .expect("subscribe");

        let port = reserve_local_port();
        registry
            .register(
                Box::new(WebhookSource::new("webhook:test", "/ingest", port)),
                SourceMode::Push,
                TrustLevel::Untrusted,
            )
            .await
            .expect("register webhook source");
        registry.start("webhook:test").await.expect("start webhook source");

        let client = reqwest::Client::builder()
            .no_proxy()
            .build()
            .expect("client should build");
        let accepted = send_with_retry(
            &client,
            format!("http://127.0.0.1:{port}/ingest"),
            serde_json::json!({"hello": "world"}),
        )
        .await;
        assert_eq!(accepted, reqwest::StatusCode::ACCEPTED);

        timeout(Duration::from_secs(1), async {
            loop {
                if !recorder.snapshot().await.is_empty() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("webhook event should enter bus");

        registry
            .apply_backpressure(BackpressureLevel::L3)
            .await
            .expect("set l3");
        let blocked = client
            .post(format!("http://127.0.0.1:{port}/ingest"))
            .json(&serde_json::json!({"drop": "me"}))
            .send()
            .await
            .expect("request should complete");
        assert_eq!(blocked.status(), reqwest::StatusCode::SERVICE_UNAVAILABLE);

        registry
            .shutdown("webhook:test")
            .await
            .expect("shutdown webhook source");
    }

    fn reserve_local_port() -> u16 {
        let listener =
            std::net::TcpListener::bind("127.0.0.1:0").expect("should reserve a local port");
        let port = listener
            .local_addr()
            .expect("listener should expose local addr")
            .port();
        drop(listener);
        port
    }

    async fn send_with_retry(client: &reqwest::Client, url: String, payload: serde_json::Value) -> reqwest::StatusCode {
        for _ in 0..20 {
            if let Ok(response) = client.post(&url).json(&payload).send().await {
                return response.status();
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("webhook server did not become reachable in time");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn sources_pause_and_resume_affect_timer_emission() {
        let bus = Arc::new(InMemoryBus::default());
        let registry = SourceRegistry::new(bus.clone() as Arc<dyn EventBus>);
        let recorder = Arc::new(RecordingHandler::default());
        bus.subscribe(
            SubscriptionFilter::default(),
            Box::new(SharedHandler(recorder.clone())),
        )
        .await
        .expect("subscribe");

        registry
            .register(
                Box::new(TimerSource::new("timer:pause-resume", 20, false)),
                SourceMode::Pull,
                TrustLevel::Untrusted,
            )
            .await
            .expect("register timer");
        registry
            .start("timer:pause-resume")
            .await
            .expect("start timer");

        timeout(Duration::from_secs(1), async {
            loop {
                if !recorder.snapshot().await.is_empty() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("timer should emit before pause");

        registry
            .pause("timer:pause-resume")
            .await
            .expect("pause timer");
        let paused_count = recorder.snapshot().await.len();
        // Allow one in-flight emission around the pause boundary.
        tokio::time::sleep(Duration::from_millis(40)).await;
        let after_pause_drain = recorder.snapshot().await.len();
        assert!(
            after_pause_drain <= paused_count + 1,
            "pause boundary should drain quickly"
        );
        tokio::time::sleep(Duration::from_millis(120)).await;
        assert_eq!(
            recorder.snapshot().await.len(),
            after_pause_drain,
            "no new events should appear while paused"
        );

        registry
            .resume("timer:pause-resume")
            .await
            .expect("resume timer");
        timeout(Duration::from_secs(1), async {
            loop {
                if recorder.snapshot().await.len() > paused_count {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("timer should resume emitting");
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn all_built_sources_can_register_start_emit_and_reach_bus() {
        let bus = Arc::new(InMemoryBus::default());
        let registry = SourceRegistry::new(bus.clone() as Arc<dyn EventBus>);
        let recorder = Arc::new(RecordingHandler::default());
        bus.subscribe(
            SubscriptionFilter::default(),
            Box::new(SharedHandler(recorder.clone())),
        )
        .await
        .expect("subscribe");

        let webhook_port = reserve_local_port();
        let udp_port = reserve_local_port();
        let watch_dir = temp_watch_dir("all-sources");
        let watch_file = watch_dir.join("demo.txt");

        let mut watch = FileWatchSource::new("watch:all", vec![watch_dir.clone()]);
        kernel::source::EventSource::reconfigure(
            &mut watch,
            serde_json::json!({"debounce_ms": 50, "max_stable_wait_ms": 2000}),
        )
        .await
        .expect("reconfigure watch");

        registry
            .register(
                Box::new(TimerSource::new("timer:all", 20, false)),
                SourceMode::Pull,
                TrustLevel::Untrusted,
            )
            .await
            .expect("register timer");
        registry
            .register(
                Box::new(WebhookSource::new("webhook:all", "/ingest", webhook_port)),
                SourceMode::Push,
                TrustLevel::Untrusted,
            )
            .await
            .expect("register webhook");
        registry
            .register(
                Box::new(SocketSource::new_udp(
                    "socket:all",
                    format!("127.0.0.1:{udp_port}"),
                )),
                SourceMode::Push,
                TrustLevel::Untrusted,
            )
            .await
            .expect("register socket");
        registry
            .register(Box::new(watch), SourceMode::Pull, TrustLevel::Untrusted)
            .await
            .expect("register file watch");
        registry
            .register(
                Box::new(SignalSource::new("signal:all")),
                SourceMode::Pull,
                TrustLevel::Untrusted,
            )
            .await
            .expect("register signal");

        for id in [
            "timer:all",
            "webhook:all",
            "socket:all",
            "watch:all",
            "signal:all",
        ] {
            registry.start(id).await.expect("start source");
        }

        let client = reqwest::Client::builder()
            .no_proxy()
            .build()
            .expect("client should build");
        let status = send_with_retry(
            &client,
            format!("http://127.0.0.1:{webhook_port}/ingest"),
            serde_json::json!({"from": "webhook"}),
        )
        .await;
        assert_eq!(status, reqwest::StatusCode::ACCEPTED);

        let udp_sender = tokio::net::UdpSocket::bind("127.0.0.1:0")
            .await
            .expect("bind udp sender");
        udp_sender
            .send_to(b"hello", format!("127.0.0.1:{udp_port}"))
            .await
            .expect("send udp");

        fs::write(&watch_file, "hello").expect("write watched file");

        let pid = std::process::id().to_string();
        let signal_status = std::process::Command::new("kill")
            .args(["-USR1", &pid])
            .status()
            .expect("invoke kill");
        assert!(signal_status.success(), "kill -USR1 should succeed");

        let expected = HashSet::from([
            "timer:all".to_owned(),
            "webhook:all".to_owned(),
            "socket:all".to_owned(),
            "watch:all".to_owned(),
            "signal:all".to_owned(),
        ]);
        timeout(Duration::from_secs(3), async {
            loop {
                let got = recorder
                    .snapshot()
                    .await
                    .into_iter()
                    .map(|event| event.source.to_string())
                    .collect::<HashSet<_>>();
                if expected.is_subset(&got) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("all sources should emit into bus");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn signal_sigterm_event_triggers_pipeline_response() {
        let bus = Arc::new(InMemoryBus::default());
        let registry = SourceRegistry::new(bus.clone() as Arc<dyn EventBus>);
        let triggered = Arc::new(AtomicBool::new(false));
        bus.subscribe(
            SubscriptionFilter::default(),
            Box::new(SigtermPipelineHandler {
                triggered: triggered.clone(),
            }),
        )
        .await
        .expect("subscribe pipeline handler");

        registry
            .register(
                Box::new(SignalSource::new("signal:pipeline")),
                SourceMode::Pull,
                TrustLevel::Untrusted,
            )
            .await
            .expect("register signal source");
        registry
            .start("signal:pipeline")
            .await
            .expect("start signal source");

        registry
            .reconfigure(
                "signal:pipeline",
                serde_json::json!({"inject_signal": "SIGTERM"}),
            )
            .await
            .expect("inject sigterm event");

        timeout(Duration::from_secs(1), async {
            loop {
                if triggered.load(Ordering::Acquire) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("pipeline handler should react to SIGTERM");
    }

    fn temp_watch_dir(suffix: &str) -> PathBuf {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("time")
            .as_nanos();
        let path = std::env::temp_dir().join(format!("aman-source-{suffix}-{nonce}"));
        fs::create_dir_all(&path).expect("create watch dir");
        path
    }
}
