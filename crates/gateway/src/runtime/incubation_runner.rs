// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! Incubation runner — triggered by IdleEvent{kind="incubation"} when the
//! agent reaches idle depth 200+. Runs the deepest idle cycle: creative
//! synthesis, long-term memory trends, and emergent insight generation.
//!
//! Follows the same dependency-injection pattern as [`ExplorationRunner`].

use async_trait::async_trait;
use config::IncubationConfig;
use event_bus::{EventHandler, EventBus};
use idle::IdleKind;
use kernel::event::{Event, EventType};
use kernel::memory::{MemoryProvider, ThinkConfig, ThinkResult};
use kernel::AmanResult;
use std::collections::HashSet;
use std::sync::atomic::Ordering;
use std::sync::{Arc, OnceLock, RwLock};
use std::time::Instant;
use tracing::{debug, info};

use super::agent_registry::AgentRegistry;

pub struct IncubationRunner {
    agent_registry: OnceLock<Arc<AgentRegistry>>,
    memory_provider: OnceLock<Arc<dyn MemoryProvider>>,
    incubation_config: OnceLock<IncubationConfig>,
    global_bus: OnceLock<Arc<dyn EventBus>>,
    active_runs: RwLock<HashSet<String>>,
}

impl IncubationRunner {
    pub fn new() -> Self {
        Self {
            agent_registry: OnceLock::new(),
            memory_provider: OnceLock::new(),
            incubation_config: OnceLock::new(),
            global_bus: OnceLock::new(),
            active_runs: RwLock::new(HashSet::new()),
        }
    }

    pub fn set_agent_registry(&self, registry: Arc<AgentRegistry>) {
        let _ = self.agent_registry.set(registry);
    }

    pub fn set_memory_provider(&self, provider: Arc<dyn MemoryProvider>) {
        let _ = self.memory_provider.set(provider);
    }

    pub fn set_incubation_config(&self, config: IncubationConfig) {
        let _ = self.incubation_config.set(config);
    }

    pub fn set_global_bus(&self, bus: Arc<dyn EventBus>) {
        let _ = self.global_bus.set(bus);
    }

    fn try_acquire(&self, agent_id: &str) -> bool {
        self.active_runs
            .write()
            .unwrap()
            .insert(agent_id.to_owned())
    }

    fn release(&self, agent_id: &str) {
        self.active_runs.write().unwrap().remove(agent_id);
    }

    async fn signal_cooldown(&self, agent_id: &str) {
        let Some(registry) = self.agent_registry.get() else {
            return;
        };
        let Some(coord) = registry.get_idle_coordination(agent_id).await else {
            return;
        };
        let cooldown_secs = self
            .incubation_config
            .get()
            .map(|c| c.cooldown_secs)
            .unwrap_or(10800);
        coord
            .set_kind_cooldown(IdleKind::Incubation, cooldown_secs)
            .await;
        debug!(
            agent_id,
            cooldown_secs,
            "Incubation: cooldown set",
        );
    }

    async fn run_phases(&self, agent_id: &str) -> AmanResult<()> {
        let started = Instant::now();

        let Some(provider) = self.memory_provider.get() else {
            debug!(agent_id, "IncubationRunner: no MemoryProvider");
            return Ok(());
        };

        // Deep creative synthesis: consolidation with lower threshold for broader reach
        let config = ThinkConfig {
            importance_threshold: 0.2,
            run_consolidation: true,
            run_conflict_scan: true,
        };

        let result: ThinkResult = provider.think(agent_id, &config).await?;
        let stored = result.triggers_fired + result.consolidation_count;

        let elapsed = started.elapsed();
        info!(
            agent_id,
            stored,
            duration_ms = elapsed.as_millis(),
            "Incubation: cycle complete",
        );

        // Publish completion event to global bus for UI notification
        if stored > 0 {
            if let Some(bus) = self.global_bus.get() {
                let event = Event::new(
                    format!("idle:incubation:{agent_id}"),
                    EventType::Custom("idle.cycle_completed".to_owned()),
                    serde_json::json!({
                        "kind": "incubation",
                        "agentId": agent_id,
                        "stored": stored,
                        "durationMs": elapsed.as_millis(),
                    }),
                );
                let _ = bus.publish(event).await;
            }
            // Signal idle depth reset — productive work completed, restart idle cycle
            if let Some(registry) = self.agent_registry.get() {
                if let Some(coord) = registry.get_idle_coordination(agent_id).await {
                    coord.pending_depth_reset.store(true, Ordering::SeqCst);
                }
            }
        }

        self.signal_cooldown(agent_id).await;
        Ok(())
    }
}

impl Default for IncubationRunner {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl EventHandler for IncubationRunner {
    async fn handle(&self, event: Event) -> AmanResult<()> {
        if event.event_type != EventType::Idle {
            return Ok(());
        }
        let Some(kind) = event.payload.get("kind").and_then(|v| v.as_str()) else {
            return Ok(());
        };
        if kind != "incubation" {
            return Ok(());
        }
        let Some(agent_id) = event
            .payload
            .get("agent_id")
            .and_then(|v| v.as_str())
        else {
            return Ok(());
        };
        if agent_id.is_empty() {
            return Ok(());
        }

        if !self.try_acquire(agent_id) {
            debug!(agent_id, "IncubationRunner: already running, skipping");
            return Ok(());
        }

        let result = self.run_phases(agent_id).await;
        self.release(agent_id);
        result
    }
}
