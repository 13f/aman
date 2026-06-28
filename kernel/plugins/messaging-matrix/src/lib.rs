// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

#![forbid(unsafe_code)]
#![doc = "Matrix messaging channel integration for aman."]

pub mod config;
pub mod sender;
pub mod source;

use async_trait::async_trait;
use config::MatrixConfig;
use kernel::context::PluginContext;
use kernel::plugin::{Plugin, PluginDependency};
use kernel::source::EventSource;
use kernel::AmanResult;
use messaging_core::registry::ChannelRegistry;
use messaging_core::router::StickyAgentRouter;
use messaging_core::session::ChatSessionStore;
use semver::Version;
use sender::MatrixSender;
use source::MatrixSource;
use std::sync::Arc;

pub struct MatrixPlugin {
    version: Version,
    config: Option<MatrixConfig>,
    channel_registry: Option<Arc<ChannelRegistry>>,
    sticky_router: Option<Arc<StickyAgentRouter>>,
    chat_session_store: Option<Arc<ChatSessionStore>>,
}

impl MatrixPlugin {
    #[must_use]
    pub fn new(config: MatrixConfig) -> Self {
        Self {
            version: Version::new(0, 1, 0),
            config: Some(config),
            channel_registry: None,
            sticky_router: None,
            chat_session_store: None,
        }
    }

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
impl Plugin for MatrixPlugin {
    fn name(&self) -> &str {
        "messaging-matrix"
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

        if !config.enabled || config.homeserver_url.is_empty() {
            return vec![];
        }

        let source_id = format!("chat:matrix:{}", &config.username);

        // Register sender.
        let sender = Arc::new(MatrixSender::new(
            &config.homeserver_url,
            &config.password, // access token
        ));
        if let Some(registry) = &self.channel_registry {
            registry.register(source_id.clone(), sender);
        }

        let source = MatrixSource::new(
            source_id,
            &config.homeserver_url,
            &config.username,
            &config.password,
            &config.device_name,
        );

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
        let config = MatrixConfig {
            enabled: false,
            username: "@test:matrix.org".to_owned(),
            ..Default::default()
        };

        let plugin = MatrixPlugin::new(config).with_registries(registry, router, store);

        assert!(plugin.event_sources().is_empty());
    }

    #[test]
    fn event_sources_no_homeserver_returns_empty() {
        let (registry, router, store) = registries();
        let config = MatrixConfig {
            enabled: true,
            homeserver_url: String::new(),
            username: "@test:matrix.org".to_owned(),
            ..Default::default()
        };

        let plugin = MatrixPlugin::new(config).with_registries(registry, router, store);

        assert!(plugin.event_sources().is_empty());
    }

    #[test]
    fn event_sources_enabled_registers_sender_and_returns_source() {
        let (registry, router, store) = registries();
        let config = MatrixConfig {
            enabled: true,
            homeserver_url: "https://matrix.org".to_owned(),
            username: "@test:matrix.org".to_owned(),
            password: "s3cret".to_owned(),
            ..Default::default()
        };

        let plugin = MatrixPlugin::new(config).with_registries(
            Arc::clone(&registry),
            Arc::clone(&router),
            Arc::clone(&store),
        );

        let sources = plugin.event_sources();
        assert_eq!(sources.len(), 1);
        assert_eq!(sources[0].id(), "chat:matrix:@test:matrix.org");
        assert_eq!(registry.len(), 1);
        assert!(registry.get("chat:matrix:@test:matrix.org").is_some());
    }
}
