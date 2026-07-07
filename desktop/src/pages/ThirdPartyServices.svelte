<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import { onMount } from "svelte";
  import { t } from "../lib/i18n.svelte";

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

<div class="page-header">
  <h2>{t("settings.third_party_services")}</h2>
</div>

{#if loading}
  <p class="dim">Loading...</p>
{:else}
  <div class="service-list">
    {#each services as service (service.id)}
      <div class="card service-card" class:disabled={service.disabled}>
        <div class="service-header">
          <strong class="service-name">{service.display_name}</strong>
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

<style>
  .page-header {
    display: flex;
    justify-content: space-between;
    align-items: center;
    margin-bottom: 20px;
  }
  .page-header h2 { margin: 0; font-size: 18px; }

  .service-list {
    display: flex;
    flex-direction: column;
    gap: 16px;
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

  .service-name { font-size: 15px; }

  .service-id {
    font-size: 0.75rem;
    color: var(--fg-dim);
    background: var(--bg);
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
    background: var(--accent-muted);
    color: var(--accent);
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
    color: var(--fg-dim);
  }

  .input-row {
    display: flex;
    gap: 8px;
    align-items: stretch;
  }

  .input-row input {
    flex: 1;
    padding: 6px 10px;
    border: 1px solid var(--border);
    border-radius: 4px;
    background: var(--bg);
    color: var(--fg);
    font-size: 0.9rem;
  }

  .input-row button {
    padding: 6px 16px;
    border: 1px solid var(--accent);
    border-radius: 4px;
    background: var(--accent);
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
    background: var(--green-muted);
    color: var(--green);
  }

  .status-badge.unconfigured {
    background: var(--red-muted);
    color: var(--red);
  }

  .msg {
    font-size: 0.8rem;
  }

  .msg.success { color: var(--green); }
  .msg.error { color: var(--red); }

  .note {
    font-size: 0.8rem;
    color: var(--fg-dim);
    margin: 8px 0 0 0;
    font-style: italic;
  }

  .dim { color: var(--fg-dim); }
</style>
