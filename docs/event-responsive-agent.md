// 提问
当前agent 都是基于聊天进行交互，自动化只有cron/定时任务。我实现一个支持事件/响应机制的agent，你认为应该参考哪个操作系统？
暂时想到的需求/场景，支持对如下事件的响应：
1. 某个目录下文本文件发生了修改
2. 第三方产品的事件通知，包括但不限于socket 响应、轮询（比如rss/atom或股票价格更新）、聊天平台sdk 收到了新消息
3. 监控某个sqlite/json数据，比如增加了新任务或者任务的流转状态发生了变更
4. 心跳机制（频率基本固定的定时任务）
5. 可变更频率的定时任务

---

Kimi 2.6

---

从需求来看，你的 Agent 是一个**事件驱动的异步系统**，需要同时处理文件系统变更、网络 I/O、数据状态变更、定时器和动态频率调整。参考操作系统的话：

## 结论：以 Linux 为骨架，吸收 Windows 的线程调度思想

### 为什么 Linux 更适合作为骨架

1. **一切皆 fd，统一事件循环**
   - 你的 5 个需求本质都是"某个 fd 就绪了"：
     - 文件修改 → `inotify`/`fanotify` fd
     - Socket/RSS 轮询 → socket fd
     - SQLite/JSON 变更 → 自建 `eventfd` 或定时器轮询 fd
     - 心跳/定时任务 → `timerfd`
   - 全部可以塞进一个 `epoll_wait` 循环，用单线程事件循环（Reactor 模式）统一收割，代码路径清晰 

2. **io_uring 的潜力**
   - 如果后期需要高吞吐（比如批量处理文件、大量 socket 连接），`io_uring` 的批处理和 Completion-based 模型可以直接升级，不用推翻架构 

3. **灵活性**
   - Windows IOCP 要求"先投递 buffer，后等完成"，这对你的场景反而累赘——比如文件修改通知不需要预分配 buffer，收到 `inotify` 事件后再决定怎么处理更自然 

### 吸收 Windows 的什么

- **IOCP 的线程池调度思想**：Linux 只负责通知，线程管理你自己写。可以借鉴 IOCP 的"并发度 ≈ CPU 核心数"逻辑，在事件循环后接工作线程池，防止事件风暴把系统拖垮 
- **完成通知的语义**：对于 RSS 轮询、股票 API 这类"请求-响应"明确的操作，内部可以用类似 Completion 的 Future/Promise 模型封装，逻辑更清晰

---

## 针对你 5 个需求的具体映射

| 需求 | Linux 机制 | 实现建议 |
|------|-----------|---------|
| **1. 目录文件修改** | `inotify`/`fanotify` → fd → `epoll` | 监控目录 `IN_MODIFY`/`IN_CLOSE_WRITE`，注意 `inotify` 不递归，子目录需自行注册 |
| **2. 第三方事件（Socket/RSS/聊天 SDK）** | Socket fd + 非阻塞 I/O；RSS 用 `timerfd` 定时触发轮询 | Socket 事件直接进 `epoll`；RSS/股票等无推送的，用 `timerfd` 做自适应轮询间隔 |
| **3. SQLite/JSON 数据变更** | 无原生机制。方案 A：应用层写 `eventfd` 触发；方案 B：`timerfd` 低频轮询 + 校验和/版本号 diff | 推荐方案 A（事务提交后手动 `write(eventfd)`），保证实时性且不过度消耗资源 |
| **4. 心跳（固定频率）** | `timerfd_create` + `epoll` | 比 `cron` 精确，纳秒级可控，且统一在事件循环内 |
| **5. 可变频率定时任务** | `timerfd_settime` 动态修改 `it_value`/`it_interval | 无需重建定时器，直接改参数，fd 不变，epoll 无需重新注册  |

---

## 架构草图（伪代码思路）

```python
# 核心：一个 epoll 事件循环（单线程 Reactor）
epoll = Epoll()

# 注册所有事件源为 fd
epoll.register(inotify_fd, FILE_MODIFY)
epoll.register(socket_fd, SOCKET_READABLE)
epoll.register(eventfd_from_sqlite, DATA_CHANGED)
epoll.register(timerfd_heartbeat, HEARTBEAT)
epoll.register(timerfd_rss_poll, RSS_POLL)

