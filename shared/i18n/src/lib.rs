#![forbid(unsafe_code)]
#![doc = "Internationalization (i18n) support for the aman agent framework."]
// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Supported locales.
///
/// Serialized as short language codes: `"en"`, `"zhs"`.
/// Default is English (`En`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Locale {
    /// English (default).
    #[default]
    En,
    /// Simplified Chinese (简体中文).
    Zhs,
}

impl Locale {
    /// Returns the ISO 639-1 / custom short code for this locale.
    #[must_use]
    pub fn code(self) -> &'static str {
        match self {
            Self::En => "en",
            Self::Zhs => "zhs",
        }
    }

    /// Returns the human-readable display name in the locale's own language.
    #[must_use]
    pub fn display_name(self) -> &'static str {
        match self {
            Self::En => "English",
            Self::Zhs => "简体中文",
        }
    }

    /// Parse a locale from a short code string (case-insensitive).
    /// Returns `None` for unrecognized codes.
    #[must_use]
    pub fn from_code(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "en" | "english" => Some(Self::En),
            "zhs" | "zh-hans" | "zh_cn" | "zh-cn" | "chinese" | "simplified_chinese" => {
                Some(Self::Zhs)
            }
            _ => None,
        }
    }

    /// All supported locales, in display order.
    #[must_use]
    pub const fn all() -> &'static [Locale] {
        &[Self::En, Self::Zhs]
    }
}

impl std::fmt::Display for Locale {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.code())
    }
}

// ── Translation infrastructure ──────────────────────────────────────

/// A translation key is a dot-separated path, e.g. `"error.config.invalid"`.
pub type TranslationKey = &'static str;

/// Translation bundle: maps keys to translated strings for one locale.
type Bundle = HashMap<TranslationKey, &'static str>;

/// Main translator — holds the active locale and all translation bundles.
///
/// # Usage
///
/// ```ignore
/// use i18n::{Translator, Locale};
///
/// let t = Translator::new(Locale::Zhs);
/// assert_eq!(t.translate("common.ok"), "确定");
/// assert_eq!(t.translate("common.cancel"), "取消");
/// ```
#[derive(Debug, Clone)]
pub struct Translator {
    locale: Locale,
}

impl Translator {
    /// Create a new translator for the given locale.
    #[must_use]
    pub fn new(locale: Locale) -> Self {
        Self { locale }
    }

    /// Return the active locale.
    #[must_use]
    pub fn locale(&self) -> Locale {
        self.locale
    }

    /// Translate a key into the active locale.
    ///
    /// Falls back to English if the key is missing in the active locale.
    /// Returns the key itself if no translation is found at all.
    #[must_use]
    pub fn translate(&self, key: TranslationKey) -> &'static str {
        let bundle = bundle_for(self.locale);
        bundle
            .get(key)
            .copied()
            .or_else(|| {
                // Fall back to English if current locale isn't already English.
                if self.locale != Locale::En {
                    bundle_for(Locale::En).get(key).copied()
                } else {
                    None
                }
            })
            .unwrap_or(key)
    }

    /// Translate a key with placeholder replacement.
    ///
    /// Placeholders are `{name}` patterns in the translated string.
    /// Returns a `String` with all placeholders replaced.
    ///
    /// ```ignore
    /// let t = Translator::new(Locale::En);
    /// let mut args = HashMap::new();
    /// args.insert("count", "5");
    /// assert_eq!(t.translate_with("messages.unread", &args), "You have 5 unread messages");
    /// ```
    #[must_use]
    pub fn translate_with(
        &self,
        key: TranslationKey,
        args: &HashMap<&str, &str>,
    ) -> String {
        let template = self.translate(key);
        let mut result = template.to_owned();
        for (placeholder, value) in args {
            let pattern = format!("{{{placeholder}}}");
            result = result.replace(&pattern, value);
        }
        result
    }

    /// Returns `true` if a translation exists for this key (in any locale).
    #[must_use]
    pub fn has_key(key: TranslationKey) -> bool {
        bundle_for(Locale::En).contains_key(key)
    }
}

