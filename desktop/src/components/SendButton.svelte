<script lang="ts">
  let {
    buttonText = "Send",
    stopText = "Stop",
    rateLimitCountdown = 0,
    isProcessing = false,
    disabled = false,
    onsend = () => {},
    onstop = () => {},
  }: {
    buttonText?: string;
    stopText?: string;
    rateLimitCountdown?: number;
    isProcessing?: boolean;
    disabled?: boolean;
    onsend?: () => void;
    onstop?: () => void;
  } = $props();
</script>

{#if rateLimitCountdown > 0}
  <button class="rate-limited-btn" disabled>{rateLimitCountdown}s</button>
{:else if isProcessing}
  <button class="stop-btn" onclick={onstop}>{stopText}</button>
{:else}
  <button class="send-btn" onclick={onsend} disabled={disabled}>{buttonText}</button>
{/if}

<style>
  .send-btn {
    padding: 8px 20px;
    border: none;
    border-radius: 8px;
    background: var(--accent);
    color: #fff;
    font-size: 13px;
    font-weight: 500;
    cursor: pointer;
    align-self: flex-end;
  }

  .send-btn:disabled {
    opacity: 0.5;
    cursor: not-allowed;
  }

  .send-btn:hover:not(:disabled) {
    background: var(--accent-hover);
  }

  .stop-btn {
    padding: 8px 20px;
    border: 1px solid var(--red);
    border-radius: 8px;
    background: transparent;
    color: var(--red);
    font-size: 13px;
    font-weight: 500;
    cursor: pointer;
    align-self: flex-end;
  }

  .stop-btn:hover {
    background: var(--red-muted);
  }

  .rate-limited-btn {
    padding: 8px 20px;
    border: 1px solid var(--yellow);
    border-radius: 8px;
    background: var(--yellow-muted);
    color: var(--yellow);
    font-size: 13px;
    font-weight: 600;
    cursor: not-allowed;
    align-self: flex-end;
  }
</style>