while True:
    events = epoll.wait()
    for fd, event_type in events:
        # 1. 非阻塞读取/收割
        data = fd.read()
        # 2. 生成任务，丢进线程池（借鉴 Windows IOCP 的线程调度）
        thread_pool.submit(handler_map[event_type], data)
```

---

## 如果非要选一个：选 Linux

Windows IOCP 的 Completion 模型对你的场景**过度设计**——它更适合"高吞吐、预分配 buffer、内核代劳 I/O"的场景（如文件服务器、游戏引擎网络层）。你的 Agent 是**事件响应型**而非**高并发 I/O 吞吐型**，Linux 的 Readiness + fd 统一抽象更轻量、更灵活，且 `io_uring` 保留了未来升级空间。

**一句话：用 Linux 的"统一事件循环"做骨架，用 Windows 的"线程池调度"做肌肉。**

---

// 提问

直接使用rust吧，一步到位。基于前面探讨的事件响应机制，提供一套 agent 核心代码框架或状态机设计思路。最好能先定义一套总揽所以需求的的接口（类似抽象类的设计），具体的事件可以做自己的实现，但是调用接口保持一致

---

Kimi 2.6

---

基于前面的讨论，用 Rust 一步到位。核心设计：**统一事件源（EventSource）接口 + 事件循环（Reactor）+ 状态机（StateMachine）+ 工作线程池**。

## 核心架构

```
┌─────────────────────────────────────────────┐
│              Event Loop (Reactor)            │
│  ┌─────────┐  ┌─────────┐  ┌─────────┐       │
│  │ SourceA │  │ SourceB │  │ SourceC │ ...   │  ← 统一 EventSource 接口
│  └───┬─────┘  └───┬─────┘  └───┬─────┘       │
│      └──────────────┴──────────────┘           │
│                 mio::Poll (epoll/kqueue/IOCP) │
└─────────────────────────────────────────────┘
                      │
                      ▼
┌─────────────────────────────────────────────┐
│           Event Dispatcher                   │
│   (路由到对应 StateMachine + Handler)        │
└─────────────────────────────────────────────┘
                      │
                      ▼
┌─────────────────────────────────────────────┐
│         Worker Pool (tokio::spawn)            │
│   (执行具体业务逻辑，可变更并发度)              │
└─────────────────────────────────────────────┘
```

## 核心接口定义

```rust
use std::any::Any;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use mio::{Events, Interest, Poll, Token};

// ==================== 统一事件抽象 ====================

/// 所有事件的基类
#[derive(Debug, Clone)]
pub enum Event {
    /// 文件系统事件
    Fs(FsEvent),
    /// 网络/Socket 事件
    Net(NetEvent),
    /// 数据变更事件
    Data(DataEvent),
    /// 定时/心跳事件
    Timer(TimerEvent),
    /// 第三方平台事件（聊天 SDK 等）
    Platform(PlatformEvent),
    /// 用户自定义扩展
    Custom(String, Box<dyn Any + Send>),
}

/// 文件系统事件
#[derive(Debug, Clone)]
pub struct FsEvent {
    pub path: std::path::PathBuf,
    pub kind: FsEventKind,
}

#[derive(Debug, Clone)]
pub enum FsEventKind {
    Modified,
    Created,
    Deleted,
    Renamed,
}

/// 网络事件
#[derive(Debug, Clone)]
pub struct NetEvent {
    pub source_id: String,
    pub payload: Vec<u8>,
    pub protocol: Protocol,
}

#[derive(Debug, Clone)]
pub enum Protocol {
    Tcp,
    Udp,
    WebSocket,
    Http,
}

/// 数据变更事件
#[derive(Debug, Clone)]
pub struct DataEvent {
    pub source_id: String,
    pub table: String,
    pub operation: DataOp,
    pub row_id: Option<String>,
    pub changes: serde_json::Value,
}

#[derive(Debug, Clone)]
pub enum DataOp {
    Insert,
    Update,
    Delete,
}

/// 定时器事件
#[derive(Debug, Clone)]
pub struct TimerEvent {
    pub timer_id: String,
    pub scheduled_at: Instant,
    pub is_heartbeat: bool,
}

/// 第三方平台事件
#[derive(Debug, Clone)]
pub struct PlatformEvent {
    pub platform: String,  // "discord", "slack", "wechat"...
    pub event_type: String,
    pub raw_payload: serde_json::Value,
}

