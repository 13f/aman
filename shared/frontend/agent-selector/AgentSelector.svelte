<svelte:options customElement="agent-selector" />

<script lang="ts">
  interface AgentInfo {
    id: string;
    name: string;
    model?: string;
  }

  interface Props {
    gatewayUrl?: string;
    placeholder?: string;
    selected?: string;
  }

  let {
    gatewayUrl = "",
    placeholder = "Select Agent",
    selected = $bindable(""),
  }: Props = $props();

  let baseUrl = $derived(gatewayUrl || (typeof window !== "undefined" ? window.location.origin : "http://localhost:9999"));

  let open = $state(false);
  let agents = $state<AgentInfo[]>([]);
  let loading = $state(false);
  let error = $state("");
  let hostEl: HTMLElement | undefined = $state();

  function dispatch(name: string, detail?: unknown) {
    hostEl?.dispatchEvent(
      new CustomEvent(name, { detail, bubbles: true, composed: true })
    );
  }

  async function fetchAgents() {
    if (agents.length > 0) return;
    loading = true;
    error = "";
    try {
      const resp = await fetch(baseUrl.replace(/\/+$/, "") + "/api/v1/agents");
      if (!resp.ok) throw new Error("HTTP " + resp.status);
      const list: any[] = await resp.json();
      agents = [];
      for (const a of list) {
        const id = a?.descriptor?.agent_id || a?.id || a?.key || "";
        const name = a?.descriptor?.display_name || a?.display_name || id;
        const model = a?.descriptor?.model || a?.model || "";
        if (id) agents.push({ id, name, model });
      }
    } catch (e: any) {
      error = e.message || "Failed to load agents";
    } finally {
      loading = false;
    }
  }

  function toggle() {
    open = !open;
    if (open) fetchAgents();
  }

  function close() {
    open = false;
  }

  function select(agent: AgentInfo) {
    selected = agent.id;
    open = false;
    dispatch("select", { id: agent.id, name: agent.name, model: agent.model });
  }

  // Close on outside click
  function handleClickOutside(e: MouseEvent) {
    if (hostEl && !hostEl.contains(e.target as Node)) {
      open = false;
    }
  }

  $effect(() => {
    if (open) {
      document.addEventListener("click", handleClickOutside);
    } else {
      document.removeEventListener("click", handleClickOutside);
    }
  });
</script>

<div bind:this={hostEl} class="agent-selector-host">
  <button class="trigger" onclick={toggle} type="button">
    <span class="trigger-icon">&#x1F916;</span>
    <span class="trigger-label">{selected
      ? (agents.find(a => a.id === selected)?.name || selected)
      : placeholder}</span>
    <span class="trigger-chevron">{open ? "▴" : "▾"}</span>
  </button>

  {#if open}
    <div class="dropdown">
      {#if loading}
        <div class="dropdown-status">Loading agents&hellip;</div>
      {:else if error}
        <div class="dropdown-status error">{error}</div>
      {:else if agents.length === 0}
        <div class="dropdown-status">No agents found</div>
      {:else}
        {#each agents as agent}
          <button
            class="agent-option"
            class:selected={selected === agent.id}
            onclick={() => select(agent)}
            type="button"
          >
            <span class="agent-name">{agent.name}</span>
            {#if agent.model}
              <span class="agent-model">{agent.model}</span>
            {/if}
            <span class="agent-id">{agent.id}</span>
          </button>
        {/each}
      {/if}
    </div>
  {/if}
</div>

<style>
  :host {
    display: inline-block;
    position: relative;
  }

  .agent-selector-host {
    position: relative;
  }

  .trigger {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    padding: 6px 12px;
    background: var(--as-trigger-bg, #161822);
    border: 1px solid var(--as-trigger-border, #2a2d3e);
    border-radius: 6px;
    color: var(--as-trigger-fg, #e1e4ed);
    font-size: 13px;
    font-family: inherit;
    cursor: pointer;
    transition: border-color 0.15s;
  }

  .trigger:hover {
    border-color: var(--as-accent, #6366f1);
  }

  .trigger-icon {
    font-size: 14px;
  }

  .trigger-label {
    font-weight: 500;
  }

  .trigger-chevron {
    font-size: 10px;
    color: var(--as-muted, #6b7085);
  }

  .dropdown {
    position: absolute;
    top: calc(100% + 4px);
    left: 0;
    min-width: 280px;
    max-width: 360px;
    max-height: 320px;
    overflow-y: auto;
    background: var(--as-dropdown-bg, #1a1d2e);
    border: 1px solid var(--as-dropdown-border, #2a2d3e);
    border-radius: 8px;
    box-shadow: 0 8px 24px rgba(0, 0, 0, 0.4);
    z-index: 200;
    padding: 4px;
  }

  .dropdown-status {
    padding: 16px;
    text-align: center;
    font-size: 13px;
    color: var(--as-muted, #6b7085);
  }

  .dropdown-status.error {
    color: var(--as-danger, #ef4444);
  }

  .agent-option {
    display: flex;
    align-items: center;
    gap: 8px;
    width: 100%;
    padding: 8px 10px;
    background: none;
    border: none;
    border-radius: 6px;
    color: var(--as-fg, #e1e4ed);
    font-size: 13px;
    font-family: inherit;
    cursor: pointer;
    transition: background 0.1s;
    text-align: left;
  }

  .agent-option:hover {
    background: var(--as-option-hover, rgba(99,102,241,0.1));
  }

  .agent-option.selected {
    background: var(--as-option-selected, rgba(99,102,241,0.15));
  }

  .agent-name {
    font-weight: 600;
  }

  .agent-model {
    font-size: 11px;
    color: var(--as-muted, #6b7085);
  }

  .agent-id {
    margin-left: auto;
    font-size: 11px;
    color: var(--as-muted, #6b7085);
    font-family: monospace;
    background: var(--as-badge-bg, rgba(255,255,255,0.05));
    padding: 1px 6px;
    border-radius: 3px;
  }
</style>
