<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";

  interface WorkflowEntry {
    id: string;
    workflow_name: string;
    current_state: string;
    status: string;
  }

  interface TransitionInfo {
    from: string;
    event: string;
    to: string;
    guard: string | null;
    has_action: boolean;
  }

  interface StateTimeoutInfo {
    state: string;
    timeout_ms: number;
    on_timeout: string;
  }

  interface WorkflowDefInfo {
    name: string;
    states: string[];
    initial_state: string;
    final_states: string[];
    error_state: string;
    transitions: TransitionInfo[];
    state_timeouts: StateTimeoutInfo[];
  }

  let instances = $state<WorkflowEntry[]>([]);
  let loading = $state(false);
  let result = $state("");
  let autoRefresh = $state(false);
  let autoTimer: ReturnType<typeof setInterval> | undefined;
  let selectedInstance = $state<string | null>(null);
  let workflowDefs = $state<Record<string, WorkflowDefInfo>>({});
  let defLoading = $state<Set<string>>(new Set());

  function toggleAuto() {
    autoRefresh = !autoRefresh;
    if (autoRefresh) {
      loadInstances();
      autoTimer = setInterval(loadInstances, 3000);
    } else {
      if (autoTimer) clearInterval(autoTimer);
      autoTimer = undefined;
    }
  }

  async function loadInstances() {
    loading = true;
    try {
      instances = await invoke<WorkflowEntry[]>("get_workflow_instances");
    } catch (e: any) {
      if (!autoRefresh) result = String(e);
    } finally {
      loading = false;
    }
  }

  async function loadWorkflowDef(name: string) {
    if (workflowDefs[name]) return;
    defLoading.add(name);
    defLoading = defLoading;
    try {
      const def = await invoke<WorkflowDefInfo>("get_workflow_def", { name });
      workflowDefs[name] = def;
      workflowDefs = workflowDefs;
    } catch {
      // not all workflows may have definitions available
    } finally {
      defLoading.delete(name);
      defLoading = defLoading;
    }
  }

  async function doRetry(id: string) {
    try {
      result = await invoke<string>("retry_workflow", { id });
      await loadInstances();
    } catch (e: any) {
      result = String(e);
    }
  }

  async function doCancel(id: string) {
    try {
      result = await invoke<string>("cancel_workflow", { id });
      await loadInstances();
    } catch (e: any) {
      result = String(e);
    }
  }

  function stateClass(state: string, def: WorkflowDefInfo | undefined): string {
    if (!def) return "state-default";
    const upper = state.toUpperCase();
    if (upper === def.initial_state.toUpperCase()) return "state-initial";
    if (upper === def.error_state.toUpperCase()) return "state-error";
    if (def.final_states.some((fs) => fs.toUpperCase() === upper)) return "state-final";
    if (def.state_timeouts.some((st) => st.state.toUpperCase() === upper)) return "state-timed";
    return "state-active";
  }

  function truncate(id: string): string {
    return id.length > 8 ? id.slice(0, 8) + "…" : id;
  }

  function stateColor(state: string, def: WorkflowDefInfo): string {
    const upper = state.toUpperCase();
    if (upper === def.initial_state.toUpperCase()) return 'var(--bg-hover)';
    if (upper === def.error_state.toUpperCase()) return 'var(--red-muted)';
    if (def.final_states.some((fs) => fs.toUpperCase() === upper)) return 'var(--green-muted)';
    if (def.state_timeouts.some((st) => st.state.toUpperCase() === upper)) return 'var(--yellow-muted)';
    return 'var(--accent-muted)';
  }

  function selectInstance(id: string) {
    selectedInstance = id;
    const inst = instances.find((i) => i.id === id);
    if (inst) loadWorkflowDef(inst.workflow_name);
  }
</script>

<div class="card" style="display:flex;align-items:center;justify-content:space-between;">
  <h2>Workflow Instances</h2>
  <div style="display:flex;gap:8px;align-items:center;">
    <label style="font-size:13px;display:flex;align-items:center;gap:4px;cursor:pointer;">
      <input type="checkbox" checked={autoRefresh} onchange={toggleAuto} />
      Auto
    </label>
    <button class="secondary" onclick={loadInstances} disabled={loading}>Refresh</button>
  </div>
</div>

