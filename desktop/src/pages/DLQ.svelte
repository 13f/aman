<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import { t } from "../lib/i18n.svelte";

  interface DlqEntry {
    id: string;
    event_source: string;
    event_type: string;
    reason: string;
    retry_count: number;
    enqueued_at_ms: number;
  }

  let entries = $state<DlqEntry[]>([]);
  let loading = $state(false);
  let result = $state("");
  let autoRefresh = $state(false);
  let autoTimer: ReturnType<typeof setInterval> | undefined;

  function toggleAuto() {
    autoRefresh = !autoRefresh;
    if (autoRefresh) {
      loadDlq();
      autoTimer = setInterval(loadDlq, 4000);
    } else {
      if (autoTimer) clearInterval(autoTimer);
      autoTimer = undefined;
    }
  }

  function fmtTime(ms: number): string {
    if (ms === 0) return "-";
    const d = new Date(ms);
    return d.toLocaleString();
  }

  async function loadDlq() {
    loading = true;
    try {
      entries = await invoke<DlqEntry[]>("list_dlq");
    } catch (e: any) {
      if (!autoRefresh) result = String(e);
    } finally {
      loading = false;
    }
  }

  async function doRetry(id: string) {
    try {
      result = await invoke<string>("retry_dlq", { id });
      await loadDlq();
    } catch (e: any) {
      result = String(e);
    }
  }

  async function doDiscard(id: string) {
    try {
      result = await invoke<string>("discard_dlq", { id });
      await loadDlq();
    } catch (e: any) {
      result = String(e);
    }
  }
</script>

<div class="card" style="display:flex;align-items:center;justify-content:space-between;">
  <h2>Dead Letter Queue</h2>
  <div style="display:flex;gap:8px;align-items:center;">
    <label style="font-size:13px;display:flex;align-items:center;gap:4px;cursor:pointer;">
      <input type="checkbox" checked={autoRefresh} onchange={toggleAuto} />
      Auto
    </label>
    <button class="secondary" onclick={loadDlq} disabled={loading}>{t("maintenance.refresh")}</button>
  </div>
</div>

{#if result}
  <div class="card">
    <p style="font-size:13px;color:var(--accent);">{result}</p>
  </div>
{/if}

<div class="card">
  {#if entries.length === 0}
    <p style="color:var(--fg-dim);font-size:13px;">No entries in the dead letter queue. Click "Refresh" to check.</p>
  {:else}
    <table>
      <thead>
        <tr><th>ID</th><th>Source</th><th>Type</th><th>Reason</th><th>Retries</th><th>Enqueued</th><th>Actions</th></tr>
      </thead>
      <tbody>
        {#each entries as e}
          <tr>
            <td style="font-family:monospace;font-size:12px;">{e.id.slice(0, 8)}…</td>
            <td>{e.event_source}</td>
            <td><span class="badge warn">{e.event_type}</span></td>
            <td style="color:var(--fg-dim);font-size:12px;max-width:200px;overflow:hidden;text-overflow:ellipsis;white-space:nowrap;">{e.reason}</td>
            <td>{e.retry_count}</td>
            <td style="font-size:11px;color:var(--fg-dim);white-space:nowrap;">{fmtTime(e.enqueued_at_ms)}</td>
            <td>
              <button style="margin-right:4px;" onclick={() => doRetry(e.id)}>{t("common.retry")}</button>
              <button class="danger" onclick={() => doDiscard(e.id)}>Discard</button>
            </td>
          </tr>
        {/each}
      </tbody>
    </table>
  {/if}
</div>