// ==================== 事件源接口（核心抽象）====================

/// 所有事件源必须实现的 trait
/// 类比 Java 的抽象类：定义生命周期 + 事件产出 + 控制方法
#[async_trait::async_trait]
pub trait EventSource: Send + Sync + 'static {
    /// 唯一标识
    fn id(&self) -> &str;
    
    /// 事件源类型
    fn source_type(&self) -> SourceType;
    
    /// 初始化：注册到 mio Poll，建立连接，启动后台任务
    /// 返回一个 mio Token 用于事件分发
    async fn init(&mut self, poll: &mut Poll, token: Token) -> anyhow::Result<()>;
    
    /// 当 mio 通知该 Token 就绪时，调用此方法收割事件
    /// 返回 Vec<Event> 可能为 0（如 EAGAIN）
    fn poll_events(&mut self, events: &mio::event::Event) -> anyhow::Result<Vec<Event>>;
    
    /// 动态控制：暂停事件产出（但不销毁）
    async fn pause(&mut self) -> anyhow::Result<()>;
    
    /// 动态控制：恢复事件产出
    async fn resume(&mut self) -> anyhow::Result<()>;
    
    /// 动态控制：调整参数（如变更定时器频率、修改轮询间隔）
    /// 通过 serde_json::Value 传递参数，实现运行时配置变更
    async fn reconfigure(&mut self, config: serde_json::Value) -> anyhow::Result<()>;
    
    /// 优雅关闭
    async fn shutdown(&mut self) -> anyhow::Result<()>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceType {
    FileSystem,
    Network,
    Database,
    Timer,
    Platform,
    Custom,
}

// ==================== 事件处理器接口 ====================

/// 状态机/处理器接口
/// 每个 Event 类型可对应一个 Handler，或一个 Handler 处理多种 Event
#[async_trait::async_trait]
pub trait EventHandler: Send + Sync + 'static {
    /// 处理器标识
    fn name(&self) -> &str;
    
    /// 是否关心该事件（过滤）
    fn interested_in(&self, event: &Event) -> bool;
    
    /// 处理事件，返回状态转换结果
    async fn handle(&self, event: Event, ctx: &mut HandlerContext) -> HandlerResult;
}

/// 处理器上下文：提供存储、日志、发送新事件等能力
pub struct HandlerContext {
    pub source_id: String,
    pub event_bus: mpsc::Sender<Event>,
    pub state_store: std::sync::Arc<dyn StateStore>,
    // ... 扩展
}

pub enum HandlerResult {
    Handled,           // 已处理，无后续
    HandledWithEvent(Event),  // 处理并产生新事件（链式响应）
    RetryLater(Duration),     // 暂无法处理，延迟重试
    Error(anyhow::Error),
}

// ==================== 状态机（高级抽象）====================

/// 有状态的事件处理器
/// 适合任务流转、订单状态机等场景
#[async_trait::async_trait]
pub trait StateMachine: EventHandler {
    /// 当前状态
    fn current_state(&self) -> &str;
    
    /// 状态转移表：从 (当前状态, 事件类型) -> 目标状态
    fn transitions(&self) -> &[(String, String, String)];
    
    /// 进入新状态时的回调
    async fn on_enter_state(&mut self, state: &str, ctx: &mut HandlerContext) -> anyhow::Result<()>;
    
    /// 离开状态时的回调
    async fn on_leave_state(&mut self, state: &str, ctx: &mut HandlerContext) -> anyhow::Result<()>;
}
```

## 具体实现示例

### 1. 文件监控事件源（inotify/fanotify）

```rust
use notify::{Config, Event as NotifyEvent, RecommendedWatcher, RecursiveMode, Watcher};
use std::path::PathBuf;

pub struct FsEventSource {
    id: String,
    path: PathBuf,
    watcher: Option<RecommendedWatcher>,
    event_tx: mpsc::Sender<Event>,
    // mio 适配：notify 本身是同步的，需要桥接到 mio
    // 方案：使用 mio::Waker 或自定义 pipe/eventfd 唤醒 Poll
}

#[async_trait::async_trait]
impl EventSource for FsEventSource {
    fn id(&self) -> &str { &self.id }
    fn source_type(&self) -> SourceType { SourceType::FileSystem }
    
