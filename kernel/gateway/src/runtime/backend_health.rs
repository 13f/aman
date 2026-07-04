// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! LLM 后端健康监控。
//!
//! 本模块提供按 `base_url` 聚合的 LLM 后端健康状态跟踪。
//! 多个 agent 共享同一个后端时，它们看到的 [`BackendHealth`] 是同一个。
//!
//! # 设计原则
//!
//! - 主推理路径的读写是 wait-free（AtomicU8），不阻塞。
//! - 只在状态翻转时产生事件，中间连续错误静默聚合。
//! - 错误信息经过 `kernel::redactor` 处理，不泄露 API key。

#![forbid(unsafe_code)]

use kernel::redactor::redact_sensitive_data;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, AtomicI64, AtomicU8, Ordering};
use std::sync::Arc;
use tokio::sync::RwLock;

/// 单个 LLM 后端的健康状态。
///
/// 一个"后端"由 `base_url` 归一化后唯一标识。
pub struct BackendHealth {
    /// 当前状态。用 AtomicU8 是因为主推理路径只需要无锁写入。
    status: AtomicU8,
    /// 最后一次 Ok 的毫秒时间戳（Unix epoch）。
    last_ok_ms: AtomicI64,
    /// 最后一次 Err 的毫秒时间戳（Unix epoch）。
    last_failure_ms: AtomicI64,
    /// 连续失败次数。
    consecutive_failures: AtomicU32,
    /// 最近一次错误信息。用 Mutex 是因为 String 不能原子更新；
    /// 且错误信息只在事件发布 / 日志时读取，频率很低。
    last_error: std::sync::Mutex<String>,
    /// 归一化后的 base_url（用于事件发布）。
    base_url: String,
}

/// 后端健康状态枚举。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[repr(u8)]
pub enum BackendStatus {
    /// 未知（初始状态）。
    Unknown = 0,
    /// 正常。
    Ok = 1,
    /// 降级（间歇性失败）。
    Degraded = 2,
    /// 不可用。
    Down = 3,
}

impl BackendStatus {
    /// 从 u8 转换为 BackendStatus。
    pub fn from_u8(v: u8) -> Self {
        match v {
            0 => Self::Unknown,
            1 => Self::Ok,
            2 => Self::Degraded,
            3 => Self::Down,
            // 未知值降级为 Unknown
            _ => Self::Unknown,
        }
    }
}

/// 健康状态变更事件。只在翻转时产生。
#[derive(Debug, Clone, serde::Serialize)]
pub struct BackendHealthChanged {
    pub base_url: String,
    pub from: BackendStatus,
    pub to: BackendStatus,
    pub consecutive_failures: u32,
    pub last_error: String,
}

/// 健康监控配置阈值。
#[derive(Debug, Clone)]
pub struct BackendHealthConfig {
    /// 连续多少次失败进入 Degraded。
    pub degraded_threshold: u32,
    /// 连续多少次失败进入 Down。
    pub down_threshold: u32,
    /// Down 状态后等待多少秒再尝试半探针。
    pub cooldown_secs: u64,
    /// 系统长时间无推理时，兜底探针间隔。
    pub probe_interval_idle_secs: u64,
}

impl Default for BackendHealthConfig {
    fn default() -> Self {
        Self {
            degraded_threshold: 3,
            down_threshold: 6,
            cooldown_secs: 60,
            probe_interval_idle_secs: 3600,
        }
    }
}

impl BackendHealth {
    /// 创建一个新的 BackendHealth，初始状态为 Unknown。
    pub fn new(base_url: String) -> Self {
        Self {
            status: AtomicU8::new(BackendStatus::Unknown as u8),
            last_ok_ms: AtomicI64::new(0),
            last_failure_ms: AtomicI64::new(0),
            consecutive_failures: AtomicU32::new(0),
            last_error: std::sync::Mutex::new(String::new()),
            base_url,
        }
    }

    /// 获取当前状态。
    pub fn status(&self) -> BackendStatus {
        BackendStatus::from_u8(self.status.load(Ordering::Relaxed))
    }

    /// 获取连续失败次数。
    pub fn consecutive_failures(&self) -> u32 {
        self.consecutive_failures.load(Ordering::Relaxed)
    }

    /// 获取最后一次错误信息。
    pub fn last_error(&self) -> String {
        self.last_error
            .lock()
            .expect("last_error lock")
            .clone()
    }

    /// 获取归一化后的 base_url。
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// 记录一次成功调用。
    ///
    /// 如果状态发生翻转，返回 `Some(BackendHealthChanged)`。
    pub fn record_success(&self, _config: &BackendHealthConfig) -> Option<BackendHealthChanged> {
        let now = now_ms();
        self.last_ok_ms.store(now, Ordering::Relaxed);
        self.consecutive_failures.store(0, Ordering::Relaxed);

        let prev = self.status.load(Ordering::Relaxed);
        let prev_status = BackendStatus::from_u8(prev);

        // 任何成功都直接回到 Ok
        self.status
            .store(BackendStatus::Ok as u8, Ordering::Relaxed);

        if prev_status != BackendStatus::Ok {
            Some(BackendHealthChanged {
                base_url: self.base_url.clone(),
                from: prev_status,
                to: BackendStatus::Ok,
                consecutive_failures: 0,
                last_error: String::new(),
            })
        } else {
            None
        }
    }

