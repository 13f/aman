<script lang="ts">
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

  let {
    agents = [],
    providers = [],
    variant = "compact",
    onSelect = (_agent: AgentEntry) => {},
    onDelete = async (_key: string) => {},
    onSaveEdit = async (_key: string, _displayName: string, _provider: string, _model: string, _soulContent: string) => {},
    onNavigate = (_page: string) => {},
  }: {
    agents: AgentEntry[];
    providers?: ProviderEntry[];
    variant?: "full" | "compact";
    onSelect?: (agent: AgentEntry) => void;
    onDelete?: (key: string) => Promise<void>;
    onSaveEdit?: (key: string, displayName: string, provider: string, model: string, soulContent: string) => Promise<void>;
    onNavigate?: (page: string) => void;
  } = $props();

  let showEditForm = $state<string | null>(null);
  let editDisplayName = $state("");
  let editProvider = $state("");
  let editModel = $state("");
  let editSoulContent = $state("");

  function openEdit(agent: AgentEntry) {
    editDisplayName = agent.display_name;
    editProvider = agent.provider;
    editModel = agent.model;
    editSoulContent = "";
    showEditForm = agent.key;
  }
</script>

{#if variant === "full"}
  {#if agents.length === 0}
    <div class="card empty-state">
      <p>还没有 Agent。</p>
      <p class="dim">点击"新建 Agent"创建你的第一个 AI 助手。</p>
    </div>
  {:else}
    <div class="agent-list">
      {#each agents as agent}
        <div class="card agent-card {agent.is_active ? 'active' : ''}" class:needs-config={!agent.provider}>
          <div class="agent-header">
            <strong class="agent-name">{agent.display_name}</strong>
            <span class="badge ok">{(agent as any).key}</span>
            {#if agent.is_active}
              <span class="badge" style="background:rgba(108,140,255,0.15);color:var(--accent);">Active</span>
            {/if}
          </div>
          <div class="agent-detail">
            {#if agent.provider}
              <span class="dim">Provider:</span> {agent.provider}
              <span class="dim" style="margin-left:16px;">Model:</span> {agent.model}
            {:else}
              <span class="needs-config-badge">⚡ 需要配置 Provider</span>
            {/if}
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
            {#if !agent.provider}
              <button class="primary" onclick={() => openEdit(agent)}>⚙ 配置 Provider</button>
              <button class="secondary" onclick={() => openEdit(agent)}>编辑</button>
              <button class="danger" onclick={() => onDelete(agent.key)}>删除</button>
            {:else if !agent.is_active}
              <button onclick={() => onSelect(agent)}>选择并聊天</button>
              <button class="secondary" onclick={() => openEdit(agent)}>编辑</button>
              <button class="danger" onclick={() => onDelete(agent.key)}>删除</button>
            {:else}
              <button class="secondary" onclick={() => onNavigate("chat")}>去 Chat</button>
              <button class="secondary" onclick={() => openEdit(agent)}>编辑</button>
              <button class="danger" onclick={() => onDelete(agent.key)}>删除</button>
            {/if}
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
                <button onclick={() => onSaveEdit(agent.key, editDisplayName, editProvider, editModel, editSoulContent)}>保存</button>
              </div>
            </div>
          {/if}
        </div>
      {/each}
    </div>
  {/if}
{:else}
  {#if agents.length === 0}
    <div class="empty-compact">
      <p class="dim">还没有 Agent。在 Services &gt; Agents 中创建。</p>
    </div>
  {:else}
    <div class="agent-grid-compact">
      {#each agents as agent}
        <button
          class="card agent-card-compact"
          class:needs-config={!agent.provider}
          class:active={agent.is_active}
          onclick={() => onSelect(agent)}
        >
          <div class="compact-name">{agent.display_name}</div>
          <div class="compact-badges">
            <span class="badge ok">{(agent as any).key}</span>
            {#if agent.is_active}
              <span class="badge" style="background:rgba(108,140,255,0.15);color:var(--accent);">Active</span>
            {/if}
          </div>
          {#if agent.provider}
            <div class="compact-detail">{agent.provider} / {agent.model}</div>
          {:else}
            <span class="needs-config-badge">⚡ 需要配置</span>
          {/if}
          {#if agent.soul_summary}
            <div class="compact-soul">{agent.soul_summary}</div>
          {/if}
        </button>
      {/each}
    </div>
  {/if}
{/if}

<style>
  /* ---- full variant (Agents page) ---- */
  .agent-list {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(340px, 1fr));
    gap: 16px;
  }
  .agent-card.active {
    border-color: var(--accent);
  }
  .agent-card.needs-config {
    border-color: var(--warn, #f59e0b);
    background: linear-gradient(135deg,
      rgba(245,158,11,0.06) 0%,
      var(--bg-card) 40%
    );
    position: relative;
  }
  .agent-card.needs-config::after {
    content: "";
    position: absolute;
    top: 0;
    left: 0;
    width: 4px;
    height: 100%;
    background: var(--warn, #f59e0b);
    border-radius: 12px 0 0 12px;
  }
  .needs-config-badge {
    display: inline-block;
    padding: 3px 10px;
    background: rgba(245,158,11,0.15);
    color: var(--warn, #f59e0b);
    border-radius: 4px;
    font-size: 12px;
    font-weight: 600;
  }
  .agent-header {
    display: flex;
    align-items: center;
    gap: 8px;
    margin-bottom: 8px;
    flex-wrap: wrap;
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
  .dim { color: var(--fg-dim); }
  .empty-state { text-align: center; padding: 40px; }

  /* ---- compact variant (Home modal) ---- */
  .agent-grid-compact {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(160px, 1fr));
    gap: 12px;
  }
  .agent-card-compact {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 6px;
    padding: 16px;
    cursor: pointer;
    text-align: center;
    background: var(--bg-card);
    border: 1px solid var(--border);
    border-radius: 12px;
    width: 100%;
    color: var(--fg);
    font-family: inherit;
    font-size: inherit;
  }
  .agent-card-compact:hover {
    border-color: var(--accent);
    transform: translateY(-2px);
    box-shadow: 0 4px 12px rgba(0,0,0,0.15);
  }
  .agent-card-compact.active {
    border-color: var(--accent);
  }
  .agent-card-compact.needs-config {
    opacity: 0.5;
  }
  .agent-card-compact.needs-config:hover {
    opacity: 0.7;
  }
  .compact-name {
    font-size: 14px;
    font-weight: 600;
  }
  .compact-badges {
    display: flex;
    gap: 4px;
    flex-wrap: wrap;
    justify-content: center;
  }
  .compact-detail {
    font-size: 11px;
    color: var(--fg-dim);
  }
  .compact-soul {
    font-size: 11px;
    color: var(--fg-dim);
    overflow: hidden;
    max-height: 36px;
    line-height: 1.3;
  }
  .empty-compact {
    text-align: center;
    padding: 20px;
  }
</style>
