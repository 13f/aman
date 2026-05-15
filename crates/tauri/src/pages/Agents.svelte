<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import { onMount } from "svelte";

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

  let agents = $state<AgentEntry[]>([]);
  let providers = $state<ProviderEntry[]>([]);
  let loading = $state(true);
  let error = $state("");
  let showCreateForm = $state(false);
  let showEditForm = $state<string | null>(null);
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

  // Edit form
  let editDisplayName = $state("");
  let editProvider = $state("");
  let editModel = $state("");
  let editSoulContent = $state("");

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

  async function updateAgent(key: string) {
    try {
      await invoke("update_agent", {
        key,
        displayName: editDisplayName || null,
        provider: editProvider || null,
        model: editModel || null,
        soulContent: editSoulContent || null,
      });
      showEditForm = null;
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

  function openEdit(agent: AgentEntry) {
    editDisplayName = agent.display_name;
    editProvider = agent.provider;
    editModel = agent.model;
    editSoulContent = "";
    showEditForm = agent.key;
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
      <select id="agent-provider" bind:value={newProvider}>
        <option value="">-- 选择 Provider --</option>
        {#each providers as p}
          <option value={p.key}>{p.display_name}</option>
        {/each}
      </select>
    </div>
    <div class="form-field">
      <label for="agent-model">Model</label>
      <input id="agent-model" type="text" placeholder="例如: gpt-5" bind:value={newModel} />
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
  <div class="agent-list">
    {#each agents as agent}
      <div class="card agent-card {agent.is_active ? 'active' : ''}">
        <div class="agent-header">
          <strong class="agent-name">{agent.display_name}</strong>
          <span class="badge ok">{(agent as any).key}</span>
          {#if agent.is_active}
            <span class="badge" style="background:rgba(108,140,255,0.15);color:var(--accent);">Active</span>
          {/if}
        </div>
        <div class="agent-detail">
          <span class="dim">Provider:</span> {agent.provider}
          <span class="dim" style="margin-left:16px;">Model:</span> {agent.model}
        </div>
        {#if agent.soul_summary}
          <div class="agent-soul-preview">
            <span class="dim">SOUL:</span>
            <pre class="soul-text">{agent.soul_summary}</pre>
          </div>
        {/if}
        <div class="agent-detail">
          <span class="dim">Sessions:</span> {agent.session_count}
        </div>
        <div class="agent-actions">
          {#if !agent.is_active}
            <button onclick={() => selectAgent(agent.key)}>选择并聊天</button>
          {:else}
            <button class="secondary" onclick={() => onNavigate("chat")}>去 Chat</button>
          {/if}
          <button class="secondary" onclick={() => openEdit(agent)}>编辑</button>
          <button class="danger" onclick={() => deleteAgent(agent.key)}>删除</button>
        </div>

        {#if showEditForm === agent.key}
          <div class="edit-form">
            <div class="form-field">
              <label for="edit-display-{agent.key}">Display Name</label>
              <input id="edit-display-{agent.key}" type="text" bind:value={editDisplayName} />
            </div>
            <div class="form-field">
              <label for="edit-provider-{agent.key}">Provider</label>
              <select id="edit-provider-{agent.key}" bind:value={editProvider}>
                {#each providers as p}
                  <option value={p.key}>{p.display_name}</option>
                {/each}
              </select>
            </div>
            <div class="form-field">
              <label for="edit-model-{agent.key}">Model</label>
              <input id="edit-model-{agent.key}" type="text" bind:value={editModel} />
            </div>
            <div class="form-field">
              <label for="edit-soul-{agent.key}">SOUL.md (留空不修改)</label>
              <textarea id="edit-soul-{agent.key}" rows="8" bind:value={editSoulContent}></textarea>
            </div>
            <div class="form-actions">
              <button class="secondary" onclick={() => { showEditForm = null; }}>取消</button>
              <button onclick={() => updateAgent(agent.key)}>保存</button>
            </div>
          </div>
        {/if}
      </div>
    {/each}
  </div>
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
  .agent-list {
    display: flex;
    flex-direction: column;
    gap: 12px;
  }
  .agent-card { max-width: 650px; }
  .agent-card.active {
    border-color: var(--accent);
  }
  .agent-header {
    display: flex;
    align-items: center;
    gap: 8px;
    margin-bottom: 8px;
  }
  .agent-name { font-size: 15px; }
  .agent-detail {
    font-size: 13px;
    margin-bottom: 4px;
  }
  .agent-soul-preview {
    margin: 8px 0;
    font-size: 13px;
  }
  .soul-text {
    background: var(--bg);
    border: 1px solid var(--border);
    border-radius: 4px;
    padding: 8px;
    margin-top: 4px;
    font-size: 12px;
    white-space: pre-wrap;
    overflow: hidden;
    max-height: 60px;
  }
  .agent-actions {
    display: flex;
    gap: 8px;
    margin-top: 12px;
  }
  .edit-form {
    margin-top: 12px;
    padding-top: 12px;
    border-top: 1px solid var(--border);
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
