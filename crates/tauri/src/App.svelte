<script lang="ts">
  import "./app.css";
  import { onMount } from "svelte";
  import { invoke } from "@tauri-apps/api/core";
  import Home from "./pages/Home.svelte";
  import Dashboard from "./pages/Dashboard.svelte";
  import Maintenance from "./pages/Maintenance.svelte";
  import WorkflowBoard from "./pages/WorkflowBoard.svelte";
  import PluginManager from "./pages/PluginManager.svelte";
  import Chat from "./pages/Chat.svelte";
  import Providers from "./pages/Providers.svelte";
  import Agents from "./pages/Agents.svelte";
  import ActivityStateWidget from "./pages/ActivityStateWidget.svelte";
  import NotificationOverlay from "./pages/NotificationOverlay.svelte";
  import Settings from "./pages/Settings.svelte";
  import ThirdPartyServices from "./pages/ThirdPartyServices.svelte";

  let currentPage = $state("dashboard");
  let runtimeRunning = $state(false);
  let hasProvider = $state(false);
  let hasAgent = $state(false);
  let activeAgentName = $state("");
  let chatPrefill = $state("");
  let chatPrefillSeq = $state(0);

  type MenuItem = { id: string; label: string };
  type MenuGroup = { name: string; label: string; items: MenuItem[] };

  const menuGroups: MenuGroup[] = [
    {
      name: "apps",
      label: "Workspace",
      items: [
        { id: "home", label: "Home" },
        { id: "chat", label: "Chat" },
      ],
    },
    {
      name: "platform",
      label: "Services",
      items: [
        { id: "agents", label: "Agents" },
        { id: "providers", label: "Providers" },
        { id: "third-party", label: "Third Party Services" },
        { id: "dashboard", label: "Dashboard" },
      ],
    },
    {
      name: "management",
      label: "Management",
      items: [
        { id: "workflows", label: "Workflow Board" },
        { id: "plugins", label: "Plugin Manager" },
        { id: "maintenance", label: "Maintenance" },
        { id: "settings", label: "Settings" },
      ],
    },
  ];

  let expandedGroups = $state<Record<string, boolean>>({
    apps: true,
    platform: true,
    management: true,
  });

  let initialLoadDone = $state(false);

  function toggleGroup(name: string) {
    expandedGroups[name] = !expandedGroups[name];
  }

  async function checkOnboarding() {
    try {
      const [hp, ha] = await Promise.all([
        invoke<boolean>("has_any_provider"),
        invoke<boolean>("has_any_agent"),
      ]);
      hasProvider = hp;
      hasAgent = ha;

      if (!hp && (currentPage === "home" || currentPage === "dashboard")) {
        currentPage = "providers";
      }
    } catch {
      // Config may not exist yet
    }
  }

  function onRuntimeStatusChange(running: boolean) {
    runtimeRunning = running;
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

  function navigateToChatWithPrefill(text: string) {
    chatPrefill = text;
    chatPrefillSeq++;
    navigateTo("chat");
  }

  async function refreshActiveAgent() {
    try {
      const agent = await invoke<{ display_name: string } | null>("get_active_agent");
      activeAgentName = agent?.display_name ?? "";
    } catch {
      activeAgentName = "";
    }
  }

  $effect(() => {
    // Auto-navigate to Dashboard when gateway is not running
    if (!runtimeRunning) {
      currentPage = "dashboard";
      return;
    }
    // Refresh active agent on every page change so the idle widget
    // shows/hides correctly regardless of which page the agent is used from.
    void currentPage;
    refreshActiveAgent();
  });

  onMount(async () => {
    await checkOnboarding();

    // Auto-detect if gateway is already running
    try {
      const status = await invoke<{ running: boolean }>("try_connect_gateway");
      if (status.running) {
        runtimeRunning = true;
        currentPage = "home";
      }
    } catch {
      // Gateway not running — stay on dashboard
    }

    initialLoadDone = true;
  });
</script>

<nav class="sidebar">
  {#each menuGroups as group}
    <button class="menu-header" onclick={() => toggleGroup(group.name)}>
      <span class="menu-arrow">{expandedGroups[group.name] ? "▾" : "▸"}</span>
      {group.label}
    </button>
    {#if expandedGroups[group.name]}
      <div class="menu-items">
        {#each group.items as page}
          {#if !runtimeRunning || page.id === "settings"}
            <span class="sidebar-link disabled" title={page.id === "settings" ? "Settings are being reorganised" : "Start the runtime first"}>
              <span class="status-dot stopped"></span>
              {page.label}
            </span>
          {:else}
            <button
              class={["nav-btn", currentPage === page.id ? "active" : ""].join(" ")}
              onclick={() => navigateTo(page.id)}
            >
              <span class="status-dot running"></span>
              {page.label}
            </button>
          {/if}
        {/each}
      </div>
    {/if}
  {/each}
  <ActivityStateWidget {runtimeRunning} visible={activeAgentName !== ""} agentName={activeAgentName} />
</nav>

<NotificationOverlay onNavigate={(p) => navigateTo(p)} />

<main class="main">
  {#if currentPage === "home"}
    <Home
      onNavigate={(p) => navigateTo(p)}
      onNavigateChatWithSkill={async (_agentKey: string, skillName: string) => {
        navigateToChatWithPrefill(`/skill ${skillName} `);
      }}
    />
  {:else if currentPage === "dashboard"}
    <Dashboard onstatuschange={(r) => onRuntimeStatusChange(r)} />
  {:else if currentPage === "maintenance"}
    <Maintenance />
  {:else if currentPage === "workflows"}
    <WorkflowBoard />
  {:else if currentPage === "plugins"}
    <PluginManager />
  {:else if currentPage === "providers"}
    <Providers />
  {:else if currentPage === "third-party"}
    <ThirdPartyServices />
  {:else if currentPage === "agents"}
    <Agents onNavigate={(p) => navigateTo(p)} />
  {:else if currentPage === "chat"}
    <Chat prefillInput={chatPrefill} prefillSeq={chatPrefillSeq} />
  {:else if currentPage === "settings"}
    <Settings />
  {/if}
</main>
