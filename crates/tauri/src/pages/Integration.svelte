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
  interface ImChannelInstance {
    name: string;
    fields: ImChannelField[];
  }
  interface ImChannel {
    id: string;
    display_name: string;
    instances: ImChannelInstance[];
  }

  let channels = $state<ImChannel[]>([]);
  let channelsLoading = $state(true);
  let chSaving = $state<Record<string, boolean>>({});
  let chInputs = $state<Record<string, string>>({});
  let chMessages = $state<Record<string, { type: "success" | "error"; text: string }>>({});
  let newInstanceInputs = $state<Record<string, string>>({});

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
        for (const inst of ch.instances) {
          for (const f of inst.fields) {
            const inputKey = `${ch.id}:${inst.name}:${f.key}`;
            chInputs[inputKey] = "";
          }
        }
      }
    } catch (e) {
      chMessages["_global"] = { type: "error", text: `Failed to load: ${e}` };
    } finally {
      channelsLoading = false;
    }
  }

  function fieldInputKey(platform: string, instance: string, field: string): string {
    return `${platform}:${instance}:${field}`;
  }

  async function saveChannelField(platform: string, instance: string, fieldKey: string) {
    const inputKey = fieldInputKey(platform, instance, fieldKey);
    const value = chInputs[inputKey]?.trim();
    if (!value) return;

    chSaving[inputKey] = true;
    try {
      await invoke("save_im_channel", { platform, instance, fieldKey, value });
      chMessages[inputKey] = { type: "success", text: "Saved to Keychain" };
      chInputs[inputKey] = "";
      await loadChannels();
    } catch (e) {
      chMessages[inputKey] = { type: "error", text: `${e}` };
    } finally {
      chSaving[inputKey] = false;
    }
  }

  async function deleteChannelField(platform: string, instance: string, fieldKey: string) {
    const inputKey = fieldInputKey(platform, instance, fieldKey);
    chSaving[inputKey] = true;
    try {
      await invoke("delete_im_channel_field", { platform, instance, fieldKey });
      chMessages[inputKey] = { type: "success", text: "Removed from Keychain" };
      await loadChannels();
    } catch (e) {
      chMessages[inputKey] = { type: "error", text: `${e}` };
    } finally {
      chSaving[inputKey] = false;
    }
  }

  async function addInstance(platform: string) {
    const name = newInstanceInputs[platform]?.trim();
    if (!name) return;
    // Creating an instance = saving the first field's token (triggers discovery)
    // The instance will appear when any field is configured.
    // We create it by saving a dummy value and then immediately clearing it,
    // or simply by directing the user to configure a token.
    // Actually, let's add the instance to the list immediately.
    newInstanceInputs[platform] = "";
    // Trigger reload — the instance will appear when at least one field has data.
    // For now, just show the instance entry and let the user fill in fields.
    await loadChannels();
  }

  async function deleteInstance(platform: string, instance: string) {
    const inputKey = fieldInputKey(platform, instance, "__delete__");
    chSaving[inputKey] = true;
    try {
      await invoke("delete_im_channel_instance", { platform, instance });
      chMessages[inputKey] = { type: "success", text: "Instance removed" };
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

  function instanceHasAnyField(inst: ImChannelInstance): boolean {
    return inst.fields.some(f => f.configured);
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
            <span class="status-badge {ch.instances.some(i => instanceHasAnyField(i)) ? 'configured' : 'unconfigured'}">
              {ch.instances.some(i => instanceHasAnyField(i)) ? "Configured" : "Not configured"}
            </span>
          </div>

          <div class="card-body">
            {#each ch.instances as inst (inst.name)}
              <div class="instance-block">
                <div class="instance-header">
                  <span class="instance-name">
                    {inst.name === "default" ? "Default" : inst.name}
                    {#if inst.name === "default"}
                      <span class="dim-label">(always present)</span>
                    {/if}
                  </span>
                  <span class="status-badge small {instanceHasAnyField(inst) ? 'configured' : 'unconfigured'}">
                    {instanceHasAnyField(inst) ? "on" : "off"}
                  </span>
                  {#if inst.name !== "default"}
                    <button
                      class="btn-delete-instance"
                      onclick={() => deleteInstance(ch.id, inst.name)}
                      disabled={chSaving[fieldInputKey(ch.id, inst.name, "__delete__")]}
                    >
                      {chSaving[fieldInputKey(ch.id, inst.name, "__delete__")] ? "..." : "Delete instance"}
                    </button>
                  {/if}
                </div>

                {#each inst.fields as field (field.key)}
                  <div class="config-row">
                    <label for="ch-{ch.id}-{inst.name}-{field.key}">{field.label}</label>
                    <div class="input-row">
                      <input
                        id="ch-{ch.id}-{inst.name}-{field.key}"
                        type="password"
                        placeholder={field.configured ? "•••••••• (replace)" : "Enter value..."}
                        bind:value={chInputs[fieldInputKey(ch.id, inst.name, field.key)]}
                        oninput={() => clearChMessage(fieldInputKey(ch.id, inst.name, field.key))}
                      />
                      <button
                        onclick={() => saveChannelField(ch.id, inst.name, field.key)}
                        disabled={!chInputs[fieldInputKey(ch.id, inst.name, field.key)]?.trim()
                          || chSaving[fieldInputKey(ch.id, inst.name, field.key)]}
                      >
                        {chSaving[fieldInputKey(ch.id, inst.name, field.key)] ? "Saving..." : field.configured ? "Update" : "Save"}
                      </button>
                      {#if field.configured}
                        <button
                          class="btn-clear"
                          onclick={() => deleteChannelField(ch.id, inst.name, field.key)}
                          disabled={chSaving[fieldInputKey(ch.id, inst.name, field.key)]}
                        >
                          ✕
                        </button>
                      {/if}
                    </div>
                    {#if chMessages[fieldInputKey(ch.id, inst.name, field.key)]}
                      <span class="msg {chMessages[fieldInputKey(ch.id, inst.name, field.key)].type}">
                        {chMessages[fieldInputKey(ch.id, inst.name, field.key)].text}
                      </span>
                    {/if}
                  </div>
                {/each}
              </div>
            {/each}

            <!-- Add Instance -->
            <div class="add-instance-row">
              <input
                type="text"
                placeholder="Instance name (e.g. work, personal)..."
                bind:value={newInstanceInputs[ch.id]}
              />
              <button onclick={() => addInstance(ch.id)} disabled={!newInstanceInputs[ch.id]?.trim()}>
                + Add
              </button>
            </div>
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
  .card-list { display: flex; flex-direction: column; gap: 16px; }
  .card.disabled { opacity: 0.45; pointer-events: none; }
  .card-header {
    display: flex; align-items: center; gap: 8px;
    margin-bottom: 12px; flex-wrap: wrap;
  }
  .card-name { font-size: 15px; }
  .card-id {
    font-size: 0.75rem; color: var(--fg-dim);
    background: var(--bg); padding: 2px 6px; border-radius: 4px;
  }
  .platform-icon { font-size: 1.2rem; }
  .card-body { display: flex; flex-direction: column; gap: 12px; }

  /* ── Instances ────────────────────────────────────────────── */
  .instance-block {
    border: 1px solid var(--border-color, #333);
    border-radius: 6px;
    padding: 12px;
    display: flex;
    flex-direction: column;
    gap: 10px;
  }
  .instance-header {
    display: flex; align-items: center; gap: 8px;
    margin-bottom: 2px;
  }
  .instance-name { font-weight: 600; font-size: 0.9rem; }
  .dim-label { font-size: 0.75rem; color: var(--fg-dim); font-weight: 400; margin-left: 4px; }

  .btn-delete-instance {
    margin-left: auto;
    padding: 3px 10px;
    border: 1px solid #f44336;
    border-radius: 4px;
    background: transparent;
    color: #f44336;
    cursor: pointer;
    font-size: 0.75rem;
  }
  .btn-delete-instance:hover { background: #f44336; color: #fff; }

  .add-instance-row {
    display: flex; gap: 8px; align-items: stretch;
    margin-top: 4px;
  }
  .add-instance-row input {
    flex: 1; padding: 6px 10px;
    border: 1px dashed var(--border-color, #444);
    border-radius: 4px;
    background: var(--bg); color: var(--fg);
    font-size: 0.85rem;
  }
  .add-instance-row button {
    padding: 6px 16px;
    border: 1px solid var(--accent);
    border-radius: 4px;
    background: transparent;
    color: var(--accent);
    cursor: pointer;
    font-size: 0.85rem;
    white-space: nowrap;
  }
  .add-instance-row button:disabled { opacity: 0.5; cursor: not-allowed; }

  /* ── Config rows ──────────────────────────────────────────── */
  .config-row { display: flex; flex-direction: column; gap: 4px; }
  .config-row label { font-size: 0.8rem; color: var(--fg-dim); }

  .input-row { display: flex; gap: 8px; align-items: stretch; }
  .input-row input {
    flex: 1; padding: 6px 10px;
    border: 1px solid var(--border);
    border-radius: 4px;
    background: var(--bg); color: var(--fg);
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
  .input-row button:disabled { opacity: 0.5; cursor: not-allowed; }

  .btn-clear {
    padding: 6px 10px !important;
    border: 1px solid #f44336 !important;
    background: transparent !important;
    color: #f44336 !important;
    font-size: 0.85rem;
  }
  .btn-clear:hover { background: #f44336 !important; color: #fff !important; }

  /* ── Tags ───────────────────────────────────────────────── */
  .tag-list { display: flex; gap: 4px; margin-left: auto; }
  .tag {
    font-size: 0.7rem; font-weight: 600; text-transform: uppercase;
    letter-spacing: 0.03em; padding: 2px 8px; border-radius: 10px;
    background: #1a3a5a; color: #6ab0ff;
  }

  /* ── Status ──────────────────────────────────────────────── */
  .status-badge {
    font-size: 0.75rem; padding: 2px 8px; border-radius: 10px;
    display: inline-block; width: fit-content;
  }
  .status-badge.small { font-size: 0.65rem; padding: 1px 6px; }
  .status-badge.configured { background: #1a3a1a; color: #4caf50; }
  .status-badge.unconfigured { background: #3a1a1a; color: #f44336; }

  /* ── Messages ────────────────────────────────────────────── */
  .msg { font-size: 0.8rem; }
  .msg.success { color: #4caf50; }
  .msg.error { color: #f44336; }

  .note {
    font-size: 0.8rem; color: var(--fg-dim);
    margin: 8px 0 0 0; font-style: italic;
  }
  .dim { color: var(--fg-dim); }
</style>
