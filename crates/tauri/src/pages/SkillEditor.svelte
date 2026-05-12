<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import { listen } from "@tauri-apps/api/event";
  import { onMount, onDestroy } from "svelte";

  interface TriggerInfo {
    event_types: string[];
    sources: string[];
    priorities: string[];
    match_all: boolean;
  }

  interface SkillEntry {
    name: string;
    version: string;
    description: string;
    enabled: boolean;
    triggers: TriggerInfo[];
    concurrency: string;
  }

  let skills = $state<SkillEntry[]>([]);
  let loading = $state(false);
  let result = $state("");
  let expanded = $state<Set<string>>(new Set());
  let autoRefresh = $state(false);
  let autoTimer: ReturnType<typeof setInterval> | undefined;

  function toggleAuto() {
    autoRefresh = !autoRefresh;
    if (autoRefresh) {
      loadSkills();
      autoTimer = setInterval(loadSkills, 3000);
    } else {
      if (autoTimer) clearInterval(autoTimer);
      autoTimer = undefined;
    }
  }

  function toggleExpand(name: string) {
    const next = new Set(expanded);
    if (next.has(name)) next.delete(name);
    else next.add(name);
    expanded = next;
  }

  async function loadSkills() {
    loading = true;
    try {
      skills = await invoke<SkillEntry[]>("list_skills");
    } catch (e: any) {
      if (!autoRefresh) result = String(e);
    } finally {
      loading = false;
    }
  }

  async function reloadSkills() {
    loading = true;
    try {
      result = await invoke<string>("reload_skills");
      await loadSkills();
    } catch (e: any) {
      result = String(e);
    } finally {
      loading = false;
    }
  }

  let unlisten: (() => void) | null = null;

  async function toggleSkill(name: string, enable: boolean) {
    try {
      result = await invoke<string>(enable ? "enable_skill" : "disable_skill", { name });
      await loadSkills();
    } catch (e: any) {
      result = String(e);
    }
  }

  // Listen for Cmd+R / Ctrl+R via menu
  onMount(() => {
    listen("menu:reload_skills", async () => {
      await reloadSkills();
    }).then((fn) => { unlisten = fn; });
  });

  onDestroy(() => {
    if (unlisten) unlisten();
  });
</script>

<div class="card" style="display:flex;align-items:center;justify-content:space-between;">
  <h2>Registered Skills</h2>
  <div style="display:flex;gap:8px;align-items:center;">
    <label style="font-size:13px;display:flex;align-items:center;gap:4px;cursor:pointer;">
      <input type="checkbox" checked={autoRefresh} onchange={toggleAuto} />
      Auto
    </label>
    <button class="secondary" onclick={loadSkills} disabled={loading}>Refresh</button>
    <button onclick={reloadSkills} disabled={loading}>Reload All</button>
    <span style="font-size:11px;color:var(--fg-dim);">(Ctrl+R)</span>
  </div>
</div>

{#if result}
  <div class="card">
    <p style="color:var(--accent);font-size:13px;">{result}</p>
  </div>
{/if}

<div class="card">
  {#if skills.length === 0}
    <p style="color:var(--fg-dim);font-size:13px;">No skills registered. Click "Refresh" to load.</p>
  {:else}
    <table>
      <thead>
        <tr><th>Name</th><th>Version</th><th>Concurrency</th><th>Triggers</th><th>Status</th><th>Actions</th></tr>
      </thead>
      <tbody>
        {#each skills as s}
          <tr>
            <td><strong>{s.name}</strong></td>
            <td style="font-family:monospace;font-size:12px;">{s.version}</td>
            <td style="font-size:12px;">{s.concurrency}</td>
            <td>
              {#if s.triggers.length > 0}
                <button class="secondary" style="font-size:11px;padding:2px 8px;" onclick={() => toggleExpand(s.name)}>
                  {s.triggers.length} cond(s)
                </button>
              {:else}
                <span style="color:var(--fg-dim);font-size:12px;">none</span>
              {/if}
            </td>
            <td><span class="badge {s.enabled ? 'ok' : 'warn'}">{s.enabled ? "Enabled" : "Disabled"}</span></td>
            <td>
              {#if s.enabled}
                <button class="danger" style="font-size:11px;padding:2px 8px;" onclick={() => toggleSkill(s.name, false)}>
                  Disable
                </button>
              {:else}
                <button style="font-size:11px;padding:2px 8px;" onclick={() => toggleSkill(s.name, true)}>
                  Enable
                </button>
              {/if}
            </td>
          </tr>
          {#if expanded.has(s.name)}
            <tr>
              <td colspan="6" style="padding:8px 12px;background:var(--bg-darker, #f5f5f5);">
                <div style="font-size:12px;">
                  <p style="color:var(--fg-dim);margin-bottom:6px;"><strong>Description:</strong> {s.description}</p>
                  {#each s.triggers as t, i}
                    <div style="margin-bottom:6px;padding:6px;border:1px solid var(--border-color, #ddd);border-radius:4px;">
                      <p><strong>Trigger #{i + 1}</strong> {t.match_all ? "(match all)" : ""}</p>
                      <p>Event Types: <span style="font-family:monospace;background:var(--bg, #eee);padding:1px 4px;border-radius:2px;">{t.event_types.join(", ") || "any"}</span></p>
                      <p>Sources: <span style="font-family:monospace;">{t.sources.join(", ") || "any"}</span></p>
                      <p>Priorities: <span style="font-family:monospace;">{t.priorities.join(", ") || "any"}</span></p>
                    </div>
                  {/each}
                </div>
              </td>
            </tr>
          {/if}
        {/each}
      </tbody>
    </table>
  {/if}
</div>
