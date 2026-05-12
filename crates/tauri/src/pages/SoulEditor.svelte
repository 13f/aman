<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";

  interface SoulInfo {
    current_soul: string | null;
    last_changed: string | null;
  }

  let soulInfo = $state<SoulInfo | null>(null);
  let systemPrompt = $state("");
  let soulContent = $state("");
  let loading = $state(false);
  let saving = $state(false);
  let result = $state("");

  async function loadSoul() {
    loading = true;
    result = "";
    try {
      soulInfo = await invoke<SoulInfo>("get_soul_info");
      systemPrompt = await invoke<string>("preview_system_prompt");
      soulContent = await invoke<string>("get_soul_raw");
    } catch (e: any) {
      soulInfo = { current_soul: null, last_changed: null };
    } finally {
      loading = false;
    }
  }

  async function saveSoul() {
    saving = true;
    result = "";
    try {
      const msg = await invoke<string>("update_soul", { content: soulContent });
      result = msg;
      // Refresh preview after save
      systemPrompt = await invoke<string>("preview_system_prompt");
      soulInfo = await invoke<SoulInfo>("get_soul_info");
    } catch (e: any) {
      result = `Error: ${e}`;
    } finally {
      saving = false;
    }
  }
</script>

<div class="card" style="display:flex;align-items:center;justify-content:space-between;">
  <h2>SOUL (System & Operating Utility Language)</h2>
  <button class="secondary" onclick={loadSoul} disabled={loading}>Load</button>
</div>

{#if result}
  <div class="card">
    <p style="font-size:13px;color:var(--accent);">{result}</p>
  </div>
{/if}

{#if soulInfo?.current_soul}
  <div class="grid-2">
    <div class="card">
      <h2>Edit SOUL</h2>
      <div style="display:flex;flex-direction:column;gap:10px;">
        <textarea rows={20} bind:value={soulContent} style="font-family:monospace;font-size:12px;"></textarea>
        <button onclick={saveSoul} disabled={saving}>Save & Reload</button>
      </div>
    </div>
    <div>
      <div class="card">
        <h2>Soul Info</h2>
        <table>
          <tbody>
            <tr><td style="color:var(--fg-dim);width:120px;">Name</td><td><strong>{soulInfo.current_soul}</strong></td></tr>
            <tr><td style="color:var(--fg-dim);width:120px;">Last Changed</td><td style="font-family:monospace;font-size:12px;">{soulInfo.last_changed ?? "N/A"}</td></tr>
          </tbody>
        </table>
      </div>
      <div class="card">
        <h2>System Prompt Preview</h2>
        <textarea rows={12} value={systemPrompt} readonly style="font-family:monospace;font-size:12px;color:var(--fg-dim);"></textarea>
      </div>
    </div>
  </div>
{:else}
  <div class="card">
    <p style="color:var(--fg-dim);font-size:13px;">No SOUL configured for this runtime. Start the runtime with a SOUL file to enable this section.</p>
  </div>
{/if}
