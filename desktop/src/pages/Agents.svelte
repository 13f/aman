<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import { listen } from "@tauri-apps/api/event";
  import { onMount, onDestroy } from "svelte";
  import AgentSelector from "./AgentSelector.svelte";
  import { t } from "../lib/i18n.svelte";

  let { onNavigate = (_page: string) => {} }: { onNavigate?: (page: string) => void } = $props();

  interface AgentEntry {
    key: string;
    display_name: string;
    provider: string;
    model: string;
    soul_summary: string;
    session_count: number;
    is_active: boolean;
  }

  interface ProviderEntry {
    key: string;
    display_name: string;
    base_url: string;
    has_api_key: boolean;
  }

  interface ModelEntry {
    id: string;
    model_id: string;
  }

  let agents = $state<AgentEntry[]>([]);
  let providers = $state<ProviderEntry[]>([]);
  let loading = $state(true);
  let error = $state("");
  let showCreateForm = $state(false);
  let noProviders = $state(false);

  // Create form
  let newKey = $state("");
  let newDisplayName = $state("");
  let newProvider = $state("");
  let newModel = $state("");

  const defaultSoul = `# {name}

## Identity
I am {name}, an AI assistant powered by Aman.

## Core
I help users accomplish their tasks efficiently and accurately.

## Expertise
General knowledge and problem-solving across many domains.

## Boundaries
I respect user privacy and follow ethical guidelines.

## Vibe
Professional, helpful, and clear.

## Preferences
I prefer concise and accurate responses.
`;

  let newSoulContent = $state(defaultSoul);

  let newModelEntries = $state<ModelEntry[]>([]);
  let showNewModelDropdown = $state(false);
  let isLoadingNewModels = $state(false);
  let newModelBlurTimer: ReturnType<typeof setTimeout> | null = null;

  // Real-time idle state per agent (from event:processed idle events).
  interface IdleState { kind: string; depth: number; arousal: number; }
  let idleStates = $state<Record<string, IdleState>>({});
  // Per-agent LLM backend health (cognitive state). 由 cognitive_state_changed
  // 事件更新,与 Home 页面一致。用来在卡片上标出 Lucid/Groggy/Catonic/Coma。
  let brainStates = $state<Record<string, string>>({});
  let unlisteners: (() => void)[] = [];

  async function fetchNewModels() {
    if (!newProvider) {
      newModelEntries = [];
      return;
    }
    isLoadingNewModels = true;
    try {
      newModelEntries = await invoke<ModelEntry[]>("list_provider_models", {
        providerKey: newProvider,
      });
      showNewModelDropdown = newModelEntries.length > 0;
    } catch {
      newModelEntries = [];
      showNewModelDropdown = false;
    } finally {
      isLoadingNewModels = false;
    }
  }

  function hideNewModelDropdown() {
    newModelBlurTimer = setTimeout(() => {
      showNewModelDropdown = false;
    }, 150);
  }

  function selectNewModel(m: ModelEntry) {
    newModel = m.id;
    showNewModelDropdown = false;
    if (newModelBlurTimer) {
      clearTimeout(newModelBlurTimer);
      newModelBlurTimer = null;
    }
  }

  async function loadData() {
    loading = true;
    error = "";
    try {
      agents = await invoke<AgentEntry[]>("list_agents");
      const provs = await invoke<ProviderEntry[]>("list_providers");
      providers = provs;
      noProviders = provs.length === 0;
    } catch (e) {
      error = String(e);
    } finally {
      loading = false;
    }
  }

  async function createAgent() {
    if (!newKey.trim() || !newDisplayName.trim() || !newProvider.trim() || !newModel.trim()) return;
    let soul = newSoulContent.replace(/\{name\}/g, newDisplayName.trim());
    try {
      await invoke("create_agent", {
        key: newKey.trim(),
        displayName: newDisplayName.trim(),
        provider: newProvider.trim(),
        model: newModel.trim(),
        soulContent: soul,
      });
      newKey = "";
      newDisplayName = "";
      newProvider = "";
      newModel = "";
      newSoulContent = defaultSoul;
      showCreateForm = false;
      await loadData();
    } catch (e) {
      error = String(e);
    }
  }

  async function handleSaveEditFromSelector(key: string, displayName: string, provider: string, model: string, soulContent: string) {
    try {
      await invoke("update_agent", {
        key,
        displayName: displayName?.trim() || null,
        provider: provider?.trim() || null,
        model: model?.trim() || null,
        soulContent: soulContent?.trim() || null,
      });
      await loadData();
    } catch (e) {
      error = String(e);
    }
  }

  async function deleteAgent(key: string) {
    if (!confirm(t("agents.confirm_delete").replace("{key}", key))) return;
    try {
      await invoke("delete_agent", { key });
      await loadData();
    } catch (e) {
      error = String(e);
    }
  }

  async function selectAgent(key: string) {
    try {
      const agent = agents.find(a => a.key === key);
      const displayName = agent?.display_name ?? key;
      await invoke("open_or_focus_agent_window", { agentKey: key, displayName });
    } catch (e) {
      error = String(e);
    }
  }

  onMount(() => {
    loadData();
    // Listen for per-agent idle events to show idle kind on agent cards.
    listen("event:processed", (e: any) => {
      const p = e.payload;
      if (p?.event_type !== "idle") return;
      const data = p.payload ?? {};
      const agentId: string | undefined = data.agent_id ?? data.payload?.agent_id;
      if (!agentId) return;
      idleStates = {
        ...idleStates,
        [agentId]: {
          kind: data.kind ?? "daze",
          depth: data.depth ?? 0,
          arousal: data.context?.arousal_level ?? 0.5,
        },
      };
    }).then(fn => { unlisteners.push(fn); });

    // Clear idle state when agents transition to non-idle system state.
    listen("agent_states:updated", (e: any) => {
      const list: Array<{ agent_id: string; system_state: string; cognitive_state?: string }> = e.payload?.agents ?? [];
      for (const a of list) {
        if (a.system_state !== "idle" && idleStates[a.agent_id]) {
          const next = { ...idleStates };
          delete next[a.agent_id];
          idleStates = next;
        }
        // agent_states:updated 快照已带 cognitive_state,直接更新。
        if (a.cognitive_state) {
          brainStates = { ...brainStates, [a.agent_id]: a.cognitive_state };
        }
      }
    }).then(fn => { unlisteners.push(fn); });

    // 实时监听 cognitive_state_changed 事件(事件驱动,比快照更快)。
    listen("event:processed", (e: any) => {
      const p = e.payload;
      if (p?.event_type !== "cognitive_state_changed") return;
      const inner = p.payload ?? {};
      const agentId: string | undefined = inner.agent_id;
      const state: string | undefined = inner.state;
      if (agentId && state) brainStates = { ...brainStates, [agentId]: state };
    }).then(fn => { unlisteners.push(fn); });
  });

  onDestroy(() => {
    for (const fn of unlisteners) fn();
  });
