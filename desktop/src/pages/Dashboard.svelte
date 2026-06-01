<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import { listen } from "@tauri-apps/api/event";
  import { onMount, onDestroy } from "svelte";
  import NotificationBell from "./NotificationBell.svelte";

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
  let gatewayLoading = $state(false);
  let gatewayStopping = $state(false);
  let gatewayError = $state("");
  let gatewayPort = $state(9999);
  let unlisteners: (() => void)[] = [];

  async function refreshStatus() {
    try {
      status = await invoke<RuntimeStatus>("get_runtime_status");
      gatewayError = "";
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

  async function startGateway() {
    gatewayLoading = true;
    gatewayError = "";
    try {
      const msg = await invoke<string>("start_runtime", {
        gatewayUrl: `http://127.0.0.1:${gatewayPort}`,
      });
      console.log(msg);
      await refreshStatus();
      await refreshConfig();
    } catch (e: any) {
      gatewayError = String(e);
    } finally {
      gatewayLoading = false;
    }
  }

  async function stopGateway() {
    gatewayStopping = true;
    gatewayError = "";
    try {
      const msg = await invoke<string>("stop_runtime");
      console.log(msg);
      await refreshStatus();
      await refreshConfig();
    } catch (e: any) {
      gatewayError = String(e);
    } finally {
      gatewayStopping = false;
    }
  }

  async function restartGateway() {
    await stopGateway();
    if (!gatewayError) {
      await startGateway();
    }
  }

  // Reset metrics display when the gateway is not running so stale values
  // don't linger on the dashboard after a stop/restart cycle.
  $effect(() => {
    if (!status.running) {
      metrics = null;
    }
  });

  interface RuntimeStatusEvent {
    phase: number;
    ready: boolean;
    live: boolean;
  }

  onMount(async () => {
    await refreshStatus();
    try {
      gatewayPort = await invoke<number>("get_gateway_port");
    } catch { /* use default */ }
    listen<MetricsSnapshot>("metrics:updated", (e) => {
      metrics = e.payload;
    }).then((fn) => { unlisteners.push(fn); });
    listen<RuntimeStatusEvent>("runtime:updated", (e) => {
      const p = e.payload;
      status = {
        phase: `Phase${p.phase}`,
        ready: p.ready,
        live: p.live,
        running: p.phase > 0,
      };
      onstatuschange(status.running);
    }).then((fn) => { unlisteners.push(fn); });
  });

  onDestroy(() => {
    for (const fn of unlisteners) fn();
  });
</script>

<div class="status-bar">
  <div class="status-info">
    <h2>Gateway Status</h2>
    <p class="dim" style="margin-top:4px;">
      Phase: <strong>{status.phase}</strong>
      <span class="sep"></span>
      Ready: <span class="badge {status.ready ? 'ok' : 'warn'}">{status.ready ? "YES" : "NO"}</span>
      <span class="sep"></span>
      Live: <span class="badge {status.live ? 'ok' : 'error'}">{status.live ? "YES" : "NO"}</span>
    </p>
  </div>
  <div class="status-actions">
    <NotificationBell />
    {#if !status.running}
      <button class="start-btn" onclick={startGateway} disabled={gatewayLoading}>
        {gatewayLoading ? "连接中..." : "启动"}
      </button>
    {:else}
      <button class="stop-btn" onclick={stopGateway} disabled={gatewayStopping}>
        {gatewayStopping ? "停止中..." : "停止"}
      </button>
      <button class="restart-btn" onclick={restartGateway} disabled={gatewayStopping || gatewayLoading}>
        重启
      </button>
    {/if}
  </div>
</div>
{#if gatewayError}
  <div class="card error-card">
    <p style="color:var(--red);font-size:13px;">{gatewayError}</p>
  </div>
{/if}

<!-- Runtime config info -->
{#if config}
  <div class="card">
    <h2>Runtime Configuration</h2>
    <table>
      <tbody>
        <tr><td class="config-label">Runtime Dir</td><td class="mono">{config.runtime_dir ?? "N/A"}</td></tr>
        <tr><td class="config-label">Bind Address</td><td class="mono">{config.bind_addr ?? "N/A"}</td></tr>
        <tr><td class="config-label">Skills Dir</td><td class="mono">{config.skills_dir ?? "N/A"}</td></tr>
        <tr><td class="config-label">API Token</td><td><span class="badge {config.has_api_token ? 'ok' : 'warn'}">{config.has_api_token ? "Configured" : "None"}</span></td></tr>
        <tr><td class="config-label">Debug Endpoints</td><td><span class="badge {config.risky_enabled ? 'warn' : 'ok'}">{config.risky_enabled ? "Enabled" : "Disabled"}</span></td></tr>
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
  <p class="dim" style="padding: 20px 0;">Waiting for metrics data...</p>
{/if}

<style>
  .status-bar {
    display: flex;
    align-items: center;
    justify-content: space-between;
    background: var(--bg-card);
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
    padding: 20px;
    margin-bottom: 16px;
  }
  .status-info h2 {
    font-size: 14px;
    font-weight: 600;
    margin-bottom: 4px;
  }
  .status-actions {
    display: flex;
    align-items: center;
    gap: 10px;
    flex-shrink: 0;
  }
  .sep {
    display: inline-block;
    width: 1px;
    height: 10px;
    background: var(--border-strong);
    margin: 0 6px;
    vertical-align: middle;
  }
  .error-card {
    border-color: var(--red);
  }
  .config-label {
    color: var(--fg-dim);
    width: 140px;
    font-size: 13px;
  }
</style>