    async fn init(&mut self, poll: &mut Poll, token: Token) -> anyhow::Result<()> {
        // notify crate 内部用 inotify/kqueue/fsevents
        // 通过 channel 桥接到 mio：创建 pipe，将 notify 的 channel 数据写入 pipe
        let (tx, mut rx) = mpsc::channel::<NotifyEvent>(1024);
        
        let mut watcher = notify::recommended_watcher(move |res| {
            if let Ok(event) = res {
                let _ = tx.try_send(event);
            }
        })?;
        
        watcher.watch(&self.path, RecursiveMode::Recursive)?;
        self.watcher = Some(watcher);
        
        // 注册 pipe 读端到 mio
        // ... 具体实现略，核心思路：用 mio::unix::pipe 或 eventfd 桥接
        Ok(())
    }
    
    fn poll_events(&mut self, _events: &mio::event::Event) -> anyhow::Result<Vec<Event>> {
        // 从内部 channel 批量读取 notify 事件，转换为统一 Event
        let mut result = Vec::new();
        // ... 实现略
        Ok(result)
    }
    
    async fn pause(&mut self) -> anyhow::Result<()> { Ok(()) }
    async fn resume(&mut self) -> anyhow::Result<()> { Ok(()) }
    
    async fn reconfigure(&mut self, config: serde_json::Value) -> anyhow::Result<()> {
        // 支持动态修改监控路径、递归模式等
        if let Some(new_path) = config.get("path").and_then(|v| v.as_str()) {
            self.path = PathBuf::from(new_path);
            // 重新初始化 watcher
        }
        Ok(())
    }
    
    async fn shutdown(&mut self) -> anyhow::Result<()> {
        if let Some(w) = self.watcher.take() {
            drop(w);
        }
        Ok(())
    }
}
```

### 2. 定时器/心跳事件源（支持动态频率变更）

```rust
use tokio::time::{interval, Interval, MissedTickBehavior};
use std::sync::atomic::{AtomicBool, Ordering};

pub struct TimerEventSource {
    id: String,
    timer_id: String,
    interval: Interval,
    period: Duration,
    is_heartbeat: bool,
    paused: AtomicBool,
    // mio 适配：tokio::time 与 mio 集成
    // 方案：使用 tokio::runtime 的 timer，通过 mio::Waker 唤醒
}

impl TimerEventSource {
    pub fn new(id: String, timer_id: String, period: Duration, is_heartbeat: bool) -> Self {
        let mut interval = interval(period);
        interval.set_missed_tick_behavior(MissedTickBehavior::Skip);
        Self {
            id, timer_id, interval, period, is_heartbeat,
            paused: AtomicBool::new(false),
        }
    }
}

#[async_trait::async_trait]
impl EventSource for TimerEventSource {
    fn id(&self) -> &str { &self.id }
    fn source_type(&self) -> SourceType { SourceType::Timer }
    
    async fn init(&mut self, _poll: &mut Poll, _token: Token) -> anyhow::Result<()> {
        // tokio timer 不需要注册到 mio，直接 spawn 任务
        // 但为了统一架构，可以通过 mio::Waker 将 timer 事件注入主循环
        Ok(())
    }
    
    fn poll_events(&mut self, _events: &mio::event::Event) -> anyhow::Result<Vec<Event>> {
        if self.paused.load(Ordering::Relaxed) {
            return Ok(vec![]);
        }
        // 检查 interval 是否到期
        // 由于 tokio timer 是 async 的，这里需要特殊处理
        // 实际实现中，timer 可能单独跑在 tokio task 里，通过 channel 发送
        Ok(vec![Event::Timer(TimerEvent {
            timer_id: self.timer_id.clone(),
            scheduled_at: Instant::now(),
            is_heartbeat: self.is_heartbeat,
        })])
    }
    
    async fn pause(&mut self) -> anyhow::Result<()> {
        self.paused.store(true, Ordering::Relaxed);
        Ok(())
    }
    
    async fn resume(&mut self) -> anyhow::Result<()> {
        self.paused.store(false, Ordering::Relaxed);
        Ok(())
    }
    
    async fn reconfigure(&mut self, config: serde_json::Value) -> anyhow::Result<()> {
        // 动态变更频率：核心需求
        if let Some(new_period_ms) = config.get("period_ms").and_then(|v| v.as_u64()) {
            self.period = Duration::from_millis(new_period_ms);
            self.interval = interval(self.period);
            self.interval.set_missed_tick_behavior(MissedTickBehavior::Skip);
        }
        Ok(())
    }
    
