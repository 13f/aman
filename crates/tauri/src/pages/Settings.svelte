<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import { onMount } from "svelte";

  interface Service {
    id: string;
    display_name: string;
    requires_key: boolean;
    has_key: boolean;
    has_cx: boolean;
    tags: string[];
    disabled: boolean;
  }

  let services = $state<Service[]>([]);
  let loading = $state(true);
  let saving = $state<Record<string, boolean>>({});
  let savingCx = $state<Record<string, boolean>>({});
  let messages = $state<Record<string, { type: "success" | "error"; text: string }>>({});

  // Editable fields
  let keyInputs = $state<Record<string, string>>({});
  let cxInputs = $state<Record<string, string>>({});

  onMount(async () => {
    await loadServices();
  });

  async function loadServices() {
    loading = true;
    try {
      services = await invoke<Service[]>("list_third_party_services");
      // Clear input fields (don't pre-fill the actual key values)
      for (const s of services) {
        keyInputs[s.id] = "";
        cxInputs[s.id] = s.has_cx ? "******" : "";
      }
    } catch (e) {
      messages["_global"] = { type: "error", text: `Failed to load: ${e}` };
    } finally {
      loading = false;
    }
  }

  async function saveKey(serviceId: string) {
    const value = keyInputs[serviceId]?.trim();
    if (!value) return;

    saving[serviceId] = true;
    try {
      await invoke("set_third_party_key", { service: serviceId, apiKey: value });
      messages[serviceId] = { type: "success", text: "Saved to Keychain" };
      keyInputs[serviceId] = "";
      // Reload to update status
      await loadServices();
    } catch (e) {
      messages[serviceId] = { type: "error", text: `${e}` };
    } finally {
      saving[serviceId] = false;
    }
  }

  async function saveCx(serviceId: string) {
    const value = cxInputs[serviceId]?.trim();
    if (!value) return;

    savingCx[serviceId] = true;
    try {
      await invoke("set_third_party_config", {
        service: serviceId,
        subKey: "cx",
        value,
      });
      messages[`${serviceId}-cx`] = { type: "success", text: "Saved to Keychain" };
      cxInputs[serviceId] = "";
      await loadServices();
    } catch (e) {
      messages[`${serviceId}-cx`] = { type: "error", text: `${e}` };
    } finally {
      savingCx[serviceId] = false;
    }
  }

  function clearMessage(serviceId: string) {
    delete messages[serviceId];
    delete messages[`${serviceId}-cx`];
    messages = { ...messages };
  }
</script>