    /// 记录一次失败调用。
    ///
    /// 错误信息会自动经过 redactor 处理。
    /// 如果状态发生翻转，返回 `Some(BackendHealthChanged)`。
    pub fn record_failure(
        &self,
        error: &str,
        config: &BackendHealthConfig,
    ) -> Option<BackendHealthChanged> {
        let now = now_ms();
        self.last_failure_ms.store(now, Ordering::Relaxed);

        // 经过 redactor 处理错误信息
        let cleaned = redact_sensitive_data(error).into_owned();
        {
            let mut last = self.last_error.lock().expect("last_error lock");
            *last = cleaned.clone();
        }

        let failures = self.consecutive_failures.fetch_add(1, Ordering::Relaxed) + 1;

        let prev = self.status.load(Ordering::Relaxed);
        let prev_status = BackendStatus::from_u8(prev);

        // 根据阈值决定新状态
        let new_status = if failures >= config.down_threshold {
            BackendStatus::Down
        } else if failures >= config.degraded_threshold {
            BackendStatus::Degraded
        } else {
            // 还没到阈值，保持当前状态
            return None;
        };

        if new_status as u8 > prev_status as u8 {
            self.status.store(new_status as u8, Ordering::Relaxed);
            Some(BackendHealthChanged {
                base_url: self.base_url.clone(),
                from: prev_status,
                to: new_status,
                consecutive_failures: failures,
                last_error: cleaned,
            })
        } else {
            None
        }
    }
}

/// 后端健康状态注册表。
///
/// key = 归一化后的 base_url，value = Arc<BackendHealth>。
/// 多个 agent 共享同一个后端时，它们看到的 `Arc<BackendHealth>` 是同一个。
pub struct BackendHealthRegistry {
    /// key = 归一化后的 base_url，value = Arc<BackendHealth>。
    /// 公开以便 LlmHealthProbe 遍历所有后端。
    pub map: RwLock<HashMap<String, Arc<BackendHealth>>>,
    config: BackendHealthConfig,
}

impl BackendHealthRegistry {
    /// 创建一个新的 BackendHealthRegistry。
    pub fn new(config: BackendHealthConfig) -> Self {
        Self {
            map: RwLock::new(HashMap::new()),
            config,
        }
    }

    /// 使用默认配置创建。
    pub fn default() -> Self {
        Self::new(BackendHealthConfig::default())
    }

    /// 获取或插入一个 BackendHealth。
    ///
    /// 如果该 base_url 已经存在，返回已有的；否则创建新的。
    pub async fn get_or_insert(&self, base_url: &str) -> Arc<BackendHealth> {
        let normalized = normalized_url(base_url);

        // 先尝试读
        {
            let map = self.map.read().await;
            if let Some(health) = map.get(&normalized) {
                return Arc::clone(health);
            }
        }

        // 不存在，创建并插入
        let health = Arc::new(BackendHealth::new(normalized.clone()));
        let mut map = self.map.write().await;
        // Double-check after acquiring write lock
        if let Some(existing) = map.get(&normalized) {
            return Arc::clone(existing);
        }
        map.insert(normalized, Arc::clone(&health));
        health
    }

    /// 获取指定 base_url 的 BackendHealth（如果存在）。
    pub async fn get(&self, base_url: &str) -> Option<Arc<BackendHealth>> {
        let normalized = normalized_url(base_url);
        let map = self.map.read().await;
        map.get(&normalized).cloned()
    }

    /// 获取当前配置。
    pub fn config(&self) -> &BackendHealthConfig {
        &self.config
    }
}

/// 归一化 URL：去除 trailing slash，统一 scheme 小写。
pub fn normalized_url(input: &str) -> String {
    normalized_url_internal(input)
}

/// 内部实现（不公开）。
fn normalized_url_internal(input: &str) -> String {
    let mut s = input.trim().to_string();
    while s.ends_with('/') {
        s.pop();
    }
    // 统一 scheme 为小写（http:// → http://, HTTP:// → http://）
    if let Some(pos) = s.find("://") {
        let scheme = &s[..pos];
        let rest = &s[pos..];
        s = format!("{}{}", scheme.to_lowercase(), rest);
    }
    s
}

