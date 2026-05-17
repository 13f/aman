<script lang="ts">
  interface TurnStep {
    id: string;
    toolName: string;
    arguments: string;
    status: "running" | "success" | "failed";
    result?: string;
    error?: string;
    timestamp: string;
  }

  let { steps = [] as TurnStep[] }: { steps: TurnStep[] } = $props();

  function formatArgs(args: string): string {
    try {
      const parsed = JSON.parse(args);
      return JSON.stringify(parsed, null, 2);
    } catch {
      return args;
    }
  }

  function statusIcon(status: string): string {
    if (status === "running") return "⟳";  // ⟳
    if (status === "success") return "✓";  // ✓
    return "✗";  // ✗
  }
</script>

<aside class="depth-panel">
  <div class="panel-header">
    <h3>Steps</h3>
    {#if steps.length > 0}
      <span class="step-badge">{steps.length}</span>
    {/if}
  </div>
  <div class="step-list">
    {#if steps.length === 0}
      <p class="empty-hint">Tool call steps appear here during processing.</p>
    {:else}
      {#each steps as step, i (step.id)}
        <details open={i === steps.length - 1}>
          <summary class="step-summary" class:running={step.status === "running"} class:done={step.status !== "running"}>
            <span class="step-icon" class:spin={step.status === "running"}>
              {statusIcon(step.status)}
            </span>
            <span class="step-name">{step.toolName}</span>
            {#if step.status === "running"}
              <span class="step-badge running-badge">running</span>
            {/if}
          </summary>
          <div class="step-body">
            <div class="step-section">
              <span class="section-label">Arguments</span>
              <pre class="section-content">{formatArgs(step.arguments)}</pre>
            </div>
            {#if step.status === "success" && step.result}
              <div class="step-section">
                <span class="section-label result-label">Result</span>
                <pre class="section-content result-text">{step.result}</pre>
              </div>
            {/if}
            {#if step.status === "failed" && step.error}
              <div class="step-section">
                <span class="section-label error-label">Error</span>
                <pre class="section-content error-text">{step.error}</pre>
              </div>
            {/if}
          </div>
        </details>
      {/each}
    {/if}
  </div>
</aside>

<style>
  .depth-panel {
    width: 280px;
    min-width: 280px;
    background: var(--bg-card);
    border-left: 1px solid var(--border);
    display: flex;
    flex-direction: column;
    overflow: hidden;
  }

  .panel-header {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 12px;
    border-bottom: 1px solid var(--border);
    flex-shrink: 0;
  }

  .panel-header h3 {
    margin: 0;
    font-size: 13px;
    font-weight: 600;
  }

  .step-badge {
    font-size: 11px;
    font-weight: 600;
    color: var(--accent);
    background: color-mix(in srgb, var(--accent) 15%, transparent);
    padding: 1px 7px;
    border-radius: 10px;
    line-height: 1.4;
  }

  .step-list {
    flex: 1;
    overflow-y: auto;
    padding: 8px;
  }

  .empty-hint {
    font-size: 12px;
    color: var(--text-secondary);
    text-align: center;
    padding: 24px 8px;
    margin: 0;
  }

  details {
    margin-bottom: 6px;
    border: 1px solid var(--border);
    border-radius: 6px;
    overflow: hidden;
  }

  details:last-child {
    margin-bottom: 0;
  }

  .step-summary {
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 6px 8px;
    cursor: pointer;
    font-size: 12px;
    font-weight: 500;
    user-select: none;
    background: var(--bg);
  }

  .step-summary.running {
    border-left: 2px solid var(--accent);
  }

  .step-summary.done {
    border-left: 2px solid transparent;
  }

  .step-summary:hover {
    background: var(--bg-hover);
  }

  .step-icon {
    flex-shrink: 0;
    width: 16px;
    text-align: center;
    font-size: 13px;
  }

  .step-icon.spin {
    animation: spin 1s linear infinite;
    display: inline-block;
  }

  @keyframes spin {
    from { transform: rotate(0deg); }
    to { transform: rotate(360deg); }
  }

  .step-name {
    flex: 1;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-family: monospace;
    font-size: 12px;
    color: var(--text-primary);
  }

  .running-badge {
    font-size: 10px;
    padding: 0 5px;
  }

  .step-body {
    padding: 6px 8px;
    border-top: 1px solid var(--border);
    background: var(--bg);
  }

  .step-section {
    margin-bottom: 6px;
  }

  .step-section:last-child {
    margin-bottom: 0;
  }

  .section-label {
    display: block;
    font-size: 10px;
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.4px;
    color: var(--text-secondary);
    margin-bottom: 3px;
  }

  .result-label {
    color: var(--green, #22c55e);
  }

  .error-label {
    color: var(--red, #ef4444);
  }

  .section-content {
    margin: 0;
    font-size: 11px;
    font-family: monospace;
    white-space: pre-wrap;
    word-break: break-all;
    color: var(--text-primary);
    line-height: 1.4;
    max-height: 120px;
    overflow-y: auto;
  }

  .result-text {
    color: var(--green);
  }

  .error-text {
    color: var(--red);
  }
</style>
