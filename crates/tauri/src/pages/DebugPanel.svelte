<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import { listen } from "@tauri-apps/api/event";
  import { onMount, onDestroy } from "svelte";

  interface DebugEventEntry {
    timestamp: string;
    event_type: string;
    session_id: string;
    trace_id?: string;
    channel_type?: string;
  }

  interface MetricsSnapshot {
    queue_depth: { high: number; normal: number; low: number };
    backpressure_level: string;
    throughput: number;
    dlq_depth: number;
    inflight_pipelines: number;
    inflight_skills: number;
  }

  let { visible = false }: { visible?: boolean } = $props();

  let eventLog = $state<DebugEventEntry[]>([]);
  let metrics = $state<MetricsSnapshot | null>(null);
  let showEventLog = $state(true);
  let showMetricsPanel = $state(true);
  let unlisteners: (() => void)[] = [];

  const MAX_EVENT_LOG = 100;

  function formatTime(ts: string): string {
    try {
      return ts.slice(11, 23); // HH:mm:ss.fff
    } catch {
      return ts;
    }
  }

  function eventTypeClass(et: string): string {
    if (et.startsWith("llm_")) return "llm";
    if (et.startsWith("tool_") || et.startsWith("TOOL_")) return "tool";
    if (et.startsWith("message")) return "msg";
    if (et.startsWith("capability") || et.startsWith("CAPABILITY")) return "cap";
    if (et.startsWith("output_blocked") || et.startsWith("security")) return "warn";
    if (et.includes("ERROR") || et.includes("error")) return "error";
    return "";
  }

  function toEntry(payload: any): DebugEventEntry {
    return {
      timestamp: new Date().toISOString(),
      event_type: payload.event_type ?? "unknown",
      session_id: payload.payload?.session_id ?? "",
      trace_id: payload.trace_id ?? payload.payload?.trace_id,
      channel_type: payload.payload?.channel_type,
    };
  }

  async function fetchDebugEvents() {
    try {
      const events: any[] = await invoke("get_debug_events", { limit: 50 });
      const entries = events.map(toEntry);
      eventLog = [...entries, ...eventLog].slice(-MAX_EVENT_LOG);
    } catch {
      // no runtime running, ignore
    }
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
      // Copy to clipboard as a simple export mechanism
      const blob = new Blob([data], { type: "application/json" });
      const url = URL.createObjectURL(blob);
      const a = document.createElement("a");
      a.href = url;
      a.download = `debug-events-${Date.now()}.json`;
      a.click();
      URL.revokeObjectURL(url);
    } catch {
      // Fallback: write to clipboard
      navigator.clipboard.writeText(data);
    }
  }

  function clearLog() {
    eventLog = [];
  }

  onMount(async () => {
    await fetchDebugEvents();
    unlisteners.push(await listen("event:processed", handleEventProcessed));
    unlisteners.push(await listen("metrics:updated", handleMetricsUpdated));
  });

  onDestroy(() => {
    for (const fn of unlisteners) fn();
  });
</script>