impl Default for Translator {
    fn default() -> Self {
        Self::new(Locale::default())
    }
}

// ── Translation bundles ─────────────────────────────────────────────

fn bundle_for(locale: Locale) -> &'static Bundle {
    match locale {
        Locale::En => &EN,
        Locale::Zhs => &ZHS,
    }
}

// ── Translation keys ────────────────────────────────────────────────

// Key constants for compile-time safety.
// Each module block groups keys by domain.

/// Common / general-purpose keys.
pub mod key {
    /// "OK"
    pub const OK: &str = "common.ok";
    /// "Cancel"
    pub const CANCEL: &str = "common.cancel";
    /// "Close"
    pub const CLOSE: &str = "common.close";
    /// "Save"
    pub const SAVE: &str = "common.save";
    /// "Delete"
    pub const DELETE: &str = "common.delete";
    /// "Edit"
    pub const EDIT: &str = "common.edit";
    /// "Search"
    pub const SEARCH: &str = "common.search";
    /// "Loading…"
    pub const LOADING: &str = "common.loading";
    /// "Error"
    pub const ERROR: &str = "common.error";
    /// "Warning"
    pub const WARNING: &str = "common.warning";
    /// "Success"
    pub const SUCCESS: &str = "common.success";
    /// "Yes"
    pub const YES: &str = "common.yes";
    /// "No"
    pub const NO: &str = "common.no";
    /// "Submit"
    pub const SUBMIT: &str = "common.submit";
    /// "Refresh"
    pub const REFRESH: &str = "common.refresh";
    /// "Settings"
    pub const SETTINGS: &str = "common.settings";
    /// "Help"
    pub const HELP: &str = "common.help";
    /// "About"
    pub const ABOUT: &str = "common.about";
    /// "Home"
    pub const HOME: &str = "common.home";
    /// "Back"
    pub const BACK: &str = "common.back";
    /// "Next"
    pub const NEXT: &str = "common.next";
    /// "Previous"
    pub const PREVIOUS: &str = "common.previous";
    /// "Finish"
    pub const FINISH: &str = "common.finish";
    /// "Retry"
    pub const RETRY: &str = "common.retry";
    /// "Skip"
    pub const SKIP: &str = "common.skip";
    /// "Confirm"
    pub const CONFIRM: &str = "common.confirm";
    /// "Are you sure?"
    pub const ARE_YOU_SURE: &str = "common.are_you_sure";
    /// "No results found."
    pub const NO_RESULTS: &str = "common.no_results";
    /// "Copied to clipboard"
    pub const COPIED: &str = "common.copied";
    /// "Enabled"
    pub const ENABLED: &str = "common.enabled";
    /// "Disabled"
    pub const DISABLED: &str = "common.disabled";
    /// "Online"
    pub const ONLINE: &str = "common.online";
    /// "Offline"
    pub const OFFLINE: &str = "common.offline";
    /// "Unknown"
    pub const UNKNOWN: &str = "common.unknown";
    /// "None"
    pub const NONE: &str = "common.none";
    /// "All"
    pub const ALL: &str = "common.all";

    // ── Config ──────────────────────────────────────────────────
    /// "Configuration"
    pub const CONFIG_TITLE: &str = "config.title";
    /// "Invalid configuration: {detail}"
    pub const CONFIG_INVALID: &str = "config.invalid";
    /// "Failed to read configuration file"
    pub const CONFIG_READ_ERROR: &str = "config.read_error";
    /// "Failed to parse configuration"
    pub const CONFIG_PARSE_ERROR: &str = "config.parse_error";

