import App from "./App.svelte";
import AgentWindow from "./pages/AgentWindow.svelte";
import { mount } from "svelte";
import { getCurrentWindow } from "@tauri-apps/api/window";
import "@shared/frontend/chat-input/ChatInput.svelte";
import "@shared/frontend/agent-selector/AgentSelector.svelte";

// Agent windows are spawned by the backend with a `agent-{key}` label and
// the default frontend URL.  We detect an agent window by checking the
// label, then mount the dedicated agent-page shell instead of the main app.
async function boot() {
  const label = getCurrentWindow().label;
  const agentPrefix = "agent-";
  let app;

  if (label.startsWith(agentPrefix)) {
    // Per-agent independent window.
    const agentKey = label.slice(agentPrefix.length);
    app = mount(AgentWindow, {
      target: document.getElementById("app")!,
      props: { agentKey },
    });
  } else {
    // Main application shell (dashboard, settings, etc.).
    app = mount(App, {
      target: document.getElementById("app")!,
    });
  }

  return app;
}

const app = boot();
export default app;