</script>

<div class="page-header">
  <h2>Agents</h2>
  <button onclick={() => { showCreateForm = !showCreateForm; }} disabled={noProviders}>
    {showCreateForm ? t("agents.cancel") : t("agents.create")}
  </button>
</div>

{#if error}
  <div class="toast toast-error">{error}</div>
{/if}

{#if noProviders}
  <div class="card empty-state">
    <p>{t("agents.no_providers")}</p>
    <p class="dim">{t("agents.no_providers_hint")}</p>
  </div>
{:else if showCreateForm}
  <div class="card form-card">
    <h3>{t("agents.create_new")}</h3>
    <div class="form-field">
      <label for="agent-key">{t("agents.form_key")}</label>
      <input id="agent-key" type="text" placeholder="例如: cortana" bind:value={newKey} />
    </div>
    <div class="form-field">
      <label for="agent-display">{t("agents.form_display")}</label>
      <input id="agent-display" type="text" placeholder="例如: Cortana" bind:value={newDisplayName} />
    </div>
    <div class="form-field">
      <label for="agent-provider">{t("agents.form_provider")}</label>
      <select id="agent-provider" bind:value={newProvider} onchange={() => { newModelEntries = []; showNewModelDropdown = false; }}>
        <option value="">{t("agents.select_provider_placeholder")}</option>
        {#each providers as p}
          <option value={p.key}>{p.display_name}</option>
        {/each}
      </select>
    </div>
    <div class="form-field model-field">
      <label for="agent-model">{t("agents.form_model")}</label>
      <input
        id="agent-model"
        type="text"
        placeholder="例如: gpt-5"
        bind:value={newModel}
        onfocus={fetchNewModels}
        onblur={hideNewModelDropdown}
      />
      {#if isLoadingNewModels}
        <div class="model-dropdown-loading">{t("agents.loading_models")}</div>
      {/if}
      {#if showNewModelDropdown && newModelEntries.length > 0}
        <div class="model-dropdown">
          {#each newModelEntries as m}
            <button
              type="button"
              class="model-entry"
              onmousedown={(e) => e.preventDefault()}
              onclick={() => selectNewModel(m)}
            >
              <span class="model-name">{m.id}</span>
              <span class="model-id">{m.model_id}</span>
            </button>
          {/each}
        </div>
      {/if}
    </div>
    <div class="form-field">
      <label for="agent-soul">{t("agents.form_soul")}</label>
      <textarea id="agent-soul" rows="10" bind:value={newSoulContent}></textarea>
    </div>
    <div class="form-actions">
      <button class="secondary" onclick={() => { showCreateForm = false; }}>{t("agents.cancel")}</button>
      <button onclick={createAgent} disabled={!newKey.trim() || !newDisplayName.trim() || !newProvider.trim() || !newModel.trim()}>{t("agents.confirm")}</button>
    </div>
  </div>
{:else if loading}
  <p class="dim">{t("common.loading")}</p>
{:else if agents.length === 0}
  <div class="card empty-state">
    <p>{t("agents.no_agents_empty")}</p>
    <p class="dim">{t("agents.no_agents_hint")}</p>
  </div>
{:else}
  <AgentSelector
    variant="full"
    {agents}
    {providers}
    {idleStates}
    {brainStates}
    onSelect={(agent) => selectAgent(agent.key)}
    onDelete={deleteAgent}
    onSaveEdit={handleSaveEditFromSelector}
    {onNavigate}
  />
{/if}

<style>
  .page-header {
    display: flex;
    justify-content: space-between;
    align-items: center;
    margin-bottom: 20px;
  }
  .page-header h2 { margin: 0; font-size: 18px; }
  .form-card { max-width: 600px; }
  .form-field {
    margin-bottom: 12px;
  }
  .form-field label {
    display: block;
    font-size: 12px;
    color: var(--fg-dim);
    margin-bottom: 4px;
  }
  .form-field select {
    width: 100%;
    background: var(--bg);
    border: 1px solid var(--border);
    border-radius: 6px;
    padding: 10px;
    color: var(--fg);
    font-size: 13px;
  }
  .form-actions {
    display: flex;
    gap: 8px;
    justify-content: flex-end;
    margin-top: 16px;
  }
  .model-field {
    position: relative;
  }
  .model-dropdown-loading {
    position: absolute;
    z-index: 10;
    background: var(--bg-card);
    backdrop-filter: blur(var(--glass-blur-far));
    -webkit-backdrop-filter: blur(var(--glass-blur-far));
    border: 1px solid var(--border);
    border-radius: 6px;
    padding: 10px 14px;
    font-size: 12px;
    color: var(--fg-dim);
    width: 100%;
    box-shadow: 0 8px 24px rgba(0,0,0,0.25);
  }
  .model-dropdown {
    position: absolute;
    z-index: 10;
    background: var(--bg-card);
    backdrop-filter: blur(var(--glass-blur-far));
    -webkit-backdrop-filter: blur(var(--glass-blur-far));
    border: 1px solid var(--border);
    border-radius: 6px;
    max-height: 240px;
    overflow-y: auto;
    width: 100%;
    box-shadow: 0 8px 24px rgba(0,0,0,0.25);
  }
  .model-entry {
    display: flex;
    flex-direction: column;
    align-items: flex-start;
    width: 100%;
    padding: 10px 14px;
    border: none;
    background: transparent;
    color: var(--fg);
    font-family: inherit;
    font-size: 13px;
    cursor: pointer;
    text-align: left;
    gap: 2px;
  }
  .model-entry:hover {
    background: var(--accent-light, rgba(108,140,255,0.1));
  }
  .model-name {
    font-weight: 600;
    font-size: 13px;
  }
  .model-id {
    font-size: 11px;
    color: var(--fg-dim);
  }
  .dim { color: var(--fg-dim); }
  .empty-state { text-align: center; padding: 40px; }
  .toast-error {
    background: rgba(248,113,113,0.15);
    color: var(--red);
    padding: 10px 16px;
    border-radius: 6px;
    margin-bottom: 16px;
    font-size: 13px;
  }
</style>
