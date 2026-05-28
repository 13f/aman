import App from "./App.svelte";
import { mount } from "svelte";
import { invoke } from "@tauri-apps/api/core";

// Load the chat-input web component (embedded at compile time from predefined/).
async function initWebComponents() {
  try {
    const js = await invoke<string>("get_chat_input_js");
    const script = document.createElement("script");
    script.textContent = js;
    document.head.appendChild(script);
  } catch {
    // Non-fatal: the component is also served by the gateway when available.
    console.warn("chat-input web component not loaded (gateway may serve it)");
  }
}

const ready = initWebComponents();

const app = mount(App, {
  target: document.getElementById("app")!,
});

// Re-export so Vite HMR and other tooling can reference it.
export default app;
export { ready };
