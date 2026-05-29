<svelte:options customElement="chat-input" />

<script lang="ts">
  interface Props {
    placeholder?: string;
    disabled?: boolean;
    rows?: number;
    buttonText?: string;
    stopText?: string;
    processing?: string | undefined;
    rateLimit?: number;
    value?: string;
    onsend?: (text: string) => void;
    onstop?: () => void;
    oninput?: (text: string) => void;
    onkeydown?: (e: KeyboardEvent) => void;
  }

  let {
    placeholder = "",
    disabled = false,
    rows = 1,
    buttonText = "Send",
    stopText = "Stop",
    processing = undefined,
    rateLimit = 0,
    value = $bindable(""),
    onsend,
    onstop,
    oninput,
    onkeydown,
  }: Props = $props();

  let textareaEl: HTMLTextAreaElement | undefined = $state();

  function autoGrow() {
    if (!textareaEl) return;
    textareaEl.style.height = "auto";
    textareaEl.style.height = Math.min(textareaEl.scrollHeight, 160) + "px";
  }

  $effect(() => {
    // Re-sync textarea when value changes externally
    if (textareaEl && textareaEl.value !== value) {
      textareaEl.value = value;
      autoGrow();
    }
  });

  function handleInput(e: Event) {
    const text = (e.target as HTMLTextAreaElement).value;
    value = text;
    autoGrow();
    oninput?.(text);
  }

  function handleKeydown(e: KeyboardEvent) {
    if ((e.ctrlKey || e.metaKey) && e.key === "Enter") {
      e.preventDefault();
      doSend();
      return;
    }
    onkeydown?.(e);
  }

  function handleButtonClick() {
    if (rateLimit > 0) return;
    if (processing !== undefined) {
      onstop?.();
    } else {
      doSend();
    }
  }

  function doSend() {
    const text = value.trim();
    if (!text) return;
    onsend?.(text);
  }

  export function focus() {
    textareaEl?.focus();
  }
</script>

<textarea
  bind:this={textareaEl}
  {placeholder}
  {disabled}
  rows={rows}
  oninput={handleInput}
  onkeydown={handleKeydown}
></textarea>

{#if rateLimit > 0}
  <button class="rate-limited-btn" disabled>{rateLimit}s</button>
{:else if processing !== undefined}
  <button class="stop-btn" onclick={handleButtonClick}>{stopText}</button>
{:else}
  <button
    class="send-btn"
    disabled={!value.trim()}
    onclick={handleButtonClick}
  >{buttonText}</button>
{/if}

<style>
  :host {
    display: flex;
    gap: 8px;
    align-items: flex-end;
    width: 100%;
  }

  textarea {
    flex: 1;
    resize: none;
    padding: 10px 14px;
    border: 1px solid var(--chat-input-border, #e2e8f0);
    border-radius: 10px;
    font-size: 14px;
    line-height: 1.5;
    font-family: inherit;
    background: var(--chat-input-bg, #fff);
    color: var(--chat-input-fg, #1e293b);
    outline: none;
    transition: border-color 0.15s;
    min-height: 42px;
    max-height: 160px;
    overflow-y: auto;
  }

  textarea:focus {
    border-color: var(--chat-input-accent, #3b82f6);
    box-shadow: 0 0 0 3px rgba(59, 130, 246, 0.15);
  }

  textarea:disabled {
    opacity: 0.6;
    cursor: not-allowed;
    background: var(--chat-input-disabled-bg, #f8fafc);
  }

  button {
    padding: 8px 20px;
    border: none;
    border-radius: 8px;
    font-size: 13px;
    font-weight: 500;
    cursor: pointer;
    white-space: nowrap;
    align-self: flex-end;
    min-height: 38px;
  }

  .send-btn {
    background: var(--chat-input-accent, #3b82f6);
    color: #fff;
  }

  .send-btn:hover:not(:disabled) {
    background: var(--chat-input-accent-hover, #2563eb);
  }

  .send-btn:disabled {
    opacity: 0.5;
    cursor: not-allowed;
  }

  .stop-btn {
    border: 1px solid var(--chat-input-red, #ef4444);
    background: transparent;
    color: var(--chat-input-red, #ef4444);
  }

  .stop-btn:hover {
    background: rgba(248, 113, 113, 0.15);
  }

  .rate-limited-btn {
    background: rgba(250, 204, 21, 0.15);
    border: 1px solid var(--chat-input-yellow, #eab308);
    color: var(--chat-input-yellow, #eab308);
    font-weight: 600;
    cursor: not-allowed;
  }
</style>
