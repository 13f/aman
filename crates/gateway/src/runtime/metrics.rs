use prometheus::{
    Encoder, Gauge, HistogramOpts, HistogramVec, IntCounter, IntCounterVec, IntGauge, IntGaugeVec,
    Opts, Registry, TextEncoder,
};

/// Holds prometheus metric descriptors for the runtime.
///
/// Created once at `AgentRuntime` construction time. Before each
/// `/metrics` scrape the handler calls `update_from(...)` to populate
/// the current values from live runtime state, then encodes via
/// `TextEncoder`.
pub struct MetricsRegistry {
    registry: Registry,
    queue_depth: IntGaugeVec,
    throughput: IntCounter,
    discarded: IntCounter,
    duplicate: IntCounter,
    subscription_count: IntGauge,
    retry_queue_depth: IntGauge,
    dlq_depth: IntGauge,
    inflight_pipelines: IntGauge,
    inflight_skills: IntGauge,
    backpressure: Gauge,
    plugin_health: IntGaugeVec,
    // --- LLM Chat metrics (§14.2) ---
    llm_requests_total: IntCounterVec,
    session_active_count: IntGauge,
    session_state_transitions_total: IntCounterVec,
    queue_message_enqueued_total: IntCounterVec,
    queue_message_dropped_total: IntCounterVec,
    // --- IPC metrics (RED model) ---
    ipc_commands_total: IntCounterVec,
    ipc_command_duration_ms: HistogramVec,
}

impl MetricsRegistry {
    #[must_use]
    pub fn new() -> Self {
        let registry = Registry::new();

        let queue_depth = IntGaugeVec::new(
            Opts::new("event_bus_queue_depth", "Current event bus queue depth by priority"),
            &["priority"],
        )
        .expect("metric");
        registry.register(Box::new(queue_depth.clone())).expect("register");

        let throughput = IntCounter::new("event_throughput_total", "Total events published through the bus")
            .expect("metric");
        registry.register(Box::new(throughput.clone())).expect("register");

        let discarded = IntCounter::new(
            "events_discarded_total",
            "Total events discarded due to backpressure or dedup",
        )
        .expect("metric");
        registry.register(Box::new(discarded.clone())).expect("register");

        let duplicate = IntCounter::new(
            "events_duplicate_total",
            "Total duplicate events detected and dropped",
        )
        .expect("metric");
        registry.register(Box::new(duplicate.clone())).expect("register");

        let subscription_count =
            IntGauge::new("subscription_count", "Number of active event bus subscriptions").expect("metric");
        registry.register(Box::new(subscription_count.clone())).expect("register");

        let retry_queue_depth =
            IntGauge::new("retry_queue_depth", "Current number of events in the retry queue").expect("metric");
        registry.register(Box::new(retry_queue_depth.clone())).expect("register");

        let dlq_depth = IntGauge::new("dlq_depth", "Current number of events in the dead letter queue")
            .expect("metric");
        registry.register(Box::new(dlq_depth.clone())).expect("register");

        let inflight_pipelines = IntGauge::new(
            "inflight_pipelines",
            "Number of pipeline executions currently in flight",
        )
        .expect("metric");
        registry.register(Box::new(inflight_pipelines.clone())).expect("register");

        let inflight_skills = IntGauge::new(
            "inflight_skills",
            "Number of skill executions currently in flight",
        )
        .expect("metric");
        registry.register(Box::new(inflight_skills.clone())).expect("register");

        let backpressure = Gauge::new("backpressure_level", "Current backpressure level (0-1 normalized)")
            .expect("metric");
        registry.register(Box::new(backpressure.clone())).expect("register");

        let plugin_health = IntGaugeVec::new(
            Opts::new("plugin_health", "Health status of loaded plugins"),
            &["plugin", "status"],
        )
        .expect("metric");
        registry.register(Box::new(plugin_health.clone())).expect("register");

        // --- LLM Chat metrics ---
        let llm_requests_total = IntCounterVec::new(
            Opts::new("llm_requests_total", "Total LLM API requests"),
            &["provider", "model", "result"],
        )
        .expect("metric");
        registry.register(Box::new(llm_requests_total.clone())).expect("register");

        let session_active_count = IntGauge::new(
            "session_active_count", "Current number of active chat sessions",
        )
        .expect("metric");
        registry.register(Box::new(session_active_count.clone())).expect("register");

        let session_state_transitions_total = IntCounterVec::new(
            Opts::new("session_state_transitions_total", "Session state machine transitions"),
            &["from_state", "to_state"],
        )
        .expect("metric");
        registry.register(Box::new(session_state_transitions_total.clone())).expect("register");

        let queue_message_enqueued_total = IntCounterVec::new(
            Opts::new("queue_message_enqueued_total", "Messages enqueued per session"),
            &["session_id"],
        )
        .expect("metric");
        registry.register(Box::new(queue_message_enqueued_total.clone())).expect("register");

        let queue_message_dropped_total = IntCounterVec::new(
            Opts::new("queue_message_dropped_total", "Messages dropped per session"),
            &["session_id", "reason"],
        )
        .expect("metric");
        registry.register(Box::new(queue_message_dropped_total.clone())).expect("register");

        // --- IPC metrics (RED model) ---
        let ipc_commands_total = IntCounterVec::new(
            Opts::new("ipc_commands_total", "IPC command invocations"),
            &["command", "result"],
        )
        .expect("metric");
        registry.register(Box::new(ipc_commands_total.clone())).expect("register");

        let ipc_command_duration_ms = HistogramVec::new(
            HistogramOpts::new("ipc_command_duration_ms", "IPC command latency distribution")
                .buckets(vec![1.0, 5.0, 10.0, 25.0, 50.0, 100.0, 250.0, 500.0, 1000.0]),
            &["command"],
        )
        .expect("metric");
        registry.register(Box::new(ipc_command_duration_ms.clone())).expect("register");

        Self {
            registry,
            queue_depth,
            throughput,
            discarded,
            duplicate,
            subscription_count,
            retry_queue_depth,
            dlq_depth,
            inflight_pipelines,
            inflight_skills,
            backpressure,
            plugin_health,
            llm_requests_total,
            session_active_count,
            session_state_transitions_total,
            queue_message_enqueued_total,
            queue_message_dropped_total,
            ipc_commands_total,
            ipc_command_duration_ms,
        }
    }