    async fn shutdown(&mut self) -> anyhow::Result<()> {
        self.paused.store(true, Ordering::Relaxed);
        Ok(())
    }
}
```

### 3. SQLite 数据监控事件源

```rust
pub struct SqliteWatchSource {
    id: String,
    db_path: String,
    table: String,
    last_checksum: u64,
    poll_interval: Duration,
    // 使用 eventfd 或 channel 与 mio 集成
}

#[async_trait::async_trait]
impl EventSource for SqliteWatchSource {
    fn id(&self) -> &str { &self.id }
    fn source_type(&self) -> SourceType { SourceType::Database }
    
    async fn init(&mut self, poll: &mut Poll, token: Token) -> anyhow::Result<()> {
        // 方案 A：应用层触发（推荐）
        // 在 SQLite 写入事务后，手动调用 agent.inject_event()
        
        // 方案 B：轮询 + 校验和
        // 注册 timerfd 到 mio，定时触发 poll
        Ok(())
    }
    
    fn poll_events(&mut self, _events: &mio::event::Event) -> anyhow::Result<Vec<Event>> {
        // 轮询：计算表行数或校验和，对比 last_checksum
        // 如果变化，生成 DataEvent
        Ok(vec![])
    }
    
    async fn reconfigure(&mut self, config: serde_json::Value) -> anyhow::Result<()> {
        if let Some(table) = config.get("table").and_then(|v| v.as_str()) {
            self.table = table.to_string();
        }
        if let Some(interval_ms) = config.get("poll_interval_ms").and_then(|v| v.as_u64()) {
            self.poll_interval = Duration::from_millis(interval_ms);
        }
        Ok(())
    }
    
    // ... pause/resume/shutdown 略
}
```

### 4. 网络/Socket 事件源

```rust
pub struct SocketEventSource {
    id: String,
    listener: Option<std::net::TcpListener>,
    connections: HashMap<Token, mio::net::TcpStream>,
    // 或直接使用 tokio::net::TcpListener，通过 mio::Waker 桥接
}

#[async_trait::async_trait]
impl EventSource for SocketEventSource {
    fn id(&self) -> &str { &self.id }
    fn source_type(&self) -> SourceType { SourceType::Network }
    
    async fn init(&mut self, poll: &mut Poll, token: Token) -> anyhow::Result<()> {
        let addr = "0.0.0.0:8080".parse()?;
        let listener = mio::net::TcpListener::bind(addr)?;
        poll.registry()
            .register(&mut listener, token, Interest::READABLE)?;
        self.listener = Some(listener);
        Ok(())
    }
    
    fn poll_events(&mut self, events: &mio::event::Event) -> anyhow::Result<Vec<Event>> {
        if events.is_readable() {
            // accept 新连接
            // 为新连接分配 Token，注册到 poll
            // 返回 NetEvent::NewConnection
        }
        Ok(vec![])
    }
    
    async fn reconfigure(&mut self, config: serde_json::Value) -> anyhow::Result<()> {
        // 动态修改监听地址、TLS 配置等
        if let Some(addr) = config.get("bind_addr").and_then(|v| v.as_str()) {
            // 重新绑定
        }
        Ok(())
    }
    
    // ... 略
}
```

## 事件循环（Reactor）核心

```rust
pub struct AgentRuntime {
    poll: Poll,
    sources: HashMap<Token, Box<dyn EventSource>>,
    handlers: Vec<Box<dyn EventHandler>>,
    state_machines: Vec<Box<dyn StateMachine>>,
    event_bus: (mpsc::Sender<Event>, mpsc::Receiver<Event>),
    worker_pool: tokio::runtime::Runtime,
    shutdown: AtomicBool,
}

impl AgentRuntime {
    pub fn new() -> anyhow::Result<Self> {
        let poll = Poll::new()?;
        let worker_pool = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(num_cpus::get())
            .build()?;
        
        Ok(Self {
            poll,
            sources: HashMap::new(),
            handlers: Vec::new(),
            state_machines: Vec::new(),
            event_bus: mpsc::channel(1024),
            worker_pool,
            shutdown: AtomicBool::new(false),
        })
    }
    
