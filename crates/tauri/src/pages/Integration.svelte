<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import { onMount } from "svelte";

  // ── Third Party Services state ───────────────────────────────────
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
  let servicesLoading = $state(true);
  let saving = $state<Record<string, boolean>>({});
  let savingCx = $state<Record<string, boolean>>({});
  let svcMessages = $state<Record<string, { type: "success" | "error"; text: string }>>({});
  let keyInputs = $state<Record<string, string>>({});
  let cxInputs = $state<Record<string, string>>({});

  // ── IM Channels state ────────────────────────────────────────────
  interface ImChannelField {
    key: string;
    label: string;
    configured: boolean;
  }
  interface ImChannel {
    id: string;
    display_name: string;
    enabled: boolean;
    fields: ImChannelField[];
  }

  let channels = $state<ImChannel[]>([]);
  let channelsLoading = $state(true);
  let chSaving = $state<Record<string, boolean>>({});
  let chInputs = $state<Record<string, string>>({});
  let chMessages = $state<Record<string, { type: "success" | "error"; text: string }>>({});

  // ── Tab state ────────────────────────────────────────────────────
  let activeTab = $state<"services" | "channels">("services");

  onMount(async () => {
    await Promise.all([loadServices(), loadChannels()]);
  });

  // ── Third Party Services ─────────────────────────────────────────

  async function loadServices() {
    servicesLoading = true;
    try {
      services = await invoke<Service[]>("list_third_party_services");
      for (const s of services) {
        keyInputs[s.id] = "";
        cxInputs[s.id] = s.has_cx ? "******" : "";
      }
    } catch (e) {
      svcMessages["_global"] = { type: "error", text: `Failed to load: ${e}` };
    } finally {
      servicesLoading = false;
    }
  }

  async function saveKey(serviceId: string) {
    const value = keyInputs[serviceId]?.trim();
    if (!value) return;
    saving[serviceId] = true;
    try {
      await invoke("set_third_party_key", { service: serviceId, apiKey: value });
      svcMessages[serviceId] = { type: "success", text: "Saved to Keychain" };
      keyInputs[serviceId] = "";
      await loadServices();
    } catch (e) {
      svcMessages[serviceId] = { type: "error", text: `${e}` };
    } finally {
      saving[serviceId] = false;
    }
  }

  async function saveCx(serviceId: string) {
    const value = cxInputs[serviceId]?.trim();
    if (!value) return;
    savingCx[serviceId] = true;
    try {
      await invoke("set_third_party_config", { service: serviceId, subKey: "cx", value });
      svcMessages[`${serviceId}-cx`] = { type: "success", text: "Saved to Keychain" };
      cxInputs[serviceId] = "";
      await loadServices();
    } catch (e) {
      svcMessages[`${serviceId}-cx`] = { type: "error", text: `${e}` };
    } finally {
      savingCx[serviceId] = false;
    }
  }

  function clearSvcMessage(key: string) {
    delete svcMessages[key];
    svcMessages = { ...svcMessages };
  }

  // ── IM Channels ───────────────────────────────────────────────────

  async function loadChannels() {
    channelsLoading = true;
    try {
      channels = await invoke<ImChannel[]>("list_im_channels");
      for (const ch of channels) {
        for (const f of ch.fields) {
          chInputs[`${ch.id}.${f.key}`] = "";
        }
      }
    } catch (e) {
      chMessages["_global"] = { type: "error", text: `Failed to load: ${e}` };
    } finally {
      channelsLoading = false;
    }
  }

  async function saveChannelField(platform: string, fieldKey: string) {
    const inputKey = `${platform}.${fieldKey}`;
    const value = chInputs[inputKey]?.trim();
    if (!value) return;

    chSaving[inputKey] = true;
    try {
      await invoke("save_im_channel", { platform, fieldKey, value });
      chMessages[inputKey] = { type: "success", text: "Saved to Keychain" };
      chInputs[inputKey] = "";
      await loadChannels();
    } catch (e) {
      chMessages[inputKey] = { type: "error", text: `${e}` };
    } finally {
      chSaving[inputKey] = false;
    }
  }

  async function deleteChannelField(platform: string, fieldKey: string) {
    const inputKey = `${platform}.${fieldKey}`;
    chSaving[inputKey] = true;
    try {
      await invoke("delete_im_channel_field", { platform, fieldKey });
      chMessages[inputKey] = { type: "success", text: "Removed from Keychain" };
      await loadChannels();
    } catch (e) {
      chMessages[inputKey] = { type: "error", text: `${e}` };
    } finally {
      chSaving[inputKey] = false;
    }
  }

  function clearChMessage(key: string) {
    delete chMessages[key];
    chMessages = { ...chMessages };
  }

  function platformIcon(id: string): string {
    switch (id) {
      case "telegram": return "✈";
      case "slack": return "💬";
      case "discord": return "🎮";
      case "matrix": return "🔗";
      default: return "📡";
    }
  }