    // ── Gateway ─────────────────────────────────────────────────
    /// "Gateway is starting…"
    pub const GATEWAY_STARTING: &str = "gateway.starting";
    /// "Gateway is ready"
    pub const GATEWAY_READY: &str = "gateway.ready";
    /// "Gateway is shutting down…"
    pub const GATEWAY_STOPPING: &str = "gateway.stopping";
    /// "Gateway shut down"
    pub const GATEWAY_STOPPED: &str = "gateway.stopped";

    // ── Agent ───────────────────────────────────────────────────
    /// "Agent is busy"
    pub const AGENT_BUSY: &str = "agent.busy";
    /// "Agent is idle"
    pub const AGENT_IDLE: &str = "agent.idle";
    /// "Agent is sleeping"
    pub const AGENT_SLEEPING: &str = "agent.sleeping";
    /// "Session started"
    pub const SESSION_STARTED: &str = "agent.session_started";

    // ── Plugin ──────────────────────────────────────────────────
    /// "Plugin loaded successfully"
    pub const PLUGIN_LOADED: &str = "plugin.loaded";
    /// "Plugin failed to load"
    pub const PLUGIN_LOAD_FAILED: &str = "plugin.load_failed";

    // ── Tool ────────────────────────────────────────────────────
    /// "Tool execution started"
    pub const TOOL_STARTED: &str = "tool.started";
    /// "Tool execution completed"
    pub const TOOL_COMPLETED: &str = "tool.completed";
    /// "Tool execution failed"
    pub const TOOL_FAILED: &str = "tool.failed";

    // ── HTTP / API ──────────────────────────────────────────────
    /// "Unauthorized"
    pub const HTTP_UNAUTHORIZED: &str = "http.unauthorized";
    /// "Forbidden"
    pub const HTTP_FORBIDDEN: &str = "http.forbidden";
    /// "Not Found"
    pub const HTTP_NOT_FOUND: &str = "http.not_found";
    /// "Internal Server Error"
    pub const HTTP_INTERNAL_ERROR: &str = "http.internal_error";
    /// "Service Unavailable"
    pub const HTTP_SERVICE_UNAVAILABLE: &str = "http.service_unavailable";
    /// "Rate limit exceeded"
    pub const HTTP_RATE_LIMITED: &str = "http.rate_limited";

    // ── Validation ──────────────────────────────────────────────
    /// "This field is required"
    pub const VALIDATION_REQUIRED: &str = "validation.required";
    /// "Value is too long"
    pub const VALIDATION_TOO_LONG: &str = "validation.too_long";
    /// "Invalid format"
    pub const VALIDATION_INVALID_FORMAT: &str = "validation.invalid_format";
    /// "Value out of range"
    pub const VALIDATION_OUT_OF_RANGE: &str = "validation.out_of_range";

    // ── Lifecycle ───────────────────────────────────────────────
    /// "Starting phase {phase}…"
    pub const LIFECYCLE_PHASE_START: &str = "lifecycle.phase_start";
    /// "Phase {phase} complete"
    pub const LIFECYCLE_PHASE_COMPLETE: &str = "lifecycle.phase_complete";