    /// 注册事件源（统一接口）
    pub async fn register_source(&mut self, mut source: Box<dyn EventSource>) -> anyhow::Result<()> {
        let token = Token(self.sources.len());
        source.init(&mut self.poll, token).await?;
        self.sources.insert(token, source);
        Ok(())
    }
    
    /// 注册处理器
    pub fn register_handler(&mut self, handler: Box<dyn EventHandler>) {
        self.handlers.push(handler);
    }
    
    /// 主循环
    pub fn run(&mut self) -> anyhow::Result<()> {
        let mut events = Events::with_capacity(1024);
        
        while !self.shutdown.load(Ordering::Relaxed) {
            // 阻塞等待事件（epoll_wait 等价）
            self.poll.poll(&mut events, Some(Duration::from_millis(100)))?;
            
            for mio_event in events.iter() {
                let token = mio_event.token();
                
                if let Some(source) = self.sources.get_mut(&token) {
                    // 1. 从事件源收割事件
                    match source.poll_events(mio_event) {
                        Ok(agent_events) => {
                            for event in agent_events {
                                // 2. 分发到工作线程池
                                let tx = self.event_bus.0.clone();
                                let handlers = self.handlers.clone(); // Arc 优化
                                let mut state_machines = self.state_machines.clone();
                                
                                self.worker_pool.spawn(async move {
                                    // 3. 路由到 Handler 或 StateMachine
                                    Self::dispatch(event, handlers, state_machines, tx).await;
                                });
                            }
                        }
                        Err(e) => {
                            tracing::error!("Source {} error: {}", source.id(), e);
                        }
                    }
                }
            }
        }
        
        Ok(())
    }
    
    async fn dispatch(
        event: Event,
        handlers: Vec<Box<dyn EventHandler>>,
        state_machines: Vec<Box<dyn StateMachine>>,
        event_bus: mpsc::Sender<Event>,
    ) {
        // 先尝试 StateMachine（有状态）
        for sm in &mut state_machines {
            if sm.interested_in(&event) {
                let mut ctx = HandlerContext {
                    source_id: sm.name().to_string(),
                    event_bus: event_bus.clone(),
                    state_store: Arc::new(InMemoryStateStore::new()),
                };
                
                match sm.handle(event.clone(), &mut ctx).await {
                    HandlerResult::HandledWithEvent(new_event) => {
                        let _ = event_bus.send(new_event).await;
                    }
                    _ => {}
                }
                return;
            }
        }
        
        // 再尝试普通 Handler
        for handler in &handlers {
            if handler.interested_in(&event) {
                let mut ctx = HandlerContext {
                    source_id: handler.name().to_string(),
                    event_bus: event_bus.clone(),
                    state_store: Arc::new(InMemoryStateStore::new()),
                };
                let _ = handler.handle(event, &mut ctx).await;
                return;
            }
        }
    }
    
    /// 运行时动态重配置（支持需求 5：变更频率）
    pub async fn reconfigure_source(&mut self, source_id: &str, config: serde_json::Value) -> anyhow::Result<()> {
        for (_, source) in &mut self.sources {
            if source.id() == source_id {
                return source.reconfigure(config).await;
            }
        }
        Err(anyhow::anyhow!("Source {} not found", source_id))
    }
    
    pub async fn shutdown(mut self) {
        self.shutdown.store(true, Ordering::Relaxed);
        for (_, source) in &mut self.sources {
            let _ = source.shutdown().await;
        }
    }
}
```

## 状态机示例（任务流转）

```rust
/// 任务状态机：Pending -> Running -> Completed/Failed
pub struct TaskStateMachine {
    name: String,
    current_state: String,
}

#[async_trait::async_trait]
impl EventHandler for TaskStateMachine {
    fn name(&self) -> &str { &self.name }
    
    fn interested_in(&self, event: &Event) -> bool {
        matches!(event, Event::Data(d) if d.table == "tasks")
    }
    
    async fn handle(&self, event: Event, ctx: &mut HandlerContext) -> HandlerResult {
        // 状态转换逻辑
        match (&self.current_state, &event) {
            ("Pending", Event::Data(d)) if d.operation == DataOp::Update => {
                // 检查是否被分配执行者
                if d.changes.get("assigned_to").is_some() {
                    return HandlerResult::HandledWithEvent(
                        Event::Data(DataEvent {
                            source_id: "state_machine".to_string(),
                            table: "tasks".to_string(),
                            operation: DataOp::Update,
                            row_id: d.row_id.clone(),
                            changes: serde_json::json!({"status": "Running"}),
                        })
                    );
                }
            }
            _ => {}
        }
        HandlerResult::Handled
    }
}