<div class="settings">
  <h1>Settings</h1>

  <div class="tabs">
    <button class="tab active">Third Party Services</button>
  </div>

  {#if loading}
    <p class="loading">Loading...</p>
  {:else}
    <div class="service-list">
      {#each services as service (service.id)}
        <div class="service-card" class:disabled={service.disabled}>
          <div class="service-header">
            <span class="service-name">{service.display_name}</span>
            <span class="service-id">{service.id}</span>
            <div class="tag-list">
              {#each service.tags as tag}
                <span class="tag">{tag}</span>
              {/each}
            </div>
          </div>

          <div class="service-config">
            <!-- API Key -->
            <div class="config-row">
              <label for="key-{service.id}">API Key</label>
              <div class="input-row">
                <input
                  id="key-{service.id}"
                  type="password"
                  placeholder={service.has_key ? "•••••••• (replace)" : service.requires_key ? "Required" : "Optional"}
                  bind:value={keyInputs[service.id]}
                  oninput={() => clearMessage(service.id)}
                />
                <button
                  onclick={() => saveKey(service.id)}
                  disabled={!keyInputs[service.id]?.trim() || saving[service.id]}
                >
                  {saving[service.id] ? "Saving..." : service.has_key ? "Update" : "Save"}
                </button>
              </div>
              {#if messages[service.id]}
                <span class="msg {messages[service.id].type}">{messages[service.id].text}</span>
              {/if}
              <span class="status-badge {service.has_key ? 'configured' : 'unconfigured'}">
                {service.has_key ? "Configured" : "Not configured"}
              </span>
            </div>

            <!-- Google CX field (only for google) -->
            {#if service.id === "google"}
              <div class="config-row">
                <label for="cx-{service.id}">Search Engine ID (cx)</label>
                <div class="input-row">
                  <input
                    id="cx-{service.id}"
                    type="password"
                    placeholder={service.has_cx ? "•••••••• (replace)" : "Required for Google"}
                    bind:value={cxInputs[service.id]}
                    oninput={() => clearMessage(`${service.id}-cx`)}
                  />
                  <button
                    onclick={() => saveCx(service.id)}
                    disabled={!cxInputs[service.id]?.trim() || savingCx[service.id]}
                  >
                    {savingCx[service.id] ? "Saving..." : service.has_cx ? "Update" : "Save"}
                  </button>
                </div>
                {#if messages[`${service.id}-cx`]}
                  <span class="msg {messages[`${service.id}-cx`].type}">{messages[`${service.id}-cx`].text}</span>
                {/if}
                <span class="status-badge {service.has_cx ? 'configured' : 'unconfigured'}">
                  {service.has_cx ? "Configured" : "Not configured"}
                </span>
              </div>
            {/if}
          </div>

          {#if !service.requires_key}
            <p class="note">This service has a free tier — an API key is optional.</p>
          {/if}
        </div>
      {/each}
    </div>
  {/if}
</div>

<style>
  .settings {
    padding: 24px;
    max-width: 800px;
  }

  h1 {
    margin: 0 0 16px 0;
    font-size: 1.4rem;
  }

  .tabs {
    display: flex;
    gap: 0;
    border-bottom: 1px solid var(--border-color, #333);
    margin-bottom: 24px;
  }

  .tab {
    padding: 8px 20px;
    border: none;
    background: none;
    cursor: pointer;
    font-size: 0.9rem;
    color: var(--text-secondary, #999);
    border-bottom: 2px solid transparent;
  }

  .tab.active {
    color: var(--text-primary, #fff);
    border-bottom-color: var(--accent, #4a9eff);
  }

  .loading {
    color: var(--text-secondary, #999);
  }

  .service-list {
    display: flex;
    flex-direction: column;
    gap: 16px;
  }

  .service-card {
    border: 1px solid var(--border-color, #333);
    border-radius: 8px;
    padding: 16px;
    background: var(--bg-secondary, #1a1a2e);
  }

  .service-card.disabled {
    opacity: 0.45;
    pointer-events: none;
  }

  .service-header {
    display: flex;
    align-items: center;
    gap: 8px;
    margin-bottom: 12px;
    flex-wrap: wrap;
  }

  .service-name {
    font-weight: 600;
    font-size: 1rem;
  }

  .service-id {
    font-size: 0.75rem;
    color: var(--text-secondary, #999);
    background: var(--bg-tertiary, #2a2a3e);
    padding: 2px 6px;
    border-radius: 4px;
  }

  .tag-list {
    display: flex;
    gap: 4px;
    margin-left: auto;
  }

  .tag {
    font-size: 0.7rem;
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.03em;
    padding: 2px 8px;
    border-radius: 10px;
    background: #1a3a5a;
    color: #6ab0ff;
  }

  .service-config {
    display: flex;
    flex-direction: column;
    gap: 12px;
  }

  .config-row {
    display: flex;
    flex-direction: column;
    gap: 4px;
  }

  .config-row label {
    font-size: 0.8rem;
    color: var(--text-secondary, #999);
  }

  .input-row {
    display: flex;
    gap: 8px;
    align-items: stretch;
  }

  .input-row input {
    flex: 1;
    padding: 6px 10px;
    border: 1px solid var(--border-color, #444);
    border-radius: 4px;
    background: var(--bg-input, #111);
    color: var(--text-primary, #fff);
    font-size: 0.9rem;
  }

  .input-row button {
    padding: 6px 16px;
    border: 1px solid var(--accent, #4a9eff);
    border-radius: 4px;
    background: var(--accent, #4a9eff);
    color: #fff;
    cursor: pointer;
    font-size: 0.85rem;
    white-space: nowrap;
  }

  .input-row button:disabled {
    opacity: 0.5;
    cursor: not-allowed;
  }

  .status-badge {
    font-size: 0.75rem;
    padding: 2px 8px;
    border-radius: 10px;
    display: inline-block;
    width: fit-content;
  }

  .status-badge.configured {
    background: #1a3a1a;
    color: #4caf50;
  }

  .status-badge.unconfigured {
    background: #3a1a1a;
    color: #f44336;
  }

  .msg {
    font-size: 0.8rem;
  }

  .msg.success {
    color: #4caf50;
  }

  .msg.error {
    color: #f44336;
  }

  .note {
    font-size: 0.8rem;
    color: var(--text-secondary, #999);
    margin: 8px 0 0 0;
    font-style: italic;
  }
</style>
