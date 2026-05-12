use config::{AgentConfig, BusMode};
use kernel::context::PluginContext;
use kernel::event::{Event, EventType};
use kernel::plugin::{Plugin, PluginDependency};
use kernel::AmanResult;
use plugin::{PluginCandidate, PluginExports, PluginIsolationMode, PluginLifecycleConfig, PluginManifest};
use semver::Version;
use serde_json::json;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use runtime::AgentRuntimeBuilder;

struct DummyPlugin {
    name: String,
    version: Version,
    unload_calls: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl Plugin for DummyPlugin {
    fn name(&self) -> &str {
        &self.name
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
        self.unload_calls.fetch_add(1, Ordering::AcqRel);
        Ok(())
    }

    async fn on_dependency_unloading(&self, _dep_name: &str) -> AmanResult<()> {
        Ok(())
    }

    fn event_sources(&self) -> Vec<Arc<dyn kernel::source::EventSource>> {
        Vec::new()
    }

    fn skills(&self) -> Vec<Arc<dyn kernel::skill::Skill>> {
        Vec::new()
    }

    fn tools(&self) -> Vec<Arc<dyn kernel::tool::Tool>> {
        Vec::new()
    }
}

fn temp_runtime_dir(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!("aman-test-{name}-{}", uuid::Uuid::now_v7()))
}

#[tokio::test]
async fn shutdown_unloads_loaded_plugins() {
    let unload_calls = Arc::new(AtomicUsize::new(0));
    let plugin_name = "dummy-plugin".to_owned();
    let version = Version::new(1, 0, 0);

    let config = AgentConfig::default();
    let runtime = AgentRuntimeBuilder::new(config)
        .with_runtime_dir(temp_runtime_dir("unload"))
        .build()
        .expect("build runtime");

    let candidate = PluginCandidate {
        manifest: PluginManifest {
            name: plugin_name.clone(),
            version: version.clone(),
            depends_on: vec![],
            lifecycle: PluginLifecycleConfig::default(),
            exports: PluginExports::default(),
            config_schema: None,
            isolation: None,
            subprocess: None,
            wasm_path: None,
        },
        plugin: Box::new(DummyPlugin {
            name: plugin_name.clone(),
            version,
            unload_calls: Arc::clone(&unload_calls),
        }),
        isolation: PluginIsolationMode::InProcess,
        subprocess: None,
        wasm_module_bytes: None,
    };

    {
        let mut loader = runtime.plugin_loader().await;
        let _ = loader
            .load_all(vec![candidate])
            .await
            .expect("load plugin");
    }

    runtime.shutdown().await.expect("shutdown");
    assert_eq!(unload_calls.load(Ordering::Acquire), 1);
}

#[tokio::test]
async fn shutdown_writes_wal_final_checkpoint_when_persistent() {
    let mut config = AgentConfig::default();
    config.event_bus.mode = BusMode::Persistent;
    let dir = temp_runtime_dir("wal");
    let runtime = AgentRuntimeBuilder::new(config)
        .with_runtime_dir(dir.clone())
        .build()
        .expect("build runtime");

    let _ = runtime
        .publish_event(Event::new("timer:test", EventType::TimerTick, json!({"ok": true})))
        .await
        .expect("publish");

    runtime.shutdown().await.expect("shutdown");

    let checkpoint_path = dir.join("wal").join("replay_checkpoint.json");
    let raw = std::fs::read_to_string(&checkpoint_path).expect("checkpoint exists");
    let parsed: serde_json::Value = serde_json::from_str(&raw).expect("checkpoint json");
    assert_eq!(parsed.get("offset"), Some(&json!(0)));
}

