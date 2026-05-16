<script lang="ts">
  import "./app.css";
  import { invoke } from "@tauri-apps/api/core";
  import { onMount } from "svelte";
  import Dashboard from "./pages/Dashboard.svelte";
  import EventViewer from "./pages/EventViewer.svelte";
  import WorkflowBoard from "./pages/WorkflowBoard.svelte";
  import PluginManager from "./pages/PluginManager.svelte";
  import DLQ from "./pages/DLQ.svelte";
  import Chat from "./pages/Chat.svelte";
  import Providers from "./pages/Providers.svelte";
  import Agents from "./pages/Agents.svelte";
  import IdleStateWidget from "./pages/IdleStateWidget.svelte";

  let currentPage = $state("dashboard");
  let runtimeRunning = $state(false);
  let chatAvailable = $state(false);
  let hasProvider = $state(false);
  let hasAgent = $state(false);

  type Page = { id: string; label: string };

  const staticPages: Page[] = [
    { id: "dashboard", label: "Dashboard" },
    { id: "events", label: "Event Viewer" },
    { id: "workflows", label: "Workflow Board" },
    { id: "plugins", label: "Plugin Manager" },
    { id: "dlq", label: "DLQ" },
  ];

  const providerPage: Page = { id: "providers", label: "Providers" };
  const agentPage: Page = { id: "agents", label: "Agents" };
  const chatPage: Page = { id: "chat", label: "Chat" };

  let pages = $derived(
    chatAvailable
      ? [...staticPages, providerPage, agentPage, chatPage]
      : [...staticPages, providerPage, agentPage, chatPage]
  );

  let initialLoadDone = $state(false);

  async function checkCapabilities() {
    try {
      const caps = await invoke<{ capability: string }[]>("get_capabilities");
      chatAvailable = caps.some((c) => c.capability === "chat");
    } catch {
      chatAvailable = false;
    }
  }

  async function checkOnboarding() {
    try {
      const [hp, ha] = await Promise.all([
        invoke<boolean>("has_any_provider"),
        invoke<boolean>("has_any_agent"),
      ]);
      hasProvider = hp;
      hasAgent = ha;

      // Onboarding: if no providers, redirect to providers page
      if (!hp && currentPage === "dashboard") {
        currentPage = "providers";
      }
    } catch {
      // Config may not exist yet — that's fine
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

  function handlePageVisited(pageId: string) {
    // Re-check onboarding state when providers or agents page is visited
    if (pageId === "providers" || pageId === "agents") {
      checkOnboarding();
    }
  }

  function navigateTo(pageId: string) {
    currentPage = pageId;
    handlePageVisited(pageId);
  }

  onMount(async () => {
    await checkOnboarding();
    initialLoadDone = true;
  });
</script>

<nav class="sidebar">
  {#each pages as page}
    {#if page.id === "chat" && !runtimeRunning}
      <span class="sidebar-link disabled" title="Start the runtime first">
        <span class="status-dot stopped"></span>
        {page.label}
      </span>
    {:else}
      <a
        href="#"
        class={currentPage === page.id ? "active" : ""}
        onclick={(e) => { e.preventDefault(); navigateTo(page.id); }}
      >
        <span class="status-dot {runtimeRunning ? 'running' : 'stopped'}"></span>
        {page.label}
      </a>
    {/if}
  {/each}
  <IdleStateWidget {runtimeRunning} />
</nav>

<main class="main">
  {#if currentPage === "dashboard"}
    <Dashboard onstatuschange={(r) => onRuntimeStatusChange(r)} />
  {:else if currentPage === "events"}
    <EventViewer />
  {:else if currentPage === "workflows"}
    <WorkflowBoard />
  {:else if currentPage === "plugins"}
    <PluginManager />
  {:else if currentPage === "dlq"}
    <DLQ />
  {:else if currentPage === "providers"}
    <Providers />
  {:else if currentPage === "agents"}
    <Agents onNavigate={(p) => navigateTo(p)} />
  {:else if currentPage === "chat"}
    <Chat />
  {/if}
</main>
