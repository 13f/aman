// i18n module — locale state and translation functions.
// Loaded on app startup via the Rust backend's `get_locale` command.

export type LocaleCode = "en" | "zhs";

export interface LocaleInfo {
  code: LocaleCode;
  display: string;
}

// Reactive state so Svelte components can react to locale changes.
let current: LocaleInfo = $state({ code: "en" as LocaleCode, display: "English" });

/** Current locale info (reactive). */
export function locale(): LocaleInfo {
  return current;
}

/** Set the active locale. */
export function setLocale(info: LocaleInfo): void {
  current = info;
}

// ── Translation map ────────────────────────────────────────────────

type Bundle = Record<string, string>;

const EN: Bundle = {
  // App shell
  "nav.dashboard": "Dashboard",
  "nav.chat": "Chat",
  "nav.agents": "Agents",
  "nav.providers": "Providers",
  "nav.workflow": "Workflow",
  "nav.maintenance": "Maintenance",
  "nav.plugins": "Plugins",
  "nav.settings": "Settings",
  "nav.integration": "Integration",
  "nav.mcp": "MCP Servers",
  "nav.activity": "Activity",
  "nav.home": "Home",

  // Common
  "common.loading": "Loading…",
  "common.error": "Error",
  "common.save": "Save",
  "common.cancel": "Cancel",
  "common.delete": "Delete",
  "common.confirm": "Confirm",
  "common.retry": "Retry",
  "common.close": "Close",
  "common.search": "Search",
  "common.edit": "Edit",
  "common.enabled": "Enabled",
  "common.disabled": "Disabled",
  "common.yes": "Yes",
  "common.no": "No",
  "common.online": "Online",
  "common.offline": "Offline",
  "common.ok": "OK",
  "common.copied": "Copied!",

  // Dashboard
  "dashboard.title": "Dashboard",
  "dashboard.gateway_status": "Gateway Status",
  "dashboard.running": "Running",
  "dashboard.stopped": "Stopped",
  "dashboard.start": "Start Gateway",
  "dashboard.stop": "Stop Gateway",
  "dashboard.port": "Port",
  "dashboard.not_connected": "Not connected",

  // Chat
  "chat.title": "Chat",
  "chat.new_session": "New Session",
  "chat.send": "Send",
  "chat.stop": "Stop",
  "chat.thinking": "Thinking…",
  "chat.type_message": "Type a message…",
  "chat.no_sessions": "No conversations yet.",

  // Settings
  "settings.title": "Settings",
  "settings.language": "Language",
  "settings.locale_en": "English",
  "settings.locale_zhs": "简体中文",
};

const ZHS: Bundle = {
  // App shell
  "nav.dashboard": "仪表盘",
  "nav.chat": "对话",
  "nav.agents": "智能体",
  "nav.providers": "服务商",
  "nav.workflow": "工作流",
  "nav.maintenance": "维护",
  "nav.plugins": "插件",
  "nav.settings": "设置",
  "nav.integration": "集成",
  "nav.mcp": "MCP 服务器",
  "nav.activity": "活动",
  "nav.home": "首页",

  // Common
  "common.loading": "加载中…",
  "common.error": "错误",
  "common.save": "保存",
  "common.cancel": "取消",
  "common.delete": "删除",
  "common.confirm": "确认",
  "common.retry": "重试",
  "common.close": "关闭",
  "common.search": "搜索",
  "common.edit": "编辑",
  "common.enabled": "已启用",
  "common.disabled": "已禁用",
  "common.yes": "是",
  "common.no": "否",
  "common.online": "在线",
  "common.offline": "离线",
  "common.ok": "确定",
  "common.copied": "已复制！",

  // Dashboard
  "dashboard.title": "仪表盘",
  "dashboard.gateway_status": "网关状态",
  "dashboard.running": "运行中",
  "dashboard.stopped": "已停止",
  "dashboard.start": "启动网关",
  "dashboard.stop": "停止网关",
  "dashboard.port": "端口",
  "dashboard.not_connected": "未连接",

  // Chat
  "chat.title": "对话",
  "chat.new_session": "新会话",
  "chat.send": "发送",
  "chat.stop": "停止",
  "chat.thinking": "思考中…",
  "chat.type_message": "输入消息…",
  "chat.no_sessions": "暂无对话。",

  // Settings
  "settings.title": "设置",
  "settings.language": "语言",
  "settings.locale_en": "English",
  "settings.locale_zhs": "简体中文",
};

/**
 * Translate a key into the current locale.
 * Falls back to English, then returns the key itself if missing.
 */
export function t(key: string): string {
  const bundle = current.code === "zhs" ? ZHS : EN;
  return bundle[key] ?? EN[key] ?? key;
}