{#if result}
  <div class="card">
    <p style="font-size:13px;color:var(--accent);">{result}</p>
  </div>
{/if}

<div class="card">
  {#if instances.length === 0}
    <p style="color:var(--fg-dim);font-size:13px;">No workflow instances found. Click "Refresh" to check.</p>
  {:else}
    <table>
      <thead>
        <tr><th>ID</th><th>Workflow</th><th>State</th><th>Status</th><th>Actions</th></tr>
      </thead>
      <tbody>
        {#each instances as inst}
          <tr>
            <td style="font-family:monospace;font-size:12px;">
              {truncate(inst.id)}
            </td>
            <td>{inst.workflow_name}</td>
            <td>
              <span class="badge state-badge {stateClass(inst.current_state, workflowDefs[inst.workflow_name])}">
                {inst.current_state}
              </span>
            </td>
            <td><span class="badge {inst.status === 'running' ? 'ok' : 'warn'}">{inst.status}</span></td>
            <td>
              <button style="margin-right:4px;font-size:11px;padding:2px 8px;" onclick={() => doRetry(inst.id)}>Retry</button>
              <button class="danger" style="font-size:11px;padding:2px 8px;margin-right:4px;" onclick={() => doCancel(inst.id)}>Cancel</button>
              <button class="secondary" style="font-size:11px;padding:2px 8px;" onclick={() => selectInstance(inst.id)}>Detail</button>
            </td>
          </tr>
        {/each}
      </tbody>
    </table>
  {/if}
</div>

<!-- Detail panel -->
{#if selectedInstance}
  {@const inst = instances.find((i) => i.id === selectedInstance)}
  {#if inst}
    {@const def = workflowDefs[inst.workflow_name]}
    <div class="card" style="margin-top:12px;">
      <div style="display:flex;align-items:center;justify-content:space-between;">
        <h3 style="margin:0;">{inst.workflow_name} — Detail</h3>
        <button class="secondary" style="font-size:11px;" onclick={() => { selectedInstance = null; }}>Close</button>
      </div>
      <p style="font-size:12px;color:var(--fg-dim);margin:8px 0;">
        Instance: <span style="font-family:monospace;">{inst.id}</span>
        &nbsp;|&nbsp; Current State: <strong>{inst.current_state}</strong>
        &nbsp;|&nbsp; Status: <strong>{inst.status}</strong>
      </p>

      {#if def}
        <div style="margin-top:12px;">
          <!-- State machine diagram -->
          <div style="display:flex;flex-wrap:wrap;gap:8px;align-items:center;margin-bottom:12px;padding:12px;background:var(--bg-darker, #f5f5f5);border-radius:6px;">
            {#each def.states as state}
              {@const isActive = state.toUpperCase() === inst.current_state.toUpperCase()}
              <div style="display:flex;align-items:center;gap:4px;">
                <div style="padding:6px 12px;border-radius:4px;font-size:12px;font-weight:{isActive ? 'bold' : 'normal'};border:2px solid {isActive ? '#4a9eff' : 'transparent'};background:{stateColor(state, def)};">
                  {state}
                </div>
                {#if def.transitions.filter(t => t.from.toUpperCase() === state.toUpperCase() || (t.from === '__ANY__')).length > 0}
                  <div style="display:flex;flex-direction:column;gap:2px;font-size:10px;color:var(--fg-dim);">
                    {#each def.transitions.filter(t => t.from.toUpperCase() === state.toUpperCase() || t.from === '__ANY__') as trans}
                      <span style="white-space:nowrap;">
                        [{trans.event}]
                        &rarr; {trans.to === '__LAST__' ? 'last' : trans.to}
                        {#if trans.has_action}
                          <span title="Has action">⚡</span>
                        {/if}
                      </span>
                    {/each}
                  </div>
                {/if}
              </div>
            {/each}
          </div>

          <!-- Timeouts -->
          {#if def.state_timeouts.length > 0}
            <div style="margin-bottom:8px;">
              <p style="font-size:12px;font-weight:bold;margin-bottom:4px;">⏱ State Timeouts</p>
              {#each def.state_timeouts as st}
                <span style="font-size:11px;display:inline-block;margin-right:8px;padding:2px 6px;background:var(--bg-darker, #f5f5f5);border-radius:3px;">
                  {st.state} → {st.on_timeout} ({(st.timeout_ms / 1000).toFixed(1)}s)
                </span>
              {/each}
            </div>
          {/if}
        </div>
      {:else if defLoading.has(inst.workflow_name)}
        <p style="font-size:12px;color:var(--fg-dim);">Loading workflow definition…</p>
      {:else}
        <p style="font-size:12px;color:var(--fg-dim);">Workflow definition not available.</p>
      {/if}
    </div>
  {/if}
{/if}

<style>
  :global(.state-badge.state-initial) {
    background: var(--bg-hover);
    color: var(--fg-dim);
  }
  :global(.state-badge.state-active) {
    background: var(--accent-muted);
    color: var(--accent);
  }
  :global(.state-badge.state-timed) {
    background: var(--yellow-muted);
    color: var(--yellow);
  }
  :global(.state-badge.state-error) {
    background: var(--red-muted);
    color: var(--red);
  }
  :global(.state-badge.state-final) {
    background: var(--green-muted);
    color: var(--green);
  }
  :global(.state-badge.state-default) {
    background: var(--bg-hover);
    color: var(--fg-dim);
  }
</style>