#[async_trait::async_trait]
impl StateMachine for TaskStateMachine {
    fn current_state(&self) -> &str { &self.current_state }
    
    fn transitions(&self) -> &[(String, String, String)] {
        &[
            ("Pending".to_string(), "assigned".to_string(), "Running".to_string()),
            ("Running".to_string(), "completed".to_string(), "Completed".to_string()),
            ("Running".to_string(), "failed".to_string(), "Failed".to_string()),
        ]
    }
    
    async fn on_enter_state(&mut self, state: &str, _ctx: &mut HandlerContext) -> anyhow::Result<()> {
        self.current_state = state.to_string();
        tracing::info!("Task entered state: {}", state);
        Ok(())
    }
    
    async fn on_leave_state(&mut self, _state: &str, _ctx: &mut HandlerContext) -> anyhow::Result<()> {
        Ok(())
    }
}
```

## 使用示例

```rust
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut agent = AgentRuntime::new()?;
    
    // 1. 文件监控
    let fs_source = Box::new(FsEventSource::new(
        "fs-1".to_string(),
        std::path::PathBuf::from("/tmp/watch"),
    ));
    agent.register_source(fs_source).await?;
    
    // 2. 心跳（固定频率）
    let heartbeat = Box::new(TimerEventSource::new(
        "heartbeat-1".to_string(),
        "main".to_string(),
        Duration::from_secs(30),
        true,
    ));
    agent.register_source(heartbeat).await?;
    
    // 3. RSS 轮询（可变更频率）
    let rss_timer = Box::new(TimerEventSource::new(
        "rss-1".to_string(),
        "rss_poll".to_string(),
        Duration::from_secs(300),
        false,
    ));
    agent.register_source(rss_timer).await?;
    
    // 4. SQLite 监控
    let db_source = Box::new(SqliteWatchSource::new(
        "db-1".to_string(),
        "/data/tasks.db".to_string(),
        "tasks".to_string(),
        Duration::from_secs(5),
    ));
    agent.register_source(db_source).await?;
    
    // 注册处理器
    agent.register_handler(Box::new(TaskHandler::new()));
    agent.register_handler(Box::new(NotificationHandler::new()));
    
    // 运行时动态调整 RSS 频率（需求 5）
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(60)).await;
        agent.reconfigure_source("rss-1", serde_json::json!({
            "period_ms": 60000  // 改为 1 分钟
        })).await.unwrap();
    });
    
    // 启动
    agent.run()?;
    
    Ok(())
}
```

## 依赖（Cargo.toml）

```toml
[dependencies]
tokio = { version = "1", features = ["full"] }
mio = { version = "1", features = ["net", "os-poll"] }
notify = "6"  # 文件系统监控
serde = { version = "1", features = ["derive"] }
serde_json = "1"
anyhow = "1"
thiserror = "1"
async-trait = "0.1"
tracing = "0.1"
rusqlite = { version = "0.30", features = ["bundled"] }  # SQLite
```

---

## 设计要点总结

| 需求 | 设计对应 |
|------|---------|
| **统一接口** | `EventSource` trait：所有事件源实现同一套生命周期（init/poll/pause/resume/reconfigure/shutdown）|
| **文件修改** | `FsEventSource` 封装 `notify` crate（inotify/fsevents）|
| **Socket/RSS/聊天** | `SocketEventSource` + `TimerEventSource`（轮询）+ 平台 SDK 桥接 |
| **SQLite/JSON 变更** | `SqliteWatchSource`：支持应用层触发（eventfd）或轮询 |
| **心跳** | `TimerEventSource`：`is_heartbeat = true` |
| **可变频率** | `EventSource::reconfigure()` 动态调整 `timerfd`/`interval` |
| **状态机** | `StateMachine` trait 扩展 `EventHandler`，支持状态转换表和生命周期回调 |

这套框架的核心是**"统一事件抽象 + mio 事件循环 + tokio 工作线程池"**，既保留了 Linux 的 fd 统一哲学，又通过 Rust 的类型系统保证了扩展性。具体事件源可以按需实现，但调用接口完全一致。