    /// Increment the LLM request counter.
    pub fn inc_llm_request(&self, provider: &str, model: &str, result: &str) {
        self.llm_requests_total
            .with_label_values(&[provider, model, result])
            .inc();
    }

    /// Set the number of active chat sessions.
    pub fn set_session_active_count(&self, count: usize) {
        self.session_active_count.set(count as i64);
    }

    /// Record a session state machine transition.
    pub fn inc_session_transition(&self, from: &str, to: &str) {
        self.session_state_transitions_total
            .with_label_values(&[from, to])
            .inc();
    }

    /// Increment the per-session message enqueued counter.
    pub fn inc_message_enqueued(&self, session_id: &str) {
        self.queue_message_enqueued_total
            .with_label_values(&[session_id])
            .inc();
    }

    /// Increment the per-session message dropped counter.
    pub fn inc_message_dropped(&self, session_id: &str, reason: &str) {
        self.queue_message_dropped_total
            .with_label_values(&[session_id, reason])
            .inc();
    }

    /// Record an IPC command invocation with result status.
    pub fn inc_ipc_command(&self, command: &str, result: &str) {
        self.ipc_commands_total
            .with_label_values(&[command, result])
            .inc();
    }

    /// Observe an IPC command duration in milliseconds.
    pub fn observe_ipc_duration(&self, command: &str, dur_ms: f64) {
        self.ipc_command_duration_ms
            .with_label_values(&[command])
            .observe(dur_ms);
    }

    /// Update all metrics from the current runtime state.
    ///
    /// Must be called before each `/metrics` scrape to ensure values
    /// reflect the latest live data.
    pub fn update_from(
        &self,
        bus: event_bus::BusMetrics,
        dlq_depth_val: usize,
        inflight_pipelines_val: usize,
        inflight_skills_val: usize,
        plugin_states: &[(String, String)], // (plugin_name, status_string)
        session_active_count: usize,
    ) {
        self.queue_depth
            .with_label_values(&["high"])
            .set(bus.queue_depth.high as i64);
        self.queue_depth
            .with_label_values(&["normal"])
            .set(bus.queue_depth.normal as i64);
        self.queue_depth
            .with_label_values(&["low"])
            .set(bus.queue_depth.low as i64);
        self.throughput.reset();
        self.throughput.inc_by(bus.throughput);
        self.discarded.reset();
        self.discarded.inc_by(bus.discarded_count);
        self.duplicate.reset();
        self.duplicate.inc_by(bus.duplicate_count);
        self.subscription_count.set(bus.subscription_count as i64);
        self.retry_queue_depth.set(bus.retry_queue_depth as i64);
        self.dlq_depth.set(dlq_depth_val as i64);
        self.inflight_pipelines.set(inflight_pipelines_val as i64);
        self.inflight_skills.set(inflight_skills_val as i64);
        self.session_active_count.set(session_active_count as i64);

        let level = normalize_backpressure(&bus.backpressure_level);
        self.backpressure.set(level);

        // Rebuild plugin health labels
        self.plugin_health.reset();
        for (name, status) in plugin_states {
            self.plugin_health
                .with_label_values(&[name, status])
                .set(1);
        }
    }

    /// Encode all registered metrics into a Prometheus text-format string.
    #[must_use]
    pub fn encode(&self) -> String {
        let encoder = TextEncoder::new();
        let metric_families = self.registry.gather();
        let mut buffer = Vec::new();
        encoder.encode(&metric_families, &mut buffer).expect("encode");
        String::from_utf8(buffer).unwrap_or_default()
    }
}

impl Default for MetricsRegistry {
    fn default() -> Self {
        Self::new()
    }
}

fn normalize_backpressure(level: &kernel::types::BackpressureLevel) -> f64 {
    match level {
        kernel::types::BackpressureLevel::Normal => 0.0,
        kernel::types::BackpressureLevel::L1 => 0.2,
        kernel::types::BackpressureLevel::L2 => 0.4,
        kernel::types::BackpressureLevel::L3 => 0.6,
        kernel::types::BackpressureLevel::L4A => 0.8,
        kernel::types::BackpressureLevel::L4B => 0.9,
        kernel::types::BackpressureLevel::Critical => 1.0,
    }
}
