// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

#![forbid(unsafe_code)]
#![doc = "Telegram messaging channel integration for aman."]

pub mod config;
pub mod sender;
pub mod source;

use async_trait::async_trait;
use config::TelegramConfig;
use kernel::context::PluginContext;
use kernel::plugin::{Plugin, PluginDependency};
use kernel::source::EventSource;
use kernel::AmanResult;
use messaging_core::registry::ChannelRegistry;
use messaging_core::router::StickyAgentRouter;
use messaging_core::session::ChatSessionStore;
use semver::Version;
use sender::TelegramSender;
use source::TelegramSource;
use std::sync::Arc;

/// Plugin that registers a Telegram bot as an event source.
pub struct TelegramPlugin {
    version: Version,
    config: Option<TelegramConfig>,
    channel_registry: Option<Arc<ChannelRegistry>>,
    sticky_router: Option<Arc<StickyAgentRouter>>,
    chat_session_store: Option<Arc<ChatSessionStore>>,
}

impl TelegramPlugin {
    #[must_use]
    pub fn new(config: TelegramConfig) -> Self {
        Self {
            version: Version::new(0, 1, 0),
            config: Some(config),
            channel_registry: None,
            sticky_router: None,
            chat_session_store: None,
        }
    }

    /// Attach shared registries so the plugin can wire its source at load time.
    #[must_use]
    pub fn with_registries(
        mut self,
        channel_registry: Arc<ChannelRegistry>,
        sticky_router: Arc<StickyAgentRouter>,
        chat_session_store: Arc<ChatSessionStore>,
    ) -> Self {
        self.channel_registry = Some(channel_registry);
        self.sticky_router = Some(sticky_router);
        self.chat_session_store = Some(chat_session_store);
        self
    }
}

#[async_trait]
impl Plugin for TelegramPlugin {
    fn name(&self) -> &str {
        "messaging-telegram"
    }

    fn version(&self) -> &Version {
        &self.version
    }

    fn dependencies(&self) -> &[PluginDependency] {
        &[]
    }

    async fn on_load(&mut self, _ctx: PluginContext) -> AmanResult<()> {
        Ok(())
    }

    async fn on_unload(&mut self) -> AmanResult<()> {
        Ok(())
    }

    async fn on_dependency_unloading(&self, _dep_name: &str) -> AmanResult<()> {
        Ok(())
    }

    fn skills(&self) -> Vec<Arc<dyn kernel::skill::Skill>> {
        vec![]
    }

    fn event_sources(&self) -> Vec<Arc<dyn EventSource>> {
        let Some(config) = &self.config else {
            return vec![];
        };

        if !config.enabled || config.bot_token.is_empty() {
            return vec![];
        }

        let source_id = if config.bot_username.is_empty() {
            format!("chat:telegram:{}", &config.bot_token[..8.min(config.bot_token.len())])
        } else {
            format!("chat:telegram:{}", config.bot_username)
        };

        let sender = Arc::new(TelegramSender::new(&config.bot_token));

        // Register the sender with the channel registry so ChatReplyHandler
        // can look it up when an agent reply is ready.
        if let Some(registry) = &self.channel_registry {
            registry.register(source_id.clone(), sender);
        }

        let source = TelegramSource::new(
            source_id,
            &config.bot_token,
            config.allowed_chat_ids.clone(),
        );

        // Wire up shared registries.
        let source = if let (Some(router), Some(store)) =
            (&self.sticky_router, &self.chat_session_store)
        {
            source.with_registries(Arc::clone(router), Arc::clone(store))
        } else {
            source
        };

        vec![Arc::new(source)]
    }

    fn tools(&self) -> Vec<Arc<dyn kernel::tool::Tool>> {
        vec![]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use messaging_core::registry::ChannelRegistry;
    use messaging_core::router::StickyAgentRouter;
    use messaging_core::session::ChatSessionStore;
    use std::sync::Arc;

    fn registries() -> (Arc<ChannelRegistry>, Arc<StickyAgentRouter>, Arc<ChatSessionStore>) {
        (
            Arc::new(ChannelRegistry::new()),
            Arc::new(StickyAgentRouter::new(vec!["cortana".to_owned()])),
            Arc::new(ChatSessionStore::new()),
        )
    }

    #[test]
    fn event_sources_disabled_returns_empty() {
        let (registry, router, store) = registries();
        let mut config = TelegramConfig::default();
        config.enabled = false;
        config.bot_token = "secret".to_owned();

        let plugin = TelegramPlugin::new(config)
            .with_registries(registry, router, store);

        assert!(plugin.event_sources().is_empty());
    }

    #[test]
    fn event_sources_no_token_returns_empty() {
        let (registry, router, store) = registries();
        let mut config = TelegramConfig::default();
        config.enabled = true;
        config.bot_token = String::new();

        let plugin = TelegramPlugin::new(config)
            .with_registries(registry, router, store);

        assert!(plugin.event_sources().is_empty());
    }

    #[test]
    fn event_sources_enabled_registers_sender_and_returns_source() {
        let (registry, router, store) = registries();
        let mut config = TelegramConfig::default();
        config.enabled = true;
        config.bot_token = "test-token-123".to_owned();
        config.bot_username = "testbot".to_owned();

        let plugin = TelegramPlugin::new(config).with_registries(
            Arc::clone(&registry),
            Arc::clone(&router),
            Arc::clone(&store),
        );

        let sources = plugin.event_sources();
        assert_eq!(sources.len(), 1);
        assert_eq!(sources[0].id(), "chat:telegram:testbot");
        assert_eq!(registry.len(), 1);
        assert!(registry.get("chat:telegram:testbot").is_some());
    }

    #[test]
    fn event_sources_uses_token_prefix_when_username_missing() {
        let (registry, router, store) = registries();
        let mut config = TelegramConfig::default();
        config.enabled = true;
        config.bot_token = "abcdefgh1234".to_owned();

        let plugin = TelegramPlugin::new(config).with_registries(
            Arc::clone(&registry),
            Arc::clone(&router),
            Arc::clone(&store),
        );

        let sources = plugin.event_sources();
        assert_eq!(sources.len(), 1);
        assert_eq!(sources[0].id(), "chat:telegram:abcdefgh");
    }
}
