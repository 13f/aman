// Shared time formatting for chat message bubbles.
// Kept locale-aware and dependency-free so both Chat.svelte and
// AgentChatTab.svelte (and tests) can reuse it.
//
// All inputs are ISO-8601 strings (e.g. "2026-07-19T14:03:22.000Z") produced
// by the Tauri client (`new Date().toISOString()`) or by replaying history
// (`new Date(evt.timestamp_ms).toISOString()`).

import type { LocaleCode } from "./i18n.svelte";
import { t } from "./i18n.svelte";

/** Map our i18n locale code to a BCP-47 tag accepted by Intl. */
export function localeTag(code: LocaleCode): string {
  return code === "zhs" ? "zh-CN" : "en-US";
}

/** Local calendar day key, e.g. "2026-07-19" (local tz, not UTC). */
export function dayKey(iso: string, code: LocaleCode = "en"): string {
  const d = new Date(iso);
  const tag = localeTag(code);
  // Date-only portion in the message's local time.
  return d.toLocaleDateString(tag, { year: "numeric", month: "2-digit", day: "2-digit" });
}

/** Full localized date+time for tooltips, e.g. "Jul 19, 2026, 2:03 PM". */
export function formatMessageFull(iso: string, code: LocaleCode = "en"): string {
  const d = new Date(iso);
  return d.toLocaleString(localeTag(code), {
    year: "numeric",
    month: "short",
    day: "numeric",
    hour: "2-digit",
    minute: "2-digit",
  });
}

/** Short local time for inline display, e.g. "14:03". */
export function formatMessageTime(iso: string, code: LocaleCode = "en"): string {
  const d = new Date(iso);
  return d.toLocaleTimeString(localeTag(code), { hour: "2-digit", minute: "2-digit", hour12: false });
}

/** Whole-day difference between two ISO timestamps (local calendars). */
export function dayDiff(aIso: string, bIso: string): number {
  const a = new Date(aIso);
  const b = new Date(bIso);
  const ms = Date.UTC(a.getFullYear(), a.getMonth(), a.getDate()) -
    Date.UTC(b.getFullYear(), b.getMonth(), b.getDate());
  return Math.round(ms / 86_400_000);
}

/**
 * Human date label for a divider: "Today" / "Yesterday" / localized date.
 * `now` defaults to the current time (injectable for tests).
 */
export function formatMessageDateLabel(iso: string, code: LocaleCode = "en", now: Date = new Date()): string {
  const diff = dayDiff(iso, now.toISOString());
  if (diff === 0) return t("chat.today");
  if (diff === 1) return t("chat.yesterday");
  const d = new Date(iso);
  return d.toLocaleDateString(localeTag(code), {
    year: "numeric",
    month: "short",
    day: "numeric",
  });
}
