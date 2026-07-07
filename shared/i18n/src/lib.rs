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
    /// "Config load error: {e}"
    pub const GATEWAY_CONFIG_ERROR: &str = "gateway.config_error";
    /// "HTTP server error: {e}"
    pub const GATEWAY_HTTP_ERROR: &str = "gateway.http_error";
    /// "Runtime start error: {e}"
    pub const GATEWAY_RUNTIME_ERROR: &str = "gateway.runtime_error";
    /// "Runtime start timed out after {secs}s (phase={phase})"
    pub const GATEWAY_RUNTIME_TIMEOUT: &str = "gateway.runtime_timeout";
    /// "Usage: aman [--config PATH] [--bind ADDR] [--token TOKEN] [--soul PATH] [--no-tui]"
    pub const GATEWAY_USAGE: &str = "gateway.usage";

    // ── CLI ─────────────────────────────────────────────────────
    /// "aman v{version} — AmanExistence"
    pub const CLI_VERSION: &str = "cli.version";
    /// "usage: aman analyze trends|anomalies [--from <iso>] [--to <iso>] [--agent <id>] [--addr <ip:port>] [--token <token>] [--grpc]"
    pub const CLI_USAGE_ANALYZE: &str = "cli.usage_analyze";
    /// "invalid time: {other} (use ISO 8601, 'today', 'yesterday', or 'now')"
    pub const CLI_INVALID_TIME: &str = "cli.invalid_time";
    /// "unsupported format: {raw} (only \"json\" is supported)"
    pub const CLI_UNSUPPORTED_FORMAT: &str = "cli.unsupported_format";
    /// "plugin '{name}' approved"
    pub const CLI_PLUGIN_APPROVED: &str = "cli.plugin_approved";
    /// "plugin '{name}' denied"
    pub const CLI_PLUGIN_DENIED: &str = "cli.plugin_denied";
    /// "No pending plugin approvals."
    pub const CLI_NO_PENDING_APPROVALS: &str = "cli.no_pending_approvals";
    /// "plugin approval commands are only available via HTTP REST (omit --grpc)"
    pub const CLI_PLUGIN_GRPC_UNAVAILABLE: &str = "cli.plugin_grpc_unavailable";
    /// "gRPC: skill version listing not yet available via gRPC"
    pub const CLI_SKILL_VERSION_GRPC: &str = "cli.skill_version_grpc";
    /// "skill directory not found: {path}"
    pub const CLI_SKILL_DIR_NOT_FOUND: &str = "cli.skill_dir_not_found";
    /// "✓ all skills passed validation"
    pub const CLI_SKILL_ALL_PASSED: &str = "cli.skill_all_passed";
    /// "{errors} error(s), {warnings} warning(s)"
    pub const CLI_SKILL_ERROR_SUMMARY: &str = "cli.skill_error_summary";
    /// "{warnings} warning(s), 0 errors"
    pub const CLI_SKILL_WARNINGS_ONLY: &str = "cli.skill_warnings_only";
    /// "usage: aman skill export <out_dir>"
    pub const CLI_SKILL_EXPORT_USAGE: &str = "cli.skill_export_usage";
    /// "exported {count} skill(s) to {dir}"
    pub const CLI_EXPORT_RESULT: &str = "cli.export_result";
    /// "stdio server error: {e}"
    pub const CLI_STDIO_SERVER_ERROR: &str = "cli.stdio_server_error";
    /// "gRPC connect error: {e}"
    pub const CLI_GRPC_CONNECT_ERROR: &str = "cli.grpc_connect_error";
    /// "gRPC: {e}"
    pub const CLI_GRPC_ERROR: &str = "cli.grpc_error";
    /// "unknown flag: {flag}"
    pub const CLI_UNKNOWN_FLAG: &str = "cli.unknown_flag";
    /// "warning: {message}"
    pub const CLI_WARNING: &str = "cli.warning";
    /// "usage: aman ..."
    pub const CLI_USAGE: &str = "cli.usage";

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

    // ── Kernel error descriptions (user-facing) ─────────────────
    /// "Event bus is full — backpressure active"
    pub const ERROR_BUS_FULL: &str = "kernel.error.bus_full";
    /// "Operation timed out"
    pub const ERROR_TIMEOUT: &str = "kernel.error.timeout";
    /// "Version mismatch: expected {expected}, found {found}"
    pub const ERROR_VERSION_MISMATCH: &str = "kernel.error.version_mismatch";
    /// "Dependency cycle detected: {path}"
    pub const ERROR_CYCLE_DETECTED: &str = "kernel.error.cycle_detected";
    /// "Compensation failed: {message}"
    pub const ERROR_COMPENSATION_FAILED: &str = "kernel.error.compensation_failed";
    /// "Unrecoverable error: {message}"
    pub const ERROR_UNRECOVERABLE: &str = "kernel.error.unrecoverable";
    /// "Configuration error: {message}"
    pub const ERROR_CONFIG_INVALID: &str = "kernel.error.config_invalid";
    /// "Secret could not be resolved: {key}"
    pub const ERROR_SECRET_UNRESOLVED: &str = "kernel.error.secret_unresolved";
    /// "Invalid retry backoff: {value}"
    pub const ERROR_INVALID_RETRY_BACKOFF: &str = "kernel.error.invalid_retry_backoff";
    /// "Resource already exists: {name}"
    pub const ERROR_ALREADY_EXISTS: &str = "kernel.error.already_exists";
    /// "Resource not found: {name}"
    pub const ERROR_NOT_FOUND: &str = "kernel.error.not_found";
    /// "Rate limited: retry after {ms}ms"
    pub const ERROR_RATE_LIMITED: &str = "kernel.error.rate_limited";
    /// "Security violation: {message}"
    pub const ERROR_SECURITY_VIOLATION: &str = "kernel.error.security_violation";
    /// "Sandbox error: {message}"
    pub const ERROR_SANDBOX: &str = "kernel.error.sandbox";

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
    /// "Gateway stopped"
    pub const DESKTOP_INFO_GATEWAY_STOPPED: &str = "desktop.info.gateway_stopped";
    /// "Disconnected from gateway"
    pub const DESKTOP_INFO_GATEWAY_DISCONNECTED: &str = "desktop.info.gateway_disconnected";
    /// "Skills reloaded"
    pub const DESKTOP_INFO_SKILLS_RELOADED: &str = "desktop.info.skills_reloaded";
    /// "Skill '{name}' enabled"
    pub const DESKTOP_INFO_SKILL_ENABLED: &str = "desktop.info.skill_enabled";
    /// "Skill '{name}' disabled"
    pub const DESKTOP_INFO_SKILL_DISABLED: &str = "desktop.info.skill_disabled";
    /// "Workflow retried"
    pub const DESKTOP_INFO_WORKFLOW_RETRIED: &str = "desktop.info.workflow_retried";
    /// "Workflow cancelled"
    pub const DESKTOP_INFO_WORKFLOW_CANCELLED: &str = "desktop.info.workflow_cancelled";
    /// "SOUL updated"
    pub const DESKTOP_INFO_SOUL_UPDATED: &str = "desktop.info.soul_updated";
    /// "Plugin {name} enabled"
    pub const DESKTOP_INFO_PLUGIN_ENABLED: &str = "desktop.info.plugin_enabled";
    /// "Plugin {name} disabled"
    pub const DESKTOP_INFO_PLUGIN_DISABLED: &str = "desktop.info.plugin_disabled";
    /// "DLQ entry retried"
    pub const DESKTOP_INFO_DLQ_RETRIED: &str = "desktop.info.dlq_retried";
    /// "DLQ entry discarded"
    pub const DESKTOP_INFO_DLQ_DISCARDED: &str = "desktop.info.dlq_discarded";
    /// "Session ID cannot be empty"
    pub const DESKTOP_ERROR_SESSION_ID_EMPTY: &str = "desktop.error.session_id_empty";
    /// "Message ID cannot be empty"
    pub const DESKTOP_ERROR_MESSAGE_ID_EMPTY: &str = "desktop.error.message_id_empty";
    /// "Edited message cannot be empty"
    pub const DESKTOP_ERROR_MESSAGE_EDITED_EMPTY: &str = "desktop.error.message_edited_empty";
    /// "Trace ID cannot be empty"
    pub const DESKTOP_ERROR_TRACE_ID_EMPTY: &str = "desktop.error.trace_id_empty";
    /// "Message cannot be empty"
    pub const DESKTOP_ERROR_MESSAGE_EMPTY: &str = "desktop.error.message_empty";
    /// "Message exceeds maximum length of {max} characters (got {len})"
    pub const DESKTOP_ERROR_MESSAGE_TOO_LONG: &str = "desktop.error.message_too_long";
    /// "Gateway startup timed out after {secs}s"
    pub const DESKTOP_ERROR_STARTUP_TIMEOUT: &str = "desktop.error.startup_timeout";

    // ── Desktop / Config ──────────────────────────────────────────
    /// "Failed to read config: {detail}"
    pub const DESKTOP_ERROR_CONFIG_READ: &str = "desktop.error.config_read";
    /// "Failed to save config: {detail}"
    pub const DESKTOP_ERROR_CONFIG_SAVE: &str = "desktop.error.config_save";
    /// "Failed to read agents directory: {detail}"
    pub const DESKTOP_ERROR_READ_AGENTS_DIR: &str = "desktop.error.read_agents_dir";

    // ── Desktop / Provider ────────────────────────────────────────
    /// "Provider key can only contain letters, digits, underscores, hyphens"
    pub const DESKTOP_ERROR_PROVIDER_KEY_INVALID: &str = "desktop.error.provider_key_invalid";
    /// "Provider '{key}' already exists"
    pub const DESKTOP_ERROR_PROVIDER_EXISTS: &str = "desktop.error.provider_exists";
    /// "Provider '{key}' not found"
    pub const DESKTOP_ERROR_PROVIDER_NOT_FOUND: &str = "desktop.error.provider_not_found";
    /// "Provider '{provider}' not found — create it first"
    pub const DESKTOP_ERROR_PROVIDER_NOT_FOUND_CREATE_FIRST: &str = "desktop.error.provider_not_found_create_first";
    /// "Provider '{key}' is referenced by agents: {agents} — cannot delete"
    pub const DESKTOP_ERROR_PROVIDER_IN_USE: &str = "desktop.error.provider_in_use";
    /// "Provider '{key}' created"
    pub const DESKTOP_INFO_PROVIDER_CREATED: &str = "desktop.info.provider_created";
    /// "Provider '{key}' updated"
    pub const DESKTOP_INFO_PROVIDER_UPDATED: &str = "desktop.info.provider_updated";
    /// "Provider '{key}' deleted"
    pub const DESKTOP_INFO_PROVIDER_DELETED: &str = "desktop.info.provider_deleted";
    /// "Provider '{key}' API key saved to {backend}"
    pub const DESKTOP_INFO_PROVIDER_API_KEY_SAVED: &str = "desktop.info.provider_api_key_saved";
    /// "Failed to save to keychain: {detail}"
    pub const DESKTOP_ERROR_KEYCHAIN_SAVE: &str = "desktop.error.keychain_save";
    /// "Failed to create HTTP client: {detail}"
    pub const DESKTOP_ERROR_HTTP_CLIENT: &str = "desktop.error.http_client";

    // ── Desktop / Agent ───────────────────────────────────────────
    /// "Agent key can only contain letters, digits, underscores, hyphens"
    pub const DESKTOP_ERROR_AGENT_KEY_INVALID: &str = "desktop.error.agent_key_invalid";
    /// "Agent '{key}' already exists"
    pub const DESKTOP_ERROR_AGENT_EXISTS: &str = "desktop.error.agent_exists";
    /// "Agent '{key}' not found"
    pub const DESKTOP_ERROR_AGENT_NOT_FOUND: &str = "desktop.error.agent_not_found";
    /// "Agent '{key}' has no provider configured"
    pub const DESKTOP_ERROR_AGENT_NO_PROVIDER: &str = "desktop.error.agent_no_provider";
    /// "Agent '{key}' created"
    pub const DESKTOP_INFO_AGENT_CREATED: &str = "desktop.info.agent_created";
    /// "Agent '{key}' updated"
    pub const DESKTOP_INFO_AGENT_UPDATED: &str = "desktop.info.agent_updated";
    /// "Agent '{key}' deleted"
    pub const DESKTOP_INFO_AGENT_DELETED: &str = "desktop.info.agent_deleted";
    /// "Agent '{key}' activated"
    pub const DESKTOP_INFO_AGENT_ACTIVATED: &str = "desktop.info.agent_activated";

    // ── Desktop / MCP ─────────────────────────────────────────────
    /// "Server name cannot be empty"
    pub const DESKTOP_ERROR_MCP_NAME_EMPTY: &str = "desktop.error.mcp_name_empty";
    /// "MCP server '{name}' created"
    pub const DESKTOP_INFO_MCP_CREATED: &str = "desktop.info.mcp_created";
    /// "MCP server '{name}' deleted"
    pub const DESKTOP_INFO_MCP_DELETED: &str = "desktop.info.mcp_deleted";
    /// "Gateway is not running"
    pub const DESKTOP_ERROR_GATEWAY_NOT_RUNNING: &str = "desktop.error.gateway_not_running";
}

// ── Translation bundles ─────────────────────────────────────────────

/// Load a bundle from a compile-time embedded JSON file.
///
/// Keys and values are intentionally leaked so they become `&'static str`
/// — translation bundles live for the entire program lifetime.
fn load_bundle(json: &'static str) -> Bundle {
    let map: std::collections::HashMap<String, String> =
        serde_json::from_str(json).expect("failed to parse i18n bundle JSON");
    let mut bundle = Bundle::with_capacity(map.len());
    for (k, v) in map {
        let k: &'static str = Box::leak(k.into_boxed_str());
        let v: &'static str = Box::leak(v.into_boxed_str());
        bundle.insert(k, v);
    }
    bundle
}

static EN: std::sync::LazyLock<Bundle> =
    std::sync::LazyLock::new(|| load_bundle(include_str!("i18n.en.json")));

static ZHS: std::sync::LazyLock<Bundle> =
    std::sync::LazyLock::new(|| load_bundle(include_str!("i18n.zhs.json")));

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
