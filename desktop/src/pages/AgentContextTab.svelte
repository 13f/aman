<script lang="ts">
  import { getAgentContext } from "../lib/agent-context.svelte";

  const { agentKey }: { agentKey: string } = $props();

  // Reactive: re-renders whenever a fresh snapshot lands for this agent.
  const snapshot = $derived(getAgentContext(agentKey));
</script>

<div class="context-tab">
  {#if !snapshot}
    <div class="context-empty">
      No context yet — send a message and the agent's working context will appear here.
    </div>
  {:else}
    <pre class="context-text">{JSON.stringify(snapshot, null, 2)}</pre>
  {/if}
</div>

<style>
  .context-tab {
    height: 100%;
    min-height: 0;
    display: flex;
    flex-direction: column;
  }

  .context-empty {
    flex: 1;
    display: flex;
    align-items: center;
    justify-content: center;
    padding: 24px;
    text-align: center;
    font-size: 13px;
    color: var(--fg-dim, #9ca3af);
  }

  .context-text {
    flex: 1;
    margin: 0;
    padding: 16px 20px;
    overflow: auto;
    white-space: pre-wrap;
    word-break: break-word;
    font-family: var(--font-mono, ui-monospace, Menlo, monospace);
    font-size: 12px;
    line-height: 1.6;
    color: var(--fg, #e5e7eb);
  }
</style>
