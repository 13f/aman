<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import { onMount } from "svelte";
  import { t } from "../lib/i18n.svelte";

  interface ProviderEntry {
    key: string;
    display_name: string;
    base_url: string;
    has_api_key: boolean;
  }

  let providers = $state<ProviderEntry[]>([]);
  let loading = $state(true);
  let error = $state("");
  let showCreateForm = $state(false);
  let showEditKeyForm = $state<string | null>(null);

  // Create form fields
  let newKey = $state("");
  let newDisplayName = $state("");
  let newBaseUrl = $state("");

  // Edit key field
  let editApiKey = $state("");

  async function loadProviders() {
    loading = true;
    error = "";
    try {
      providers = await invoke<ProviderEntry[]>("list_providers");
    } catch (e) {
      error = String(e);
    } finally {
      loading = false;
    }
  }

  async function createProvider() {
    if (!newKey.trim() || !newDisplayName.trim() || !newBaseUrl.trim()) return;
    try {
      await invoke("create_provider", {
        key: newKey.trim(),
        displayName: newDisplayName.trim(),
        baseUrl: newBaseUrl.trim(),
      });
      newKey = "";
      newDisplayName = "";
      newBaseUrl = "";
      showCreateForm = false;
      await loadProviders();
    } catch (e) {
      error = String(e);
    }
  }

  async function deleteProvider(key: string) {
    if (!confirm(t("providers.confirm_delete").replace("{key}", key))) return;
    try {
      await invoke("delete_provider", { key });
      await loadProviders();
    } catch (e) {
      error = String(e);
    }
  }

  async function setApiKey(key: string) {
    try {
      await invoke("set_provider_api_key", { key, apiKey: editApiKey });
      editApiKey = "";
      showEditKeyForm = null;
      await loadProviders();
    } catch (e) {
      error = String(e);
      editApiKey = "";
      showEditKeyForm = null;
    }
  }

  function openEditKey(key: string) {
    editApiKey = "";
    showEditKeyForm = key;
  }

  onMount(() => { loadProviders(); });
</script>

<div class="page-header">
  <h2>Providers</h2>
  <button onclick={() => { showCreateForm = !showCreateForm; }}>
    {showCreateForm ? t("providers.cancel") : t("providers.create")}
  </button>
</div>

{#if error}
  <div class="toast toast-error">{error}</div>
{/if}

{#if showCreateForm}
  <div class="card form-card">
    <h3>{t("providers.create_new")}</h3>
    <div class="form-field">
      <label for="new-key">{t("providers.form_key")}</label>
      <input id="new-key" type="text" placeholder="例如: openai" bind:value={newKey} />
    </div>
    <div class="form-field">
      <label for="new-display">{t("providers.form_display")}</label>
      <input id="new-display" type="text" placeholder="例如: OpenAI" bind:value={newDisplayName} />
    </div>
    <div class="form-field">
      <label for="new-url">{t("providers.form_base_url")}</label>
      <input id="new-url" type="text" placeholder="例如: https://api.openai.com/v1" bind:value={newBaseUrl} />
    </div>
    <div class="form-actions">
      <button class="secondary" onclick={() => { showCreateForm = false; }}>{t("providers.cancel")}</button>
      <button onclick={createProvider} disabled={!newKey.trim() || !newDisplayName.trim() || !newBaseUrl.trim()}>{t("providers.confirm")}</button>
    </div>
  </div>
{/if}

{#if loading}
  <p class="dim">{t("providers.loading")}</p>
{:else if providers.length === 0}
  <div class="card empty-state">
    <p>{t("providers.no_providers_empty")}</p>
    <p class="dim">{t("providers.no_providers_hint")}</p>
  </div>
{:else}
  <div class="provider-list">
    {#each providers as provider}
      <div class="card provider-card">
        <div class="provider-header">
          <strong class="provider-name">{provider.display_name}</strong>
          <span class="badge ok">{(provider as any).key}</span>
        </div>
        <div class="provider-detail">
          <span class="dim">Base URL:</span> {provider.base_url}
        </div>
        <div class="provider-detail">
          <span class="dim">API Key:</span>
          {#if provider.has_api_key}
            <span class="badge ok">{t("providers.configured")}</span>
          {:else}
            <span class="badge warn">{t("providers.not_configured")}</span>
          {/if}
        </div>
        <div class="provider-actions">
          {#if provider.has_api_key}
            <button class="secondary" onclick={() => openEditKey(provider.key)}>{t("providers.update_key")}</button>
          {:else}
            <button class="secondary" onclick={() => openEditKey(provider.key)}>{t("providers.set_key")}</button>
          {/if}
          <button class="danger" onclick={() => deleteProvider(provider.key)}>{t("common.delete")}</button>
        </div>

        {#if showEditKeyForm === provider.key}
          <div class="key-form">
            <div class="form-field">
              <label for="api-key-{provider.key}">API Key</label>
              <input id="api-key-{provider.key}" type="password" placeholder="sk-..." bind:value={editApiKey} />
            </div>
            <div class="form-actions">
              <button class="secondary" onclick={() => { showEditKeyForm = null; }}>{t("providers.cancel")}</button>
              <button onclick={() => setApiKey(provider.key)} disabled={!editApiKey.trim()}>{t("common.save")}</button>
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
  .form-card { max-width: 500px; }
  .form-field {
    margin-bottom: 12px;
  }
  .form-field label {
    display: block;
    font-size: 12px;
    color: var(--fg-dim);
    margin-bottom: 4px;
  }
  .form-actions {
    display: flex;
    gap: 8px;
    justify-content: flex-end;
    margin-top: 16px;
  }
  .provider-list {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(340px, 1fr));
    gap: 16px;
  }
  .provider-header {
    display: flex;
    align-items: center;
    gap: 8px;
    margin-bottom: 8px;
    flex-wrap: wrap;
  }
  .provider-name { font-size: 15px; }
  .provider-detail {
    font-size: 13px;
    margin-bottom: 4px;
  }
  .provider-actions {
    display: flex;
    gap: 8px;
    margin-top: 12px;
  }
  .key-form {
    margin-top: 12px;
    padding-top: 12px;
    border-top: 1px solid var(--border);
  }
  .dim { color: var(--fg-dim); }
  .empty-state { text-align: center; padding: 40px; }
  .toast-error {
    background: var(--red-muted);
    color: var(--red);
    padding: 10px 16px;
    border-radius: 6px;
    margin-bottom: 16px;
    font-size: 13px;
  }
</style>
