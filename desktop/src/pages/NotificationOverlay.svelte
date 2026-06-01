<script lang="ts">
  import { listen } from "@tauri-apps/api/event";
  import { invoke } from "@tauri-apps/api/core";
  import { onMount, onDestroy } from "svelte";

  interface Notification {
    id: string;
    severity: "critical" | "warning" | "info";
    category: string;
    title: string;
    message: string;
    dismissible: boolean;
    action_label: string | null;
    action_route: string | null;
  }

  let notifications = $state<Notification[]>([]);
  let timers = $state<Map<string, ReturnType<typeof setTimeout>>>(new Map());
  let unlisteners: (() => void)[] = [];
  let seenIds = new Set<string>();

  let { onNavigate }: { onNavigate?: (page: string) => void } = $props();

  function showOverlay(notif: Notification) {
    notifications = [...notifications, notif];
    seenIds.add(notif.id);

    if (notif.dismissible) {
      const timeoutMs = notif.severity === "info" ? 3000 : 5000;
      const timer = setTimeout(() => {
        // Visual-only dismiss — notification stays active in the store
        notifications = notifications.filter((n) => n.id !== notif.id);
        timers.delete(notif.id);
      }, timeoutMs);
      timers.set(notif.id, timer);
    }
  }

  function dismiss(id: string) {
    const existing = timers.get(id);
    if (existing) {
      clearTimeout(existing);
      timers.delete(id);
    }
    notifications = notifications.filter((n) => n.id !== id);
    invoke("notification_dismiss", { id }).catch(() => {});
  }

  function acknowledge(notif: Notification) {
    dismiss(notif.id);
    invoke("notification_ack", { id: notif.id }).catch(() => {});
    if (notif.action_route && onNavigate) {
      onNavigate(notif.action_route);
    }
  }

  async function handleNotificationUpdated() {
    try {
      const items = await invoke<Notification[]>("get_notifications", {
        activeOnly: true,
        severity: null,
      });
      const activeIds = new Set(items.map((n) => n.id));

      // Remove locally-shown notifications that are no longer active
      // (e.g., dismissed/acked from the bell dropdown)
      notifications = notifications.filter((n) => activeIds.has(n.id));

      // Show new notifications
      for (const item of items) {
        if (!seenIds.has(item.id)) {
          showOverlay(item);
        }
      }

      // Prune seenIds of notifications no longer active
      for (const id of seenIds) {
        if (!activeIds.has(id)) {
          seenIds.delete(id);
        }
      }
    } catch {
      // Gateway not connected
    }
  }

  onMount(async () => {
    unlisteners.push(await listen("notification:updated", handleNotificationUpdated));
  });

  onDestroy(() => {
    for (const fn of unlisteners) fn();
    for (const timer of timers.values()) clearTimeout(timer);
  });
</script>

{#if notifications.length > 0}
  <div class="overlay-container">
    {#each notifications as notif (notif.id)}
      <div class="notif-banner severity-{notif.severity}">
        <div class="notif-icon">
          {#if notif.severity === "critical"}🔴{:else if notif.severity === "warning"}⚠️{:else}✅{/if}
        </div>
        <div class="notif-body">
          <div class="notif-title">{notif.title}</div>
          <div class="notif-message">{notif.message}</div>
        </div>
        <div class="notif-actions">
          {#if notif.action_label}
            <button class="action-btn" onclick={() => acknowledge(notif)}>
              {notif.action_label}
            </button>
          {:else if !notif.dismissible}
            <button class="action-btn" onclick={() => acknowledge(notif)}>
              确认
            </button>
          {/if}
          {#if notif.dismissible}
            <button class="dismiss-btn" onclick={() => dismiss(notif.id)}>✕</button>
          {/if}
        </div>
      </div>
    {/each}
  </div>
{/if}

<style>
  .overlay-container {
    position: fixed;
    top: 16px;
    left: 50%;
    transform: translateX(-50%);
    z-index: 9999;
    display: flex;
    flex-direction: column;
    gap: 8px;
    pointer-events: none;
  }

  .notif-banner {
    display: flex;
    align-items: center;
    gap: 12px;
    padding: 12px 20px;
    border-radius: 12px;
    box-shadow: 0 4px 24px rgba(0, 0, 0, 0.35);
    backdrop-filter: blur(12px);
    min-width: 380px;
    max-width: 520px;
    animation: slideIn 0.25s ease-out;
    pointer-events: auto;
  }

  .severity-critical {
    background: color-mix(in srgb, var(--red) 96%, transparent);
    color: white;
    border: 1px solid rgba(255, 255, 255, 0.2);
  }

  .severity-warning {
    background: color-mix(in srgb, var(--yellow) 95%, transparent);
    color: #1a1a1a;
    border: 1px solid rgba(255, 255, 255, 0.3);
  }

  .severity-info {
    background: color-mix(in srgb, var(--green) 94%, transparent);
    color: white;
    border: 1px solid rgba(255, 255, 255, 0.2);
  }

  .notif-icon {
    font-size: 20px;
    flex-shrink: 0;
  }

  .notif-body {
    flex: 1;
    min-width: 0;
  }

  .notif-title {
    font-weight: 700;
    font-size: 14px;
    line-height: 1.3;
    margin-bottom: 2px;
  }

  .notif-message {
    font-size: 12px;
    opacity: 0.85;
    line-height: 1.4;
    word-break: break-word;
  }

  .notif-actions {
    display: flex;
    align-items: center;
    gap: 6px;
    flex-shrink: 0;
  }

  .action-btn {
    background: rgba(255, 255, 255, 0.2);
    border: 1px solid rgba(255, 255, 255, 0.3);
    color: inherit;
    padding: 4px 12px;
    border-radius: 6px;
    font-size: 12px;
    font-weight: 600;
    cursor: pointer;
    white-space: nowrap;
    transition: background 0.15s;
  }

  .action-btn:hover {
    background: rgba(255, 255, 255, 0.35);
  }

  .dismiss-btn {
    background: none;
    border: none;
    color: inherit;
    font-size: 16px;
    cursor: pointer;
    opacity: 0.6;
    padding: 2px 4px;
    line-height: 1;
  }

  .dismiss-btn:hover {
    opacity: 1;
  }

  @keyframes slideIn {
    from {
      opacity: 0;
      transform: translateY(-16px);
    }
    to {
      opacity: 1;
      transform: translateY(0);
    }
  }
</style>
