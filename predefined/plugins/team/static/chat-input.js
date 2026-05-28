class ChatInput extends HTMLElement {
  static get observedAttributes() {
    return ["placeholder", "disabled", "rows", "buttontext", "stoptext", "ratelimit", "processing"];
  }

  constructor() {
    super();
    this.attachShadow({ mode: "open" });

    this._value = "";
    this._focused = false;
    this._observer = null;

    this.shadowRoot.innerHTML = `
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

      <textarea rows="1"></textarea>
      <button class="send-btn" disabled>Send</button>
    `;

    this._textarea = this.shadowRoot.querySelector("textarea");
    this._button = this.shadowRoot.querySelector("button");
  }

  connectedCallback() {
    this._syncAttrs();
    this._syncButton();
    this._textarea.addEventListener("keydown", this._handleKeydown);
    this._textarea.addEventListener("input", this._handleInput);
    this._textarea.addEventListener("focus", this._onFocus);
    this._textarea.addEventListener("blur", this._onBlur);
    this._button.addEventListener("click", this._handleButtonClick);

    // Auto-grow on input (respects max-height)
    this._observer = new ResizeObserver(() => {
      this._autoGrow();
    });
    // Observe the textarea itself for content changes
    this._textarea.addEventListener("input", () => this._autoGrow());
  }

  disconnectedCallback() {
    this._textarea.removeEventListener("keydown", this._handleKeydown);
    this._textarea.removeEventListener("input", this._handleInput);
    this._textarea.removeEventListener("focus", this._onFocus);
    this._textarea.removeEventListener("blur", this._onBlur);
    this._button.removeEventListener("click", this._handleButtonClick);
    if (this._observer) {
      this._observer.disconnect();
      this._observer = null;
    }
  }

  attributeChangedCallback(name, oldVal, newVal) {
    if (oldVal === newVal) return;
    if (!this._textarea || !this._button) return;

    switch (name) {
      case "placeholder":
        this._textarea.placeholder = newVal || "";
        break;
      case "disabled":
        this._textarea.disabled = newVal !== null;
        break;
      case "rows":
        this._textarea.rows = parseInt(newVal) || 1;
        break;
      case "buttontext":
        if (!this.hasAttribute("ratelimit") && !this.hasAttribute("processing")) {
          this._button.textContent = newVal || "Send";
        }
        break;
      case "stoptext":
        this._stopText = newVal || "Stop";
        if (this.hasAttribute("processing")) {
          this._button.textContent = this._stopText;
        }
        break;
      case "ratelimit":
      case "processing":
        this._syncButton();
        break;
    }
  }

  // ── Public API ────────────────────────────────────────────────

  get value() {
    return this._textarea ? this._textarea.value : this._value;
  }

  set value(v) {
    this._value = v;
    if (this._textarea) {
      this._textarea.value = v;
      this._autoGrow();
    }
  }

  focus() {
    if (this._textarea) this._textarea.focus();
  }

  // ── Internal ──────────────────────────────────────────────────

  _syncAttrs() {
    this._textarea.placeholder = this.getAttribute("placeholder") || "";
    if (this.hasAttribute("disabled")) {
      this._textarea.disabled = true;
    }
    const rows = parseInt(this.getAttribute("rows")) || 1;
    this._textarea.rows = rows;
    this._stopText = this.getAttribute("stoptext") || "Stop";
    if (this._value) {
      this._textarea.value = this._value;
    }
  }

  _syncButton() {
    const rateLimit = parseInt(this.getAttribute("ratelimit")) || 0;
    const isProcessing = this.hasAttribute("processing");

    this._button.className = "";
    if (rateLimit > 0) {
      this._button.className = "rate-limited-btn";
      this._button.textContent = `${rateLimit}s`;
      this._button.disabled = true;
      this._textarea.disabled = true;
    } else if (isProcessing) {
      this._button.className = "stop-btn";
      this._button.textContent = this._stopText || "Stop";
      this._button.disabled = false;
    } else {
      this._button.className = "send-btn";
      this._button.textContent = this.getAttribute("buttontext") || "Send";
      this._button.disabled = !this._textarea.value.trim();
    }
  }

  _autoGrow() {
    const ta = this._textarea;
    ta.style.height = "auto";
    ta.style.height = Math.min(ta.scrollHeight, 160) + "px";
  }

  _onFocus = () => {
    this._focused = true;
  };

  _onBlur = () => {
    this._focused = false;
  };

  _handleKeydown = (e) => {
    if ((e.ctrlKey || e.metaKey) && e.key === "Enter") {
      e.preventDefault();
      this._doSend();
    }
  };

  _handleInput = () => {
    this._syncButton();
    this.dispatchEvent(
      new CustomEvent("input", {
        detail: { text: this._textarea.value },
        bubbles: false,
      })
    );
  };

  _handleButtonClick = () => {
    if (this.hasAttribute("ratelimit")) return;
    if (this.hasAttribute("processing")) {
      this.dispatchEvent(new CustomEvent("stop", { bubbles: false }));
    } else {
      this._doSend();
    }
  };

  _doSend() {
    const text = this._textarea.value.trim();
    if (!text) return;
    this.dispatchEvent(
      new CustomEvent("send", {
        detail: { text },
        bubbles: false,
      })
    );
  }
}

customElements.define("chat-input", ChatInput);
