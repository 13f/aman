<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";

  let traceId = $state("");
  let traceResult = $state("");
  let error = $state("");

  let source = $state("tauri:inject");
  let eventType = $state("custom");
  let payload = $state('{"hello": "world"}');
  let injectResult = $state("");

  async function searchTrace() {
    traceResult = "";
    error = "";
    try {
      const events = await invoke("get_event_trace", { traceId });
      traceResult = JSON.stringify(events, null, 2);
    } catch (e: any) {
      error = String(e);
    }
  }

  async function doInject() {
    injectResult = "";
    try {
      const parsed = JSON.parse(payload);
      const id = await invoke<string>("inject_event", {
        source,
        eventType,
        payload: parsed,
      });
      injectResult = `Event injected: ${id}`;
    } catch (e: any) {
      injectResult = `Error: ${e}`;
    }
  }
</script>

<div class="grid-2">
  <div class="card">
    <h2>Inject Event</h2>
    <div style="display:flex;flex-direction:column;gap:10px;">
      <input type="text" bind:value={source} placeholder="Source (e.g. tauri:inject)" />
      <input type="text" bind:value={eventType} placeholder="Event type (e.g. custom)" />
      <textarea rows={6} bind:value={payload} placeholder='JSON payload'></textarea>
      <button onclick={doInject}>Inject</button>
      {#if injectResult}
        <p style="font-size:13px;color:var(--accent);margin-top:4px;">{injectResult}</p>
      {/if}
    </div>
  </div>

  <div class="card">
    <h2>Trace Lookup</h2>
    <div style="display:flex;flex-direction:column;gap:10px;">
      <input type="text" bind:value={traceId} placeholder="Trace ID" />
      <button onclick={searchTrace}>Search</button>
      {#if error}
        <p style="font-size:13px;color:var(--red);">{error}</p>
      {/if}
      {#if traceResult}
        <textarea rows={10} value={traceResult} readonly></textarea>
      {/if}
    </div>
  </div>
</div>
