<script lang="ts">
  import { listen } from "@tauri-apps/api/event";
  import { invoke } from "@tauri-apps/api/core";
  import { onMount, onDestroy } from "svelte";

  interface Notification {
    id: string;
    severity: "critical" | "warning";
    category: string;
    created_at: number;
    title: string;
    message: string;
    dismissed: boolean;
    dismissible: boolean;
    action_label: string | null;
    action_route: string | null;
    event_id: string | null;
    source: string | null;
  }

  let unreadCount = $state(0);
  let dropdownOpen = $state(false);
  let notifications = $state<Notification[]>([]);
  let unlisteners: (() => void)[] = [];

  let { onNavigate }: { onNavigate?: (page: string) => void } = $props();

  function formatTime(ms: number): string {
    const secs = Math.floor((Date.now() - ms) / 1000);
    if (secs < 60) return `${secs}s`;
    if (secs < 3600) return `${Math.floor(secs / 60)}m`;
    if (secs < 86400) return `${Math.floor(secs / 3600)}h`;
    return `${Math.floor(secs / 86400)}d`;
  }

  async function fetchNotifications() {
    try {
      const items = await invoke<Notification[]>("get_notifications", {
        activeOnly: true,
        severity: null,
      });
      notifications = items;
      unreadCount = items.length;
    } catch {
      // Gateway not connected
    }
  }

  async function handleDismiss(id: string) {
    await invoke("notification_dismiss", { id });
    await fetchNotifications();
  }

  async function handleAck(id: string, route: string | null) {
    await invoke("notification_ack", { id });
    await fetchNotifications();
    if (route && onNavigate) {
      dropdownOpen = false;
      onNavigate(route);
    }
  }

  async function handleDismissAll() {
    await invoke("notification_dismiss_all");
    await fetchNotifications();
  }

  function handleNotificationUpdated() {
    fetchNotifications();
  }

  function toggleDropdown() {
    if (!dropdownOpen) {
      fetchNotifications();
    }
    dropdownOpen = !dropdownOpen;
  }

  function handleClickOutside(e: MouseEvent) {
    const target = e.target as HTMLElement;
    if (!target.closest(".bell-wrapper")) {
      dropdownOpen = false;
    }
  }

  onMount(async () => {
    await fetchNotifications();
    unlisteners.push(await listen("notification:updated", handleNotificationUpdated));
    document.addEventListener("click", handleClickOutside);
  });

  onDestroy(() => {
    for (const fn of unlisteners) fn();
    document.removeEventListener("click", handleClickOutside);
  });
</script>