    // ── TUI ─────────────────────────────────────────────────────
    /// "Logs"
    pub const TUI_LOGS_TITLE: &str = "tui.logs.title";
    /// "Tab to switch"
    pub const TUI_LOGS_SWITCH_HINT: &str = "tui.logs.switch_hint";
    /// "Pending Approvals"
    pub const TUI_APPROVALS_TITLE: &str = "tui.approvals.title";
    /// "No pending approvals."
    pub const TUI_APPROVALS_NO_PENDING: &str = "tui.approvals.no_pending";
    /// "Capabilities for"
    pub const TUI_APPROVALS_CAPABILITIES_FOR: &str = "tui.approvals.capabilities_for";
    /// "Enter=Approve  d=Deny"
    pub const TUI_APPROVALS_APPROVE_DENY_HINT: &str = "tui.approvals.approve_deny_hint";
    /// "Tab"
    pub const TUI_FOOTER_TAB: &str = "tui.footer.tab";
    /// "switch focus"
    pub const TUI_FOOTER_SWITCH_FOCUS: &str = "tui.footer.switch_focus";
    /// "navigate"
    pub const TUI_FOOTER_NAVIGATE: &str = "tui.footer.navigate";
    /// "Enter"
    pub const TUI_FOOTER_ENTER: &str = "tui.footer.enter";
    /// "approve"
    pub const TUI_FOOTER_APPROVE: &str = "tui.footer.approve";
    /// "d"
    pub const TUI_FOOTER_D_KEY: &str = "tui.footer.d_key";
    /// "deny"
    pub const TUI_FOOTER_DENY: &str = "tui.footer.deny";
    /// "scroll"
    pub const TUI_FOOTER_SCROLL: &str = "tui.footer.scroll";
    /// "q"
    pub const TUI_FOOTER_Q_KEY: &str = "tui.footer.q_key";
    /// "quit"
    pub const TUI_FOOTER_QUIT: &str = "tui.footer.quit";
    /// "Approve failed"
    pub const TUI_ERROR_APPROVE_FAILED: &str = "tui.error.approve_failed";
    /// "Deny failed"
    pub const TUI_ERROR_DENY_FAILED: &str = "tui.error.deny_failed";
    /// "No plugin selected"
    pub const TUI_ERROR_NO_PLUGIN_SELECTED: &str = "tui.error.no_plugin_selected";

    // ── Desktop ─────────────────────────────────────────────────
    /// "Reload Skills"
    pub const DESKTOP_MENU_RELOAD_SKILLS: &str = "desktop.menu.reload_skills";
    /// "Quit aman desktop"
    pub const DESKTOP_MENU_QUIT: &str = "desktop.menu.quit";
    /// "File"
    pub const DESKTOP_MENU_FILE: &str = "desktop.menu.file";
    /// "Edit"
    pub const DESKTOP_MENU_EDIT: &str = "desktop.menu.edit";
    /// "Help"
    pub const DESKTOP_MENU_HELP: &str = "desktop.menu.help";
    /// "About aman desktop"
    pub const DESKTOP_MENU_ABOUT: &str = "desktop.menu.about";
    /// "Toggle DevTools"
    pub const DESKTOP_MENU_DEVTOOLS: &str = "desktop.menu.devtools";
    /// "Gateway not connected. Start the gateway daemon first."
    pub const DESKTOP_ERROR_NO_GATEWAY: &str = "desktop.error.no_gateway";
    /// "Already connected to a gateway"
    pub const DESKTOP_ERROR_ALREADY_CONNECTED: &str = "desktop.error.already_connected";
    /// "Gateway not reachable at {url}"
    pub const DESKTOP_ERROR_GATEWAY_UNREACHABLE: &str = "desktop.error.gateway_unreachable";
    /// "Failed to spawn gateway at {path}"
    pub const DESKTOP_ERROR_SPAWN_FAILED: &str = "desktop.error.spawn_failed";
    /// "Gateway started at {url}"
    pub const DESKTOP_INFO_GATEWAY_STARTED: &str = "desktop.info.gateway_started";
    /// "Connected to already-running gateway at {url}"
    pub const DESKTOP_INFO_GATEWAY_CONNECTED: &str = "desktop.info.gateway_connected";
    /// "Gateway startup timed out after {secs}s"
    pub const DESKTOP_ERROR_STARTUP_TIMEOUT: &str = "desktop.error.startup_timeout";
}

// ── English bundle ──────────────────────────────────────────────────

