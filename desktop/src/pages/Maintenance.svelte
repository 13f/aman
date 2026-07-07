<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import { listen } from "@tauri-apps/api/event";
  import { onMount, onDestroy } from "svelte";
  import { t } from "../lib/i18n.svelte";

  // ── Debug Event Entry ──
  interface DebugEventEntry {
    timestamp: string;
    event_type: string;
    session_id: string;
    trace_id?: string;
    channel_type?: string;
    payload?: any;
  }

  // ── Metrics ──
  interface MetricsSnapshot {
    queue_depth: { high: number; normal: number; low: number };
    backpressure_level: string;
    throughput: number;
    dlq_depth: number;
    inflight_pipelines: number;
    inflight_skills: number;
  }

  // ── DLQ ──
  interface DlqEntry {
    id: string;
    event_source: string;
    event_type: string;
    reason: string;
    retry_count: number;
    enqueued_at_ms: number;
  }

  // ── State ──
  let eventLog = $state<DebugEventEntry[]>([]);
  let metrics = $state<MetricsSnapshot | null>(null);
  let showEventLog = $state(true);
  let showMetricsPanel = $state(true);
  let showEventTools = $state(false);
  let showDlqPanel = $state(false);
  let selectedIdx = $state<number | null>(null);
  let unlisteners: (() => void)[] = [];

  const MAX_EVENT_LOG = 100;

  // ── Event Tools state ──
  let injectSource = $state("tauri:inject");
  let injectEventType = $state("custom");
  let injectPayload = $state('{"hello": "world"}');
  let injectResult = $state("");
  let traceId = $state("");
  let traceResult = $state("");
  let traceError = $state("");

  // ── DLQ state ──
  let dlqEntries = $state<DlqEntry[]>([]);
  let dlqLoading = $state(false);
  let dlqResult = $state("");

  // ── Helpers ──
  function formatTime(ts: string): string {
    try { return ts.slice(11, 23); } catch { return ts; }
  }

  function fmtDlqTime(ms: number): string {
    if (ms === 0) return "-";
    return new Date(ms).toLocaleString();
  }

  function eventTypeClass(et: string): string {
    if (et.startsWith("llm_") || et.startsWith("llm:")) return "llm";
    if (et.startsWith("tool_") || et.startsWith("TOOL_")) return "tool";
    if (et.startsWith("message")) return "msg";
    if (et.startsWith("capability") || et.startsWith("CAPABILITY")) return "cap";
    if (et.startsWith("output_blocked") || et.startsWith("security")) return "warn";
    if (et.includes("ERROR") || et.includes("error")) return "error";
    return "";
  }

  function toEntry(raw: any): DebugEventEntry {
    return {
      timestamp: new Date().toISOString(),
      event_type: raw.event_type ?? "unknown",
      session_id: raw.payload?.session_id ?? "",
      trace_id: raw.trace_id ?? raw.payload?.trace_id,
      channel_type: raw.payload?.channel_type,
      payload: raw.payload,
    };
  }

  // ── Event Log ──
  async function fetchDebugEvents() {
    try {
      const events: any[] = await invoke("get_debug_events", { limit: 50 });
      const entries = events.map(toEntry);
      eventLog = [...entries, ...eventLog].slice(-MAX_EVENT_LOG);
    } catch { /* runtime not running */ }
  }

  function handleEventProcessed(e: any) {
    eventLog = [...eventLog, toEntry(e.payload)].slice(-MAX_EVENT_LOG);
  }

  function handleMetricsUpdated(e: any) {
    metrics = e.payload as MetricsSnapshot;
  }

  function exportLog() {
    const data = JSON.stringify({ events: eventLog, metrics }, null, 2);
    try {
      const blob = new Blob([data], { type: "application/json" });
      const url = URL.createObjectURL(blob);
      const a = document.createElement("a");
      a.href = url;
      a.download = `debug-events-${Date.now()}.json`;
      a.click();
      URL.revokeObjectURL(url);
    } catch {
      navigator.clipboard.writeText(data);
    }
  }

  function clearLog() {
    eventLog = [];
    selectedIdx = null;
  }

  // ── Event Tools ──
  async function doInject() {
    injectResult = "";
    try {
      const parsed = JSON.parse(injectPayload);
      const id = await invoke<string>("inject_event", {
        source: injectSource,
        eventType: injectEventType,
        payload: parsed,
      });
      injectResult = `Event injected: ${id}`;
    } catch (e: any) {
      injectResult = `Error: ${e}`;
    }
  }

  async function searchTrace() {
    traceResult = "";
    traceError = "";
    try {
      const events = await invoke("get_event_trace", { traceId });
      traceResult = JSON.stringify(events, null, 2);
    } catch (e: any) {
      traceError = String(e);
    }
  }

  // ── DLQ ──
  async function loadDlq() {
    dlqLoading = true;
    try {
      dlqEntries = await invoke<DlqEntry[]>("list_dlq");
    } catch (e: any) {
      dlqResult = String(e);
    } finally {
      dlqLoading = false;
    }
  }

  async function doRetry(id: string) {
    try {
      dlqResult = await invoke<string>("retry_dlq", { id });
      await loadDlq();
    } catch (e: any) {
      dlqResult = String(e);
    }
  }

  async function doDiscard(id: string) {
    try {
      dlqResult = await invoke<string>("discard_dlq", { id });
      await loadDlq();
    } catch (e: any) {
      dlqResult = String(e);
    }
  }

  // ── Lifecycle ──
  onMount(async () => {
    await fetchDebugEvents();
    unlisteners.push(await listen("event:processed", handleEventProcessed));
    unlisteners.push(await listen("metrics:updated", handleMetricsUpdated));
    loadDlq();
  });

  onDestroy(() => {
    for (const fn of unlisteners) fn();
  });
