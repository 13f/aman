use kernel::context::SourceContext;
use kernel::event::{Event, EventType};
use kernel::source::EventSource;
use kernel::types::{BackpressureLevel, HealthStatus, SourceType};
use kernel::AmanResult;
use serde_json::Value;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use tokio::io::AsyncReadExt;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::Duration;

const SOCKET_POLL_TIMEOUT_MS: u64 = 25;
const DEFAULT_TCP_USER_TIMEOUT_MS: u64 = 5_000;

#[derive(Debug, Clone, PartialEq, Eq)]
enum SocketBinding {
    Tcp(String),
    Udp(String),
    Unix(PathBuf),
}

pub struct SocketSource {
    id: String,
    binding: SocketBinding,
    initialized: bool,
    paused: Arc<AtomicBool>,
    queue_rx: mpsc::UnboundedReceiver<Event>,
    queue_tx: mpsc::UnboundedSender<Event>,
    tasks: Vec<JoinHandle<()>>,
    local_addr: Option<String>,
    tcp_user_timeout_ms: Arc<AtomicU64>,
}

impl SocketSource {
    #[must_use]
    pub fn new_tcp(id: impl Into<String>, address: impl Into<String>) -> Self {
        Self::new(id, SocketBinding::Tcp(address.into()))
    }

    #[must_use]
    pub fn new_udp(id: impl Into<String>, address: impl Into<String>) -> Self {
        Self::new(id, SocketBinding::Udp(address.into()))
    }

    #[must_use]
    pub fn new_unix(id: impl Into<String>, path: impl Into<PathBuf>) -> Self {
        Self::new(id, SocketBinding::Unix(path.into()))
    }

    fn new(id: impl Into<String>, binding: SocketBinding) -> Self {
        let (queue_tx, queue_rx) = mpsc::unbounded_channel();
        Self {
            id: id.into(),
            binding,
            initialized: false,
            paused: Arc::new(AtomicBool::new(false)),
            queue_rx,
            queue_tx,
            tasks: Vec::new(),
            local_addr: None,
            tcp_user_timeout_ms: Arc::new(AtomicU64::new(DEFAULT_TCP_USER_TIMEOUT_MS)),
        }
    }

    #[cfg(test)]
    fn local_addr(&self) -> Option<String> {
        self.local_addr.clone()
    }

    async fn init_tcp(&mut self, address: &str) -> AmanResult<()> {
        let listener = tokio::net::TcpListener::bind(address).await?;
        self.local_addr = Some(listener.local_addr()?.to_string());
        let paused = Arc::clone(&self.paused);
        let tx = self.queue_tx.clone();
        let source_id = self.id.clone();
        let tcp_user_timeout_ms = Arc::clone(&self.tcp_user_timeout_ms);

        self.tasks.push(tokio::spawn(async move {
            while let Ok((mut stream, peer)) = listener.accept().await {
                let paused = Arc::clone(&paused);
                let tx = tx.clone();
                let source_id = source_id.clone();
                let tcp_user_timeout_ms = Arc::clone(&tcp_user_timeout_ms);
                tokio::spawn(async move {
                    let mut buffer = vec![0_u8; 8192];
                    loop {
                        let wait_ms = if paused.load(Ordering::Acquire) {
                            tcp_user_timeout_ms.load(Ordering::Acquire).max(1)
                        } else {
                            250
                        };
                        let read_result = tokio::time::timeout(
                            Duration::from_millis(wait_ms),
                            stream.read(&mut buffer),
                        )
                        .await;
                        let Ok(Ok(read)) = read_result else {
                            // Backpressure + timeout: close idle TCP connection quickly.
                            if paused.load(Ordering::Acquire) {
                                return;
                            }
                            continue;
                        };
                        if read == 0 {
                            return;
                        }
                        if paused.load(Ordering::Acquire) {
                            return;
                        }
                        let data = String::from_utf8_lossy(&buffer[..read]).to_string();
                        let _ = tx.send(Event::new(
                            source_id,
                            EventType::MessageReceived,
                            serde_json::json!({
                                "transport": "tcp",
                                "peer": peer.to_string(),
                                "data": data
                            }),
                        ));
                        return;
                    }
                });
            }
        }));
        Ok(())
    }