<div class="bell-wrapper">
  <button class="bell-btn" onclick={toggleDropdown} title="通知">
    <span class="bell-icon">🔔</span>
    {#if unreadCount > 0}
      <span class="badge severity-{notifications[0]?.severity ?? 'warning'}">
        {unreadCount > 99 ? "99+" : unreadCount}
      </span>
    {/if}
  </button>

  {#if dropdownOpen}
    <div class="dropdown">
      <div class="dropdown-header">
        <span class="dropdown-title">通知</span>
        {#if unreadCount > 0}
          <button class="dismiss-all-btn" onclick={handleDismissAll}>全部已读</button>
        {/if}
      </div>

      <div class="dropdown-list">
        {#if notifications.length === 0}
          <div class="empty-state">暂无通知</div>
        {:else}
          {#each notifications as notif (notif.id)}
            <div class="notif-item severity-{notif.severity}">
              <div class="notif-indicator"></div>
              <div class="notif-content">
                <div class="notif-title-row">
                  <span class="notif-item-title">{notif.title}</span>
                  <span class="notif-time">{formatTime(notif.created_at)}</span>
                </div>
                <div class="notif-item-message">{notif.message}</div>
                <div class="notif-item-actions">
                  {#if notif.action_label && notif.action_route}
                    <button
                      class="notif-action-link"
                      onclick={() => handleAck(notif.id, notif.action_route)}
                    >
                      {notif.action_label}
                    </button>
                  {/if}
                  {#if notif.dismissible}
                    <button class="notif-dismiss-link" onclick={() => handleDismiss(notif.id)}>
                      忽略
                    </button>
                  {:else}
                    <button
                      class="notif-action-link"
                      onclick={() => handleAck(notif.id, notif.action_route)}
                    >
                      确认
                    </button>
                  {/if}
                </div>
              </div>
            </div>
          {/each}
        {/if}
      </div>
    </div>
  {/if}
</div>

<style>
  .bell-wrapper {
    position: relative;
    display: inline-block;
  }

  .bell-btn {
    position: relative;
    background: none;
    border: none;
    cursor: pointer;
    padding: 6px;
    display: flex;
    align-items: center;
    justify-content: center;
    width: 100%;
    border-radius: 8px;
    transition: background 0.15s;
  }

  .bell-btn:hover {
    background: var(--bg-hover);
  }

  .bell-icon {
    font-size: 18px;
    line-height: 1;
  }

  .badge {
    position: absolute;
    top: 0;
    right: 4px;
    min-width: 16px;
    height: 16px;
    padding: 0 4px;
    border-radius: 8px;
    font-size: 10px;
    font-weight: 700;
    line-height: 16px;
    text-align: center;
    color: white;
  }

  .badge.severity-critical {
    background: var(--red);
    animation: pulse 1.5s ease-in-out infinite;
  }

  .badge.severity-warning {
    background: var(--yellow);
    color: #1a1a1a;
  }

  .dropdown {
    position: absolute;
    bottom: calc(100% + 8px);
    right: 0;
    background: var(--bg-elevated);
    border: 1px solid var(--border);
    border-radius: 12px;
    box-shadow: 0 -4px 24px rgba(0, 0, 0, 0.4);
    max-height: 400px;
    overflow-y: auto;
    z-index: 9998;
    width: 320px;
  }

  .dropdown-header {
    display: flex;
    justify-content: space-between;
    align-items: center;
    padding: 12px 16px;
    border-bottom: 1px solid var(--border);
  }

  .dropdown-title {
    font-weight: 700;
    font-size: 13px;
    color: var(--fg);
  }

  .dismiss-all-btn {
    background: none;
    border: none;
    color: var(--accent);
    font-size: 11px;
    cursor: pointer;
    padding: 2px 6px;
    border-radius: 4px;
  }

  .dismiss-all-btn:hover {
    background: var(--accent-muted);
  }

  .dropdown-list {
    padding: 4px 0;
  }

  .empty-state {
    padding: 24px 16px;
    text-align: center;
    color: var(--fg-dim);
    font-size: 12px;
  }

  .notif-item {
    display: flex;
    gap: 10px;
    padding: 10px 16px;
    border-bottom: 1px solid var(--border);
  }

  .notif-item:last-child {
    border-bottom: none;
  }

  .notif-indicator {
    width: 8px;
    height: 8px;
    border-radius: 50%;
    margin-top: 5px;
    flex-shrink: 0;
  }

  .notif-item.severity-critical .notif-indicator {
    background: var(--red);
  }

  .notif-item.severity-warning .notif-indicator {
    background: var(--yellow);
  }

  .notif-content {
    flex: 1;
    min-width: 0;
  }

  .notif-title-row {
    display: flex;
    justify-content: space-between;
    align-items: center;
    gap: 8px;
  }

  .notif-item-title {
    font-weight: 600;
    font-size: 12px;
    color: var(--fg);
  }

  .notif-time {
    font-size: 10px;
    color: var(--fg-dim);
    white-space: nowrap;
  }

  .notif-item-message {
    font-size: 11px;
    color: var(--fg-dim);
    margin-top: 2px;
    line-height: 1.3;
  }

  .notif-item-actions {
    display: flex;
    gap: 8px;
    margin-top: 6px;
  }

  .notif-action-link {
    background: none;
    border: none;
    color: var(--accent);
    font-size: 11px;
    font-weight: 600;
    cursor: pointer;
    padding: 0;
  }

  .notif-action-link:hover {
    text-decoration: underline;
  }

  .notif-dismiss-link {
    background: none;
    border: none;
    color: var(--fg-dim);
    font-size: 11px;
    cursor: pointer;
    padding: 0;
  }

  .notif-dismiss-link:hover {
    color: var(--fg);
  }

  @keyframes pulse {
    0%, 100% { opacity: 1; }
    50% { opacity: 0.6; }
  }
</style>