{#if visible}
  <div class="debug-panel-overlay" onclick={() => visible = false} role="presentation">
    <!-- svelte-ignore a11y_interactive_supports_focus a11y_click_events_have_key_events -->
    <div class="debug-panel" onclick={(e) => e.stopPropagation()} onkeydown={() => {}} role="dialog" aria-label="Debug Panel">
      <header class="dp-header">
        <h3>&#x2699; Debug Panel</h3>
        <div class="dp-header-actions">
          <button class="dp-btn" onclick={fetchDebugEvents} title="Backfill from EventStore">Refresh</button>
          <button class="dp-btn" onclick={exportLog} title="Export as JSON">Export</button>
          <button class="dp-btn" onclick={clearLog} title="Clear event log">Clear</button>
          <button class="dp-btn dp-close" onclick={() => visible = false} title="Close">&times;</button>
        </div>
      </header>

      <div class="dp-body">
        <!-- Event Bus Metrics -->
        <section class="dp-section">
          <button class="dp-section-header" onclick={() => showMetricsPanel = !showMetricsPanel}>
            <span>{showMetricsPanel ? "&#9660;" : "&#9654;"} Event Bus</span>
          </button>
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
                    <span class="dp-metric-value">{metrics.dlq_depth}</span>
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
                <p class="dp-empty">Waiting for metrics...</p>
              {/if}
            </div>
          {/if}
        </section>

        <!-- Event Log -->
        <section class="dp-section">
          <button class="dp-section-header" onclick={() => showEventLog = !showEventLog}>
            <span>{showEventLog ? "&#9660;" : "&#9654;"} Event Log <span class="dp-count">({eventLog.length})</span></span>
          </button>
          {#if showEventLog}
            <div class="dp-event-log">
              {#if eventLog.length === 0}
                <p class="dp-empty">No events yet.</p>
              {:else}
                {#each eventLog as entry, i}
                  <div class="dp-event-row" class:even={i % 2 === 0}>
                    <span class="dp-event-time">{formatTime(entry.timestamp)}</span>
                    <span class="dp-event-type {eventTypeClass(entry.event_type)}">{entry.event_type}</span>
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
                {/each}
              {/if}
            </div>
          {/if}
        </section>
      </div>
    </div>
  </div>
{/if}

<style>
  .debug-panel-overlay {
    position: fixed;
    inset: 0;
    z-index: 1000;
    background: rgba(0, 0, 0, 0.3);
    display: flex;
    justify-content: flex-end;
  }

  .debug-panel {
    width: 480px;
    max-width: 90vw;
    height: 100%;
    background: #1a1a2e;
    color: #e0e0e0;
    font-family: "SF Mono", "Fira Code", "Cascadia Code", monospace;
    font-size: 12px;
    display: flex;
    flex-direction: column;
    box-shadow: -4px 0 20px rgba(0, 0, 0, 0.4);
  }

  .dp-header {
    display: flex;
    justify-content: space-between;
    align-items: center;
    padding: 10px 14px;
    border-bottom: 1px solid #333;
    background: #16162a;
    flex-shrink: 0;
  }

  .dp-header h3 {
    margin: 0;
    font-size: 13px;
    font-weight: 600;
    color: #fff;
  }

  .dp-header-actions {
    display: flex;
    gap: 6px;
  }

  .dp-btn {
    padding: 3px 10px;
    border: 1px solid #444;
    border-radius: 4px;
    background: transparent;
    color: #ccc;
    font-size: 11px;
    font-family: inherit;
    cursor: pointer;
  }

  .dp-btn:hover {
    background: #333;
    color: #fff;
  }

  .dp-close {
    font-size: 16px;
    padding: 1px 8px;
    line-height: 1;
  }

  .dp-body {
    flex: 1;
    overflow-y: auto;
    padding: 0;
  }

  .dp-section {
    border-bottom: 1px solid #2a2a3e;
  }

  .dp-section-header {
    width: 100%;
    padding: 8px 14px;
    border: none;
    background: #1e1e34;
    color: #aaa;
    font-size: 11px;
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.5px;
    cursor: pointer;
    text-align: left;
    font-family: inherit;
    display: flex;
    justify-content: space-between;
  }

  .dp-section-header:hover {
    background: #252542;
  }

  .dp-section-body {
    padding: 10px 14px;
  }

  .dp-metrics-grid {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 8px;
  }

  .dp-metric {
    display: flex;
    flex-direction: column;
    gap: 2px;
  }

  .dp-metric-label {
    font-size: 10px;
    color: #888;
    text-transform: uppercase;
    letter-spacing: 0.3px;
  }

  .dp-metric-value {
    font-size: 16px;
    font-weight: 700;
    color: #4ade80;
  }

  .dp-metric-value.bp-critical {
    color: #ef4444;
  }

  .dp-metric-value.bp-warn {
    color: #f59e0b;
  }

  .dp-count {
    color: #666;
    font-weight: 400;
  }

  .dp-empty {
    color: #666;
    font-style: italic;
    padding: 12px;
    text-align: center;
  }

  .dp-event-log {
    max-height: 400px;
    overflow-y: auto;
  }

  .dp-event-row {
    display: flex;
    gap: 8px;
    align-items: center;
    padding: 3px 14px;
    font-size: 11px;
    line-height: 1.6;
  }

  .dp-event-row.even {
    background: rgba(255, 255, 255, 0.02);
  }

  .dp-event-time {
    color: #666;
    flex-shrink: 0;
    width: 80px;
  }

  .dp-event-type {
    color: #93c5fd;
    font-weight: 500;
    flex-shrink: 0;
  }

  .dp-event-type.llm {
    color: #a78bfa;
  }

  .dp-event-type.tool {
    color: #34d399;
  }

  .dp-event-type.msg {
    color: #fbbf24;
  }

  .dp-event-type.cap {
    color: #60a5fa;
  }

  .dp-event-type.warn {
    color: #fb923c;
  }

  .dp-event-type.error {
    color: #ef4444;
  }

  .dp-channel-tag {
    font-size: 9px;
    padding: 1px 5px;
    border-radius: 3px;
    background: #2a2a4a;
    color: #888;
    text-transform: uppercase;
    flex-shrink: 0;
  }

  .dp-event-sid {
    color: #555;
    flex-shrink: 0;
  }

  .dp-event-trace {
    color: #555;
    font-size: 10px;
    flex-shrink: 0;
  }
</style>
