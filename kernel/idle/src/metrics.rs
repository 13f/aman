// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! IdleMetrics — 空闲系统指标收集。
//!
//! Architecture ref: idle-design.md §12

/// Comprehensive idle system metrics.
///
/// All 16 fields from the design spec are tracked here.
/// Values are reset when the Agent restarts.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct IdleMetrics {
    // ── Depth & Kind ──────────────────────────────────
    pub idle_depth: u32,
    pub idle_kind: String,

    // ── Timing ────────────────────────────────────────
    pub total_idle_seconds: f64,

    // ── Reflection ────────────────────────────────────
    pub reflections_completed: u64,
    pub reflections_preempted: u64,
    pub reflections_timeout: u64,
    pub reflections_breaker: u64,

    // ── Chat mode ─────────────────────────────────────
    pub chat_mode_active_seconds: f64,
    pub chat_to_full_switches: u64,

    // ── Workflows ─────────────────────────────────────
    pub idle_workflows_cancelled: u64,
    pub explorations_completed: u64,
    pub explorations_quota_exhausted: u64,
    pub meditations_completed: u64,

    // ── Incubation ────────────────────────────────────
    pub incubation_threads_spawned: u64,
    pub incubation_threads_cancelled: u64,

    // ── Reflections ───────────────────────────────────
    pub reflections_false_wakeup: u64,
}

impl IdleMetrics {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a successful reflection.
    pub fn record_reflection_completed(&mut self) {
        self.reflections_completed = self.reflections_completed.saturating_add(1);
    }

    /// Record a preempted reflection.
    pub fn record_reflection_preempted(&mut self) {
        self.reflections_preempted = self.reflections_preempted.saturating_add(1);
    }

    /// Record a reflection timeout.
    pub fn record_reflection_timeout(&mut self) {
        self.reflections_timeout = self.reflections_timeout.saturating_add(1);
    }

    /// Record a circuit-breaker activation.
    pub fn record_breaker_activation(&mut self) {
        self.reflections_breaker = self.reflections_breaker.saturating_add(1);
    }

    /// Update idle depth and kind.
    pub fn set_idle_state(&mut self, depth: u32, kind: &str) {
        self.idle_depth = depth;
        self.idle_kind = kind.to_owned();
    }

    /// Record idle time.
    pub fn add_idle_seconds(&mut self, secs: f64) {
        self.total_idle_seconds += secs;
    }

    /// Record a chat→full mode switch.
    pub fn record_chat_to_full_switch(&mut self) {
        self.chat_to_full_switches = self.chat_to_full_switches.saturating_add(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_metrics_are_zero() {
        let m = IdleMetrics::new();
        assert_eq!(m.reflections_completed, 0);
        assert_eq!(m.idle_depth, 0);
        assert_eq!(m.total_idle_seconds, 0.0);
    }

    #[test]
    fn record_reflection_completed_increments() {
        let mut m = IdleMetrics::new();
        m.record_reflection_completed();
        assert_eq!(m.reflections_completed, 1);
        m.record_reflection_completed();
        assert_eq!(m.reflections_completed, 2);
    }

    #[test]
    fn set_idle_state_updates_depth_and_kind() {
        let mut m = IdleMetrics::new();
        m.set_idle_state(3, "sleep");
        assert_eq!(m.idle_depth, 3);
        assert_eq!(m.idle_kind, "sleep");
    }

    #[test]
    fn record_chat_to_full_switch() {
        let mut m = IdleMetrics::new();
        m.record_chat_to_full_switch();
        assert_eq!(m.chat_to_full_switches, 1);
    }

    #[test]
    fn add_idle_seconds_accumulates() {
        let mut m = IdleMetrics::new();
        m.add_idle_seconds(5.5);
        m.add_idle_seconds(2.0);
        assert!((m.total_idle_seconds - 7.5).abs() < f64::EPSILON);
    }
}
