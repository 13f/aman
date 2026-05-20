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
  import ActivityStateWidget from "./pages/ActivityStateWidget.svelte";
  import NotificationBell from "./pages/NotificationBell.svelte";
  import NotificationOverlay from "./pages/NotificationOverlay.svelte";
  import Settings from "./pages/Settings.svelte";

  let currentPage = $state("dashboard");
  let runtimeRunning = $state(false);
  let chatAvailable = $state(false);
  let hasProvider = $state(false);
  let hasAgent = $state(false);
  let gatewayLoading = $state(false);

  type Page = { id: string; label: string };

  const staticPages: Page[] = [
    { id: "dashboard", label: "Dashboard" },
    { id: "events", label: "Event Viewer" },
    { id: "workflows", label: "Workflow Board" },
    { id: "plugins", label: "Plugin Manager" },
    { id: "dlq", label: "DLQ" },
    { id: "settings", label: "Settings" },
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

      if (!hp && currentPage === "dashboard") {
        currentPage = "providers";
      }
    } catch {
      // Config may not exist yet
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
    if (pageId === "providers" || pageId === "agents") {
      checkOnboarding();
    }
  }

  function navigateTo(pageId: string) {
    currentPage = pageId;
    handlePageVisited(pageId);
  }

  async function startGateway() {
    gatewayLoading = true;
    try {
      const msg = await invoke<string>("start_runtime", { gatewayUrl: "http://127.0.0.1:9999" });
      console.log(msg);
      onRuntimeStatusChange(true);
    } catch (e: any) {
      console.error("Failed to start gateway:", e);
    } finally {
      gatewayLoading = false;
    }
  }

  async function stopGateway() {
    gatewayLoading = true;
    try {
      const msg = await invoke<string>("stop_runtime");
      console.log(msg);
      onRuntimeStatusChange(false);
    } catch (e: any) {
      console.error("Failed to stop gateway:", e);
    } finally {
      gatewayLoading = false;
    }
  }

  async function restartGateway() {
    gatewayLoading = true;
    try {
      await invoke<string>("stop_runtime");
      const msg = await invoke<string>("start_runtime", { gatewayUrl: "http://127.0.0.1:9999" });
      console.log(msg);
      onRuntimeStatusChange(true);
    } catch (e: any) {
      console.error("Failed to restart gateway:", e);
    } finally {
      gatewayLoading = false;
    }
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
      <button
        class={["nav-btn", currentPage === page.id ? "active" : ""].join(" ")}
        onclick={() => navigateTo(page.id)}
      >
        <span class="status-dot {runtimeRunning ? 'running' : 'stopped'}"></span>
        {page.label}
      </button>
    {/if}
  {/each}
  <div class="gateway-controls">
    {#if runtimeRunning}
      <button class="gw-btn" onclick={stopGateway} disabled={gatewayLoading}>停止</button>
      <button class="gw-btn" onclick={restartGateway} disabled={gatewayLoading}>重启</button>
    {:else}
      <button class="gw-btn start" onclick={startGateway} disabled={gatewayLoading}>启动</button>
    {/if}
  </div>
  <NotificationBell onNavigate={(p) => navigateTo(p)} />
  <ActivityStateWidget {runtimeRunning} />
</nav>

<NotificationOverlay onNavigate={(p) => navigateTo(p)} />

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
  {:else if currentPage === "settings"}
    <Settings />
  {/if}
</main>
