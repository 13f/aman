<script lang="ts">
  import "./app.css";
  import { invoke } from "@tauri-apps/api/core";
  import Dashboard from "./pages/Dashboard.svelte";
  import SkillEditor from "./pages/SkillEditor.svelte";
  import EventViewer from "./pages/EventViewer.svelte";
  import WorkflowBoard from "./pages/WorkflowBoard.svelte";
  import SoulEditor from "./pages/SoulEditor.svelte";
  import PluginManager from "./pages/PluginManager.svelte";
  import DLQ from "./pages/DLQ.svelte";
  import Chat from "./pages/Chat.svelte";

  let currentPage = $state("dashboard");
  let runtimeRunning = $state(false);
  let chatAvailable = $state(false);

  type Page = { id: string; label: string };

  const staticPages: Page[] = [
    { id: "dashboard", label: "Dashboard" },
    { id: "skills", label: "Skill Editor" },
    { id: "events", label: "Event Viewer" },
    { id: "workflows", label: "Workflow Board" },
    { id: "soul", label: "SOUL Editor" },
    { id: "plugins", label: "Plugin Manager" },
    { id: "dlq", label: "DLQ" },
  ];

  const chatPage: Page = { id: "chat", label: "Chat" };

  let pages = $derived(chatAvailable ? [...staticPages, chatPage] : staticPages);

  async function checkCapabilities() {
    try {
      const caps = await invoke<{ capability: string }[]>("get_capabilities");
      chatAvailable = caps.some((c) => c.capability === "chat");
    } catch {
      chatAvailable = false;
    }
  }

  function onRuntimeStatusChange(running: boolean) {
    runtimeRunning = running;
    if (running) {
      checkCapabilities();
    } else {
      chatAvailable = false;
    }
  }
</script>

<nav class="sidebar">
  <h1>Aman</h1>
  {#each pages as page}
    <a
      href="#"
      class={currentPage === page.id ? "active" : ""}
      onclick={(e) => { e.preventDefault(); currentPage = page.id; }}
    >
      <span class="status-dot {runtimeRunning ? 'running' : 'stopped'}"></span>
      {page.label}
    </a>
  {/each}
</nav>

<main class="main">
  {#if currentPage === "dashboard"}
    <Dashboard onstatuschange={(r) => onRuntimeStatusChange(r)} />
  {:else if currentPage === "skills"}
    <SkillEditor />
  {:else if currentPage === "events"}
    <EventViewer />
  {:else if currentPage === "workflows"}
    <WorkflowBoard />
  {:else if currentPage === "soul"}
    <SoulEditor />
  {:else if currentPage === "plugins"}
    <PluginManager />
  {:else if currentPage === "dlq"}
    <DLQ />
  {:else if currentPage === "chat"}
    <Chat />
  {/if}
</main>