</script>

<div class="maintenance">
  <!-- ═══ Event Bus Metrics ═══ -->
  <section class="dp-section">
    <div class="dp-section-header" role="button" tabindex="0" onclick={() => showMetricsPanel = !showMetricsPanel} onkeydown={(e) => e.key === 'Enter' && (showMetricsPanel = !showMetricsPanel)}>
      <span>{showMetricsPanel ? "▼" : "▶"} {t("maintenance.event_bus")}</span>
      <span class="dp-header-right" onclick={(e) => e.stopPropagation()}>
        <button class="dp-btn" onclick={fetchDebugEvents} title="Backfill from EventStore">{t("maintenance.refresh")}</button>
        <button class="dp-btn" onclick={exportLog} title="Export as JSON">{t("maintenance.export")}</button>
        <button class="dp-btn" onclick={clearLog} title="Clear event log">{t("maintenance.clear")}</button>
      </span>
    </div>
    {#if showMetricsPanel}
      <div class="dp-section-body">
        {#if metrics}
          <div class="dp-metrics-grid">
            <div class="dp-metric">
              <span class="dp-metric-label">Backpressure</span>
              <span class="dp-metric-value" class:bp-critical={metrics.backpressure_level.includes("CRITICAL") || metrics.backpressure_level.includes("L4")}
                    class:bp-warn={metrics.backpressure_level.includes("L3")}>
                {metrics.backpressure_level}
              </span>
            </div>
            <div class="dp-metric">
              <span class="dp-metric-label">Queue</span>
              <span class="dp-metric-value">{metrics.queue_depth.high + metrics.queue_depth.normal + metrics.queue_depth.low}</span>
            </div>
            <div class="dp-metric">
              <span class="dp-metric-label">Throughput</span>
              <span class="dp-metric-value">{metrics.throughput}</span>
            </div>
            <div class="dp-metric">
              <span class="dp-metric-label">DLQ</span>
              <span class="dp-metric-value" class:bp-warn={metrics.dlq_depth > 0}>{metrics.dlq_depth}</span>
            </div>
            <div class="dp-metric">
              <span class="dp-metric-label">Pipelines</span>
              <span class="dp-metric-value">{metrics.inflight_pipelines}</span>
            </div>
            <div class="dp-metric">
              <span class="dp-metric-label">Skills</span>
              <span class="dp-metric-value">{metrics.inflight_skills}</span>
            </div>
          </div>
        {:else}
          <p class="dp-empty">{t("maintenance.waiting_metrics")}</p>
        {/if}
      </div>
    {/if}
  </section>

  <!-- ═══ Event Log ═══ -->
  <section class="dp-section">
    <div class="dp-section-header" role="button" tabindex="0" onclick={() => showEventLog = !showEventLog} onkeydown={(e) => e.key === 'Enter' && (showEventLog = !showEventLog)}>
      <span>{showEventLog ? "▼" : "▶"} {t("maintenance.event_log")} <span class="dp-count">({eventLog.length})</span></span>
    </div>
    {#if showEventLog}
      <div class="dp-event-log">
        {#if eventLog.length === 0}
          <p class="dp-empty">{t("maintenance.no_events")}</p>
        {:else}
          {#each eventLog as entry, i}
            <!-- svelte-ignore a11y_interactive_supports_focus a11y_click_events_have_key_events -->
            <div class="dp-event-row" class:even={i % 2 === 0} class:selected={selectedIdx === i}
                 onclick={() => selectedIdx = selectedIdx === i ? null : i}
                 onkeydown={() => {}} role="button" tabindex="0">
              <span class="dp-event-time">{formatTime(entry.timestamp)}</span>
              <span class="dp-event-type {eventTypeClass(entry.event_type)}">
                {entry.event_type}{entry.payload?.kind ? ":" + entry.payload.kind : ""}
              </span>
              {#if entry.channel_type}
                <span class="dp-channel-tag">{entry.channel_type}</span>
              {/if}
              {#if entry.session_id}
                <span class="dp-event-sid" title="session_id">{entry.session_id.slice(0, 8)}</span>
              {/if}
              {#if entry.trace_id}
                <span class="dp-event-trace" title="trace_id">#{entry.trace_id.slice(0, 12)}</span>
              {/if}
            </div>
            {#if selectedIdx === i && entry.payload}
              <div class="dp-event-detail" class:even={i % 2 === 0}>
                <pre class="dp-event-payload">{JSON.stringify(entry.payload, null, 2)}</pre>
              </div>
            {/if}
          {/each}
        {/if}
      </div>
    {/if}
  </section>

  <!-- ═══ Event Tools (was EventViewer) ═══ -->
  <section class="dp-section">
    <div class="dp-section-header" role="button" tabindex="0" onclick={() => showEventTools = !showEventTools} onkeydown={(e) => e.key === 'Enter' && (showEventTools = !showEventTools)}>
      <span>{showEventTools ? "▼" : "▶"} Event Tools</span>
    </div>
    {#if showEventTools}
      <div class="dp-section-body">
        <div class="grid-2">
          <div>
            <h3 class="sub-heading">Inject Event</h3>
            <div style="display:flex;flex-direction:column;gap:8px;">
              <input type="text" bind:value={injectSource} placeholder="Source (e.g. tauri:inject)" />
              <input type="text" bind:value={injectEventType} placeholder="Event type (e.g. custom)" />
              <textarea rows={4} bind:value={injectPayload} placeholder='JSON payload'></textarea>
              <button onclick={doInject}>Inject</button>
              {#if injectResult}
                <p class="action-result">{injectResult}</p>
              {/if}
            </div>
          </div>
          <div>
            <h3 class="sub-heading">Trace Lookup</h3>
            <div style="display:flex;flex-direction:column;gap:8px;">
              <input type="text" bind:value={traceId} placeholder="Trace ID" />
              <button onclick={searchTrace}>Search</button>
              {#if traceError}
                <p class="action-result error">{traceError}</p>
              {/if}
              {#if traceResult}
                <textarea rows={8} value={traceResult} readonly></textarea>
              {/if}
            </div>
          </div>
        </div>
      </div>
    {/if}
  </section>

  <!-- ═══ DLQ (was DLQ.svelte) ═══ -->
  <section class="dp-section">
    <div class="dp-section-header" role="button" tabindex="0" onclick={() => showDlqPanel = !showDlqPanel} onkeydown={(e) => e.key === 'Enter' && (showDlqPanel = !showDlqPanel)}>
      <span>{showDlqPanel ? "▼" : "▶"} Dead Letter Queue <span class="dp-count">({dlqEntries.length})</span></span>
      <span class="dp-header-right" role="button" onclick={(e) => e.stopPropagation()}>
        <button class="dp-btn" onclick={loadDlq} disabled={dlqLoading}>Refresh</button>
      </span>
    </div>
    {#if showDlqPanel}
      <div class="dp-section-body">
        {#if dlqResult}
          <p class="action-result">{dlqResult}</p>
        {/if}
        {#if dlqEntries.length === 0}
          <p class="dp-empty">No entries in the dead letter queue.</p>
        {:else}
          <table>
            <thead>
              <tr><th>ID</th><th>Source</th><th>Type</th><th>Reason</th><th>Retries</th><th>Enqueued</th><th>Actions</th></tr>
            </thead>
            <tbody>
              {#each dlqEntries as e}
                <tr>
                  <td style="font-family:monospace;font-size:12px;">{e.id.slice(0, 8)}…</td>
                  <td>{e.event_source}</td>
                  <td><span class="badge warn">{e.event_type}</span></td>
                  <td style="color:var(--fg-dim);font-size:12px;max-width:200px;overflow:hidden;text-overflow:ellipsis;white-space:nowrap;">{e.reason}</td>
                  <td>{e.retry_count}</td>
                  <td style="font-size:11px;color:var(--fg-dim);white-space:nowrap;">{fmtDlqTime(e.enqueued_at_ms)}</td>
                  <td>
                    <button style="margin-right:4px;" onclick={() => doRetry(e.id)}>Retry</button>
                    <button class="danger" onclick={() => doDiscard(e.id)}>Discard</button>
                  </td>
                </tr>
              {/each}
            </tbody>
          </table>
        {/if}
      </div>
    {/if}
  </section>
</div>

<style>
  .maintenance {
    padding: 0;
    font-family: var(--font-mono);
    font-size: 12px;
  }

  /* ── section ── */
  .dp-section {
    border: 1px solid var(--border);
    border-radius: 8px;
    margin-bottom: 10px;
    overflow: hidden;
  }

  .dp-section-header {
    width: 100%;
    padding: 10px 14px;
    border: none;
    background: var(--bg-card);
    color: var(--fg);
    font-size: 12px;
    font-weight: 600;
    cursor: pointer;
    text-align: left;
    font-family: inherit;
    display: flex;
    justify-content: space-between;
    align-items: center;
    transition: background 0.15s;
  }

  .dp-section-header:hover {
    background: var(--bg-hover);
  }

  .dp-header-right {
    display: flex;
    gap: 6px;
  }

  .dp-section-body {
    padding: 12px 14px;
    background: var(--bg-card);
  }

  .dp-count {
    color: var(--fg-dim);
    font-weight: 400;
    font-size: 11px;
  }

  /* ── buttons ── */
  .dp-btn {
    padding: 3px 10px;
    border: 1px solid var(--border);
    border-radius: 4px;
    background: transparent;
    color: var(--fg-dim);
    font-size: 11px;
    font-family: inherit;
    cursor: pointer;
    transition: background 0.15s, color 0.15s;
  }

  .dp-btn:hover {
    background: var(--bg-hover);
    color: var(--fg);
  }

  .dp-btn:disabled {
    opacity: 0.4;
    cursor: not-allowed;
  }

  /* ── metrics grid ── */
  .dp-metrics-grid {
    display: grid;
    grid-template-columns: repeat(3, 1fr);
    gap: 12px;
  }

  .dp-metric {
    display: flex;
    flex-direction: column;
    gap: 2px;
  }

  .dp-metric-label {
    font-size: 10px;
    color: var(--fg-dim);
    text-transform: uppercase;
    letter-spacing: 0.3px;
  }

  .dp-metric-value {
    font-size: 22px;
    font-weight: 700;
    color: var(--accent);
  }

  .dp-metric-value.bp-critical {
    color: var(--red);
  }

  .dp-metric-value.bp-warn {
    color: var(--yellow);
  }

  /* ── event log ── */
  .dp-event-log {
    max-height: 500px;
    overflow-y: auto;
    background: var(--bg-card);
  }

  .dp-empty {
    color: var(--fg-dim);
    font-style: italic;
    padding: 16px 14px;
    text-align: center;
    font-size: 12px;
  }

  .dp-event-row {
    display: flex;
    gap: 8px;
    align-items: center;
    padding: 3px 14px;
    font-size: 11px;
    line-height: 1.8;
    cursor: pointer;
    transition: background 0.1s;
  }

  .dp-event-row:hover {
    background: var(--bg-hover);
  }

  .dp-event-row.selected {
    background: var(--accent-muted);
    border-left: 2px solid var(--accent);
    padding-left: 12px;
  }

  .dp-event-detail {
    padding: 0 14px 6px;
  }

  .dp-event-payload {
    margin: 0;
    padding: 8px 10px;
    background: rgba(0, 0, 0, 0.15);
    border-radius: 4px;
    font-size: 10px;
    line-height: 1.5;
    color: var(--fg-dim);
    overflow-x: auto;
    white-space: pre-wrap;
    word-break: break-all;
  }

  .dp-event-time {
    color: var(--fg-dim);
    flex-shrink: 0;
    width: 80px;
    font-size: 10px;
  }

  .dp-event-type {
    color: #93c5fd;
    font-weight: 500;
    flex-shrink: 0;
  }

  .dp-event-type.llm  { color: #a78bfa; }
  .dp-event-type.tool { color: #34d399; }
  .dp-event-type.msg  { color: #fbbf24; }
  .dp-event-type.cap  { color: #60a5fa; }
  .dp-event-type.warn { color: #fb923c; }
  .dp-event-type.error { color: var(--red); }

  .dp-channel-tag {
    font-size: 9px;
    padding: 1px 5px;
    border-radius: 3px;
    background: var(--bg-hover);
    color: var(--fg-dim);
    text-transform: uppercase;
    flex-shrink: 0;
  }

  .dp-event-sid {
    color: var(--fg-dim);
    flex-shrink: 0;
    font-size: 10px;
  }

  .dp-event-trace {
    color: var(--fg-dim);
    font-size: 10px;
    flex-shrink: 0;
  }

  /* ── event tools ── */
  .grid-2 {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 16px;
  }

  .sub-heading {
    font-size: 13px;
    font-weight: 600;
    margin: 0 0 8px 0;
    color: var(--fg);
    font-family: var(--font-ui);
  }

  input, textarea {
    width: 100%;
    padding: 8px 10px;
    border: 1px solid var(--border);
    border-radius: 6px;
    background: var(--bg-input);
    color: var(--fg);
    font-size: 12px;
    font-family: "SF Mono", monospace;
    resize: vertical;
  }

  textarea[readonly] {
    background: var(--bg-card);
    color: var(--fg-dim);
  }

  .action-result {
    font-size: 12px;
    margin-top: 4px;
    color: var(--accent);
  }

  .action-result.error {
    color: var(--red);
  }

  /* ── DLQ table ── */
  table {
    width: 100%;
    border-collapse: collapse;
    font-size: 12px;
  }

  th {
    text-align: left;
    padding: 6px 8px;
    font-size: 10px;
    text-transform: uppercase;
    letter-spacing: 0.3px;
    color: var(--fg-dim);
    border-bottom: 1px solid var(--border);
  }

  td {
    padding: 6px 8px;
    border-bottom: 1px solid var(--border);
    font-size: 12px;
  }

  tr:last-child td {
    border-bottom: none;
  }

  .badge {
    display: inline-block;
    padding: 1px 6px;
    border-radius: 3px;
    font-size: 10px;
    font-weight: 600;
  }

  .badge.warn {
    background: var(--yellow-muted);
    color: var(--yellow);
  }

  button {
    padding: 6px 14px;
    border: 1px solid var(--border);
    border-radius: 6px;
    background: var(--bg-card);
    color: var(--fg);
    font-size: 12px;
    font-family: var(--font-ui);
    cursor: pointer;
    transition: background 0.15s;
  }

  button:hover {
    background: var(--bg-hover);
  }

  button.danger {
    border-color: color-mix(in srgb, var(--red) 30%, transparent);
    color: var(--red);
  }

  button.danger:hover {
    background: var(--red-muted);
  }

  button:disabled {
    opacity: 0.4;
    cursor: not-allowed;
  }
</style>