    async fn init_udp(&mut self, address: &str) -> AmanResult<()> {
        let socket = tokio::net::UdpSocket::bind(address).await?;
        self.local_addr = Some(socket.local_addr()?.to_string());
        let paused = Arc::clone(&self.paused);
        let tx = self.queue_tx.clone();
        let source_id = self.id.clone();

        self.tasks.push(tokio::spawn(async move {
            let mut buffer = vec![0_u8; 65_535];
            while let Ok((read, peer)) = socket.recv_from(&mut buffer).await {
                if read == 0 || paused.load(Ordering::Acquire) {
                    continue;
                }
                let data = String::from_utf8_lossy(&buffer[..read]).to_string();
                let _ = tx.send(Event::new(
                    source_id.clone(),
                    EventType::MessageReceived,
                    serde_json::json!({
                        "transport": "udp",
                        "peer": peer.to_string(),
                        "data": data
                    }),
                ));
            }
        }));
        Ok(())
    }

    #[cfg(unix)]
    async fn init_unix(&mut self, path: &PathBuf) -> AmanResult<()> {
        if path.exists() {
            std::fs::remove_file(path)?;
        }
        let listener = tokio::net::UnixListener::bind(path)?;
        self.local_addr = Some(path.to_string_lossy().to_string());
        let paused = Arc::clone(&self.paused);
        let tx = self.queue_tx.clone();
        let source_id = self.id.clone();

        self.tasks.push(tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                let paused = Arc::clone(&paused);
                let tx = tx.clone();
                let source_id = source_id.clone();
                tokio::spawn(async move {
                    let mut buffer = vec![0_u8; 8192];
                    if let Ok(read) = stream.read(&mut buffer).await {
                        if read == 0 || paused.load(Ordering::Acquire) {
                            return;
                        }
                        let data = String::from_utf8_lossy(&buffer[..read]).to_string();
                        let _ = tx.send(Event::new(
                            source_id,
                            EventType::MessageReceived,
                            serde_json::json!({
                                "transport": "unix",
                                "data": data
                            }),
                        ));
                    }
                });
            }
        }));
        Ok(())
    }
}

#[async_trait::async_trait]
impl EventSource for SocketSource {
    fn id(&self) -> &str {
        &self.id
    }

    fn source_type(&self) -> SourceType {
        SourceType::Network
    }

    async fn init(&mut self, _ctx: SourceContext) -> AmanResult<()> {
        if self.initialized {
            return Ok(());
        }

        let binding = self.binding.clone();
        match binding {
            SocketBinding::Tcp(addr) => self.init_tcp(&addr).await?,
            SocketBinding::Udp(addr) => self.init_udp(&addr).await?,
            SocketBinding::Unix(path) => {
                #[cfg(unix)]
                self.init_unix(&path).await?;
                #[cfg(not(unix))]
                {
                    let _ = path;
                    return Err(kernel::Error::Unrecoverable {
                        message: "unix domain socket is not supported on this platform".to_owned(),
                    });
                }
            }
        }
        self.paused.store(false, Ordering::Release);
        self.initialized = true;
        Ok(())
    }

    async fn shutdown(&mut self) -> AmanResult<()> {
        for task in self.tasks.drain(..) {
            task.abort();
        }
        self.paused.store(true, Ordering::Release);
        self.initialized = false;
        Ok(())
    }

    async fn poll(&mut self, _ctx: &SourceContext) -> AmanResult<Vec<Event>> {
        if !self.initialized {
            return Ok(Vec::new());
        }

        match tokio::time::timeout(
            Duration::from_millis(SOCKET_POLL_TIMEOUT_MS),
            self.queue_rx.recv(),
        )
        .await
        {
            Ok(Some(first)) => {
                let mut events = vec![first];
                while let Ok(event) = self.queue_rx.try_recv() {
                    events.push(event);
                    if events.len() >= 256 {
                        break;
                    }
                }
                Ok(events)
            }
            Ok(None) => Ok(Vec::new()),
            Err(_) => Ok(Vec::new()),
        }
    }

