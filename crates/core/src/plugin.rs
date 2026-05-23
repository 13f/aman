// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

use crate::context::PluginContext;
use crate::error::AmanResult;
use crate::hook::Hook;
use crate::skill::Skill;
use crate::source::EventSource;
use crate::tool::Tool;
use async_trait::async_trait;
use semver::Version;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginDependency {
    pub name: String,
    pub version_range: String,
}

#[async_trait]
pub trait Plugin: Send + Sync {
    fn name(&self) -> &str;
    fn version(&self) -> &Version;
    fn dependencies(&self) -> &[PluginDependency];

    async fn on_load(&mut self, ctx: PluginContext) -> AmanResult<()>;
    async fn on_unload(&mut self) -> AmanResult<()>;
    async fn on_dependency_unloading(&self, dep_name: &str) -> AmanResult<()>;

    fn event_sources(&self) -> Vec<Arc<dyn EventSource>>;
    fn skills(&self) -> Vec<Arc<dyn Skill>>;
    fn tools(&self) -> Vec<Arc<dyn Tool>>;
    fn hooks(&self) -> Vec<Arc<dyn Hook>> {
        vec![]
    }
}