static EN: std::sync::LazyLock<Bundle> = std::sync::LazyLock::new(|| {
    let mut b = Bundle::new();
    // Common
    b.insert("common.ok", "OK");
    b.insert("common.cancel", "Cancel");
    b.insert("common.close", "Close");
    b.insert("common.save", "Save");
    b.insert("common.delete", "Delete");
    b.insert("common.edit", "Edit");
    b.insert("common.search", "Search");
    b.insert("common.loading", "Loading\u{2026}");
    b.insert("common.error", "Error");
    b.insert("common.warning", "Warning");
    b.insert("common.success", "Success");
    b.insert("common.yes", "Yes");
    b.insert("common.no", "No");
    b.insert("common.submit", "Submit");
    b.insert("common.refresh", "Refresh");
    b.insert("common.settings", "Settings");
    b.insert("common.help", "Help");
    b.insert("common.about", "About");
    b.insert("common.home", "Home");
    b.insert("common.back", "Back");
    b.insert("common.next", "Next");
    b.insert("common.previous", "Previous");
    b.insert("common.finish", "Finish");
    b.insert("common.retry", "Retry");
    b.insert("common.skip", "Skip");
    b.insert("common.confirm", "Confirm");
    b.insert("common.are_you_sure", "Are you sure?");
    b.insert("common.no_results", "No results found.");
    b.insert("common.copied", "Copied to clipboard");
    b.insert("common.enabled", "Enabled");
    b.insert("common.disabled", "Disabled");
    b.insert("common.online", "Online");
    b.insert("common.offline", "Offline");
    b.insert("common.unknown", "Unknown");
    b.insert("common.none", "None");
    b.insert("common.all", "All");
    // Config
    b.insert("config.title", "Configuration");
    b.insert("config.invalid", "Invalid configuration: {detail}");
    b.insert("config.read_error", "Failed to read configuration file");
    b.insert("config.parse_error", "Failed to parse configuration");
    // Gateway
    b.insert("gateway.starting", "Gateway is starting\u{2026}");
    b.insert("gateway.ready", "Gateway is ready");
    b.insert("gateway.stopping", "Gateway is shutting down\u{2026}");
    b.insert("gateway.stopped", "Gateway shut down");
    // Agent
    b.insert("agent.busy", "Agent is busy");
    b.insert("agent.idle", "Agent is idle");
    b.insert("agent.sleeping", "Agent is sleeping");
    b.insert("agent.session_started", "Session started");
    // Plugin
    b.insert("plugin.loaded", "Plugin loaded successfully");
    b.insert("plugin.load_failed", "Plugin failed to load");
    // Tool
    b.insert("tool.started", "Tool execution started");
    b.insert("tool.completed", "Tool execution completed");
    b.insert("tool.failed", "Tool execution failed");
    // HTTP / API
    b.insert("http.unauthorized", "Unauthorized");
    b.insert("http.forbidden", "Forbidden");
    b.insert("http.not_found", "Not Found");
    b.insert("http.internal_error", "Internal Server Error");
    b.insert("http.service_unavailable", "Service Unavailable");
    b.insert("http.rate_limited", "Rate limit exceeded");
    // Validation
    b.insert("validation.required", "This field is required");
    b.insert("validation.too_long", "Value is too long");
    b.insert("validation.invalid_format", "Invalid format");
    b.insert("validation.out_of_range", "Value out of range");
    // Lifecycle
    b.insert("lifecycle.phase_start", "Starting phase {phase}\u{2026}");
    b.insert("lifecycle.phase_complete", "Phase {phase} complete");
    // TUI
    b.insert("tui.logs.title", "Logs");
    b.insert("tui.logs.switch_hint", "Tab to switch");
    b.insert("tui.approvals.title", "Pending Approvals");
    b.insert("tui.approvals.no_pending", "No pending approvals.");
    b.insert("tui.approvals.capabilities_for", "Capabilities for");
    b.insert("tui.approvals.approve_deny_hint", "Enter=Approve  d=Deny");
    b.insert("tui.footer.tab", "Tab");
    b.insert("tui.footer.switch_focus", "switch focus");
    b.insert("tui.footer.navigate", "navigate");
    b.insert("tui.footer.enter", "Enter");
    b.insert("tui.footer.approve", "approve");
    b.insert("tui.footer.d_key", "d");
    b.insert("tui.footer.deny", "deny");
    b.insert("tui.footer.scroll", "scroll");
    b.insert("tui.footer.q_key", "q");
    b.insert("tui.footer.quit", "quit");
    b.insert("tui.error.approve_failed", "Approve failed");
    b.insert("tui.error.deny_failed", "Deny failed");
    b.insert("tui.error.no_plugin_selected", "No plugin selected");
    // Desktop
    b.insert("desktop.menu.reload_skills", "Reload Skills");
    b.insert("desktop.menu.quit", "Quit aman desktop");
    b.insert("desktop.menu.file", "File");
    b.insert("desktop.menu.edit", "Edit");
    b.insert("desktop.menu.help", "Help");
    b.insert("desktop.menu.about", "About aman desktop");
    b.insert("desktop.menu.devtools", "Toggle DevTools");
    b.insert("desktop.error.no_gateway", "Gateway not connected. Start the gateway daemon first.");
    b.insert("desktop.error.already_connected", "Already connected to a gateway");
    b.insert("desktop.error.gateway_unreachable", "Gateway not reachable at {url}");
    b.insert("desktop.error.spawn_failed", "Failed to spawn gateway at {path}");
    b.insert("desktop.info.gateway_started", "Gateway started at {url}");
    b.insert("desktop.info.gateway_connected", "Connected to already-running gateway at {url}");
    b.insert("desktop.error.startup_timeout", "Gateway startup timed out after {secs}s");
    b
});

