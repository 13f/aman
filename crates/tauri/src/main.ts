import App from "./App.svelte";
import { mount } from "svelte";
import "@shared/frontend/chat-input/ChatInput.svelte";

const app = mount(App, {
  target: document.getElementById("app")!,
});

export default app;
