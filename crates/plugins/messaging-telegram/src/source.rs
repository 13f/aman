// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! [`TelegramSource`] — `EventSource` backed by teloxide long-polling.

use async_trait::async_trait;
use kernel::context::SourceContext;
use kernel::event::{Event, EventType};
use kernel::source::EventSource;
use kernel::types::{BackpressureLevel, HealthStatus, SourceType};
use kernel::AmanResult;
use messaging_core::router::StickyAgentRouter;
use messaging_core::session::ChatSessionStore;
use messaging_core::types::{
    make_session_id, ChatTarget, PlatformKind, AGENT_ID_KEY, CHAT_TARGET_KEY, SESSION_ID_KEY,
};
use serde_json::json;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use teloxide::prelude::*;
use teloxide::types::UpdateKind;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio::time::Duration;

const POLL_TIMEOUT_MS: u64 = 25;
const MAX_BATCH_SIZE: usize = 256;

// ── Shared state between the teloxide handler and the EventSource ──

struct HandlerState {
    source_id: String,
    tx: mpsc::UnboundedSender<Event>,
    paused: Arc<AtomicBool>,
    sticky_router: Arc<StickyAgentRouter>,
    chat_session_store: Arc<ChatSessionStore>,
    allowed_chat_ids: Vec<i64>,
}

// ── Teloxide message handler ──────────────────────────────────────

async fn message_handler(
    bot: Bot,
    msg: Message,
    state: Arc<HandlerState>,
) -> ResponseResult<()> {
    // If paused by backpressure, skip processing.
    if state.paused.load(Ordering::Acquire) {
        return Ok(());
    }

    let Some(user_text) = msg.text().or_else(|| msg.caption()) else {
        return Ok(());
    };

    let chat_id = msg.chat.id.0;
    let chat_id_str = chat_id.to_string();

    // Respect allowed_chat_ids whitelist, if configured.
    if !state.allowed_chat_ids.is_empty()
        && !state.allowed_chat_ids.contains(&chat_id)
    {
        // Reply to unauthorized users so they can share their chat ID
        // with the admin for pairing.
        let reply = format!(
            "👋 Hi! You're not authorized to use this bot yet.\n\
             \n\
             Your chat ID: `{chat_id}`\n\
             \n\
             Please send this ID to the bot administrator to get access."
        );
        let _ = bot
            .send_message(teloxide::types::Recipient::Id(teloxide::types::ChatId(chat_id)), reply)
            .await;
        tracing::info!(
            chat_id = %chat_id,
            "telegram: unauthorized user, sent pairing instructions"
        );
        return Ok(());
    }

    // Resolve target agent via sticky @mention routing.
    let resolution = state
        .sticky_router
        .resolve(PlatformKind::Telegram, &chat_id_str, user_text);

    // If no agent could be resolved, prompt the user to @mention one.
    if !resolution.agent_resolved {
        let reply = format!(
            "👋 Hi! I don't know which agent to route your message to.\n\
             \n\
             Please @mention an agent in your message, for example:\n\
             @minmax Hello!\n\
             @health How are you?\n\
             \n\
             After your first @mention, I'll remember your preference for future messages."
        );
        let _ = bot
            .send_message(teloxide::types::Recipient::Id(teloxide::types::ChatId(chat_id)), reply)
            .await;
        return Ok(());
    }
    let session_id = make_session_id(PlatformKind::Telegram, &chat_id_str);

    // Store session → chat target mapping for reply routing.
    state.chat_session_store.store(
        session_id.clone(),
        ChatTarget {
            platform: PlatformKind::Telegram,
            chat_id: chat_id_str.clone(),
            source_id: state.source_id.clone(),
            thread_id: msg.thread_id.map(|id| id.to_string()),
        },
    );

    // Build the event payload.
    let payload = json!({
        "text": user_text,
        SESSION_ID_KEY: session_id,
        AGENT_ID_KEY: resolution.agent_id,
        "platform": "telegram",
        "source_id": state.source_id,
        "chat_id": chat_id_str,
        "message_id": msg.id.0.to_string(),
        "thread_id": msg.thread_id.map(|id| id.to_string()),
        "user_display_name": msg.from.as_ref().map(|u| u.full_name()),
        CHAT_TARGET_KEY: {
            "platform": "telegram",
            "chat_id": chat_id_str,
            "source_id": state.source_id,
            "thread_id": msg.thread_id.map(|id| id.to_string()),
        }
    });

    let event = Event::new(state.source_id.as_str(), EventType::MessageReceived, payload);

    // Push into the mpsc channel — the poll loop drains it.
    if state.tx.send(event).is_err() {
        tracing::warn!(
            source_id = %state.source_id,
            "telegram: mpsc channel closed, dropping message"
        );
    }

    Ok(())
}

// ── EventSource implementation ────────────────────────────────────

