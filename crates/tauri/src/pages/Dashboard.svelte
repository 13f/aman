<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import { listen } from "@tauri-apps/api/event";
  import { onMount, onDestroy } from "svelte";

  let { onstatuschange = (_running: boolean) => {} } = $props();

  interface MetricsSnapshot {
    queue_depth: { high: number; normal: number; low: number };
    throughput: number;
    discarded: number;
    duplicate: number;
    subscription_count: number;
    retry_queue_depth: number;
    dlq_depth: number;
    inflight_pipelines: number;
    inflight_skills: number;
    backpressure_level: string;
    plugin_health: { name: string; status: string }[];
  }

  interface RuntimeStatus {
    phase: string;
    ready: boolean;
    live: boolean;
    running: boolean;
  }

  interface RuntimeConfig {
    runtime_dir: string | null;
    bind_addr: string | null;
    has_api_token: boolean;
    risky_enabled: boolean;
    skills_dir: string | null;
  }

  let metrics = $state<MetricsSnapshot | null>(null);
  let status = $state<RuntimeStatus>({ phase: "stopped", ready: false, live: false, running: false });
  let config = $state<RuntimeConfig | null>(null);
  let unlisten: (() => void) | null = null;

  async function refreshStatus() {
    try {
      status = await invoke<RuntimeStatus>("get_runtime_status");
      onstatuschange(status.running);
    } catch {
      status = { phase: "stopped", ready: false, live: false, running: false };
      onstatuschange(false);
    }
  }

  async function refreshConfig() {
    try {
      config = await invoke<RuntimeConfig>("get_runtime_config");
    } catch {
      config = null;
    }
  }

  onMount(async () => {
    await refreshStatus();
    listen<MetricsSnapshot>("metrics:updated", (e) => {
      metrics = e.payload;
    }).then((fn) => { unlisten = fn; });
  });

  onDestroy(() => {
    if (unlisten) unlisten();
  });
</script>

<div class="card">
  <h2>Gateway Status</h2>
  <p style="color:var(--fg-dim);margin-top:4px;">
    Phase: <strong>{status.phase}</strong>
    &middot; Ready: <strong class="badge {status.ready ? 'ok' : 'warn'}">{status.ready ? "YES" : "NO"}</strong>
    &middot; Live: <strong class="badge {status.live ? 'ok' : 'error'}">{status.live ? "YES" : "NO"}</strong>
  </p>
</div>

<!-- Runtime config info -->
{#if config}
  <div class="card">
    <h2>Runtime Configuration</h2>
    <table>
      <tbody>
        <tr><td style="color:var(--fg-dim);width:140px;">Runtime Dir</td><td style="font-family:monospace;font-size:12px;">{config.runtime_dir ?? "N/A"}</td></tr>
        <tr><td style="color:var(--fg-dim);width:140px;">Bind Address</td><td style="font-family:monospace;font-size:12px;">{config.bind_addr ?? "N/A"}</td></tr>
        <tr><td style="color:var(--fg-dim);width:140px;">Skills Dir</td><td style="font-family:monospace;font-size:12px;">{config.skills_dir ?? "N/A"}</td></tr>
        <tr><td style="color:var(--fg-dim);width:140px;">API Token</td><td><span class="badge {config.has_api_token ? 'ok' : 'warn'}">{config.has_api_token ? "Configured" : "None"}</span></td></tr>
        <tr><td style="color:var(--fg-dim);width:140px;">Debug Endpoints</td><td><span class="badge {config.risky_enabled ? 'warn' : 'ok'}">{config.risky_enabled ? "Enabled" : "Disabled"}</span></td></tr>
      </tbody>
    </table>
  </div>
{/if}

{#if metrics}
  <div class="grid-4">
    <div class="card stat">
      <div class="value">{metrics.queue_depth.high + metrics.queue_depth.normal + metrics.queue_depth.low}</div>
      <div class="label">Total Queue Depth</div>
      <div style="font-size:11px;color:var(--fg-dim);margin-top:4px;">
        H:{metrics.queue_depth.high} N:{metrics.queue_depth.normal} L:{metrics.queue_depth.low}
      </div>
    </div>
    <div class="card stat">
      <div class="value">{metrics.throughput}</div>
      <div class="label">Event Throughput</div>
    </div>
    <div class="card stat">
      <div class="value">{metrics.inflight_pipelines}</div>
      <div class="label">Inflight Pipelines</div>
    </div>
    <div class="card stat">
      <div class="value">{metrics.inflight_skills}</div>
      <div class="label">Inflight Skills</div>
    </div>
  </div>

  <div class="grid-3">
    <div class="card stat">
      <div class="value" style="color:var(--green)">{metrics.retry_queue_depth}</div>
      <div class="label">Retry Queue</div>
    </div>
    <div class="card stat">
      <div class="value" style="color:var(--yellow)">{metrics.dlq_depth}</div>
      <div class="label">DLQ Depth</div>
    </div>
    <div class="card stat">
      <div class="value" style="font-size:20px;">{metrics.backpressure_level}</div>
      <div class="label">Backpressure</div>
    </div>
  </div>

  <div class="card">
    <h2>Plugin Health</h2>
    {#if metrics.plugin_health.length === 0}
      <p style="color:var(--fg-dim);font-size:13px;">No plugins loaded</p>
    {:else}
      <table>
        <thead><tr><th>Plugin</th><th>Status</th></tr></thead>
        <tbody>
          {#each metrics.plugin_health as p}
            <tr>
              <td>{p.name}</td>
              <td><span class="badge {p.status === 'Ok' || p.status === 'ok' ? 'ok' : 'warn'}">{p.status}</span></td>
            </tr>
          {/each}
        </tbody>
      </table>
    {/if}
  </div>
{:else if status.running}
  <p style="color:var(--fg-dim);">Waiting for metrics data...</p>
{/if}
