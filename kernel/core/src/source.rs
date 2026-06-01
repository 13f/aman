// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

use crate::context::SourceContext;
use crate::error::AmanResult;
use crate::event::Event;
use crate::types::{BackpressureLevel, HealthStatus, SourceType};
use async_trait::async_trait;
use serde_json::Value;

#[async_trait]
pub trait EventSource: Send + Sync {
    fn id(&self) -> &str;
    fn source_type(&self) -> SourceType;

    async fn init(&mut self, ctx: SourceContext) -> AmanResult<()>;
    async fn shutdown(&mut self) -> AmanResult<()>;

    async fn poll(&mut self, _ctx: &SourceContext) -> AmanResult<Vec<Event>> {
        Ok(Vec::new())
    }

    async fn on_backpressure(
        &mut self,
        _level: BackpressureLevel,
        _ctx: &SourceContext,
    ) -> AmanResult<()> {
        Ok(())
    }

    fn health(&self) -> HealthStatus;

    async fn pause(&mut self) -> AmanResult<()> {
        Ok(())
    }

    async fn resume(&mut self) -> AmanResult<()> {
        Ok(())
    }

    async fn reconfigure(&mut self, _config: Value) -> AmanResult<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::EventSource;
    use crate::context::{BaseContext, SourceContext};
    use crate::event::{Event, EventType};
    use crate::types::{HealthStatus, SourceType, TraceId};
    use pollster::block_on;
    use serde_json::json;

    struct DummySource {
        health: HealthStatus,
    }

    #[async_trait::async_trait]
    impl EventSource for DummySource {
        fn id(&self) -> &str {
            "dummy-source"
        }

        fn source_type(&self) -> SourceType {
            SourceType::Custom
        }

        async fn init(&mut self, _ctx: SourceContext) -> crate::error::AmanResult<()> {
            Ok(())
        }

        async fn shutdown(&mut self) -> crate::error::AmanResult<()> {
            Ok(())
        }

        fn health(&self) -> HealthStatus {
            self.health
        }
    }

    fn context() -> SourceContext {
        SourceContext {
            base: BaseContext::new(TraceId::new()),
            source_name: Some("dummy-source".to_owned()),
        }
    }

    #[test]
    fn source_default_async_methods_work() {
        let mut source = DummySource {
            health: HealthStatus::Ok,
        };
        let ctx = context();

        block_on(source.init(ctx.clone())).expect("init succeeds");
        let events = block_on(source.poll(&ctx)).expect("poll succeeds");
        block_on(source.on_backpressure(crate::types::BackpressureLevel::L1, &ctx))
            .expect("backpressure succeeds");
        block_on(source.pause()).expect("pause succeeds");
        block_on(source.resume()).expect("resume succeeds");
        block_on(source.reconfigure(json!({"interval_ms": 1000}))).expect("reconfigure succeeds");
        block_on(source.shutdown()).expect("shutdown succeeds");

        assert!(events.is_empty());
        assert_eq!(source.id(), "dummy-source");
        assert_eq!(source.source_type(), SourceType::Custom);
        assert_eq!(source.health(), HealthStatus::Ok);
    }

    #[test]
    fn source_trait_can_return_events() {
        struct EventfulSource;

        #[async_trait::async_trait]
        impl EventSource for EventfulSource {
            fn id(&self) -> &str {
                "eventful"
            }

            fn source_type(&self) -> SourceType {
                SourceType::Timer
            }

            async fn init(&mut self, _ctx: SourceContext) -> crate::error::AmanResult<()> {
                Ok(())
            }

            async fn shutdown(&mut self) -> crate::error::AmanResult<()> {
                Ok(())
            }

            async fn poll(&mut self, _ctx: &SourceContext) -> crate::error::AmanResult<Vec<Event>> {
                Ok(vec![Event::new(
                    "timer:test",
                    EventType::TimerTick,
                    json!({}),
                )])
            }

            fn health(&self) -> HealthStatus {
                HealthStatus::Ok
            }
        }

        let mut source = EventfulSource;
        let events = block_on(source.poll(&context())).expect("poll succeeds");
        assert_eq!(events.len(), 1);
    }
}
