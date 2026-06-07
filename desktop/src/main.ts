import App from "./App.svelte";
import { mount } from "svelte";
import "@shared/frontend/chat-input/ChatInput.svelte";
import "@shared/frontend/agent-selector/AgentSelector.svelte";

const app = mount(App, {
  target: document.getElementById("app")!,
});

export default app;