</script>

<div class="page-header">
  <h2>Integration</h2>
</div>

<!-- ── Tabs ────────────────────────────────────────────────────── -->
<div class="tabs">
  <button
    class="tab"
    class:active={activeTab === "services"}
    onclick={() => (activeTab = "services")}
  >
    3rd Services
  </button>
  <button
    class="tab"
    class:active={activeTab === "channels"}
    onclick={() => (activeTab = "channels")}
  >
    IM Channels
  </button>
</div>

<!-- ── 3rd Services Tab ─────────────────────────────────────────── -->
{#if activeTab === "services"}
  {#if servicesLoading}
    <p class="dim">Loading...</p>
  {:else}
    <div class="card-list">
      {#each services as service (service.id)}
        <div class="card service-card" class:disabled={service.disabled}>
          <div class="card-header">
            <strong class="card-name">{service.display_name}</strong>
            <span class="card-id">{service.id}</span>
            <div class="tag-list">
              {#each service.tags as tag}
                <span class="tag">{tag}</span>
              {/each}
            </div>
          </div>

          <div class="card-body">
            <!-- API Key -->
            <div class="config-row">
              <label for="key-{service.id}">API Key</label>
              <div class="input-row">
                <input
                  id="key-{service.id}"
                  type="password"
                  placeholder={service.has_key ? "•••••••• (replace)" : service.requires_key ? "Required" : "Optional"}
                  bind:value={keyInputs[service.id]}
                  oninput={() => clearSvcMessage(service.id)}
                />
                <button
                  onclick={() => saveKey(service.id)}
                  disabled={!keyInputs[service.id]?.trim() || saving[service.id]}
                >
                  {saving[service.id] ? "Saving..." : service.has_key ? "Update" : "Save"}
                </button>
              </div>
              {#if svcMessages[service.id]}
                <span class="msg {svcMessages[service.id].type}">{svcMessages[service.id].text}</span>
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
                    oninput={() => clearSvcMessage(`${service.id}-cx`)}
                  />
                  <button
                    onclick={() => saveCx(service.id)}
                    disabled={!cxInputs[service.id]?.trim() || savingCx[service.id]}
                  >
                    {savingCx[service.id] ? "Saving..." : service.has_cx ? "Update" : "Save"}
                  </button>
                </div>
                {#if svcMessages[`${service.id}-cx`]}
                  <span class="msg {svcMessages[`${service.id}-cx`].type}">{svcMessages[`${service.id}-cx`].text}</span>
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
{/if}

<!-- ── IM Channels Tab ──────────────────────────────────────────── -->
{#if activeTab === "channels"}
  {#if channelsLoading}
    <p class="dim">Loading...</p>
  {:else}
    <div class="card-list">
      {#each channels as ch (ch.id)}
        <div class="card channel-card">
          <div class="card-header">
            <span class="platform-icon">{platformIcon(ch.id)}</span>
            <strong class="card-name">{ch.display_name}</strong>
            <span class="card-id">{ch.id}</span>
            <span class="status-badge {ch.fields.some(f => f.configured) ? 'configured' : 'unconfigured'}">
              {ch.fields.some(f => f.configured) ? "Configured" : "Not configured"}
            </span>
          </div>

          <div class="card-body">
            {#each ch.fields as field (field.key)}
              <div class="config-row">
                <label for="ch-{ch.id}-{field.key}">{field.label}</label>
                <div class="input-row">
                  <input
                    id="ch-{ch.id}-{field.key}"
                    type="password"
                    placeholder={field.configured ? "•••••••• (replace)" : "Enter value..."}
                    bind:value={chInputs[`${ch.id}.${field.key}`]}
                    oninput={() => clearChMessage(`${ch.id}.${field.key}`)}
                  />
                  <button
                    onclick={() => saveChannelField(ch.id, field.key)}
                    disabled={!chInputs[`${ch.id}.${field.key}`]?.trim() || chSaving[`${ch.id}.${field.key}`]}
                  >
                    {chSaving[`${ch.id}.${field.key}`] ? "Saving..." : field.configured ? "Update" : "Save"}
                  </button>
                  {#if field.configured}
                    <button
                      class="btn-delete"
                      onclick={() => deleteChannelField(ch.id, field.key)}
                      disabled={chSaving[`${ch.id}.${field.key}`]}
                    >
                      ✕
                    </button>
                  {/if}
                </div>
                {#if chMessages[`${ch.id}.${field.key}`]}
                  <span class="msg {chMessages[`${ch.id}.${field.key}`].type}">
                    {chMessages[`${ch.id}.${field.key}`].text}
                  </span>
                {/if}
                <span class="status-badge {field.configured ? 'configured' : 'unconfigured'}">
                  {field.configured ? "Configured" : "Not configured"}
                </span>
              </div>
            {/each}
          </div>
        </div>
      {/each}
    </div>
  {/if}
{/if}

<style>
  .page-header {
    display: flex;
    justify-content: space-between;
    align-items: center;
    margin-bottom: 20px;
  }
  .page-header h2 { margin: 0; font-size: 18px; }

  /* ── Tabs ───────────────────────────────────────────────── */
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

  .tab:hover { color: var(--text-primary, #fff); }

  .tab.active {
    color: var(--text-primary, #fff);
    border-bottom-color: var(--accent, #4a9eff);
  }

  /* ── Cards ───────────────────────────────────────────────── */
  .card-list {
    display: flex;
    flex-direction: column;
    gap: 16px;
  }

  .card.disabled {
    opacity: 0.45;
    pointer-events: none;
  }

  .card-header {
    display: flex;
    align-items: center;
    gap: 8px;
    margin-bottom: 12px;
    flex-wrap: wrap;
  }

  .card-name { font-size: 15px; }

  .card-id {
    font-size: 0.75rem;
    color: var(--fg-dim);
    background: var(--bg);
    padding: 2px 6px;
    border-radius: 4px;
  }

  .platform-icon { font-size: 1.2rem; }

  .card-body {
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

  .btn-delete {
    padding: 6px 10px !important;
    border: 1px solid #f44336 !important;
    background: transparent !important;
    color: #f44336 !important;
    font-size: 0.85rem;
  }

  .btn-delete:hover { background: #f44336 !important; color: #fff !important; }

  /* ── Tags ───────────────────────────────────────────────── */
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

  /* ── Status ──────────────────────────────────────────────── */
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

  /* ── Messages ────────────────────────────────────────────── */
  .msg { font-size: 0.8rem; }
  .msg.success { color: #4caf50; }
  .msg.error { color: #f44336; }

  .note {
    font-size: 0.8rem;
    color: var(--fg-dim);
    margin: 8px 0 0 0;
    font-style: italic;
  }

  .dim { color: var(--fg-dim); }
</style>