// ── Simplified Chinese bundle ───────────────────────────────────────

static ZHS: std::sync::LazyLock<Bundle> = std::sync::LazyLock::new(|| {
    let mut b = Bundle::new();
    // Common
    b.insert("common.ok", "确定");
    b.insert("common.cancel", "取消");
    b.insert("common.close", "关闭");
    b.insert("common.save", "保存");
    b.insert("common.delete", "删除");
    b.insert("common.edit", "编辑");
    b.insert("common.search", "搜索");
    b.insert("common.loading", "加载中\u{2026}");
    b.insert("common.error", "错误");
    b.insert("common.warning", "警告");
    b.insert("common.success", "成功");
    b.insert("common.yes", "是");
    b.insert("common.no", "否");
    b.insert("common.submit", "提交");
    b.insert("common.refresh", "刷新");
    b.insert("common.settings", "设置");
    b.insert("common.help", "帮助");
    b.insert("common.about", "关于");
    b.insert("common.home", "首页");
    b.insert("common.back", "返回");
    b.insert("common.next", "下一步");
    b.insert("common.previous", "上一步");
    b.insert("common.finish", "完成");
    b.insert("common.retry", "重试");
    b.insert("common.skip", "跳过");
    b.insert("common.confirm", "确认");
    b.insert("common.are_you_sure", "确定吗？");
    b.insert("common.no_results", "未找到结果。");
    b.insert("common.copied", "已复制到剪贴板");
    b.insert("common.enabled", "已启用");
    b.insert("common.disabled", "已禁用");
    b.insert("common.online", "在线");
    b.insert("common.offline", "离线");
    b.insert("common.unknown", "未知");
    b.insert("common.none", "无");
    b.insert("common.all", "全部");
    // Config
    b.insert("config.title", "配置");
    b.insert("config.invalid", "配置无效：{detail}");
    b.insert("config.read_error", "无法读取配置文件");
    b.insert("config.parse_error", "无法解析配置");
    // Gateway
    b.insert("gateway.starting", "网关正在启动\u{2026}");
    b.insert("gateway.ready", "网关已就绪");
    b.insert("gateway.stopping", "网关正在关闭\u{2026}");
    b.insert("gateway.stopped", "网关已关闭");
    // Agent
    b.insert("agent.busy", "智能体忙碌中");
    b.insert("agent.idle", "智能体空闲中");
    b.insert("agent.sleeping", "智能体休眠中");
    b.insert("agent.session_started", "会话已开始");
    // Plugin
    b.insert("plugin.loaded", "插件加载成功");
    b.insert("plugin.load_failed", "插件加载失败");
    // Tool
    b.insert("tool.started", "工具执行开始");
    b.insert("tool.completed", "工具执行完成");
    b.insert("tool.failed", "工具执行失败");
    // HTTP / API
    b.insert("http.unauthorized", "未授权");
    b.insert("http.forbidden", "禁止访问");
    b.insert("http.not_found", "未找到");
    b.insert("http.internal_error", "内部服务器错误");
    b.insert("http.service_unavailable", "服务不可用");
    b.insert("http.rate_limited", "请求频率超限");
    // Validation
    b.insert("validation.required", "此字段为必填项");
    b.insert("validation.too_long", "值过长");
    b.insert("validation.invalid_format", "格式无效");
    b.insert("validation.out_of_range", "值超出范围");
    // Lifecycle
    b.insert("lifecycle.phase_start", "正在启动阶段 {phase}\u{2026}");
    b.insert("lifecycle.phase_complete", "阶段 {phase} 完成");
    // TUI
    b.insert("tui.logs.title", "日志");
    b.insert("tui.logs.switch_hint", "Tab 切换");
    b.insert("tui.approvals.title", "待审批");
    b.insert("tui.approvals.no_pending", "暂无待审批项。");
    b.insert("tui.approvals.capabilities_for", "能力详情");
    b.insert("tui.approvals.approve_deny_hint", "Enter=批准  d=拒绝");
    b.insert("tui.footer.tab", "Tab");
    b.insert("tui.footer.switch_focus", "切换焦点");
    b.insert("tui.footer.navigate", "导航");
    b.insert("tui.footer.enter", "Enter");
    b.insert("tui.footer.approve", "批准");
    b.insert("tui.footer.d_key", "d");
    b.insert("tui.footer.deny", "拒绝");
    b.insert("tui.footer.scroll", "滚动");
    b.insert("tui.footer.q_key", "q");
    b.insert("tui.footer.quit", "退出");
    b.insert("tui.error.approve_failed", "批准失败");
    b.insert("tui.error.deny_failed", "拒绝失败");
    b.insert("tui.error.no_plugin_selected", "未选择插件");
    // Desktop
    b.insert("desktop.menu.reload_skills", "重新加载技能");
    b.insert("desktop.menu.quit", "退出 aman desktop");
    b.insert("desktop.menu.file", "文件");
    b.insert("desktop.menu.edit", "编辑");
    b.insert("desktop.menu.help", "帮助");
    b.insert("desktop.menu.about", "关于 aman desktop");
    b.insert("desktop.menu.devtools", "切换开发者工具");
    b.insert("desktop.error.no_gateway", "网关未连接，请先启动网关守护进程。");
    b.insert("desktop.error.already_connected", "已连接到网关");
    b.insert("desktop.error.gateway_unreachable", "无法访问网关 {url}");
    b.insert("desktop.error.spawn_failed", "无法启动网关 {path}");
    b.insert("desktop.info.gateway_started", "网关已启动 {url}");
    b.insert("desktop.info.gateway_connected", "已连接到运行中的网关 {url}");
    b.insert("desktop.error.startup_timeout", "网关启动超时 ({secs}秒)");
    b
});

