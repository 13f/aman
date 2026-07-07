<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import { t } from "../lib/i18n.svelte";

  interface PluginEntry {
    name: string;
    version: string | null;
    loaded: boolean;
    state: string | null;
    enabled: boolean;
  }

  let plugins = $state<PluginEntry[]>([]);
  let loading = $state(false);
  let result = $state("");
  let autoRefresh = $state(false);
  let autoTimer: ReturnType<typeof setInterval> | undefined;

  function toggleAuto() {
    autoRefresh = !autoRefresh;
    if (autoRefresh) {
      loadPlugins();
      autoTimer = setInterval(loadPlugins, 4000);
    } else {
      if (autoTimer) clearInterval(autoTimer);
      autoTimer = undefined;
    }
  }

  async function loadPlugins() {
    loading = true;
    try {
      plugins = await invoke<PluginEntry[]>("list_plugins");
    } catch (e: any) {
      if (!autoRefresh) result = String(e);
    } finally {
      loading = false;
    }
  }

  async function doEnable(name: string) {
    try {
      result = await invoke<string>("enable_plugin", { name });
      await loadPlugins();
    } catch (e: any) {
      result = String(e);
    }
  }

  async function doDisable(name: string) {
    try {
      result = await invoke<string>("disable_plugin", { name });
      await loadPlugins();
    } catch (e: any) {
      result = String(e);
    }
  }
</script>

<div class="card" style="display:flex;align-items:center;justify-content:space-between;">
  <h2>{t("plugin.title")}</h2>
  <div style="display:flex;gap:8px;align-items:center;">
    <label style="font-size:13px;display:flex;align-items:center;gap:4px;cursor:pointer;">
      <input type="checkbox" checked={autoRefresh} onchange={toggleAuto} />
      Auto
    </label>
    <button class="secondary" onclick={loadPlugins} disabled={loading}>{t("maintenance.refresh")}</button>
  </div>
</div>

{#if result}
  <div class="card">
    <p style="font-size:13px;color:var(--accent);">{result}</p>
  </div>
{/if}

<div class="card">
  {#if plugins.length === 0}
    <p style="color:var(--fg-dim);font-size:13px;">No plugins loaded. Click "Refresh" to check.</p>
  {:else}
    <table>
      <thead>
        <tr><th>Name</th><th>State</th><th>Actions</th></tr>
      </thead>
      <tbody>
        {#each plugins as p}
          <tr>
            <td><strong>{p.name}</strong></td>
            <td>
              <span class="badge {p.enabled ? 'ok' : 'warn'}">
                {p.enabled ? t("common.enabled") : t("common.disabled")}
              </span>
              {#if p.state}
                <span style="font-size:11px;color:var(--fg-dim);margin-left:8px;">({p.state})</span>
              {/if}
            </td>
            <td>
              {#if p.enabled}
                <button class="danger" onclick={() => doDisable(p.name)}>Disable</button>
              {:else}
                <button onclick={() => doEnable(p.name)}>Enable</button>
              {/if}
            </td>
          </tr>
        {/each}
      </tbody>
    </table>
  {/if}
</div>
