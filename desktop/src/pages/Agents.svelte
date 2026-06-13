<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import { onMount } from "svelte";
  import AgentSelector from "./AgentSelector.svelte";

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
    if (!confirm(`删除 Agent "${key}"？\n此操作会删除所有相关的 session 和记忆。`)) return;
    try {
      await invoke("delete_agent", { key });
      await loadData();
    } catch (e) {
      error = String(e);
    }
  }

  async function selectAgent(key: string) {
    try {
      await invoke("select_agent", { key });
      onNavigate("chat");
    } catch (e) {
      error = String(e);
    }
  }

  onMount(() => { loadData(); });
</script>

<div class="page-header">
  <h2>Agents</h2>
  <button onclick={() => { showCreateForm = !showCreateForm; }} disabled={noProviders}>
    {showCreateForm ? "取消" : "+ 新建 Agent"}
  </button>
</div>

{#if error}
  <div class="toast toast-error">{error}</div>
{/if}

{#if noProviders}
  <div class="card empty-state">
    <p>请先配置 Provider 再创建 Agent。</p>
    <p class="dim">前往 Providers 页面添加 LLM Provider。</p>
  </div>
{:else if showCreateForm}
  <div class="card form-card">
    <h3>创建新 Agent</h3>
    <div class="form-field">
      <label for="agent-key">Key</label>
      <input id="agent-key" type="text" placeholder="例如: cortana" bind:value={newKey} />
    </div>
    <div class="form-field">
      <label for="agent-display">Display Name</label>
      <input id="agent-display" type="text" placeholder="例如: Cortana" bind:value={newDisplayName} />
    </div>
    <div class="form-field">
      <label for="agent-provider">Provider</label>
      <select id="agent-provider" bind:value={newProvider} onchange={() => { newModelEntries = []; showNewModelDropdown = false; }}>
        <option value="">-- 选择 Provider --</option>
        {#each providers as p}
          <option value={p.key}>{p.display_name}</option>
        {/each}
      </select>
    </div>
    <div class="form-field model-field">
      <label for="agent-model">Model</label>
      <input
        id="agent-model"
        type="text"
        placeholder="例如: gpt-5"
        bind:value={newModel}
        onfocus={fetchNewModels}
        onblur={hideNewModelDropdown}
      />
      {#if isLoadingNewModels}
        <div class="model-dropdown-loading">加载中...</div>
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
      <label for="agent-soul">SOUL.md 内容</label>
      <textarea id="agent-soul" rows="10" bind:value={newSoulContent}></textarea>
    </div>
    <div class="form-actions">
      <button class="secondary" onclick={() => { showCreateForm = false; }}>取消</button>
      <button onclick={createAgent} disabled={!newKey.trim() || !newDisplayName.trim() || !newProvider.trim() || !newModel.trim()}>创建</button>
    </div>
  </div>
{:else if loading}
  <p class="dim">加载中...</p>
{:else if agents.length === 0}
  <div class="card empty-state">
    <p>还没有 Agent。</p>
    <p class="dim">点击"新建 Agent"创建你的第一个 AI 助手。</p>
  </div>
{:else}
  <AgentSelector
    variant="full"
    {agents}
    {providers}
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
    backdrop-filter: blur(12px);
    -webkit-backdrop-filter: blur(12px);
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
    backdrop-filter: blur(12px);
    -webkit-backdrop-filter: blur(12px);
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
