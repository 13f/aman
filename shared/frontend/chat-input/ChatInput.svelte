<svelte:options customElement="chat-input" />

<script lang="ts">
  interface Skill {
    name: string;
    description: string;
  }

  interface Props {
    placeholder?: string;
    disabled?: boolean;
    rows?: number;
    buttonText?: string;
    stopText?: string;
    processing?: string | undefined;
    rateLimit?: number;
    value?: string;
    skills?: Skill[];
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
    skills = [],
    onsend,
    onstop,
    oninput,
    onkeydown,
  }: Props = $props();

  let textareaEl: HTMLTextAreaElement | undefined = $state();

  // ── Skill picker state ───────────────────────────────────────────────
  let showSkillPicker = $state(false);
  let skillPickerResults = $state<Skill[]>([]);
  let skillPickerIndex = $state(0);
  let pickerListEl: HTMLUListElement | undefined = $state();

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

  function dispatch(name: string, detail?: unknown) {
    textareaEl?.dispatchEvent(
      new CustomEvent(name, { detail, bubbles: true, composed: true })
    );
  }

  // ── Skill picker logic ───────────────────────────────────────────────

  function updateSkillPicker(text: string) {
    if (!text.startsWith("/skill")) {
      showSkillPicker = false;
      return;
    }

    const afterCommand = text.slice("/skill".length);
    // If user typed a space after "/skill", they're entering args — close picker
    if (afterCommand.startsWith(" ")) {
      showSkillPicker = false;
      return;
    }

    const prefix = afterCommand.trim().toLowerCase();

    if (prefix) {
      skillPickerResults = skills.filter(
        s => s.name.toLowerCase().includes(prefix) ||
             s.description.toLowerCase().includes(prefix)
      );
    } else {
      skillPickerResults = [...skills];
    }

    showSkillPicker = skillPickerResults.length > 0;
    skillPickerIndex = 0;
  }

  function selectSkill(skillName: string) {
    value = "/skill " + skillName + " ";
    if (textareaEl) {
      textareaEl.value = value;
      textareaEl.focus();
      autoGrow();
    }
    showSkillPicker = false;
    oninput?.(value);
    dispatch("input", { text: value });
  }

  function closeSkillPicker() {
    showSkillPicker = false;
  }

  function scrollSkillIntoView(index: number) {
    if (!pickerListEl) return;
    const item = pickerListEl.children[index] as HTMLElement | undefined;
    item?.scrollIntoView({ block: "nearest" });
  }

  // ── Input handlers ───────────────────────────────────────────────────

  function handleInput(e: Event) {
    const text = (e.target as HTMLTextAreaElement).value;
    value = text;
    autoGrow();
    updateSkillPicker(text);
    oninput?.(text);
    dispatch("input", { text });
  }

  function handleKeydown(e: KeyboardEvent) {
    // Skill picker keyboard navigation
    if (showSkillPicker) {
      if (e.key === "ArrowDown") {
        e.preventDefault();
        skillPickerIndex = Math.min(skillPickerIndex + 1, skillPickerResults.length - 1);
        scrollSkillIntoView(skillPickerIndex);
        return;
      }
      if (e.key === "ArrowUp") {
        e.preventDefault();
        skillPickerIndex = Math.max(skillPickerIndex - 1, 0);
        scrollSkillIntoView(skillPickerIndex);
        return;
      }
      if (e.key === "Enter" && !e.shiftKey) {
        e.preventDefault();
        if (skillPickerResults[skillPickerIndex]) {
          selectSkill(skillPickerResults[skillPickerIndex].name);
        }
        return;
      }
      if (e.key === "Tab") {
        e.preventDefault();
        if (skillPickerResults[skillPickerIndex]) {
          selectSkill(skillPickerResults[skillPickerIndex].name);
        }
        return;
      }
      if (e.key === "Escape") {
        e.preventDefault();
        closeSkillPicker();
        return;
      }
    }

    if ((e.ctrlKey || e.metaKey) && e.key === "Enter") {
      e.preventDefault();
      doSend();
      return;
    }
    onkeydown?.(e);
    dispatch("keydown", e);
  }

  function handleButtonClick() {
    if (rateLimit > 0) return;
    if (processing !== undefined) {
      onstop?.();
      dispatch("stop");
    } else {
      doSend();
    }
  }

  function doSend() {
    const text = value.trim();
    if (!text) return;
    // Close picker on send in case it was open
    showSkillPicker = false;
    onsend?.(text);
    dispatch("send", { text });
  }

  export function focus() {
    textareaEl?.focus();
  }
</script>

<div class="input-row">
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
</div>

{#if showSkillPicker}
  <!-- svelte-ignore a11y_no_static_element_interactions -->
  <ul
    class="skill-picker"
    bind:this={pickerListEl}
    role="listbox"
    onkeydown={(e: KeyboardEvent) => e.stopPropagation()}
  >
    {#each skillPickerResults as skill, i}
      <li
        class="skill-picker-item"
        class:selected={i === skillPickerIndex}
        role="option"
        aria-selected={i === skillPickerIndex}
        onclick={() => selectSkill(skill.name)}
        onkeydown={(e: KeyboardEvent) => { if (e.key === 'Enter' || e.key === ' ') { e.preventDefault(); selectSkill(skill.name); } }}
        onmouseenter={() => skillPickerIndex = i}
      >
        <span class="skill-picker-name">/{skill.name}</span>
        <span class="skill-picker-desc">{skill.description}</span>
      </li>
    {/each}
    {#if skillPickerResults.length === 0}
      <li class="skill-picker-empty">No matching skills</li>
    {/if}
  </ul>
{/if}

<style>
  :host {
    display: flex;
    flex-direction: column;
    gap: 0;
    width: 100%;
    position: relative;
  }

  .input-row {
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

  /* ── Skill picker ─────────────────────────────────────────────────── */

  .skill-picker {
    position: absolute;
    bottom: calc(100% + 4px);
    left: 0;
    right: 0;
    max-height: 260px;
    overflow-y: auto;
    margin: 0;
    padding: 0;
    list-style: none;
    background: var(--chat-input-picker-bg, #1e1e2e);
    border: 1px solid var(--chat-input-picker-border, #2a2d3a);
    border-radius: 8px;
    box-shadow: 0 -4px 20px rgba(0, 0, 0, 0.3);
    z-index: 100;
  }

  .skill-picker-item {
    display: flex;
    align-items: center;
    gap: 12px;
    padding: 8px 12px;
    cursor: pointer;
    border-bottom: 1px solid var(--chat-input-picker-border, #2a2d3a);
    transition: background 0.1s;
  }

  .skill-picker-item:last-child {
    border-bottom: none;
  }

  .skill-picker-item:hover,
  .skill-picker-item.selected {
    background: var(--chat-input-picker-hover, rgba(99, 102, 241, 0.15));
  }

  .skill-picker-name {
    font-family: "SF Mono", "Fira Code", monospace;
    font-size: 13px;
    font-weight: 600;
    color: var(--chat-input-accent, #3b82f6);
    white-space: nowrap;
    min-width: fit-content;
  }

  .skill-picker-desc {
    font-size: 12px;
    color: var(--chat-input-picker-desc, #a0a0b0);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .skill-picker-empty {
    padding: 12px;
    text-align: center;
    font-size: 12px;
    color: var(--chat-input-picker-desc, #a0a0b0);
  }
</style>
