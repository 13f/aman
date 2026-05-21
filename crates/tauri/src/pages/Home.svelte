<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import { onMount } from "svelte";

  interface RuntimeStatus {
    phase: string;
    ready: boolean;
    live: boolean;
    running: boolean;
  }

  let status = $state<RuntimeStatus>({ phase: "stopped", ready: false, live: false, running: false });
  let gatewayPort = $state(9999);

  async function refreshStatus() {
    try {
      status = await invoke<RuntimeStatus>("get_runtime_status");
    } catch {
      status = { phase: "stopped", ready: false, live: false, running: false };
    }
  }

  onMount(async () => {
    await refreshStatus();
    try {
      gatewayPort = await invoke<number>("get_gateway_port");
    } catch { /* use default */ }
  });
</script>

<div class="card" style="text-align:center;padding:48px 20px;">
  <h1 style="font-size:24px;font-weight:700;margin-bottom:8px;">Welcome to Aman</h1>
  <p style="color:var(--fg-dim);font-size:14px;max-width:480px;margin:0 auto 24px;">
    Multi-agent framework with event-driven pipelines, tool orchestration, and LLM integration.
  </p>
  <div style="display:flex;justify-content:center;gap:24px;flex-wrap:wrap;">
    <div class="card stat" style="min-width:120px;">
      <div class="value" style="font-size:20px;color:{status.running ? 'var(--green)' : 'var(--fg-dim)'};">
        {status.running ? "Running" : "Stopped"}
      </div>
      <div class="label">Runtime</div>
    </div>
    <div class="card stat" style="min-width:120px;">
      <div class="value" style="font-size:20px;">{gatewayPort}</div>
      <div class="label">Gateway Port</div>
    </div>
    <div class="card stat" style="min-width:120px;">
      <div class="value" style="font-size:20px;color:{status.ready ? 'var(--green)' : 'var(--yellow)'};">
        {status.ready ? "Ready" : "N/A"}
      </div>
      <div class="label">Status</div>
    </div>
  </div>
</div>
