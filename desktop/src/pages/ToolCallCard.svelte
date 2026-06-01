<script lang="ts">
  export interface ToolCallData {
    callId: string;
    toolName: string;
    arguments: string;
    status: "running" | "success" | "failed";
    result?: string;
    error?: string;
  }

  let { tool }: { tool: ToolCallData } = $props();

  let expanded = $state(false);

  function toggle() {
    expanded = !expanded;
  }

  function formatArgs(args: string): string {
    try {
      const parsed = JSON.parse(args);
      return JSON.stringify(parsed, null, 2);
    } catch {
      return args;
    }
  }

  function truncateArgs(args: string): string {
    if (args.length <= 80) return args;
    return args.slice(0, 80) + "...";
  }
</script>

<div class="tool-card" class:expanded class:running={tool.status === "running"} class:success={tool.status === "success"} class:failed={tool.status === "failed"}>
  <button class="tool-header" onclick={toggle}>
    <span class="tool-icon">
      {#if tool.status === "running"}
        <span class="spinner">&#9696;</span>
      {:else if tool.status === "success"}
        <span class="check">&#10003;</span>
      {:else}
        <span class="cross">&#10007;</span>
      {/if}
    </span>
    <span class="tool-name">{tool.toolName}</span>
    <span class="tool-args-preview">
      {#if !expanded}
        ({truncateArgs(tool.arguments)})
      {/if}
    </span>
    <span class="expand-icon">{expanded ? "▲" : "▼"}</span>
  </button>
  {#if expanded}
    <div class="tool-body">
      <div class="tool-section">
        <span class="section-label">Arguments:</span>
        <pre class="args-json">{formatArgs(tool.arguments)}</pre>
      </div>
      {#if tool.status === "success" && tool.result}
        <div class="tool-section">
          <span class="section-label result-label">Result:</span>
          <pre class="result-text">{tool.result}</pre>
        </div>
      {/if}
      {#if tool.status === "failed" && tool.error}
        <div class="tool-section">
          <span class="section-label error-label">Error:</span>
          <pre class="error-text">{tool.error}</pre>
        </div>
      {/if}
    </div>
  {/if}
</div>

<style>
  .tool-card {
    margin: 6px 0;
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
    background: var(--bg);
    overflow: hidden;
    font-size: 13px;
  }

  .tool-card.running {
    border-color: var(--accent);
  }

  .tool-card.success {
    border-color: var(--green);
  }

  .tool-card.failed {
    border-color: var(--red);
  }

  .tool-header {
    display: flex;
    align-items: center;
    gap: 8px;
    width: 100%;
    padding: 8px 12px;
    border: none;
    background: transparent;
    cursor: pointer;
    text-align: left;
    font-family: inherit;
    font-size: 13px;
    color: var(--fg);
  }

  .tool-header:hover {
    background: var(--bg-hover);
  }

  .tool-icon {
    flex-shrink: 0;
    width: 18px;
    height: 18px;
    display: flex;
    align-items: center;
    justify-content: center;
    font-size: 14px;
  }

  .spinner {
    animation: spin 1s linear infinite;
    display: inline-block;
  }

  .check {
    color: var(--green);
    font-weight: bold;
  }

  .cross {
    color: var(--red);
    font-weight: bold;
  }

  .tool-name {
    font-weight: 600;
    flex-shrink: 0;
  }

  .tool-args-preview {
    flex: 1;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    color: var(--fg-dim);
    font-size: 12px;
  }

  .expand-icon {
    flex-shrink: 0;
    color: var(--fg-dim);
    font-size: 10px;
  }

  .tool-body {
    padding: 8px 12px;
    border-top: 1px solid var(--border);
  }

  .tool-section {
    margin-bottom: 8px;
  }

  .tool-section:last-child {
    margin-bottom: 0;
  }

  .section-label {
    font-size: 11px;
    font-weight: 600;
    color: var(--fg-dim);
    text-transform: uppercase;
    letter-spacing: 0.5px;
    display: block;
    margin-bottom: 4px;
  }

  .result-label {
    color: var(--green);
  }

  .error-label {
    color: var(--red);
  }

  .args-json {
    margin: 0;
    font-size: 12px;
    font-family: var(--font-mono);
    white-space: pre-wrap;
    word-break: break-all;
    color: var(--fg);
    line-height: 1.4;
  }

  .result-text {
    margin: 0;
    font-size: 12px;
    white-space: pre-wrap;
    word-break: break-word;
    color: var(--fg);
    line-height: 1.4;
  }

  .error-text {
    margin: 0;
    font-size: 12px;
    white-space: pre-wrap;
    word-break: break-word;
    color: var(--red);
    line-height: 1.4;
  }
</style>