// ── Convenience free functions ──────────────────────────────────────

/// Translate a key using the given locale (no placeholder substitution).
///
/// Shortcut for `Translator::new(locale).translate(key)`.
#[must_use]
pub fn t(locale: Locale, key: TranslationKey) -> &'static str {
    Translator::new(locale).translate(key)
}

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locale_default_is_en() {
        assert_eq!(Locale::default(), Locale::En);
    }

    #[test]
    fn locale_code_roundtrip() {
        for locale in Locale::all() {
            let code = locale.code();
            let parsed = Locale::from_code(code);
            assert_eq!(parsed, Some(*locale), "roundtrip failed for {code}");
        }
    }

    #[test]
    fn locale_from_code_case_insensitive() {
        assert_eq!(Locale::from_code("EN"), Some(Locale::En));
        assert_eq!(Locale::from_code("ZHS"), Some(Locale::Zhs));
    }

    #[test]
    fn locale_from_code_aliases() {
        assert_eq!(Locale::from_code("zh-hans"), Some(Locale::Zhs));
        assert_eq!(Locale::from_code("zh_cn"), Some(Locale::Zhs));
        assert_eq!(Locale::from_code("zh-CN"), Some(Locale::Zhs));
        assert_eq!(Locale::from_code("english"), Some(Locale::En));
    }

    #[test]
    fn locale_from_code_unknown() {
        assert_eq!(Locale::from_code("ja"), None);
        assert_eq!(Locale::from_code(""), None);
    }

    #[test]
    fn locale_display() {
        assert_eq!(Locale::En.to_string(), "en");
        assert_eq!(Locale::Zhs.to_string(), "zhs");
    }

    #[test]
    fn translate_english() {
        let t = Translator::new(Locale::En);
        assert_eq!(t.translate("common.ok"), "OK");
        assert_eq!(t.translate("common.cancel"), "Cancel");
        assert_eq!(t.translate("gateway.ready"), "Gateway is ready");
    }

    #[test]
    fn translate_chinese() {
        let t = Translator::new(Locale::Zhs);
        assert_eq!(t.translate("common.ok"), "确定");
        assert_eq!(t.translate("common.cancel"), "取消");
        assert_eq!(t.translate("gateway.ready"), "网关已就绪");
    }

    #[test]
    fn translate_fallback_to_en() {
        // Add a key only in EN, then query in ZHS — should get EN fallback.
        let t = Translator::new(Locale::Zhs);
        // "common.none" exists in both bundles, should get ZHS version.
        assert_eq!(t.translate("common.none"), "无");
        // All keys from key module exist in both bundles, so fallback is hard to test
        // without a test-only key. Instead test that missing keys return themselves.
    }

    #[test]
    fn translate_missing_key_returns_key() {
        let t = Translator::new(Locale::En);
        assert_eq!(t.translate("nonexistent.key.12345"), "nonexistent.key.12345");
    }

    #[test]
    fn translate_with_placeholders() {
        let t = Translator::new(Locale::En);
        let mut args = HashMap::new();
        args.insert("detail", "something went wrong");
        let result = t.translate_with("config.invalid", &args);
        assert_eq!(result, "Invalid configuration: something went wrong");
    }

    #[test]
    fn translate_with_placeholders_zhs() {
        let t = Translator::new(Locale::Zhs);
        let mut args = HashMap::new();
        args.insert("phase", "3");
        let result = t.translate_with("lifecycle.phase_start", &args);
        assert_eq!(result, "正在启动阶段 3\u{2026}");
    }

    #[test]
    fn has_key() {
        assert!(Translator::has_key("common.ok"));
        assert!(!Translator::has_key("nonexistent.key"));
    }

    #[test]
    fn translator_default_is_en() {
        let t = Translator::default();
        assert_eq!(t.locale(), Locale::En);
        assert_eq!(t.translate("common.ok"), "OK");
    }

    #[test]
    fn all_keys_have_both_locales() {
        // Every key in the EN bundle must also exist in ZHS.
        for key in EN.keys() {
            assert!(
                ZHS.contains_key(key),
                "key '{key}' missing from ZHS bundle"
            );
        }
        // And vice versa.
        for key in ZHS.keys() {
            assert!(
                EN.contains_key(key),
                "key '{key}' missing from EN bundle"
            );
        }
    }

    #[test]
    fn serde_locale() {
        let json = serde_json::to_string(&Locale::En).unwrap();
        assert_eq!(json, "\"en\"");
        let parsed: Locale = serde_json::from_str("\"zhs\"").unwrap();
        assert_eq!(parsed, Locale::Zhs);
    }
}