/// 当前 Unix 时间戳（毫秒）。
fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_record_success_initial_state() {
        let health = BackendHealth::new("https://api.openai.com/v1".to_string());
        let config = BackendHealthConfig::default();

        assert_eq!(health.status(), BackendStatus::Unknown);

        let changed = health.record_success(&config);
        assert!(changed.is_some());
        let changed = changed.unwrap();
        assert_eq!(changed.from, BackendStatus::Unknown);
        assert_eq!(changed.to, BackendStatus::Ok);
        assert_eq!(health.status(), BackendStatus::Ok);
        assert_eq!(health.consecutive_failures(), 0);
    }

    #[test]
    fn test_consecutive_failures_increment() {
        let health = BackendHealth::new("https://api.openai.com/v1".to_string());
        let config = BackendHealthConfig::default();

        // 前 2 次失败不触发状态变化（阈值是 3）
        assert!(health.record_failure("err1", &config).is_none());
        assert!(health.record_failure("err2", &config).is_none());
        assert_eq!(health.consecutive_failures(), 2);
        assert_eq!(health.status(), BackendStatus::Unknown);

        // 第 3 次失败触发 Degraded
        let changed = health.record_failure("err3", &config);
        assert!(changed.is_some());
        let changed = changed.unwrap();
        assert_eq!(changed.to, BackendStatus::Degraded);
        assert_eq!(changed.consecutive_failures, 3);
    }

    #[test]
    fn test_transition_ok_to_degraded_at_threshold() {
        let health = BackendHealth::new("https://api.openai.com/v1".to_string());
        let config = BackendHealthConfig::default();

        // 先成功进入 Ok
        health.record_success(&config);
        assert_eq!(health.status(), BackendStatus::Ok);

        // 连续失败到阈值
        for _ in 0..2 {
            health.record_failure("err", &config);
        }
        assert_eq!(health.status(), BackendStatus::Ok); // 还没到阈值

        let changed = health.record_failure("err", &config);
        assert!(changed.is_some());
        assert_eq!(changed.unwrap().to, BackendStatus::Degraded);
    }

    #[test]
    fn test_transition_degraded_to_down_at_threshold() {
        let health = BackendHealth::new("https://api.openai.com/v1".to_string());
        let config = BackendHealthConfig::default();

        // 直接连续失败 6 次
        for _ in 0..5 {
            health.record_failure("err", &config);
        }
        assert_eq!(health.status(), BackendStatus::Degraded);

        let changed = health.record_failure("err", &config);
        assert!(changed.is_some());
        assert_eq!(changed.unwrap().to, BackendStatus::Down);
    }

    #[test]
    fn test_transition_down_to_ok_on_success() {
        let health = BackendHealth::new("https://api.openai.com/v1".to_string());
        let config = BackendHealthConfig::default();

        // 进入 Down
        for _ in 0..6 {
            health.record_failure("err", &config);
        }
        assert_eq!(health.status(), BackendStatus::Down);

        // 一次成功就回到 Ok
        let changed = health.record_success(&config);
        assert!(changed.is_some());
        let changed = changed.unwrap();
        assert_eq!(changed.from, BackendStatus::Down);
        assert_eq!(changed.to, BackendStatus::Ok);
        assert_eq!(health.consecutive_failures(), 0);
    }

    #[test]
    fn test_normalized_url_strips_trailing_slash() {
        assert_eq!(normalized_url("https://api.openai.com/v1/"), "https://api.openai.com/v1");
        assert_eq!(normalized_url("https://api.openai.com/v1"), "https://api.openai.com/v1");
        assert_eq!(normalized_url("https://api.openai.com/v1///"), "https://api.openai.com/v1");
    }

    #[test]
    fn test_normalized_url_lowercases_scheme() {
        assert_eq!(normalized_url("HTTPS://api.openai.com/v1"), "https://api.openai.com/v1");
        assert_eq!(normalized_url("HTTP://localhost:11434/v1"), "http://localhost:11434/v1");
    }

    #[test]
    fn test_error_message_is_redacted() {
        let health = BackendHealth::new("https://api.openai.com/v1".to_string());
        let config = BackendHealthConfig::default();

        // 足够长的 sk- 密钥（需要 20+ 字符才能匹配 redactor 的模式）
        health.record_failure("request failed: sk-abcdefghijklmnopqrstuvwxyz123456 failed", &config);
        let err = health.last_error();
        // API key should be redacted
        assert!(!err.contains("sk-abcdefghijklmnopqrstuvwxyz123456"), "error was: {err}");
        assert!(err.contains("[REDACTED_API_KEY]"), "error was: {err}");
    }

    #[tokio::test]
    async fn test_registry_get_or_insert() {
        let registry = BackendHealthRegistry::default();

        let h1 = registry.get_or_insert("https://api.openai.com/v1/").await;
        let h2 = registry.get_or_insert("https://api.openai.com/v1").await;

        // 应该返回同一个 Arc
        assert!(Arc::ptr_eq(&h1, &h2));
        assert_eq!(h1.base_url(), "https://api.openai.com/v1");
    }

    #[tokio::test]
    async fn test_registry_get_returns_none_for_missing() {
        let registry = BackendHealthRegistry::default();
        assert!(registry.get("https://nonexistent.com/v1").await.is_none());
    }
}