pub struct TelegramSource {
    id: String,
    bot_token: String,
    initialized: bool,
    paused: Arc<AtomicBool>,
    task: Option<JoinHandle<()>>,
    shutdown_tx: Option<oneshot::Sender<()>>,
    rx: mpsc::UnboundedReceiver<Event>,
    tx: mpsc::UnboundedSender<Event>,
    // Shared registries / router (set in init).
    sticky_router: Option<Arc<StickyAgentRouter>>,
    chat_session_store: Option<Arc<ChatSessionStore>>,
    allowed_chat_ids: Vec<i64>,
}

impl TelegramSource {
    #[must_use]
    pub fn new(
        id: impl Into<String>,
        bot_token: impl Into<String>,
        allowed_chat_ids: Vec<i64>,
    ) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        Self {
            id: id.into(),
            bot_token: bot_token.into(),
            initialized: false,
            paused: Arc::new(AtomicBool::new(false)),
            task: None,
            shutdown_tx: None,
            rx,
            tx,
            sticky_router: None,
            chat_session_store: None,
            allowed_chat_ids,
        }
    }

    /// Inject the shared registries (called before init).
    pub fn with_registries(
        mut self,
        sticky_router: Arc<StickyAgentRouter>,
        chat_session_store: Arc<ChatSessionStore>,
    ) -> Self {
        self.sticky_router = Some(sticky_router);
        self.chat_session_store = Some(chat_session_store);
        self
    }
}

#[async_trait]
impl EventSource for TelegramSource {
    fn id(&self) -> &str {
        &self.id
    }

    fn source_type(&self) -> SourceType {
        SourceType::Chat
    }

    async fn init(&mut self, _ctx: SourceContext) -> AmanResult<()> {
        if self.initialized {
            return Ok(());
        }

        let sticky_router = self
            .sticky_router
            .clone()
            .ok_or_else(|| kernel::Error::config_invalid("TelegramSource missing sticky_router"))?;
        let chat_session_store = self
            .chat_session_store
            .clone()
            .ok_or_else(|| {
                kernel::Error::config_invalid("TelegramSource missing chat_session_store")
            })?;

        let handler_state = Arc::new(HandlerState {
            source_id: self.id.clone(),
            tx: self.tx.clone(),
            paused: Arc::clone(&self.paused),
            sticky_router,
            chat_session_store,
            allowed_chat_ids: self.allowed_chat_ids.clone(),
        });

        let bot = crate::sender::build_telegram_bot(&self.bot_token);

        let (shutdown_tx, mut shutdown_rx) = oneshot::channel();
        self.shutdown_tx = Some(shutdown_tx);

        self.task = Some(tokio::spawn(async move {
            // Manual long-polling loop — uses our proxy-aware bot instead of
            // teloxide's Dispatcher (which creates its own reqwest client and
            // doesn't inherit our proxy settings).
            //
            // We call GetUpdates in a loop and process each update through the
            // same message_handler that the Dispatcher would use.
            let mut offset: i32 = 0;
            loop {
                // Check shutdown / pause
                if shutdown_rx.try_recv().is_ok() {
                    break;
                }
                if handler_state.paused.load(Ordering::Acquire) {
                    tokio::time::sleep(Duration::from_secs(2)).await;
                    continue;
                }

                // Fetch updates
                let updates = match bot.get_updates()
                    .offset(offset)
                    .timeout(30)
                    .send()
                    .await
                {
                    Ok(updates) => updates,
                    Err(e) => {
                        tracing::warn!(error = %e, "telegram: getUpdates failed, retrying in 5s");
                        tokio::time::sleep(Duration::from_secs(5)).await;
                        continue;
                    }
                };

                for update in updates {
                    // Track the highest update_id for the next poll.
                    let update_id: i32 = update.id.0.try_into().unwrap_or(i32::MAX);
                    offset = offset.max(update_id + 1);

                    // Process messages only.
                    if let UpdateKind::Message(msg) = update.kind {
                        if let Err(e) = message_handler(
                            bot.clone(),
                            msg,
                            Arc::clone(&handler_state),
                        )
                        .await
                        {
                            tracing::warn!(error = %e, "telegram: message handler error");
                        }
                    }
                }

                // Small delay between polls to avoid hammering the API.
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
        }));

        self.paused.store(false, Ordering::Release);
        self.initialized = true;

        tracing::info!(
            source_id = %self.id,
            "telegram source initialized (long-polling)"
        );

        Ok(())
    }

    async fn shutdown(&mut self) -> AmanResult<()> {
        if let Some(shutdown_tx) = self.shutdown_tx.take() {
            let _ = shutdown_tx.send(());
        }
        if let Some(task) = self.task.take() {
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

        match tokio::time::timeout(Duration::from_millis(POLL_TIMEOUT_MS), self.rx.recv()).await {
            Ok(Some(first)) => {
                let mut events = vec![first];
                while let Ok(event) = self.rx.try_recv() {
                    events.push(event);
                    if events.len() >= MAX_BATCH_SIZE {
                        break;
                    }
                }
                Ok(events)
            }
            Ok(None) | Err(_) => Ok(Vec::new()),
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
}