    async fn on_backpressure(
        &mut self,
        level: BackpressureLevel,
        _ctx: &SourceContext,
    ) -> AmanResult<()> {
        let should_pause = matches!(
            level,
            BackpressureLevel::L3
                | BackpressureLevel::L4A
                | BackpressureLevel::L4B
                | BackpressureLevel::Critical
        );
        self.paused.store(should_pause, Ordering::Release);
        Ok(())
    }

    fn health(&self) -> HealthStatus {
        if self.initialized {
            HealthStatus::Ok
        } else {
            HealthStatus::Degraded
        }
    }

    async fn pause(&mut self) -> AmanResult<()> {
        self.paused.store(true, Ordering::Release);
        Ok(())
    }

    async fn resume(&mut self) -> AmanResult<()> {
        if self.initialized {
            self.paused.store(false, Ordering::Release);
        }
        Ok(())
    }

    async fn reconfigure(&mut self, _config: Value) -> AmanResult<()> {
        if let Some(timeout_ms) = _config.get("tcp_user_timeout_ms").and_then(Value::as_u64) {
            self.tcp_user_timeout_ms
                .store(timeout_ms.max(1), Ordering::Release);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::SocketSource;
    use kernel::context::{BaseContext, SourceContext};
    use kernel::event::EventType;
    use kernel::source::EventSource;
    use kernel::types::{BackpressureLevel, TraceId};
    use tokio::io::AsyncReadExt;
    use tokio::time::Duration;

    fn context() -> SourceContext {
        SourceContext {
            base: BaseContext::new(TraceId::new()),
            source_name: Some("socket:test".to_owned()),
        }
    }

    #[tokio::test]
    async fn udp_socket_emits_message_received_event() {
        let mut source = SocketSource::new_udp("socket:udp", "127.0.0.1:0");
        let ctx = context();
        source.init(ctx.clone()).await.expect("init");
        let local_addr = source
            .local_addr()
            .expect("source should have local address");

        let sender = tokio::net::UdpSocket::bind("127.0.0.1:0")
            .await
            .expect("sender bind");
        sender
            .send_to(b"ping", local_addr.as_str())
            .await
            .expect("send");

        let mut events = Vec::new();
        for _ in 0..20 {
            events = source.poll(&ctx).await.expect("poll");
            if !events.is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        assert!(!events.is_empty());
        assert_eq!(events[0].event_type, EventType::MessageReceived);
        assert_eq!(
            events[0].payload.get("transport"),
            Some(&serde_json::Value::String("udp".to_owned()))
        );
    }

    #[tokio::test]
    async fn l3_backpressure_pauses_udp_receiving() {
        let mut source = SocketSource::new_udp("socket:udp", "127.0.0.1:0");
        let ctx = context();
        source.init(ctx.clone()).await.expect("init");
        source
            .on_backpressure(BackpressureLevel::L3, &ctx)
            .await
            .expect("set l3");

        let local_addr = source
            .local_addr()
            .expect("source should have local address");
        let sender = tokio::net::UdpSocket::bind("127.0.0.1:0")
            .await
            .expect("sender bind");
        sender
            .send_to(b"blocked", local_addr.as_str())
            .await
            .expect("send");
        tokio::time::sleep(Duration::from_millis(20)).await;
        let events = source.poll(&ctx).await.expect("poll");
        assert!(events.is_empty(), "events should be suppressed during L3");
    }

    #[tokio::test]
    async fn tcp_user_timeout_closes_idle_connection_when_paused() {
        let mut source = SocketSource::new_tcp("socket:tcp", "127.0.0.1:0");
        let ctx = context();
        source
            .reconfigure(serde_json::json!({ "tcp_user_timeout_ms": 40 }))
            .await
            .expect("set tcp user timeout");
        source.init(ctx.clone()).await.expect("init");

        let local_addr = source
            .local_addr()
            .expect("source should have local address");
        let mut client = tokio::net::TcpStream::connect(local_addr)
            .await
            .expect("connect");
        source
            .on_backpressure(BackpressureLevel::L3, &ctx)
            .await
            .expect("set l3");

        let mut buf = [0_u8; 1];
        let read = tokio::time::timeout(Duration::from_secs(1), client.read(&mut buf))
            .await
            .expect("read should complete within timeout")
            .expect("read should not error");
        assert_eq!(read, 0, "server should close idle connection on timeout");
    }
}
